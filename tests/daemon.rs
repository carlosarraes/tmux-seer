use tmux_seer::daemon::{
    format_notification, notification_for_transition, remote_snapshot_args, HostTracker,
};
use tmux_seer::model::AgentState;
use tmux_seer::snapshot::HostSnapshot;

#[test]
fn notifies_only_actionable_transitions() {
    assert_eq!(notification_for_transition(None, AgentState::Idle), None);
    assert_eq!(
        notification_for_transition(Some(AgentState::Working), AgentState::Idle),
        Some("finished; now idle")
    );
    assert_eq!(
        notification_for_transition(Some(AgentState::Working), AgentState::NeedsInput),
        Some("needs input")
    );
    assert_eq!(
        notification_for_transition(Some(AgentState::NeedsInput), AgentState::NeedsInput),
        None
    );
    assert_eq!(
        notification_for_transition(Some(AgentState::Idle), AgentState::Working),
        None
    );
}

#[test]
fn remote_goes_offline_after_two_failures_and_retains_tree() {
    let mut tracker = HostTracker::new("mac");
    let mut online = HostSnapshot::empty("mac", 10);
    online.push_test_agent(AgentState::Idle);
    tracker.success(online);

    tracker.failure("timeout", 20);
    assert!(tracker.snapshot().unwrap().online);

    tracker.failure("timeout", 30);
    let cached = tracker.snapshot().unwrap();
    assert!(!cached.online);
    assert_eq!(cached.sessions.len(), 1);
    assert_eq!(cached.error.as_deref(), Some("timeout"));
}

#[test]
fn remote_snapshot_uses_batch_mode_login_shell_and_known_binary_path() {
    let args = remote_snapshot_args("mac").unwrap();
    assert!(args.iter().any(|arg| arg == "BatchMode=yes"));
    assert!(args.iter().any(|arg| arg == "mac"));
    assert!(args.last().unwrap().contains("$HOME/.local/bin/tmux-seer"));
    assert!(remote_snapshot_args("bad host").is_err());
}

#[test]
fn notification_copy_uses_seer_brand_and_location() {
    assert_eq!(
        format_notification("mac", "main", "storefront", "Claude", "needs input"),
        "Seer: mac › main › storefront › Claude needs input"
    );
}
