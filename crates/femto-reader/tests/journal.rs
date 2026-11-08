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
fn sync_checkpoints_default_for_old_state_and_validate_before_writes() {
    use femto_reader::{
        layout::Locator,
        progress::{Progress, SyncCheckpoint},
    };
    let id = "a".repeat(64);
    let mut state = ReaderState::default();
    state.set_position(&id, Locator::default(), 1);
    let mut legacy = serde_json::to_value(&state).unwrap();
    legacy["books"][&id].as_object_mut().unwrap().remove("sync");
    let mut state: ReaderState = serde_json::from_value(legacy).unwrap();
    assert!(state.books[&id].sync.is_none());
    let checkpoint = SyncCheckpoint {
        source: "https://peer.example/progress/book".into(),
        baseline: Progress::from_state(&state, &id).unwrap(),
        etag: "\"saved\"".into(),
    };
    state.books.get_mut(&id).unwrap().sync = Some(checkpoint.clone());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reader.json");
    let (mut journal, _) = StateJournal::open(directory.path()).unwrap();
    state.save(&path).unwrap();
    journal.save(&state).unwrap();
    assert_eq!(
        ReaderState::load(&path).unwrap().books[&id].sync,
        Some(checkpoint.clone())
    );
    for mode in 0..5 {
        let mut bad = checkpoint.clone();
        match mode {
            0 => bad.etag = "W/\"weak\"".into(),
            1 => bad.etag = "\"".into(),
            2 => bad.source = "x".repeat(4097),
            3 => bad.baseline.book_id = "b".repeat(64),
            _ => bad.baseline.version = 2,
        }
        state.books.get_mut(&id).unwrap().sync = Some(bad);
        assert!(state.save(&path).is_err());
        assert!(journal.save(&state).is_err());
        assert_eq!(
            StateJournal::open(directory.path()).unwrap().1.books[&id].sync,
            Some(checkpoint.clone())
        );
        std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(ReaderState::load(&path).is_err());
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
