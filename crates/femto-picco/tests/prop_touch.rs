//! Property tests for touch reports: generated finger tracks against a model
//! written from docs/picco-firmware.md, and arbitrary bytes.
use femto_picco_board::{
    HEIGHT, WIDTH,
    input::Turn,
    orientation::Orientation,
    touch::{Gesture, MAX_REPORT, Touch},
};
use proptest::prelude::*;

const ORIENTATIONS: [Orientation; 4] = [
    Orientation::Portrait,
    Orientation::Clockwise,
    Orientation::Inverted,
    Orientation::Counterclockwise,
];

/// (id, event, raw x, raw y); event 3 is a lift record.
type Contact = (u8, u8, u16, u16);

fn report(contacts: &[Contact]) -> Vec<u8> {
    let mut bytes = vec![0; 7 + contacts.len() * 10];
    let len = bytes.len() as u16;
    bytes[..2].copy_from_slice(&len.to_le_bytes());
    bytes[2] = 1;
    bytes[5] = contacts.len() as u8;
    for (record, &(id, event, x, y)) in bytes[7..].chunks_exact_mut(10).zip(contacts) {
        record[1] = id | (event << 5);
        record[2..4].copy_from_slice(&x.to_le_bytes());
        record[4..6].copy_from_slice(&y.to_le_bytes());
    }
    bytes
}

#[derive(Clone, Debug)]
enum Op {
    /// The firmware's poll without a pending report: expire, then held.
    Poll(u64),
    Report(u64, Vec<Contact>),
}

#[derive(Clone, Copy)]
struct Track {
    id: u8,
    start: (i32, i32),
    last: (i32, i32),
    started: u64,
    updated: u64,
    moved: bool,
}

#[derive(Default)]
struct Model {
    track: Option<Track>,
    blocked: bool,
}

impl Model {
    fn cancel(&mut self) {
        self.track = None;
        self.blocked = true;
    }
    fn expire(&mut self, now: u64) {
        if self.track.is_some_and(|t| now - t.updated >= 3000) {
            self.cancel();
        }
    }
    fn poll(&mut self, now: u64) -> Option<Gesture> {
        self.expire(now);
        let track = self.track.filter(|t| !t.moved && now - t.started >= 600)?;
        self.cancel();
        Some(Gesture::Hold {
            x: track.start.0 as u16,
            y: track.start.1 as u16,
        })
    }
    fn report(
        &mut self,
        orientation: Orientation,
        contacts: &[Contact],
        now: u64,
    ) -> Option<Gesture> {
        self.expire(now);
        let live: Vec<_> = contacts.iter().filter(|c| c.1 != 3).collect();
        if live.len() > 1 {
            self.cancel();
            return None;
        }
        let Some(&&(id, _, x, y)) = live.first() else {
            let track = self.track.take();
            if core::mem::take(&mut self.blocked) {
                return None;
            }
            let track = track?;
            let duration = now - track.started;
            if !(40..=1000).contains(&duration) {
                return None;
            }
            let dx = track.last.0 - track.start.0;
            let dy = track.last.1 - track.start.1;
            return if dx.abs() >= 80 && dy.abs() <= 60 {
                Some(Gesture::Swipe(if dx < 0 {
                    Turn::Next
                } else {
                    Turn::Previous
                }))
            } else if dy.abs() >= 80 && dx.abs() <= 60 {
                Some(Gesture::Slide { up: dy < 0 })
            } else if !track.moved {
                Some(Gesture::Tap {
                    x: track.start.0 as u16,
                    y: track.start.1 as u16,
                })
            } else {
                None
            };
        };
        if self.blocked {
            return None;
        }
        let (x, y) = orientation
            .from_portrait(
                usize::from((480 - x).min(479)),
                usize::from((800 - y).min(799)),
            )
            .unwrap();
        let point = (x as i32, y as i32);
        match &mut self.track {
            Some(track) if track.id != id => self.cancel(),
            Some(track) => {
                track.moved |=
                    (track.start.0 - point.0).abs() > 20 || (track.start.1 - point.1).abs() > 20;
                track.last = point;
                track.updated = now;
            }
            None => {
                self.track = Some(Track {
                    id,
                    start: point,
                    last: point,
                    started: now,
                    updated: now,
                    moved: false,
                })
            }
        }
        None
    }
}

fn ops() -> impl Strategy<Value = Vec<Op>> {
    let dt = prop_oneof![
        12 => 1u64..120,
        4 => 120u64..700,
        1 => 2900u64..3100,
        1 => Just(2999u64),
        1 => Just(3000u64),
        1 => prop::sample::select(vec![39u64, 40, 599, 600, 1000, 1001]),
    ];
    // A finger wandering near an anchor, with occasional long strokes.
    let point = (0u16..=480, 0u16..=800);
    let gesture = (
        0u8..3,
        point,
        prop::collection::vec(
            (
                dt.clone(),
                prop_oneof![3 => -25i32..=25, 1 => -200i32..=200],
                prop_oneof![3 => -25i32..=25, 1 => -200i32..=200],
                0u8..12,
            ),
            1..8,
        ),
        dt,
        0u8..8,
    )
        .prop_map(|(id, (x, y), moves, lift_dt, lift_kind)| {
            let mut ops = Vec::new();
            let (mut x, mut y) = (i32::from(x), i32::from(y));
            for (dt, dx, dy, kind) in moves {
                x = (x + dx).clamp(0, 480);
                y = (y + dy).clamp(0, 800);
                let contact = (id, 1, x as u16, y as u16);
                ops.push(match kind {
                    0 => Op::Poll(dt),
                    1 => Op::Report(dt, vec![contact, (id + 1, 1, 5, 5)]),
                    2 => Op::Report(dt, vec![(id + 1, 1, x as u16, y as u16)]),
                    _ => Op::Report(dt, vec![contact]),
                });
            }
            ops.push(match lift_kind {
                0 => Op::Report(lift_dt, vec![(id, 3, x as u16, y as u16)]),
                1 => Op::Poll(lift_dt),
                _ => Op::Report(lift_dt, vec![]),
            });
            ops
        });
    prop::collection::vec(gesture, 1..6).prop_map(|g| g.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn gestures_match_the_documented_model(
        orientation in prop::sample::select(&ORIENTATIONS[..]),
        start in prop_oneof![Just(0u64), 0u64..1_000_000, Just(u64::MAX - 10_000_000)],
        ops in ops(),
    ) {
        let mut touch = Touch::default();
        touch.set_orientation(orientation);
        // An orientation change blocks until the first empty report.
        let mut model = Model { track: None, blocked: orientation != Orientation::Portrait };
        let mut now = start;
        for (index, op) in ops.iter().enumerate() {
            let (got, want) = match op {
                Op::Poll(dt) => {
                    now += dt;
                    touch.expire(now);
                    (touch.held(now), model.poll(now))
                }
                Op::Report(dt, contacts) => {
                    now += dt;
                    (touch.report(&report(contacts), now), model.report(orientation, contacts, now))
                }
            };
            prop_assert_eq!(got, want, "op {} of {:?}", index, ops);
            if let Some(Gesture::Tap { x, y } | Gesture::Hold { x, y }) = got {
                let (width, height) = orientation.dimensions();
                prop_assert!(usize::from(x) < width && usize::from(y) < height);
            }
        }
    }

    #[test]
    fn arbitrary_reports_never_panic_and_yield_bounded_points(
        orientation in prop::sample::select(&ORIENTATIONS[..]),
        reports in prop::collection::vec(
            (
                prop::collection::vec(any::<u8>(), 0..140),
                prop::bool::ANY,
                prop::bool::ANY,
                0u64..5000,
            ),
            1..12,
        ),
    ) {
        let mut touch = Touch::default();
        touch.set_orientation(orientation);
        let mut now = 0;
        for (mut bytes, fix_length, fix_kind, dt) in reports {
            now += dt;
            if fix_length && bytes.len() >= 2 {
                let len = (bytes.len() as u16).to_le_bytes();
                bytes[..2].copy_from_slice(&len);
            }
            if fix_kind && bytes.len() >= 6 {
                bytes[2] = 1;
                bytes[5] = ((bytes.len() - 7.min(bytes.len())) / 10) as u8;
            }
            prop_assert!(bytes.len() <= MAX_REPORT + 12);
            for gesture in [touch.report(&bytes, now), touch.held(now)].into_iter().flatten() {
                if let Gesture::Tap { x, y } | Gesture::Hold { x, y } = gesture {
                    let (width, height) = orientation.dimensions();
                    prop_assert!(usize::from(x) < width && usize::from(y) < height);
                }
            }
        }
    }

    #[test]
    fn orientation_transforms_invert_each_other(
        orientation in prop::sample::select(&ORIENTATIONS[..]),
        x in 0usize..820,
        y in 0usize..820,
    ) {
        let (width, height) = orientation.dimensions();
        prop_assert_eq!(width * height, WIDTH * HEIGHT);
        match orientation.to_portrait(x, y) {
            Some((px, py)) => {
                prop_assert!(x < width && y < height && px < HEIGHT && py < WIDTH);
                prop_assert_eq!(orientation.from_portrait(px, py), Some((x, y)));
            }
            None => prop_assert!(x >= width || y >= height),
        }
        match orientation.from_portrait(x, y) {
            Some((vx, vy)) => {
                prop_assert!(x < HEIGHT && y < WIDTH && vx < width && vy < height);
                prop_assert_eq!(orientation.to_portrait(vx, vy), Some((x, y)));
            }
            None => prop_assert!(x >= HEIGHT || y >= WIDTH),
        }
    }
}

/// Turning the device a quarter turn clockwise twice is the inverted view, and
/// a clockwise view followed by a counterclockwise one is the portrait view.
#[test]
fn quarter_turns_compose() {
    use Orientation::*;
    for px in [0, 1, 239, 478, 479] {
        for py in [0, 1, 400, 798, 799] {
            let (cx, cy) = Clockwise.from_portrait(px, py).unwrap();
            let (ix, iy) = Inverted.from_portrait(px, py).unwrap();
            let (ccx, ccy) = Counterclockwise.from_portrait(px, py).unwrap();
            // Rotating the landscape views by a half turn maps one to the other.
            assert_eq!((WIDTH - 1 - cx, HEIGHT - 1 - cy), (ccx, ccy));
            assert_eq!((HEIGHT - 1 - px, WIDTH - 1 - py), (ix, iy));
        }
    }
    // The portrait top-left corner is a different corner of each landscape view.
    assert_eq!(Clockwise.from_portrait(0, 0), Some((WIDTH - 1, 0)));
    assert_eq!(Counterclockwise.from_portrait(0, 0), Some((0, HEIGHT - 1)));
}
