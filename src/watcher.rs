use std::{path::Path, time::Duration};

use anyhow::{Context, Result};
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

    pub async fn changed(&mut self) -> Result<()> {
        loop {
            let event = self
                .events
                .recv()
                .await
                .context("filesystem watcher stopped")??;
            if is_change(&event) {
                break;
            }
        }

        time::sleep(COALESCE_WINDOW).await;
        while let Ok(event) = self.events.try_recv() {
            event.context("filesystem watcher failed")?;
        }
        Ok(())
    }
}

fn is_change(event: &Event) -> bool {
    !matches!(event.kind, EventKind::Access(_))
}
