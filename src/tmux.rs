use std::{
    ffi::OsStr,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};

use crate::{
    hook_state::load_for_tmux_rows,
    snapshot::{
        parse_tmux_rows_with_cached_processes_and_states,
        parse_tmux_rows_with_processes_and_states, HostSnapshot, ProcessTable,
    },
};

const SNAPSHOT_FORMAT: &str = concat!(
    "#{session_id}\x1f#{session_name}\x1f",
    "#{window_id}\x1f#{window_index}\x1f#{window_name}\x1f",
    "#{pane_id}\x1f#{pane_index}\x1f#{pane_current_path}\x1f",
    "#{pane_pid}\x1f#{pane_current_command}\x1f",
    "#{@seer_agent_kind}\x1f#{@seer_state}\x1f#{@seer_state_since_ms}\x1f",
    "#{@seer_session_id}\x1f#{@seer_turn_id}\x1f#{@seer_reason}\x1f",
    "#{socket_path}"
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

    pub fn snapshot(&self, host: &str) -> Result<HostSnapshot> {
        let processes = self.process_table();
        self.snapshot_with_processes(host, &processes)
    }

    pub fn snapshot_with_processes(
        &self,
        host: &str,
        processes: &ProcessTable,
    ) -> Result<HostSnapshot> {
        self.snapshot_with_process_freshness(host, processes, true)
    }

    pub fn snapshot_with_cached_processes(
        &self,
        host: &str,
        processes: &ProcessTable,
    ) -> Result<HostSnapshot> {
        self.snapshot_with_process_freshness(host, processes, false)
    }

    fn snapshot_with_process_freshness(
        &self,
        host: &str,
        processes: &ProcessTable,
        processes_are_fresh: bool,
    ) -> Result<HostSnapshot> {
        let rows = self.pane_rows()?;
        self.snapshot_from_rows(host, &rows, processes, processes_are_fresh)
    }

    pub fn pane_rows(&self) -> Result<String> {
        self.output(["list-panes", "-a", "-F", SNAPSHOT_FORMAT])
    }

    pub fn snapshot_from_rows(
        &self,
        host: &str,
        rows: &str,
        processes: &ProcessTable,
        processes_are_fresh: bool,
    ) -> Result<HostSnapshot> {
        let states = load_for_tmux_rows(rows);
        let snapshot = if processes_are_fresh {
            parse_tmux_rows_with_processes_and_states(
                host,
                now_ms(),
                rows,
                processes,
                &states.states,
            )?
        } else {
            parse_tmux_rows_with_cached_processes_and_states(
                host,
                now_ms(),
                rows,
                processes,
                &states.states,
            )?
        };
        let active_agent_panes = snapshot
            .agents()
            .map(|pane| pane.key.pane_id.clone())
            .collect();
        if processes_are_fresh {
            states.reconcile(&active_agent_panes)?;
        }
        Ok(snapshot)
    }

    pub fn process_table(&self) -> ProcessTable {
        let process_program = std::env::var_os("TMUX_SEER_PS").unwrap_or_else(|| "ps".into());
        Command::new(process_program)
            .args(["-Ao", "pid=,ppid=,command="])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| ProcessTable::parse(&String::from_utf8_lossy(&output.stdout)))
            .unwrap_or_default()
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

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
