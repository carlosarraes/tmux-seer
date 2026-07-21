use tmux_seer::daemon::{
    classify_local_paths, format_notification, notification_for_transition, HostTracker, LocalWake,
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
fn local_file_changes_select_the_cheapest_refresh() {
    assert_eq!(
        classify_local_paths(&["/tmp/server/snapshot.json".into()]),
        LocalWake::Ignore
    );
    assert_eq!(
        classify_local_paths(&["/tmp/server/panes/_4.json".into()]),
        LocalWake::State
    );
    assert_eq!(
        classify_local_paths(&["/tmp/server/refresh".into()]),
        LocalWake::Full
    );
}

#[test]
fn notification_copy_uses_seer_brand_and_location() {
    assert_eq!(
        format_notification("mac", "main", "storefront", "Claude", "needs input"),
        "Seer: mac › main › storefront › Claude needs input"
    );
}
