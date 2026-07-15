use anyhow::{bail, Result};

use crate::{snapshot::AgentKey, tmux::Tmux};

const BRIDGE_SESSION: &str = "__seer_bridges";

#[derive(Debug, Clone)]
pub struct Navigator {
    tmux: Tmux,
}

impl Navigator {
    pub fn new(tmux: Tmux) -> Self {
        Self { tmux }
    }

    pub fn jump(&self, key: &AgentKey, client: Option<&str>) -> Result<()> {
        validate_target(key)?;
        if key.host == "local" {
            self.local_jump(key, client)
        } else {
            self.remote_jump(key, client)
        }
    }

    fn local_jump(&self, key: &AgentKey, client: Option<&str>) -> Result<()> {
        match client {
            Some(client) => {
                self.tmux
                    .output(["switch-client", "-c", client, "-t", &key.session_id])?;
            }
            None => {
                self.tmux.output(["switch-client", "-t", &key.session_id])?;
            }
        }
        self.tmux.output([
            "select-window",
            "-t",
            &format!("{}:{}", key.session_id, key.window_id),
        ])?;
        self.tmux.output(["select-pane", "-t", &key.pane_id])?;
        Ok(())
    }

    fn remote_jump(&self, key: &AgentKey, client: Option<&str>) -> Result<()> {
        let window_name = format!("seer:{}", key.host);
        let remote = format!(
            "tmux select-window -t {} \\; select-pane -t {} \\; attach-session -t {}",
            shell_quote(&format!("{}:{}", key.session_id, key.window_id)),
            shell_quote(&key.pane_id),
            shell_quote(&key.session_id),
        );
        let ssh_command = format!("ssh -tt {} {}", key.host, shell_quote(&remote));
        let has_bridge_session = self
            .tmux
            .output(["has-session", "-t", &format!("={BRIDGE_SESSION}")])
            .is_ok();
        if !has_bridge_session {
            self.tmux.output([
                "new-session",
                "-d",
                "-s",
                BRIDGE_SESSION,
                "-n",
                &window_name,
                &ssh_command,
            ])?;
        } else {
            let windows = self
                .tmux
                .output(["list-windows", "-t", BRIDGE_SESSION, "-F", "#{window_name}"])
                .unwrap_or_default();
            if windows.lines().any(|name| name == window_name) {
                self.tmux.output([
                    "respawn-pane",
                    "-k",
                    "-t",
                    &format!("{BRIDGE_SESSION}:{window_name}"),
                    &ssh_command,
                ])?;
            } else {
                self.tmux.output([
                    "new-window",
                    "-d",
                    "-t",
                    &format!("{BRIDGE_SESSION}:"),
                    "-n",
                    &window_name,
                    &ssh_command,
                ])?;
            }
        }

        let local_session = match client {
            Some(client) => self
                .tmux
                .output(["display-message", "-p", "-t", client, "#{session_id}"])?
                .trim()
                .to_owned(),
            None => self
                .tmux
                .output(["display-message", "-p", "#{session_id}"])?
                .trim()
                .to_owned(),
        };
        let _ = self.tmux.output([
            "link-window",
            "-s",
            &format!("={BRIDGE_SESSION}:{window_name}"),
            "-t",
            &format!("{local_session}:"),
        ]);
        let target = format!("{local_session}:{window_name}");
        match client {
            Some(client) => {
                self.tmux
                    .output(["switch-client", "-c", client, "-t", &target])?;
            }
            None => {
                self.tmux.output(["switch-client", "-t", &target])?;
            }
        }
        Ok(())
    }
}

fn validate_target(key: &AgentKey) -> Result<()> {
    if key.host.is_empty()
        || !key
            .host
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        bail!("invalid host alias: {}", key.host);
    }
    if !key.session_id.starts_with('$')
        || !key.window_id.starts_with('@')
        || !key.pane_id.starts_with('%')
    {
        bail!("invalid tmux target identifiers");
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
