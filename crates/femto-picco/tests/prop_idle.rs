//! Property tests for the automatic-sleep policy, the Wi-Fi retry schedule,
//! the battery curve and the clock gate.
use femto_picco_board::{
    battery::{Power, Sample},
    clock,
    idle::Idle,
    wifi::{Action, Retry, Saved},
};
use proptest::prelude::*;

#[derive(Clone, Debug)]
enum IdleOp {
    Activity,
    Configure(u64),
    Poll { enabled: bool, busy: bool },
    Suppress,
}

/// docs/picco-firmware.md: sleep is requested once after `timeout` without a
/// user event; busy work, disabled sleep and a backwards clock restart the
/// interval; a changed setting restarts it; zero disables; a failed attempt
/// suppresses retries until another user event.
struct IdleModel {
    timeout: u64,
    /// Start of the current uninterrupted idle interval.
    since: u64,
    last: u64,
    suppressed: bool,
}

fn time() -> impl Strategy<Value = u64> {
    prop_oneof![
        6 => 0u64..2000,
        1 => Just(0u64),
        1 => Just(u64::MAX),
        1 => u64::MAX - 2000..=u64::MAX,
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(5000))]

    #[test]
    fn idle_matches_model(
        start in time(),
        timeout in prop_oneof![Just(0u64), 1u64..600, Just(u64::MAX)],
        ops in prop::collection::vec(
            (
                prop_oneof![
                    2 => Just(IdleOp::Activity),
                    1 => prop_oneof![Just(0u64), 1u64..600, Just(u64::MAX)].prop_map(IdleOp::Configure),
                    8 => (prop::bool::weighted(0.85), prop::bool::weighted(0.15))
                        .prop_map(|(enabled, busy)| IdleOp::Poll { enabled, busy }),
                    1 => Just(IdleOp::Suppress),
                ],
                // Mostly forward steps, sometimes an arbitrary (possibly earlier) time.
                prop_oneof![5 => (0u64..300).prop_map(Ok), 1 => time().prop_map(Err)],
            ),
            0..40,
        ),
    ) {
        let mut idle = Idle::new(start, timeout);
        let mut model = IdleModel { timeout, since: start, last: start, suppressed: false };
        let mut now = start;
        for (index, (op, step)) in ops.iter().enumerate() {
            now = match step {
                Ok(dt) => now.saturating_add(*dt),
                Err(t) => *t,
            };
            match op {
                IdleOp::Activity => {
                    idle.activity(now);
                    model.since = now;
                    model.last = now;
                    model.suppressed = false;
                }
                IdleOp::Configure(timeout) => {
                    idle.configure(now, *timeout);
                    if *timeout != model.timeout {
                        model.timeout = *timeout;
                        model.since = now;
                        model.last = now;
                        // Changing the setting is itself a user event.
                        model.suppressed = false;
                    }
                }
                IdleOp::Suppress => {
                    idle.suppress_until_activity();
                    model.suppressed = true;
                }
                IdleOp::Poll { enabled, busy } => {
                    if now < model.last || !enabled || *busy {
                        model.since = now;
                    }
                    model.last = now;
                    let due = *enabled
                        && !busy
                        && !model.suppressed
                        && model.timeout != 0
                        && now >= model.since
                        && now - model.since >= model.timeout;
                    model.suppressed |= due;
                    prop_assert_eq!(idle.poll(now, *enabled, *busy), due, "op {} at {}", index, now);
                }
            }
        }
    }

    /// Whatever happens, polling emits at most one request between user events.
    #[test]
    fn idle_emits_once_between_activities(
        timeout in 1u64..500,
        polls in prop::collection::vec((0u64..400, prop::bool::weighted(0.9), prop::bool::weighted(0.1)), 1..60),
    ) {
        let mut idle = Idle::new(0, timeout);
        let mut now = 0;
        let mut emitted = 0;
        let mut quiet = 0u64;
        for (dt, enabled, busy) in polls {
            now += dt;
            quiet = if !enabled || busy { 0 } else { quiet + dt };
            if idle.poll(now, enabled, busy) {
                emitted += 1;
                prop_assert!(quiet >= timeout);
            }
        }
        prop_assert!(emitted <= 1);
    }

    #[test]
    fn battery_percentage_is_bounded_and_monotone(
        low in 1200u16..2300,
        charger in any::<u8>(),
    ) {
        let reading = |mv: u16| Sample::from_readings(Some([mv; 3]), Some(charger));
        let (a, b) = (reading(low), reading(low + 1));
        prop_assert_eq!(a.power, b.power);
        match (a.percentage, b.percentage) {
            (Some(a), Some(b)) => prop_assert!(a <= b && b <= 100 && b - a <= 10, "{} then {}", a, b),
            (None, _) => prop_assert!(low * 2 < 2500 || low * 2 > 4500),
            (_, None) => prop_assert!((low + 1) * 2 > 4500),
        }
    }

    #[test]
    fn battery_uses_the_median_and_never_panics(
        samples in prop::array::uniform3(any::<u16>()),
        charger in prop::option::of(any::<u8>()),
    ) {
        let sample = Sample::from_readings(Some(samples), charger);
        let mut sorted = samples;
        sorted.sort_unstable();
        let median = u32::from(sorted[1]) * 2;
        prop_assert_eq!(
            sample.millivolts.map(u32::from),
            (2500..=4500).contains(&median).then_some(median)
        );
        prop_assert_eq!(sample.percentage.is_some(), sample.millivolts.is_some() && charger.is_some());
        prop_assert_eq!(sample.power.is_some(), charger.is_some());
        if let Some(status) = charger {
            // Without a power source the charger state bits are irrelevant.
            if status & 2 == 0 {
                prop_assert_eq!(sample.power, Some(Power::Battery));
            }
        }
        prop_assert!(sample.percentage.is_none_or(|p| p <= 100));
    }

    /// A connection is attempted at most once per five seconds, an attempt is
    /// abandoned after thirty, and a link that drops is retried within a minute.
    #[test]
    fn wifi_retry_is_bounded(
        start in prop_oneof![Just(0u64), 0u64..100_000, Just(u64::MAX - 20_000_000)],
        steps in prop::collection::vec(
            (prop_oneof![4 => 1u64..2000, 1 => 2000u64..70_000], prop::bool::weighted(0.2), 0u8..20),
            1..200,
        ),
    ) {
        let mut retry = Retry::default();
        let mut now = start;
        let mut last_connect: Option<u64> = None;
        let mut connecting_since: Option<u64> = None;
        let mut waiting_since: Option<u64> = Some(start);
        // The link can only come up while an attempt is outstanding or once up.
        let mut up = false;
        for (dt, toggle, extra) in steps {
            now = now.saturating_add(dt);
            if toggle {
                up = if up { false } else { connecting_since.is_some() };
            }
            if extra == 0 && !up && connecting_since.is_some() {
                retry.failed(now);
                connecting_since = None;
                waiting_since = Some(now);
                continue;
            }
            let action = retry.poll(now, up);
            prop_assert_eq!(retry.is_online(), up && retry.label() == "Wi-Fi online");
            match action {
                Action::Connect => {
                    prop_assert!(!up);
                    if let Some(last) = last_connect {
                        prop_assert!(now - last >= 5000, "reconnect after {} ms", now - last);
                    }
                    last_connect = Some(now);
                    connecting_since = Some(now);
                    waiting_since = None;
                }
                Action::Disconnect => {
                    prop_assert!(!up);
                    connecting_since = None;
                    waiting_since = Some(now);
                }
                Action::None => {
                    if up {
                        connecting_since = None;
                        waiting_since = None;
                    } else if let Some(since) = connecting_since {
                        prop_assert!(now - since < 30_000, "attempt outstanding {} ms", now - since);
                    } else if let Some(since) = waiting_since {
                        prop_assert!(now - since < 60_000, "waited {} ms", now - since);
                    } else {
                        prop_assert!(false, "link down but neither waiting nor connecting");
                    }
                }
            }
        }
    }

    /// The saved-network list behaves as a most-recent-first list of eight and
    /// always survives its own serialization.
    #[test]
    fn saved_networks_match_model(
        ops in prop::collection::vec(
            (
                0u8..4,
                prop_oneof![
                    4 => "[a-e]",
                    2 => "[\\\\\"é a-c]{1,32}",
                    1 => "\\PC{0,40}",
                    1 => Just(String::from("a\u{7f}")),
                ],
                prop_oneof![
                    2 => Just(String::new()),
                    3 => "[ -~]{8,63}",
                    1 => "[\\\\\"]{63}",
                    1 => "\\PC{0,70}",
                ],
            ),
            0..40,
        ),
    ) {
        let mut saved = Saved::default();
        let mut model: Vec<(String, String)> = Vec::new();
        for (op, ssid, password) in ops {
            match op {
                0 | 1 => {
                    let valid_ssid = !ssid.is_empty()
                        && ssid.len() <= 32
                        && !ssid.chars().any(char::is_control);
                    let valid_password = password.is_empty()
                        || ((8..=63).contains(&password.len())
                            && password.bytes().all(|b| (0x20..=0x7e).contains(&b)));
                    let result = saved.remember(&ssid, &password);
                    prop_assert_eq!(result.is_ok(), valid_ssid && valid_password);
                    if result.is_ok() {
                        model.retain(|(name, _)| *name != ssid);
                        model.insert(0, (ssid, password));
                        model.truncate(8);
                    }
                }
                2 => {
                    let before = model.len();
                    model.retain(|(name, _)| *name != ssid);
                    prop_assert_eq!(saved.forget(&ssid), before != model.len());
                }
                _ => saved.set_enabled(!saved.enabled()),
            }
            prop_assert_eq!(saved.is_empty(), model.is_empty());
            let restored = Saved::parse(&saved.to_bytes());
            prop_assert!(restored.is_ok(), "saved networks do not parse back: {:?}", restored.err());
            let restored = restored.unwrap();
            prop_assert_eq!(restored.enabled(), saved.enabled());
            for (name, password) in &model {
                prop_assert_eq!(saved.password(name), Some(password.as_str()));
                prop_assert_eq!(restored.password(name), Some(password.as_str()));
            }
            // The strongest visible remembered network is chosen.
            let visible: Vec<(&str, i8)> = ["a", "b", "c", "zz"]
                .into_iter()
                .zip([-70, -40, -90, -10])
                .collect();
            let want = visible
                .iter()
                .filter(|(name, _)| model.iter().any(|(known, _)| known == name))
                .max_by_key(|(_, signal)| *signal)
                .map(|(name, _)| *name);
            prop_assert_eq!(saved.choose(visible.iter().copied()), want);
        }
    }

    #[test]
    fn saved_and_credentials_parsing_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..300)) {
        let _ = Saved::parse(&bytes);
        let _ = femto_picco_board::wifi::Credentials::parse(&bytes);
    }

    /// A clock sample vouches for wall time only while it is younger than a
    /// day and agrees with it to two seconds.
    #[test]
    fn clock_gate_matches_definition(
        sample_ms in 0u64..10_000_000,
        sample_unix in prop_oneof![clock::MIN_UNIX_SECONDS - 5..clock::MIN_UNIX_SECONDS + 100_000, any::<u64>()],
        age in prop_oneof![0u64..200_000, clock::MAX_AGE_MS - 5..clock::MAX_AGE_MS + 5],
        drift in -5i64..=5,
        backwards in prop::bool::weighted(0.1),
    ) {
        let mut gate = clock::Gate::new();
        gate.synchronized(sample_ms, sample_unix);
        prop_assume!(!backwards || sample_ms > 0);
        let now = if backwards { sample_ms - 1 } else { sample_ms + age };
        let unix = sample_unix.saturating_add(age / 1000).saturating_add_signed(drift);
        let in_range = |t: u64| (clock::MIN_UNIX_SECONDS..clock::MAX_UNIX_SECONDS).contains(&t);
        let want = in_range(sample_unix)
            && in_range(unix)
            && now >= sample_ms
            && now - sample_ms < clock::MAX_AGE_MS
            && drift.abs() <= 2;
        prop_assert_eq!(gate.ready(now, unix), want);
        // A failed check discards the sample.
        if !want {
            prop_assert!(!gate.ready(sample_ms, sample_unix));
        }
    }

    #[test]
    fn time_server_names_are_hostnames(server in prop_oneof!["[a-z0-9.-]{0,70}", "\\PC{0,20}", "[a-z]{60,70}(\\.[a-z]{60,63}){0,4}"]) {
        let want = !server.is_empty()
            && server.len() <= 253
            && server.split('.').all(|label| {
                (1..=63).contains(&label.len())
                    && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                    && !label.starts_with('-')
                    && !label.ends_with('-')
            });
        prop_assert_eq!(clock::valid_server(&server), want);
    }
}

/// The tables anchor every five percent; the anchors must come out exactly.
#[test]
fn battery_anchors() {
    let percent = |mv: u16, charger: u8| {
        Sample::from_readings(Some([mv / 2; 3]), Some(charger))
            .percentage
            .unwrap()
    };
    // Discharging (status 0): 4304 mV is just below full, 3300 mV is empty.
    assert_eq!(percent(4306, 0), 100);
    assert_eq!(percent(4230, 0), 95);
    assert_eq!(percent(3300, 0), 0);
    assert_eq!(percent(3566, 0), 5);
    // Charging (status 0x0a): the duplicated top anchor interpolates from 90.
    assert_eq!(percent(4300, 0x0a), 90);
    assert_eq!(percent(4304, 0x0a), 98);
    assert_eq!(percent(4306, 0x0a), 100);
    assert_eq!(percent(3500, 0x0a), 0);
}
