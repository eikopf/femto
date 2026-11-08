use femto_picco_board::{
    Bus, Display, Error, PLANE_BYTES, Panel, Refresh, identify, portrait_plane,
};
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct Trace {
    commands: Vec<(u8, Vec<u8>)>,
    waits: usize,
    fail_at: Option<usize>,
    operations: usize,
}
struct Mock(Rc<RefCell<Trace>>);
impl Mock {
    fn operation(&mut self) -> Result<(), ()> {
        let mut trace = self.0.borrow_mut();
        trace.operations += 1;
        if trace.fail_at == Some(trace.operations) {
            Err(())
        } else {
            Ok(())
        }
    }
}
impl Bus for Mock {
    type Error = ();
    fn reset(&mut self) -> Result<(), ()> {
        self.operation()
    }
    fn command(&mut self, cmd: u8, data: &[u8]) -> Result<(), ()> {
        self.operation()?;
        self.0.borrow_mut().commands.push((cmd, data.to_vec()));
        Ok(())
    }
    fn delay_ms(&mut self, _: u32) {}
    fn command_stream(&mut self, command: u8, data: impl Iterator<Item = u8>) -> Result<(), ()> {
        self.command(command, &data.collect::<Vec<_>>())
    }
    fn wait_idle(&mut self, timeout: u32) -> Result<(), ()> {
        assert!((1..=10_000).contains(&timeout));
        self.operation()?;
        self.0.borrow_mut().waits += 1;
        Ok(())
    }
}

#[test]
fn panel_ids_select_distinct_controllers_and_reject_disconnected_bus() {
    assert_eq!(
        identify([0xca, 0xfe, 0, 0x16, 0x77, 2, 0x10, 1, 0, 0x91], [0; 3]),
        Some(Panel::Yrd0426)
    );
    assert_eq!(
        identify([0, 0xaf, 0, 1, 0xaa, 9, 0x12, 0x20, 0x69, 0], [0; 3]),
        Some(Panel::Yrd0426)
    );
    assert_eq!(
        identify([0xca, 0xfe, 0, 0x16, 0x77, 0, 0, 0, 0, 0], [0; 3]),
        Some(Panel::Depg0397)
    );
    assert_eq!(identify(*b"W040B3\0\0\0\0", [0; 3]), Some(Panel::Opm040b3));
    assert_eq!(identify([255; 10], [7, 1, 1]), Some(Panel::Se0400nqw47));
    assert_eq!(identify([255; 10], [255; 3]), None);
    assert_eq!(identify([0; 10], [0; 3]), None);
    assert!(Display::new(Mock(Default::default()), Panel::Se0400nqw47).is_ok());
}

#[test]
fn portrait_corners_and_byte_boundaries_preserve_native_pixels() {
    let mut pixels = vec![255; 480 * 800];
    // (portrait x,y) maps to native (y,479-x).
    for (x, y) in [(0, 0), (479, 0), (0, 799), (479, 799), (478, 7), (478, 8)] {
        pixels[y * 480 + x] = 0;
    }
    let mut plane = vec![0; PLANE_BYTES];
    portrait_plane(&pixels, &mut plane).unwrap();
    for (offset, value) in [
        (0, 0x7f),
        (99, 0xfe),
        (47900, 0x7f),
        (47999, 0xfe),
        (100, 0xfe),
        (101, 0x7f),
    ] {
        assert_eq!(plane[offset], value);
    }
    assert_eq!(plane.iter().map(|v| v.count_zeros()).sum::<u32>(), 6);
    let before = plane.clone();
    pixels[1] = 127;
    assert_eq!(
        portrait_plane(&pixels, &mut plane),
        Err(Error::FrameEncoding)
    );
    assert_eq!(plane, before);
    assert_eq!(
        portrait_plane(&pixels[..20], &mut plane),
        Err(Error::FrameSize)
    );
}

#[test]
fn full_refresh_uses_panel_waveform_and_resynchronizes_both_planes() {
    for panel in [Panel::Yrd0426, Panel::Opm040b3, Panel::Depg0397] {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut display = Display::new(Mock(trace.clone()), panel).unwrap();
        let frame = vec![0xa5; PLANE_BYTES];
        assert_eq!(display.full_refresh(&frame), Err(Error::NotReady));
        assert!(trace.borrow().commands.is_empty());
        display.initialize().unwrap();
        display.full_refresh(&frame).unwrap();
        let t = trace.borrow();
        assert!(t.commands.contains(&(0x44, vec![0, 0, 0x1f, 3])));
        assert!(t.commands.contains(&(0x45, vec![0xdf, 1, 0, 0])));
        let activation = t.commands.iter().position(|(c, _)| *c == 0x20).unwrap();
        assert_eq!(
            t.commands[activation - 1],
            (
                0x22,
                vec![if panel == Panel::Depg0397 { 0xe7 } else { 0xf7 }]
            )
        );
        for cmd in [0x24, 0x26] {
            assert_eq!(
                t.commands
                    .iter()
                    .filter(|(c, d)| *c == cmd && *d == frame)
                    .count(),
                2
            );
            assert!(t.commands[activation + 1..].contains(&(cmd, frame.clone())));
        }
        assert_eq!(t.waits, 4);
        if panel == Panel::Depg0397 {
            assert_eq!(
                t.commands.iter().find(|(c, _)| *c == 0x32).unwrap().1.len(),
                105
            );
            assert!(t.commands.contains(&(0x04, vec![0x41, 0xba, 0x32])));
            assert!(t.commands.contains(&(0x2c, vec![0x50])));
        } else {
            assert!(!t.commands.iter().any(|(c, _)| *c == 0x32));
        }
    }
}

#[test]
fn every_bus_failure_requires_reset_before_more_refresh_commands() {
    for panel in [Panel::Depg0397, Panel::Se0400nqw47] {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut display = Display::new(Mock(trace.clone()), panel).unwrap();
        let frame = vec![255; PLANE_BYTES];
        display.initialize().unwrap();
        display.full_refresh(&frame).unwrap();
        let count = trace.borrow().operations;
        for fail_at in 1..=count {
            let trace = Rc::new(RefCell::new(Trace {
                fail_at: Some(fail_at),
                ..Default::default()
            }));
            let mut display = Display::new(Mock(trace.clone()), panel).unwrap();
            let result = display
                .initialize()
                .and_then(|()| display.full_refresh(&frame));
            assert_eq!(result, Err(Error::Bus(())), "operation {fail_at}");
            let stopped = trace.borrow().operations;
            assert_eq!(display.full_refresh(&frame), Err(Error::NotReady));
            assert_eq!(trace.borrow().operations, stopped);
            trace.borrow_mut().fail_at = None;
            display.initialize().unwrap();
            display.full_refresh(&frame).unwrap();
        }
    }
}

#[test]
fn ssd2677_stream_retains_previous_frame_and_cycles_power_after_refresh() {
    let trace = Rc::new(RefCell::new(Trace::default()));
    let mut display = Display::new(Mock(trace.clone()), Panel::Se0400nqw47).unwrap();
    display.initialize().unwrap();
    assert_eq!(trace.borrow().commands.first(), Some(&(0, vec![0x27, 9])));
    assert!(
        trace
            .borrow()
            .commands
            .contains(&(0x83, vec![0, 0, 3, 0x1f, 0, 0xc8, 2, 0xa8, 1]))
    );
    let first: Vec<_> = (0..PLANE_BYTES).map(|i| i as u8).collect();
    let second: Vec<_> = first.iter().map(|p| !p).collect();
    let white = vec![255; PLANE_BYTES];
    for (old, new) in [(&white, &first), (&first, &second)] {
        trace.borrow_mut().commands.clear();
        display.full_refresh(new).unwrap();
        let t = trace.borrow();
        assert_eq!(
            t.commands.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
            [0x10, 0x20, 0x04, 0x12, 0x02]
        );
        let packed = &t.commands[0].1;
        assert_eq!(packed.len(), 96_000);
        // Decode wire pixels independently of the driver's packing operation.
        for pixel in 0..384_000 {
            let pair = (packed[pixel / 4] >> (6 - 2 * (pixel % 4))) & 3;
            assert_eq!(pair >> 1, (old[pixel / 8] >> (7 - pixel % 8)) & 1);
            assert_eq!(pair & 1, (new[pixel / 8] >> (7 - pixel % 8)) & 1);
        }
        assert_eq!(t.commands[1].1.len(), 535);
        assert_eq!(&t.commands[1].1[..6], &[0, 0x78, 0x78, 0x78, 0x78, 0x10]);
        assert_eq!(t.commands[3].1, [0]);
        assert_eq!(t.commands[4].1, [0]);
    }
    // Reset invalidates the retained image and restores the white startup baseline.
    display.initialize().unwrap();
    trace.borrow_mut().commands.clear();
    display.full_refresh(&vec![0; PLANE_BYTES]).unwrap();
    assert!(trace.borrow().commands[0].1.iter().all(|b| *b == 0xaa));
}

#[test]
fn fast_updates_preserve_the_baseline_until_activation_and_promote_after_reset() {
    for panel in [
        Panel::Yrd0426,
        Panel::Depg0397,
        Panel::Opm040b3,
        Panel::Se0400nqw47,
    ] {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut display = Display::new(Mock(trace.clone()), panel).unwrap();
        display.initialize().unwrap();
        let first = vec![0; PLANE_BYTES];
        let next = vec![255; PLANE_BYTES];
        assert_eq!(
            display.refresh(&first, Refresh::Fast).unwrap(),
            Refresh::Full
        );
        let full_lut = trace
            .borrow()
            .commands
            .iter()
            .find(|(c, _)| {
                *c == if panel == Panel::Se0400nqw47 {
                    0x20
                } else {
                    0x32
                }
            })
            .map(|(_, v)| v.clone());
        trace.borrow_mut().commands.clear();
        assert_eq!(
            display.refresh(&next[..10], Refresh::Fast),
            Err(Error::FrameSize)
        );
        assert!(trace.borrow().commands.is_empty());
        assert_eq!(
            display.refresh(&next, Refresh::Fast).unwrap(),
            Refresh::Fast
        );
        {
            let t = trace.borrow();
            if panel == Panel::Se0400nqw47 {
                assert!(t.commands[0].1.iter().all(|b| *b == 0x55));
                assert_eq!(t.commands[1].0, 0x20);
                assert_eq!(t.commands[1].1.len(), 535);
                assert_ne!(Some(&t.commands[1].1), full_lut.as_ref());
            } else {
                let activation = t.commands.iter().position(|(c, _)| *c == 0x20).unwrap();
                assert!(t.commands[..activation].contains(&(0x24, next.clone())));
                assert!(!t.commands[..activation].iter().any(|(c, _)| *c == 0x26));
                assert!(t.commands[..activation].contains(&(0x21, vec![0])));
                assert_eq!(t.commands[activation - 1], (0x22, vec![0xcf]));
                assert!(t.commands[activation + 1..].contains(&(0x26, next.clone())));
                assert_eq!(
                    t.commands.iter().find(|(c, _)| *c == 0x32).unwrap().1.len(),
                    105
                );
            }
        }
        display.initialize().unwrap();
        assert_eq!(
            display.refresh(&next, Refresh::Fast).unwrap(),
            Refresh::Full
        );
    }
}

#[test]
fn failed_fast_refresh_invalidates_history_until_reinitialized() {
    for panel in [Panel::Depg0397, Panel::Se0400nqw47] {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut display = Display::new(Mock(trace.clone()), panel).unwrap();
        let frame = vec![0; PLANE_BYTES];
        display.initialize().unwrap();
        display.full_refresh(&frame).unwrap();
        let before = trace.borrow().operations;
        display.refresh(&frame, Refresh::Fast).unwrap();
        let count = trace.borrow().operations - before;
        for fail in 1..=count {
            let trace = Rc::new(RefCell::new(Trace::default()));
            let mut display = Display::new(Mock(trace.clone()), panel).unwrap();
            display.initialize().unwrap();
            display.full_refresh(&frame).unwrap();
            let start = trace.borrow().operations;
            trace.borrow_mut().fail_at = Some(start + fail);
            assert_eq!(display.refresh(&frame, Refresh::Fast), Err(Error::Bus(())));
            let stopped = trace.borrow().operations;
            assert_eq!(display.refresh(&frame, Refresh::Fast), Err(Error::NotReady));
            assert_eq!(trace.borrow().operations, stopped);
            trace.borrow_mut().fail_at = None;
            display.initialize().unwrap();
            assert_eq!(
                display.refresh(&frame, Refresh::Fast).unwrap(),
                Refresh::Full
            );
        }
    }
}

#[test]
fn grayscale_portrait_encoding_preserves_all_four_levels() {
    use femto_picco_board::portrait_grayscale;
    let pixels: Vec<_> = (0..384_000).map(|i| ((i % 4) * 85) as u8).collect();
    let mut msb = vec![0; PLANE_BYTES];
    let mut lsb = msb.clone();
    portrait_grayscale(&pixels, &mut msb, &mut lsb).unwrap();
    for native_y in 0..480 {
        for native_x in 0..800 {
            let i = native_y * 800 + native_x;
            let bit = 7 - i % 8;
            let level = ((msb[i / 8] >> bit & 1) << 1) | (lsb[i / 8] >> bit & 1);
            assert_eq!(level * 85, pixels[native_x * 480 + 479 - native_y]);
        }
    }
    let old_msb = msb.clone();
    let old_lsb = lsb.clone();
    let mut invalid = pixels;
    invalid[0] = 127;
    assert_eq!(
        portrait_grayscale(&invalid, &mut msb, &mut lsb),
        Err(Error::FrameEncoding)
    );
    assert_eq!(msb, old_msb);
    assert_eq!(lsb, old_lsb);
}

#[test]
fn absolute_gray_uses_controller_encoding_and_forces_monochrome_cleanup() {
    for panel in [
        Panel::Yrd0426,
        Panel::Opm040b3,
        Panel::Depg0397,
        Panel::Se0400nqw47,
    ] {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut display = Display::new(Mock(trace.clone()), panel).unwrap();
        let high = vec![0x33; PLANE_BYTES];
        let low = vec![0x55; PLANE_BYTES];
        assert_eq!(display.grayscale(&high, &low), Err(Error::NotReady));
        display.initialize().unwrap();
        trace.borrow_mut().commands.clear();
        display.grayscale(&high, &low).unwrap();
        {
            let t = trace.borrow();
            if panel == Panel::Se0400nqw47 {
                assert_eq!(
                    t.commands.iter().map(|(c, _)| *c).collect::<Vec<_>>(),
                    [4, 0x10, 0x20, 0x12, 2]
                );
                assert!(t.commands[1].1.iter().all(|b| *b == 0x1b));
                assert_eq!(t.commands[2].1.len(), 535);
            } else {
                assert!(t.commands.contains(&(0x24, vec![0xaa; PLANE_BYTES])));
                assert!(t.commands.contains(&(0x26, vec![0xcc; PLANE_BYTES])));
                let activates: Vec<_> = t
                    .commands
                    .iter()
                    .enumerate()
                    .filter(|(_, (c, _))| *c == 0x20)
                    .map(|(i, _)| i)
                    .collect();
                assert_eq!(activates.len(), 2);
                assert_eq!(t.commands[activates[0] - 1], (0x22, vec![0xcc]));
                assert_eq!(t.commands[activates[1] - 1], (0x22, vec![3]));
                assert!(t.commands.contains(&(0x21, vec![0])));
            }
        }
        assert_eq!(
            display.refresh(&high, Refresh::Fast).unwrap(),
            Refresh::Full
        );
        assert_eq!(display.refresh(&low, Refresh::Fast).unwrap(), Refresh::Fast);
    }
}

#[test]
fn grayscale_failures_require_reinitialization_at_every_bus_boundary() {
    for panel in [Panel::Depg0397, Panel::Se0400nqw47] {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut display = Display::new(Mock(trace.clone()), panel).unwrap();
        let plane = vec![0x55; PLANE_BYTES];
        display.initialize().unwrap();
        let start = trace.borrow().operations;
        display.grayscale(&plane, &plane).unwrap();
        let count = trace.borrow().operations - start;
        for fail in 1..=count {
            let trace = Rc::new(RefCell::new(Trace::default()));
            let mut display = Display::new(Mock(trace.clone()), panel).unwrap();
            display.initialize().unwrap();
            let start = trace.borrow().operations;
            trace.borrow_mut().fail_at = Some(start + fail);
            assert_eq!(display.grayscale(&plane, &plane), Err(Error::Bus(())));
            let stopped = trace.borrow().operations;
            assert_eq!(display.refresh(&plane, Refresh::Fast), Err(Error::NotReady));
            assert_eq!(display.grayscale(&plane, &plane), Err(Error::NotReady));
            assert_eq!(trace.borrow().operations, stopped);
            trace.borrow_mut().fail_at = None;
            display.initialize().unwrap();
            display.grayscale(&plane, &plane).unwrap();
        }
    }
}

#[test]
fn sleep_invalidates_refresh_baseline_and_requires_reset_after_every_failure() {
    for panel in [
        Panel::Yrd0426,
        Panel::Depg0397,
        Panel::Opm040b3,
        Panel::Se0400nqw47,
    ] {
        let trace = Rc::new(RefCell::new(Trace::default()));
        let mut display = Display::new(Mock(trace.clone()), panel).unwrap();
        assert_eq!(display.sleep(), Err(Error::NotReady));
        assert_eq!(trace.borrow().operations, 0);
        display.initialize().unwrap();
        *trace.borrow_mut() = Trace::default();
        display.sleep().unwrap();
        let operations = trace.borrow().operations;
        let expected = if panel == Panel::Se0400nqw47 {
            vec![(0x07, vec![0xa5])]
        } else {
            vec![
                (0x3c, vec![if panel == Panel::Opm040b3 { 3 } else { 1 }]),
                (0x22, vec![3]),
                (0x20, vec![]),
                (0x10, vec![3]),
            ]
        };
        assert_eq!(trace.borrow().commands, expected);
        let plane = vec![255; PLANE_BYTES];
        assert_eq!(display.refresh(&plane, Refresh::Fast), Err(Error::NotReady));
        assert_eq!(display.sleep(), Err(Error::NotReady));
        display.initialize().unwrap();
        assert_eq!(
            display.refresh(&plane, Refresh::Fast).unwrap(),
            Refresh::Full
        );
        for failure in 1..=operations {
            display.initialize().unwrap();
            *trace.borrow_mut() = Trace {
                fail_at: Some(failure),
                ..Default::default()
            };
            assert_eq!(display.sleep(), Err(Error::Bus(())));
            assert_eq!(display.refresh(&plane, Refresh::Full), Err(Error::NotReady));
            assert_eq!(trace.borrow().operations, failure);
            trace.borrow_mut().fail_at = None;
            display.initialize().unwrap();
            assert_eq!(
                display.refresh(&plane, Refresh::Fast).unwrap(),
                Refresh::Full
            );
        }
    }
}
