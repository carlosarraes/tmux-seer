use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use fs2::FileExt;

use crate::{
    adapters::{normalize, NativeEvent},
    model::{AgentKind, AgentState, EventKind},
    reducer::{reduce, AgentRecord, CodexPaneTracker},
    snapshot::{parse_tmux_rows_with_processes, HostSnapshot, ProcessTable},
};

pub const PANE_OPTIONS: &[&str] = &[
    "@seer_agent_kind",
    "@seer_state",
    "@seer_state_since_ms",
    "@seer_session_id",
    "@seer_turn_id",
    "@seer_reason",
    "@seer_record",
    "@seer_codex_tracker",
];

const SNAPSHOT_FORMAT: &str = concat!(
    "#{session_id}\x1f#{session_name}\x1f",
    "#{window_id}\x1f#{window_index}\x1f#{window_name}\x1f",
    "#{pane_id}\x1f#{pane_index}\x1f#{pane_current_path}\x1f",
    "#{pane_pid}\x1f#{pane_current_command}\x1f",
    "#{@seer_agent_kind}\x1f#{@seer_state}\x1f#{@seer_state_since_ms}\x1f",
    "#{@seer_session_id}\x1f#{@seer_turn_id}\x1f#{@seer_reason}"
);

#[derive(Debug, Clone)]
pub struct Tmux {
    program: PathBuf,
}

impl Default for Tmux {
    fn default() -> Self {
        Self::new()
    }
}

impl Tmux {
    pub fn new() -> Self {
        Self {
            program: std::env::var_os("TMUX_SEER_TMUX")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("tmux")),
        }
    }

    pub fn output<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = Command::new(&self.program)
            .args(args)
            .output()
            .with_context(|| format!("failed to run {}", self.program.display()))?;
        if !output.status.success() {
            bail!(
                "tmux failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        String::from_utf8(output.stdout).context("tmux returned non-UTF-8 output")
    }

    pub fn show_pane_option(&self, pane: &str, option: &str) -> Option<String> {
        self.output(["show-options", "-p", "-v", "-t", pane, option])
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }

    pub fn set_pane_option(&self, pane: &str, option: &str, value: &str) -> Result<()> {
        self.output(["set-option", "-p", "-t", pane, option, value])?;
        Ok(())
    }

    pub fn unset_pane_option(&self, pane: &str, option: &str) -> Result<()> {
        self.output(["set-option", "-p", "-u", "-t", pane, option])?;
        Ok(())
    }

    pub fn apply_hook(&self, pane: &str, native: NativeEvent) -> Result<()> {
        let event = normalize(native);
        if event.agent == AgentKind::Codex {
            let _lock = acquire_hook_lock(pane)?;
            return self.apply_codex_hook(pane, event);
        }
        if event.kind == EventKind::Ended {
            for option in PANE_OPTIONS {
                let _ = self.unset_pane_option(pane, option);
            }
            return Ok(());
        }

        let current = self
            .show_pane_option(pane, "@seer_record")
            .and_then(|value| serde_json::from_str::<AgentRecord>(&value).ok());
        let record = reduce(current.clone(), event, now_ms());
        self.publish_record(pane, current.as_ref(), &record)
    }

    fn apply_codex_hook(&self, pane: &str, event: crate::model::NormalizedEvent) -> Result<()> {
        let current = self
            .show_pane_option(pane, "@seer_record")
            .and_then(|value| serde_json::from_str::<AgentRecord>(&value).ok());
        let mut tracker = self
            .show_pane_option(pane, "@seer_codex_tracker")
            .and_then(|value| serde_json::from_str::<CodexPaneTracker>(&value).ok())
            .unwrap_or_default();
        let previous_tracker = tracker.clone();
        let record = tracker.apply(event, now_ms());

        if tracker != previous_tracker {
            self.set_pane_option(
                pane,
                "@seer_codex_tracker",
                &serde_json::to_string(&tracker)?,
            )?;
        }

        let Some(record) = record else {
            for option in PANE_OPTIONS {
                let _ = self.unset_pane_option(pane, option);
            }
            return Ok(());
        };
        self.publish_record(pane, current.as_ref(), &record)
    }

    fn publish_record(
        &self,
        pane: &str,
        current: Option<&AgentRecord>,
        record: &AgentRecord,
    ) -> Result<()> {
        if current == Some(record) {
            return Ok(());
        }

        self.set_pane_option(pane, "@seer_agent_kind", agent_slug(record.agent))?;
        self.set_pane_option(pane, "@seer_state", state_slug(record.state))?;
        self.set_pane_option(
            pane,
            "@seer_state_since_ms",
            &record.state_since_ms.to_string(),
        )?;
        self.set_pane_option(pane, "@seer_session_id", &record.session_id)?;
        set_optional(self, pane, "@seer_turn_id", record.active_turn.as_deref())?;
        set_optional(self, pane, "@seer_reason", record.reason.as_deref())?;
        self.set_pane_option(pane, "@seer_record", &serde_json::to_string(&record)?)?;
        Ok(())
    }

    pub fn snapshot(&self, host: &str) -> Result<HostSnapshot> {
        let rows = self.output(["list-panes", "-a", "-F", SNAPSHOT_FORMAT])?;
        let process_program = std::env::var_os("TMUX_SEER_PS").unwrap_or_else(|| "ps".into());
        let processes = Command::new(process_program)
            .args(["-Ao", "pid=,ppid=,command="])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| ProcessTable::parse(&String::from_utf8_lossy(&output.stdout)))
            .unwrap_or_default();
        parse_tmux_rows_with_processes(host, now_ms(), &rows, &processes)
    }

    pub fn show_global_option(&self, option: &str) -> Option<String> {
        self.output(["show-options", "-g", "-v", option])
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }

    pub fn set_global_option(&self, option: &str, value: &str) -> Result<()> {
        self.output(["set-option", "-g", option, value])?;
        Ok(())
    }

    pub fn unset_global_option(&self, option: &str) -> Result<()> {
        self.output(["set-option", "-g", "-u", option])?;
        Ok(())
    }

    pub fn clients(&self) -> Result<Vec<String>> {
        Ok(self
            .output(["list-clients", "-F", "#{client_tty}"])?
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    pub fn display_message(&self, client: &str, duration_ms: u64, message: &str) -> Result<()> {
        self.output([
            "display-message",
            "-t",
            client,
            "-d",
            &duration_ms.to_string(),
            message,
        ])?;
        Ok(())
    }

    pub fn refresh_status(&self) {
        let _ = self.output(["refresh-client", "-S"]);
    }
}

fn acquire_hook_lock(pane: &str) -> Result<File> {
    let directory = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("tmux-seer")
        .join("hooks");
    fs::create_dir_all(&directory)?;
    let safe_pane = pane
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join(format!("{safe_pane}.lock")))?;
    FileExt::lock_exclusive(&lock)?;
    Ok(lock)
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn set_optional(tmux: &Tmux, pane: &str, option: &str, value: Option<&str>) -> Result<()> {
    match value {
        Some(value) => tmux.set_pane_option(pane, option, value),
        None => {
            let _ = tmux.unset_pane_option(pane, option);
            Ok(())
        }
    }
}

fn agent_slug(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
        AgentKind::Pi => "pi",
    }
}

fn state_slug(state: AgentState) -> &'static str {
    match state {
        AgentState::Working => "working",
        AgentState::Idle => "idle",
        AgentState::NeedsInput => "needs_input",
        AgentState::Untracked => "untracked",
    }
}
