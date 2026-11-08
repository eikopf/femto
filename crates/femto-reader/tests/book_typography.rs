use femto_reader::{
    controller::{Action, Reader, ReaderStartup},
    document::Document,
    journal::StateJournal,
    layout::{Font, Fonts, Hyphenation},
    state::{BookTypography, ReaderState, Rotation},
};
use std::path::PathBuf;

fn fonts() -> Fonts {
    Fonts::new(
        Font::load(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/DejaVuSerif.ttf"),
        )
        .unwrap(),
    )
}
fn book(name: &str) -> Document {
    Document::from_text(
        name,
        &format!(
            "{name}\n{}",
            "Extraordinary typography and independent book settings. ".repeat(100)
        ),
    )
    .unwrap()
}

#[test]
fn switching_books_restores_typography_and_keeps_device_geometry() {
    let mut reader = Reader::new(book("A"), fonts(), ReaderState::default()).unwrap();
    let a_id = reader.document.metadata.id.clone();
    let a = BookTypography::current(&reader.state);
    reader.dispatch(Action::Next).unwrap();
    let a_position = reader.position();
    reader.open_document(book("B")).unwrap();
    assert_eq!(BookTypography::current(&reader.state), a);
    let mut config = reader.state.layout;
    config.font_size = 30.0;
    config.line_height = 1.6;
    config.margin = 32;
    config.hyphenation = Hyphenation::German;
    reader.dispatch(Action::Reflow(config)).unwrap();
    reader.state.text_raster.stroke = 0.2;
    let b = BookTypography::current(&reader.state);
    reader
        .dispatch(Action::Rotate(Rotation::Clockwise))
        .unwrap();
    let viewport = (reader.state.layout.width, reader.state.layout.height);
    reader.open_document(book("A")).unwrap();
    assert_eq!(BookTypography::current(&reader.state), a);
    assert_eq!(reader.position(), a_position);
    assert_eq!(reader.state.rotation, Rotation::Clockwise);
    assert_eq!(
        (reader.state.layout.width, reader.state.layout.height),
        viewport
    );
    reader.open_document(book("B")).unwrap();
    assert_eq!(BookTypography::current(&reader.state), b);
    reader.record_position(123);
    assert_eq!(reader.state.books[&a_id].typography, Some(a));

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("reader.json");
    reader.state.save(&path).unwrap();
    let state = ReaderState::load(path).unwrap();
    let mut sync = Reader::new(book("A"), fonts(), state.clone()).unwrap();
    let mut startup = ReaderStartup::from_document(book("A"), fonts(), state).unwrap();
    while !startup.step().unwrap() {}
    let mut cooperative = startup.finish().unwrap();
    assert_eq!(BookTypography::current(&cooperative.state), a);
    assert_eq!(cooperative.position(), a_position);
    assert_eq!(cooperative.render(4).unwrap(), sync.render(4).unwrap());
    assert_eq!(cooperative.state.rotation, Rotation::Clockwise);
}

#[test]
fn cancelled_stale_and_invalid_openings_leave_current_typography_intact() {
    let mut reader = Reader::new(book("A"), fonts(), ReaderState::default()).unwrap();
    let id = book("B").metadata.id;
    let mut target = BookTypography::current(&reader.state);
    target.font_size = 36.0;
    reader.state.books.entry(id.clone()).or_default().typography = Some(target);
    let before = serde_json::to_value(&reader.state).unwrap();
    let pixels = reader.render(4).unwrap();
    let mut cancelled = reader.begin_open(book("B")).unwrap();
    reader.step_open(&mut cancelled).unwrap();
    drop(cancelled);
    assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
    assert_eq!(reader.render(4).unwrap(), pixels);

    let mut stale = reader.begin_open(book("B")).unwrap();
    while !reader.step_open(&mut stale).unwrap() {}
    target.hyphenation = Hyphenation::Off;
    reader.state.books.get_mut(&id).unwrap().typography = Some(target);
    assert!(reader.finish_open(stale).is_err());
    assert_eq!(reader.state.layout.font_size, 24.0);
    assert_eq!(reader.render(4).unwrap(), pixels);

    target.margin = 250;
    reader.state.books.get_mut(&id).unwrap().typography = Some(target);
    assert!(reader.begin_open(book("B")).is_err());
    assert_eq!(reader.state.layout.font_size, 24.0);
    assert_eq!(reader.render(4).unwrap(), pixels);
}

#[test]
fn legacy_books_inherit_current_settings_and_invalid_saved_preferences_are_rejected() {
    let mut state = ReaderState::default();
    let id = book("A").metadata.id;
    state.set_position(&id, Default::default(), 1);
    let json = serde_json::to_value(&state).unwrap();
    assert!(json["books"][&id].get("typography").is_none());
    let mut legacy: ReaderState = serde_json::from_value(json).unwrap();
    legacy.layout.hyphenation = Hyphenation::French;
    let reader = Reader::new(book("A"), fonts(), legacy).unwrap();
    assert_eq!(reader.state.layout.hyphenation, Hyphenation::French);

    let mut preference = BookTypography::current(&reader.state);
    state.books.get_mut(&id).unwrap().typography = Some(preference);
    let dir = tempfile::tempdir().unwrap();
    let (mut journal, _) = StateJournal::open(dir.path()).unwrap();
    journal.save(&state).unwrap();
    assert_eq!(
        StateJournal::open(dir.path()).unwrap().1.books[&id].typography,
        Some(preference)
    );
    let path = dir.path().join("host.json");
    state.save(&path).unwrap();
    let good = std::fs::read(&path).unwrap();
    for kind in 0..3 {
        preference = BookTypography::current(&reader.state);
        match kind {
            0 => preference.font_size = 0.0,
            1 => preference.text_raster.stroke = 10.0,
            _ => preference.margin = u32::MAX,
        }
        state.books.get_mut(&id).unwrap().typography = Some(preference);
        assert!(state.save(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), good);
        assert!(journal.save(&state).is_err());
        let invalid = dir.path().join("invalid.json");
        std::fs::write(&invalid, serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(ReaderState::load(invalid).is_err());
    }
    assert_eq!(
        StateJournal::open(dir.path()).unwrap().1.books[&id].typography,
        Some(BookTypography::current(&reader.state))
    );
}

#[test]
fn typography_transition_rejects_stale_raster_and_invalid_preferences() {
    let mut reader = Reader::new(book("A"), fonts(), ReaderState::default()).unwrap();
    let mut next = BookTypography::current(&reader.state);
    next.font_size = 30.0;
    next.text_raster.stroke = 0.4;
    let before = reader.render(4).unwrap();
    let mut job = reader.begin_typography(next).unwrap();
    while !reader.step_reflow(&mut job).unwrap() {}
    assert_eq!(reader.state.layout.font_size, 24.0);
    assert_eq!(reader.render(4).unwrap(), before);
    reader.state.text_raster.coverage_gamma = 0.7;
    assert!(reader.finish_reflow(job).is_err());
    assert_eq!(reader.state.layout.font_size, 24.0);
    assert_eq!(reader.state.text_raster.stroke, 0.0);
    assert_eq!(reader.state.text_raster.coverage_gamma, 0.7);
    next.text_raster.stroke = f32::NAN;
    assert!(reader.begin_typography(next).is_err());
    next.text_raster.stroke = 0.2;
    next.margin = u32::MAX;
    assert!(reader.begin_typography(next).is_err());
}

#[test]
fn tuned_pixels_survive_host_and_journal_reload_in_every_orientation() {
    use femto_reader::display::TextRaster;

    let tuning = TextRaster {
        hinting: false,
        stroke: 0.2,
        coverage_gamma: 0.7,
        fractional: true,
    };
    for rotation in [
        Rotation::Portrait,
        Rotation::Clockwise,
        Rotation::Inverted,
        Rotation::Counterclockwise,
    ] {
        let mut reader = Reader::new(book("Tuned"), fonts(), ReaderState::default()).unwrap();
        reader.dispatch(Action::Rotate(rotation)).unwrap();
        reader.dispatch(Action::Next).unwrap();
        let anchor = reader.position();
        let page_index = reader.page_index();
        let page_count = reader.page_count();
        let neutral = [reader.render(2).unwrap(), reader.render(4).unwrap()];
        reader.state.text_raster = tuning;
        let expected = [reader.render(2).unwrap(), reader.render(4).unwrap()];
        for (pixels, neutral) in expected.iter().zip(&neutral) {
            assert_ne!(pixels, neutral, "tuning did not affect {rotation:?}");
        }
        assert!(expected[0].iter().all(|p| matches!(p, 0 | 255)));
        assert!(expected[1].iter().all(|p| matches!(p, 0 | 85 | 170 | 255)));
        assert!(expected[1].iter().any(|p| matches!(p, 85 | 170)));
        assert_eq!(reader.position(), anchor);
        assert_eq!(
            (reader.page_index(), reader.page_count()),
            (page_index, page_count)
        );
        reader.record_position(42);
        let dir = tempfile::tempdir().unwrap();
        let host = dir.path().join("reader.json");
        reader.state.save(&host).unwrap();
        let (mut journal, _) = StateJournal::open(dir.path()).unwrap();
        journal.save(&reader.state).unwrap();
        drop(journal);

        for mut state in [
            ReaderState::load(&host).unwrap(),
            StateJournal::open(dir.path()).unwrap().1,
        ] {
            // A different active book must not hide loss of the saved book controls.
            state.text_raster = TextRaster::default();
            let mut startup = ReaderStartup::from_document(book("Tuned"), fonts(), state).unwrap();
            while !startup.step().unwrap() {}
            let mut reopened = startup.finish().unwrap();
            assert_eq!(reopened.state.text_raster, tuning);
            assert_eq!(reopened.state.rotation, rotation);
            assert_eq!(reopened.position(), anchor);
            assert_eq!(
                (reopened.page_index(), reopened.page_count()),
                (page_index, page_count)
            );
            assert_eq!(reopened.render(2).unwrap(), expected[0]);
            assert_eq!(reopened.render(4).unwrap(), expected[1]);
        }
    }
}
