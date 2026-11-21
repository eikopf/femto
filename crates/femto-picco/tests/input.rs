use femto_picco_board::input::{Buttons, Press, Turn};

fn immediate(buttons: &mut Buttons, previous: bool, next: bool, now: u64) -> Option<Turn> {
    buttons
        .sample(previous, next, now, false)
        .map(|press| match press {
            Press::Short(turn) => turn,
            Press::Long(_) => panic!("long presses are disabled"),
        })
}

#[test]
fn bounce_hold_and_chords_require_a_stable_release() {
    let mut buttons = Buttons::default();
    assert_eq!(immediate(&mut buttons, false, true, 10), None);
    assert_eq!(immediate(&mut buttons, false, false, 20), None);
    assert_eq!(immediate(&mut buttons, false, true, 25), None);
    assert_eq!(immediate(&mut buttons, false, true, 54), None);
    assert_eq!(immediate(&mut buttons, false, true, 55), Some(Turn::Next));
    assert_eq!(immediate(&mut buttons, false, true, 1000), None);
    assert_eq!(immediate(&mut buttons, true, true, 1010), None);
    assert_eq!(immediate(&mut buttons, true, true, 1040), None);
    assert_eq!(immediate(&mut buttons, true, false, 1050), None);
    assert_eq!(immediate(&mut buttons, true, false, 1080), None);
    assert_eq!(immediate(&mut buttons, false, false, 1090), None);
    assert_eq!(immediate(&mut buttons, false, false, 1120), None);
    assert_eq!(immediate(&mut buttons, true, false, 1130), None);
    assert_eq!(
        immediate(&mut buttons, true, false, 1160),
        Some(Turn::Previous)
    );
}

#[test]
fn simultaneous_buttons_and_clock_reset_do_not_invent_page_turns() {
    let mut buttons = Buttons::default();
    assert_eq!(immediate(&mut buttons, true, true, 100), None);
    assert_eq!(immediate(&mut buttons, true, true, 130), None);
    assert_eq!(immediate(&mut buttons, false, true, 140), None);
    assert_eq!(immediate(&mut buttons, false, true, 170), None);
    assert_eq!(immediate(&mut buttons, false, false, 180), None);
    assert_eq!(immediate(&mut buttons, false, false, 210), None);
    assert_eq!(immediate(&mut buttons, true, false, 220), None);
    assert_eq!(immediate(&mut buttons, true, false, 0), None);
    assert_eq!(immediate(&mut buttons, true, false, 29), None);
    assert_eq!(
        immediate(&mut buttons, true, false, 30),
        Some(Turn::Previous)
    );
}

#[test]
fn long_presses_report_once_and_short_presses_wait_for_release() {
    let mut buttons = Buttons::default();
    // Short press: nothing until the stable release.
    assert_eq!(buttons.sample(false, true, 0, true), None);
    assert_eq!(buttons.sample(false, true, 30, true), None);
    assert_eq!(buttons.sample(false, true, 200, true), None);
    assert_eq!(buttons.sample(false, false, 210, true), None);
    assert_eq!(
        buttons.sample(false, false, 240, true),
        Some(Press::Short(Turn::Next))
    );
    assert_eq!(buttons.sample(false, false, 300, true), None);
    // Long press: reported at the threshold, not again while held or on release.
    assert_eq!(buttons.sample(true, false, 1000, true), None);
    assert_eq!(buttons.sample(true, false, 1030, true), None);
    assert_eq!(buttons.sample(true, false, 1529, true), None);
    assert_eq!(
        buttons.sample(true, false, 1530, true),
        Some(Press::Long(Turn::Previous))
    );
    assert_eq!(buttons.sample(true, false, 3000, true), None);
    assert_eq!(buttons.sample(false, false, 3010, true), None);
    assert_eq!(buttons.sample(false, false, 3040, true), None);
    // The mode is fixed at press time, and a second button cancels the press.
    assert_eq!(buttons.sample(false, true, 4000, true), None);
    assert_eq!(buttons.sample(false, true, 4030, true), None);
    assert_eq!(buttons.sample(true, true, 4100, false), None);
    assert_eq!(buttons.sample(true, true, 4130, false), None);
    assert_eq!(buttons.sample(false, false, 4200, false), None);
    assert_eq!(buttons.sample(false, false, 4230, false), None);
    assert_eq!(buttons.sample(false, true, 5000, true), None);
    assert_eq!(
        buttons.sample(false, true, 5030, false),
        Some(Press::Short(Turn::Next))
    );
    assert_eq!(buttons.sample(false, true, 6000, true), None);
}

#[test]
fn view_barrier_suppresses_transition_events_and_requires_button_release() {
    use femto_picco_board::input::ViewGate;
    let mut view = ViewGate::default();
    let mut queue = vec![(view.touch().unwrap(), "old tap")];
    assert!(view.change(1, true));
    assert_eq!(view.touch(), None);
    assert_eq!(view.buttons(true), None);
    assert!(view.change(2, false));
    assert_eq!(view.buttons(false), None);
    queue.push((view.touch().unwrap(), "new tap"));
    assert_eq!(view.buttons(true), Some(2));
    assert_eq!(view.buttons(false), Some(2));
    let delivered: Vec<_> = queue
        .into_iter()
        .filter(|(generation, _)| *generation == 2)
        .collect();
    assert_eq!(delivered, vec![(2, "new tap")]);
    assert!(!view.change(2, true));
    assert!(!view.change(4, true));
    assert_eq!(view.touch(), Some(2));
    assert!(view.change(3, true));
    assert_eq!(view.touch(), None);
}
