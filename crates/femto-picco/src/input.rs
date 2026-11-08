//! Debounced page-button edges. A held button emits only one event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    Previous,
    Next,
}

#[derive(Default)]
pub struct Buttons {
    candidate: u8,
    stable: u8,
    since: u64,
}

impl Buttons {
    /// Inputs are true when pressed. Timestamps are monotonic milliseconds.
    pub fn sample(&mut self, previous: bool, next: bool, now: u64) -> Option<Turn> {
        let value = u8::from(previous) | (u8::from(next) << 1);
        if value != self.candidate || now < self.since {
            self.candidate = value;
            self.since = now;
        }
        if now.saturating_sub(self.since) < 30 || self.stable == value {
            return None;
        }
        let was_released = self.stable == 0;
        self.stable = value;
        if !was_released {
            return None;
        }
        match value {
            1 => Some(Turn::Previous),
            2 => Some(Turn::Next),
            _ => None,
        }
    }
}

/// Producer-side barrier for input events across display-view changes.
#[derive(Default)]
pub struct ViewGate {
    generation: u64,
    paused: bool,
    buttons_blocked: bool,
}
impl ViewGate {
    /// Each pause or resume is a distinct generation. Reject stale transitions.
    pub fn change(&mut self, generation: u64, paused: bool) -> bool {
        if self.generation.checked_add(1) != Some(generation) {
            return false;
        }
        self.generation = generation;
        self.paused = paused;
        self.buttons_blocked = true;
        true
    }
    pub fn buttons(&mut self, released: bool) -> Option<u64> {
        if released {
            self.buttons_blocked = false;
        }
        (!self.paused && !self.buttons_blocked).then_some(self.generation)
    }
    pub fn touch(&self) -> Option<u64> {
        (!self.paused).then_some(self.generation)
    }
}
