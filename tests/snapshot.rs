use tmux_seer::model::{AgentKind, AgentState};
use tmux_seer::snapshot::{
    aggregate_online, parse_tmux_rows, parse_tmux_rows_with_processes, status_widget,
    status_widget_with, ProcessTable, StatusColors, SCHEMA_VERSION,
};

const SEP: char = '\u{1f}';

fn row(fields: &[&str]) -> String {
    fields.join(&SEP.to_string())
}

fn tmux_35_row(fields: &[&str]) -> String {
    fields.join(r"\037")
}

#[test]
fn parses_only_agent_panes_and_preserves_hierarchy() {
    let input = [
        row(&[
            "$1",
            "main",
            "@2",
            "1",
            "app",
            "%3",
            "0",
            "/tmp/shop",
            "100",
            "zsh",
            "",
            "",
            "",
            "",
            "",
            "",
        ]),
        row(&[
            "$1",
            "main",
            "@2",
            "1",
            "app",
            "%4",
            "1",
            "/tmp/shop",
            "101",
            "claude",
            "claude",
            "working",
            "50",
            "s1",
            "t1",
            "tool",
        ]),
    ]
    .join("\n");

    let snapshot = parse_tmux_rows("local", 100, &input).unwrap();

    assert_eq!(snapshot.schema_version, SCHEMA_VERSION);
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.sessions[0].windows[0].panes.len(), 1);
    assert_eq!(
        snapshot.sessions[0].windows[0].panes[0].agent,
        AgentKind::Claude
    );
    assert_eq!(
        snapshot.sessions[0].windows[0].panes[0].state,
        AgentState::Working
    );
}

#[test]
fn parses_separators_escaped_by_tmux_35() {
    let input = tmux_35_row(&[
        "$1",
        "main",
        "@2",
        "1",
        "app",
        "%4",
        "1",
        "/tmp/shop",
        "101",
        "claude",
        "claude",
        "idle",
        "50",
        "s1",
        "",
        "",
    ]);

    let snapshot = parse_tmux_rows("zapsign", 100, &input).unwrap();

    assert_eq!(
        snapshot.sessions[0].windows[0].panes[0].agent,
        AgentKind::Claude
    );
    assert_eq!(
        snapshot.sessions[0].windows[0].panes[0].state,
        AgentState::Idle
    );
}

#[test]
fn recognized_process_without_options_is_untracked() {
    let input = row(&[
        "$1",
        "main",
        "@2",
        "1",
        "app",
        "%4",
        "1",
        "/tmp/shop",
        "101",
        "codex",
        "",
        "",
        "",
        "",
        "",
        "",
    ]);

    let snapshot = parse_tmux_rows("local", 100, &input).unwrap();

    assert_eq!(
        snapshot.sessions[0].windows[0].panes[0].agent,
        AgentKind::Codex
    );
    assert_eq!(
        snapshot.sessions[0].windows[0].panes[0].state,
        AgentState::Untracked
    );
}

#[test]
fn offline_hosts_do_not_affect_aggregate() {
    use tmux_seer::snapshot::HostSnapshot;

    let mut local = HostSnapshot::empty("local", 1);
    local.push_test_agent(AgentState::Working);
    let mut remote = HostSnapshot::empty("mac", 1);
    remote.push_test_agent(AgentState::NeedsInput);
    remote.online = false;

    assert_eq!(aggregate_online([&local, &remote]), AgentState::Working);
}

#[test]
fn widget_uses_expected_color_and_single_square() {
    assert_eq!(
        status_widget(AgentState::NeedsInput),
        "#[fg=#7aa2f7]■#[default]"
    );
}

#[test]
fn widget_accepts_configured_colors() {
    let colors = StatusColors {
        working: "green".into(),
        idle: "yellow".into(),
        needs_input: "blue".into(),
        untracked: "gray".into(),
    };
    assert_eq!(
        status_widget_with(AgentState::Working, &colors),
        "#[fg=green]■#[default]"
    );
}

#[test]
fn detects_working_codex_hidden_below_node_process() {
    let input = row(&[
        "$1",
        "main",
        "@2",
        "1",
        "app",
        "%4",
        "1",
        "/tmp/shop",
        "101",
        "node",
        "",
        "",
        "",
        "",
        "",
        "",
    ]);
    let processes = ProcessTable::parse(
        "101 1 node\n202 101 /usr/bin/node /opt/codex/bin/codex.js\n203 202 rg foo",
    );

    let snapshot = parse_tmux_rows_with_processes("local", 100, &input, &processes).unwrap();

    assert_eq!(
        snapshot.sessions[0].windows[0].panes[0].agent,
        AgentKind::Codex
    );
    assert_eq!(
        snapshot.sessions[0].windows[0].panes[0].state,
        AgentState::Working
    );
}

#[test]
fn codex_process_fallback_is_idle_before_the_first_hook_event() {
    let input = row(&[
        "$1",
        "main",
        "@2",
        "1",
        "app",
        "%4",
        "1",
        "/tmp/shop",
        "101",
        "zsh",
        "",
        "",
        "",
        "",
        "",
        "",
    ]);
    let processes = ProcessTable::parse(
        "101 1 zsh\n202 101 node /home/me/.local/bin/codex\n203 202 /opt/codex/bin/codex\n204 203 /opt/codex/bin/codex-code-mode-host",
    );

    let snapshot = parse_tmux_rows_with_processes("local", 100, &input, &processes).unwrap();

    assert_eq!(
        snapshot.sessions[0].windows[0].panes[0].state,
        AgentState::Idle
    );
}

#[test]
fn codex_process_fallback_is_working_while_the_runner_has_a_tool_child() {
    let input = row(&[
        "$1",
        "main",
        "@2",
        "1",
        "app",
        "%4",
        "1",
        "/tmp/shop",
        "101",
        "zsh",
        "",
        "",
        "",
        "",
        "",
        "",
    ]);
    let processes = ProcessTable::parse(
        "101 1 zsh\n202 101 node /home/me/.local/bin/codex\n203 202 /opt/codex/bin/codex\n204 203 /opt/codex/bin/codex-code-mode-host\n205 204 /bin/zsh -c cargo test",
    );

    let snapshot = parse_tmux_rows_with_processes("local", 100, &input, &processes).unwrap();

    assert_eq!(
        snapshot.sessions[0].windows[0].panes[0].state,
        AgentState::Working
    );
}

#[test]
fn codex_process_fallback_detects_a_tool_sibling_of_the_code_mode_host() {
    let input = row(&[
        "$1",
        "main",
        "@2",
        "1",
        "app",
        "%4",
        "1",
        "/tmp/shop",
        "101",
        "zsh",
        "",
        "",
        "",
        "",
        "",
        "",
    ]);
    let processes = ProcessTable::parse(
        "101 1 zsh\n202 101 node /home/me/.local/bin/codex\n203 202 /opt/codex/bin/codex\n204 203 /opt/codex/bin/codex-code-mode-host\n205 203 /bin/zsh -c cargo test",
    );

    let snapshot = parse_tmux_rows_with_processes("local", 100, &input, &processes).unwrap();

    assert_eq!(
        snapshot.sessions[0].windows[0].panes[0].state,
        AgentState::Working
    );
}

#[test]
fn process_table_accepts_ps_column_spacing() {
    let processes =
        ProcessTable::parse("  101     1 node\n  202   101 /usr/bin/node /opt/codex/bin/codex.js");
    assert_eq!(processes.agent_below(101), Some(AgentKind::Codex));
}

#[test]
fn stale_hook_options_are_ignored_after_agent_process_exits() {
    let input = row(&[
        "$1",
        "main",
        "@2",
        "1",
        "app",
        "%4",
        "1",
        "/tmp/shop",
        "101",
        "zsh",
        "claude",
        "idle",
        "50",
        "s1",
        "",
        "",
    ]);
    let processes = ProcessTable::parse("101 1 zsh\n202 101 sleep 100");

    let snapshot = parse_tmux_rows_with_processes("local", 100, &input, &processes).unwrap();

    assert!(snapshot.sessions.is_empty());
}
