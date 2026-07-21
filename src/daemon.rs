use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    path::PathBuf,
    time::Duration,
};

use anyhow::{Context, Result};
use fs2::FileExt;
use tokio::{sync::mpsc, task::JoinHandle, time};

use crate::{
    collector::Collector,
    diagnostics::{Diagnostics, HealthSnapshot, HostHealth, LogLevel},
    model::AgentState,
    popup::client_is_suppressed,
    remote::{self, RemoteEvent},
    runtime,
    snapshot::{
        aggregate_online, status_widget_with, AgentKey, AggregateSnapshot, HostSnapshot,
        StatusColors, SCHEMA_VERSION,
    },
    tmux::{now_ms, Tmux},
    watcher::FileSignal,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalWake {
    Ignore,
    State,
    Full,
}

pub fn classify_local_paths(paths: &[PathBuf]) -> LocalWake {
    if paths
        .iter()
        .any(|path| path.file_name().and_then(|name| name.to_str()) == Some("refresh"))
    {
        return LocalWake::Full;
    }
    if paths.iter().any(|path| {
        path.components()
            .any(|component| component.as_os_str() == "panes")
    }) {
        return LocalWake::State;
    }
    LocalWake::Ignore
}

const DEFAULT_RECONCILE_INTERVAL_MS: u64 = 60_000;

#[derive(Debug, Clone)]
pub struct HostTracker {
    host: String,
    failures: u8,
    snapshot: Option<HostSnapshot>,
}

impl HostTracker {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            failures: 0,
            snapshot: None,
        }
    }

    pub fn success(&mut self, mut snapshot: HostSnapshot) {
        snapshot.online = true;
        snapshot.error = None;
        self.failures = 0;
        self.snapshot = Some(snapshot);
    }

    pub fn failure(&mut self, error: impl Into<String>, now_ms: u64) {
        self.failures = self.failures.saturating_add(1);
        if self.failures < 2 {
            return;
        }
        let snapshot = self
            .snapshot
            .get_or_insert_with(|| HostSnapshot::empty(&self.host, now_ms));
        snapshot.online = false;
        snapshot.collected_at_ms = now_ms;
        snapshot.error = Some(error.into());
    }

    pub fn snapshot(&self) -> Option<&HostSnapshot> {
        self.snapshot.as_ref()
    }
}

pub fn notification_for_transition(
    previous: Option<AgentState>,
    current: AgentState,
) -> Option<&'static str> {
    if current == AgentState::NeedsInput && previous != Some(AgentState::NeedsInput) {
        return previous.map(|_| "needs input");
    }
    (previous == Some(AgentState::Working) && current == AgentState::Idle)
        .then_some("finished; now idle")
}

pub fn format_notification(
    host: &str,
    session: &str,
    project: &str,
    agent: &str,
    transition: &str,
) -> String {
    format!("Seer: {host} › {session} › {project} › {agent} {transition}")
}

pub fn runtime_snapshot_path() -> PathBuf {
    runtime::current_server_directory().join("snapshot.json")
}

pub async fn run() -> Result<()> {
    let _lock = match acquire_lock()? {
        Some(lock) => lock,
        None => return Ok(()),
    };
    let tmux = Tmux::new();
    let directory = runtime::current_server_directory();
    runtime::ensure_private_directory(&directory)?;
    let mut local_signal = FileSignal::watch(&directory)?;
    let reconcile_interval_ms = option(
        &tmux,
        "@seer_reconcile_interval_ms",
        &DEFAULT_RECONCILE_INTERVAL_MS.to_string(),
    )
    .parse::<u64>()
    .unwrap_or(DEFAULT_RECONCILE_INTERVAL_MS)
    .max(1_000);
    let remote_max_backoff = option(&tmux, "@seer_remote_max_backoff_ms", "60000")
        .parse::<u64>()
        .unwrap_or(60_000)
        .max(1_000);
    let notification_duration = option(&tmux, "@seer_notify_ms", "4000")
        .parse::<u64>()
        .unwrap_or(4_000);
    let diagnostics =
        Diagnostics::current(LogLevel::parse(&option(&tmux, "@seer_log_level", "warn")))?;
    diagnostics.log(
        LogLevel::Debug,
        "daemon",
        &format!("event coordinator started; safety reconciliation {reconcile_interval_ms}ms"),
    )?;
    let colors = StatusColors {
        working: option(&tmux, "@seer_color_working", "#9ece6a"),
        idle: option(&tmux, "@seer_color_idle", "#e0af68"),
        needs_input: option(&tmux, "@seer_color_input", "#7aa2f7"),
        untracked: option(&tmux, "@seer_color_offline", "#565f89"),
    };
    let mut hosts = configured_hosts(&tmux);
    let mut trackers: HashMap<String, HostTracker> = std::iter::once("local".to_owned())
        .chain(hosts.iter().cloned())
        .map(|host| (host.clone(), HostTracker::new(host)))
        .collect();
    let (remote_sender, mut remote_receiver) = mpsc::channel(64);
    let mut remote_tasks = HashMap::new();
    sync_remote_tasks(
        &hosts,
        &mut remote_tasks,
        &remote_sender,
        remote_max_backoff,
        false,
    );
    let mut collector = Collector::new("local", tmux.clone());
    let scan_started = now_ms();
    trackers
        .get_mut("local")
        .unwrap()
        .success(collector.full_scan()?);
    let mut local_scan_ms = now_ms().saturating_sub(scan_started);
    let mut previous = HashMap::new();
    let mut previous_online = HashMap::new();
    let mut previous_widget = None;
    let mut previous_published = Vec::new();
    let mut initial = true;
    let mut host_health = HashMap::new();
    publish_runtime(
        &tmux,
        &trackers,
        &hosts,
        &mut previous,
        &mut previous_online,
        &mut previous_widget,
        &mut previous_published,
        &mut initial,
        &colors,
        notification_duration,
        local_scan_ms,
        &host_health,
        &diagnostics,
    )?;

    let mut safety = time::interval(Duration::from_millis(reconcile_interval_ms));
    safety.tick().await;
    loop {
        tokio::select! {
            changed = local_signal.changed() => {
                match classify_local_paths(&changed?) {
                    LocalWake::Ignore => continue,
                    LocalWake::State => {
                        match collector.state_refresh() {
                            Ok(snapshot) => trackers.get_mut("local").unwrap().success(snapshot),
                            Err(error) => {
                                diagnostics.log(LogLevel::Warn, "local", &error.to_string())?;
                                continue;
                            }
                        }
                    }
                    LocalWake::Full => {
                        refresh_all(
                            &tmux, &mut collector, &mut trackers, &mut hosts,
                            &mut remote_tasks, &remote_sender, remote_max_backoff,
                            true, &mut local_scan_ms,
                        )?;
                    }
                }
            }
            remote = remote_receiver.recv() => {
                let Some(remote) = remote else { return Ok(()); };
                match remote {
                    RemoteEvent::Snapshot { host, snapshot } => {
                        if let Some(tracker) = trackers.get_mut(&host) {
                            tracker.success(snapshot);
                            host_health.insert(host.clone(), HostHealth {
                                host,
                                online: true,
                                latency_ms: 0,
                                failures: 0,
                                next_retry_ms: 0,
                                last_error: None,
                            });
                        }
                    }
                    RemoteEvent::Disconnected { host, error } => {
                        if let Some(tracker) = trackers.get_mut(&host) {
                            tracker.failure(error.clone(), now_ms());
                            let failures = tracker.failures;
                            if failures == 2 {
                                diagnostics.log(LogLevel::Warn, &host, &error)?;
                            }
                            host_health.insert(host.clone(), HostHealth {
                                host,
                                online: tracker.snapshot().is_some_and(|snapshot| snapshot.online),
                                latency_ms: 0,
                                failures: failures.into(),
                                next_retry_ms: now_ms().saturating_add(remote::reconnect_delay_ms(failures, remote_max_backoff)),
                                last_error: Some(error),
                            });
                        }
                    }
                }
            }
            _ = safety.tick() => {
                refresh_all(
                    &tmux, &mut collector, &mut trackers, &mut hosts,
                    &mut remote_tasks, &remote_sender, remote_max_backoff,
                    false, &mut local_scan_ms,
                )?;
            }
            result = tokio::signal::ctrl_c() => {
                result.context("failed to listen for shutdown")?;
                for (_, task) in remote_tasks.drain() { task.abort(); }
                return Ok(());
            }
        }

        publish_runtime(
            &tmux,
            &trackers,
            &hosts,
            &mut previous,
            &mut previous_online,
            &mut previous_widget,
            &mut previous_published,
            &mut initial,
            &colors,
            notification_duration,
            local_scan_ms,
            &host_health,
            &diagnostics,
        )?;
    }
}
fn configured_hosts(tmux: &Tmux) -> Vec<String> {
    option(tmux, "@seer_hosts", "")
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

fn sync_remote_tasks(
    hosts: &[String],
    tasks: &mut HashMap<String, JoinHandle<()>>,
    sender: &mpsc::Sender<RemoteEvent>,
    maximum_backoff_ms: u64,
    restart_all: bool,
) {
    let configured = hosts.iter().cloned().collect::<HashSet<_>>();
    tasks.retain(|host, task| {
        let keep = configured.contains(host) && !restart_all;
        if !keep {
            task.abort();
        }
        keep
    });
    for host in hosts {
        tasks.entry(host.clone()).or_insert_with(|| {
            tokio::spawn(remote::supervise(
                host.clone(),
                sender.clone(),
                maximum_backoff_ms,
            ))
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn refresh_all(
    tmux: &Tmux,
    collector: &mut Collector,
    trackers: &mut HashMap<String, HostTracker>,
    hosts: &mut Vec<String>,
    remote_tasks: &mut HashMap<String, JoinHandle<()>>,
    remote_sender: &mpsc::Sender<RemoteEvent>,
    remote_max_backoff: u64,
    restart_all: bool,
    local_scan_ms: &mut u64,
) -> Result<()> {
    let started = now_ms();
    trackers
        .get_mut("local")
        .expect("local tracker")
        .success(collector.full_scan()?);
    *local_scan_ms = now_ms().saturating_sub(started);

    let next = configured_hosts(tmux);
    let retained = next.iter().map(String::as_str).collect::<HashSet<_>>();
    trackers.retain(|host, _| host == "local" || retained.contains(host.as_str()));
    for host in &next {
        trackers
            .entry(host.clone())
            .or_insert_with(|| HostTracker::new(host));
    }
    *hosts = next;
    sync_remote_tasks(
        hosts,
        remote_tasks,
        remote_sender,
        remote_max_backoff,
        restart_all,
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn publish_runtime(
    tmux: &Tmux,
    trackers: &HashMap<String, HostTracker>,
    hosts: &[String],
    previous: &mut HashMap<AgentKey, AgentState>,
    previous_online: &mut HashMap<String, bool>,
    previous_widget: &mut Option<String>,
    previous_published: &mut Vec<HostSnapshot>,
    initial: &mut bool,
    colors: &StatusColors,
    notification_duration: u64,
    local_scan_ms: u64,
    host_health: &HashMap<String, HostHealth>,
    diagnostics: &Diagnostics,
) -> Result<()> {
    let now = now_ms();
    let snapshots: Vec<HostSnapshot> = std::iter::once("local")
        .chain(hosts.iter().map(String::as_str))
        .filter_map(|host| trackers.get(host).and_then(HostTracker::snapshot).cloned())
        .collect();
    if !same_snapshot_content(previous_published, &snapshots) {
        publish_snapshot(&snapshots, now)?;
        diagnostics.publish_health(&HealthSnapshot {
            generated_at_ms: now,
            local_scan_ms,
            hosts: hosts
                .iter()
                .filter_map(|host| host_health.get(host).cloned())
                .collect(),
        })?;
        *previous_published = snapshots.clone();
    }
    publish_widget_if_changed(
        previous_widget,
        status_widget_with(aggregate_online(snapshots.iter()), colors),
        |widget| {
            tmux.set_global_option("@seer_widget", widget)?;
            tmux.refresh_status();
            Ok(())
        },
    )?;

    for host in &snapshots {
        let was_online = previous_online.get(&host.host).copied();
        if host.online {
            let can_notify = !*initial && was_online != Some(false);
            let mut current_keys = HashSet::new();
            for session in &host.sessions {
                for window in &session.windows {
                    for pane in &window.panes {
                        current_keys.insert(pane.key.clone());
                        if can_notify {
                            if let Some(transition) = notification_for_transition(
                                previous.get(&pane.key).copied(),
                                pane.state,
                            ) {
                                notify_clients(
                                    tmux,
                                    notification_duration,
                                    &format_notification(
                                        &host.host,
                                        &session.name,
                                        &pane.project,
                                        &pane.agent.to_string(),
                                        transition,
                                    ),
                                    now,
                                );
                            }
                        }
                        previous.insert(pane.key.clone(), pane.state);
                    }
                }
            }
            previous.retain(|key, _| key.host != host.host || current_keys.contains(key));
        }
        previous_online.insert(host.host.clone(), host.online);
    }
    let configured = std::iter::once("local")
        .chain(hosts.iter().map(String::as_str))
        .collect::<HashSet<_>>();
    previous.retain(|key, _| configured.contains(key.host.as_str()));
    previous_online.retain(|host, _| configured.contains(host.as_str()));
    *initial = false;
    Ok(())
}

fn notify_clients(tmux: &Tmux, duration_ms: u64, message: &str, now: u64) {
    let Ok(clients) = tmux.clients() else { return };
    for client in clients {
        if !client_is_suppressed(&client, now) {
            let _ = tmux.display_message(&client, duration_ms, message);
        }
    }
}

fn publish_snapshot(hosts: &[HostSnapshot], now: u64) -> Result<()> {
    let path = runtime_snapshot_path();
    let snapshot = AggregateSnapshot {
        schema_version: SCHEMA_VERSION,
        generated_at_ms: now,
        hosts: hosts.to_vec(),
    };
    runtime::atomic_write(&path, &serde_json::to_vec(&snapshot)?)
}

fn same_snapshot_content(previous: &[HostSnapshot], current: &[HostSnapshot]) -> bool {
    if previous.len() != current.len() {
        return false;
    }
    previous.iter().zip(current).all(|(previous, current)| {
        let mut previous = previous.clone();
        let mut current = current.clone();
        previous.collected_at_ms = 0;
        current.collected_at_ms = 0;
        previous == current
    })
}

fn acquire_lock() -> Result<Option<File>> {
    let directory = runtime::current_server_directory();
    runtime::ensure_private_directory(&directory)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("daemon.lock"))?;
    match lock.try_lock_exclusive() {
        Ok(()) => Ok(Some(lock)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn option(tmux: &Tmux, name: &str, default: &str) -> String {
    tmux.show_global_option(name)
        .unwrap_or_else(|| default.into())
}

fn publish_widget_if_changed(
    previous: &mut Option<String>,
    widget: String,
    publish: impl FnOnce(&str) -> Result<()>,
) -> Result<()> {
    if previous.as_deref() == Some(widget.as_str()) {
        return Ok(());
    }
    publish(&widget)?;
    *previous = Some(widget);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widget_publishes_only_initial_and_changed_values() {
        let mut previous = None;
        let mut published = Vec::new();
        for widget in ["gray", "gray", "green"] {
            publish_widget_if_changed(&mut previous, widget.to_owned(), |value| {
                published.push(value.to_owned());
                Ok(())
            })
            .unwrap();
        }
        assert_eq!(published, ["gray", "green"]);
    }
}
