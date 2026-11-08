pub const CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionToken(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResetReason {
    SessionChanged,
    Disconnected,
    Overflow,
    InvalidNotification,
    TokenExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Submit {
    Queued,
    Stale,
    Invalidated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Notification {
    pub token: SessionToken,
    pub reference: [u8; 2],
    bytes: [u8; 64],
    len: usize,
}

impl Notification {
    pub fn payload(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Reset(ResetReason),
    Input(Notification),
}

/// The platform must serialize callback and reader access to this queue.
pub struct Inbox {
    counter: u64,
    active: Option<SessionToken>,
    entries: [Option<Notification>; CAPACITY],
    head: usize,
    len: usize,
    reset: Option<ResetReason>,
}

impl Default for Inbox {
    fn default() -> Self {
        Self {
            counter: 0,
            active: None,
            entries: [None; CAPACITY],
            head: 0,
            len: 0,
            reset: None,
        }
    }
}

impl Inbox {
    /// Check under the platform's serialization before dispatching an action.
    pub fn is_current(&self, token: SessionToken) -> bool {
        self.active == Some(token)
    }

    fn invalidate(&mut self, reason: ResetReason) {
        self.active = None;
        self.entries.fill(None);
        self.head = 0;
        self.len = 0;
        self.reset = Some(reason);
    }

    pub fn begin(&mut self) -> Result<SessionToken, &'static str> {
        let Some(counter) = self.counter.checked_add(1) else {
            self.invalidate(ResetReason::TokenExhausted);
            return Err("session token exhausted");
        };
        self.invalidate(ResetReason::SessionChanged);
        self.counter = counter;
        let token = SessionToken(counter);
        self.active = Some(token);
        Ok(token)
    }

    pub fn disconnect(&mut self, token: SessionToken) {
        if self.active == Some(token) {
            self.invalidate(ResetReason::Disconnected);
        }
    }

    pub fn submit(&mut self, token: SessionToken, reference: [u8; 2], payload: &[u8]) -> Submit {
        if self.active != Some(token) {
            return Submit::Stale;
        }
        if payload.is_empty() || payload.len() > 64 || reference[1] != 1 {
            self.invalidate(ResetReason::InvalidNotification);
            return Submit::Invalidated;
        }
        if self.len == CAPACITY {
            self.invalidate(ResetReason::Overflow);
            return Submit::Invalidated;
        }
        let mut notification = Notification {
            token,
            reference,
            bytes: [0; 64],
            len: payload.len(),
        };
        notification.bytes[..payload.len()].copy_from_slice(payload);
        let tail = (self.head + self.len) % CAPACITY;
        self.entries[tail] = Some(notification);
        self.len += 1;
        Submit::Queued
    }

    pub fn poll(&mut self) -> Option<Event> {
        if let Some(reason) = self.reset.take() {
            return Some(Event::Reset(reason));
        }
        if self.len == 0 {
            return None;
        }
        let notification = self.entries[self.head].take()?;
        self.head = (self.head + 1) % CAPACITY;
        self.len -= 1;
        Some(Event::Input(notification))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_discards_old_input_and_rejects_old_callbacks() {
        let mut queue = Inbox::default();
        let old = queue.begin().unwrap();
        assert_eq!(queue.submit(old, [1, 1], &[10]), Submit::Queued);
        let current = queue.begin().unwrap();
        assert_ne!(old, current);
        assert_eq!(queue.submit(old, [1, 1], &[20]), Submit::Stale);
        queue.disconnect(old);
        assert_eq!(queue.submit(current, [1, 1], &[30]), Submit::Queued);
        assert_eq!(
            queue.poll(),
            Some(Event::Reset(ResetReason::SessionChanged))
        );
        let Some(Event::Input(value)) = queue.poll() else {
            panic!()
        };
        assert_eq!(value.payload(), &[30]);
        assert_eq!(queue.poll(), None);
    }

    #[test]
    fn fifo_wraps_without_losing_or_reordering_reports() {
        let mut queue = Inbox::default();
        let token = queue.begin().unwrap();
        queue.poll();
        for batch in 0..4 {
            for n in 0..CAPACITY {
                assert_eq!(
                    queue.submit(token, [2, 1], &[(batch * CAPACITY + n) as u8]),
                    Submit::Queued
                );
            }
            for n in 0..CAPACITY {
                let Some(Event::Input(value)) = queue.poll() else {
                    panic!()
                };
                assert_eq!(value.reference, [2, 1]);
                assert_eq!(value.payload(), &[(batch * CAPACITY + n) as u8]);
            }
            assert_eq!(queue.poll(), None);
        }
    }

    #[test]
    fn overflow_discards_queue_and_requires_new_session() {
        let mut queue = Inbox::default();
        let token = queue.begin().unwrap();
        for _ in 0..CAPACITY {
            assert_eq!(queue.submit(token, [0, 1], &[1]), Submit::Queued);
        }
        assert_eq!(queue.submit(token, [0, 1], &[0]), Submit::Invalidated);
        assert_eq!(queue.submit(token, [0, 1], &[1]), Submit::Stale);
        assert_eq!(queue.poll(), Some(Event::Reset(ResetReason::Overflow)));
        assert_eq!(queue.poll(), None);
        let next = queue.begin().unwrap();
        assert_eq!(queue.submit(next, [0, 1], &[1]), Submit::Queued);
    }

    #[test]
    fn invalid_notification_and_disconnect_clear_queued_input() {
        let mut queue = Inbox::default();
        for (reference, payload) in [([0, 1], &[][..]), ([0, 1], &[0; 65]), ([0, 2], &[1])] {
            let token = queue.begin().unwrap();
            queue.submit(token, [0, 1], &[1]);
            assert_eq!(queue.submit(token, reference, payload), Submit::Invalidated);
            assert_eq!(
                queue.poll(),
                Some(Event::Reset(ResetReason::InvalidNotification))
            );
            assert_eq!(queue.poll(), None);
        }
        let token = queue.begin().unwrap();
        queue.submit(token, [0, 1], &[0; 64]);
        queue.disconnect(token);
        assert_eq!(queue.poll(), Some(Event::Reset(ResetReason::Disconnected)));
        assert_eq!(queue.poll(), None);
        assert_eq!(queue.submit(token, [0, 1], &[1]), Submit::Stale);
    }

    #[test]
    fn token_exhaustion_never_reuses_a_generation() {
        let mut queue = Inbox {
            counter: u64::MAX - 1,
            ..Inbox::default()
        };
        let token = queue.begin().unwrap();
        queue.submit(token, [0, 1], &[1]);
        assert!(queue.begin().is_err());
        assert_eq!(queue.submit(token, [0, 1], &[1]), Submit::Stale);
        assert_eq!(
            queue.poll(),
            Some(Event::Reset(ResetReason::TokenExhausted))
        );
        assert_eq!(queue.poll(), None);
    }

    #[test]
    fn dequeued_notification_can_be_invalidated_before_dispatch() {
        let mut queue = Inbox::default();
        let token = queue.begin().unwrap();
        queue.poll();
        queue.submit(token, [0, 1], &[1]);
        let Some(Event::Input(value)) = queue.poll() else {
            panic!()
        };
        assert!(queue.is_current(value.token));
        queue.disconnect(token);
        assert!(!queue.is_current(value.token));
        queue.begin().unwrap();
        assert!(!queue.is_current(value.token));
    }
}
