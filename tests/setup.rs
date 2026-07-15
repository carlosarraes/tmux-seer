use std::fs;
use tempfile::tempdir;
use tmux_seer::setup::{
    apply_json_integration, merge_hook_json, pi_extension_source, preview_target,
    remote_setup_script, snapshot_freshness, Integration, SetupItem, SetupModel,
};

#[test]
fn hook_merge_preserves_unrelated_entries_and_is_idempotent() {
    let original = serde_json::json!({
        "custom": true,
        "hooks": {
            "Stop": [{"hooks": [{"type": "command", "command": "echo existing"}]}]
        }
    });

    let once = merge_hook_json(original.clone(), Integration::Codex, false).unwrap();
    let twice = merge_hook_json(once.clone(), Integration::Codex, false).unwrap();

    assert_eq!(once, twice);
    assert_eq!(once["custom"], true);
    let stop = once["hooks"]["Stop"].as_array().unwrap();
    assert!(stop
        .iter()
        .any(|group| group.to_string().contains("echo existing")));
    assert_eq!(
        stop.iter()
            .filter(|group| group.to_string().contains("tmux-seer hook codex"))
            .count(),
        1
    );
}

#[test]
fn uninstall_removes_only_owned_hook_groups() {
    let original = serde_json::json!({
        "hooks": {
            "Stop": [
                {"hooks": [{"type": "command", "command": "echo existing"}]},
                {"hooks": [{"type": "command", "command": "tmux-seer hook codex Stop"}]}
            ]
        }
    });
    let removed = merge_hook_json(original, Integration::Codex, true).unwrap();
    let serialized = removed.to_string();
    assert!(serialized.contains("echo existing"));
    assert!(!serialized.contains("tmux-seer hook codex"));
}

#[test]
fn changed_json_is_backed_up_and_written_atomically() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("hooks.json");
    fs::write(&path, r#"{"keep":"yes"}"#).unwrap();

    let result = apply_json_integration(&path, Integration::Codex, false).unwrap();

    assert!(result.changed);
    assert!(result.backup.as_ref().unwrap().exists());
    let value: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
    assert_eq!(value["keep"], "yes");
    assert!(value.to_string().contains("tmux-seer hook codex"));
}

#[test]
fn malformed_json_is_not_modified_or_backed_up() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("hooks.json");
    fs::write(&path, "not json").unwrap();

    assert!(apply_json_integration(&path, Integration::Claude, false).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "not json");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn pi_extension_covers_lifecycle_and_structured_question_tools() {
    let source = pi_extension_source();
    for event in [
        "session_start",
        "agent_start",
        "agent_end",
        "session_shutdown",
    ] {
        assert!(source.contains(event));
    }
    assert!(source.contains("tool_execution_start"));
    assert!(source.contains("request_user_input"));
}

#[test]
fn setup_preselects_available_missing_integrations() {
    let model = SetupModel::new(
        vec![
            SetupItem::detected("local", Integration::Claude, true, false),
            SetupItem::detected("local", Integration::Codex, true, true),
            SetupItem::detected("mac", Integration::Pi, false, false),
        ],
        false,
    );

    assert!(model.items[0].selected);
    assert!(!model.items[1].selected);
    assert!(!model.items[2].selected);
}

#[test]
fn uninstall_preselects_only_configured_integrations() {
    let model = SetupModel::new(
        vec![
            SetupItem::detected("local", Integration::Claude, true, false),
            SetupItem::detected("local", Integration::Codex, true, true),
        ],
        true,
    );

    assert!(!model.items[0].selected);
    assert!(model.items[1].selected);
}

#[test]
fn remote_setup_bootstraps_version_matched_binary_before_hooks() {
    let script = remote_setup_script("claude,codex", false);
    let version = env!("CARGO_PKG_VERSION");
    assert!(script.contains(&format!(
        "https://github.com/carlosarraes/tmux-seer/releases/download/v{version}/install.sh"
    )));
    assert!(!script.contains("raw.githubusercontent.com"));
    assert!(script.contains(&format!("TMUX_SEER_VERSION=v{version}")));
    assert!(script.contains("setup --non-interactive --agents claude,codex"));
}

#[test]
fn uninstall_is_noop_when_configuration_has_no_owned_handlers() {
    let original = serde_json::json!({
        "hooks": {"Stop": []},
        "keep": "exactly"
    });
    assert_eq!(
        merge_hook_json(original.clone(), Integration::Codex, true).unwrap(),
        original
    );
}

#[test]
fn review_target_names_exact_host_and_file() {
    assert_eq!(
        preview_target("mac", Integration::Codex),
        "mac:~/.codex/hooks.json"
    );
}

#[test]
fn doctor_classifies_runtime_snapshot_freshness() {
    assert_eq!(snapshot_freshness(10_000, 9_000).0, "ok");
    assert_eq!(snapshot_freshness(20_000, 1_000).0, "warn");
}
