use std::{
    collections::HashSet,
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
    snapshot::{AgentKey, AggregateSnapshot},
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
    pub key: Option<AgentKey>,
    pub offline: bool,
}

#[derive(Debug, Clone)]
pub struct Dashboard {
    snapshot: AggregateSnapshot,
    rows: Vec<DashboardRow>,
    selected: usize,
    filter: String,
    collapsed: HashSet<String>,
}

impl Dashboard {
    pub fn new(snapshot: AggregateSnapshot) -> Self {
        let mut dashboard = Self {
            snapshot,
            rows: Vec::new(),
            selected: 0,
            filter: String::new(),
            collapsed: HashSet::new(),
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
        if row.kind == RowKind::Agent {
            return;
        }
        let id = row.id.clone();
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
        self.rebuild(false);
    }

    pub fn replace_snapshot(&mut self, snapshot: AggregateSnapshot) {
        let selected_id = self.selected().map(|row| row.id.clone());
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
                            key: None,
                            offline: !host.online,
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
                                key: host.online.then(|| pane.key.clone()),
                                offline: !host.online,
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
                    key: None,
                    offline: !host.online,
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
                key: None,
                offline: !host.online,
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

pub fn run(client: Option<String>) -> Result<Option<AgentKey>> {
    let snapshot = load_snapshot()?;
    let mut dashboard = Dashboard::new(snapshot);
    let tmux = Tmux::new();
    let mut popup_guard = client.map(|client| PopupGuard::new(tmux.clone(), client));
    let mut terminal = open_terminal()?;
    let result = dashboard_loop(&mut terminal, &mut dashboard, &mut popup_guard);
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
) -> Result<Option<AgentKey>> {
    let mut filter_mode = false;
    let mut last_reload = Instant::now();

    let selected = loop {
        if let Some(guard) = popup_guard.as_mut() {
            guard.heartbeat();
        }
        terminal.draw(|frame| render(frame, dashboard, filter_mode))?;
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
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
                    KeyCode::Enter => {
                        if let Some(key) = dashboard.selected().and_then(|row| row.key.clone()) {
                            break Some(key);
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

fn render(frame: &mut ratatui::Frame<'_>, dashboard: &Dashboard, filter_mode: bool) {
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
    let footer_text = if filter_mode {
        format!("/{}", dashboard.filter)
    } else {
        "↑↓/jk move · Tab fold · / filter · Enter jump · q close".into()
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
