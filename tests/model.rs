use tmux_seer::adapters::{normalize, NativeEvent};
use tmux_seer::model::{AgentKind, AgentState, EventKind};
use tmux_seer::reducer::{reduce, AgentRecord, CodexPaneTracker};

#[test]
fn structured_permission_becomes_needs_input() {
    let event = normalize(NativeEvent {
        agent: AgentKind::Claude,
        event: "PermissionRequest".into(),
        matcher: None,
        session_id: Some("session-1".into()),
        turn_id: None,
        reason: None,
    });

    assert_eq!(event.kind, EventKind::NeedsInput);
}

#[test]
fn prose_question_stop_remains_idle() {
    let event = normalize(NativeEvent {
        agent: AgentKind::Claude,
        event: "Stop".into(),
        matcher: None,
        session_id: Some("session-1".into()),
        turn_id: None,
        reason: Some("What should I do next?".into()),
    });

    assert_eq!(event.kind, EventKind::Stopped);
    assert_eq!(reduce(None, event, 100).state, AgentState::Idle);
}

#[test]
fn nested_stop_does_not_idle_active_parent_turn() {
    let parent = AgentRecord::new(AgentKind::Codex, "session-1", 10)
        .with_active_turn("parent")
        .with_state(AgentState::Working, 20);
    let child_stop = normalize(NativeEvent {
        agent: AgentKind::Codex,
        event: "Stop".into(),
        matcher: None,
        session_id: Some("session-1".into()),
        turn_id: Some("child".into()),
        reason: None,
    });

    assert_eq!(
        reduce(Some(parent), child_stop, 30).state,
        AgentState::Working
    );
}

fn codex_event(
    event: &str,
    session_id: &str,
    turn_id: Option<&str>,
) -> tmux_seer::model::NormalizedEvent {
    normalize(NativeEvent {
        agent: AgentKind::Codex,
        event: event.into(),
        matcher: None,
        session_id: Some(session_id.into()),
        turn_id: turn_id.map(str::to_owned),
        reason: None,
    })
}

#[test]
fn codex_child_stop_keeps_the_parent_working() {
    let mut tracker = CodexPaneTracker::default();

    tracker.apply(codex_event("UserPromptSubmit", "parent", Some("p1")), 10);
    tracker.apply(codex_event("PreToolUse", "child", Some("c1")), 20);
    let after_child = tracker.apply(codex_event("Stop", "child", Some("c1")), 30);

    assert_eq!(after_child.unwrap().state, AgentState::Working);
    let after_parent = tracker.apply(codex_event("Stop", "parent", Some("p1")), 40);
    assert_eq!(after_parent.unwrap().state, AgentState::Idle);
}

#[test]
fn codex_overlapping_turns_idle_only_after_both_stop() {
    let mut tracker = CodexPaneTracker::default();

    tracker.apply(codex_event("UserPromptSubmit", "session", Some("t1")), 10);
    tracker.apply(codex_event("UserPromptSubmit", "session", Some("t2")), 20);
    let first_stop = tracker.apply(codex_event("Stop", "session", Some("t1")), 30);

    assert_eq!(first_stop.unwrap().state, AgentState::Working);
    let second_stop = tracker.apply(codex_event("Stop", "session", Some("t2")), 40);
    assert_eq!(second_stop.unwrap().state, AgentState::Idle);
}

#[test]
fn codex_late_activity_cannot_resurrect_a_completed_turn() {
    let mut tracker = CodexPaneTracker::default();

    tracker.apply(codex_event("UserPromptSubmit", "session", Some("t1")), 10);
    let stopped = tracker.apply(codex_event("Stop", "session", Some("t1")), 20);
    let stale = tracker.apply(codex_event("PostToolUse", "session", Some("t1")), 30);

    assert_eq!(stale, stopped);
    assert_eq!(stale.unwrap().state_since_ms, 20);
}

#[test]
fn codex_input_has_priority_over_other_active_turns() {
    let mut tracker = CodexPaneTracker::default();

    tracker.apply(codex_event("UserPromptSubmit", "parent", Some("p1")), 10);
    let input = tracker.apply(codex_event("PermissionRequest", "child", Some("c1")), 20);

    assert_eq!(input.unwrap().state, AgentState::NeedsInput);
}

#[test]
fn aggregate_priority_is_input_idle_working_untracked() {
    assert_eq!(AgentState::aggregate([]), AgentState::Untracked);
    assert_eq!(
        AgentState::aggregate([AgentState::Working]),
        AgentState::Working
    );
    assert_eq!(
        AgentState::aggregate([AgentState::Working, AgentState::Idle]),
        AgentState::Idle
    );
    assert_eq!(
        AgentState::aggregate([AgentState::Idle, AgentState::NeedsInput]),
        AgentState::NeedsInput
    );
}
