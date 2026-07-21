use std::{path::Path, process::Command};

use anyhow::{Context, Result};

use crate::{model::AgentState, snapshot::status_widget, tmux::Tmux};

pub fn with_status_widget(status_right: &str) -> String {
    if status_right.contains("#{@seer_widget}") {
        return status_right.to_owned();
    }
    if status_right.trim().is_empty() {
        "#{@seer_widget}".into()
    } else {
        format!("{} #{{@seer_widget}}", status_right.trim_end())
    }
}

pub fn bootstrap(tmux: Tmux, binary: &str) -> Result<()> {
    ensure_option(&tmux, "@seer_key", "S")?;
    ensure_option(&tmux, "@seer_fullscreen_key", "s")?;
    ensure_option(&tmux, "@seer_hosts", "")?;
    ensure_option(&tmux, "@seer_popup_width", "76")?;
    ensure_option(&tmux, "@seer_popup_height", "70%")?;
    ensure_option(&tmux, "@seer_remote_interval_ms", "2000")?;
    ensure_option(&tmux, "@seer_remote_max_backoff_ms", "60000")?;
    ensure_option(&tmux, "@seer_log_level", "warn")?;
    ensure_option(&tmux, "@seer_notify_ms", "4000")?;
    ensure_option(&tmux, "@seer_color_working", "#9ece6a")?;
    ensure_option(&tmux, "@seer_color_idle", "#e0af68")?;
    ensure_option(&tmux, "@seer_color_input", "#7aa2f7")?;
    ensure_option(&tmux, "@seer_color_offline", "#565f89")?;
    ensure_option(&tmux, "@seer_widget", &status_widget(AgentState::Untracked))?;

    let existing = tmux.show_global_option("status-right").unwrap_or_default();
    let status = with_status_widget(&existing);
    if status != existing {
        tmux.set_global_option("status-right", &status)?;
    }

    let key = tmux
        .show_global_option("@seer_key")
        .unwrap_or_else(|| "S".into());
    let fullscreen_key = tmux
        .show_global_option("@seer_fullscreen_key")
        .unwrap_or_else(|| "s".into());
    let width = tmux
        .show_global_option("@seer_popup_width")
        .unwrap_or_else(|| "76".into());
    let height = tmux
        .show_global_option("@seer_popup_height")
        .unwrap_or_else(|| "70%".into());
    let popup_command = format!("{} popup --client '#{{client_tty}}'", shell_quote(binary));
    let display_command = format!(
        "display-popup -EE -T Seer -x R -y 0 -w {} -h {} {}",
        tmux_quote(&width),
        tmux_quote(&height),
        tmux_quote(&popup_command),
    );
    tmux.output(["bind-key", &key, "run-shell", "-C", &display_command])?;
    let fullscreen_command = format!(
        "display-popup -B -EE -T Seer -w {} -h {} {}",
        tmux_quote("100%"),
        tmux_quote("100%"),
        tmux_quote(&popup_command),
    );
    tmux.output([
        "bind-key",
        &fullscreen_key,
        "run-shell",
        "-C",
        &fullscreen_command,
    ])?;
    tmux.output([
        "run-shell",
        "-b",
        &format!("sleep 0.25; exec {} daemon", shell_quote(binary)),
    ])?;
    tmux.refresh_status();
    Ok(())
}

pub fn restart_existing_daemons() -> Result<()> {
    let Some(server_pid) = std::env::var("TMUX")
        .ok()
        .and_then(|value| value.split(',').nth(1)?.parse::<u32>().ok())
    else {
        return Ok(());
    };
    let ps = std::env::var_os("TMUX_SEER_PS").unwrap_or_else(|| "ps".into());
    let output = Command::new(ps)
        .args(["-Ao", "pid=,ppid=,command="])
        .output()
        .context("failed to inspect existing Seer daemons")?;
    if !output.status.success() {
        return Ok(());
    }
    let kill = std::env::var_os("TMUX_SEER_KILL").unwrap_or_else(|| "kill".into());
    for pid in daemon_pids_for_server(&String::from_utf8_lossy(&output.stdout), server_pid) {
        let _ = Command::new(&kill).arg(pid.to_string()).status();
    }
    Ok(())
}

pub fn daemon_pids_for_server(processes: &str, server_pid: u32) -> Vec<u32> {
    processes
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let parent = fields.next()?.parse::<u32>().ok()?;
            let executable = fields.next()?;
            let command = fields.next()?;
            let no_more_arguments = fields.next().is_none();
            let is_seer = Path::new(executable)
                .file_name()
                .and_then(|name| name.to_str())
                == Some("tmux-seer");
            (parent == server_pid && is_seer && command == "daemon" && no_more_arguments)
                .then_some(pid)
        })
        .collect()
}

fn ensure_option(tmux: &Tmux, name: &str, default: &str) -> Result<()> {
    if tmux.show_global_option(name).is_none() {
        tmux.set_global_option(name, default)?;
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn tmux_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
