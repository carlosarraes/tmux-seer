use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::{popup::PopupLease, snapshot::AgentKey, tmux::Tmux};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NavigationTarget {
    Host {
        host: String,
    },
    Session {
        host: String,
        session_id: String,
    },
    Window {
        host: String,
        session_id: String,
        window_id: String,
    },
    Agent(AgentKey),
}

#[derive(Debug, Clone)]
pub struct Navigator {
    tmux: Tmux,
}

impl Navigator {
    pub fn new(tmux: Tmux) -> Self {
        Self { tmux }
    }

    pub fn navigate(&self, target: &NavigationTarget, client: Option<&str>) -> Result<()> {
        match target {
            NavigationTarget::Session { host, session_id } if host == "local" => {
                validate_tmux_id(session_id, '$', "session")?;
                self.switch_client(session_id, client)
            }
            NavigationTarget::Window {
                host,
                session_id,
                window_id,
            } if host == "local" => {
                validate_tmux_id(session_id, '$', "session")?;
                validate_tmux_id(window_id, '@', "window")?;
                self.switch_client(&format!("{session_id}:{window_id}"), client)
            }
            NavigationTarget::Host { host } => self.remote_attach(host, None, None, None, client),
            NavigationTarget::Session { host, session_id } => {
                self.remote_attach(host, Some(session_id), None, None, client)
            }
            NavigationTarget::Window {
                host,
                session_id,
                window_id,
            } => self.remote_attach(host, Some(session_id), Some(window_id), None, client),
            NavigationTarget::Agent(key) => self.jump(key, client),
        }
    }

    pub fn rename_session(&self, host: &str, session_id: &str, name: &str) -> Result<()> {
        validate_host(host)?;
        validate_tmux_id(session_id, '$', "session")?;
        if name.trim().is_empty() {
            bail!("session name cannot be empty");
        }
        if host == "local" {
            self.tmux
                .output(["rename-session", "-t", session_id, name])?;
            return Ok(());
        }

        let ssh = std::env::var_os("TMUX_SEER_SSH").unwrap_or_else(|| "ssh".into());
        let command = remote_login_command(&format!(
            "tmux rename-session -t {} {}",
            shell_quote(session_id),
            shell_quote(name),
        ));
        let output = Command::new(ssh)
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=2",
                host,
                &command,
            ])
            .output()
            .with_context(|| format!("failed to rename tmux session on {host}"))?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr);
            bail!("remote tmux rename failed on {host}: {}", error.trim());
        }
        Ok(())
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
        self.switch_client(&key.pane_id, client)
    }

    fn switch_client(&self, session_id: &str, client: Option<&str>) -> Result<()> {
        match client {
            Some(client) => {
                self.tmux
                    .output(["switch-client", "-c", client, "-t", session_id])?;
            }
            None => {
                self.tmux.output(["switch-client", "-t", session_id])?;
            }
        }
        Ok(())
    }

    fn remote_jump(&self, key: &AgentKey, client: Option<&str>) -> Result<()> {
        self.remote_attach(
            &key.host,
            Some(&key.session_id),
            Some(&key.window_id),
            Some(&key.pane_id),
            client,
        )
    }

    fn remote_attach(
        &self,
        host: &str,
        session_id: Option<&str>,
        window_id: Option<&str>,
        pane_id: Option<&str>,
        client: Option<&str>,
    ) -> Result<()> {
        validate_host(host)?;
        if let Some(session_id) = session_id {
            validate_tmux_id(session_id, '$', "session")?;
        }
        if let Some(window_id) = window_id {
            validate_tmux_id(window_id, '@', "window")?;
        }
        if let Some(pane_id) = pane_id {
            validate_tmux_id(pane_id, '%', "pane")?;
        }

        let remote = match (session_id, window_id, pane_id) {
            (None, None, None) => "exec tmux attach-session".to_owned(),
            (Some(session), None, None) => {
                format!("exec tmux attach-session -t {}", shell_quote(session))
            }
            (Some(session), Some(window), None) => format!(
                "tmux select-window -t {} && exec tmux attach-session -t {}",
                shell_quote(&format!("{session}:{window}")),
                shell_quote(session),
            ),
            (Some(session), Some(window), Some(pane)) => format!(
                "tmux select-window -t {} && tmux select-pane -t {} && exec tmux attach-session -t {}",
                shell_quote(&format!("{session}:{window}")),
                shell_quote(pane),
                shell_quote(session),
            ),
            _ => bail!("incomplete remote tmux target"),
        };
        let ssh = std::env::var_os("TMUX_SEER_SSH").unwrap_or_else(|| "ssh".into());
        let _suppression = client.map(PopupLease::new).transpose()?;
        let remote = remote_login_command(&remote);
        let mut child = Command::new(ssh)
            .args([
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=2",
                "-tt",
                host,
                &remote,
            ])
            .spawn()
            .with_context(|| format!("failed to open remote tmux on {host}"))?;
        let status = child.wait()?;
        if !status.success() {
            bail!("remote tmux attachment failed on {host}");
        }
        Ok(())
    }
}

fn validate_target(key: &AgentKey) -> Result<()> {
    validate_host(&key.host)?;
    if !key.session_id.starts_with('$')
        || !key.window_id.starts_with('@')
        || !key.pane_id.starts_with('%')
    {
        bail!("invalid tmux target identifiers");
    }
    Ok(())
}

fn validate_host(host: &str) -> Result<()> {
    if host.is_empty()
        || !host
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
    {
        bail!("invalid host alias: {host}");
    }
    Ok(())
}

fn validate_tmux_id(value: &str, prefix: char, label: &str) -> Result<()> {
    if !value.starts_with(prefix) {
        bail!("invalid {label} identifier: {value}");
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn remote_login_command(command: &str) -> String {
    format!("exec \"$SHELL\" -lc {}", shell_quote(command))
}
