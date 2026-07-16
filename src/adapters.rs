use serde::{Deserialize, Serialize};

use crate::model::{AgentKind, EventKind, NormalizedEvent};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeEvent {
    pub agent: AgentKind,
    pub event: String,
    pub matcher: Option<String>,
    pub session_id: Option<String>,
    pub turn_id: Option<String>,
    pub reason: Option<String>,
}

pub fn normalize(native: NativeEvent) -> NormalizedEvent {
    let event = native.event.as_str();
    let matcher = native.matcher.as_deref().unwrap_or_default();
    let kind = match native.agent {
        AgentKind::Claude => match event {
            "SessionStart" => EventKind::Started,
            "PermissionRequest" | "Elicitation" => EventKind::NeedsInput,
            "Notification" if matches!(matcher, "permission_prompt" | "elicitation_dialog") => {
                EventKind::NeedsInput
            }
            "Notification" if matcher == "idle_prompt" => EventKind::Stopped,
            "Stop" | "StopFailure" => EventKind::Stopped,
            "SessionEnd" => EventKind::Ended,
            "ElicitationResult" => EventKind::InputResolved,
            _ => EventKind::Activity,
        },
        AgentKind::Codex => match event {
            "SessionStart" => EventKind::Started,
            "PermissionRequest" => EventKind::NeedsInput,
            "Stop" | "StopFailure" | "SubagentStop" => EventKind::Stopped,
            "SessionEnd" => EventKind::Ended,
            _ => EventKind::Activity,
        },
        AgentKind::Pi => match event {
            "session_start" => EventKind::Started,
            "agent_start" => EventKind::Activity,
            "agent_end" => EventKind::Stopped,
            "session_shutdown" => EventKind::Ended,
            "tool_execution_start" if is_question_tool(matcher) => EventKind::NeedsInput,
            "tool_execution_end" if is_question_tool(matcher) => EventKind::InputResolved,
            _ => EventKind::Activity,
        },
    };

    NormalizedEvent {
        agent: native.agent,
        kind,
        session_id: native.session_id,
        turn_id: native.turn_id,
        reason: native
            .reason
            .or_else(|| (!matcher.is_empty()).then(|| matcher.to_owned())),
    }
}

fn is_question_tool(name: &str) -> bool {
    matches!(name, "question" | "request_user_input" | "ask_user")
}
