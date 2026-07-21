use std::{io::Write, time::Duration};

use anyhow::{Context, Result};

use crate::{
    runtime,
    snapshot::{HostSnapshot, ProcessTable},
    tmux::Tmux,
    watcher::FileSignal,
};

pub struct Collector {
    host: String,
    tmux: Tmux,
    pane_rows: Option<String>,
    processes: ProcessTable,
}

pub struct SnapshotWriter<W> {
    writer: W,
    previous: Option<HostSnapshot>,
}

impl<W: Write> SnapshotWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            previous: None,
        }
    }

    pub fn publish(&mut self, snapshot: &HostSnapshot) -> Result<bool> {
        if self
            .previous
            .as_ref()
            .is_some_and(|previous| same_snapshot_content(previous, snapshot))
        {
            return Ok(false);
        }
        serde_json::to_writer(&mut self.writer, snapshot)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        self.previous = Some(snapshot.clone());
        Ok(true)
    }
}

pub async fn run_stream(host: String) -> Result<()> {
    let directory = runtime::current_server_directory();
    runtime::ensure_private_directory(&directory)?;
    let mut signal = FileSignal::watch(&directory)?;
    let mut refresh_token = runtime::refresh_token();
    let mut collector = Collector::new(host, Tmux::new());
    let stdout = std::io::stdout();
    let mut writer = SnapshotWriter::new(std::io::BufWriter::new(stdout));
    writer.publish(&collector.full_scan()?)?;

    let mut safety = tokio::time::interval(Duration::from_secs(60));
    safety.tick().await;
    loop {
        let snapshot = tokio::select! {
            changed = signal.changed() => {
                let paths = changed?;
                let next_token = runtime::refresh_token();
                let full = next_token != refresh_token || paths.iter().any(|path| {
                    path.file_name().and_then(|name| name.to_str()) == Some("refresh")
                });
                refresh_token = next_token;
                if full { collector.full_scan()? } else { collector.state_refresh()? }
            }
            _ = safety.tick() => collector.full_scan()?,
            result = tokio::signal::ctrl_c() => {
                result.context("failed to listen for stream shutdown")?;
                return Ok(());
            }
        };
        writer.publish(&snapshot)?;
    }
}

fn same_snapshot_content(previous: &HostSnapshot, current: &HostSnapshot) -> bool {
    let mut previous = previous.clone();
    let mut current = current.clone();
    previous.collected_at_ms = 0;
    current.collected_at_ms = 0;
    previous == current
}

impl Collector {
    pub fn new(host: impl Into<String>, tmux: Tmux) -> Self {
        Self {
            host: host.into(),
            tmux,
            pane_rows: None,
            processes: ProcessTable::default(),
        }
    }

    pub fn full_scan(&mut self) -> Result<HostSnapshot> {
        let rows = self.tmux.pane_rows()?;
        let processes = self.tmux.process_table();
        let snapshot = self
            .tmux
            .snapshot_from_rows(&self.host, &rows, &processes, true)?;
        self.pane_rows = Some(rows);
        self.processes = processes;
        Ok(snapshot)
    }

    pub fn state_refresh(&mut self) -> Result<HostSnapshot> {
        let rows = self
            .pane_rows
            .as_deref()
            .context("collector requires an initial full scan")?;
        self.tmux
            .snapshot_from_rows(&self.host, rows, &self.processes, false)
    }
}
