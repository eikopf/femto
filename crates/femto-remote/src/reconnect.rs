//! Retry timing for one configured peer. The platform must finish teardown first.

pub const INITIAL_DELAY_MS: u64 = 2_000;
pub const MAX_DELAY_MS: u64 = 60_000;
pub const STABLE_MS: u64 = 30_000;

pub struct Retry {
    delay: u64,
    due: Option<u64>,
    ready_since: Option<u64>,
    active: bool,
    stopped: bool,
}

impl Default for Retry {
    fn default() -> Self {
        Self {
            delay: INITIAL_DELAY_MS,
            due: None,
            ready_since: None,
            active: true,
            stopped: false,
        }
    }
}

impl Retry {
    pub fn ready(&mut self, now_ms: u64, ready: bool) {
        if !self.active || self.stopped {
            return;
        }
        if ready {
            let since = *self.ready_since.get_or_insert(now_ms);
            if now_ms.saturating_sub(since) >= STABLE_MS {
                self.delay = INITIAL_DELAY_MS;
            }
        } else {
            self.ready_since = None;
        }
    }

    /// Call only after acknowledged shutdown and complete driver destruction.
    pub fn released(&mut self, now_ms: u64) {
        if !self.active {
            return;
        }
        self.active = false;
        self.ready_since = None;
        if !self.stopped {
            // Overflow leaves retries disabled rather than immediately due.
            self.due = now_ms.checked_add(self.delay);
            self.delay = (self.delay * 2).min(MAX_DELAY_MS);
        }
    }

    pub fn take_attempt(&mut self, now_ms: u64) -> bool {
        if self.stopped || self.active || !self.due.is_some_and(|due| now_ms >= due) {
            return false;
        }
        self.due = None;
        self.active = true;
        true
    }

    pub fn stop(&mut self) {
        self.stopped = true;
        self.due = None;
    }
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn teardown_is_required_and_backoff_is_capped() {
        let mut retry = Retry::default();
        let mut now = 0;
        assert!(!retry.take_attempt(u64::MAX));
        for delay in [2000, 4000, 8000, 16000, 32000, 60000, 60000] {
            retry.released(now);
            retry.released(now + 1); // duplicate completion cannot postpone retries
            assert!(!retry.take_attempt(now + delay - 1));
            now += delay;
            assert!(retry.take_attempt(now));
            assert!(!retry.take_attempt(now));
        }
    }
    #[test]
    fn brief_connections_do_not_reset_backoff_but_stable_connections_do() {
        let mut retry = Retry::default();
        retry.ready(0, true);
        retry.ready(29_999, false);
        retry.released(30_000);
        assert!(retry.take_attempt(32_000));
        retry.released(33_000);
        assert!(!retry.take_attempt(36_999));
        assert!(retry.take_attempt(37_000));
        retry.ready(38_000, true);
        retry.ready(68_000, true);
        retry.ready(68_001, false);
        retry.released(70_000);
        assert!(!retry.take_attempt(71_999));
        assert!(retry.take_attempt(72_000));
    }
    #[test]
    fn stop_cancels_active_and_waiting_retries_and_overflow_disables_them() {
        for waiting in [false, true] {
            let mut retry = Retry::default();
            if waiting {
                retry.released(0);
            }
            retry.stop();
            retry.released(1);
            retry.ready(100_000, true);
            assert!(retry.is_stopped());
            assert!(!retry.take_attempt(u64::MAX));
        }
        let mut retry = Retry::default();
        retry.released(u64::MAX - 1);
        assert!(!retry.take_attempt(u64::MAX));
    }
}
