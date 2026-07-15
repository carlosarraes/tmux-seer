use tmux_seer::adapters::{normalize, NativeEvent};
use tmux_seer::model::{AgentKind, AgentState, EventKind};
use tmux_seer::reducer::{reduce, AgentRecord};

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
