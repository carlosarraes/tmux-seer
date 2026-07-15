use anyhow::Result;

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
    ensure_option(&tmux, "@seer_hosts", "")?;
    ensure_option(&tmux, "@seer_popup_width", "76")?;
    ensure_option(&tmux, "@seer_popup_height", "70%")?;
    ensure_option(&tmux, "@seer_remote_interval_ms", "2000")?;
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
    tmux.output([
        "run-shell",
        "-b",
        &format!("{} daemon", shell_quote(binary)),
    ])?;
    tmux.refresh_status();
    Ok(())
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
