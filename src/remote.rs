use std::{ffi::OsString, path::PathBuf, process::Stdio, time::Duration};

use anyhow::{bail, Context, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    sync::mpsc,
    time,
};

use crate::snapshot::{HostSnapshot, SCHEMA_VERSION};

const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_STDERR_BYTES: u64 = 4 * 1024;

#[derive(Debug, Clone)]
pub enum RemoteEvent {
    Snapshot {
        host: String,
        snapshot: HostSnapshot,
    },
    Disconnected {
        host: String,
        error: String,
    },
}

pub fn remote_stream_args(host: &str) -> Result<Vec<String>> {
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
        "-o".into(),
        "ServerAliveInterval=15".into(),
        "-o".into(),
        "ServerAliveCountMax=2".into(),
        host.into(),
        format!(
            "exec \"$SHELL\" -lc 'socket=$(tmux display-message -p \"#{{socket_path}}\"); export TMUX=\"$socket,0,0\"; exec \"$HOME/.local/bin/tmux-seer\" stream --host {host}'"
        ),
    ])
}

pub async fn read_remote_once(
    host: String,
    ssh: PathBuf,
    sender: mpsc::Sender<RemoteEvent>,
) -> Result<usize> {
    let mut child = Command::new(ssh)
        .args(remote_stream_args(&host)?)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("failed to start SSH stream for {host}"))?;
    let stdout = child.stdout.take().context("SSH stream has no stdout")?;
    let stderr = child.stderr.take().context("SSH stream has no stderr")?;
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr
            .take(MAX_STDERR_BYTES)
            .read_to_end(&mut bytes)
            .await
            .map(|_| bytes)
    });
    let mut reader = BufReader::new(stdout);
    let mut snapshots = 0;
    while let Some(line) = read_bounded_line(&mut reader).await? {
        let snapshot: HostSnapshot = serde_json::from_slice(&line)
            .with_context(|| format!("invalid snapshot stream from {host}"))?;
        if snapshot.schema_version != SCHEMA_VERSION {
            bail!("snapshot schema mismatch from {host}");
        }
        if snapshot.host != host {
            bail!("snapshot host mismatch from {host}");
        }
        sender
            .send(RemoteEvent::Snapshot {
                host: host.clone(),
                snapshot,
            })
            .await
            .context("remote snapshot receiver stopped")?;
        snapshots += 1;
    }
    let status = child.wait().await?;
    let stderr = stderr_task.await??;
    if !status.success() {
        bail!("{}", String::from_utf8_lossy(&stderr).trim().to_owned());
    }
    Ok(snapshots)
}

pub async fn supervise(host: String, sender: mpsc::Sender<RemoteEvent>, maximum_backoff_ms: u64) {
    let ssh: OsString = std::env::var_os("TMUX_SEER_SSH").unwrap_or_else(|| "ssh".into());
    let mut failures = 0_u8;
    loop {
        let result = read_remote_once(host.clone(), PathBuf::from(&ssh), sender.clone()).await;
        failures = if result.as_ref().is_ok_and(|count| *count > 0) {
            1
        } else {
            failures.saturating_add(1)
        };
        let error = result
            .err()
            .map(|error| error.to_string())
            .unwrap_or_else(|| "SSH stream closed".into());
        if sender
            .send(RemoteEvent::Disconnected {
                host: host.clone(),
                error,
            })
            .await
            .is_err()
        {
            return;
        }
        time::sleep(Duration::from_millis(reconnect_delay_ms(
            failures,
            maximum_backoff_ms,
        )))
        .await;
    }
}

pub fn reconnect_delay_ms(failures: u8, maximum_backoff_ms: u64) -> u64 {
    1_000_u64
        .saturating_mul(1_u64 << failures.saturating_sub(1).min(10))
        .min(maximum_backoff_ms.max(1_000))
}

async fn read_bounded_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        let length = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len().saturating_add(length) > MAX_LINE_BYTES {
            bail!("remote snapshot line exceeds {MAX_LINE_BYTES} bytes");
        }
        line.extend_from_slice(&available[..length]);
        reader.consume(length);
        if line.last() == Some(&b'\n') {
            line.pop();
            return Ok(Some(line));
        }
    }
}
