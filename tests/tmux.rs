use std::{fs, os::unix::fs::PermissionsExt};

use tmux_seer::{
    hook_state::HookStateStore,
    model::{AgentKind, EventKind, NormalizedEvent},
    snapshot::ProcessTable,
    tmux::Tmux,
};

#[test]
fn cached_process_scan_does_not_delete_new_hook_state() {
    let directory = tempfile::tempdir().unwrap();
    let fake_tmux = directory.path().join("tmux");
    fs::write(
        &fake_tmux,
        "#!/bin/sh\nprintf '%s\\n' \"$TMUX_SEER_ROWS\"\n",
    )
    .unwrap();
    fs::set_permissions(&fake_tmux, fs::Permissions::from_mode(0o755)).unwrap();
    let socket = "/tmp/tmux-test/default";
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
        "bash",
        "",
        "",
        "",
        "",
        "",
        "",
        socket,
    ]
    .join("\u{1f}");

    temp_env::with_vars(
        [
            ("TMUX_SEER_RUNTIME_DIR", Some(directory.path().as_os_str())),
            ("TMUX_SEER_TMUX", Some(fake_tmux.as_os_str())),
            ("TMUX_SEER_ROWS", Some(row.as_ref())),
            (
                "TMUX",
                Some(std::ffi::OsStr::new("/tmp/tmux-test/default,123,0")),
            ),
        ],
        || {
            let store = HookStateStore::for_socket(socket);
            store.apply("%4", started(), 10).unwrap();
            let stale_processes = ProcessTable::parse("999 1 bash");

            Tmux::new()
                .snapshot_with_cached_processes("local", &stale_processes)
                .unwrap();
            assert!(store.load("%4").unwrap().is_some());

            Tmux::new()
                .snapshot_with_processes("local", &stale_processes)
                .unwrap();
            assert!(store.load("%4").unwrap().is_none());
        },
    );
}

fn started() -> NormalizedEvent {
    NormalizedEvent {
        agent: AgentKind::Claude,
        kind: EventKind::Started,
        session_id: Some("s1".to_owned()),
        turn_id: None,
        reason: None,
    }
}
