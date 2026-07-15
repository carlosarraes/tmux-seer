use assert_cmd::Command;
use predicates::prelude::*;
use std::{fs, os::unix::fs::PermissionsExt};
use tempfile::TempDir;

#[test]
fn help_exposes_operator_commands() {
    Command::cargo_bin("tmux-seer")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("snapshot"))
        .stdout(predicate::str::contains("setup"))
        .stdout(predicate::str::contains("doctor"));
}

#[test]
fn hook_outside_tmux_is_a_successful_noop() {
    Command::cargo_bin("tmux-seer")
        .unwrap()
        .args(["hook", "claude", "SessionStart"])
        .env_remove("TMUX_PANE")
        .write_stdin(r#"{"session_id":"s1"}"#)
        .assert()
        .success();
}

#[test]
fn hook_updates_only_seer_pane_options() {
    let fake = FakeTmux::new();
    Command::cargo_bin("tmux-seer")
        .unwrap()
        .args(["hook", "claude", "SessionStart"])
        .env("TMUX_PANE", "%9")
        .env("TMUX_SEER_TMUX", &fake.program)
        .env("TMUX_SEER_TEST_LOG", &fake.log)
        .write_stdin(r#"{"session_id":"s1"}"#)
        .assert()
        .success();

    let log = fs::read_to_string(&fake.log).unwrap();
    assert!(log.contains("set-option -p -t %9 @seer_agent_kind claude"));
    assert!(log.contains("set-option -p -t %9 @seer_state idle"));
    assert!(log.contains("@seer_record"));
}

#[test]
fn snapshot_command_prints_versioned_json() {
    let fake = FakeTmux::new();
    let row = [
        "$1",
        "main",
        "@2",
        "1",
        "app",
        "%4",
        "1",
        "/tmp/shop",
        "101",
        "claude",
        "claude",
        "idle",
        "50",
        "s1",
        "",
        "",
    ]
    .join("\u{1f}");

    Command::cargo_bin("tmux-seer")
        .unwrap()
        .args(["snapshot", "--host", "local"])
        .env("TMUX_SEER_TMUX", &fake.program)
        .env("TMUX_SEER_PS", &fake.program)
        .env("TMUX_SEER_TEST_LOG", &fake.log)
        .env("TMUX_SEER_ROWS", row)
        .assert()
        .success()
        .stdout(predicate::str::contains(r#""schema_version": 1"#))
        .stdout(predicate::str::contains(r#""agent": "claude""#));
}

#[test]
fn noninteractive_setup_writes_selected_integration_to_explicit_home() {
    let home = tempfile::tempdir().unwrap();
    Command::cargo_bin("tmux-seer")
        .unwrap()
        .args(["setup", "--non-interactive", "--agents", "codex"])
        .env("TMUX_SEER_HOME", home.path())
        .assert()
        .success();

    let hooks = fs::read_to_string(home.path().join(".codex/hooks.json")).unwrap();
    assert!(hooks.contains("tmux-seer hook codex PermissionRequest"));
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
case "$1" in
  show-options) exit 1 ;;
  list-panes) printf '%s\n' "$TMUX_SEER_ROWS" ;;
esac
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
