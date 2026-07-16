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

const MAX_COMPLETED_CODEX_TURNS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CodexTurnKey {
    session_id: String,
    turn_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CodexTurn {
    key: CodexTurnKey,
    record: AgentRecord,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexPaneTracker {
    active: Vec<CodexTurn>,
    completed: Vec<CodexTurnKey>,
    last_idle: Option<AgentRecord>,
}

impl CodexPaneTracker {
    pub fn apply(&mut self, event: NormalizedEvent, now_ms: u64) -> Option<AgentRecord> {
        debug_assert_eq!(event.agent, AgentKind::Codex);
        let key = CodexTurnKey {
            session_id: event.session_id.as_deref().unwrap_or("unknown").to_owned(),
            turn_id: event.turn_id.clone(),
        };

        match event.kind {
            EventKind::Started => {
                self.active.clear();
                self.completed.clear();
                self.last_idle = Some(reduce(None, event, now_ms));
            }
            EventKind::Ended => {
                self.active.clear();
                self.completed.clear();
                self.last_idle = None;
            }
            EventKind::Stopped => {
                if self.completed.contains(&key) {
                    return self.aggregate();
                }
                if key.turn_id.is_some() {
                    self.active.retain(|turn| turn.key != key);
                } else {
                    self.active
                        .retain(|turn| turn.key.session_id != key.session_id);
                }
                self.remember_completed(key);
                self.last_idle = Some(reduce(None, event, now_ms));
            }
            EventKind::Activity | EventKind::NeedsInput | EventKind::InputResolved => {
                if self.completed.contains(&key) {
                    return self.aggregate();
                }
                if let Some(turn) = self.active.iter_mut().find(|turn| turn.key == key) {
                    turn.record = reduce(Some(turn.record.clone()), event, now_ms);
                } else {
                    self.active.push(CodexTurn {
                        key,
                        record: reduce(None, event, now_ms),
                    });
                }
            }
        }

        self.aggregate()
    }

    fn aggregate(&self) -> Option<AgentRecord> {
        for state in [AgentState::NeedsInput, AgentState::Working] {
            if let Some(turn) = self
                .active
                .iter()
                .filter(|turn| turn.record.state == state)
                .min_by_key(|turn| turn.record.state_since_ms)
            {
                let mut record = turn.record.clone();
                record.active_turn = turn.key.turn_id.clone();
                return Some(record);
            }
        }
        self.last_idle.clone()
    }

    fn remember_completed(&mut self, key: CodexTurnKey) {
        self.completed.push(key);
        let overflow = self
            .completed
            .len()
            .saturating_sub(MAX_COMPLETED_CODEX_TURNS);
        if overflow > 0 {
            self.completed.drain(..overflow);
        }
    }
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
