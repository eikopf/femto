use femto_picco_board::{
    input::Turn,
    touch::{Gesture, Touch},
};

fn report(contacts: &[(u8, u8, u16, u16)]) -> Vec<u8> {
    let mut bytes = vec![0; 7 + contacts.len() * 10];
    let len = bytes.len() as u16;
    bytes[..2].copy_from_slice(&len.to_le_bytes());
    bytes[2] = 1;
    bytes[5] = contacts.len() as u8;
    for (record, &(id, event, x, y)) in bytes[7..].as_chunks_mut::<10>().0.iter_mut().zip(contacts)
    {
        record[1] = id | (event << 5);
        record[2..4].copy_from_slice(&x.to_le_bytes());
        record[4..6].copy_from_slice(&y.to_le_bytes());
    }
    bytes
}

#[test]
fn tap_coordinates_match_portrait_edges_and_explicit_lift() {
    for (raw_x, raw_y, x, y) in [(0, 0, 479, 799), (480, 800, 0, 0), (400, 600, 80, 200)] {
        let mut touch = Touch::default();
        assert_eq!(touch.report(&report(&[(3, 1, raw_x, raw_y)]), 10), None);
        assert_eq!(
            touch.report(&report(&[(3, 3, raw_x, raw_y)]), 100),
            Some(Gesture::Tap { x, y })
        );
        assert_eq!(touch.report(&report(&[]), 110), None);
    }
}

#[test]
fn horizontal_swipes_and_tap_slop_are_distinct() {
    let mut touch = Touch::default();
    touch.report(&report(&[(0, 1, 100, 400)]), 10);
    touch.report(&report(&[(0, 2, 300, 400)]), 100);
    assert_eq!(
        touch.report(&report(&[]), 150),
        Some(Gesture::Swipe(Turn::Next))
    );
    touch.report(&report(&[(0, 1, 300, 400)]), 200);
    touch.report(&report(&[(0, 2, 100, 400)]), 300);
    assert_eq!(
        touch.report(&report(&[]), 350),
        Some(Gesture::Swipe(Turn::Previous))
    );
    touch.report(&report(&[(0, 1, 100, 400)]), 400);
    touch.report(&report(&[(0, 2, 150, 400)]), 450);
    touch.report(&report(&[(0, 2, 100, 400)]), 500);
    assert_eq!(touch.report(&report(&[]), 550), None);
}

#[test]
fn malformed_multitouch_and_identity_changes_cancel_until_release() {
    let mut malformed = report(&[(0, 1, 100, 200)]);
    malformed[5] = 2;
    for invalid in [
        malformed,
        report(&[(0, 1, 481, 200)]),
        report(&[(0, 1, 100, 801)]),
        report(&[(0, 1, 100, 200), (1, 1, 200, 200)]),
        report(&[(0, 1, 100, 200), (0, 2, 200, 200)]),
        report(&[(1, 1, 100, 200)]),
        vec![0; 129],
        vec![1],
        vec![],
    ] {
        let mut touch = Touch::default();
        touch.report(&report(&[(0, 1, 100, 200)]), 10);
        assert_eq!(touch.report(&invalid, 50), None);
        assert_eq!(touch.report(&report(&[(0, 2, 100, 200)]), 70), None);
        assert_eq!(touch.report(&report(&[]), 100), None);
        touch.report(&report(&[(0, 1, 100, 200)]), 200);
        assert_eq!(
            touch.report(&report(&[]), 250),
            Some(Gesture::Tap { x: 380, y: 600 })
        );
    }
}

#[test]
fn stale_reset_and_long_holds_do_not_become_taps() {
    let mut touch = Touch::default();
    touch.report(&report(&[(0, 1, 100, 200)]), 100);
    touch.expire(3100);
    assert_eq!(touch.report(&report(&[]), 3101), None);
    touch.report(&report(&[(0, 1, 100, 200)]), 4000);
    assert_eq!(touch.report(&report(&[]), 5100), None);
    touch.report(&report(&[(0, 1, 100, 200)]), 6000);
    assert_eq!(touch.report(&report(&[]), 0), None);
    touch.report(&report(&[(0, 1, 100, 200)]), 10);
    touch.cancel();
    assert_eq!(touch.report(&report(&[]), 100), None);
}

#[test]
fn rotated_taps_and_swipes_use_the_display_view_axes() {
    use femto_picco_board::orientation::Orientation;
    for orientation in [
        Orientation::Portrait,
        Orientation::Clockwise,
        Orientation::Inverted,
        Orientation::Counterclockwise,
    ] {
        let raw = |x, y| {
            let (px, py) = orientation.to_portrait(x, y).unwrap();
            ((480 - px) as u16, (800 - py) as u16)
        };
        let mut touch = Touch::default();
        touch.set_orientation(orientation);
        touch.report(&report(&[]), 0);
        let (x, y) = raw(31, 97);
        touch.report(&report(&[(0, 1, x, y)]), 10);
        assert_eq!(
            touch.report(&report(&[]), 100),
            Some(Gesture::Tap { x: 31, y: 97 })
        );
        let (x, y) = raw(200, 100);
        touch.report(&report(&[(0, 1, x, y)]), 200);
        let (x, y) = raw(80, 100);
        touch.report(&report(&[(0, 2, x, y)]), 300);
        assert_eq!(
            touch.report(&report(&[]), 400),
            Some(Gesture::Swipe(Turn::Next))
        );
        let (x, y) = raw(80, 100);
        touch.report(&report(&[(0, 1, x, y)]), 500);
        let (x, y) = raw(200, 100);
        touch.report(&report(&[(0, 2, x, y)]), 600);
        assert_eq!(
            touch.report(&report(&[]), 700),
            Some(Gesture::Swipe(Turn::Previous))
        );
    }
}
#[test]
fn orientation_changes_discard_the_contact_until_release() {
    use femto_picco_board::orientation::Orientation;
    let mut touch = Touch::default();
    touch.report(&report(&[(0, 1, 100, 200)]), 10);
    touch.set_orientation(Orientation::Clockwise);
    assert_eq!(touch.report(&report(&[(0, 2, 100, 200)]), 50), None);
    assert_eq!(touch.report(&report(&[]), 100), None);
    touch.report(&report(&[(0, 1, 100, 200)]), 200);
    touch.set_orientation(Orientation::Clockwise);
    assert_eq!(
        touch.report(&report(&[]), 300),
        Some(Gesture::Tap { x: 199, y: 380 })
    );
}
