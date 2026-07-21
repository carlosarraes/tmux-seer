use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::Result;

use crate::{runtime, tmux::now_ms};

const LEASE_DURATION_MS: u64 = 2_500;
const RENEW_INTERVAL: Duration = Duration::from_secs(1);

pub struct PopupLease {
    path: PathBuf,
    stop: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl PopupLease {
    pub fn new(client: &str) -> Result<Self> {
        let path = popup_path(client);
        write_expiry(&path)?;
        let (stop_tx, stop_rx) = mpsc::channel();
        let worker_path = path.clone();
        let worker = thread::spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) = stop_rx.recv_timeout(RENEW_INTERVAL) {
                if write_expiry(&worker_path).is_err() {
                    break;
                }
            }
            let _ = fs::remove_file(worker_path);
        });
        Ok(Self {
            path,
            stop: Some(stop_tx),
            worker: Some(worker),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PopupLease {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let _ = fs::remove_file(&self.path);
    }
}

pub fn client_is_suppressed(client: &str, now_ms: u64) -> bool {
    let path = popup_path(client);
    let expiry = fs::read_to_string(&path)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default();
    if expiry > now_ms {
        true
    } else {
        let _ = fs::remove_file(path);
        false
    }
}

fn popup_path(client: &str) -> PathBuf {
    runtime::current_server_directory()
        .join("popups")
        .join(format!("{}.lease", runtime::server_id(client)))
}

fn write_expiry(path: &Path) -> Result<()> {
    let expiry = now_ms().saturating_add(LEASE_DURATION_MS).to_string();
    runtime::atomic_write(path, expiry.as_bytes())
}
