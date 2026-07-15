use std::{fs, os::unix::fs::PermissionsExt};
use tempfile::TempDir;
use tmux_seer::{
    navigation::{NavigationTarget, Navigator},
    snapshot::AgentKey,
    tmux::Tmux,
};

#[test]
fn local_session_navigation_switches_the_requested_client() {
    let fake = FakeTmux::new();
    temp_env::with_vars(
        [
            ("TMUX_SEER_TMUX", Some(fake.program.to_str().unwrap())),
            ("TMUX_SEER_TEST_LOG", Some(fake.log.to_str().unwrap())),
        ],
        || {
            Navigator::new(Tmux::new())
                .navigate(
                    &NavigationTarget::Session {
                        host: "local".into(),
                        session_id: "$1".into(),
                    },
                    Some("/dev/pts/1"),
                )
                .unwrap();
        },
    );

    assert_eq!(
        fs::read_to_string(fake.log).unwrap(),
        "switch-client -c /dev/pts/1 -t $1\n"
    );
}

#[test]
fn local_window_navigation_selects_the_requested_window() {
    let fake = FakeTmux::new();
    temp_env::with_vars(
        [
            ("TMUX_SEER_TMUX", Some(fake.program.to_str().unwrap())),
            ("TMUX_SEER_TEST_LOG", Some(fake.log.to_str().unwrap())),
        ],
        || {
            Navigator::new(Tmux::new())
                .navigate(
                    &NavigationTarget::Window {
                        host: "local".into(),
                        session_id: "$1".into(),
                        window_id: "@2".into(),
                    },
                    Some("/dev/pts/1"),
                )
                .unwrap();
        },
    );

    assert_eq!(
        fs::read_to_string(fake.log).unwrap(),
        "switch-client -c /dev/pts/1 -t $1:@2\n"
    );
}

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
    assert_eq!(
        fs::read_to_string(fake.log).unwrap(),
        "switch-client -c /dev/pts/1 -t %3\n"
    );
}

#[test]
fn remote_agent_navigation_uses_current_popup_without_bridge_windows() {
    let tmux = FakeTmux::new();
    let ssh = FakeSsh::new();
    temp_env::with_vars(
        [
            ("TMUX_SEER_TMUX", Some(tmux.program.to_str().unwrap())),
            ("TMUX_SEER_TEST_LOG", Some(tmux.log.to_str().unwrap())),
            ("TMUX_SEER_SSH", Some(ssh.program.to_str().unwrap())),
            ("TMUX_SEER_SSH_LOG", Some(ssh.log.to_str().unwrap())),
        ],
        || {
            Navigator::new(Tmux::new())
                .navigate(
                    &NavigationTarget::Agent(AgentKey {
                        host: "mac".into(),
                        session_id: "$1".into(),
                        window_id: "@2".into(),
                        pane_id: "%3".into(),
                    }),
                    Some("/dev/pts/1"),
                )
                .unwrap();
        },
    );
    let ssh_log = fs::read_to_string(ssh.log).unwrap_or_default();
    assert!(ssh_log.contains("-tt mac"));
    assert!(ssh_log.contains("exec \"$SHELL\" -lc"));
    assert!(ssh_log.contains("tmux select-window -t"));
    assert!(ssh_log.contains("$1:@2"));
    assert!(ssh_log.contains("&& tmux select-pane -t"));
    assert!(ssh_log.contains("%3"));
    assert!(ssh_log.contains("&& exec tmux attach-session -t"));
    assert!(!ssh_log.contains(r"\;"));
    let tmux_log = fs::read_to_string(tmux.log).unwrap_or_default();
    assert!(!tmux_log.contains("new-window"));
    assert!(!tmux_log.contains("link-window"));
    assert!(!tmux_log.contains("switch-client"));
}

#[test]
fn remote_popup_suppresses_notifications_for_its_client() {
    let tmux = FakeTmux::new();
    let ssh = FakeSsh::new();
    temp_env::with_vars(
        [
            ("TMUX_SEER_TMUX", Some(tmux.program.to_str().unwrap())),
            ("TMUX_SEER_TEST_LOG", Some(tmux.log.to_str().unwrap())),
            ("TMUX_SEER_SSH", Some(ssh.program.to_str().unwrap())),
            ("TMUX_SEER_SSH_LOG", Some(ssh.log.to_str().unwrap())),
        ],
        || {
            Navigator::new(Tmux::new())
                .navigate(
                    &NavigationTarget::Host { host: "mac".into() },
                    Some("/dev/pts/1"),
                )
                .unwrap();
        },
    );

    let log = fs::read_to_string(tmux.log).unwrap_or_default();
    assert!(log.contains("set-option -g @seer_popup__dev_pts_1"));
    assert!(log.contains("set-option -g -u @seer_popup__dev_pts_1"));
}

#[test]
fn remote_hierarchy_navigation_attaches_to_the_selected_scope() {
    let host = remote_ssh_log(NavigationTarget::Host { host: "mac".into() });
    assert!(host.contains("exec tmux attach-session"));
    assert!(!host.contains("attach-session -t"));

    let session = remote_ssh_log(NavigationTarget::Session {
        host: "mac".into(),
        session_id: "$1".into(),
    });
    assert!(session.contains("exec tmux attach-session -t"));
    assert!(session.contains("$1"));

    let window = remote_ssh_log(NavigationTarget::Window {
        host: "mac".into(),
        session_id: "$1".into(),
        window_id: "@2".into(),
    });
    assert!(window.contains("tmux select-window -t"));
    assert!(window.contains("$1:@2"));
    assert!(window.contains("exec tmux attach-session -t"));
}

#[test]
fn session_rename_targets_stable_ids_locally_and_remotely() {
    let tmux = FakeTmux::new();
    temp_env::with_vars(
        [
            ("TMUX_SEER_TMUX", Some(tmux.program.to_str().unwrap())),
            ("TMUX_SEER_TEST_LOG", Some(tmux.log.to_str().unwrap())),
        ],
        || {
            Navigator::new(Tmux::new())
                .rename_session("local", "$1", "deep work")
                .unwrap();
        },
    );
    assert_eq!(
        fs::read_to_string(tmux.log).unwrap(),
        "rename-session -t $1 deep work\n"
    );

    let tmux = FakeTmux::new();
    let ssh = FakeSsh::new();
    temp_env::with_vars(
        [
            ("TMUX_SEER_TMUX", Some(tmux.program.to_str().unwrap())),
            ("TMUX_SEER_TEST_LOG", Some(tmux.log.to_str().unwrap())),
            ("TMUX_SEER_SSH", Some(ssh.program.to_str().unwrap())),
            ("TMUX_SEER_SSH_LOG", Some(ssh.log.to_str().unwrap())),
        ],
        || {
            Navigator::new(Tmux::new())
                .rename_session("mac", "$2", "review's ready")
                .unwrap();
        },
    );
    let ssh_log = fs::read_to_string(ssh.log).unwrap();
    assert!(ssh_log.contains("mac exec \"$SHELL\" -lc"));
    assert!(ssh_log.contains("tmux rename-session -t"));
    assert!(ssh_log.contains("review"));
    assert!(fs::read_to_string(tmux.log).unwrap_or_default().is_empty());
}

#[test]
fn remote_session_rename_reports_ssh_errors() {
    let tmux = FakeTmux::new();
    let ssh = FakeSsh::new();
    let error = temp_env::with_vars(
        [
            ("TMUX_SEER_TMUX", Some(tmux.program.to_str().unwrap())),
            ("TMUX_SEER_TEST_LOG", Some(tmux.log.to_str().unwrap())),
            ("TMUX_SEER_SSH", Some(ssh.program.to_str().unwrap())),
            ("TMUX_SEER_SSH_LOG", Some(ssh.log.to_str().unwrap())),
            ("TMUX_SEER_SSH_ERROR", Some("session not found")),
        ],
        || {
            Navigator::new(Tmux::new())
                .rename_session("mac", "$2", "review")
                .unwrap_err()
                .to_string()
        },
    );

    assert!(error.contains("session not found"));
}

fn remote_ssh_log(target: NavigationTarget) -> String {
    let tmux = FakeTmux::new();
    let ssh = FakeSsh::new();
    temp_env::with_vars(
        [
            ("TMUX_SEER_TMUX", Some(tmux.program.to_str().unwrap())),
            ("TMUX_SEER_TEST_LOG", Some(tmux.log.to_str().unwrap())),
            ("TMUX_SEER_SSH", Some(ssh.program.to_str().unwrap())),
            ("TMUX_SEER_SSH_LOG", Some(ssh.log.to_str().unwrap())),
        ],
        || Navigator::new(Tmux::new()).navigate(&target, None).unwrap(),
    );
    fs::read_to_string(ssh.log).unwrap_or_default()
}

struct FakeSsh {
    _directory: TempDir,
    program: std::path::PathBuf,
    log: std::path::PathBuf,
}

impl FakeSsh {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let program = directory.path().join("ssh");
        let log = directory.path().join("ssh.log");
        fs::write(
            &program,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TMUX_SEER_SSH_LOG\"\nif [ -n \"$TMUX_SEER_SSH_ERROR\" ]; then printf '%s\\n' \"$TMUX_SEER_SSH_ERROR\" >&2; exit 1; fi\n",
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
