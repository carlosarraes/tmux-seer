use std::{fs, time::Duration};

use tmux_seer::watcher::FileSignal;

#[tokio::test]
async fn atomic_write_bursts_coalesce_into_one_signal() {
    let directory = tempfile::tempdir().unwrap();
    let mut signal = FileSignal::watch(directory.path()).unwrap();

    for index in 0..3 {
        let temporary = directory.path().join(format!("state-{index}.tmp"));
        let destination = directory.path().join(format!("state-{index}.json"));
        fs::write(&temporary, b"{}").unwrap();
        fs::rename(temporary, destination).unwrap();
    }

    tokio::time::timeout(Duration::from_millis(250), signal.changed())
        .await
        .expect("filesystem signal timed out")
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(75), signal.changed())
            .await
            .is_err(),
        "burst leaked a second signal"
    );
}

#[tokio::test]
async fn changed_paths_can_be_drained_without_blocking() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("snapshot.json");
    let mut signal = FileSignal::watch(directory.path()).unwrap();

    assert!(signal.try_changed().unwrap().is_empty());
    fs::write(&destination, b"{}").unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(signal
        .try_changed()
        .unwrap()
        .iter()
        .any(|path| path == &destination));
    assert!(signal.try_changed().unwrap().is_empty());
}
