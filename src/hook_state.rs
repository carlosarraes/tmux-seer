use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    path::PathBuf,
};

use anyhow::{Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::{
    model::{AgentKind, EventKind, NormalizedEvent},
    reducer::{reduce, AgentRecord, CodexPaneTracker},
    runtime,
};

const HOOK_STATE_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneState {
    pub schema_version: u16,
    pub updated_at_ms: u64,
    pub record: Option<AgentRecord>,
    pub codex_tracker: Option<CodexPaneTracker>,
}

#[derive(Debug, Clone)]
pub struct HookStateStore {
    server_id: String,
    server_directory: PathBuf,
}

impl HookStateStore {
    pub fn from_env() -> Self {
        Self::for_socket(&runtime::current_socket())
    }

    pub fn for_socket(socket: &str) -> Self {
        Self {
            server_id: runtime::server_id(socket),
            server_directory: runtime::server_directory_for_socket(socket),
        }
    }

    pub fn for_server(server_id: impl Into<String>) -> Self {
        let server_id = server_id.into();
        Self {
            server_directory: runtime::server_directory(&server_id),
            server_id,
        }
    }

    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    pub fn apply(&self, pane: &str, event: NormalizedEvent, now_ms: u64) -> Result<()> {
        let _lock = self.lock(pane)?;
        let current = self.load(pane)?;
        let (record, codex_tracker) = if event.agent == AgentKind::Codex {
            let mut tracker = current
                .as_ref()
                .and_then(|state| state.codex_tracker.clone())
                .unwrap_or_default();
            let record = tracker.apply(event, now_ms);
            (record, Some(tracker))
        } else {
            let record = if event.kind == EventKind::Ended {
                None
            } else {
                Some(reduce(
                    current.as_ref().and_then(|state| state.record.clone()),
                    event,
                    now_ms,
                ))
            };
            (record, None)
        };

        if current
            .as_ref()
            .is_some_and(|state| state.record == record && state.codex_tracker == codex_tracker)
        {
            return Ok(());
        }

        let state = PaneState {
            schema_version: HOOK_STATE_SCHEMA_VERSION,
            updated_at_ms: now_ms,
            record,
            codex_tracker,
        };
        runtime::atomic_write(&self.pane_path(pane), &serde_json::to_vec(&state)?)
    }

    pub fn load(&self, pane: &str) -> Result<Option<PaneState>> {
        let path = self.pane_path(pane);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read pane state {}", path.display()))
            }
        };
        let state: PaneState = serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid pane state {}", path.display()))?;
        if state.schema_version != HOOK_STATE_SCHEMA_VERSION {
            return Ok(None);
        }
        Ok(Some(state))
    }

    pub fn pane_path(&self, pane: &str) -> PathBuf {
        self.server_directory
            .join("panes")
            .join(format!("{}.json", safe_component(pane)))
    }

    pub fn reconcile(&self, active_agent_panes: &HashSet<String>) -> Result<usize> {
        let directory = self.server_directory.join("panes");
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to list pane states {}", directory.display()))
            }
        };
        let active = active_agent_panes
            .iter()
            .map(|pane| safe_component(pane))
            .collect::<HashSet<_>>();
        let mut removed = 0;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let stem = path.file_stem().and_then(|value| value.to_str());
            if stem.is_some_and(|stem| !active.contains(stem)) {
                fs::remove_file(&path).with_context(|| {
                    format!("failed to remove stale pane state {}", path.display())
                })?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn lock(&self, pane: &str) -> Result<File> {
        let directory = self.server_directory.join("locks");
        runtime::ensure_private_directory(&directory)?;
        let path = directory.join(format!("{}.lock", safe_component(pane)));
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("failed to open hook lock {}", path.display()))?;
        lock.lock_exclusive()
            .with_context(|| format!("failed to lock hook state {}", path.display()))?;
        Ok(lock)
    }
}

#[derive(Debug, Default)]
pub struct LoadedPaneStates {
    pub states: HashMap<String, PaneState>,
    stores: Vec<HookStateStore>,
}

impl LoadedPaneStates {
    pub fn reconcile(&self, active_agent_panes: &HashSet<String>) -> Result<usize> {
        self.stores.iter().try_fold(0, |removed, store| {
            Ok(removed + store.reconcile(active_agent_panes)?)
        })
    }
}

pub fn load_for_tmux_rows(rows: &str) -> LoadedPaneStates {
    let mut stores = HashMap::<String, HookStateStore>::new();
    let mut states = HashMap::new();
    for line in rows.lines().filter(|line| !line.trim().is_empty()) {
        let fields = split_fields(line);
        if fields.len() < 16 {
            continue;
        }
        let pane = fields[5];
        let socket = fields.get(16).copied().filter(|socket| !socket.is_empty());
        let server_id = socket
            .map(runtime::server_id)
            .unwrap_or_else(runtime::current_server_id);
        let store = stores.entry(server_id.clone()).or_insert_with(|| {
            socket
                .map(HookStateStore::for_socket)
                .unwrap_or_else(|| HookStateStore::for_server(server_id))
        });
        if let Ok(Some(state)) = store.load(pane) {
            states.insert(pane.to_owned(), state);
        }
    }
    LoadedPaneStates {
        states,
        stores: stores.into_values().collect(),
    }
}

fn split_fields(line: &str) -> Vec<&str> {
    if line.contains('\u{1f}') {
        line.split('\u{1f}').collect()
    } else {
        line.split(r"\037").collect()
    }
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}
