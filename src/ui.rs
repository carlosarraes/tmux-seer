use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Stdout},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Terminal,
};

use crate::{
    daemon::{popup_option_name, runtime_snapshot_path},
    model::AgentState,
    navigation::NavigationTarget,
    snapshot::AggregateSnapshot,
    tmux::{now_ms, Tmux},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Host,
    Session,
    Window,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DashboardRow {
    pub id: String,
    pub kind: RowKind,
    pub depth: u16,
    pub label: String,
    pub state: Option<AgentState>,
    pub target: Option<NavigationTarget>,
    pub offline: bool,
    fold_target: String,
}

#[derive(Debug, Clone)]
pub struct Dashboard {
    snapshot: AggregateSnapshot,
    rows: Vec<DashboardRow>,
    selected: usize,
    filter: String,
    collapsed: HashSet<String>,
    renamed_sessions: HashMap<(String, String), String>,
}

impl Dashboard {
    pub fn new(snapshot: AggregateSnapshot) -> Self {
        let mut dashboard = Self {
            snapshot,
            rows: Vec::new(),
            selected: 0,
            filter: String::new(),
            collapsed: HashSet::new(),
            renamed_sessions: HashMap::new(),
        };
        dashboard.rebuild(true);
        dashboard
    }

    pub fn title(&self) -> String {
        let agents = self
            .snapshot
            .hosts
            .iter()
            .flat_map(|host| host.agents())
            .count();
        let needs_input = self
            .snapshot
            .hosts
            .iter()
            .filter(|host| host.online)
            .flat_map(|host| host.agents())
            .filter(|pane| pane.state == AgentState::NeedsInput)
            .count();
        format!("Seer · {agents} agents · {needs_input} needs input")
    }

    pub fn rows(&self) -> &[DashboardRow] {
        &self.rows
    }

    pub fn selected(&self) -> Option<&DashboardRow> {
        self.rows.get(self.selected)
    }

    pub fn set_filter(&mut self, filter: impl Into<String>) {
        self.filter = filter.into();
        self.rebuild(true);
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn shortcut_hint(&self) -> &'static str {
        let Some(row) = self.selected() else {
            return "q close";
        };
        if row.offline {
            return match row.kind {
                RowKind::Agent => "↑↓/jk move · Tab fold session · / filter · offline · q close",
                _ => "↑↓/jk move · Tab fold · / filter · offline · q close",
            };
        }
        match row.kind {
            RowKind::Host if row.target.is_some() => {
                "↑↓/jk move · Tab fold · / filter · Enter connect · q close"
            }
            RowKind::Host => "↑↓/jk move · Tab fold · / filter · Enter fold · q close",
            RowKind::Session => {
                "↑↓/jk move · Tab fold · / filter · Enter jump session · r rename · q close"
            }
            RowKind::Window => "↑↓/jk move · Tab fold · / filter · Enter jump window · q close",
            RowKind::Agent => {
                "↑↓/jk move · Tab fold session · / filter · Enter jump pane · q close"
            }
        }
    }

    pub fn selected_session(&self) -> Option<(String, String, String)> {
        let row = self.selected()?;
        let NavigationTarget::Session { host, session_id } = row.target.as_ref()? else {
            return None;
        };
        Some((host.clone(), session_id.clone(), row.label.clone()))
    }

    pub fn update_session_name(&mut self, host: &str, session_id: &str, name: &str) {
        let selected_id = self.selected().map(|row| row.id.clone());
        self.renamed_sessions
            .insert((host.to_owned(), session_id.to_owned()), name.to_owned());
        if let Some(session) = self
            .snapshot
            .hosts
            .iter_mut()
            .find(|snapshot| snapshot.host == host)
            .and_then(|snapshot| {
                snapshot
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == session_id)
            })
        {
            session.name = name.to_owned();
        }
        self.rebuild(false);
        if let Some(id) = selected_id {
            if let Some(index) = self.rows.iter().position(|row| row.id == id) {
                self.selected = index;
            }
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = self
            .selected
            .saturating_add_signed(delta)
            .min(self.rows.len() - 1);
    }

    pub fn toggle_selected(&mut self) {
        let Some(row) = self.selected() else { return };
        let id = row.fold_target.clone();
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id.clone());
        }
        self.rebuild(false);
        if let Some(index) = self.rows.iter().position(|row| row.id == id) {
            self.selected = index;
        }
    }

    pub fn replace_snapshot(&mut self, mut snapshot: AggregateSnapshot) {
        let selected_id = self.selected().map(|row| row.id.clone());
        let mut confirmed = Vec::new();
        for host in &mut snapshot.hosts {
            for session in &mut host.sessions {
                let key = (host.host.clone(), session.id.clone());
                if let Some(name) = self.renamed_sessions.get(&key) {
                    if session.name == *name {
                        confirmed.push(key);
                    } else {
                        session.name.clone_from(name);
                    }
                }
            }
        }
        for key in confirmed {
            self.renamed_sessions.remove(&key);
        }
        self.snapshot = snapshot;
        self.rebuild(false);
        if let Some(id) = selected_id {
            if let Some(index) = self.rows.iter().position(|row| row.id == id) {
                self.selected = index;
            }
        }
    }

    fn rebuild(&mut self, select_by_priority: bool) {
        let needle = self.filter.to_ascii_lowercase();
        let mut rows = Vec::new();
        for host in &self.snapshot.hosts {
            let host_text = format!("{} {}", host.host, host.error.as_deref().unwrap_or(""));
            let mut host_rows = Vec::new();
            for session in &host.sessions {
                let mut session_rows = Vec::new();
                for window in &session.windows {
                    let matching_panes: Vec<_> = window
                        .panes
                        .iter()
                        .filter(|pane| {
                            needle.is_empty()
                                || format!(
                                    "{} {} {} {} {} {}",
                                    host_text,
                                    session.name,
                                    window.name,
                                    pane.project,
                                    pane.agent,
                                    pane.state
                                )
                                .to_ascii_lowercase()
                                .contains(&needle)
                        })
                        .collect();
                    if matching_panes.is_empty() {
                        continue;
                    }
                    let window_id = format!("w:{}:{}", host.host, window.id);
                    let show_window = session.windows.len() > 1;
                    if show_window {
                        session_rows.push(DashboardRow {
                            id: window_id.clone(),
                            kind: RowKind::Window,
                            depth: 2,
                            label: format!("{}:{}", window.index, window.name),
                            state: None,
                            target: host.online.then(|| NavigationTarget::Window {
                                host: host.host.clone(),
                                session_id: session.id.clone(),
                                window_id: window.id.clone(),
                            }),
                            offline: !host.online,
                            fold_target: window_id.clone(),
                        });
                    }
                    if !show_window || !self.collapsed.contains(&window_id) {
                        for pane in matching_panes {
                            session_rows.push(DashboardRow {
                                id: format!(
                                    "a:{}:{}:{}:{}",
                                    pane.key.host,
                                    pane.key.session_id,
                                    pane.key.window_id,
                                    pane.key.pane_id
                                ),
                                kind: RowKind::Agent,
                                depth: if show_window { 3 } else { 2 },
                                label: format!(
                                    "{}.{}  {}  {}  {}  {}",
                                    window.index,
                                    pane.pane_index,
                                    pane.project,
                                    pane.agent,
                                    pane.state,
                                    elapsed(self.snapshot.generated_at_ms, pane.state_since_ms)
                                ),
                                state: Some(pane.state),
                                target: host
                                    .online
                                    .then(|| NavigationTarget::Agent(pane.key.clone())),
                                offline: !host.online,
                                fold_target: format!("s:{}:{}", host.host, session.id),
                            });
                        }
                    }
                }
                if session_rows.is_empty() {
                    continue;
                }
                let session_id = format!("s:{}:{}", host.host, session.id);
                host_rows.push(DashboardRow {
                    id: session_id.clone(),
                    kind: RowKind::Session,
                    depth: 1,
                    label: session.name.clone(),
                    state: None,
                    target: host.online.then(|| NavigationTarget::Session {
                        host: host.host.clone(),
                        session_id: session.id.clone(),
                    }),
                    offline: !host.online,
                    fold_target: session_id.clone(),
                });
                if !self.collapsed.contains(&session_id) {
                    host_rows.extend(session_rows);
                }
            }
            if host_rows.is_empty()
                && !needle.is_empty()
                && !host_text.to_ascii_lowercase().contains(&needle)
            {
                continue;
            }
            let host_id = format!("h:{}", host.host);
            rows.push(DashboardRow {
                id: host_id.clone(),
                kind: RowKind::Host,
                depth: 0,
                label: if host.online {
                    host.host.clone()
                } else {
                    format!("{}  offline", host.host)
                },
                state: (!host.online).then_some(AgentState::Untracked),
                target: (host.online && host.host != "local").then(|| NavigationTarget::Host {
                    host: host.host.clone(),
                }),
                offline: !host.online,
                fold_target: host_id.clone(),
            });
            if !self.collapsed.contains(&host_id) {
                rows.extend(host_rows);
            }
        }
        self.rows = rows;
        self.selected = if select_by_priority {
            [
                AgentState::NeedsInput,
                AgentState::Idle,
                AgentState::Working,
                AgentState::Untracked,
            ]
            .into_iter()
            .find_map(|state| {
                self.rows
                    .iter()
                    .position(|row| !row.offline && row.state == Some(state))
            })
            .unwrap_or(0)
        } else {
            self.selected.min(self.rows.len().saturating_sub(1))
        };
    }
}

pub fn run(client: Option<String>) -> Result<Option<NavigationTarget>> {
    let snapshot = load_snapshot()?;
    let mut dashboard = Dashboard::new(snapshot);
    let tmux = Tmux::new();
    let mut popup_guard = client.map(|client| PopupGuard::new(tmux.clone(), client));
    let mut terminal = open_terminal()?;
    let result = dashboard_loop(&mut terminal, &mut dashboard, &mut popup_guard, &tmux);
    let cleanup = close_terminal(&mut terminal);
    match (result, cleanup) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(selected), Ok(())) => Ok(selected),
    }
}

fn dashboard_loop(
    terminal: &mut Tui,
    dashboard: &mut Dashboard,
    popup_guard: &mut Option<PopupGuard>,
    tmux: &Tmux,
) -> Result<Option<NavigationTarget>> {
    let mut filter_mode = false;
    let mut rename_editor = None;
    let mut status_message = None;
    let mut last_reload = Instant::now();

    let selected =
        loop {
            if let Some(guard) = popup_guard.as_mut() {
                guard.heartbeat();
            }
            terminal.draw(|frame| {
                render(
                    frame,
                    dashboard,
                    filter_mode,
                    rename_editor.as_ref(),
                    status_message.as_deref(),
                )
            })?;
            if event::poll(Duration::from_millis(200))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    status_message = None;
                    if let Some(editor) = rename_editor.as_mut() {
                        match key.code {
                            KeyCode::Esc => rename_editor = None,
                            KeyCode::Enter if editor.name.trim().is_empty() => {
                                status_message = Some("Session name cannot be empty".to_owned());
                            }
                            KeyCode::Enter => {
                                match crate::navigation::Navigator::new(tmux.clone())
                                    .rename_session(&editor.host, &editor.session_id, &editor.name)
                                {
                                    Ok(()) => {
                                        dashboard.update_session_name(
                                            &editor.host,
                                            &editor.session_id,
                                            &editor.name,
                                        );
                                        status_message =
                                            Some(format!("Renamed session to {}", editor.name));
                                        rename_editor = None;
                                    }
                                    Err(error) => status_message = Some(error.to_string()),
                                }
                            }
                            KeyCode::Backspace => editor.backspace(),
                            KeyCode::Char(character) => editor.push(character),
                            _ => {}
                        }
                        continue;
                    }
                    if filter_mode {
                        match key.code {
                            KeyCode::Esc | KeyCode::Enter => filter_mode = false,
                            KeyCode::Backspace => {
                                let mut filter = dashboard.filter().to_owned();
                                filter.pop();
                                dashboard.set_filter(filter);
                            }
                            KeyCode::Char(character) => {
                                let mut filter = dashboard.filter().to_owned();
                                filter.push(character);
                                dashboard.set_filter(filter);
                            }
                            _ => {}
                        }
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break None,
                        KeyCode::Up | KeyCode::Char('k') => dashboard.move_selection(-1),
                        KeyCode::Down | KeyCode::Char('j') => dashboard.move_selection(1),
                        KeyCode::Tab => dashboard.toggle_selected(),
                        KeyCode::Char('/') => filter_mode = true,
                        KeyCode::Char('r') => {
                            if let Some((host, session_id, name)) = dashboard.selected_session() {
                                rename_editor = Some(RenameEditor::new(host, session_id, name));
                            }
                        }
                        KeyCode::Enter => {
                            if let Some(target) =
                                dashboard.selected().and_then(|row| row.target.clone())
                            {
                                break Some(target);
                            } else {
                                dashboard.toggle_selected();
                            }
                        }
                        _ => {}
                    }
                }
            }
            if last_reload.elapsed() >= Duration::from_millis(500) {
                if let Ok(snapshot) = load_snapshot() {
                    dashboard.replace_snapshot(snapshot);
                }
                last_reload = Instant::now();
            }
        };
    Ok(selected)
}

fn render(
    frame: &mut ratatui::Frame<'_>,
    dashboard: &Dashboard,
    filter_mode: bool,
    rename_editor: Option<&RenameEditor>,
    status_message: Option<&str>,
) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    frame.render_widget(
        Paragraph::new(dashboard.title()).style(Style::default().add_modifier(Modifier::BOLD)),
        header,
    );
    let items = dashboard.rows.iter().map(|row| {
        let prefix = match row.kind {
            RowKind::Host | RowKind::Session | RowKind::Window => "▾ ",
            RowKind::Agent => "● ",
        };
        let color = match row.state {
            Some(AgentState::Working) => Color::Rgb(158, 206, 106),
            Some(AgentState::Idle) => Color::Rgb(224, 175, 104),
            Some(AgentState::NeedsInput) => Color::Rgb(122, 162, 247),
            Some(AgentState::Untracked) => Color::Rgb(86, 95, 137),
            None => Color::Reset,
        };
        let style = if row.offline {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(color)
        };
        ListItem::new(Line::from(vec![
            Span::raw("  ".repeat(row.depth as usize)),
            Span::styled(prefix, style),
            Span::styled(&row.label, style),
        ]))
    });
    let list = List::new(items)
        .block(Block::default().borders(Borders::TOP))
        .highlight_style(
            Style::default()
                .bg(Color::Rgb(41, 46, 66))
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("› ");
    let mut state = ListState::default()
        .with_selected((!dashboard.rows.is_empty()).then_some(dashboard.selected));
    frame.render_stateful_widget(list, body, &mut state);
    let footer_text = if let Some(editor) = rename_editor {
        format!("Rename session: {}_ · Enter save · Esc cancel", editor.name)
    } else if filter_mode {
        format!("/{}", dashboard.filter)
    } else if let Some(message) = status_message {
        message.to_owned()
    } else {
        dashboard.shortcut_hint().into()
    };
    frame.render_widget(
        Paragraph::new(footer_text).style(Style::default().fg(Color::DarkGray)),
        footer,
    );
}

fn load_snapshot() -> Result<AggregateSnapshot> {
    let path = runtime_snapshot_path();
    let bytes = fs::read(&path).with_context(|| {
        format!(
            "no Seer snapshot at {}; run `tmux-seer daemon`",
            path.display()
        )
    })?;
    serde_json::from_slice(&bytes).context("invalid Seer runtime snapshot")
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

#[derive(Debug, Clone)]
struct RenameEditor {
    host: String,
    session_id: String,
    name: String,
}

impl RenameEditor {
    fn new(host: String, session_id: String, name: String) -> Self {
        Self {
            host,
            session_id,
            name,
        }
    }

    fn push(&mut self, character: char) {
        self.name.push(character);
    }

    fn backspace(&mut self) {
        self.name.pop();
    }
}

fn open_terminal() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn close_terminal(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

struct PopupGuard {
    tmux: Tmux,
    client: String,
}

impl PopupGuard {
    fn new(tmux: Tmux, client: String) -> Self {
        let mut guard = Self { tmux, client };
        guard.heartbeat();
        guard
    }

    fn heartbeat(&mut self) {
        let expiry = now_ms().saturating_add(2_000).to_string();
        let _ = self
            .tmux
            .set_global_option(&popup_option_name(&self.client), &expiry);
    }
}

impl Drop for PopupGuard {
    fn drop(&mut self) {
        let _ = self
            .tmux
            .unset_global_option(&popup_option_name(&self.client));
    }
}

fn elapsed(now: u64, since: u64) -> String {
    let seconds = now.saturating_sub(since) / 1_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}h", seconds / 3_600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_editor_keeps_target_identity_while_editing_the_name() {
        let mut editor = RenameEditor::new("mac".into(), "$2".into(), "review".into());

        editor.push('s');
        editor.backspace();

        assert_eq!(editor.host, "mac");
        assert_eq!(editor.session_id, "$2");
        assert_eq!(editor.name, "review");
    }
}
