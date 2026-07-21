use std::fs;

use tmux_seer::diagnostics::{
    format_health, format_host_health, Diagnostics, HealthSnapshot, HostHealth, LogLevel,
};

#[test]
fn logs_rotate_at_the_configured_bound() {
    let directory = tempfile::tempdir().unwrap();
    let diagnostics = Diagnostics::at(directory.path(), 96, LogLevel::Debug).unwrap();

    diagnostics
        .log(LogLevel::Warn, "remote", &"first".repeat(12))
        .unwrap();
    diagnostics
        .log(LogLevel::Warn, "remote", &"second".repeat(12))
        .unwrap();

    assert!(directory.path().join("seer.log").exists());
    assert!(directory.path().join("seer.log.1").exists());
    assert!(
        fs::metadata(directory.path().join("seer.log"))
            .unwrap()
            .len()
            <= 96
    );
}

#[test]
fn log_level_filters_debug_and_sanitizes_newlines() {
    let directory = tempfile::tempdir().unwrap();
    let diagnostics = Diagnostics::at(directory.path(), 1_024, LogLevel::Warn).unwrap();

    diagnostics
        .log(LogLevel::Debug, "daemon", "hidden")
        .unwrap();
    diagnostics
        .log(LogLevel::Warn, "daemon", "line one\nline two")
        .unwrap();

    let log = fs::read_to_string(directory.path().join("seer.log")).unwrap();
    assert!(!log.contains("hidden"));
    assert!(log.contains("line one line two"));
    assert_eq!(log.lines().count(), 1);
}

#[test]
fn health_summary_exposes_latency_backoff_and_errors() {
    let health = HealthSnapshot {
        generated_at_ms: 10_000,
        local_scan_ms: 12,
        hosts: vec![HostHealth {
            host: "vps".into(),
            online: false,
            latency_ms: 2_003,
            failures: 3,
            next_retry_ms: 18_000,
            last_error: Some("connection timed out".into()),
        }],
    };

    let output = format_health(&health, 10_000);
    assert!(output.contains("local scan: 12ms"));
    assert!(output.contains("vps: offline, 2003ms, 3 failures, retry in 8s"));
    assert!(output.contains("connection timed out"));
    assert_eq!(format_host_health(&health.hosts[0], 10_000).0, "warn");
}

#[test]
fn online_host_health_is_ok() {
    let health = HostHealth {
        host: "mac".into(),
        online: true,
        latency_ms: 22,
        failures: 0,
        next_retry_ms: 12_000,
        last_error: None,
    };

    let (level, line) = format_host_health(&health, 10_000);
    assert_eq!(level, "ok");
    assert!(line.contains("mac: online, 22ms"));
}
