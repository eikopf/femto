//! Bounded TMA525C PIP reports and single-finger gestures in view coordinates.
use crate::{input::Turn, orientation::Orientation};

pub const MAX_REPORT: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    Tap { x: u16, y: u16 },
    Swipe(Turn),
}

#[derive(Clone, Copy)]
struct Contact {
    id: u8,
    x: u16,
    y: u16,
    started: u64,
    updated: u64,
    moved: bool,
}

#[derive(Default)]
pub struct Touch {
    orientation: Orientation,
    contact: Option<Contact>,
    blocked: bool,
    last: Option<(u16, u16)>,
}

impl Touch {
    /// A changed orientation discards the active gesture until all contacts lift.
    pub fn set_orientation(&mut self, orientation: Orientation) {
        if orientation != self.orientation {
            self.cancel();
            self.orientation = orientation;
        }
    }

    /// An input gap cancels the gesture; it must never manufacture a tap.
    pub fn expire(&mut self, now: u64) {
        if self
            .contact
            .is_some_and(|c| now < c.updated || now - c.updated >= 3000)
        {
            self.cancel();
        }
    }

    pub fn cancel(&mut self) {
        self.contact = None;
        self.last = None;
        self.blocked = true;
    }

    pub fn report(&mut self, bytes: &[u8], now: u64) -> Option<Gesture> {
        self.expire(now);
        if bytes.len() < 3
            || bytes.len() > MAX_REPORT
            || usize::from(u16::from_le_bytes([bytes[0], bytes[1]])) != bytes.len()
        {
            self.cancel();
            return None;
        }
        if bytes[2] != 1 {
            return None;
        }
        if bytes.len() < 7 {
            self.cancel();
            return None;
        }
        let count = usize::from(bytes[5] & 31);
        if count > 12 || bytes.len() < 7 + count * 10 {
            self.cancel();
            return None;
        }
        let mut live = None;
        let mut ids = 0_u32;
        for record in bytes[7..7 + count * 10].as_chunks::<10>().0 {
            let id = record[1] & 31;
            if ids & (1 << id) != 0 {
                self.cancel();
                return None;
            }
            ids |= 1 << id;
            if (record[1] >> 5) & 3 == 3 {
                continue;
            }
            let x = u16::from_le_bytes([record[2], record[3]]);
            let y = u16::from_le_bytes([record[4], record[5]]);
            if x > 480 || y > 800 || live.is_some() {
                self.cancel();
                return None;
            }
            let (x, y) = self
                .orientation
                .from_portrait(
                    usize::from((480 - x).min(479)),
                    usize::from((800 - y).min(799)),
                )
                .expect("bounded calibrated touch coordinates");
            live = Some((id, x as u16, y as u16));
        }
        let Some((id, x, y)) = live else {
            let contact = self.contact.take();
            let last = self.last.take();
            let blocked = core::mem::replace(&mut self.blocked, false);
            if blocked {
                return None;
            }
            let (c, (x, y)) = (contact?, last?);
            let duration = now.saturating_sub(c.started);
            if !(40..=1000).contains(&duration) {
                return None;
            }
            let dx = i32::from(x) - i32::from(c.x);
            let dy = i32::from(y) - i32::from(c.y);
            return if dx.abs() >= 80 && dy.abs() <= 60 {
                Some(Gesture::Swipe(if dx < 0 {
                    Turn::Next
                } else {
                    Turn::Previous
                }))
            } else if !c.moved {
                Some(Gesture::Tap { x: c.x, y: c.y })
            } else {
                None
            };
        };
        if self.blocked {
            return None;
        }
        if let Some(c) = &mut self.contact {
            if c.id != id {
                self.cancel();
                return None;
            }
            c.moved |= c.x.abs_diff(x) > 20 || c.y.abs_diff(y) > 20;
            c.updated = now;
        } else {
            self.contact = Some(Contact {
                id,
                x,
                y,
                started: now,
                updated: now,
                moved: false,
            });
        }
        self.last = Some((x, y));
        None
    }
}
