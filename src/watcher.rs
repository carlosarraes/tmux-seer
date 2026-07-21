use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::{sync::mpsc, time};

const COALESCE_WINDOW: Duration = Duration::from_millis(25);

pub struct FileSignal {
    _watcher: RecommendedWatcher,
    events: mpsc::Receiver<notify::Result<Event>>,
    pending: Vec<PathBuf>,
}

pub fn paths_include_atomic_target(paths: &[PathBuf], target: &Path) -> bool {
    let Some(parent) = target.parent() else {
        return paths.iter().any(|path| path == target);
    };
    let Some(stem) = target.file_stem().and_then(|stem| stem.to_str()) else {
        return paths.iter().any(|path| path == target);
    };
    let temporary_prefix = format!("{stem}.tmp-");
    paths.iter().any(|path| {
        path == target
            || path.parent() == Some(parent)
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&temporary_prefix))
    })
}

impl FileSignal {
    pub fn watch(path: &Path) -> Result<Self> {
        let (sender, events) = mpsc::channel(64);
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = sender.try_send(event);
        })
        .context("failed to create filesystem watcher")?;
        watcher
            .watch(path, RecursiveMode::Recursive)
            .with_context(|| format!("failed to watch {}", path.display()))?;
        Ok(Self {
            _watcher: watcher,
            events,
            pending: Vec::new(),
        })
    }

    pub async fn changed(&mut self) -> Result<Vec<PathBuf>> {
        if self.pending.is_empty() {
            loop {
                let event = self
                    .events
                    .recv()
                    .await
                    .context("filesystem watcher stopped")??;
                if is_change(&event) {
                    self.pending.extend(event.paths);
                    break;
                }
            }
        }

        time::sleep(COALESCE_WINDOW).await;
        let mut paths = std::mem::take(&mut self.pending);
        paths.extend(self.drain_events()?);
        deduplicate(&mut paths);
        Ok(paths)
    }

    pub fn try_changed(&mut self) -> Result<Vec<PathBuf>> {
        let mut paths = std::mem::take(&mut self.pending);
        paths.extend(self.drain_events()?);
        deduplicate(&mut paths);
        Ok(paths)
    }

    fn drain_events(&mut self) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        loop {
            match self.events.try_recv() {
                Ok(event) => {
                    let event = event.context("filesystem watcher failed")?;
                    if is_change(&event) {
                        paths.extend(event.paths);
                    }
                }
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    bail!("filesystem watcher stopped")
                }
            }
        }
        Ok(paths)
    }
}

fn deduplicate(paths: &mut Vec<PathBuf>) {
    let mut unique = HashSet::new();
    paths.retain(|path| unique.insert(path.clone()));
}

fn is_change(event: &Event) -> bool {
    !matches!(event.kind, EventKind::Access(_))
}
