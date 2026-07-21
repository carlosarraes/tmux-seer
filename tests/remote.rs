use std::{fs, os::unix::fs::PermissionsExt};

use tmux_seer::{
    remote::{read_remote_once, remote_stream_args, RemoteEvent},
    snapshot::HostSnapshot,
};

#[test]
fn remote_stream_uses_keepalive_login_shell_and_known_binary() {
    let args = remote_stream_args("mac").unwrap();
    for expected in [
        "BatchMode=yes",
        "ConnectTimeout=2",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=2",
        "mac",
    ] {
        assert!(args.iter().any(|arg| arg == expected), "missing {expected}");
    }
    let command = args.last().unwrap();
    assert!(command.contains("$HOME/.local/bin/tmux-seer"));
    assert!(command.contains("stream --host mac"));
    assert!(remote_stream_args("bad host").is_err());
}

#[tokio::test]
async fn one_ssh_process_delivers_multiple_snapshots() {
    let directory = tempfile::tempdir().unwrap();
    let ssh = directory.path().join("ssh");
    let log = directory.path().join("ssh.log");
    let snapshots = [
        serde_json::to_string(&HostSnapshot::empty("mac", 10)).unwrap(),
        serde_json::to_string(&HostSnapshot::empty("mac", 20)).unwrap(),
    ];
    fs::write(
        &ssh,
        format!(
            "#!/bin/sh\nprintf 'one\\n' >> '{}'\nprintf '%s\\n' '{}' '{}'\n",
            log.display(),
            snapshots[0],
            snapshots[1]
        ),
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o755)).unwrap();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);

    read_remote_once("mac".into(), ssh, sender).await.unwrap();

    let mut snapshots = 0;
    while let Ok(event) = receiver.try_recv() {
        if matches!(event, RemoteEvent::Snapshot { .. }) {
            snapshots += 1;
        }
    }
    assert_eq!(snapshots, 2);
    assert_eq!(fs::read_to_string(log).unwrap().lines().count(), 1);
}
