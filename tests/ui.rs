use tmux_seer::{
    model::AgentState,
    snapshot::{AggregateSnapshot, HostSnapshot, SCHEMA_VERSION},
    ui::{Dashboard, RowKind},
};

fn dashboard_with(states: &[AgentState]) -> Dashboard {
    let mut hosts = Vec::new();
    for (index, state) in states.iter().copied().enumerate() {
        let mut host = HostSnapshot::empty(format!("host-{index}"), 1);
        host.push_test_agent(state);
        hosts.push(host);
    }
    Dashboard::new(AggregateSnapshot {
        schema_version: SCHEMA_VERSION,
        generated_at_ms: 10,
        hosts,
    })
}

#[test]
fn title_uses_seer_brand_and_input_count() {
    let dashboard = dashboard_with(&[
        AgentState::Working,
        AgentState::NeedsInput,
        AgentState::Idle,
    ]);
    assert_eq!(dashboard.title(), "Seer · 3 agents · 1 needs input");
}

#[test]
fn single_agent_window_is_collapsed_from_tree() {
    let dashboard = dashboard_with(&[AgentState::Idle]);
    assert!(!dashboard
        .rows()
        .iter()
        .any(|row| row.kind == RowKind::Window));
    assert!(dashboard
        .rows()
        .iter()
        .any(|row| row.kind == RowKind::Agent));
}

#[test]
fn initial_selection_prefers_blue_then_yellow_then_green() {
    let dashboard = dashboard_with(&[
        AgentState::Working,
        AgentState::Idle,
        AgentState::NeedsInput,
    ]);
    assert_eq!(
        dashboard.selected().and_then(|row| row.state),
        Some(AgentState::NeedsInput)
    );
}

#[test]
fn filter_keeps_matching_agent_and_ancestors() {
    let mut dashboard = dashboard_with(&[AgentState::Working, AgentState::Idle]);
    dashboard.set_filter("host-1");
    let rows = dashboard.rows();
    assert!(rows.iter().any(|row| row.label.contains("host-1")));
    assert!(!rows.iter().any(|row| row.label.contains("host-0")));
}

#[test]
fn agent_row_ids_are_unique_across_hosts() {
    let dashboard = dashboard_with(&[AgentState::Idle, AgentState::Working]);
    let ids = dashboard
        .rows()
        .iter()
        .filter(|row| row.kind == RowKind::Agent)
        .map(|row| &row.id)
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids.len(), 2);
}

#[test]
fn offline_blue_agent_does_not_steal_initial_selection() {
    let mut snapshot = AggregateSnapshot {
        schema_version: SCHEMA_VERSION,
        generated_at_ms: 10,
        hosts: Vec::new(),
    };
    let mut offline = HostSnapshot::empty("offline", 1);
    offline.push_test_agent(AgentState::NeedsInput);
    offline.online = false;
    let mut online = HostSnapshot::empty("online", 1);
    online.push_test_agent(AgentState::Idle);
    snapshot.hosts = vec![offline, online];
    let dashboard = Dashboard::new(snapshot);

    let selected = dashboard.selected().unwrap();
    assert_eq!(selected.state, Some(AgentState::Idle));
    assert!(!selected.offline);
}
