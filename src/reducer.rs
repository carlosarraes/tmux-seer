use serde::{Deserialize, Serialize};

use crate::model::{AgentKind, AgentState, EventKind, NormalizedEvent};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRecord {
    pub agent: AgentKind,
    pub session_id: String,
    pub active_turn: Option<String>,
    pub state: AgentState,
    pub state_since_ms: u64,
    pub reason: Option<String>,
}

impl AgentRecord {
    pub fn new(agent: AgentKind, session_id: impl Into<String>, now_ms: u64) -> Self {
        Self {
            agent,
            session_id: session_id.into(),
            active_turn: None,
            state: AgentState::Idle,
            state_since_ms: now_ms,
            reason: None,
        }
    }

    pub fn with_active_turn(mut self, turn_id: impl Into<String>) -> Self {
        self.active_turn = Some(turn_id.into());
        self
    }

    pub fn with_state(mut self, state: AgentState, since_ms: u64) -> Self {
        self.state = state;
        self.state_since_ms = since_ms;
        self
    }
}

pub fn reduce(current: Option<AgentRecord>, event: NormalizedEvent, now_ms: u64) -> AgentRecord {
    let session_id = event.session_id.as_deref().unwrap_or("unknown");
    let mut record = current
        .filter(|record| record.agent == event.agent && record.session_id == session_id)
        .unwrap_or_else(|| AgentRecord::new(event.agent, session_id, now_ms));

    if event.kind == EventKind::Stopped
        && event.turn_id.is_some()
        && record.active_turn.is_some()
        && event.turn_id != record.active_turn
    {
        return record;
    }

    let next = match event.kind {
        EventKind::Started | EventKind::Stopped => AgentState::Idle,
        EventKind::Activity | EventKind::InputResolved => AgentState::Working,
        EventKind::NeedsInput => AgentState::NeedsInput,
        EventKind::Ended => AgentState::Untracked,
    };

    if next != record.state {
        record.state = next;
        record.state_since_ms = now_ms;
    }
    record.reason = event.reason;

    match event.kind {
        EventKind::Activity | EventKind::InputResolved => {
            if event.turn_id.is_some() {
                record.active_turn = event.turn_id;
            }
        }
        EventKind::Stopped | EventKind::Ended => record.active_turn = None,
        EventKind::Started | EventKind::NeedsInput => {}
    }

    record
}
