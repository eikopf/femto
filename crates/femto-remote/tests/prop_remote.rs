//! Property tests for Bluetooth page-turner support: report descriptors and
//! input decoding, the notification inbox, reconnect timing, the scan
//! inventory and the discovery, subscription and shutdown sequencers.
use femto_remote::{
    Bindings, PageTurn, RemoteInput,
    discovery::{
        self, AddressType, Characteristic, CharacteristicKind, Connection, Discovery,
        HidCharacteristics, Peer, ReadKind, ReadRequest, ServiceRange,
    },
    gatt::{Descriptor, DescriptorKind, InputReport, ReportDescriptors, ReportRoutes},
    inbox::{self, Event, Inbox, ResetReason, Submit},
    reconnect, scan, shutdown, subscriptions,
};
use proptest::prelude::*;
use std::collections::{BTreeSet, VecDeque};

// ---------------------------------------------------------------- descriptors

/// One input report of a generated keyboard-like device.
#[derive(Clone, Debug)]
struct ReportSpec {
    /// Usage page and one-byte usage IDs of single-bit buttons.
    button_page: u8,
    buttons: Vec<u8>,
    /// Keycode array: usage minimum, logical minimum, number of selectable
    /// usages, extra logical values without a usage, and slots.
    usage_min: u8,
    logical_min: u8,
    span: u8,
    unmapped: u8,
    slots: u8,
}

impl ReportSpec {
    fn payload_len(&self) -> usize {
        self.buttons.len().div_ceil(8) + usize::from(self.slots)
    }

    /// The usages a payload presses, or None when the report must be refused.
    fn pressed(&self, payload: &[u8]) -> Option<BTreeSet<u32>> {
        let mut pressed = BTreeSet::new();
        let mut add = |page: u8, id: u32| {
            let supported = matches!(page, 7 | 12);
            if supported && id != 0 {
                if page == 7 && id <= 3 {
                    return false;
                }
                pressed.insert(u32::from(page) << 16 | id);
            }
            true
        };
        for (index, usage) in self.buttons.iter().enumerate() {
            if payload[index / 8] >> (index % 8) & 1 == 1 && !add(self.button_page, u32::from(*usage))
            {
                return None;
            }
        }
        for value in &payload[self.buttons.len().div_ceil(8)..] {
            let offset = value.checked_sub(self.logical_min)?;
            if offset >= self.span + self.unmapped {
                return None;
            }
            if offset >= self.span {
                // Inside the logical range but beyond the declared usages.
                return None;
            }
            if !add(7, u32::from(self.usage_min) + u32::from(offset)) {
                return None;
            }
        }
        Some(pressed)
    }
}

fn descriptor(reports: &[ReportSpec], numbered: bool) -> Vec<u8> {
    let mut bytes = vec![0x05, 1, 0x09, 6, 0xa1, 1];
    for (index, report) in reports.iter().enumerate() {
        if numbered {
            bytes.extend([0x85, index as u8 + 1]);
        }
        if !report.buttons.is_empty() {
            bytes.extend([0x05, report.button_page]);
            for usage in &report.buttons {
                bytes.extend([0x09, *usage]);
            }
            bytes.extend([0x15, 0, 0x25, 1, 0x75, 1, 0x95, report.buttons.len() as u8, 0x81, 2]);
            let padding = report.buttons.len().div_ceil(8) * 8 - report.buttons.len();
            if padding != 0 {
                bytes.extend([0x75, 1, 0x95, padding as u8, 0x81, 3]);
            }
        }
        if report.slots != 0 {
            let logical_max =
                u16::from(report.logical_min) + u16::from(report.span + report.unmapped) - 1;
            bytes.extend([0x05, 7, 0x19, report.usage_min]);
            bytes.extend([0x29, report.usage_min + report.span - 1]);
            bytes.extend([0x15, report.logical_min, 0x26]);
            bytes.extend(logical_max.to_le_bytes());
            bytes.extend([0x75, 8, 0x95, report.slots, 0x81, 0]);
        }
    }
    bytes.push(0xc0);
    bytes
}

fn report_spec() -> impl Strategy<Value = ReportSpec> {
    (
        prop_oneof![3 => Just(7u8), 3 => Just(12u8), 1 => Just(1u8)],
        prop::collection::btree_set(prop_oneof![1 => 0u8..4, 6 => 4u8..120], 0..=8),
        0u8..6,
        0u8..3,
        1u8..90,
        prop_oneof![3 => Just(0u8), 1 => 1u8..4],
        0u8..=4,
    )
        .prop_map(|(button_page, buttons, usage_min, logical_min, span, unmapped, slots)| {
            ReportSpec {
                button_page,
                buttons: buttons.into_iter().collect(),
                usage_min,
                logical_min,
                span,
                unmapped,
                slots,
            }
        })
        .prop_filter("a report carries data", |r| r.payload_len() != 0)
}

/// A raw item stream that is plausible enough to pass preflight often.
fn item() -> impl Strategy<Value = Vec<u8>> {
    let small = prop_oneof![4 => 0u8..10, 2 => any::<u8>()];
    prop_oneof![
        // Usage page, logical range, size, count, report ID.
        3 => (prop::sample::select(vec![0x05u8, 0x15, 0x25, 0x35, 0x45, 0x55, 0x65, 0x75, 0x95, 0x85]), small.clone()).prop_map(|(p, v)| vec![p, v]),
        1 => (prop::sample::select(vec![0x06u8, 0x16, 0x26, 0x76, 0x96]), any::<u16>()).prop_map(|(p, v)| [vec![p], v.to_le_bytes().to_vec()].concat()),
        1 => (prop::sample::select(vec![0x07u8, 0x17, 0x27, 0x77, 0x97, 0x0b, 0x1b, 0x2b]), any::<u32>()).prop_map(|(p, v)| [vec![p], v.to_le_bytes().to_vec()].concat()),
        // Locals: usage, usage minimum and maximum, and the rarer ones.
        3 => (prop::sample::select(vec![0x09u8, 0x19, 0x29, 0x39, 0x49, 0x59, 0x79, 0x89, 0x99]), small.clone()).prop_map(|(p, v)| vec![p, v]),
        1 => (prop::sample::select(vec![0x0au8, 0x1a, 0x2a]), any::<u16>()).prop_map(|(p, v)| [vec![p], v.to_le_bytes().to_vec()].concat()),
        // Mains: input, output, feature with arbitrary flags.
        4 => (prop::sample::select(vec![0x81u8, 0x91, 0xb1]), small.clone()).prop_map(|(p, v)| vec![p, v]),
        1 => (prop::sample::select(vec![0x82u8, 0x92, 0xb2]), any::<u16>()).prop_map(|(p, v)| [vec![p], v.to_le_bytes().to_vec()].concat()),
        2 => (0u8..7).prop_map(|v| vec![0xa1, v]),
        2 => Just(vec![0xc0]),
        1 => Just(vec![0xa4]),
        1 => Just(vec![0xb4]),
        1 => prop::collection::vec(any::<u8>(), 1..4),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// New presses are exactly the usages that were up in every report before
    /// and are down now; refused reports change nothing.
    #[test]
    fn decoded_presses_match_model(
        reports in prop::collection::vec(report_spec(), 1..=2),
        numbered in any::<bool>(),
        notifications in prop::collection::vec(
            (any::<prop::sample::Index>(), any::<prop::sample::Index>(), 0u8..12, any::<u64>()),
            1..12,
        ),
    ) {
        let numbered = numbered || reports.len() > 1;
        let bytes = descriptor(&reports, numbered);
        let mut input = match RemoteInput::new(&bytes) {
            Ok(input) => input,
            Err(error) => return Err(TestCaseError::fail(format!("{error}: {bytes:02x?}"))),
        };
        let mut held = vec![BTreeSet::new(); reports.len()];
        for (which, seed, fault, random) in notifications {
            let slot = which.index(reports.len());
            let spec = &reports[slot];
            let id = if numbered { slot as u8 + 1 } else { 0 };
            // Derive a payload deterministically from the generated numbers.
            let mut state = random | 1;
            let mut bytes: Vec<u8> = (0..spec.payload_len())
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 7;
                    state ^= state << 17;
                    match state % 8 {
                        0..=3 => 0,
                        4..=6 => (state >> 8) as u8 % (spec.logical_min + spec.span + spec.unmapped + 1),
                        _ => (state >> 8) as u8,
                    }
                })
                .collect();
            let _ = seed;
            let (reference, want): ([u8; 2], Option<BTreeSet<u32>>) = match fault {
                0 => ([id, 2], None),
                1 => ([id.wrapping_add(7), 1], None),
                2 => {
                    bytes.push(0);
                    ([id, 1], None)
                }
                3 if bytes.len() > 1 => {
                    bytes.pop();
                    ([id, 1], None)
                }
                4 => {
                    input.reset();
                    held.iter_mut().for_each(BTreeSet::clear);
                    ([id, 1], spec.pressed(&bytes))
                }
                _ => ([id, 1], spec.pressed(&bytes)),
            };
            let before: BTreeSet<u32> = held.iter().flatten().copied().collect();
            let got = input.accept(&reference, &bytes);
            match want {
                None => prop_assert!(got.is_err(), "accepted {:02x?} for {:?}: {:?}", bytes, spec, got),
                Some(pressed) => {
                    let events = got.map_err(|e| TestCaseError::fail(format!("{e}: {bytes:02x?} for {spec:?}")))?;
                    held[slot] = pressed;
                    let after: BTreeSet<u32> = held.iter().flatten().copied().collect();
                    let new: BTreeSet<u32> = after.difference(&before).copied().collect();
                    prop_assert_eq!(events.as_slice().len(), new.len(), "duplicates in {:?}", events);
                    prop_assert_eq!(events.as_slice().iter().copied().collect::<BTreeSet<_>>(), new);
                }
            }
        }
    }

    /// Whatever a peer sends as its report map and notifications, decoding
    /// returns an error or bounded presses; it never panics.
    #[test]
    fn hostile_descriptors_and_reports_never_panic(
        items in prop::collection::vec(item(), 1..40),
        wrap in any::<bool>(),
        notifications in prop::collection::vec((0u8..4, 0u8..3, prop::collection::vec(any::<u8>(), 0..70)), 0..8),
    ) {
        let mut bytes = if wrap { vec![0x05, 7, 0x15, 0, 0x25, 1, 0x75, 1, 0x95, 3] } else { Vec::new() };
        bytes.extend(items.concat());
        if let Ok(mut input) = RemoteInput::new(&bytes) {
            for (id, kind, payload) in notifications {
                if let Ok(presses) = input.accept(&[id, kind], &payload) {
                    prop_assert!(presses.as_slice().len() <= 32);
                    for usage in presses.as_slice() {
                        prop_assert!(matches!(usage >> 16, 7 | 12) && usage & 0xffff != 0);
                    }
                    let _ = Bindings::default().resolve(&presses);
                }
            }
            input.reset();
        }
    }

    #[test]
    fn arbitrary_descriptor_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..200)) {
        let _ = RemoteInput::new(&bytes);
    }

    /// Bindings: one direction wins only when no opposing key is down.
    #[test]
    fn bindings_resolve_by_direction(
        entries in prop::collection::vec((prop_oneof![4 => 4u32..40, 1 => 0u32..4], prop::sample::select(vec![7u32, 12, 1]), any::<bool>()), 0..18),
        down in prop::collection::btree_set(4u8..40, 0..6),
    ) {
        let table: Vec<(u32, PageTurn)> = entries
            .iter()
            .map(|(id, page, next)| (page << 16 | id, if *next { PageTurn::Next } else { PageTurn::Previous }))
            .collect();
        let unique = table.iter().map(|(usage, _)| usage).collect::<BTreeSet<_>>().len() == table.len();
        let supported = table.iter().all(|(usage, _)| match usage >> 16 {
            7 => usage & 0xffff > 3,
            12 => usage & 0xffff > 0,
            _ => false,
        });
        let bindings = Bindings::new(&table);
        prop_assert_eq!(bindings.is_ok(), table.len() <= 16 && unique && supported);
        let Ok(bindings) = bindings else { return Ok(()) };
        prop_assert_eq!(bindings.entries(), table.as_slice());
        // Press keyboard usages through a real descriptor.
        let mut input = RemoteInput::new(&[0x05, 7, 0x19, 0, 0x29, 255, 0x15, 0, 0x26, 255, 0, 0x75, 8, 0x95, 6, 0x81, 0]).unwrap();
        let mut payload = [0u8; 6];
        for (slot, key) in payload.iter_mut().zip(&down) {
            *slot = *key;
        }
        let presses = input.accept(&[0, 1], &payload).unwrap();
        let directions: BTreeSet<bool> = table
            .iter()
            .filter(|(usage, _)| usage >> 16 == 7 && down.contains(&(*usage as u8)))
            .map(|(_, turn)| *turn == PageTurn::Next)
            .collect();
        let want = match (directions.contains(&false), directions.contains(&true)) {
            (true, false) => Some(PageTurn::Previous),
            (false, true) => Some(PageTurn::Next),
            _ => None,
        };
        prop_assert_eq!(bindings.resolve(&presses), want);
    }
}

// ---------------------------------------------------------------------- inbox

#[derive(Clone, Debug)]
enum InboxOp {
    Begin,
    Disconnect(usize),
    Submit(usize, u8, usize),
    Poll,
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn inbox_matches_queue_model(
        ops in prop::collection::vec(
            prop_oneof![
                1 => Just(InboxOp::Begin),
                1 => (0usize..3).prop_map(InboxOp::Disconnect),
                10 => (prop_oneof![9 => Just(0usize), 1 => 1usize..3], prop_oneof![12 => Just(1u8), 1 => any::<u8>()], prop_oneof![12 => 1usize..=64, 1 => Just(0usize), 1 => 65usize..70])
                    .prop_map(|(t, k, l)| InboxOp::Submit(t, k, l)),
                5 => Just(InboxOp::Poll),
            ],
            0..120,
        ),
    ) {
        let mut inbox = Inbox::default();
        // Tokens most recent first; index 0 is the session begun last.
        let mut tokens = Vec::new();
        let mut active = false;
        let mut queue: VecDeque<(inbox::SessionToken, [u8; 2], Vec<u8>)> = VecDeque::new();
        let mut reset = None;
        let mut serial = 0u8;
        for op in ops {
            match op {
                InboxOp::Begin => {
                    let token = inbox.begin().unwrap();
                    prop_assert!(!tokens.contains(&token), "token reused");
                    tokens.insert(0, token);
                    active = true;
                    queue.clear();
                    reset = Some(ResetReason::SessionChanged);
                }
                InboxOp::Disconnect(index) => {
                    let Some(token) = tokens.get(index) else { continue };
                    inbox.disconnect(*token);
                    if index == 0 && active {
                        active = false;
                        queue.clear();
                        reset = Some(ResetReason::Disconnected);
                    }
                }
                InboxOp::Submit(index, kind, len) => {
                    let Some(token) = tokens.get(index) else { continue };
                    serial = serial.wrapping_add(1);
                    let payload = vec![serial; len];
                    let reference = [serial, kind];
                    let got = inbox.submit(*token, reference, &payload);
                    let want = if index != 0 || !active {
                        Submit::Stale
                    } else if len == 0 || len > 64 || kind != 1 {
                        reset = Some(ResetReason::InvalidNotification);
                        Submit::Invalidated
                    } else if queue.len() == inbox::CAPACITY {
                        reset = Some(ResetReason::Overflow);
                        Submit::Invalidated
                    } else {
                        queue.push_back((*token, reference, payload));
                        Submit::Queued
                    };
                    if want == Submit::Invalidated {
                        active = false;
                        queue.clear();
                    }
                    prop_assert_eq!(got, want);
                }
                InboxOp::Poll => {
                    let want = match reset.take() {
                        Some(reason) => Some(Err(reason)),
                        None => queue.pop_front().map(Ok),
                    };
                    match (inbox.poll(), want) {
                        (None, None) => {}
                        (Some(Event::Reset(got)), Some(Err(want))) => prop_assert_eq!(got, want),
                        (Some(Event::Input(got)), Some(Ok((token, reference, payload)))) => {
                            prop_assert_eq!(got.token, token);
                            prop_assert_eq!(got.reference, reference);
                            prop_assert_eq!(got.payload(), payload.as_slice());
                            // Input is only ever delivered for the live session.
                            prop_assert!(inbox.is_current(got.token));
                        }
                        (got, want) => prop_assert!(false, "{:?} instead of {:?}", got, want),
                    }
                }
            }
            for (index, token) in tokens.iter().enumerate() {
                prop_assert_eq!(inbox.is_current(*token), index == 0 && active);
            }
        }
    }
}

// ------------------------------------------------------------------ reconnect

#[derive(Clone, Debug)]
enum RetryOp {
    Ready(bool),
    Released,
    Take,
    Stop,
    Resume,
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// The model follows the caller's view: an attempt is offered once, only
    /// after teardown, never while stopped, and after a delay that doubles
    /// from 2 s to a 60 s cap unless a connection stayed ready for 30 s.
    #[test]
    fn reconnect_matches_model(
        start in prop_oneof![Just(0u64), 0u64..100_000, Just(u64::MAX - 70_000), Just(u64::MAX - 5_000_000)],
        ops in prop::collection::vec(
            (
                prop_oneof![
                    4 => any::<bool>().prop_map(RetryOp::Ready),
                    4 => Just(RetryOp::Released),
                    6 => Just(RetryOp::Take),
                    1 => Just(RetryOp::Stop),
                    1 => Just(RetryOp::Resume),
                ],
                prop_oneof![3 => 0u64..3000, 2 => 3000u64..70_000],
            ),
            0..80,
        ),
    ) {
        let mut retry = reconnect::Retry::default();
        let mut now = start;
        let mut active = true;
        let mut stopped = false;
        let mut delay = reconnect::INITIAL_DELAY_MS;
        let mut due: Option<u64> = None;
        let mut ready_since: Option<u64> = None;
        for (op, dt) in ops {
            now = now.saturating_add(dt);
            match op {
                RetryOp::Ready(ready) => {
                    retry.ready(now, ready);
                    if active && !stopped {
                        if ready {
                            let since = *ready_since.get_or_insert(now);
                            if now - since >= reconnect::STABLE_MS {
                                delay = reconnect::INITIAL_DELAY_MS;
                            }
                        } else {
                            ready_since = None;
                        }
                    }
                }
                RetryOp::Released => {
                    retry.released(now);
                    if active {
                        active = false;
                        ready_since = None;
                        if !stopped {
                            due = now.checked_add(delay);
                            delay = (delay * 2).min(reconnect::MAX_DELAY_MS);
                        }
                    }
                }
                RetryOp::Take => {
                    let want = !stopped && !active && due.is_some_and(|due| now >= due);
                    prop_assert_eq!(retry.take_attempt(now), want, "at {}", now);
                    if want {
                        due = None;
                        active = true;
                    }
                }
                RetryOp::Stop => {
                    retry.stop();
                    stopped = true;
                    due = None;
                }
                RetryOp::Resume => {
                    retry.resume(now);
                    stopped = false;
                    delay = reconnect::INITIAL_DELAY_MS;
                    if !active {
                        due = now.checked_add(delay);
                    }
                }
            }
            prop_assert_eq!(retry.is_stopped(), stopped);
            prop_assert!((reconnect::INITIAL_DELAY_MS..=reconnect::MAX_DELAY_MS).contains(&delay));
        }
    }

    /// A stopped retry that is resumed, and a torn-down one that is not
    /// stopped, always offers another attempt within the cap.
    #[test]
    fn reconnect_does_not_stall(
        ops in prop::collection::vec(
            (prop_oneof![
                3 => any::<bool>().prop_map(RetryOp::Ready),
                3 => Just(RetryOp::Released),
                4 => Just(RetryOp::Take),
                1 => Just(RetryOp::Stop),
                1 => Just(RetryOp::Resume),
            ], 0u64..40_000),
            0..60,
        ),
    ) {
        let mut retry = reconnect::Retry::default();
        let mut now = 0u64;
        let mut active = true;
        for (op, dt) in ops {
            now += dt;
            match op {
                RetryOp::Ready(ready) => retry.ready(now, ready),
                RetryOp::Released => {
                    retry.released(now);
                    active = false;
                }
                RetryOp::Take => active |= retry.take_attempt(now),
                RetryOp::Stop => retry.stop(),
                RetryOp::Resume => retry.resume(now),
            }
        }
        if retry.is_stopped() {
            prop_assert!(!retry.take_attempt(u64::MAX));
            retry.resume(now);
        }
        if active {
            retry.released(now);
        }
        prop_assert!(retry.take_attempt(now + reconnect::MAX_DELAY_MS));
    }
}

// ----------------------------------------------------------------------- scan

/// Splits advertising data into (type, value) structures; None if malformed.
/// A zero length ends the data and may only be followed by zero padding.
fn structures(mut bytes: &[u8]) -> Option<Vec<(u8, Vec<u8>)>> {
    let mut out = Vec::new();
    if bytes.len() > 31 {
        return None;
    }
    while !bytes.is_empty() {
        let len = usize::from(bytes[0]);
        if len == 0 {
            return bytes.iter().all(|b| *b == 0).then_some(out);
        }
        if len + 1 > bytes.len() {
            return None;
        }
        out.push((bytes[1], bytes[2..=len].to_vec()));
        bytes = &bytes[len + 1..];
    }
    Some(out)
}

fn advertising() -> impl Strategy<Value = Vec<u8>> {
    let structure = prop_oneof![
        3 => (prop::sample::select(vec![8u8, 9]), "[a-zA-Z \u{e9}]{0,8}").prop_map(|(tag, name)| {
            let mut bytes = vec![name.len() as u8 + 1, tag];
            bytes.extend(name.bytes());
            bytes
        }),
        3 => (prop::sample::select(vec![2u8, 3, 7]), prop::collection::vec(prop::sample::select(vec![[0x12u8, 0x18], [0x0f, 0x18], [0x18, 0x12]]), 0..4), any::<bool>()).prop_map(|(tag, uuids, odd)| {
            let mut bytes = vec![0, tag];
            bytes.extend(uuids.concat());
            if odd {
                bytes.push(0x12);
            }
            bytes[0] = bytes.len() as u8 - 1;
            bytes
        }),
        1 => Just(vec![2, 9, 0xff]),
        1 => Just(vec![2, 9, 7]),
        1 => Just(vec![1, 9]),
        2 => prop::collection::vec(any::<u8>(), 1..6),
        1 => prop::collection::vec(Just(0u8), 1..4),
    ];
    prop::collection::vec(structure, 0..5).prop_map(|parts| parts.concat())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    #[test]
    fn scan_inventory_matches_model(
        records in prop::collection::vec(
            (0u8..20, 0u8..5, any::<i8>(), advertising(), advertising()),
            0..40,
        ),
    ) {
        let mut inventory = scan::Inventory::default();
        // (address, type, rssi, advertising, response) in first-seen order.
        let mut model: Vec<([u8; 6], u8, i8, Vec<u8>, Vec<u8>)> = Vec::new();
        let mut dropped = 0u32;
        for (address, address_type, rssi, advertising, response) in records {
            let address = [address, 1, 2, 3, 4, 5];
            let valid = address_type <= 3
                && structures(&advertising).is_some()
                && structures(&response).is_some();
            let result = inventory.record(address, address_type, rssi, &advertising, &response);
            prop_assert_eq!(result.is_ok(), valid, "{:02x?} / {:02x?}", advertising, response);
            if !valid {
                // fall through to the comparison: a refused packet changes nothing
            } else if let Some(entry) = model.iter_mut().find(|e| e.0 == address && e.1 == address_type) {
                entry.2 = rssi;
                if !advertising.is_empty() {
                    entry.3 = advertising;
                }
                if !response.is_empty() {
                    entry.4 = response;
                }
            } else if model.len() < scan::CAPACITY {
                model.push((address, address_type, rssi, advertising, response));
            } else {
                dropped += 1;
            }
            prop_assert_eq!(inventory.dropped, dropped);
            let entries: Vec<_> = inventory.entries().collect();
            prop_assert_eq!(entries.len(), model.len());
            for (entry, (address, address_type, rssi, advertising, response)) in entries.iter().zip(&model) {
                prop_assert_eq!((entry.address, entry.address_type, entry.rssi), (*address, *address_type, *rssi));
                let fields: Vec<(u8, Vec<u8>)> = [advertising, response]
                    .into_iter()
                    .flat_map(|data| structures(data).unwrap())
                    .collect();
                let name = |kind: u8| {
                    fields.iter().find_map(|(tag, value)| {
                        let name = std::str::from_utf8(value).ok()?;
                        (*tag == kind && !name.is_empty() && !name.chars().any(char::is_control))
                            .then_some(name)
                    })
                };
                prop_assert_eq!(entry.name(), name(9).or(name(8)));
                let hid = fields.iter().any(|(tag, value)| {
                    matches!(tag, 2 | 3)
                        && value.len() % 2 == 0
                        && value.chunks(2).any(|uuid| uuid == [0x12, 0x18])
                });
                prop_assert_eq!(entry.advertises_hid(), hid);
            }
        }
    }

    #[test]
    fn peer_configuration_round_trips_and_rejects_noise(
        address in prop::array::uniform6(any::<u8>()),
        random in any::<bool>(),
        upper in any::<bool>(),
        noise in prop_oneof![3 => Just(String::new()), 1 => "[ \\t\\n:0-9a-fx]{0,8}", 1 => "\\PC{0,6}"],
        bytes in prop::collection::vec(any::<u8>(), 0..70),
    ) {
        let _ = Peer::parse(&bytes);
        let hex: Vec<String> = address
            .iter()
            .map(|b| if upper { format!("{b:02X}") } else { format!("{b:02x}") })
            .collect();
        let text = format!("{} {}\n", if random { "random" } else { "public" }, hex.join(":"));
        let parsed = Peer::parse(text.as_bytes());
        if address == [0; 6] || address == [255; 6] {
            prop_assert!(parsed.is_err());
        } else {
            let peer = parsed.unwrap();
            prop_assert_eq!(peer.address(), address);
            prop_assert_eq!(peer.address_type(), if random { AddressType::Random } else { AddressType::Public });
        }
        // Anything accepted names exactly one type and one six-byte address.
        let noisy = format!("{}{}", text.trim_end(), noise);
        if let Ok(peer) = Peer::parse(noisy.as_bytes()) {
            let words: Vec<&str> = noisy.split_ascii_whitespace().collect();
            prop_assert_eq!(words.len(), 2);
            let digits: Vec<u8> = words[1].split(':').map(|part| u8::from_str_radix(part, 16).unwrap()).collect();
            prop_assert_eq!(digits, peer.address().to_vec());
            prop_assert!(words[1].len() == 17 && noisy.len() <= discovery::MAX_PEER_BYTES);
        }
    }
}

// ------------------------------------------------------ discovery sequencing

fn connection() -> impl Strategy<Value = Connection> {
    (prop_oneof![5 => Just(4u8), 1 => Just(5u8), 1 => Just(255u8)], prop_oneof![5 => Just(12u16), 1 => Just(13u16)])
        .prop_map(|(interface, id)| Connection { interface, id })
}

#[derive(Clone, Debug)]
enum Step {
    Registered(u8, bool),
    Opened(Connection, bool, bool),
    Service(Connection, u16, u16, bool),
    Searched(Connection, bool),
    Inspected(Connection),
    Disconnected(Connection),
    ServiceChanged(bool),
    BeginRead(Connection, u16, bool),
    ReadCompleted(Connection, u16, bool, bool, usize),
    TakeRead,
    Finish(Connection),
    TakeCommand,
    Abort,
}

fn step() -> impl Strategy<Value = Step> {
    let ok = || prop::bool::weighted(0.9);
    prop_oneof![
        3 => (prop_oneof![6 => Just(4u8), 1 => Just(255u8)], ok()).prop_map(|(i, s)| Step::Registered(i, s)),
        3 => (connection(), ok(), ok()).prop_map(|(c, s, a)| Step::Opened(c, s, a)),
        3 => (connection(), 0u16..3, 2u16..5, ok()).prop_map(|(c, s, e, p)| Step::Service(c, s, e, p)),
        3 => (connection(), ok()).prop_map(|(c, s)| Step::Searched(c, s)),
        3 => connection().prop_map(Step::Inspected),
        1 => connection().prop_map(Step::Disconnected),
        1 => any::<bool>().prop_map(Step::ServiceChanged),
        4 => (connection(), 0u16..3, any::<bool>()).prop_map(|(c, h, m)| Step::BeginRead(c, h, m)),
        4 => (connection(), 0u16..3, any::<bool>(), ok(), prop_oneof![Just(0usize), Just(1usize), Just(2usize), Just(512usize), Just(513usize)])
            .prop_map(|(c, h, m, s, l)| Step::ReadCompleted(c, h, m, s, l)),
        3 => Just(Step::TakeRead),
        2 => connection().prop_map(Step::Finish),
        8 => Just(Step::TakeCommand),
        1 => Just(Step::Abort),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4000))]

    /// Events in any order: commands come out once each and in order, nothing
    /// is commanded after a failure, an error never clears, and completion
    /// requires that every step was acknowledged for the configured peer.
    #[test]
    fn discovery_survives_any_event_order(steps in prop::collection::vec(step(), 0..50)) {
        let peer = Peer::parse(b"public 12:34:56:78:9a:bc").unwrap();
        let mut discovery = Discovery::new(peer);
        let mut commands = Vec::new();
        let mut error = None;
        let mut opened_ok = false;
        let mut reads_delivered = 0;
        let mut pending_read: Option<ReadRequest> = None;
        for step in steps.iter() {
            match step.clone() {
                Step::Registered(interface, success) => discovery.registered(interface, success),
                Step::Opened(connection, success, right_address) => {
                    let address = if right_address { peer.address() } else { [9; 6] };
                    discovery.opened(connection, address, success);
                    opened_ok |= success && right_address;
                }
                Step::Service(connection, start, end, primary) => {
                    discovery.hid_service(connection, ServiceRange { start, end }, primary)
                }
                Step::Searched(connection, success) => discovery.searched(connection, success),
                Step::Inspected(connection) => discovery.inspected(connection),
                Step::Disconnected(connection) => discovery.disconnected(connection),
                Step::ServiceChanged(ours) => {
                    discovery.service_changed(if ours { peer.address() } else { [9; 6] })
                }
                Step::BeginRead(connection, handle, map) => {
                    let request = ReadRequest {
                        connection,
                        handle,
                        kind: if map { ReadKind::ReportMap } else { ReadKind::ReportReference },
                    };
                    let was_discovered = discovery.is_discovered();
                    if discovery.begin_read(request).is_ok() {
                        prop_assert!(was_discovered && handle != 0 && pending_read.is_none());
                        prop_assert_eq!(discovery.connection(), Some(connection));
                        pending_read = Some(request);
                    }
                }
                Step::ReadCompleted(connection, handle, map, success, len) => {
                    let request = ReadRequest {
                        connection,
                        handle,
                        kind: if map { ReadKind::ReportMap } else { ReadKind::ReportReference },
                    };
                    discovery.read_completed(request, success, &vec![7; len]);
                }
                Step::TakeRead => match discovery.take_read() {
                    Ok(Some(response)) => {
                        prop_assert_eq!(Some(response.request), pending_read.take());
                        prop_assert!(response.bytes().iter().all(|b| *b == 7));
                        match response.request.kind {
                            ReadKind::ReportMap => prop_assert!((1..=512).contains(&response.bytes().len())),
                            ReadKind::ReportReference => prop_assert_eq!(response.bytes().len(), 2),
                        }
                        reads_delivered += 1;
                    }
                    Ok(None) => {}
                    Err(_) => pending_read = None,
                },
                Step::Finish(connection) => discovery.finish(connection),
                Step::TakeCommand => commands.extend(discovery.take_command()),
                Step::Abort => discovery.abort("test abort"),
            }
            if let Some(first) = error {
                prop_assert_eq!(discovery.error(), Some(first), "the first error is kept");
                prop_assert!(!discovery.is_done() || reads_delivered == usize::MAX);
                prop_assert!(!discovery.is_discovered());
                let probe = ReadRequest { connection: Connection { interface: 4, id: 12 }, handle: 1, kind: ReadKind::ReportMap };
                prop_assert!(discovery.begin_read(probe).is_err());
            }
            error = discovery.error();
            if discovery.is_done() || discovery.is_discovered() {
                prop_assert!(opened_ok && commands.len() == 3, "{:?}", commands);
                prop_assert_eq!(discovery.interface(), Some(4));
            }
        }
        let kinds: Vec<u8> = commands
            .iter()
            .map(|command| match command {
                discovery::Command::Open { interface, peer: target } => {
                    assert_eq!((*interface, *target), (discovery.interface().unwrap(), peer));
                    0
                }
                discovery::Command::SearchHid(connection) => {
                    assert_eq!(Some(*connection), discovery.connection());
                    1
                }
                discovery::Command::Inspect { connection, service } => {
                    assert_eq!(Some(*connection), discovery.connection());
                    assert!(service.start != 0 && service.start < service.end);
                    2
                }
            })
            .collect();
        prop_assert!(kinds.iter().copied().eq(0..kinds.len() as u8), "{:?}", commands);
    }

    /// Shutdown: whatever happened before, once stopped the sequencer closes
    /// the connection, then unregisters, each once, and finishes when the
    /// platform acknowledges every command it was given.
    #[test]
    fn shutdown_finishes_when_commands_are_acknowledged(
        steps in prop::collection::vec(
            prop_oneof![
                2 => (prop_oneof![5 => Just(4u8), 1 => Just(5u8), 1 => Just(255u8)], prop::bool::weighted(0.85)).prop_map(|(i, s)| (0u8, Connection { interface: i, id: 0 }, s)),
                2 => connection().prop_map(|c| (1u8, c, true)),
                2 => (connection(), prop::bool::weighted(0.8)).prop_map(|(c, s)| (2u8, c, s)),
                1 => connection().prop_map(|c| (3u8, c, true)),
                1 => (connection(), any::<bool>()).prop_map(|(c, s)| (4u8, c, s)),
                1 => connection().prop_map(|c| (5u8, c, true)),
                1 => connection().prop_map(|c| (6u8, c, true)),
                3 => connection().prop_map(|c| (7u8, c, true)),
            ],
            0..40,
        ),
    ) {
        let mut state = shutdown::Shutdown::default();
        let mut issued: Vec<shutdown::Command> = Vec::new();
        let mut opens_outstanding = false;
        for (kind, connection, success) in steps {
            match kind {
                0 => state.registered(connection.interface, success),
                1 => {
                    let allowed = state.opening(connection.interface);
                    prop_assert!(!allowed || (!state.is_stopping() && !opens_outstanding && state.connection().is_none()));
                    opens_outstanding |= allowed;
                }
                2 => {
                    state.opened(connection, success);
                    if state.interface() == Some(connection.interface) {
                        opens_outstanding = false;
                    }
                }
                3 => {
                    state.open_rejected();
                    opens_outstanding = false;
                }
                4 => state.closed(connection, success),
                5 => state.unregistered(connection.interface),
                6 => state.stop(),
                _ => {
                    let command = state.take_command();
                    prop_assert!(command.is_none() || state.is_stopping());
                    issued.extend(command);
                }
            }
            prop_assert!(!state.is_done() || (state.interface().is_none() && state.is_stopping()));
        }
        // The platform now cooperates: pending callbacks arrive, commands are
        // acknowledged.
        state.stop();
        state.registered(4, true);
        state.open_rejected();
        for _ in 0..4 {
            match state.take_command() {
                Some(command) => {
                    issued.push(command);
                    match command {
                        shutdown::Command::Close(connection) => state.closed(connection, true),
                        shutdown::Command::Unregister(interface) => state.unregistered(interface),
                    }
                }
                None => break,
            }
        }
        prop_assert!(state.take_command().is_none());
        // An unregister issued earlier and never acknowledged is the caller's
        // deadline to handle; otherwise the sequencer must be finished.
        let unacknowledged = state.interface().is_some();
        if unacknowledged {
            prop_assert!(issued.iter().any(|c| matches!(c, shutdown::Command::Unregister(_))) || state.connection().is_some());
        } else {
            prop_assert!(state.is_done());
        }
        // A connection is never left open behind an unregistered interface
        // without a close having been issued for it.
        if let Some(connection) = state.connection() {
            prop_assert!(issued.contains(&shutdown::Command::Close(connection)));
        }
        let unregisters = issued.iter().filter(|c| matches!(c, shutdown::Command::Unregister(_))).count();
        prop_assert!(unregisters <= 1, "{:?}", issued);
        if let Some(position) = issued.iter().position(|c| matches!(c, shutdown::Command::Unregister(_))) {
            prop_assert_eq!(position, issued.len() - 1, "{:?}", issued);
        }
    }

    /// Notification setup: register then enable each report in order; any
    /// unexpected or failed acknowledgement fails for good.
    #[test]
    fn subscriptions_follow_the_handles_in_order(
        count in 1usize..=3,
        events in prop::collection::vec(
            (any::<bool>(), prop_oneof![8 => Just(0u16), 1 => 1u16..40], prop::bool::weighted(0.95), prop::bool::weighted(0.9), prop::bool::weighted(0.8)),
            0..24,
        ),
    ) {
        // One report ID per notifying characteristic.
        let mut map = vec![0x05, 7, 0x15, 0, 0x25, 1, 0x75, 8, 0x95, 1];
        let mut reports = Vec::new();
        for index in 0..count {
            map.extend([0x85, index as u8 + 1, 0x09, 4 + index as u8, 0x81, 2]);
            reports.push(InputReport {
                value_handle: 10 + 3 * index as u16,
                cccd_handle: 11 + 3 * index as u16,
                reference: [index as u8 + 1, 1],
            });
        }
        let input = RemoteInput::new(&map).unwrap();
        let routes = ReportRoutes::new(&input, &reports).unwrap();
        for report in &reports {
            prop_assert_eq!(routes.reference(report.value_handle), Some(report.reference));
        }
        let ours = Connection { interface: 1, id: 2 };
        let mut state = subscriptions::Subscriptions::new(ours, &routes);
        // The acknowledgement the sequencer is waiting for, once its command
        // has been taken.
        let mut awaiting: Option<subscriptions::Command> = None;
        let mut issued = Vec::new();
        let mut failed = false;
        for (take, wrong_handle, success, right_connection, expected_kind) in events {
            if take {
                let command = state.take_command();
                prop_assert!(command.is_none() || (awaiting.is_none() && !failed));
                if let Some(command) = command {
                    issued.push(command);
                    awaiting = Some(command);
                }
                continue;
            }
            // Deliver an acknowledgement, usually the awaited one.
            let (register, handle) = match awaiting {
                Some(subscriptions::Command::Register(handle)) => (expected_kind, handle),
                Some(subscriptions::Command::Enable(handle)) => (!expected_kind, handle),
                None => (expected_kind, 10),
            };
            let handle = handle + wrong_handle;
            let exact = awaiting == Some(if register { subscriptions::Command::Register(handle) } else { subscriptions::Command::Enable(handle) });
            if register {
                state.registered(if right_connection { 1 } else { 9 }, handle, success);
            } else {
                state.configured(if right_connection { ours } else { Connection { interface: 1, id: 3 } }, handle, success);
            }
            if right_connection && !failed {
                if exact && success {
                    awaiting = None;
                } else {
                    failed = true;
                }
            }
            prop_assert_eq!(state.error().is_some(), failed);
            prop_assert!(!(failed && state.is_ready()));
        }
        let want: Vec<_> = reports
            .iter()
            .flat_map(|r| [subscriptions::Command::Register(r.value_handle), subscriptions::Command::Enable(r.cccd_handle)])
            .collect();
        prop_assert!(want.starts_with(&issued), "{:?}", issued);
        prop_assert_eq!(state.is_ready(), !failed && issued.len() == want.len() && awaiting.is_none());
    }

    /// GATT layout validation accepts exactly the well-formed layouts.
    #[test]
    fn characteristic_and_descriptor_layouts(
        start in 0u16..4,
        end in 0u16..40,
        characteristics in prop::collection::vec((1u16..6, 0u8..3, prop::bool::weighted(0.9), prop::bool::weighted(0.8)), 0..12),
        descriptors in prop::collection::vec((1u16..4, 0u8..3), 0..6),
    ) {
        let mut handle = start;
        let list: Vec<Characteristic> = characteristics
            .iter()
            .map(|(step, kind, readable, notifying)| {
                handle += step;
                Characteristic {
                    handle,
                    kind: [CharacteristicKind::ReportMap, CharacteristicKind::Report, CharacteristicKind::Other][usize::from(*kind)],
                    readable: *readable,
                    notifying: *notifying,
                }
            })
            .collect();
        let maps: Vec<_> = list.iter().filter(|c| c.kind == CharacteristicKind::ReportMap).collect();
        let notifying: Vec<u16> = list.iter().filter(|c| c.kind == CharacteristicKind::Report && c.notifying).map(|c| c.handle).collect();
        let valid = start != 0
            && start < end
            && !list.is_empty()
            && list.last().unwrap().handle <= end
            && maps.len() == 1
            && maps[0].readable
            && (1..=8).contains(&notifying.len());
        match HidCharacteristics::new(ServiceRange { start, end }, &list) {
            Ok(found) => {
                prop_assert!(valid, "{:?}", list);
                prop_assert_eq!(found.map_handle(), maps[0].handle);
                prop_assert_eq!(found.report_handles(), notifying.as_slice());
            }
            Err(error) => prop_assert!(!valid, "{}: {:?}", error, list),
        }

        let mut handle = start;
        let list: Vec<Descriptor> = descriptors
            .iter()
            .map(|(step, kind)| {
                handle += step;
                Descriptor {
                    handle,
                    kind: [DescriptorKind::ReportReference, DescriptorKind::ClientConfiguration, DescriptorKind::Other][usize::from(*kind)],
                }
            })
            .collect();
        let of = |kind: DescriptorKind| list.iter().filter(|d| d.kind == kind).map(|d| d.handle).collect::<Vec<_>>();
        let (references, configurations) = (of(DescriptorKind::ReportReference), of(DescriptorKind::ClientConfiguration));
        let valid = start != 0
            && end > start
            && !list.is_empty()
            && list.last().unwrap().handle <= end
            && references.len() == 1
            && configurations.len() == 1;
        match ReportDescriptors::new(start, end, &list) {
            Ok(found) => {
                prop_assert!(valid, "{:?}", list);
                prop_assert_eq!((found.reference_handle(), found.configuration_handle()), (references[0], configurations[0]));
            }
            Err(error) => prop_assert!(!valid, "{}: {:?}", error, list),
        }
    }
}
