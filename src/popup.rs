use std::{
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
    daemon::popup_option_name,
    tmux::{now_ms, Tmux},
};

const LEASE_DURATION_MS: u64 = 2_500;
const RENEW_INTERVAL: Duration = Duration::from_secs(1);

pub struct PopupLease {
    stop: Option<Sender<()>>,
    worker: Option<JoinHandle<()>>,
}

impl PopupLease {
    pub fn new(tmux: Tmux, client: &str) -> Self {
        let option = popup_option_name(client);
        let (stop_tx, stop_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            loop {
                let expiry = now_ms().saturating_add(LEASE_DURATION_MS).to_string();
                let _ = tmux.set_global_option(&option, &expiry);
                match stop_rx.recv_timeout(RENEW_INTERVAL) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    _ => break,
                }
            }
            let _ = tmux.unset_global_option(&option);
        });
        Self {
            stop: Some(stop_tx),
            worker: Some(worker),
        }
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
    }
}
