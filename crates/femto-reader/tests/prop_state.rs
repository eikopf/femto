//! Property tests of reader state persistence: the host JSON file and the
//! device's two alternating journal slots.
use femto_reader::{
    clipping::Clipping,
    display::TextRaster,
    journal::StateJournal,
    layout::{Hyphenation, LayoutConfig, Locator},
    remote::{RemoteAction, RemoteBinding, SavedBindings},
    state::{
        BookState, BookTypography, Bookmark, FrontlightSettings, GestureAction, Gestures,
        IdleSleep, ReaderState, Rotation, SyncConflict, Synced,
    },
};
use proptest::prelude::*;
use sha2::{Digest, Sha256};
use std::path::Path;

// ---------------------------------------------------------------- generators

fn word() -> impl Strategy<Value = usize> {
    prop_oneof![
        6 => 0usize..40,
        1 => any::<usize>(),
        1 => Just(usize::MAX),
        1 => Just(u32::MAX as usize),
    ]
}

fn locator() -> impl Strategy<Value = Locator> {
    (word(), word(), word()).prop_map(|(chapter, block, byte)| Locator {
        chapter,
        block,
        byte,
    })
}

fn text() -> impl Strategy<Value = String> {
    let awkward = prop_oneof![
        Just('"'),
        Just('\\'),
        Just('/'),
        Just('\n'),
        Just('\0'),
        Just('\u{7f}'),
        Just('\u{e9}'),
        Just('\u{301}'),
        Just('\u{1F600}'),
        Just('\u{2028}'),
        Just('\u{feff}'),
        Just('\u{fffd}'),
        any::<char>(),
    ];
    prop_oneof![
        2 => Just(String::new()),
        4 => "[a-z ]{1,12}",
        4 => prop::collection::vec(awkward, 0..24).prop_map(String::from_iter),
        1 => any::<String>(),
    ]
}

const IDS: [&str; 3] = [
    "0000000000000000000000000000000000000000000000000000000000000000",
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
];

fn book_key() -> impl Strategy<Value = String> {
    prop_oneof![
        8 => prop::sample::select(&IDS[..]).prop_map(String::from),
        1 => "[0-9a-f]{64}",
        // Not a content hash: fine for a book without clippings.
        1 => text(),
    ]
}

fn float(valid: std::ops::RangeInclusive<f32>, listed: &'static [f32]) -> BoxedStrategy<f32> {
    let (low, high) = (*valid.start(), *valid.end());
    prop_oneof![
        30 => prop::sample::select(listed),
        20 => valid,
        5 => Just(low),
        5 => Just(high),
        1 => any::<u32>().prop_map(f32::from_bits),
    ]
    .boxed()
}

fn raster() -> impl Strategy<Value = TextRaster> {
    (
        any::<bool>(),
        float(0.0..=0.75, &[0.0, 0.2, 0.4]),
        float(0.3..=3.0, &[1.0, 0.85, 0.7]),
        any::<bool>(),
    )
        .prop_map(|(hinting, stroke, coverage_gamma, fractional)| TextRaster {
            hinting,
            stroke,
            coverage_gamma,
            fractional,
        })
}

fn hyphenation() -> impl Strategy<Value = Hyphenation> {
    use Hyphenation::*;
    prop::sample::select(vec![
        Book, Off, English, German, French, Spanish, Italian, Dutch, Portuguese,
    ])
}

fn margin() -> impl Strategy<Value = u32> {
    prop_oneof![60 => 0u32..=48, 1 => 0u32..2048, 1 => any::<u32>()]
}
fn font_size() -> BoxedStrategy<f32> {
    float(8.0..=96.0, &[14.0, 18.0, 24.0, 40.0])
}
fn line_height() -> BoxedStrategy<f32> {
    float(1.0..=3.0, &[1.0, 1.15, 1.35, 1.6, 2.0])
}

fn typography() -> impl Strategy<Value = BookTypography> {
    (margin(), font_size(), line_height(), hyphenation(), raster()).prop_map(
        |(margin, font_size, line_height, hyphenation, text_raster)| BookTypography {
            margin,
            font_size,
            line_height,
            hyphenation,
            text_raster,
        },
    )
}

fn layout() -> impl Strategy<Value = LayoutConfig> {
    let side = || prop_oneof![40 => 400u32..=2048, 2 => 80u32..400, 1 => 0u32..4096];
    (
        side(),
        side(),
        margin(),
        font_size(),
        line_height(),
        hyphenation(),
        prop_oneof![20 => Just(0u32), 20 => 0u32..64, 1 => any::<u32>()],
    )
        .prop_map(
            |(width, height, margin, font_size, line_height, hyphenation, footer)| LayoutConfig {
                width,
                height,
                margin,
                font_size,
                line_height,
                hyphenation,
                footer,
            },
        )
}

/// Clippings of one book, mostly valid and with distinct ranges; `book_id`
/// is filled in from the map key.
fn clippings() -> impl Strategy<Value = Vec<Clipping>> {
    let one = (
        0usize..20,
        0usize..20,
        0usize..7,
        1usize..20,
        prop_oneof![
            30 => "[a-z ]{1,20}".boxed(),
            10 => text().prop_map(|text| if text.is_empty() { "\u{e9}".into() } else { text }).boxed(),
            1 => Just(String::new()).boxed(),
        ],
        any::<u64>(),
        any::<bool>(),
        prop_oneof![40 => Just(1u32), 1 => any::<u32>()],
        prop_oneof![40 => Just(false), 1 => Just(true)],
    );
    prop::collection::vec(one, 0..4).prop_map(|parts| {
        parts
            .into_iter()
            .enumerate()
            .map(
                |(index, (block, byte, span, step, text, recorded_at, highlighted, version, flip))| {
                    let start = Locator {
                        chapter: index * 8,
                        block,
                        byte,
                    };
                    let end = Locator {
                        chapter: index * 8 + span,
                        block: block + step,
                        byte,
                    };
                    Clipping {
                        version,
                        book_id: String::new(),
                        range: if flip { end..start } else { start..end },
                        text,
                        recorded_at,
                        highlighted,
                    }
                },
            )
            .collect()
    })
}

fn book() -> BoxedStrategy<BookState> {
    (
        locator(),
        prop::collection::vec((locator(), text()), 0..4),
        any::<u64>(),
        clippings(),
        prop::option::of(typography()),
        prop::option::of((locator(), text(), any::<u64>())),
    )
        .prop_map(
            |(position, bookmarks, last_opened, clippings, typography, synced)| BookState {
                position,
                bookmarks: bookmarks
                    .into_iter()
                    .map(|(location, label)| Bookmark { location, label })
                    .collect(),
                last_opened,
                clippings,
                typography,
                synced: synced.map(|(position, device_id, timestamp)| Synced {
                    position,
                    device_id,
                    timestamp,
                }),
            },
        ).boxed()
}

fn bindings() -> impl Strategy<Value = Option<SavedBindings>> {
    let usage = prop_oneof![
        (4u32..40).prop_map(|id| 7 << 16 | id),
        (1u32..40).prop_map(|id| 12 << 16 | id),
        Just(12 << 16 | 0xffff),
    ];
    let action = prop_oneof![Just(RemoteAction::Previous), Just(RemoteAction::Next)];
    prop::option::of(prop::collection::btree_map(usage, action, 0..=16)).prop_map(|entries| {
        let entries: Vec<_> = entries?
            .into_iter()
            .map(|(usage, action)| RemoteBinding { usage, action })
            .collect();
        Some(SavedBindings::new(&entries).expect("generated bindings are valid"))
    })
}

fn gesture() -> impl Strategy<Value = GestureAction> {
    prop::sample::select(&GestureAction::ALL[..])
}

fn reader_state() -> BoxedStrategy<ReaderState> {
    let dictionary = prop_oneof![
        20 => Just(None),
        20 => "[a-z\u{e9}\u{4e2d} ]{1,12}\\.ifo".prop_map(Some),
        1 => text().prop_map(Some),
    ];
    let settings = (
        prop_oneof![40 => Just(1u32), 1 => any::<u32>()],
        dictionary,
        bindings(),
        prop::option::of(text()),
        layout(),
        raster(),
        (
            prop_oneof![40 => 0u8..=10, 1 => any::<u8>()],
            prop_oneof![40 => 0u8..=9, 1 => any::<u8>()],
        ),
    );
    let choices = (
        prop::sample::select(vec![
            IdleSleep::Off,
            IdleSleep::FiveMinutes,
            IdleSleep::TenMinutes,
            IdleSleep::ThirtyMinutes,
            IdleSleep::SixtyMinutes,
        ]),
        prop::sample::select(vec![
            Rotation::Portrait,
            Rotation::Clockwise,
            Rotation::Inverted,
            Rotation::Counterclockwise,
        ]),
        (gesture(), gesture(), gesture()),
        any::<[bool; 4]>(),
        prop::sample::select(vec![
            SyncConflict::Ask,
            SyncConflict::Remote,
            SyncConflict::Local,
        ]),
        any::<u8>(),
    );
    let books = prop::collection::btree_map(book_key(), book(), 0..4);
    (settings, choices, books, prop_oneof![30 => Just(false), 1 => Just(true)]).prop_map(
        |(settings, choices, mut books, foreign)| {
            let (version, dictionary_name, remote_bindings, font_family, layout, text_raster, light) =
                settings;
            let (idle_sleep, rotation, gestures, flags, sync_conflict, full_refresh_interval) =
                choices;
            for (id, book) in &mut books {
                for clipping in &mut book.clippings {
                    clipping.book_id = if foreign { IDS[0].into() } else { id.clone() };
                }
            }
            ReaderState {
                version,
                dictionary_name,
                remote_bindings,
                font_family,
                layout,
                text_raster,
                frontlight: FrontlightSettings {
                    brightness: light.0,
                    warmth: light.1,
                },
                idle_sleep,
                rotation,
                gestures: Gestures {
                    swipe_up: gestures.0,
                    swipe_down: gestures.1,
                    hold: gestures.2,
                },
                check_updates: flags[0],
                swap_page_buttons: flags[1],
                swap_menu_buttons: flags[2],
                sync_automatically: flags[3],
                sync_conflict,
                full_refresh_interval,
                books,
            }
        },
    ).boxed()
}

/// A state both stores accept: generated states that are refused are replaced
/// by their settings on a default layout, and by the default as a last resort.
fn valid_state() -> BoxedStrategy<ReaderState> {
    reader_state().prop_map(|state| {
        if accepted(&state) {
            return state;
        }
        let mut plain = ReaderState {
            font_family: state.font_family.clone(),
            idle_sleep: state.idle_sleep,
            rotation: state.rotation,
            gestures: state.gestures,
            full_refresh_interval: state.full_refresh_interval,
            ..ReaderState::default()
        };
        for (id, book) in &state.books {
            let mut book = book.clone();
            book.clippings.clear();
            book.typography = None;
            plain.books.insert(id.clone(), book);
        }
        assert!(accepted(&plain));
        plain
    }).boxed()
}

// ------------------------------------------------------------------- helpers

fn json(state: &ReaderState) -> serde_json::Value {
    serde_json::to_value(state).unwrap()
}

/// Whether the journal accepts the state, asked of a scratch journal.
fn accepted(state: &ReaderState) -> bool {
    let dir = tempfile::tempdir().unwrap();
    let (mut journal, _) = StateJournal::open(dir.path()).unwrap();
    journal.save(state).is_ok()
}

fn frame(generation: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = b"FEMTOST1".to_vec();
    bytes.extend_from_slice(&generation.to_le_bytes());
    bytes.extend_from_slice(payload);
    let digest = Sha256::digest(&bytes);
    bytes.extend_from_slice(&digest);
    bytes
}

fn slot(dir: &Path, index: usize) -> std::path::PathBuf {
    dir.join(format!("state{index}.bin"))
}

fn reopened(dir: &Path) -> serde_json::Value {
    json(&StateJournal::open(dir).unwrap().1)
}

// ---------------------------------------------------------------- properties

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    /// Whatever either store accepts comes back unchanged, both stores accept
    /// the same states, and a refused state leaves nothing behind.
    #[test]
    fn accepted_states_round_trip_and_refused_states_write_nothing(state in reader_state()) {
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("host").join("reader.json");
        let (mut journal, _) = StateJournal::open(dir.path()).unwrap();
        let saved = journal.save(&state);
        let host_saved = state.save(&host);
        prop_assert_eq!(saved.is_ok(), host_saved.is_ok(), "{:?} {:?}", saved, host_saved);
        if saved.is_ok() {
            prop_assert_eq!(reopened(dir.path()), json(&state));
            prop_assert_eq!(json(&ReaderState::load(&host).unwrap()), json(&state));
            // What was read back is itself storable and stable.
            let (mut again, back) = StateJournal::open(dir.path()).unwrap();
            again.save(&back).unwrap();
            prop_assert_eq!(reopened(dir.path()), json(&state));
            prop_assert_eq!(
                std::fs::read(slot(dir.path(), 0)).unwrap()[16..].len(),
                std::fs::read(slot(dir.path(), 1)).unwrap()[16..].len()
            );
        } else {
            prop_assert!(!slot(dir.path(), 0).exists() && !slot(dir.path(), 1).exists());
            prop_assert!(!host.exists());
            // A refusal is not a write failure: the journal stays usable.
            journal.save(&ReaderState::default()).unwrap();
            prop_assert_eq!(reopened(dir.path()), json(&ReaderState::default()));
        }
    }

    /// Reopening always yields the last state saved, however saves and
    /// reopenings interleave.
    #[test]
    fn the_journal_reopens_the_last_saved_state(
        states in prop::collection::vec((valid_state(), any::<bool>()), 1..7),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (mut journal, first) = StateJournal::open(dir.path()).unwrap();
        prop_assert_eq!(json(&first), json(&ReaderState::default()));
        for (index, (state, reopen)) in states.iter().enumerate() {
            journal.save(state).unwrap();
            // Saves alternate between the two slots.
            let written = std::fs::read(slot(dir.path(), index % 2)).unwrap();
            prop_assert_eq!(
                &written,
                &frame(index as u64 + 1, &serde_json::to_vec(state).unwrap())
            );
            if *reopen {
                let (next, back) = StateJournal::open(dir.path()).unwrap();
                prop_assert_eq!(json(&back), json(state));
                journal = next;
            }
        }
        prop_assert_eq!(reopened(dir.path()), json(&states.last().unwrap().0));
    }

    /// Power loss at any byte of a save after the first leaves the previous
    /// state, and the journal keeps working from there.
    #[test]
    fn a_torn_save_recovers_the_previous_state(
        states in prop::collection::vec(valid_state(), 2..5),
        cuts in prop::collection::vec(any::<prop::sample::Index>(), 1..12),
        keep_old_tail in any::<bool>(),
        after in valid_state(),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let (mut journal, _) = StateJournal::open(dir.path()).unwrap();
        let (last, before) = states.split_last().unwrap();
        for state in before {
            journal.save(state).unwrap();
        }
        let target = slot(dir.path(), before.len() % 2);
        let old = std::fs::read(&target).unwrap_or_default();
        journal.save(last).unwrap();
        let new = std::fs::read(&target).unwrap();
        let other = std::fs::read(slot(dir.path(), 1 - before.len() % 2)).unwrap();
        let mut offsets: Vec<usize> = cuts.iter().map(|cut| cut.index(new.len())).collect();
        offsets.extend([0, 1, 8, 15, 16, 47, 48, new.len() - 32, new.len() - 1]);
        for cut in offsets.into_iter().filter(|cut| *cut < new.len()) {
            // Either the file was truncated first, or the old bytes beyond
            // the written prefix are still there.
            let mut torn = new[..cut].to_vec();
            if keep_old_tail && old.len() > cut {
                torn.extend_from_slice(&old[cut..]);
            }
            if torn == new {
                continue;
            }
            std::fs::write(&target, &torn).unwrap();
            let (mut recovered, state) = StateJournal::open(dir.path())
                .map_err(|error| TestCaseError::fail(format!("cut {cut}: {error}")))?;
            prop_assert_eq!(json(&state), json(before.last().unwrap()), "cut {}", cut);
            // Recovery never touches the surviving slot.
            prop_assert_eq!(&std::fs::read(slot(dir.path(), 1 - before.len() % 2)).unwrap(), &other);
            recovered.save(&after).unwrap();
            prop_assert_eq!(reopened(dir.path()), json(&after));
            // The next save went to the torn slot, not over the survivor.
            prop_assert_eq!(&std::fs::read(slot(dir.path(), 1 - before.len() % 2)).unwrap(), &other);
        }
    }
}

/// What a slot file holds in the arbitrary-contents property.
#[derive(Clone, Debug)]
enum SlotContent {
    Missing,
    /// A complete frame of a storable state.
    Valid(u64, Box<ReaderState>),
    /// A complete frame whose payload the journal may or may not accept.
    Framed(u64, Vec<u8>),
    Garbage(Vec<u8>),
}

fn generation() -> impl Strategy<Value = u64> {
    prop_oneof![
        6 => 1u64..4,
        1 => Just(0u64),
        1 => Just(u64::MAX),
        1 => Just(u64::MAX - 1),
        1 => any::<u64>(),
    ]
}

fn slot_content() -> BoxedStrategy<SlotContent> {
    prop_oneof![
        2 => Just(SlotContent::Missing),
        6 => (generation(), valid_state()).prop_map(|(g, s)| SlotContent::Valid(g, Box::new(s))),
        2 => (generation(), reader_state())
            .prop_map(|(g, s)| SlotContent::Framed(g, serde_json::to_vec(&s).unwrap_or_default())),
        1 => (generation(), prop::collection::vec(any::<u8>(), 0..64))
            .prop_map(|(g, bytes)| SlotContent::Framed(g, bytes)),
        2 => prop::collection::vec(any::<u8>(), 0..128).prop_map(SlotContent::Garbage),
        // A valid frame with one flipped bit or a cut.
        2 => (valid_state(), any::<prop::sample::Index>(), 0u8..8, any::<bool>()).prop_map(
            |(state, at, bit, cut)| {
                let mut bytes = frame(1, &serde_json::to_vec(&state).unwrap());
                let at = at.index(bytes.len());
                if cut {
                    bytes.truncate(at);
                } else {
                    bytes[at] ^= 1 << bit;
                }
                SlotContent::Garbage(bytes)
            }
        ),
    ].boxed()
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 600, ..ProptestConfig::default() })]

    /// Opening arbitrary slot files never panics, selects the newest storable
    /// frame, and yields a state that can be saved and read again.
    #[test]
    fn arbitrary_slot_contents_open_to_the_newest_valid_frame_or_fail(
        contents in [slot_content(), slot_content()],
        next in valid_state(),
    ) {
        let dir = tempfile::tempdir().unwrap();
        // (generation, state) of slots certain to be valid, and whether any
        // slot is certain to be invalid or of unknown validity.
        let mut valid = Vec::new();
        let mut unknown = false;
        for (index, content) in contents.iter().enumerate() {
            let bytes = match content {
                SlotContent::Missing => continue,
                SlotContent::Valid(generation, state) => {
                    if *generation == 0 {
                        unknown = true;
                    } else {
                        valid.push((*generation, json(state)));
                    }
                    frame(*generation, &serde_json::to_vec(state).unwrap())
                }
                SlotContent::Framed(generation, payload) => {
                    unknown = true;
                    frame(*generation, payload)
                }
                SlotContent::Garbage(bytes) => {
                    unknown = true;
                    bytes.clone()
                }
            };
            std::fs::write(slot(dir.path(), index), bytes).unwrap();
        }
        let before = [0, 1].map(|index| std::fs::read(slot(dir.path(), index)).ok());
        let opened = StateJournal::open(dir.path());
        // Opening is read-only.
        prop_assert_eq!(&before, &[0, 1].map(|index| std::fs::read(slot(dir.path(), index)).ok()));
        valid.sort_by_key(|(generation, _)| *generation);
        if !unknown {
            match valid.as_slice() {
                [] => prop_assert_eq!(json(&opened.as_ref().unwrap().1), json(&ReaderState::default())),
                [(_, only)] => prop_assert_eq!(&json(&opened.as_ref().unwrap().1), only),
                [(low, _), (high, newest)] if low != high => {
                    prop_assert_eq!(&json(&opened.as_ref().unwrap().1), newest)
                }
                _ => prop_assert!(opened.is_err()),
            }
        }
        let Ok((mut journal, state)) = opened else { return Ok(()) };
        // Never older than a slot known to be valid, unless something newer
        // of unknown validity was accepted instead.
        if let [.., (_, newest)] = valid.as_slice()
            && !unknown
        {
            prop_assert_eq!(&json(&state), newest);
        }
        prop_assert!(accepted(&state), "opened a state that cannot be saved");
        let highest = contents.iter().filter_map(|content| match content {
            SlotContent::Valid(generation, _) | SlotContent::Framed(generation, _) => Some(*generation),
            _ => None,
        }).max();
        match journal.save(&next) {
            Ok(()) => prop_assert_eq!(reopened(dir.path()), json(&next)),
            // The generation counter is exhausted only by a crafted file.
            Err(error) => prop_assert!(highest == Some(u64::MAX), "{}", error),
        }
    }
}

// ------------------------------------------------------ near-valid JSON input

#[derive(Clone, Debug)]
enum Edit {
    Replace(serde_json::Value),
    Remove,
    Insert(String, serde_json::Value),
}

fn scalar() -> impl Strategy<Value = serde_json::Value> {
    use serde_json::json;
    prop_oneof![
        Just(json!(null)),
        Just(json!(true)),
        Just(json!(0)),
        Just(json!(1)),
        Just(json!(-1)),
        Just(json!(255)),
        Just(json!(256)),
        Just(json!(u32::MAX)),
        Just(json!(u32::MAX as u64 + 1)),
        Just(json!(u64::MAX)),
        Just(json!(0.5)),
        Just(json!(-0.0)),
        Just(json!(1e39)),
        Just(json!(f64::MAX)),
        Just(json!(1e-320)),
        Just(json!("")),
        Just(json!("menu")),
        Just(json!([])),
        Just(json!({})),
        any::<i64>().prop_map(|n| json!(n)),
        any::<f64>().prop_filter("finite", |n| n.is_finite()).prop_map(|n| json!(n)),
        text().prop_map(|s| json!(s)),
    ]
}

fn edit() -> impl Strategy<Value = Edit> {
    prop_oneof![
        6 => scalar().prop_map(Edit::Replace),
        2 => Just(Edit::Remove),
        1 => ("[a-z_]{1,8}", scalar()).prop_map(|(key, value)| Edit::Insert(key, value)),
    ]
}

/// Applies the edit at the place the path selects, descending while it can.
fn apply(value: &mut serde_json::Value, path: &[prop::sample::Index], edit: &Edit) {
    use serde_json::Value;
    let Some((first, rest)) = path.split_first() else {
        if let Edit::Replace(replacement) = edit {
            *value = replacement.clone();
        }
        return;
    };
    match value {
        Value::Object(map) if !map.is_empty() => {
            let key = map.keys().nth(first.index(map.len())).unwrap().clone();
            let leaf = rest.is_empty() || !matches!(map[&key], Value::Object(_) | Value::Array(_));
            match (leaf, edit) {
                (false, _) => apply(map.get_mut(&key).unwrap(), rest, edit),
                (true, Edit::Replace(replacement)) => map[&key] = replacement.clone(),
                (true, Edit::Remove) => drop(map.remove(&key)),
                (true, Edit::Insert(name, inserted)) => drop(map.insert(name.clone(), inserted.clone())),
            }
        }
        Value::Array(items) if !items.is_empty() => {
            let index = first.index(items.len());
            let leaf = rest.is_empty() || !matches!(items[index], Value::Object(_) | Value::Array(_));
            match (leaf, edit) {
                (false, _) => apply(&mut items[index], rest, edit),
                (true, Edit::Replace(replacement)) => items[index] = replacement.clone(),
                (true, Edit::Remove) => drop(items.remove(index)),
                (true, Edit::Insert(_, inserted)) => items.insert(index, inserted.clone()),
            }
        }
        other => {
            if let Edit::Replace(replacement) = edit {
                *other = replacement.clone();
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 1500, ..ProptestConfig::default() })]

    /// A state file that differs from a valid one in a few places is either
    /// refused by both stores or read as a state that saves and reads back.
    #[test]
    fn edited_state_files_are_refused_or_read_as_storable_states(
        state in valid_state(),
        edits in prop::collection::vec(
            (prop::collection::vec(any::<prop::sample::Index>(), 1..6), edit()),
            1..3,
        ),
    ) {
        let mut value = json(&state);
        for (path, edit) in &edits {
            apply(&mut value, path, edit);
        }
        let bytes = serde_json::to_vec(&value).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("reader.json");
        std::fs::write(&host, &bytes).unwrap();
        std::fs::write(slot(dir.path(), 0), frame(1, &bytes)).unwrap();
        let loaded = ReaderState::load(&host);
        let opened = StateJournal::open(dir.path());
        prop_assert_eq!(loaded.is_ok(), opened.is_ok(), "{:?} {:?}", loaded.as_ref().err(), opened.as_ref().err());
        let Ok(loaded) = loaded else {
            // A refused file is left as it was.
            prop_assert_eq!(std::fs::read(&host).unwrap(), bytes);
            return Ok(());
        };
        let (mut journal, opened) = opened.unwrap();
        prop_assert_eq!(json(&loaded), json(&opened));
        loaded.save(&host).unwrap();
        prop_assert_eq!(json(&ReaderState::load(&host).unwrap()), json(&loaded));
        journal.save(&opened).unwrap();
        prop_assert_eq!(reopened(dir.path()), json(&loaded));
    }

    /// Arbitrary bytes as a host state file never panic the loader.
    #[test]
    fn arbitrary_bytes_never_panic_the_loaders(bytes in prop_oneof![
        prop::collection::vec(any::<u8>(), 0..200),
        "\\PC{0,200}".prop_map(String::into_bytes),
        valid_state().prop_flat_map(|state| {
            let bytes = serde_json::to_vec(&state).unwrap();
            let len = bytes.len();
            (Just(bytes), 0..len, any::<u8>(), any::<bool>())
        }).prop_map(|(mut bytes, at, byte, cut)| {
            if cut { bytes.truncate(at) } else { bytes[at] = byte }
            bytes
        }),
    ]) {
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("reader.json");
        std::fs::write(&host, &bytes).unwrap();
        std::fs::write(slot(dir.path(), 1), frame(7, &bytes)).unwrap();
        let loaded = ReaderState::load(&host);
        let opened = StateJournal::open(dir.path());
        prop_assert_eq!(loaded.is_ok(), opened.is_ok());
        if let Ok(state) = loaded {
            prop_assert!(accepted(&state));
            prop_assert_eq!(json(&state), json(&opened.unwrap().1));
        }
    }
}

// --------------------------------------------------------------- calibration

/// Not a property: reports what the generators produce.
#[test]
fn generator_statistics() {
    use proptest::strategy::ValueTree;
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    let (mut ok, mut books, mut clips, mut typo, mut bytes) = (0, 0, 0, 0, 0);
    let total = 1000;
    for _ in 0..total {
        let state = reader_state().new_tree(&mut runner).unwrap().current();
        if accepted(&state) {
            ok += 1;
            books += state.books.len();
            clips += state.books.values().map(|b| b.clippings.len()).sum::<usize>();
            typo += state.books.values().filter(|b| b.typography.is_some()).count();
            bytes += serde_json::to_vec(&state).unwrap().len();
        }
    }
    println!(
        "accepted {ok}/{total}; in accepted states: {books} books, {clips} clippings, \
         {typo} book typographies, {} mean bytes",
        bytes / ok.max(1)
    );
    assert!(ok * 4 > total, "too few generated states are storable");
    assert!(clips > 50 && typo > 50);
}

// ------------------------------------------------------- deterministic cases

fn sample_state(byte: usize) -> ReaderState {
    let mut state = ReaderState::default();
    let position = Locator {
        chapter: 1,
        block: 2,
        byte,
    };
    state.set_position(IDS[1], position, 7);
    state.toggle_bookmark(IDS[1], position, "caf\u{e9} \"quoted\"".into());
    state
}

/// Every byte offset of a second and a third save, with and without the old
/// bytes of the slot left beyond the written prefix.
#[test]
fn a_save_torn_at_any_byte_after_the_first_recovers_the_previous_state() {
    for saves in [2usize, 3] {
        let dir = tempfile::tempdir().unwrap();
        let (mut journal, _) = StateJournal::open(dir.path()).unwrap();
        for byte in 0..saves - 1 {
            journal.save(&sample_state(byte * 1000)).unwrap();
        }
        let previous = sample_state((saves - 2) * 1000);
        let target = slot(dir.path(), (saves - 1) % 2);
        let old = std::fs::read(&target).unwrap_or_default();
        let new = frame(
            saves as u64,
            &serde_json::to_vec(&sample_state(123_456)).unwrap(),
        );
        for cut in 0..new.len() {
            for keep_old_tail in [false, true] {
                let mut torn = new[..cut].to_vec();
                if keep_old_tail && old.len() > cut {
                    torn.extend_from_slice(&old[cut..]);
                }
                std::fs::write(&target, &torn).unwrap();
                let (_, state) = StateJournal::open(dir.path())
                    .unwrap_or_else(|error| panic!("{saves} saves, cut {cut}: {error}"));
                assert_eq!(json(&state), json(&previous), "{saves} saves, cut {cut}");
            }
        }
        std::fs::write(&target, &new).unwrap();
        assert_eq!(reopened(dir.path()), json(&sample_state(123_456)));
    }
}

/// The state before the first save is the empty default, so a first save cut
/// short by power loss should leave exactly that. Instead the lone torn slot
/// makes every later open fail; the firmware then runs with saving (and
/// sleep) disabled on every boot until the file is removed by hand.
/// docs/picco-firmware.md documents "If neither existing slot is valid,
/// saving is disabled", so this is the documented rule applied to a case
/// where no state can have been lost.
#[test]
#[ignore = "bug: power loss during the first-ever journal save leaves a journal that never opens again"]
fn a_torn_first_save_recovers_the_empty_state() {
    let dir = tempfile::tempdir().unwrap();
    let state = sample_state(0);
    let new = frame(1, &serde_json::to_vec(&state).unwrap());
    for cut in 0..new.len() {
        std::fs::write(slot(dir.path(), 0), &new[..cut]).unwrap();
        let (mut journal, recovered) = StateJournal::open(dir.path())
            .unwrap_or_else(|error| panic!("first save cut at byte {cut}: {error}"));
        assert_eq!(json(&recovered), json(&ReaderState::default()));
        journal.save(&state).unwrap();
        assert_eq!(reopened(dir.path()), json(&state));
    }
}
