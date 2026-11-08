use femto_picco_board::input::{Buttons, Turn};

#[test]
fn bounce_hold_and_chords_require_a_stable_release() {
    let mut buttons = Buttons::default();
    assert_eq!(buttons.sample(false, true, 10), None);
    assert_eq!(buttons.sample(false, false, 20), None);
    assert_eq!(buttons.sample(false, true, 25), None);
    assert_eq!(buttons.sample(false, true, 54), None);
    assert_eq!(buttons.sample(false, true, 55), Some(Turn::Next));
    assert_eq!(buttons.sample(false, true, 1000), None);
    assert_eq!(buttons.sample(true, true, 1010), None);
    assert_eq!(buttons.sample(true, true, 1040), None);
    assert_eq!(buttons.sample(true, false, 1050), None);
    assert_eq!(buttons.sample(true, false, 1080), None);
    assert_eq!(buttons.sample(false, false, 1090), None);
    assert_eq!(buttons.sample(false, false, 1120), None);
    assert_eq!(buttons.sample(true, false, 1130), None);
    assert_eq!(buttons.sample(true, false, 1160), Some(Turn::Previous));
}

#[test]
fn simultaneous_buttons_and_clock_reset_do_not_invent_page_turns() {
    let mut buttons = Buttons::default();
    assert_eq!(buttons.sample(true, true, 100), None);
    assert_eq!(buttons.sample(true, true, 130), None);
    assert_eq!(buttons.sample(false, true, 140), None);
    assert_eq!(buttons.sample(false, true, 170), None);
    assert_eq!(buttons.sample(false, false, 180), None);
    assert_eq!(buttons.sample(false, false, 210), None);
    assert_eq!(buttons.sample(true, false, 220), None);
    assert_eq!(buttons.sample(true, false, 0), None);
    assert_eq!(buttons.sample(true, false, 29), None);
    assert_eq!(buttons.sample(true, false, 30), Some(Turn::Previous));
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
