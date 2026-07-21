use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::Path,
    str::FromStr,
};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::hook_state::PaneState;
use crate::model::{AgentKind, AgentState};

pub const SCHEMA_VERSION: u16 = 1;
const FIELD_SEPARATOR: char = '\u{1f}';
const TMUX_ESCAPED_FIELD_SEPARATOR: &str = r"\037";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentKey {
    pub host: String,
    pub session_id: String,
    pub window_id: String,
    pub pane_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub key: AgentKey,
    pub pane_index: u32,
    pub cwd: String,
    pub project: String,
    pub pane_pid: u32,
    pub command: String,
    pub agent: AgentKind,
    pub state: AgentState,
    pub state_since_ms: u64,
    pub native_session_id: Option<String>,
    pub turn_id: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSnapshot {
    pub id: String,
    pub index: u32,
    pub name: String,
    pub panes: Vec<PaneSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub id: String,
    pub name: String,
    pub windows: Vec<WindowSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSnapshot {
    pub schema_version: u16,
    pub host: String,
    pub collected_at_ms: u64,
    pub online: bool,
    pub error: Option<String>,
    pub sessions: Vec<SessionSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateSnapshot {
    pub schema_version: u16,
    pub generated_at_ms: u64,
    pub hosts: Vec<HostSnapshot>,
}

#[derive(Debug, Clone, Default)]
pub struct ProcessTable {
    processes: HashMap<u32, ProcessEntry>,
}

#[derive(Debug, Clone)]
struct ProcessEntry {
    parent: u32,
    command: String,
}

impl ProcessTable {
    pub fn parse(input: &str) -> Self {
        let processes = input
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                let first_end = line.find(char::is_whitespace)?;
                let pid = line[..first_end].parse().ok()?;
                let remainder = line[first_end..].trim_start();
                let second_end = remainder.find(char::is_whitespace)?;
                let parent = remainder[..second_end].parse().ok()?;
                let command = remainder[second_end..].trim_start().to_owned();
                Some((pid, ProcessEntry { parent, command }))
            })
            .collect();
        Self { processes }
    }

    pub fn agent_below(&self, root: u32) -> Option<AgentKind> {
        let mut queue = VecDeque::from([root]);
        let mut visited = HashSet::new();
        while let Some(parent) = queue.pop_front() {
            if !visited.insert(parent) {
                continue;
            }
            for (pid, process) in &self.processes {
                if *pid == parent || process.parent == parent {
                    if let Some(agent) = detect_agent(&process.command) {
                        return Some(agent);
                    }
                    if process.parent == parent {
                        queue.push_back(*pid);
                    }
                }
            }
        }
        None
    }

    fn codex_fallback_state(&self, root: u32) -> Option<AgentState> {
        let mut queue = VecDeque::from([root]);
        let mut visited = HashSet::new();
        let mut runners = HashSet::new();
        while let Some(pid) = queue.pop_front() {
            if !visited.insert(pid) {
                continue;
            }
            if self
                .processes
                .get(&pid)
                .is_some_and(|process| detect_agent(&process.command) == Some(AgentKind::Codex))
            {
                runners.insert(pid);
            }
            queue.extend(
                self.processes
                    .iter()
                    .filter(|(_, process)| process.parent == pid)
                    .map(|(child, _)| *child),
            );
        }
        if runners.is_empty() {
            return None;
        }
        let has_tool_child = self
            .processes
            .iter()
            .any(|(pid, process)| runners.contains(&process.parent) && !runners.contains(pid));
        Some(if has_tool_child {
            AgentState::Working
        } else {
            AgentState::Idle
        })
    }

    pub fn is_empty(&self) -> bool {
        self.processes.is_empty()
    }
}

impl HostSnapshot {
    pub fn empty(host: impl Into<String>, collected_at_ms: u64) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            host: host.into(),
            collected_at_ms,
            online: true,
            error: None,
            sessions: Vec::new(),
        }
    }

    pub fn agents(&self) -> impl Iterator<Item = &PaneSnapshot> {
        self.sessions
            .iter()
            .flat_map(|session| &session.windows)
            .flat_map(|window| &window.panes)
    }

    #[doc(hidden)]
    pub fn push_test_agent(&mut self, state: AgentState) {
        let pane = PaneSnapshot {
            key: AgentKey {
                host: self.host.clone(),
                session_id: "$test".into(),
                window_id: "@test".into(),
                pane_id: "%test".into(),
            },
            pane_index: 0,
            cwd: "/tmp".into(),
            project: "tmp".into(),
            pane_pid: 1,
            command: "claude".into(),
            agent: AgentKind::Claude,
            state,
            state_since_ms: self.collected_at_ms,
            native_session_id: None,
            turn_id: None,
            reason: None,
        };
        self.sessions.push(SessionSnapshot {
            id: "$test".into(),
            name: "test".into(),
            windows: vec![WindowSnapshot {
                id: "@test".into(),
                index: 0,
                name: "test".into(),
                panes: vec![pane],
            }],
        });
    }
}

pub fn parse_tmux_rows(host: &str, collected_at_ms: u64, input: &str) -> Result<HostSnapshot> {
    parse_tmux_rows_with_processes(host, collected_at_ms, input, &ProcessTable::default())
}

pub fn parse_tmux_rows_with_processes(
    host: &str,
    collected_at_ms: u64,
    input: &str,
    processes: &ProcessTable,
) -> Result<HostSnapshot> {
    parse_tmux_rows_with_processes_and_states(
        host,
        collected_at_ms,
        input,
        processes,
        &HashMap::new(),
    )
}

pub fn parse_tmux_rows_with_processes_and_states(
    host: &str,
    collected_at_ms: u64,
    input: &str,
    processes: &ProcessTable,
    states: &HashMap<String, PaneState>,
) -> Result<HostSnapshot> {
    parse_tmux_rows_with_process_data(host, collected_at_ms, input, processes, states, true)
}

pub fn parse_tmux_rows_with_cached_processes_and_states(
    host: &str,
    collected_at_ms: u64,
    input: &str,
    processes: &ProcessTable,
    states: &HashMap<String, PaneState>,
) -> Result<HostSnapshot> {
    parse_tmux_rows_with_process_data(host, collected_at_ms, input, processes, states, false)
}

fn parse_tmux_rows_with_process_data(
    host: &str,
    collected_at_ms: u64,
    input: &str,
    processes: &ProcessTable,
    states: &HashMap<String, PaneState>,
    processes_are_fresh: bool,
) -> Result<HostSnapshot> {
    let mut snapshot = HostSnapshot::empty(host, collected_at_ms);

    for (line_number, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = if line.contains(FIELD_SEPARATOR) {
            line.split(FIELD_SEPARATOR).collect()
        } else {
            line.split(TMUX_ESCAPED_FIELD_SEPARATOR).collect()
        };
        if !matches!(fields.len(), 16 | 17) {
            bail!(
                "tmux row {} has {} fields, expected 16 or 17",
                line_number + 1,
                fields.len()
            );
        }

        let file_state = states.get(fields[5]);
        let file_record = file_state.and_then(|state| state.record.as_ref());
        let configured_agent = match file_state {
            Some(_) => file_record.map(|record| record.agent),
            None => (!fields[10].is_empty())
                .then(|| AgentKind::from_str(fields[10]).ok())
                .flatten(),
        };
        let pane_pid = fields[8].parse::<u32>().ok();
        let live_agent = pane_pid.and_then(|pid| processes.agent_below(pid));
        let agent = if processes.is_empty() {
            configured_agent.or_else(|| detect_agent(fields[9]))
        } else if processes_are_fresh {
            live_agent
        } else {
            configured_agent.or(live_agent)
        };
        let Some(agent) = agent else { continue };
        let state = if let Some(record) = file_record.filter(|record| record.agent == agent) {
            record.state
        } else if file_state.is_none() && !fields[11].is_empty() && configured_agent == Some(agent)
        {
            AgentState::from_str(fields[11]).unwrap_or(AgentState::Untracked)
        } else if agent == AgentKind::Codex {
            pane_pid
                .and_then(|pid| processes.codex_fallback_state(pid))
                .unwrap_or(AgentState::Untracked)
        } else {
            AgentState::Untracked
        };

        let session_id = fields[0].to_owned();
        let window_id = fields[2].to_owned();
        let pane = PaneSnapshot {
            key: AgentKey {
                host: host.to_owned(),
                session_id: session_id.clone(),
                window_id: window_id.clone(),
                pane_id: fields[5].to_owned(),
            },
            pane_index: parse_number(fields[6], "pane index")?,
            cwd: fields[7].to_owned(),
            project: Path::new(fields[7])
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(fields[7])
                .to_owned(),
            pane_pid: parse_number(fields[8], "pane pid")?,
            command: fields[9].to_owned(),
            agent,
            state,
            state_since_ms: file_record
                .map(|record| record.state_since_ms)
                .unwrap_or_else(|| fields[12].parse().unwrap_or(collected_at_ms)),
            native_session_id: file_record
                .map(|record| record.session_id.clone())
                .or_else(|| file_state.is_none().then(|| nonempty(fields[13])).flatten()),
            turn_id: file_record
                .and_then(|record| record.active_turn.clone())
                .or_else(|| file_state.is_none().then(|| nonempty(fields[14])).flatten()),
            reason: file_record
                .and_then(|record| record.reason.clone())
                .or_else(|| file_state.is_none().then(|| nonempty(fields[15])).flatten()),
        };

        let session_index = match snapshot
            .sessions
            .iter()
            .position(|session| session.id == session_id)
        {
            Some(index) => index,
            None => {
                snapshot.sessions.push(SessionSnapshot {
                    id: session_id,
                    name: fields[1].to_owned(),
                    windows: Vec::new(),
                });
                snapshot.sessions.len() - 1
            }
        };
        let session = &mut snapshot.sessions[session_index];
        let window_index = match session
            .windows
            .iter()
            .position(|window| window.id == window_id)
        {
            Some(index) => index,
            None => {
                session.windows.push(WindowSnapshot {
                    id: window_id,
                    index: parse_number(fields[3], "window index")?,
                    name: fields[4].to_owned(),
                    panes: Vec::new(),
                });
                session.windows.len() - 1
            }
        };
        session.windows[window_index].panes.push(pane);
    }

    Ok(snapshot)
}

pub fn aggregate_online<'a>(hosts: impl IntoIterator<Item = &'a HostSnapshot>) -> AgentState {
    AgentState::aggregate(
        hosts
            .into_iter()
            .filter(|host| host.online)
            .flat_map(HostSnapshot::agents)
            .map(|agent| agent.state),
    )
}

pub fn status_widget(state: AgentState) -> String {
    status_widget_with(state, &StatusColors::default())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusColors {
    pub working: String,
    pub idle: String,
    pub needs_input: String,
    pub untracked: String,
}

impl Default for StatusColors {
    fn default() -> Self {
        Self {
            working: "#9ece6a".into(),
            idle: "#e0af68".into(),
            needs_input: "#7aa2f7".into(),
            untracked: "#565f89".into(),
        }
    }
}

pub fn status_widget_with(state: AgentState, colors: &StatusColors) -> String {
    let color = match state {
        AgentState::Working => &colors.working,
        AgentState::Idle => &colors.idle,
        AgentState::NeedsInput => &colors.needs_input,
        AgentState::Untracked => &colors.untracked,
    };
    format!("#[fg={color}]■#[default]")
}

fn detect_agent(command: &str) -> Option<AgentKind> {
    let executable = command.split_whitespace().next().unwrap_or(command);
    let basename = Path::new(executable)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(executable)
        .to_ascii_lowercase();
    let command = command.to_ascii_lowercase();
    if basename.contains("claude") || command.contains("claude-code") {
        Some(AgentKind::Claude)
    } else if basename.contains("codex")
        || command.contains("/codex/")
        || command.contains("@openai/codex")
    {
        Some(AgentKind::Codex)
    } else if basename == "pi" || basename.starts_with("pi-") {
        Some(AgentKind::Pi)
    } else {
        None
    }
}

fn parse_number<T>(value: &str, label: &str) -> Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    value
        .parse()
        .with_context(|| format!("invalid {label}: {value}"))
}

fn nonempty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}
