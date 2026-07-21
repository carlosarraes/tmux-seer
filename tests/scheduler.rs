use tmux_seer::scheduler::HostSchedule;

#[test]
fn failures_back_off_exponentially_to_the_configured_cap() {
    let mut schedule = HostSchedule::immediate();

    schedule.failure(1_000, 2_000, 60_000);
    assert_eq!(schedule.failures(), 1);
    assert_eq!(schedule.next_due_ms(), 3_000);

    schedule.failure(3_000, 2_000, 60_000);
    assert_eq!(schedule.failures(), 2);
    assert_eq!(schedule.next_due_ms(), 7_000);

    for now in [7_000, 15_000, 31_000, 63_000, 123_000] {
        schedule.failure(now, 2_000, 60_000);
    }
    assert_eq!(schedule.next_due_ms(), 183_000);
}

#[test]
fn success_resets_backoff_and_force_makes_host_due() {
    let mut schedule = HostSchedule::immediate();
    schedule.failure(1_000, 2_000, 60_000);
    schedule.failure(3_000, 2_000, 60_000);

    schedule.success(4_000, 2_000);
    assert_eq!(schedule.failures(), 0);
    assert_eq!(schedule.next_due_ms(), 6_000);
    assert!(!schedule.is_due(5_999));

    schedule.force(5_000);
    assert!(schedule.is_due(5_000));
}

#[test]
fn one_hosts_backoff_does_not_change_another_hosts_schedule() {
    let mut slow = HostSchedule::immediate();
    let mut healthy = HostSchedule::immediate();

    slow.failure(10, 2_000, 60_000);
    healthy.success(10, 2_000);

    assert_eq!(slow.next_due_ms(), 2_010);
    assert_eq!(healthy.next_due_ms(), 2_010);
    slow.failure(2_010, 2_000, 60_000);
    assert_eq!(slow.next_due_ms(), 6_010);
    assert_eq!(healthy.next_due_ms(), 2_010);
}
