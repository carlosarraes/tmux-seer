use std::{fs, time::Duration};

use tmux_seer::watcher::{paths_include_atomic_target, FileSignal};

#[test]
fn atomic_target_accepts_final_and_temporary_paths_only() {
    let target = std::path::Path::new("/tmp/runtime/snapshot.json");

    assert!(paths_include_atomic_target(&[target.to_owned()], target));
    assert!(paths_include_atomic_target(
        &["/tmp/runtime/snapshot.tmp-42".into()],
        target
    ));
    assert!(!paths_include_atomic_target(
        &["/tmp/runtime/health.tmp-42".into()],
        target
    ));
}

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

    let destination = directory.path().join("snapshot.json");
    let temporary = directory.path().join("snapshot.tmp-123");
    assert!(signal.try_changed().unwrap().is_empty());
    fs::write(&temporary, b"{}").unwrap();
    fs::rename(temporary, &destination).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if paths_include_atomic_target(&signal.try_changed().unwrap(), &destination) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("filesystem signal timed out");
    assert!(signal.try_changed().unwrap().is_empty());
}
