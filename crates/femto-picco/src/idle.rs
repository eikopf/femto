//! Monotonic inactivity tracking for automatic sleep.
#[derive(Debug)]
pub struct Idle {
    timeout_ms: u64,
    last_ms: u64,
    activity_ms: u64,
    attempted: bool,
}
impl Idle {
    pub fn new(now_ms: u64, timeout_ms: u64) -> Self {
        Self {
            timeout_ms,
            last_ms: now_ms,
            activity_ms: now_ms,
            attempted: false,
        }
    }
    pub fn configure(&mut self, now_ms: u64, timeout_ms: u64) {
        if self.timeout_ms != timeout_ms {
            self.timeout_ms = timeout_ms;
            self.activity(now_ms);
        }
    }
    pub fn activity(&mut self, now_ms: u64) {
        self.last_ms = now_ms;
        self.activity_ms = now_ms;
        self.attempted = false;
    }
    pub fn suppress_until_activity(&mut self) {
        self.attempted = true;
    }
    /// Emits at most one attempt between user actions. Busy work resets elapsed idle time.
    pub fn poll(&mut self, now_ms: u64, enabled: bool, busy: bool) -> bool {
        if now_ms < self.last_ms || !enabled || busy {
            self.activity_ms = now_ms;
        }
        self.last_ms = now_ms;
        if self.attempted || !enabled || busy || self.timeout_ms == 0 {
            return false;
        }
        if now_ms.saturating_sub(self.activity_ms) >= self.timeout_ms {
            self.attempted = true;
            return true;
        }
        false
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_and_failed_attempt_require_new_activity() {
        let mut idle = Idle::new(5, 100);
        assert!(!idle.poll(104, true, false));
        assert!(idle.poll(105, true, false));
        assert!(!idle.poll(1000, true, false));
        assert!(!idle.poll(1001, true, true));
        assert!(!idle.poll(2000, true, false));
        idle.activity(2001);
        assert!(!idle.poll(2100, true, false));
        assert!(idle.poll(2101, true, false));
        idle.activity(3000);
        idle.suppress_until_activity();
        assert!(!idle.poll(3100, true, false));
    }
    #[test]
    fn configuration_changes_restart_timer_and_off_disables_it() {
        let mut idle = Idle::new(0, 100);
        idle.configure(90, 100);
        assert!(idle.poll(100, true, false));
        idle.configure(101, 0);
        assert!(!idle.poll(1000, true, false));
        idle.configure(1001, 50);
        assert!(!idle.poll(1050, true, false));
        assert!(idle.poll(1051, true, false));
    }
    #[test]
    fn busy_disabled_and_clock_rollback_restart_interval() {
        let mut idle = Idle::new(0, 100);
        for (time, enabled, busy) in [(200, true, true), (300, false, false), (10, true, false)] {
            assert!(!idle.poll(time, enabled, busy));
        }
        assert!(!idle.poll(109, true, false));
        assert!(idle.poll(110, true, false));
        assert!(!Idle::new(0, 0).poll(u64::MAX, true, false));
        assert!(!Idle::new(u64::MAX - 10, 100).poll(u64::MAX, true, false));
    }
}
