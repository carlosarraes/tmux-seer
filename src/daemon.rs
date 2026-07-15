use std::{
    collections::{hash_map::DefaultHasher, HashMap, HashSet},
    fs::{self, File, OpenOptions},
    hash::{Hash, Hasher},
    path::PathBuf,
    process::Stdio,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use fs2::FileExt;
use tokio::{process::Command, task::JoinSet, time};

use crate::{
    model::AgentState,
    snapshot::{
        aggregate_online, status_widget_with, AgentKey, AggregateSnapshot, HostSnapshot,
        StatusColors, SCHEMA_VERSION,
    },
    tmux::{now_ms, Tmux},
};

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

pub fn popup_option_name(client: &str) -> String {
    let safe: String = client
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect();
    format!("@seer_popup_{safe}")
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
    runtime_directory().join(format!("{}.json", server_key()))
}

pub async fn run() -> Result<()> {
    let _lock = match acquire_lock()? {
        Some(lock) => lock,
        None => return Ok(()),
    };
    let tmux = Tmux::new();
    let hosts = option(&tmux, "@seer_hosts", "")
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let remote_interval = option(&tmux, "@seer_remote_interval_ms", "2000")
        .parse::<u64>()
        .unwrap_or(2_000)
        .max(500);
    let notification_duration = option(&tmux, "@seer_notify_ms", "4000")
        .parse::<u64>()
        .unwrap_or(4_000);
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
    let mut previous: HashMap<AgentKey, AgentState> = HashMap::new();
    let mut previous_online: HashMap<String, bool> = HashMap::new();
    let mut initial = true;
    let mut last_remote = 0;

    loop {
        let now = now_ms();
        match tmux.snapshot("local") {
            Ok(snapshot) => trackers.get_mut("local").unwrap().success(snapshot),
            Err(error) => trackers
                .get_mut("local")
                .unwrap()
                .failure(error.to_string(), now),
        }

        if now.saturating_sub(last_remote) >= remote_interval {
            collect_remotes(&hosts, &mut trackers, now).await;
            last_remote = now;
        }

        let snapshots: Vec<HostSnapshot> = std::iter::once("local")
            .chain(hosts.iter().map(String::as_str))
            .filter_map(|host| trackers.get(host).and_then(HostTracker::snapshot).cloned())
            .collect();
        publish_snapshot(&snapshots, now)?;
        let aggregate = aggregate_online(snapshots.iter());
        tmux.set_global_option("@seer_widget", &status_widget_with(aggregate, &colors))?;
        tmux.refresh_status();

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
            _ = time::sleep(Duration::from_millis(500)) => {}
            result = tokio::signal::ctrl_c() => {
                result.context("failed to listen for shutdown")?;
                return Ok(());
            }
        }
    }
}

async fn collect_remotes(hosts: &[String], trackers: &mut HashMap<String, HostTracker>, now: u64) {
    let ssh = std::env::var_os("TMUX_SEER_SSH").unwrap_or_else(|| "ssh".into());
    let mut tasks = JoinSet::new();
    for host in hosts {
        let host = host.clone();
        let ssh = ssh.clone();
        tasks.spawn(async move {
            let result = async {
                let args = remote_snapshot_args(&host)?;
                let output = Command::new(ssh)
                    .args(args)
                    .stdin(Stdio::null())
                    .output()
                    .await
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
            (host, result)
        });
    }

    while let Some(result) = tasks.join_next().await {
        match result {
            Ok((host, remote_result)) => {
                record_remote_result(trackers, &host, remote_result, now);
            }
            Err(error) => eprintln!("Seer remote collector task failed: {error}"),
        }
    }
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
        let popup_until = tmux
            .show_global_option(&popup_option_name(&client))
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or_default();
        if popup_until <= now {
            let _ = tmux.display_message(&client, duration_ms, message);
        }
    }
}

fn publish_snapshot(hosts: &[HostSnapshot], now: u64) -> Result<()> {
    let path = runtime_snapshot_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("json.tmp");
    let snapshot = AggregateSnapshot {
        schema_version: SCHEMA_VERSION,
        generated_at_ms: now,
        hosts: hosts.to_vec(),
    };
    fs::write(&temporary, serde_json::to_vec(&snapshot)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn acquire_lock() -> Result<Option<File>> {
    let directory = runtime_directory();
    fs::create_dir_all(&directory)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(format!("{}.lock", server_key())))?;
    match lock.try_lock_exclusive() {
        Ok(()) => Ok(Some(lock)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn runtime_directory() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("tmux-seer")
}

fn server_key() -> u64 {
    let tmux = std::env::var("TMUX").unwrap_or_else(|_| "default".into());
    let identity = tmux.split(',').take(2).collect::<Vec<_>>().join(",");
    let mut hasher = DefaultHasher::new();
    identity.hash(&mut hasher);
    hasher.finish()
}

fn option(tmux: &Tmux, name: &str, default: &str) -> String {
    tmux.show_global_option(name)
        .unwrap_or_else(|| default.into())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
