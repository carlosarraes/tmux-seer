use tmux_seer::{
    model::AgentState,
    navigation::NavigationTarget,
    snapshot::{AggregateSnapshot, HostSnapshot, SCHEMA_VERSION},
    ui::{Dashboard, RowKind},
};

fn snapshot_with(states: &[AgentState]) -> AggregateSnapshot {
    let mut hosts = Vec::new();
    for (index, state) in states.iter().copied().enumerate() {
        let mut host = HostSnapshot::empty(format!("host-{index}"), 1);
        host.push_test_agent(state);
        hosts.push(host);
    }
    AggregateSnapshot {
        schema_version: SCHEMA_VERSION,
        generated_at_ms: 10,
        hosts,
    }
}

fn dashboard_with(states: &[AgentState]) -> Dashboard {
    Dashboard::new(snapshot_with(states))
}

#[test]
fn activity_age_advances_without_a_new_snapshot() {
    let mut host = HostSnapshot::empty("local", 1_000);
    host.push_test_agent(AgentState::Working);
    let snapshot = AggregateSnapshot {
        schema_version: SCHEMA_VERSION,
        generated_at_ms: 1_000,
        hosts: vec![host],
    };
    let mut dashboard = Dashboard::new_at(snapshot, 1_000);

    assert!(dashboard
        .rows()
        .iter()
        .any(|row| row.kind == RowKind::Agent && row.label.ends_with("0s")));

    dashboard.refresh_elapsed(3_000);

    assert!(dashboard
        .rows()
        .iter()
        .any(|row| row.kind == RowKind::Agent && row.label.ends_with("2s")));
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

#[test]
fn tab_on_agent_folds_its_containing_session() {
    let mut dashboard = dashboard_with(&[AgentState::Working]);
    assert_eq!(dashboard.selected().unwrap().kind, RowKind::Agent);
    let expanded_rows = dashboard.rows().len();

    dashboard.toggle_selected();

    assert!(dashboard.rows().len() < expanded_rows);
    assert_eq!(dashboard.selected().unwrap().kind, RowKind::Session);
}

#[test]
fn hierarchy_rows_expose_typed_navigation_targets() {
    let dashboard = dashboard_with(&[AgentState::Working]);
    let host = dashboard
        .rows()
        .iter()
        .find(|row| row.kind == RowKind::Host)
        .unwrap();
    let session = dashboard
        .rows()
        .iter()
        .find(|row| row.kind == RowKind::Session)
        .unwrap();
    let agent = dashboard
        .rows()
        .iter()
        .find(|row| row.kind == RowKind::Agent)
        .unwrap();

    assert_eq!(
        host.target,
        Some(NavigationTarget::Host {
            host: "host-0".into()
        })
    );
    assert_eq!(
        session.target,
        Some(NavigationTarget::Session {
            host: "host-0".into(),
            session_id: "$test".into(),
        })
    );
    assert!(matches!(agent.target, Some(NavigationTarget::Agent(_))));
}

#[test]
fn remote_shortcut_hints_explain_how_to_return() {
    let mut dashboard = dashboard_with(&[AgentState::Working]);
    assert_eq!(
        dashboard.shortcut_hint(),
        "j/k move · h/l host · Tab fold · / filter · Enter attach · R refresh · Prefix+d return"
    );

    dashboard.move_selection(-1);
    assert_eq!(
        dashboard.shortcut_hint(),
        "j/k move · h/l host · Tab fold · Enter attach · r rename · R refresh · Prefix+d return"
    );

    dashboard.move_selection(-1);
    assert_eq!(
        dashboard.shortcut_hint(),
        "j/k move · h/l host · Tab fold · / filter · Enter connect · R refresh · Prefix+d return"
    );
}

#[test]
fn local_shortcut_hints_keep_jump_wording() {
    let mut host = HostSnapshot::empty("local", 1);
    host.push_test_agent(AgentState::Working);
    let mut dashboard = Dashboard::new(AggregateSnapshot {
        schema_version: SCHEMA_VERSION,
        generated_at_ms: 10,
        hosts: vec![host],
    });

    assert_eq!(
        dashboard.shortcut_hint(),
        "j/k move · h/l host · Tab fold · / filter · Enter jump · R refresh · q close"
    );
    dashboard.move_selection(-1);
    assert_eq!(
        dashboard.shortcut_hint(),
        "j/k move · h/l host · Tab fold · Enter jump · r rename · R refresh · q close"
    );
}

#[test]
fn selected_session_can_be_renamed_optimistically_by_stable_id() {
    let mut dashboard = dashboard_with(&[AgentState::Working]);
    let stale_snapshot = snapshot_with(&[AgentState::Working]);
    dashboard.move_selection(-1);

    assert_eq!(
        dashboard.selected_session(),
        Some(("host-0".into(), "$test".into(), "test".into()))
    );

    dashboard.update_session_name("host-0", "$test", "deep work");
    dashboard.replace_snapshot(stale_snapshot);
    let session = dashboard
        .rows()
        .iter()
        .find(|row| row.kind == RowKind::Session)
        .unwrap();
    assert_eq!(session.label, "deep work");
}

#[test]
fn offline_rows_do_not_advertise_navigation() {
    let mut snapshot = snapshot_with(&[AgentState::Idle]);
    snapshot.hosts[0].online = false;
    let mut dashboard = Dashboard::new(snapshot);
    dashboard.move_selection(2);

    assert_eq!(dashboard.selected().unwrap().kind, RowKind::Agent);
    assert!(!dashboard.shortcut_hint().contains("Enter"));
}

#[test]
fn host_cycling_wraps_and_skips_offline_hosts() {
    let mut local = HostSnapshot::empty("local", 1);
    local.push_test_agent(AgentState::NeedsInput);
    let mac = HostSnapshot::empty("mac", 1);
    let mut offline = HostSnapshot::empty("offline", 1);
    offline.push_test_agent(AgentState::NeedsInput);
    offline.online = false;
    let mut zapsign = HostSnapshot::empty("zapsign", 1);
    zapsign.push_test_agent(AgentState::Working);
    let mut dashboard = Dashboard::new(AggregateSnapshot {
        schema_version: SCHEMA_VERSION,
        generated_at_ms: 10,
        hosts: vec![local, mac, offline, zapsign],
    });

    dashboard.cycle_host(1);
    assert_eq!(dashboard.selected().unwrap().label, "mac");
    dashboard.cycle_host(1);
    assert_selected_agent(&dashboard, "zapsign", AgentState::Working);
    dashboard.cycle_host(1);
    assert_selected_agent(&dashboard, "local", AgentState::NeedsInput);
    dashboard.cycle_host(-1);
    assert_selected_agent(&dashboard, "zapsign", AgentState::Working);
}

#[test]
fn host_cycling_lands_on_the_highest_priority_visible_agent() {
    let mut local = HostSnapshot::empty("local", 1);
    local.push_test_agent(AgentState::NeedsInput);
    let mut mac = HostSnapshot::empty("mac", 1);
    mac.push_test_agent(AgentState::Working);
    mac.push_test_agent(AgentState::Idle);
    mac.push_test_agent(AgentState::NeedsInput);
    let mut dashboard = Dashboard::new(AggregateSnapshot {
        schema_version: SCHEMA_VERSION,
        generated_at_ms: 10,
        hosts: vec![local, mac],
    });

    dashboard.cycle_host(1);

    assert_selected_agent(&dashboard, "mac", AgentState::NeedsInput);
}

fn assert_selected_agent(dashboard: &Dashboard, host: &str, state: AgentState) {
    let selected = dashboard.selected().unwrap();
    assert_eq!(selected.state, Some(state));
    assert!(matches!(
        selected.target.as_ref(),
        Some(NavigationTarget::Agent(key)) if key.host == host
    ));
}
