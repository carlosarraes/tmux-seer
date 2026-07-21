use std::{
    fs, thread,
    time::{Duration, Instant},
};

use tmux_seer::{
    popup::{client_is_suppressed, PopupLease},
    tmux::now_ms,
};

#[test]
fn popup_lease_uses_runtime_files_and_drops_without_tmux() {
    let runtime = tempfile::tempdir().unwrap();
    temp_env::with_vars(
        [
            ("TMUX_SEER_RUNTIME_DIR", Some(runtime.path().as_os_str())),
            (
                "TMUX",
                Some(std::ffi::OsStr::new("/tmp/tmux-test/default,123,0")),
            ),
        ],
        || {
            let started = Instant::now();
            let lease = PopupLease::new("/dev/pts/1").unwrap();
            assert!(started.elapsed() < Duration::from_millis(50));
            wait_until(|| client_is_suppressed("/dev/pts/1", now_ms()));

            let started = Instant::now();
            drop(lease);
            assert!(started.elapsed() < Duration::from_millis(50));
            wait_until(|| !client_is_suppressed("/dev/pts/1", now_ms()));
        },
    );
}

#[test]
fn expired_popup_lease_does_not_suppress_notifications() {
    let runtime = tempfile::tempdir().unwrap();
    temp_env::with_vars(
        [
            ("TMUX_SEER_RUNTIME_DIR", Some(runtime.path().as_os_str())),
            (
                "TMUX",
                Some(std::ffi::OsStr::new("/tmp/tmux-test/default,123,0")),
            ),
        ],
        || {
            let lease = PopupLease::new("/dev/pts/2").unwrap();
            let path = lease.path().to_owned();
            fs::write(path, "1").unwrap();
            assert!(!client_is_suppressed("/dev/pts/2", 2));
        },
    );
}

fn wait_until(predicate: impl Fn() -> bool) {
    for _ in 0..50 {
        if predicate() {
            return;
        }
        thread::sleep(Duration::from_millis(5));
    }
    panic!("condition did not become true");
}
