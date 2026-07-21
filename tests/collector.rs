use std::{fs, io::Cursor, os::unix::fs::PermissionsExt};

use tmux_seer::{
    collector::{Collector, SnapshotWriter},
    hook_state::HookStateStore,
    model::{AgentKind, EventKind, NormalizedEvent},
    tmux::Tmux,
};

#[test]
fn state_refresh_reuses_cached_tmux_rows_and_processes() {
    let fixture = Fixture::new();
    temp_env::with_vars(
        [
            (
                "TMUX_SEER_RUNTIME_DIR",
                Some(fixture.runtime.path().as_os_str()),
            ),
            ("TMUX_SEER_TMUX", Some(fixture.tmux.as_os_str())),
            ("TMUX_SEER_PS", Some(fixture.ps.as_os_str())),
            ("TMUX_SEER_TEST_LOG", Some(fixture.log.as_os_str())),
            ("TMUX_SEER_ROWS", Some(fixture.row.as_str().as_ref())),
            ("TMUX", Some(fixture.tmux_env.as_str().as_ref())),
        ],
        || {
            let mut collector = Collector::new("local", Tmux::new());
            collector.full_scan().unwrap();
            assert_eq!(fixture.invocations(), (1, 1));

            HookStateStore::for_socket(&fixture.socket)
                .apply(
                    "%4",
                    NormalizedEvent {
                        agent: AgentKind::Claude,
                        kind: EventKind::Activity,
                        session_id: Some("s1".into()),
                        turn_id: None,
                        reason: None,
                    },
                    20,
                )
                .unwrap();
            collector.state_refresh().unwrap();
            collector.state_refresh().unwrap();
            assert_eq!(fixture.invocations(), (1, 1));

            collector.full_scan().unwrap();
            assert_eq!(fixture.invocations(), (2, 2));
        },
    );
}

#[test]
fn snapshot_stream_emits_only_semantic_changes() {
    let mut output = Cursor::new(Vec::new());
    {
        let mut writer = SnapshotWriter::new(&mut output);
        let first = tmux_seer::snapshot::HostSnapshot::empty("mac", 10);
        assert!(writer.publish(&first).unwrap());

        let same_content = tmux_seer::snapshot::HostSnapshot::empty("mac", 20);
        assert!(!writer.publish(&same_content).unwrap());

        let mut changed = same_content;
        changed.online = false;
        changed.error = Some("offline".into());
        assert!(writer.publish(&changed).unwrap());
    }

    let lines = String::from_utf8(output.into_inner()).unwrap();
    assert_eq!(lines.lines().count(), 2);
    for line in lines.lines() {
        let snapshot: tmux_seer::snapshot::HostSnapshot = serde_json::from_str(line).unwrap();
        assert_eq!(snapshot.host, "mac");
    }
}

struct Fixture {
    _directory: tempfile::TempDir,
    runtime: tempfile::TempDir,
    tmux: std::path::PathBuf,
    ps: std::path::PathBuf,
    log: std::path::PathBuf,
    socket: String,
    tmux_env: String,
    row: String,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        let tmux = directory.path().join("tmux");
        let ps = directory.path().join("ps");
        let log = directory.path().join("commands.log");
        fs::write(
            &tmux,
            "#!/bin/sh\nprintf 'tmux\\n' >> \"$TMUX_SEER_TEST_LOG\"\nprintf '%s\\n' \"$TMUX_SEER_ROWS\"\n",
        )
        .unwrap();
        fs::write(
            &ps,
            "#!/bin/sh\nprintf 'ps\\n' >> \"$TMUX_SEER_TEST_LOG\"\nprintf '101 1 bash\\n202 101 claude\\n'\n",
        )
        .unwrap();
        for program in [&tmux, &ps] {
            fs::set_permissions(program, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let socket = "/tmp/tmux-collector/default".to_owned();
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
            &socket,
        ]
        .join("\u{1f}");
        Self {
            _directory: directory,
            runtime,
            tmux,
            ps,
            log,
            tmux_env: format!("{socket},123,0"),
            socket,
            row,
        }
    }

    fn invocations(&self) -> (usize, usize) {
        let log = fs::read_to_string(&self.log).unwrap_or_default();
        (
            log.lines().filter(|line| *line == "tmux").count(),
            log.lines().filter(|line| *line == "ps").count(),
        )
    }
}
