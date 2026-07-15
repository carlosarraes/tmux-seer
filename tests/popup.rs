use std::{
    fs,
    os::unix::fs::PermissionsExt,
    thread,
    time::{Duration, Instant},
};

use tempfile::TempDir;
use tmux_seer::{popup::PopupLease, tmux::Tmux};

#[test]
fn popup_lease_renews_without_blocking_the_ui_thread() {
    let fake = FakeTmux::new();
    let tmux = temp_env::with_vars(
        [("TMUX_SEER_TMUX", Some(fake.program.to_str().unwrap()))],
        Tmux::new,
    );

    let started = Instant::now();
    let lease = PopupLease::new(tmux, "/dev/pts/1");
    assert!(started.elapsed() < Duration::from_millis(100));

    wait_for_log(&fake.log, "set-option -g @seer_popup__dev_pts_1");
    drop(lease);
    wait_for_log(&fake.log, "set-option -g -u @seer_popup__dev_pts_1");

    let log = fs::read_to_string(fake.log).unwrap();
    assert_eq!(
        log.lines()
            .filter(|line| line.starts_with("set-option -g @seer_popup"))
            .count(),
        1
    );
}

fn wait_for_log(path: &std::path::Path, expected: &str) {
    for _ in 0..50 {
        if fs::read_to_string(path)
            .unwrap_or_default()
            .contains(expected)
        {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {expected}");
}

struct FakeTmux {
    _directory: TempDir,
    program: std::path::PathBuf,
    log: std::path::PathBuf,
}

impl FakeTmux {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("tmux");
        let log = directory.path().join("tmux.log");
        fs::write(
            &program,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{}'
if [ "$1" = "set-option" ] && [ "$3" != "-u" ]; then sleep 0.2; fi
"#,
                log.display()
            ),
        )
        .unwrap();
        let mut permissions = fs::metadata(&program).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&program, permissions).unwrap();
        Self {
            _directory: directory,
            program,
            log,
        }
    }
}
