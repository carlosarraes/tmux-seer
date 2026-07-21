use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{runtime, tmux::now_ms};

pub const DEFAULT_LOG_MAX_BYTES: u64 = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug,
    Warn,
    Error,
}

impl LogLevel {
    pub fn parse(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "debug" => Self::Debug,
            "error" => Self::Error,
            _ => Self::Warn,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Diagnostics {
    directory: PathBuf,
    max_bytes: u64,
    level: LogLevel,
}

impl Diagnostics {
    pub fn current(level: LogLevel) -> Result<Self> {
        Self::at(
            runtime::state_directory().join(runtime::current_server_id()),
            DEFAULT_LOG_MAX_BYTES,
            level,
        )
    }

    pub fn at(directory: impl AsRef<Path>, max_bytes: u64, level: LogLevel) -> Result<Self> {
        let directory = directory.as_ref().to_owned();
        runtime::ensure_private_directory(&directory)?;
        Ok(Self {
            directory,
            max_bytes: max_bytes.max(1),
            level,
        })
    }

    pub fn log(&self, level: LogLevel, component: &str, message: &str) -> Result<()> {
        if level < self.level {
            return Ok(());
        }
        let component = one_line(component);
        let message = one_line(message);
        let prefix = format!("{} {} {} ", now_ms(), level.label(), component);
        let budget = self
            .max_bytes
            .saturating_sub(prefix.len() as u64 + 1)
            .try_into()
            .unwrap_or(usize::MAX);
        let message = truncate_utf8(&message, budget);
        let line = format!("{prefix}{message}\n");
        let path = self.log_path();
        let existing = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if existing > 0 && existing.saturating_add(line.len() as u64) > self.max_bytes {
            let previous = self.directory.join("seer.log.1");
            let _ = fs::remove_file(&previous);
            fs::rename(&path, previous)
                .with_context(|| format!("failed to rotate Seer log {}", path.display()))?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("failed to open Seer log {}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(line.as_bytes())?;
        Ok(())
    }

    pub fn log_path(&self) -> PathBuf {
        self.directory.join("seer.log")
    }

    pub fn publish_health(&self, health: &HealthSnapshot) -> Result<()> {
        runtime::atomic_write(&health_path(), &serde_json::to_vec(health)?)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostHealth {
    pub host: String,
    pub online: bool,
    pub latency_ms: u64,
    pub failures: u32,
    pub next_retry_ms: u64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthSnapshot {
    pub generated_at_ms: u64,
    pub local_scan_ms: u64,
    pub hosts: Vec<HostHealth>,
}

pub fn health_path() -> PathBuf {
    runtime::current_server_directory().join("health.json")
}

pub fn load_health() -> Option<HealthSnapshot> {
    fs::read(health_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

pub fn format_health(health: &HealthSnapshot, now_ms: u64) -> String {
    let mut lines = vec![format!("local scan: {}ms", health.local_scan_ms)];
    for host in &health.hosts {
        let (_, line) = format_host_health(host, now_ms);
        lines.push(line);
    }
    lines.join("\n")
}

pub fn format_host_health(host: &HostHealth, now_ms: u64) -> (&'static str, String) {
    let (level, state) = if host.online {
        ("ok", "online")
    } else {
        ("warn", "offline")
    };
    let retry = host.next_retry_ms.saturating_sub(now_ms) / 1_000;
    let mut line = format!(
        "{}: {state}, {}ms, {} failures, retry in {retry}s",
        host.host, host.latency_ms, host.failures
    );
    if let Some(error) = &host.last_error {
        line.push_str(&format!(" ({})", one_line(error)));
    }
    (level, line)
}

pub fn print_logs(follow: bool) -> Result<()> {
    let path = runtime::state_directory()
        .join(runtime::current_server_id())
        .join("seer.log");
    if !path.exists() {
        println!("No Seer logs yet.");
        return Ok(());
    }
    let mut file = fs::File::open(&path)?;
    let mut position = 0;
    loop {
        file.seek(SeekFrom::Start(position))?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        print!("{contents}");
        std::io::stdout().flush()?;
        position = file.stream_position()?;
        if !follow {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
        if fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0)
            < position
        {
            file = fs::File::open(&path)?;
            position = 0;
        }
    }
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_utf8(value: &str, maximum_bytes: usize) -> &str {
    if value.len() <= maximum_bytes {
        return value;
    }
    let mut end = maximum_bytes.min(value.len());
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &value[..end]
}
