use std::{fs, os::unix::fs::PermissionsExt};
use tempfile::TempDir;
use tmux_seer::{navigation::Navigator, snapshot::AgentKey, tmux::Tmux};

#[test]
fn local_jump_targets_only_requested_client_and_pane() {
    let fake = FakeTmux::new();
    temp_env::with_vars(
        [
            ("TMUX_SEER_TMUX", Some(fake.program.to_str().unwrap())),
            ("TMUX_SEER_TEST_LOG", Some(fake.log.to_str().unwrap())),
        ],
        || {
            Navigator::new(Tmux::new())
                .jump(
                    &AgentKey {
                        host: "local".into(),
                        session_id: "$1".into(),
                        window_id: "@2".into(),
                        pane_id: "%3".into(),
                    },
                    Some("/dev/pts/1"),
                )
                .unwrap();
        },
    );
    let log = fs::read_to_string(fake.log).unwrap();
    assert!(log.contains("switch-client -c /dev/pts/1 -t $1"));
    assert!(log.contains("select-window -t $1:@2"));
    assert!(log.contains("select-pane -t %3"));
}

#[test]
fn remote_jump_creates_reusable_named_bridge_window() {
    let fake = FakeTmux::new();
    temp_env::with_vars(
        [
            ("TMUX_SEER_TMUX", Some(fake.program.to_str().unwrap())),
            ("TMUX_SEER_TEST_LOG", Some(fake.log.to_str().unwrap())),
        ],
        || {
            Navigator::new(Tmux::new())
                .jump(
                    &AgentKey {
                        host: "mac".into(),
                        session_id: "$1".into(),
                        window_id: "@2".into(),
                        pane_id: "%3".into(),
                    },
                    Some("/dev/pts/1"),
                )
                .unwrap();
        },
    );
    let log = fs::read_to_string(fake.log).unwrap();
    assert!(log.contains("seer:mac"));
    assert!(log.contains("ssh -tt mac"));
    assert!(log.contains("link-window"));
    assert!(log.contains("switch-client -c /dev/pts/1"));
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
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$TMUX_SEER_TEST_LOG"
if [ "$1" = "display-message" ]; then printf '%s\n' '$local'; fi
if [ "$1" = "has-session" ] || [ "$1" = "list-windows" ]; then exit 1; fi
"#,
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
