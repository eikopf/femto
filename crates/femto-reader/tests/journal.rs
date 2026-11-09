use femto_reader::{journal::StateJournal, state::ReaderState};

#[test]
fn newer_checksummed_but_invalid_state_falls_back_and_can_be_replaced() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let (mut journal, state) = StateJournal::open(dir.path()).unwrap();
    journal.save(&state).unwrap();
    let mut invalid = state.clone();
    invalid.version = 999;
    let mut bytes = b"FEMTOST1".to_vec();
    bytes.extend_from_slice(&2u64.to_le_bytes());
    bytes.extend_from_slice(&serde_json::to_vec(&invalid).unwrap());
    let checksum = Sha256::digest(&bytes);
    bytes.extend_from_slice(&checksum);
    std::fs::write(dir.path().join("state1.bin"), bytes).unwrap();
    let (mut reopened, mut recovered) = StateJournal::open(dir.path()).unwrap();
    assert_eq!(recovered.version, state.version);
    recovered.layout.font_size = 30.0;
    reopened.save(&recovered).unwrap();
    assert_eq!(
        StateJournal::open(dir.path()).unwrap().1.layout.font_size,
        30.0
    );
}

#[test]
fn journal_segments_preserve_the_existing_frame_bytes() {
    use sha2::{Digest, Sha256};
    let dir = tempfile::tempdir().unwrap();
    let (mut journal, mut state) = StateJournal::open(dir.path()).unwrap();
    for generation in 1u64..=3 {
        state.toggle_bookmark(
            "book",
            femto_reader::layout::Locator {
                chapter: 0,
                block: 0,
                byte: generation as usize,
            },
            "accent é and \"escape\"\n".into(),
        );
        let mut expected = b"FEMTOST1".to_vec();
        expected.extend_from_slice(&generation.to_le_bytes());
        expected.extend_from_slice(&serde_json::to_vec(&state).unwrap());
        let digest = Sha256::digest(&expected);
        expected.extend_from_slice(&digest);
        journal.save(&state).unwrap();
        assert_eq!(
            std::fs::read(
                dir.path()
                    .join(format!("state{}.bin", (generation - 1) % 2))
            )
            .unwrap(),
            expected
        );
        assert_eq!(
            StateJournal::open(dir.path()).unwrap().1.books["book"]
                .bookmarks
                .len(),
            generation as usize
        );
    }
}

#[test]
fn oversized_escaped_state_preserves_host_file_and_both_slots_and_allows_retry() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reader.json");
    let (mut journal, mut state) = StateJournal::open(dir.path()).unwrap();
    journal.save(&state).unwrap();
    state.layout.font_size = 26.0;
    journal.save(&state).unwrap();
    state.save(&path).unwrap();
    let files = [
        path.clone(),
        dir.path().join("state0.bin"),
        dir.path().join("state1.bin"),
    ];
    let before = files.each_ref().map(|file| std::fs::read(file).unwrap());
    // JSON escaping expands this one-MiB label beyond the four-MiB output limit.
    state.toggle_bookmark("book", Default::default(), "\0".repeat(1024 * 1024));
    assert!(matches!(
        state.save(&path),
        Err(femto_reader::Error::Limit("state file"))
    ));
    assert!(matches!(
        journal.save(&state),
        Err(femto_reader::Error::Limit("state file"))
    ));
    for (file, expected) in files.iter().zip(before) {
        assert_eq!(std::fs::read(file).unwrap(), expected);
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 3);
    state.books.clear();
    state.layout.font_size = 28.0;
    journal.save(&state).unwrap();
    state.save(&path).unwrap();
    assert_eq!(
        StateJournal::open(dir.path()).unwrap().1.layout.font_size,
        28.0
    );
    assert_eq!(ReaderState::load(&path).unwrap().layout.font_size, 28.0);
}

#[test]
fn saved_numeric_settings_accept_integer_and_decimal_forms() {
    let state: ReaderState = serde_json::from_str(
        r#"{
        "version":1,"books":{},
        "layout":{"width":480,"height":800,"margin":24,"font_size":24,"line_height":1.35},
        "text_raster":{"stroke":0,"coverage_gamma":0.7}
    }"#,
    )
    .unwrap();
    assert_eq!(state.layout.font_size, 24.0);
    assert_eq!(state.text_raster.stroke, 0.0);
    assert_eq!(state.text_raster.coverage_gamma, 0.7);
    assert!(state.text_raster.hinting);
    let dir = tempfile::tempdir().unwrap();
    let (mut store, _) = StateJournal::open(dir.path()).unwrap();
    store.save(&state).unwrap();
    let (_, loaded) = StateJournal::open(dir.path()).unwrap();
    assert_eq!(loaded.layout.line_height, state.layout.line_height);
    let mut invalid = loaded;
    invalid.text_raster.stroke = f32::INFINITY;
    assert!(store.save(&invalid).is_err());
    assert_eq!(
        StateJournal::open(dir.path()).unwrap().1.text_raster.stroke,
        0.0
    );
}

#[test]
fn journal_reopens_newest_state_and_recovers_from_a_torn_next_write() {
    let dir = tempfile::tempdir().unwrap();
    let (mut journal, mut state) = StateJournal::open(dir.path()).unwrap();
    state.layout.font_size = 20.0;
    journal.save(&state).unwrap();
    state.layout.font_size = 30.0;
    journal.save(&state).unwrap();
    assert_eq!(
        StateJournal::open(dir.path()).unwrap().1.layout.font_size,
        30.0
    );
    let good = std::fs::read(dir.path().join("state1.bin")).unwrap();
    for length in [0, 7, 16, good.len() / 2, good.len() - 1] {
        std::fs::write(dir.path().join("state1.bin"), &good[..length]).unwrap();
        let (mut reopened, recovered) = StateJournal::open(dir.path()).unwrap();
        assert_eq!(recovered.layout.font_size, 20.0);
        reopened.save(&state).unwrap();
        assert_eq!(
            StateJournal::open(dir.path()).unwrap().1.layout.font_size,
            30.0
        );
    }
    let mut corrupted = good;
    corrupted[8] ^= 1;
    std::fs::write(dir.path().join("state1.bin"), corrupted).unwrap();
    assert_eq!(
        StateJournal::open(dir.path()).unwrap().1.layout.font_size,
        20.0
    );
}

#[test]
fn invalid_slots_and_failed_writes_are_not_silently_replaced() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("state0.bin"), b"corrupt").unwrap();
    assert!(StateJournal::open(dir.path()).is_err());
    std::fs::remove_file(dir.path().join("state0.bin")).unwrap();
    let (mut journal, state) = StateJournal::open(dir.path()).unwrap();
    journal.save(&state).unwrap();
    std::fs::create_dir(dir.path().join("state1.bin")).unwrap();
    assert!(journal.save(&state).is_err());
    std::fs::remove_dir(dir.path().join("state1.bin")).unwrap();
    assert!(journal.save(&state).is_err());
    let (mut reopened, _) = StateJournal::open(dir.path()).unwrap();
    reopened.save(&ReaderState::default()).unwrap();
}

#[test]
fn frontlight_state_defaults_and_invalid_ranges() {
    let mut value = serde_json::to_value(ReaderState::default()).unwrap();
    value.as_object_mut().unwrap().remove("frontlight");
    let mut state: ReaderState = serde_json::from_value(value).unwrap();
    assert_eq!(state.frontlight.brightness, 0);
    assert_eq!(state.frontlight.warmth, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reader.json");
    let (mut journal, _) = StateJournal::open(dir.path()).unwrap();
    state.frontlight.brightness = 10;
    state.frontlight.warmth = 9;
    state.save(&path).unwrap();
    journal.save(&state).unwrap();
    assert_eq!(
        ReaderState::load(&path).unwrap().frontlight,
        state.frontlight
    );
    assert_eq!(
        StateJournal::open(dir.path()).unwrap().1.frontlight,
        state.frontlight
    );
    for (brightness, warmth) in [(11, 0), (0, 10)] {
        state.frontlight.brightness = brightness;
        state.frontlight.warmth = warmth;
        assert!(state.save(&path).is_err());
        assert!(journal.save(&state).is_err());
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(ReaderState::load(&path).is_err());
    }
}

#[test]
fn idle_sleep_defaults_for_old_state_and_roundtrips_all_choices() {
    use femto_reader::state::IdleSleep;
    let mut old = serde_json::to_value(ReaderState::default()).unwrap();
    old.as_object_mut().unwrap().remove("idle_sleep");
    let mut state: ReaderState = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(state.idle_sleep, IdleSleep::TenMinutes);
    old["idle_sleep"] = serde_json::json!("invalid");
    assert!(serde_json::from_value::<ReaderState>(old).is_err());
    let root = tempfile::tempdir().unwrap();
    let (mut store, _) = StateJournal::open(root.path()).unwrap();
    for timeout in [
        IdleSleep::Off,
        IdleSleep::FiveMinutes,
        IdleSleep::TenMinutes,
        IdleSleep::ThirtyMinutes,
        IdleSleep::SixtyMinutes,
    ] {
        state.idle_sleep = timeout;
        store.save(&state).unwrap();
        assert_eq!(
            StateJournal::open(root.path()).unwrap().1.idle_sleep,
            timeout
        );
        let host = root.path().join("host.json");
        state.save(&host).unwrap();
        assert_eq!(ReaderState::load(host).unwrap().idle_sleep, timeout);
    }
}

#[test]
fn rotation_defaults_for_legacy_state_and_roundtrips_in_the_journal() {
    use femto_reader::state::Rotation;
    let mut value = serde_json::to_value(ReaderState::default()).unwrap();
    value.as_object_mut().unwrap().remove("rotation");
    let mut state: ReaderState = serde_json::from_value(value).unwrap();
    assert_eq!(state.rotation, Rotation::Portrait);
    let root = tempfile::tempdir().unwrap();
    let (mut store, _) = StateJournal::open(root.path()).unwrap();
    for rotation in [
        Rotation::Clockwise,
        Rotation::Inverted,
        Rotation::Counterclockwise,
        Rotation::Portrait,
    ] {
        state.rotation = rotation;
        store.save(&state).unwrap();
        assert_eq!(
            StateJournal::open(root.path()).unwrap().1.rotation,
            rotation
        );
    }
    let mut value = serde_json::to_value(state).unwrap();
    value["rotation"] = serde_json::json!("invalid");
    assert!(serde_json::from_value::<ReaderState>(value).is_err());
}

#[test]
fn obsolete_sync_metadata_is_discarded_without_losing_reading_state() {
    use femto_reader::{clipping::Clipping, document::Document, layout::Locator};
    use sha2::{Digest, Sha256};
    let mut document = Document::from_text("Book", "First paragraph.").unwrap();
    let id = document.metadata.id.clone();
    let start = Locator {
        chapter: 0,
        block: 0,
        byte: 0,
    };
    let end = Locator { byte: 5, ..start };
    let mut state = ReaderState::default();
    state.set_position(&id, end, 123);
    state.toggle_bookmark(&id, start, "Saved place".into());
    let mut clipping = Clipping::capture(&mut document, start..end, 122).unwrap();
    clipping.highlighted = true;
    state.add_clipping(clipping).unwrap();
    let expected: serde_json::Value =
        serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    let mut legacy = expected.clone();
    legacy["books"][&id]["sync"] = serde_json::json!({
        "source": "https://old.invalid/progress/book",
        "baseline": {"version": 1, "book_id": id, "position": end, "recorded_at": 123},
        "etag": "\"old-version\""
    });
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reader.json");
    std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let loaded = ReaderState::load(&path).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&serde_json::to_vec(&loaded).unwrap()).unwrap(),
        expected
    );
    loaded.save(&path).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved, expected);

    let mut slot = b"FEMTOST1".to_vec();
    slot.extend_from_slice(&1u64.to_le_bytes());
    slot.extend_from_slice(&serde_json::to_vec(&legacy).unwrap());
    slot.extend_from_slice(&Sha256::digest(&slot));
    std::fs::write(directory.path().join("state0.bin"), slot).unwrap();
    let (mut journal, recovered) = StateJournal::open(directory.path()).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&serde_json::to_vec(&recovered).unwrap())
            .unwrap(),
        expected
    );
    journal.save(&recovered).unwrap();
    let (_, reopened) = StateJournal::open(directory.path()).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&serde_json::to_vec(&reopened).unwrap())
            .unwrap(),
        expected
    );
}
