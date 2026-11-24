//! Property tests for the panel driver's readiness contract and plane packing,
//! with a bus that fails at a chosen call.
use femto_picco_board::{
    Bus, Display, Error, HEIGHT, PLANE_BYTES, Panel, Refresh, WIDTH, orientation::Orientation,
};
use proptest::prelude::*;
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct Trace {
    calls: usize,
    fail_at: Option<usize>,
    commands: Vec<(u8, Vec<u8>)>,
}

struct Mock(Rc<RefCell<Trace>>);

impl Mock {
    fn step(&mut self) -> Result<(), ()> {
        let mut trace = self.0.borrow_mut();
        trace.calls += 1;
        if trace.fail_at == Some(trace.calls) {
            Err(())
        } else {
            Ok(())
        }
    }
}

impl Bus for Mock {
    type Error = ();
    fn reset(&mut self) -> Result<(), ()> {
        self.step()
    }
    fn command(&mut self, command: u8, data: &[u8]) -> Result<(), ()> {
        self.step()?;
        self.0.borrow_mut().commands.push((command, data.to_vec()));
        Ok(())
    }
    fn command_stream(&mut self, command: u8, data: impl Iterator<Item = u8>) -> Result<(), ()> {
        self.step()?;
        let data = data.collect();
        self.0.borrow_mut().commands.push((command, data));
        Ok(())
    }
    fn delay_ms(&mut self, _: u32) {}
    fn wait_idle(&mut self, _: u32) -> Result<(), ()> {
        self.step()
    }
}

fn plane(seed: u8) -> Vec<u8> {
    match seed {
        0 => vec![0; PLANE_BYTES],
        1 => vec![255; PLANE_BYTES],
        _ => (0..PLANE_BYTES)
            .map(|i| {
                (i as u32)
                    .wrapping_mul(2654435761)
                    .rotate_left(u32::from(seed)) as u8
                    ^ seed
            })
            .collect(),
    }
}

/// Two bits per pixel, most significant pixel first: (high, low).
fn interleave(high: &[u8], low: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(high.len() * 2);
    for (h, l) in high.iter().zip(low) {
        let mut word = 0u16;
        for bit in (0..8).rev() {
            word = (word << 2) | u16::from((h >> bit) & 1) << 1 | u16::from((l >> bit) & 1);
        }
        out.extend([(word >> 8) as u8, word as u8]);
    }
    out
}

#[derive(Clone, Debug)]
enum Op {
    Initialize,
    Refresh(u8, bool),
    Grayscale(u8, u8),
    Base(u8, u8, bool),
    Overlay(u8, u8),
    Sleep,
    WrongSize,
}

fn op() -> impl Strategy<Value = Op> {
    let seed = || 0u8..6;
    prop_oneof![
        2 => Just(Op::Initialize),
        5 => (seed(), any::<bool>()).prop_map(|(a, f)| Op::Refresh(a, f)),
        2 => (seed(), seed()).prop_map(|(a, b)| Op::Grayscale(a, b)),
        3 => (seed(), seed(), any::<bool>()).prop_map(|(a, b, f)| Op::Base(a, b, f)),
        2 => (seed(), seed()).prop_map(|(a, b)| Op::Overlay(a, b)),
        1 => Just(Op::Sleep),
        1 => Just(Op::WrongSize),
    ]
}

fn stream(commands: &[(u8, Vec<u8>)], command: u8) -> Vec<&Vec<u8>> {
    commands
        .iter()
        .filter(|(c, _)| *c == command)
        .map(|(_, data)| data)
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(150))]

    #[test]
    fn readiness_and_differential_state_follow_the_contract(
        panel in prop::sample::select(vec![Panel::Yrd0426, Panel::Depg0397, Panel::Opm040b3, Panel::Se0400nqw47]),
        ops in prop::collection::vec((op(), prop::option::weighted(0.25, 1usize..14)), 1..14),
    ) {
        let ssd2677 = panel == Panel::Se0400nqw47;
        let shared = Rc::new(RefCell::new(Trace::default()));
        let mut display = Display::new(Mock(shared.clone()), panel).unwrap();
        // The model: what the documentation promises a caller.
        let mut ready = false;
        let mut needs_full = true;
        // The black-and-white image the SSD2677 driver last put on the panel.
        let mut shown = vec![255u8; PLANE_BYTES];
        let mode = |fast: bool| if fast { Refresh::Fast } else { Refresh::Full };
        for (index, (op, fail_at)) in ops.iter().enumerate() {
            {
                let mut bus = shared.borrow_mut();
                bus.calls = 0;
                bus.fail_at = *fail_at;
                bus.commands.clear();
            }
            let context = format!("op {index} {op:?} fail_at {fail_at:?} of {ops:?}");
            match op {
                Op::Initialize => {
                    let result = display.initialize();
                    let bus = shared.borrow();
                    let failed = bus.fail_at.is_some_and(|at| at <= bus.calls);
                    prop_assert_eq!(result.is_err(), failed, "{}", &context);
                    ready = result.is_ok();
                    needs_full = true;
                    if result.is_ok() {
                        shown.fill(255);
                    }
                }
                Op::WrongSize => {
                    let short = vec![0; PLANE_BYTES - 1];
                    let full = plane(1);
                    prop_assert_eq!(display.refresh(&short, Refresh::Fast), Err(Error::FrameSize));
                    prop_assert_eq!(display.grayscale(&short, &full), Err(Error::FrameSize));
                    prop_assert_eq!(display.grayscale(&full, &short), Err(Error::FrameSize));
                    prop_assert_eq!(display.antialiased_base(&full, &short, Refresh::Fast).err(), Some(Error::FrameSize));
                    if ssd2677 {
                        prop_assert_eq!(display.overlay(&short, &full), Err(Error::FrameSize));
                    }
                    prop_assert_eq!(shared.borrow().calls, 0, "{}", &context);
                }
                Op::Refresh(seed, fast) => {
                    let new = plane(*seed);
                    let result = display.refresh(&new, mode(*fast));
                    let bus = shared.borrow();
                    if !ready {
                        prop_assert_eq!(result, Err(Error::NotReady), "{}", &context);
                        prop_assert_eq!(bus.calls, 0);
                        continue;
                    }
                    let want = if needs_full { Refresh::Full } else { mode(*fast) };
                    match result {
                        Ok(executed) => {
                            prop_assert_eq!(executed, want, "{}", &context);
                            if ssd2677 {
                                let streams = stream(&bus.commands, 0x10);
                                prop_assert_eq!(streams.len(), 1);
                                prop_assert!(*streams[0] == interleave(&shown, &new), "old/new planes, {}", &context);
                            } else {
                                // The new image is loaded before the update and
                                // left in both memories afterwards.
                                let current = stream(&bus.commands, 0x24);
                                let old = stream(&bus.commands, 0x26);
                                prop_assert!(current.len() == 2 && current.iter().all(|data| **data == new));
                                prop_assert_eq!(old.len(), if want == Refresh::Full { 2 } else { 1 });
                                prop_assert!(old.iter().all(|data| **data == new));
                            }
                            shown = new;
                            needs_full = false;
                        }
                        Err(error) => {
                            prop_assert_eq!(error, Error::Bus(()), "{}", &context);
                            prop_assert!(bus.fail_at.is_some());
                            ready = false;
                            needs_full = true;
                        }
                    }
                }
                Op::Grayscale(a, b) => {
                    let (msb, lsb) = (plane(*a), plane(*b));
                    let result = display.grayscale(&msb, &lsb);
                    let bus = shared.borrow();
                    if !ready {
                        prop_assert_eq!(result, Err(Error::NotReady), "{}", &context);
                        prop_assert_eq!(bus.calls, 0);
                        continue;
                    }
                    needs_full = true;
                    match result {
                        Ok(()) => {
                            if ssd2677 {
                                let streams = stream(&bus.commands, 0x10);
                                prop_assert!(streams.len() == 1 && *streams[0] == interleave(&msb, &lsb), "{}", &context);
                                shown = msb;
                            } else {
                                let inverted = |plane: &[u8]| plane.iter().map(|b| !b).collect::<Vec<_>>();
                                prop_assert!(stream(&bus.commands, 0x24) == [&inverted(&lsb)]);
                                prop_assert!(stream(&bus.commands, 0x26) == [&inverted(&msb)]);
                            }
                        }
                        Err(error) => {
                            prop_assert_eq!(error, Error::Bus(()));
                            ready = false;
                        }
                    }
                }
                Op::Base(a, b, fast) => {
                    let (msb, lsb) = (plane(*a), plane(*b));
                    let result = display.antialiased_base(&msb, &lsb, mode(*fast));
                    let bus = shared.borrow();
                    if !ready {
                        prop_assert_eq!(result, Err(Error::NotReady), "{}", &context);
                        prop_assert_eq!(bus.calls, 0);
                        continue;
                    }
                    let want = if !ssd2677 || needs_full { Refresh::Full } else { mode(*fast) };
                    match result {
                        Ok(executed) => {
                            prop_assert_eq!(executed, want, "{}", &context);
                            if ssd2677 {
                                // Only white stays white; both grays are ink.
                                let base: Vec<u8> = msb.iter().zip(&lsb).map(|(h, l)| h & l).collect();
                                let old: Vec<u8> = if want == Refresh::Fast {
                                    shown.iter().zip(&base).map(|(old, new)| old | !new).collect()
                                } else {
                                    shown.clone()
                                };
                                let streams = stream(&bus.commands, 0x10);
                                prop_assert!(streams.len() == 1 && *streams[0] == interleave(&old, &base), "{}", &context);
                                shown = base;
                                needs_full = false;
                            } else {
                                needs_full = true;
                            }
                        }
                        Err(error) => {
                            prop_assert_eq!(error, Error::Bus(()));
                            ready = false;
                            needs_full = true;
                        }
                    }
                }
                Op::Overlay(a, b) => {
                    let (msb, lsb) = (plane(*a), plane(*b));
                    let result = display.overlay(&msb, &lsb);
                    let bus = shared.borrow();
                    if !ssd2677 {
                        prop_assert_eq!(result, Ok(()));
                        prop_assert_eq!(bus.calls, 0);
                        continue;
                    }
                    if !ready {
                        prop_assert_eq!(result, Err(Error::NotReady), "{}", &context);
                        prop_assert_eq!(bus.calls, 0);
                        continue;
                    }
                    match result {
                        Ok(()) => {
                            let gray: Vec<u8> = msb.iter().zip(&lsb).map(|(h, l)| h ^ l).collect();
                            let dark: Vec<u8> = msb.iter().zip(&lsb).map(|(h, l)| !h & l).collect();
                            let streams = stream(&bus.commands, 0x10);
                            prop_assert!(streams.len() == 1 && *streams[0] == interleave(&gray, &dark), "{}", &context);
                        }
                        Err(error) => {
                            prop_assert_eq!(error, Error::Bus(()));
                            ready = false;
                            needs_full = true;
                        }
                    }
                }
                Op::Sleep => {
                    let result = display.sleep();
                    if !ready {
                        prop_assert_eq!(result, Err(Error::NotReady), "{}", &context);
                        prop_assert_eq!(shared.borrow().calls, 0);
                        continue;
                    }
                    ready = false;
                    needs_full = true;
                }
            }
        }
    }

    /// A view pixel lands on exactly one native bit, and rotating the picture
    /// by a half turn is the same as turning the device by a half turn.
    #[test]
    fn packing_preserves_pixels_and_half_turns(
        landscape in any::<bool>(),
        points in prop::collection::vec((0usize..WIDTH * HEIGHT, 1u8..4), 1..40),
    ) {
        let (upright, turned) = if landscape {
            (Orientation::Clockwise, Orientation::Counterclockwise)
        } else {
            (Orientation::Portrait, Orientation::Inverted)
        };
        let mut pixels = vec![0u8; WIDTH * HEIGHT];
        let mut rotated = vec![0u8; WIDTH * HEIGHT];
        for &(index, level) in &points {
            pixels[index] = level * 85;
            rotated[WIDTH * HEIGHT - 1 - index] = level * 85;
        }
        let planes = |orientation: Orientation, pixels: &[u8]| {
            let (mut msb, mut lsb) = (vec![0xaa; PLANE_BYTES], vec![0x55; PLANE_BYTES]);
            orientation.grayscale(pixels, &mut msb, &mut lsb).unwrap();
            (msb, lsb)
        };
        let (msb, lsb) = planes(upright, &pixels);
        prop_assert!(planes(turned, &rotated) == (msb.clone(), lsb.clone()));
        let ones = |plane: &[u8]| plane.iter().map(|b| b.count_ones() as usize).sum::<usize>();
        prop_assert_eq!(ones(&msb), pixels.iter().filter(|p| **p >= 170).count());
        prop_assert_eq!(ones(&lsb), pixels.iter().filter(|p| matches!(**p, 85 | 255)).count());
        // Each lit pixel is found at the bit its portrait position names:
        // native rows run along the portrait y axis, last portrait column first.
        let (width, _) = upright.dimensions();
        for &(index, _) in &points {
            let (px, py) = upright.to_portrait(index % width, index / width).unwrap();
            let native = (HEIGHT - 1 - px) * WIDTH + py;
            let bit = |plane: &[u8]| plane[native / 8] >> (7 - native % 8) & 1;
            prop_assert_eq!(bit(&msb) << 1 | bit(&lsb), pixels[index] / 85);
        }
        // Monochrome agrees with the high plane for pure black and white.
        let binary: Vec<u8> = pixels.iter().map(|p| if *p >= 170 { 255 } else { 0 }).collect();
        let mut mono = vec![0x33; PLANE_BYTES];
        upright.monochrome(&binary, &mut mono).unwrap();
        prop_assert!(mono == msb);
        // Rejected input leaves the output untouched.
        let mut untouched = vec![0x33; PLANE_BYTES];
        prop_assert!(upright.monochrome(&pixels, &mut untouched).is_err() || pixels.iter().all(|p| matches!(p, 0 | 255)));
        if pixels.iter().any(|p| !matches!(p, 0 | 255)) {
            prop_assert!(untouched.iter().all(|b| *b == 0x33));
        }
        prop_assert!(upright.monochrome(&binary[1..], &mut untouched).is_err());
    }
}
