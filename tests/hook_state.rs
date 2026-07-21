use std::collections::HashSet;

use tmux_seer::{
    hook_state::HookStateStore,
    model::{AgentKind, EventKind, NormalizedEvent},
};

#[test]
fn reconciliation_removes_state_for_panes_without_agents() {
    let runtime = tempfile::tempdir().unwrap();
    temp_env::with_var("TMUX_SEER_RUNTIME_DIR", Some(runtime.path()), || {
        let store = HookStateStore::for_server("server");
        store.apply("%live", started(), 10).unwrap();
        store.apply("%stale", started(), 10).unwrap();

        let removed = store
            .reconcile(&HashSet::from(["%live".to_owned()]))
            .unwrap();

        assert_eq!(removed, 1);
        assert!(store.load("%live").unwrap().is_some());
        assert!(store.load("%stale").unwrap().is_none());
    });
}

fn started() -> NormalizedEvent {
    NormalizedEvent {
        agent: AgentKind::Claude,
        kind: EventKind::Started,
        session_id: Some("s1".to_owned()),
        turn_id: None,
        reason: None,
    }
}
