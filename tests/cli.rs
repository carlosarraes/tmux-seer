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
        .stdout(predicate::str::contains("doctor"))
        .stdout(predicate::str::contains("logs"));
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
fn hook_never_invokes_tmux_and_writes_runtime_state() {
    let fake = FakeTmux::new();
    let runtime = tempfile::tempdir().unwrap();
    Command::cargo_bin("tmux-seer")
        .unwrap()
        .args(["hook", "claude", "SessionStart"])
        .env("TMUX_PANE", "%9")
        .env("TMUX", "/tmp/tmux-test/default,123,0")
        .env("TMUX_SEER_RUNTIME_DIR", runtime.path())
        .env("TMUX_SEER_TMUX", &fake.program)
        .env("TMUX_SEER_TEST_LOG", &fake.log)
        .write_stdin(r#"{"session_id":"s1"}"#)
        .assert()
        .success();

    assert_eq!(fs::read_to_string(&fake.log).unwrap_or_default(), "");
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(find_pane_state(runtime.path())).unwrap()).unwrap();
    assert_eq!(state["record"]["agent"], "claude");
    assert_eq!(state["record"]["state"], "idle");
    assert_eq!(state["record"]["session_id"], "s1");
}

#[test]
fn repeated_hook_activity_never_enters_tmux_command_queue() {
    let fake = FakeTmux::new();
    let runtime = tempfile::tempdir().unwrap();
    let run_hook = || {
        Command::cargo_bin("tmux-seer")
            .unwrap()
            .args(["hook", "codex", "PreToolUse"])
            .env("TMUX_PANE", "%9")
            .env("TMUX", "/tmp/tmux-test/default,123,0")
            .env("TMUX_SEER_RUNTIME_DIR", runtime.path())
            .env("TMUX_SEER_TMUX", &fake.program)
            .env("TMUX_SEER_TEST_LOG", &fake.log)
            .write_stdin(r#"{"session_id":"s1","turn_id":"t1"}"#)
            .assert()
            .success();
    };

    run_hook();
    run_hook();

    assert_eq!(fs::read_to_string(&fake.log).unwrap_or_default(), "");
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(find_pane_state(runtime.path())).unwrap()).unwrap();
    assert_eq!(state["record"]["state"], "working");
    assert_eq!(
        state["codex_tracker"]["active"].as_array().unwrap().len(),
        1
    );
}

#[test]
fn codex_child_stop_does_not_idle_the_parent_pane() {
    let fake = FakeTmux::new();
    let runtime = tempfile::tempdir().unwrap();
    let run_hook = |event: &str, payload: &str| {
        Command::cargo_bin("tmux-seer")
            .unwrap()
            .args(["hook", "codex", event])
            .env("TMUX_PANE", "%9")
            .env("TMUX", "/tmp/tmux-test/default,123,0")
            .env("TMUX_SEER_RUNTIME_DIR", runtime.path())
            .env("TMUX_SEER_TMUX", &fake.program)
            .env("TMUX_SEER_TEST_LOG", &fake.log)
            .write_stdin(payload)
            .assert()
            .success();
    };

    run_hook(
        "UserPromptSubmit",
        r#"{"session_id":"parent","turn_id":"p1"}"#,
    );
    run_hook("PreToolUse", r#"{"session_id":"child","turn_id":"c1"}"#);
    run_hook("Stop", r#"{"session_id":"child","turn_id":"c1"}"#);

    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(find_pane_state(runtime.path())).unwrap()).unwrap();
    assert_eq!(state["record"]["state"], "working");
    assert_eq!(state["record"]["session_id"], "parent");
    assert_eq!(fs::read_to_string(&fake.log).unwrap_or_default(), "");
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

fn find_pane_state(runtime: &std::path::Path) -> std::path::PathBuf {
    let servers = fs::read_dir(runtime.join("servers")).unwrap();
    for server in servers {
        let panes = server.unwrap().path().join("panes");
        if let Ok(entries) = fs::read_dir(panes) {
            if let Some(entry) = entries.flatten().next() {
                return entry.path();
            }
        }
    }
    panic!("no pane state written under {}", runtime.display());
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
  show-options)
    case "${6:-}" in
      @seer_record)
        test -f "${TMUX_SEER_TEST_RECORD:-}" || exit 1
        cat "$TMUX_SEER_TEST_RECORD"
        exit 0
        ;;
      @seer_codex_tracker)
        test -f "${TMUX_SEER_TEST_TRACKER:-}" || exit 1
        cat "$TMUX_SEER_TEST_TRACKER"
        exit 0
        ;;
    esac
    exit 1
    ;;
  set-option)
    case "${5:-}" in
      @seer_record)
        test -n "${TMUX_SEER_TEST_RECORD:-}" && printf '%s' "${6:-}" > "$TMUX_SEER_TEST_RECORD"
        ;;
      @seer_codex_tracker)
        test -n "${TMUX_SEER_TEST_TRACKER:-}" && printf '%s' "${6:-}" > "$TMUX_SEER_TEST_TRACKER"
        ;;
    esac
    ;;
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
