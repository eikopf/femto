//! Property tests for the page-button debouncer over irregular sample traces.
//!
//! The oracle works on the whole sampled trace at once: it first derives the
//! debounced signal, cuts it into episodes, and then decides one event per
//! press episode. `Buttons` is an incremental state machine, so the two share
//! the 30 ms / 500 ms constants but not the bookkeeping.
use femto_picco_board::input::{Buttons, Press, Turn, ViewGate};
use proptest::prelude::*;

#[derive(Clone, Copy, Debug)]
struct Sample {
    t: u64,
    value: u8,
    long: bool,
}

fn turn(value: u8) -> Option<Turn> {
    match value {
        1 => Some(Turn::Previous),
        2 => Some(Turn::Next),
        _ => None,
    }
}

/// Debounced value after each sample: a value becomes stable once the samples
/// have shown it without interruption for 30 ms.
fn debounced(samples: &[Sample]) -> Vec<u8> {
    let mut stable = 0;
    let mut out = Vec::new();
    for (i, s) in samples.iter().enumerate() {
        let mut first = i;
        while first > 0 && samples[first - 1].value == s.value {
            first -= 1;
        }
        // Before the first sample the buttons are taken to be released.
        let since = if first == 0 && s.value == 0 {
            0
        } else {
            samples[first].t
        };
        if s.t - since >= 30 {
            stable = s.value;
        }
        out.push(stable);
    }
    out
}

/// `strict`: a long press is only reported by a sample that still shows the
/// button pressed.
fn expected(samples: &[Sample], strict: bool) -> Vec<(usize, Press)> {
    let stable = debounced(samples);
    let mut events = Vec::new();
    let mut i = 0;
    while i < samples.len() {
        let before = if i == 0 { 0 } else { stable[i - 1] };
        let Some(turn) = turn(stable[i]).filter(|_| before == 0 && stable[i] != 0) else {
            i += 1;
            continue;
        };
        let end = (i..samples.len())
            .find(|&j| stable[j] != stable[i])
            .unwrap_or(samples.len());
        if !samples[i].long {
            events.push((i, Press::Short(turn)));
        } else if let Some(at) = (i..end).find(|&j| {
            samples[j].t - samples[i].t >= 500 && (!strict || samples[j].value == stable[i])
        }) {
            events.push((at, Press::Long(turn)));
        } else if end < samples.len() && stable[end] == 0 {
            events.push((end, Press::Short(turn)));
        }
        i = end;
    }
    events
}

fn actual(samples: &[Sample]) -> Vec<(usize, Press)> {
    let mut buttons = Buttons::default();
    samples
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            buttons
                .sample(s.value & 1 != 0, s.value & 2 != 0, s.t, s.long)
                .map(|press| (i, press))
        })
        .collect()
}

/// Segments of a constant physical state, each sampled at irregular intervals.
fn trace() -> impl Strategy<Value = Vec<Sample>> {
    let interval = prop_oneof![
        4 => Just(10u64),
        2 => Just(30u64),
        2 => 1u64..=100,
        1 => 100u64..=700,
    ];
    let value = prop_oneof![3 => Just(0u8), 3 => Just(1u8), 3 => Just(2u8), 1 => Just(3u8)];
    let segment = (
        value,
        prop::collection::vec(interval, 1..40),
        prop::bool::weighted(0.5),
    );
    (
        prop_oneof![Just(0u64), 0u64..100_000, Just(u64::MAX - 1_000_000)],
        prop::collection::vec(segment, 1..14),
        prop::bool::ANY,
        prop::bool::ANY,
    )
        .prop_map(|(start, segments, fixed_mode, mode)| {
            let mut t = start;
            let mut samples = Vec::new();
            for (value, intervals, long) in segments {
                for dt in intervals {
                    t += dt;
                    samples.push(Sample {
                        t,
                        value,
                        long: if fixed_mode { mode } else { long },
                    });
                }
            }
            samples
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// Pins the present behaviour, in which a long press may be reported by a
    /// sample that no longer shows the button. Once that is fixed, the strict
    /// property below replaces this one.
    #[test]
    fn events_match_the_episode_oracle(samples in trace()) {
        prop_assert_eq!(actual(&samples), expected(&samples, false));
    }

    #[test]
    #[ignore = "bug: a long press is reported by the sample that first shows the button released"]
    fn long_presses_are_reported_only_while_the_button_is_down(samples in trace()) {
        prop_assert_eq!(actual(&samples), expected(&samples, true));
    }

    /// Clean single-button presses separated by clean releases: one event each.
    #[test]
    fn every_clean_press_is_reported_exactly_once(
        presses in prop::collection::vec((1u8..=2, 4u64..120, 4u64..40), 1..8),
        long in prop::bool::ANY,
        step in prop_oneof![Just(10u64), Just(30u64)],
    ) {
        let mut samples = Vec::new();
        let mut t = 0;
        let mut want = Vec::new();
        for (value, down, up) in presses {
            let held = down * step;
            // Thresholds are measured from the debounced edge; stay clear of them.
            prop_assume!(!(500..=560).contains(&held));
            for _ in 0..=down {
                samples.push(Sample { t, value, long });
                t += step;
            }
            for _ in 0..=up {
                samples.push(Sample { t, value: 0, long });
                t += step;
            }
            let turn = turn(value).unwrap();
            want.push(if long && held > 530 { Press::Long(turn) } else { Press::Short(turn) });
        }
        let got: Vec<_> = actual(&samples).into_iter().map(|(_, press)| press).collect();
        prop_assert_eq!(got, want);
    }

    /// Stale generations are rejected and nothing passes while paused or
    /// before the buttons were seen released.
    #[test]
    fn view_gate_matches_model(ops in prop::collection::vec((0u8..3, 0u64..6, prop::bool::ANY), 0..60)) {
        let mut gate = ViewGate::default();
        let (mut generation, mut paused, mut blocked) = (0u64, false, false);
        for (op, argument, flag) in ops {
            match op {
                0 => {
                    let accepted = argument == generation + 1;
                    prop_assert_eq!(gate.change(argument, flag), accepted);
                    if accepted {
                        generation = argument;
                        paused = flag;
                        blocked = true;
                    }
                }
                1 => {
                    blocked &= !flag;
                    prop_assert_eq!(gate.buttons(flag), (!paused && !blocked).then_some(generation));
                }
                _ => prop_assert_eq!(gate.touch(), (!paused).then_some(generation)),
            }
        }
    }
}

fn run(samples: &[(u64, u8)], long: bool) -> Vec<(u64, Press)> {
    let mut buttons = Buttons::default();
    samples
        .iter()
        .filter_map(|&(t, value)| {
            buttons
                .sample(value & 1 != 0, value & 2 != 0, t, long)
                .map(|press| (t, press))
        })
        .collect()
}

/// The button is held from 0 to 450 ms, a short press. The poll after 450 ms
/// is late (the input task also reads the battery, the touch controller and
/// the frontlight on this thread) and already shows the button released, yet
/// that very sample reports a long press, which menus treat as Back.
#[test]
#[ignore = "bug: a long press is reported by the sample that first shows the button released"]
fn late_sample_after_release_is_not_a_long_press() {
    let mut samples: Vec<(u64, u8)> = (0..=45).map(|i| (i * 10, 1)).collect();
    samples.extend([(540, 0), (550, 0), (560, 0), (570, 0), (580, 0)]);
    assert_eq!(
        run(&samples, true),
        [(570, Press::Short(Turn::Previous))],
        "held for 450 ms, below the 500 ms threshold"
    );
}

/// Rolling from one button to the other: Previous is released 20 ms before
/// Next goes down, so the buttons are never pressed together, but the release
/// is shorter than the debounce interval and the second press is dropped.
#[test]
#[ignore = "ambiguous: a press within 30 ms of releasing the other button is swallowed"]
fn press_right_after_releasing_the_other_button_is_reported() {
    let mut samples: Vec<(u64, u8)> = (0..=10).map(|i| (i * 10, 1)).collect();
    samples.extend([(110, 0), (120, 0)]);
    samples.extend((13..=30).map(|i| (i * 10, 2)));
    samples.extend((31..=40).map(|i| (i * 10, 0)));
    assert_eq!(
        run(&samples, false),
        [
            (30, Press::Short(Turn::Previous)),
            (160, Press::Short(Turn::Next))
        ]
    );
}
