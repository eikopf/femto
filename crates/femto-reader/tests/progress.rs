use femto_reader::{
    document::Document,
    layout::Locator,
    progress::{MAX_PROGRESS_BYTES, Outcome, Progress},
    state::ReaderState,
};

#[test]
fn progress_merge_preserves_other_state_and_detects_divergence_without_clock_ordering() {
    let mut doc = Document::from_text("Title", "café reading progress").unwrap();
    let id = doc.metadata.id.clone();
    let mut state = ReaderState::default();
    state.set_position(&id, Locator::default(), 100);
    state.toggle_bookmark(&id, Locator::default(), "Keep".into());
    state.set_position("another book", Locator::default(), 42);
    let baseline = Progress::from_state(&state, &id).unwrap();
    let remote = Progress {
        position: Locator {
            byte: 6,
            ..Default::default()
        },
        recorded_at: 1,
        ..baseline.clone()
    };
    assert!(matches!(
        remote.apply(None, &mut state, &mut doc, 200).unwrap(),
        Outcome::Conflict { .. }
    ));
    assert_eq!(Progress::from_state(&state, &id), Some(baseline.clone()));
    assert_eq!(
        remote
            .apply(Some(&baseline), &mut state, &mut doc, 200)
            .unwrap(),
        Outcome::Applied
    );
    assert_eq!(state.books[&id].position, remote.position);
    assert_eq!(state.books[&id].last_opened, 200);
    assert_eq!(state.books[&id].bookmarks[0].label, "Keep");
    assert_eq!(state.books["another book"].last_opened, 42);
    assert_eq!(
        remote
            .apply(Some(&baseline), &mut state, &mut doc, 300)
            .unwrap(),
        Outcome::Unchanged
    );
    assert_eq!(state.books[&id].last_opened, 200);
    let newer = Progress {
        position: Locator {
            byte: 7,
            ..Default::default()
        },
        recorded_at: u64::MAX,
        ..remote.clone()
    };
    assert!(matches!(
        newer
            .apply(Some(&baseline), &mut state, &mut doc, 400)
            .unwrap(),
        Outcome::Conflict { .. }
    ));
    let mut empty = ReaderState::default();
    assert!(
        remote
            .apply(Some(&baseline), &mut empty, &mut doc, 400)
            .is_err()
    );
    assert!(empty.books.is_empty());
    assert_eq!(
        remote.apply(None, &mut empty, &mut doc, 400).unwrap(),
        Outcome::Applied
    );
}
#[test]
fn progress_checks_exact_contents_utf8_and_resource_bounds_transactionally() {
    let mut doc = Document::from_text("Title", "café").unwrap();
    let mut state = ReaderState::default();
    let good = Progress {
        version: 1,
        book_id: doc.metadata.id.clone(),
        position: Locator::default(),
        recorded_at: 0,
    };
    for position in [
        Locator {
            chapter: 1,
            ..Default::default()
        },
        Locator {
            block: 1,
            ..Default::default()
        },
        Locator {
            byte: 4,
            ..Default::default()
        },
        Locator {
            byte: usize::MAX,
            ..Default::default()
        },
    ] {
        let bad = Progress {
            position,
            ..good.clone()
        };
        assert!(bad.apply(None, &mut state, &mut doc, 1).is_err());
        assert!(state.books.is_empty());
    }
    let mut other = Document::from_text("Title", "cafe").unwrap();
    assert!(good.validate_document(&mut other).is_err());
    let bytes = serde_json::to_vec(&good).unwrap();
    assert_eq!(Progress::decode(&bytes).unwrap(), good);
    assert!(Progress::decode(&vec![b' '; MAX_PROGRESS_BYTES + 1]).is_err());
    for (field, value) in [
        ("version", serde_json::json!(2)),
        ("book_id", serde_json::json!("bad")),
        ("unknown", serde_json::json!(true)),
    ] {
        let mut value_json = serde_json::to_value(&good).unwrap();
        value_json[field] = value;
        assert!(Progress::decode(&serde_json::to_vec(&value_json).unwrap()).is_err());
    }
}

#[test]
fn three_way_sync_uses_positions_and_never_resurrects_missing_baseline_data() {
    use femto_reader::progress::{SyncAction::*, sync_action};
    let a = Some(Locator::default());
    let b = Some(Locator {
        byte: 1,
        ..Default::default()
    });
    let c = Some(Locator {
        byte: 2,
        ..Default::default()
    });
    for (base, local, remote, expected) in [
        (None, None, None, Empty),
        (None, a, None, Create),
        (None, None, a, Download),
        (None, a, a, Converged),
        (None, a, b, Conflict),
        (a, a, b, Download),
        (a, b, a, Upload),
        (a, b, b, Converged),
        (a, b, c, Conflict),
        (a, None, a, Conflict),
        (a, a, None, Conflict),
        (a, None, None, Conflict),
        (a, a, a, Converged),
    ] {
        assert_eq!(sync_action(base, local, remote), expected);
    }
}
