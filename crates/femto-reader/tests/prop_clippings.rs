//! Model test of the clipping store in reader state: additions, exact-record
//! removal and highlighting against a list per book, with the documented
//! duplicate, conflict and capacity rules, and a state that stays storable.
use femto_reader::{
    clipping::{Clipping, MAX_CLIPPINGS_PER_BOOK},
    journal::StateJournal,
    layout::Locator,
    state::ReaderState,
};
use proptest::prelude::*;
use std::collections::BTreeMap;

const BOOKS: [&str; 2] = [
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
];

#[derive(Clone, Debug)]
enum Op {
    Add(Clipping),
    /// Adds this many clippings with ranges of their own to one book.
    Fill(usize, usize),
    /// Removes or highlights a record chosen from the model, possibly after
    /// changing one field so that it is no longer the stored record.
    Remove(usize, prop::sample::Index, Option<Change>),
    Highlight(usize, prop::sample::Index, Option<Change>, bool),
    Bookmark(usize, usize),
    Position(usize, usize, u64),
}

#[derive(Clone, Copy, Debug)]
enum Change {
    Text,
    Time,
    Range,
    Book,
    Flag,
}

fn clipping() -> impl Strategy<Value = Clipping> {
    (
        0usize..2,
        // Few ranges, so that repeats and conflicts are common.
        (0usize..3, 0usize..3, 0usize..10, 0usize..3),
        prop::sample::select(vec!["one", "two", "", "caf\u{e9}"]),
        0u64..3,
        prop_oneof![12 => Just(1u32), 1 => Just(0u32), 1 => Just(2u32)],
        prop_oneof![12 => Just(None), 1 => Just(Some("short")), 1 => Just(Some(BOOKS[1]))],
        prop::sample::select(vec![16_383usize, 16_384, 16_385]),
        prop_oneof![9 => Just(false), 1 => Just(true)],
    )
        .prop_map(|(book, (chapter, block, span, step), text, recorded_at, version, id, long, use_long)| {
            let start = Locator { chapter, block, byte: 0 };
            let end = Locator { chapter: chapter + span, block: block + step, byte: 0 };
            Clipping {
                version,
                book_id: id.unwrap_or(BOOKS[book]).into(),
                range: start..end,
                text: if use_long { "x".repeat(long) } else { text.into() },
                recorded_at,
                highlighted: false,
            }
        })
}

fn change() -> impl Strategy<Value = Option<Change>> {
    prop_oneof![
        3 => Just(None),
        1 => prop::sample::select(vec![Change::Text, Change::Time, Change::Range, Change::Book, Change::Flag])
            .prop_map(Some),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        10 => clipping().prop_map(Op::Add),
        1 => (0usize..2, 100usize..140).prop_map(|(book, count)| Op::Fill(book, count)),
        4 => (0usize..2, any::<prop::sample::Index>(), change())
            .prop_map(|(book, at, change)| Op::Remove(book, at, change)),
        4 => (0usize..2, any::<prop::sample::Index>(), change(), any::<bool>())
            .prop_map(|(book, at, change, on)| Op::Highlight(book, at, change, on)),
        1 => (0usize..2, 0usize..3).prop_map(|(book, byte)| Op::Bookmark(book, byte)),
        1 => (0usize..2, 0usize..3, 1u64..9).prop_map(|(book, byte, now)| Op::Position(book, byte, now)),
    ]
}

/// The documented validity of a clipping's own fields.
fn well_formed(clipping: &Clipping) -> bool {
    clipping.version == 1
        && clipping.book_id.len() == 64
        && clipping.range.start < clipping.range.end
        && clipping.range.end.chapter - clipping.range.start.chapter < 8
        && !clipping.text.is_empty()
        && clipping.text.len() <= 16 * 1024
}

fn add(model: &mut BTreeMap<String, Vec<Clipping>>, clipping: &Clipping) -> Result<bool, ()> {
    if !well_formed(clipping) {
        return Err(());
    }
    let book = model.entry(clipping.book_id.clone()).or_default();
    if let Some(existing) = book.iter().find(|other| other.range == clipping.range) {
        return if existing.text == clipping.text { Ok(false) } else { Err(()) };
    }
    if book.len() >= MAX_CLIPPINGS_PER_BOOK {
        return Err(());
    }
    book.push(clipping.clone());
    Ok(true)
}

fn changed(mut record: Clipping, change: Option<Change>) -> Clipping {
    match change {
        None => {}
        Some(Change::Text) => record.text.push('!'),
        Some(Change::Time) => record.recorded_at += 1,
        Some(Change::Range) => record.range.end.byte += 1,
        Some(Change::Book) => {
            record.book_id = if record.book_id == BOOKS[0] { BOOKS[1] } else { BOOKS[0] }.into()
        }
        Some(Change::Flag) => record.highlighted = !record.highlighted,
    }
    record
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 300, max_shrink_iters: 300, ..ProptestConfig::default() })]

    #[test]
    fn the_clipping_store_agrees_with_a_list_per_book(ops in prop::collection::vec(op(), 1..40)) {
        let mut state = ReaderState::default();
        let mut model: BTreeMap<String, Vec<Clipping>> = BTreeMap::new();
        for op in &ops {
            // Reading position, recency and bookmarks of each book before.
            let reading = |state: &ReaderState| -> Vec<_> {
                state.books.iter().map(|(id, book)| {
                    (id.clone(), book.position, book.last_opened, book.bookmarks.len())
                }).filter(|(_, position, opened, marks)| (*position, *opened, *marks) != (Default::default(), 0, 0)).collect()
            };
            let before = reading(&state);
            let mut clipping_only = true;
            match op {
                Op::Add(clipping) => {
                    let expected = add(&mut model, clipping);
                    prop_assert_eq!(state.add_clipping(clipping.clone()).map_err(|_| ()), expected, "{:?}", clipping);
                }
                Op::Fill(book, count) => {
                    for index in 0..*count {
                        let start = Locator { chapter: 100 + index, block: 0, byte: 0 };
                        let clipping = Clipping {
                            version: 1,
                            book_id: BOOKS[*book].into(),
                            range: start..Locator { byte: 1, ..start },
                            text: "filler".into(),
                            recorded_at: index as u64,
                            highlighted: false,
                        };
                        let expected = add(&mut model, &clipping);
                        prop_assert_eq!(state.add_clipping(clipping).map_err(|_| ()), expected, "fill {}", index);
                    }
                }
                Op::Remove(book, at, change) => {
                    let list = model.entry(BOOKS[*book].into()).or_default();
                    if list.is_empty() { continue }
                    let index = at.index(list.len());
                    let target = changed(list[index].clone(), *change);
                    // A changed record may happen to be the other book's.
                    let list = model.entry(target.book_id.clone()).or_default();
                    let stored = list.iter().position(|record| *record == target);
                    let result = state.remove_clipping(&target);
                    prop_assert_eq!(result.is_ok(), stored.is_some(), "{:?}", change);
                    if let Some(stored) = stored {
                        list.remove(stored);
                    }
                }
                Op::Highlight(book, at, change, on) => {
                    let list = model.entry(BOOKS[*book].into()).or_default();
                    if list.is_empty() { continue }
                    let index = at.index(list.len());
                    let target = changed(list[index].clone(), *change);
                    let list = model.entry(target.book_id.clone()).or_default();
                    let stored = list.iter().position(|record| *record == target);
                    let result = state.set_clipping_highlight(&target, *on);
                    prop_assert_eq!(result.is_ok(), stored.is_some(), "{:?}", change);
                    if let Some(stored) = stored {
                        list[stored].highlighted = *on;
                    }
                }
                Op::Bookmark(book, byte) => {
                    clipping_only = false;
                    state.toggle_bookmark(BOOKS[*book], Locator { chapter: 0, block: 0, byte: *byte }, "mark".into());
                }
                Op::Position(book, byte, now) => {
                    clipping_only = false;
                    state.set_position(BOOKS[*book], Locator { chapter: 0, block: 0, byte: *byte }, *now);
                }
            }
            if clipping_only {
                prop_assert_eq!(reading(&state), before);
            }
            for (id, expected) in &model {
                let stored = state.books.get(id).map(|book| book.clippings.clone()).unwrap_or_default();
                prop_assert_eq!(&stored, expected);
            }
        }
        // The state is storable and comes back with the same excerpts.
        let dir = tempfile::tempdir().unwrap();
        let (mut journal, _) = StateJournal::open(dir.path()).unwrap();
        journal.save(&state).unwrap();
        let (_, back) = StateJournal::open(dir.path()).unwrap();
        for (id, expected) in &model {
            let stored = back.books.get(id).map(|book| book.clippings.clone()).unwrap_or_default();
            prop_assert_eq!(&stored, expected);
        }
        if std::env::var_os("PROP_STATS").is_some() {
            eprintln!("stats clippings {} full {}", model.values().map(Vec::len).sum::<usize>(),
                model.values().filter(|list| list.len() == MAX_CLIPPINGS_PER_BOOK).count());
        }
    }
}
