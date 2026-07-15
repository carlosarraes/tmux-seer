use std::{fs, os::unix::fs::PermissionsExt};
use tempfile::TempDir;
use tmux_seer::{
    bootstrap::{bootstrap, with_status_widget},
    tmux::Tmux,
};

#[test]
fn status_widget_append_is_idempotent_and_preserves_theme() {
    let once = with_status_widget("#[fg=red]theme");
    assert_eq!(once, "#[fg=red]theme #{@seer_widget}");
    assert_eq!(with_status_widget(&once), once);
}

#[test]
fn bootstrap_sets_binding_and_status_without_touching_interval() {
    let fake = FakeTmux::new();
    temp_env::with_vars(
        [
            ("TMUX_SEER_TMUX", Some(fake.program.to_str().unwrap())),
            ("TMUX_SEER_TEST_LOG", Some(fake.log.to_str().unwrap())),
        ],
        || bootstrap(Tmux::new(), "/tmp/tmux-seer").unwrap(),
    );

    let log = fs::read_to_string(fake.log).unwrap();
    assert!(log.contains("set-option -g status-right #[fg=red]theme #{@seer_widget}"));
    assert!(log.contains("bind-key S display-popup"));
    assert!(log.contains("display-popup -EE"));
    assert!(log.contains("-x R -y 0"));
    assert!(log.contains("run-shell -b"));
    assert!(!log.contains("status-interval"));
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
if [ "$1" = "show-options" ] && [ "$4" = "status-right" ]; then
  printf '%s\n' '#[fg=red]theme'
  exit 0
fi
if [ "$1" = "show-options" ]; then exit 1; fi
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
