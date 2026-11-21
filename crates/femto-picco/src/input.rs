//! Debounced page-button presses. A held button emits only one event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Turn {
    Previous,
    Next,
}

impl Turn {
    pub const fn opposite(self) -> Self {
        match self {
            Self::Previous => Self::Next,
            Self::Next => Self::Previous,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Press {
    Short(Turn),
    /// Held for at least [`LONG_PRESS_MS`] while long presses were enabled.
    Long(Turn),
}

/// Debounce interval and long-press threshold in milliseconds.
pub const DEBOUNCE_MS: u64 = 30;
pub const LONG_PRESS_MS: u64 = 500;

#[derive(Default)]
pub struct Buttons {
    candidate: u8,
    stable: u8,
    since: u64,
    /// A press that waits for release or the long-press threshold.
    held: Option<(Turn, u64)>,
}

impl Buttons {
    /// Inputs are true when pressed. Timestamps are monotonic milliseconds.
    ///
    /// Without `long_presses` a press reports [`Press::Short`] as soon as it is
    /// stable, so reading has no release latency. With them, a short press is
    /// reported on release and a long press at the threshold; the mode is
    /// fixed when the press begins. Pressing both buttons reports nothing.
    pub fn sample(
        &mut self,
        previous: bool,
        next: bool,
        now: u64,
        long_presses: bool,
    ) -> Option<Press> {
        let value = u8::from(previous) | (u8::from(next) << 1);
        if value != self.candidate || now < self.since {
            self.candidate = value;
            self.since = now;
        }
        if now.saturating_sub(self.since) >= DEBOUNCE_MS && self.stable != value {
            let was_released = self.stable == 0;
            self.stable = value;
            let held = self.held.take();
            let turn = match value {
                1 => Turn::Previous,
                2 => Turn::Next,
                0 => return held.map(|(turn, _)| Press::Short(turn)),
                _ => return None,
            };
            if !was_released {
                return None;
            }
            if !long_presses {
                return Some(Press::Short(turn));
            }
            self.held = Some((turn, now));
        }
        match self.held {
            Some((turn, since)) if now.saturating_sub(since) >= LONG_PRESS_MS => {
                self.held = None;
                Some(Press::Long(turn))
            }
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
