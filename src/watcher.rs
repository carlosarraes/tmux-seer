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
        })
    }

    pub async fn changed(&mut self) -> Result<Vec<PathBuf>> {
        let mut paths = loop {
            let event = self
                .events
                .recv()
                .await
                .context("filesystem watcher stopped")??;
            if is_change(&event) {
                break event.paths;
            }
        };

        time::sleep(COALESCE_WINDOW).await;
        paths.extend(self.try_changed()?);
        deduplicate(&mut paths);
        Ok(paths)
    }

    pub fn try_changed(&mut self) -> Result<Vec<PathBuf>> {
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
        deduplicate(&mut paths);
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
