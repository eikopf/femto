use femto_reader::{
    clipping::{Clipping, MAX_CLIPPINGS_PER_BOOK},
    document::Document,
    journal::StateJournal,
    layout::Locator,
    state::ReaderState,
};

fn book() -> Document {
    Document::from_text("Excerpt", "Ae\u{301} 👩‍💻 שלום\n\nAnother paragraph.").unwrap()
}
fn capture() -> Clipping {
    Clipping::capture(
        &mut book(),
        Locator {
            chapter: 0,
            block: 0,
            byte: 1,
        }..Locator {
            chapter: 0,
            block: 1,
            byte: 7,
        },
        42,
    )
    .unwrap()
}

#[test]
fn removal_follows_the_confirmed_record_after_reordering() {
    let expected = capture();
    let mut range = expected.range.clone();
    range.start.byte = 0;
    let other = Clipping::capture(&mut book(), range, 43).unwrap();
    let mut state = ReaderState::default();
    state.add_clipping(expected.clone()).unwrap();
    state.add_clipping(other.clone()).unwrap();
    state
        .books
        .get_mut(&expected.book_id)
        .unwrap()
        .clippings
        .swap(0, 1);
    state.remove_clipping(&expected).unwrap();
    assert_eq!(state.books[&expected.book_id].clippings, [other]);
    let before = serde_json::to_value(&state).unwrap();
    assert!(state.remove_clipping(&expected).is_err());
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
}

#[test]
fn capture_roundtrips_through_both_stores_without_changing_progress() {
    let clipping = capture();
    assert_eq!(clipping.text, "e\u{301} 👩‍💻 שלום\n\nAnother");
    clipping.validate_document(&mut book()).unwrap();
    let mut state = ReaderState::default();
    state.set_position(&clipping.book_id, clipping.range.end, 17);
    assert!(state.add_clipping(clipping.clone()).unwrap());
    let before = serde_json::to_value(&state).unwrap();
    let mut repeated = clipping.clone();
    repeated.recorded_at = 99;
    assert!(!state.add_clipping(repeated).unwrap());
    assert_eq!(serde_json::to_value(&state).unwrap(), before);
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join("state.json");
    state.save(&host).unwrap();
    let (mut journal, _) = StateJournal::open(root.path()).unwrap();
    journal.save(&state).unwrap();
    for restored in [
        ReaderState::load(host).unwrap(),
        StateJournal::open(root.path()).unwrap().1,
    ] {
        let saved = &restored.books[&clipping.book_id];
        assert_eq!(saved.clippings, std::slice::from_ref(&clipping));
        assert_eq!(saved.position, clipping.range.end);
        assert_eq!(saved.last_opened, 17);
        saved.clippings[0].validate_document(&mut book()).unwrap();
    }
}

#[test]
fn changed_book_forged_text_and_split_graphemes_are_rejected() {
    let mut clipping = capture();
    let mut changed = Document::from_text("Excerpt", "Different contents.").unwrap();
    assert!(clipping.validate_document(&mut changed).is_err());
    clipping.text.push('!');
    assert!(clipping.validate_document(&mut book()).is_err());
    clipping = capture();
    clipping.range.start.byte = 2;
    assert!(clipping.validate_document(&mut book()).is_err());
    assert!(Clipping::capture(&mut book(), clipping.range, 0).is_err());
}

#[test]
fn invalid_records_and_conflicting_duplicates_preserve_saved_state() {
    let clipping = capture();
    let mut state = ReaderState::default();
    state.add_clipping(clipping.clone()).unwrap();
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join("state.json");
    state.save(&host).unwrap();
    let good = std::fs::read(&host).unwrap();
    let (mut journal, _) = StateJournal::open(root.path()).unwrap();
    journal.save(&state).unwrap();
    for kind in 0..7 {
        let mut invalid = clipping.clone();
        match kind {
            0 => invalid.version = 2,
            1 => invalid.book_id = "wrong".into(),
            2 => invalid.range.end = invalid.range.start,
            3 => invalid.range.end.chapter = 8,
            4 => invalid.text.clear(),
            5 => invalid.text = "x".repeat(16 * 1024 + 1),
            _ => invalid.text.push('!'),
        }
        assert!(state.add_clipping(invalid.clone()).is_err());
        let mut bad = state.clone();
        if kind == 6 {
            bad.books
                .get_mut(&clipping.book_id)
                .unwrap()
                .clippings
                .push(invalid);
        } else {
            bad.books.get_mut(&clipping.book_id).unwrap().clippings[0] = invalid;
        }
        assert!(bad.save(&host).is_err());
        assert!(journal.save(&bad).is_err());
        assert_eq!(std::fs::read(&host).unwrap(), good);
        let corrupt = root.path().join("corrupt.json");
        std::fs::write(&corrupt, serde_json::to_vec(&bad).unwrap()).unwrap();
        assert!(ReaderState::load(corrupt).is_err());
    }
    assert_eq!(
        StateJournal::open(root.path()).unwrap().1.books[&clipping.book_id].clippings,
        [clipping]
    );
}

#[test]
fn clipping_count_is_bounded_and_legacy_state_defaults_to_empty() {
    let mut state = ReaderState::default();
    let original = capture();
    for index in 0..MAX_CLIPPINGS_PER_BOOK {
        let mut clipping = original.clone();
        clipping.range.start.byte = index;
        clipping.range.end.block = 2;
        assert!(state.add_clipping(clipping).unwrap());
    }
    let before = serde_json::to_vec(&state).unwrap();
    let mut excess = original.clone();
    excess.range.start.byte = MAX_CLIPPINGS_PER_BOOK;
    excess.range.end.block = 2;
    assert!(state.add_clipping(excess).is_err());
    assert_eq!(serde_json::to_vec(&state).unwrap(), before);
    let mut legacy = ReaderState::default();
    legacy.set_position(&original.book_id, original.range.start, 1);
    let json = serde_json::to_value(&legacy).unwrap();
    assert!(json["books"][&original.book_id].get("clippings").is_none());
    let restored: ReaderState = serde_json::from_value(json).unwrap();
    assert!(restored.books[&original.book_id].clippings.is_empty());
}
