#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSchedule {
    failures: u32,
    next_due_ms: u64,
}

impl HostSchedule {
    pub const fn immediate() -> Self {
        Self {
            failures: 0,
            next_due_ms: 0,
        }
    }

    pub const fn failures(&self) -> u32 {
        self.failures
    }

    pub const fn next_due_ms(&self) -> u64 {
        self.next_due_ms
    }

    pub const fn is_due(&self, now_ms: u64) -> bool {
        now_ms >= self.next_due_ms
    }

    pub fn success(&mut self, now_ms: u64, interval_ms: u64) {
        self.failures = 0;
        self.next_due_ms = now_ms.saturating_add(interval_ms);
    }

    pub fn failure(&mut self, now_ms: u64, base_ms: u64, maximum_ms: u64) {
        self.failures = self.failures.saturating_add(1);
        let shift = self.failures.saturating_sub(1).min(63);
        let multiplier = 1_u64.checked_shl(shift).unwrap_or(u64::MAX);
        let delay = base_ms.saturating_mul(multiplier).min(maximum_ms);
        self.next_due_ms = now_ms.saturating_add(delay);
    }

    pub fn force(&mut self, now_ms: u64) {
        self.next_due_ms = now_ms;
    }
}

impl Default for HostSchedule {
    fn default() -> Self {
        Self::immediate()
    }
}
