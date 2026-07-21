use std::{
    collections::{HashMap, HashSet},
    fs::{File, OpenOptions},
    path::PathBuf,
    process::Stdio,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use tokio::{process::Command, task::JoinSet, time};

use crate::{
    diagnostics::{Diagnostics, HealthSnapshot, HostHealth, LogLevel},
    model::AgentState,
    popup::client_is_suppressed,
    runtime,
    scheduler::HostSchedule,
    snapshot::{
        aggregate_online, status_widget_with, AgentKey, AggregateSnapshot, HostSnapshot,
        ProcessTable, StatusColors, SCHEMA_VERSION,
    },
    tmux::{now_ms, Tmux},
};

const LOCAL_INTERVAL_MS: u64 = 500;
const TOPOLOGY_INTERVAL_MS: u64 = 5_000;
const PROCESS_INTERVAL_MS: u64 = 5_000;
const CONFIG_INTERVAL_MS: u64 = 5_000;
const SNAPSHOT_HEARTBEAT_MS: u64 = 5_000;
const REMOTE_TIMEOUT: Duration = Duration::from_secs(5);
const LOOP_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug)]
struct RemoteResult {
    host: String,
    started_at_ms: u64,
    finished_at_ms: u64,
    result: std::result::Result<HostSnapshot, String>,
}

#[derive(Debug, Clone, Copy)]
struct RemoteTiming {
    interval_ms: u64,
    maximum_backoff_ms: u64,
}

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

pub fn remote_snapshot_args(host: &str) -> Result<Vec<String>> {
    if host.is_empty()
        || !host
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        bail!("invalid SSH alias: {host}");
    }
    Ok(vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=2".into(),
        host.into(),
        format!("exec \"$SHELL\" -lc '\"$HOME/.local/bin/tmux-seer\" snapshot --host {host}'"),
    ])
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
    let mut hosts = option(&tmux, "@seer_hosts", "")
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let remote_interval = option(&tmux, "@seer_remote_interval_ms", "2000")
        .parse::<u64>()
        .unwrap_or(2_000)
        .max(500);
    let remote_max_backoff = option(&tmux, "@seer_remote_max_backoff_ms", "60000")
        .parse::<u64>()
        .unwrap_or(60_000)
        .max(remote_interval);
    let remote_timing = RemoteTiming {
        interval_ms: remote_interval,
        maximum_backoff_ms: remote_max_backoff,
    };
    let notification_duration = option(&tmux, "@seer_notify_ms", "4000")
        .parse::<u64>()
        .unwrap_or(4_000);
    let diagnostics =
        Diagnostics::current(LogLevel::parse(&option(&tmux, "@seer_log_level", "warn")))?;
    diagnostics.log(
        LogLevel::Debug,
        "daemon",
        &format!(
            "started with {} remote hosts; remote interval {}ms; maximum backoff {}ms",
            hosts.len(),
            remote_timing.interval_ms,
            remote_timing.maximum_backoff_ms
        ),
    )?;
    let colors = StatusColors {
        working: option(&tmux, "@seer_color_working", "#9ece6a"),
        idle: option(&tmux, "@seer_color_idle", "#e0af68"),
        needs_input: option(&tmux, "@seer_color_input", "#7aa2f7"),
        untracked: option(&tmux, "@seer_color_offline", "#565f89"),
    };
    let mut trackers: HashMap<String, HostTracker> = std::iter::once("local".to_owned())
        .chain(hosts.iter().cloned())
        .map(|host| (host.clone(), HostTracker::new(host)))
        .collect();
    let mut schedules = hosts
        .iter()
        .cloned()
        .map(|host| (host, HostSchedule::immediate()))
        .collect::<HashMap<_, _>>();
    let mut remote_tasks = JoinSet::new();
    let mut in_flight = HashSet::new();
    let mut host_health = HashMap::new();
    let mut previous: HashMap<AgentKey, AgentState> = HashMap::new();
    let mut previous_online: HashMap<String, bool> = HashMap::new();
    let mut previous_widget = None;
    let mut previous_published = Vec::new();
    let mut processes = ProcessTable::default();
    let mut pane_rows = None;
    let mut initial = true;
    let mut last_local = 0;
    let mut last_topology = 0;
    let mut last_process = 0;
    let mut last_config = 0;
    let mut last_publish = 0;
    let mut refresh_token = runtime::refresh_token();
    let mut local_scan_ms = 0;

    loop {
        let now = now_ms();
        let next_refresh_token = runtime::refresh_token();
        let forced = next_refresh_token != refresh_token;
        refresh_token = next_refresh_token;

        if forced || now.saturating_sub(last_config) >= CONFIG_INTERVAL_MS {
            if reconcile_hosts(
                &option(&tmux, "@seer_hosts", ""),
                &mut hosts,
                &mut trackers,
                &mut previous,
                &mut previous_online,
            ) {
                let configured = hosts.iter().cloned().collect::<HashSet<_>>();
                schedules.retain(|host, _| configured.contains(host));
                in_flight.retain(|host| configured.contains(host));
                host_health.retain(|host, _| configured.contains(host));
                for host in &hosts {
                    schedules.entry(host.clone()).or_default().force(now);
                }
            }
            last_config = now;
        }

        if forced {
            let _ = diagnostics.log(LogLevel::Debug, "daemon", "forced refresh requested");
            last_local = 0;
            last_topology = 0;
            last_process = 0;
            for schedule in schedules.values_mut() {
                schedule.force(now);
            }
        }

        if last_local == 0 || now.saturating_sub(last_local) >= LOCAL_INTERVAL_MS {
            let scan_started = now_ms();
            let refresh_topology =
                last_topology == 0 || now.saturating_sub(last_topology) >= TOPOLOGY_INTERVAL_MS;
            let mut topology_is_fresh = false;
            if refresh_topology {
                match tmux.pane_rows() {
                    Ok(rows) => {
                        pane_rows = Some(rows);
                        topology_is_fresh = true;
                    }
                    Err(error) => {
                        let error = error.to_string();
                        let _ = diagnostics.log(LogLevel::Warn, "local", &error);
                        if pane_rows.is_none() {
                            trackers.get_mut("local").unwrap().failure(error, now);
                        }
                    }
                }
                last_topology = now;
            }
            let refreshed_processes =
                last_process == 0 || now.saturating_sub(last_process) >= PROCESS_INTERVAL_MS;
            if refreshed_processes {
                processes = tmux.process_table();
                last_process = now;
            }
            if let Some(rows) = pane_rows.as_deref() {
                match tmux.snapshot_from_rows(
                    "local",
                    rows,
                    &processes,
                    topology_is_fresh && refreshed_processes,
                ) {
                    Ok(snapshot) => trackers.get_mut("local").unwrap().success(snapshot),
                    Err(error) => {
                        let error = error.to_string();
                        let _ = diagnostics.log(LogLevel::Warn, "local", &error);
                        trackers.get_mut("local").unwrap().failure(error, now);
                    }
                }
            }
            local_scan_ms = now_ms().saturating_sub(scan_started);
            let _ = diagnostics.log(
                LogLevel::Debug,
                "local",
                &format!("scan completed in {local_scan_ms}ms"),
            );
            last_local = now;
        }

        drain_ready_remotes(
            &mut remote_tasks,
            &mut trackers,
            &mut schedules,
            &mut in_flight,
            &mut host_health,
            &diagnostics,
            remote_timing,
        );
        spawn_due_remotes(&hosts, &schedules, &mut in_flight, &mut remote_tasks, now);

        let snapshots: Vec<HostSnapshot> = std::iter::once("local")
            .chain(hosts.iter().map(String::as_str))
            .filter_map(|host| trackers.get(host).and_then(HostTracker::snapshot).cloned())
            .collect();
        if !same_snapshot_content(&previous_published, &snapshots)
            || now.saturating_sub(last_publish) >= SNAPSHOT_HEARTBEAT_MS
        {
            publish_snapshot(&snapshots, now)?;
            let health = HealthSnapshot {
                generated_at_ms: now,
                local_scan_ms,
                hosts: hosts
                    .iter()
                    .filter_map(|host| host_health.get(host).cloned())
                    .collect(),
            };
            diagnostics.publish_health(&health)?;
            previous_published = snapshots.clone();
            last_publish = now;
        }
        let aggregate = aggregate_online(snapshots.iter());
        publish_widget_if_changed(
            &mut previous_widget,
            status_widget_with(aggregate, &colors),
            |widget| {
                tmux.set_global_option("@seer_widget", widget)?;
                tmux.refresh_status();
                Ok(())
            },
        )?;

        for host in &snapshots {
            let was_online = previous_online.get(&host.host).copied();
            if host.online {
                let can_notify = !initial && was_online != Some(false);
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
                                    let message = format_notification(
                                        &host.host,
                                        &session.name,
                                        &pane.project,
                                        &pane.agent.to_string(),
                                        transition,
                                    );
                                    notify_clients(&tmux, notification_duration, &message, now);
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
        initial = false;

        tokio::select! {
            _ = time::sleep(LOOP_INTERVAL) => {}
            result = tokio::signal::ctrl_c() => {
                result.context("failed to listen for shutdown")?;
                return Ok(());
            }
        }
    }
}

fn reconcile_hosts(
    configured: &str,
    hosts: &mut Vec<String>,
    trackers: &mut HashMap<String, HostTracker>,
    previous: &mut HashMap<AgentKey, AgentState>,
    previous_online: &mut HashMap<String, bool>,
) -> bool {
    let next = configured
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if *hosts == next {
        return false;
    }

    let retained = next.iter().map(String::as_str).collect::<HashSet<_>>();
    trackers.retain(|host, _| host == "local" || retained.contains(host.as_str()));
    previous.retain(|key, _| key.host == "local" || retained.contains(key.host.as_str()));
    previous_online.retain(|host, _| host == "local" || retained.contains(host.as_str()));
    for host in &next {
        trackers
            .entry(host.clone())
            .or_insert_with(|| HostTracker::new(host));
    }
    *hosts = next;
    true
}

fn spawn_due_remotes(
    hosts: &[String],
    schedules: &HashMap<String, HostSchedule>,
    in_flight: &mut HashSet<String>,
    tasks: &mut JoinSet<RemoteResult>,
    now: u64,
) {
    let ssh = std::env::var_os("TMUX_SEER_SSH").unwrap_or_else(|| "ssh".into());
    for host in hosts {
        if in_flight.contains(host)
            || !schedules
                .get(host)
                .is_some_and(|schedule| schedule.is_due(now))
        {
            continue;
        }
        in_flight.insert(host.clone());
        let host = host.clone();
        let ssh = ssh.clone();
        tasks.spawn(async move { collect_remote(host, ssh, now).await });
    }
}

async fn collect_remote(host: String, ssh: std::ffi::OsString, started_at_ms: u64) -> RemoteResult {
    let result = async {
        let args = remote_snapshot_args(&host)?;
        let output = time::timeout(
            REMOTE_TIMEOUT,
            Command::new(ssh).args(args).stdin(Stdio::null()).output(),
        )
        .await
        .with_context(|| format!("SSH snapshot timed out for {host}"))?
        .with_context(|| format!("failed to run SSH for {host}"))?;
        if !output.status.success() {
            bail!(
                "{}",
                String::from_utf8_lossy(&output.stderr).trim().to_owned()
            );
        }
        let snapshot: HostSnapshot = serde_json::from_slice(&output.stdout)
            .with_context(|| format!("invalid snapshot from {host}"))?;
        if snapshot.schema_version != SCHEMA_VERSION {
            bail!("snapshot schema mismatch from {host}");
        }
        Ok::<_, anyhow::Error>(snapshot)
    }
    .await
    .map_err(|error| error.to_string());
    RemoteResult {
        host,
        started_at_ms,
        finished_at_ms: now_ms(),
        result,
    }
}

fn drain_ready_remotes(
    tasks: &mut JoinSet<RemoteResult>,
    trackers: &mut HashMap<String, HostTracker>,
    schedules: &mut HashMap<String, HostSchedule>,
    in_flight: &mut HashSet<String>,
    health: &mut HashMap<String, HostHealth>,
    diagnostics: &Diagnostics,
    timing: RemoteTiming,
) -> usize {
    let mut applied = 0;
    while let Some(result) = tasks.try_join_next() {
        match result {
            Ok(remote) => {
                in_flight.remove(&remote.host);
                let succeeded = remote.result.is_ok();
                let error = remote.result.as_ref().err().cloned();
                let latency_ms = remote.finished_at_ms.saturating_sub(remote.started_at_ms);
                record_remote_result(trackers, &remote.host, remote.result, remote.finished_at_ms);
                if let Some(schedule) = schedules.get_mut(&remote.host) {
                    if succeeded {
                        schedule.success(remote.finished_at_ms, timing.interval_ms);
                    } else {
                        schedule.failure(
                            remote.finished_at_ms,
                            timing.interval_ms,
                            timing.maximum_backoff_ms,
                        );
                    }
                    let online = trackers
                        .get(&remote.host)
                        .and_then(HostTracker::snapshot)
                        .is_some_and(|snapshot| snapshot.online);
                    health.insert(
                        remote.host.clone(),
                        HostHealth {
                            host: remote.host.clone(),
                            online,
                            latency_ms,
                            failures: schedule.failures(),
                            next_retry_ms: schedule.next_due_ms(),
                            last_error: error.clone(),
                        },
                    );
                }
                if let Some(error) = error {
                    let _ = diagnostics.log(LogLevel::Warn, &remote.host, &error);
                } else {
                    let _ = diagnostics.log(
                        LogLevel::Debug,
                        &remote.host,
                        &format!("scan completed in {latency_ms}ms"),
                    );
                }
                applied += 1;
            }
            Err(error) => {
                let _ = diagnostics.log(
                    LogLevel::Error,
                    "remote",
                    &format!("collector task failed: {error}"),
                );
            }
        }
    }
    applied
}

fn record_remote_result(
    trackers: &mut HashMap<String, HostTracker>,
    host: &str,
    result: std::result::Result<HostSnapshot, String>,
    now: u64,
) {
    let Some(tracker) = trackers.get_mut(host) else {
        return;
    };
    match result {
        Ok(snapshot) => tracker.success(snapshot),
        Err(error) => tracker.failure(error, now),
    }
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
    fn host_configuration_changes_reconcile_runtime_state() {
        let mut hosts = vec!["mac".to_owned(), "old".to_owned()];
        let mut trackers = HashMap::from([
            ("local".into(), HostTracker::new("local")),
            ("mac".into(), HostTracker::new("mac")),
            ("old".into(), HostTracker::new("old")),
        ]);
        let old_key = AgentKey {
            host: "old".into(),
            session_id: "$1".into(),
            window_id: "@1".into(),
            pane_id: "%1".into(),
        };
        let mut previous = HashMap::from([(old_key, AgentState::Working)]);
        let mut previous_online = HashMap::from([("old".into(), true)]);

        let changed = reconcile_hosts(
            "mac vps",
            &mut hosts,
            &mut trackers,
            &mut previous,
            &mut previous_online,
        );

        assert!(changed);
        assert_eq!(hosts, ["mac", "vps"]);
        assert!(trackers.contains_key("local"));
        assert!(trackers.contains_key("mac"));
        assert!(trackers.contains_key("vps"));
        assert!(!trackers.contains_key("old"));
        assert!(previous.is_empty());
        assert!(previous_online.is_empty());
    }

    #[test]
    fn unchanged_host_configuration_preserves_runtime_state() {
        let mut hosts = vec!["mac".to_owned()];
        let mut trackers = HashMap::from([
            ("local".into(), HostTracker::new("local")),
            ("mac".into(), HostTracker::new("mac")),
        ]);
        let mut previous = HashMap::new();
        let mut previous_online = HashMap::new();

        assert!(!reconcile_hosts(
            "mac",
            &mut hosts,
            &mut trackers,
            &mut previous,
            &mut previous_online,
        ));
    }

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

    #[test]
    fn remote_failure_changes_only_originating_host() {
        let mut trackers = HashMap::from([
            ("mac".into(), HostTracker::new("mac")),
            ("zapsign".into(), HostTracker::new("zapsign")),
        ]);
        trackers
            .get_mut("mac")
            .unwrap()
            .success(HostSnapshot::empty("mac", 1));
        trackers
            .get_mut("zapsign")
            .unwrap()
            .success(HostSnapshot::empty("zapsign", 1));

        record_remote_result(&mut trackers, "mac", Err("timeout".into()), 2);
        record_remote_result(&mut trackers, "mac", Err("timeout".into()), 3);

        assert!(!trackers["mac"].snapshot().unwrap().online);
        assert!(trackers["zapsign"].snapshot().unwrap().online);
    }

    #[tokio::test]
    async fn ready_remote_result_is_applied_while_another_host_is_still_pending() {
        let mut tasks = JoinSet::new();
        tasks.spawn(std::future::pending::<RemoteResult>());
        tasks.spawn(async {
            RemoteResult {
                host: "fast".into(),
                started_at_ms: 10,
                finished_at_ms: 20,
                result: Ok(HostSnapshot::empty("fast", 20)),
            }
        });
        tokio::task::yield_now().await;
        let mut trackers = HashMap::from([
            ("slow".into(), HostTracker::new("slow")),
            ("fast".into(), HostTracker::new("fast")),
        ]);
        let mut schedules = HashMap::from([
            ("slow".into(), HostSchedule::immediate()),
            ("fast".into(), HostSchedule::immediate()),
        ]);
        let mut in_flight = HashSet::from(["slow".into(), "fast".into()]);
        let mut health = HashMap::new();
        let directory = tempfile::tempdir().unwrap();
        let diagnostics = Diagnostics::at(directory.path(), 1_024, LogLevel::Warn).unwrap();

        let applied = drain_ready_remotes(
            &mut tasks,
            &mut trackers,
            &mut schedules,
            &mut in_flight,
            &mut health,
            &diagnostics,
            RemoteTiming {
                interval_ms: 2_000,
                maximum_backoff_ms: 60_000,
            },
        );

        assert_eq!(applied, 1);
        assert!(trackers["fast"].snapshot().unwrap().online);
        assert_eq!(health["fast"].latency_ms, 10);
        assert_eq!(health["fast"].failures, 0);
        assert!(!in_flight.contains("fast"));
        assert!(in_flight.contains("slow"));
        tasks.abort_all();
    }
}
