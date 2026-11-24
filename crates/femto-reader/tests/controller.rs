use femto_reader::{
    controller::{Action, Effect, Reader},
    document::Document,
    layout::{Font, Fonts},
    state::ReaderState,
};
use std::path::PathBuf;
fn reader() -> Reader {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    Reader::new(
        Document::open(root.join("sample.epub")).unwrap(),
        Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap()
}

#[test]
fn saved_navigation_publishes_before_display_and_preserves_annotations() {
    let mut reader = reader();
    reader.record_position(10);
    let original = reader.position();
    let target = femto_reader::layout::Locator {
        byte: 4,
        ..original
    };
    let mut job = reader.begin_location(target).unwrap();
    while !reader.step_navigation(&mut job).unwrap() {}
    let before = serde_json::to_vec(&reader.state).unwrap();
    assert!(
        reader
            .finish_navigation_saved(job, 20, |_| {
                Err(std::io::Error::other("disk full").into())
            })
            .is_err()
    );
    assert_eq!(reader.position(), original);
    assert_eq!(serde_json::to_vec(&reader.state).unwrap(), before);
    assert!(reader.begin_navigation(Action::Back).unwrap().is_none());

    let mut job = reader.begin_location(target).unwrap();
    while !reader.step_navigation(&mut job).unwrap() {}
    reader.dispatch(Action::ToggleBookmark).unwrap();
    let id = reader.document.metadata.id.clone();
    let mut saved = None;
    reader
        .finish_navigation_saved(job, 30, |state| {
            assert_eq!(state.books[&id].position, target);
            assert_eq!(state.books[&id].bookmarks.len(), 1);
            saved = Some(serde_json::to_vec(state).unwrap());
            Ok(())
        })
        .unwrap();
    assert_eq!(reader.position(), target);
    assert_eq!(Some(serde_json::to_vec(&reader.state).unwrap()), saved);
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.position(), original);
}

#[test]
fn stale_saved_navigation_never_calls_storage() {
    let mut reader = reader();
    let target = femto_reader::layout::Locator {
        byte: 4,
        ..reader.position()
    };
    let mut job = reader.begin_location(target).unwrap();
    while !reader.step_navigation(&mut job).unwrap() {}
    reader.dispatch(Action::Chapter(1)).unwrap();
    assert!(
        reader
            .finish_navigation_saved(job, 20, |_| { panic!("stale navigation must not save") })
            .is_err()
    );
    assert!(
        reader
            .begin_location(femto_reader::layout::Locator {
                byte: usize::MAX,
                ..reader.position()
            })
            .is_err()
    );
}

#[test]
fn word_hits_resolve_ligatures_combining_text_and_rtl_clusters_after_rotation() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut fonts = Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap());
    fonts
        .fallback
        .push(Font::load(root.join("DejaVuSans.ttf")).unwrap());
    let text = "office nai\u{308}ve שלום punctuation!";
    let mut reader = Reader::new(
        Document::from_text("Word hits", text).unwrap(),
        fonts,
        ReaderState::default(),
    )
    .unwrap();
    for rotation in [
        femto_reader::state::Rotation::Portrait,
        femto_reader::state::Rotation::Clockwise,
        femto_reader::state::Rotation::Inverted,
        femto_reader::state::Rotation::Counterclockwise,
    ] {
        reader.dispatch(Action::Rotate(rotation)).unwrap();
        let position = reader.position();
        let before = serde_json::to_value(&reader.state).unwrap();
        for word in ["office", "nai\u{308}ve", "שלום", "punctuation"] {
            let start = text.find(word).unwrap();
            let end = start + word.len();
            let line = reader
                .page()
                .lines
                .iter()
                .find(|line| {
                    line.glyphs
                        .iter()
                        .any(|g| (start..end).contains(&g.cluster))
                })
                .unwrap();
            let glyphs: Vec<_> = line
                .glyphs
                .iter()
                .filter(|g| (start..end).contains(&g.cluster) && g.advance >= 2.0)
                .collect();
            if word == "office" {
                assert!(glyphs.len() < word.len(), "fixture must shape a ligature");
            }
            if word == "שלום" {
                assert!(
                    glyphs
                        .windows(2)
                        .any(|pair| pair[0].cluster > pair[1].cluster)
                );
            }
            for glyph in glyphs {
                let x = (glyph.x + glyph.advance / 2.0).floor() as u32;
                let y = (line.baseline - line.ascent / 2.0).floor() as u32;
                let range = reader.word_at(x, y).unwrap();
                assert_eq!(range.start.byte, start);
                assert_eq!(range.end.byte, end);
                assert_eq!(range.start.chapter, 0);
                assert_eq!(range.start.block, 0);
            }
        }
        for line in &reader.page().lines {
            for glyph in &line.glyphs {
                if text[glyph.cluster..].starts_with([' ', '!']) && glyph.advance >= 2.0 {
                    assert!(
                        reader
                            .word_at(
                                (glyph.x + glyph.advance / 2.0) as u32,
                                (line.baseline - line.ascent / 2.0) as u32
                            )
                            .is_none()
                    );
                }
            }
        }
        assert!(reader.word_at(0, 0).is_none());
        assert!(reader.word_at(reader.state.layout.width, 0).is_none());
        assert!(reader.word_at(0, reader.state.layout.height).is_none());
        assert_eq!(reader.position(), position);
        assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
    }
}

#[test]
fn word_hit_selects_the_full_source_word_across_line_and_page_breaks() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let text = "uninterrupted".repeat(30);
    let mut state = ReaderState::default();
    state.layout.width = 160;
    state.layout.height = 160;
    state.layout.margin = 8;
    state.layout.hyphenation = femto_reader::layout::Hyphenation::Off;
    let mut reader = Reader::new(
        Document::from_text("Long word", &text).unwrap(),
        Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap()),
        state,
    )
    .unwrap();
    assert!(reader.page_count() > 1);
    reader.dispatch(Action::Next).unwrap();
    assert!(reader.page().start.byte > 0);
    let line = &reader.page().lines[0];
    let glyph = &line.glyphs[0];
    let range = reader
        .word_at(
            (glyph.x + glyph.advance / 2.0) as u32,
            (line.baseline - line.ascent / 2.0) as u32,
        )
        .unwrap();
    assert_eq!(range.start.byte, 0);
    assert_eq!(range.end.byte, text.len());
    assert_eq!(
        femto_reader::selection::extract(&mut reader.document, range).unwrap(),
        text
    );
}

#[test]
fn clipping_navigation_verifies_source_and_retains_return_history() {
    let mut reader = reader();
    reader.dispatch(Action::Chapter(1)).unwrap();
    reader.clip_page_text(42).unwrap();
    let id = reader.document.metadata.id.clone();
    let target = reader.state.books[&id].clippings[0].range.start;
    reader.dispatch(Action::Chapter(0)).unwrap();
    let origin = reader.position();
    let pixels = reader.render(4).unwrap();
    let mut cancelled = reader
        .begin_navigation(Action::Clipping(0))
        .unwrap()
        .unwrap();
    reader.step_navigation(&mut cancelled).unwrap();
    drop(cancelled);
    assert_eq!(reader.position(), origin);
    assert_eq!(reader.render(4).unwrap(), pixels);
    complete_navigation(&mut reader, Action::Clipping(0));
    assert_eq!(reader.position(), target);
    complete_navigation(&mut reader, Action::Back);
    assert_eq!(reader.position(), origin);
    assert_eq!(reader.render(4).unwrap(), pixels);
    reader.dispatch(Action::Clipping(0)).unwrap();
    assert_eq!(reader.position(), target);
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.position(), origin);
    reader.state.books.get_mut(&id).unwrap().clippings[0]
        .text
        .push('!');
    assert!(reader.begin_navigation(Action::Clipping(0)).is_err());
    assert!(reader.dispatch(Action::Clipping(0)).is_err());
    assert!(reader.dispatch(Action::Clipping(99)).is_err());
    assert_eq!(reader.position(), origin);
    assert_eq!(reader.render(4).unwrap(), pixels);
}

#[test]
fn removing_a_clipping_invalidates_pending_navigation_without_moving() {
    let mut reader = reader();
    reader.dispatch(Action::Chapter(1)).unwrap();
    reader.clip_page_text(42).unwrap();
    reader.dispatch(Action::Chapter(0)).unwrap();
    let position = reader.position();
    let pixels = reader.render(4).unwrap();
    let mut job = reader
        .begin_navigation(Action::Clipping(0))
        .unwrap()
        .unwrap();
    while !reader.step_navigation(&mut job).unwrap() {}
    let id = reader.document.metadata.id.clone();
    reader.state.books.get_mut(&id).unwrap().clippings.clear();
    assert!(reader.step_navigation(&mut job).is_err());
    assert!(reader.finish_navigation(job).is_err());
    assert_eq!(reader.position(), position);
    assert_eq!(reader.render(4).unwrap(), pixels);
}

#[test]
fn cooperative_startup_restores_position_and_matches_synchronous_pixels() {
    use femto_reader::controller::ReaderStartup;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let fonts = || Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap());
    let mut original = reader();
    original.dispatch(Action::Next).unwrap();
    original.dispatch(Action::ToggleBookmark).unwrap();
    original.record_position(123);
    let state = original.state.clone();
    let mut expected = Reader::new(
        Document::open(root.join("sample.epub")).unwrap(),
        fonts(),
        state.clone(),
    )
    .unwrap();
    let mut startup =
        ReaderStartup::from_file(root.join("sample.epub"), fonts(), state.clone()).unwrap();
    let mut steps = 0;
    loop {
        let before = startup.indexed_pages();
        let done = startup.step().unwrap();
        assert!(startup.indexed_pages() <= before + 1);
        steps += 1;
        assert!(steps < 16_384);
        if done {
            break;
        }
    }
    let mut actual = startup.finish().unwrap();
    assert_eq!(actual.position(), expected.position());
    assert_eq!(actual.page_index(), expected.page_index());
    // Opening stops at the saved page; later steps index the rest.
    assert!(actual.page_count() <= expected.page_count());
    while actual.index_step().unwrap() {}
    assert_eq!(actual.page_count(), expected.page_count());
    assert_eq!(actual.render(4).unwrap(), expected.render(4).unwrap());
    assert_eq!(
        serde_json::to_value(actual.state).unwrap(),
        serde_json::to_value(state).unwrap()
    );
    let incomplete = ReaderStartup::from_document(
        Document::open(root.join("sample.epub")).unwrap(),
        fonts(),
        ReaderState::default(),
    )
    .unwrap();
    assert!(incomplete.finish().is_err());
}

#[test]
fn startup_can_be_dropped_during_hashing_and_defers_invalid_document_errors() {
    use femto_reader::controller::ReaderStartup;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("broken.epub");
    std::fs::write(&path, vec![b'x'; 24_576]).unwrap();
    let fonts = || {
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        )
    };
    let mut startup = ReaderStartup::from_file(&path, fonts(), ReaderState::default()).unwrap();
    assert!(!startup.step().unwrap());
    assert_eq!(startup.indexed_pages(), 0);
    drop(startup);
    let incomplete = ReaderStartup::from_file(&path, fonts(), ReaderState::default()).unwrap();
    assert!(incomplete.finish().is_err());
    let mut startup = ReaderStartup::from_file(&path, fonts(), ReaderState::default()).unwrap();
    for _ in 0..3 {
        assert!(!startup.step().unwrap());
    }
    assert!(startup.step().is_err());
}

#[test]
fn startup_retry_and_cancel_preserve_inputs_and_render_loading_for_device_levels() {
    use femto_reader::controller::ReaderStartup;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("retry.txt");
    std::fs::write(&path, "Replacement book.\n\nText after retry.").unwrap();
    for (width, height, levels) in [(480, 800, 2), (800, 480, 4)] {
        let mut state = ReaderState::default();
        state.layout.width = width;
        state.layout.height = height;
        state.frontlight.brightness = 37;
        let expected = serde_json::to_value(&state).unwrap();
        let fonts = Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        );
        let mut startup = ReaderStartup::from_document(
            Document::from_text("Fallback", "Fallback page.").unwrap(),
            fonts,
            state,
        )
        .unwrap();
        assert!(
            startup
                .open_file(directory.path().join("missing.epub"))
                .is_err()
        );
        assert!(startup.step().unwrap()); // Failed replacement kept the fallback.
        startup.open_file(&path).unwrap();
        assert_eq!(startup.indexed_pages(), 0);
        let pixels = startup.loading_frame(levels).unwrap();
        assert_eq!(pixels.len(), (width * height) as usize);
        assert!(pixels.contains(&0));
        assert!(pixels.iter().all(|pixel| if levels == 2 {
            matches!(pixel, 0 | 255)
        } else {
            matches!(pixel, 0 | 85 | 170 | 255)
        }));
        assert!(!startup.step().unwrap());
        let (fonts, state) = startup.cancel();
        assert_eq!(serde_json::to_value(&state).unwrap(), expected);
        let mut retry = ReaderStartup::from_file(&path, fonts, state).unwrap();
        while !retry.step().unwrap() {}
        let reader = retry.finish().unwrap();
        assert_eq!(
            reader.document.metadata.id,
            Document::open(&path).unwrap().metadata.id
        );
        assert_eq!(serde_json::to_value(reader.state).unwrap(), expected);
    }
}

#[test]
fn embedded_device_text_uses_the_same_navigation_and_monochrome_rendering() {
    let text = include_str!("../../../tests/fixtures/device-reader.txt");
    let document = Document::from_text("Device specimen", text).unwrap();
    let file = Document::open(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/device-reader.txt"),
    )
    .unwrap();
    assert_eq!(document.metadata.id, file.metadata.id);
    let font = Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
        .unwrap();
    let mut reader = Reader::new(document, Fonts::new(font), ReaderState::default()).unwrap();
    assert!(reader.page_count() >= 3);
    assert_eq!(
        reader.dispatch(Action::Previous).unwrap(),
        Effect::Unchanged
    );
    let first = reader.render(2).unwrap();
    assert_eq!(first.len(), 480 * 800);
    assert!(first.iter().all(|p| matches!(p, 0 | 255)));
    assert!(first.contains(&0));
    for _ in 1..reader.page_count() {
        assert_eq!(reader.dispatch(Action::Next).unwrap(), Effect::Redraw);
    }
    assert_eq!(reader.dispatch(Action::Next).unwrap(), Effect::Unchanged);
    for _ in 1..reader.page_count() {
        reader.dispatch(Action::Previous).unwrap();
    }
    assert_eq!(reader.render(2).unwrap(), first);
    assert!(
        Document::from_text(
            "too long",
            &"x".repeat(femto_reader::document::MAX_CHAPTER_BYTES + 1)
        )
        .is_err()
    );
}

#[test]
fn rejected_document_does_not_replace_reader_state_or_navigation_history() {
    let mut reader = reader();
    reader.dispatch(Action::Next).unwrap();
    let original = reader.page().start;
    let id = reader.document.metadata.id.clone();
    let mut invalid = Document::from_text("Invalid", "Text").unwrap();
    invalid.metadata.chapters.clear();
    assert!(reader.open_document(invalid).is_err());
    assert_eq!(reader.document.metadata.id, id);
    assert_eq!(reader.page().start, original);
    let raster = reader.render(2).unwrap();
    assert!(raster.contains(&0));
}

#[test]
fn transitions_cross_chapters_and_return_to_bookmarks() {
    let mut reader = reader();
    assert_eq!(
        reader.dispatch(Action::Previous).unwrap(),
        Effect::Unchanged
    );
    reader.dispatch(Action::Next).unwrap();
    let marked = reader.page().start;
    reader.dispatch(Action::ToggleBookmark).unwrap();
    reader.dispatch(Action::Chapter(1)).unwrap();
    assert_eq!(reader.page().start.chapter, 1);
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.page().start, marked);
    reader.dispatch(Action::Chapter(1)).unwrap();
    reader.dispatch(Action::Previous).unwrap();
    assert_eq!(reader.page_index(), reader.page_count() - 1);
    reader.dispatch(Action::Next).unwrap();
    assert_eq!(reader.page().start.chapter, 1);
    reader.dispatch(Action::Bookmark(0)).unwrap();
    assert_eq!(reader.page().start, marked);
    reader.record_position(123);
    let saved = &reader.state.books[&reader.document.metadata.id];
    assert_eq!(saved.position, marked);
    assert_eq!(saved.last_opened, 123);
}

#[test]
fn failed_transitions_preserve_page_layout_and_history() {
    let mut reader = reader();
    reader.dispatch(Action::Next).unwrap();
    let origin = reader.page().start;
    reader.dispatch(Action::Chapter(1)).unwrap();
    let current = reader.page().start;
    assert!(reader.dispatch(Action::Chapter(999)).is_err());
    assert!(reader.dispatch(Action::Bookmark(999)).is_err());
    let mut invalid = reader.state.layout;
    invalid.width = 0;
    assert!(reader.dispatch(Action::Reflow(invalid)).is_err());
    assert_eq!(reader.page().start, current);
    assert_eq!(reader.state.layout.width, 480);
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.page().start, origin);
}

#[test]
fn touch_zones_and_reflow_share_button_navigation() {
    let mut reader = reader();
    assert_eq!(
        reader.dispatch(Action::Tap { x: 240, y: 400 }).unwrap(),
        Effect::Menu
    );
    assert_eq!(
        reader.dispatch(Action::Tap { x: 480, y: 400 }).unwrap(),
        Effect::Unchanged
    );
    assert_eq!(
        reader.dispatch(Action::Tap { x: 400, y: 800 }).unwrap(),
        Effect::Unchanged
    );
    reader.dispatch(Action::Tap { x: 470, y: 400 }).unwrap();
    assert_eq!(reader.page_index(), 1);
    let anchor = reader.page().start;
    let mut layout = reader.state.layout;
    layout.font_size = 28.0;
    reader.dispatch(Action::Reflow(layout)).unwrap();
    assert!(reader.page().start <= anchor && reader.page().end >= anchor);
    reader.dispatch(Action::Tap { x: 0, y: 400 }).unwrap();
    assert_eq!(reader.page_index(), 0);
}

#[test]
fn source_anchor_survives_repeated_reflow_save_and_reopen() {
    let mut reader = reader();
    reader.dispatch(Action::Next).unwrap();
    let anchor = reader.position();
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let directory = tempfile::tempdir().unwrap();
    let state_path = directory.path().join("reader.json");
    for size in [32.0, 18.0, 28.0, 24.0] {
        let mut config = reader.state.layout;
        config.font_size = size;
        reader.dispatch(Action::Reflow(config)).unwrap();
        assert_eq!(reader.position(), anchor);
        assert!(reader.page().start <= anchor && reader.page().end >= anchor);
        reader.record_position(123);
        assert_eq!(
            reader.state.books[&reader.document.metadata.id].position,
            anchor
        );
        reader.state.save(&state_path).unwrap();
        reader = Reader::new(
            Document::open(root.join("sample.epub")).unwrap(),
            Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap()),
            ReaderState::load(&state_path).unwrap(),
        )
        .unwrap();
        assert_eq!(reader.position(), anchor);
    }
    assert_eq!(reader.page().start, anchor);
    reader.dispatch(Action::Next).unwrap();
    assert_ne!(reader.position(), anchor);
    assert_eq!(reader.position(), reader.page().start);
    reader.dispatch(Action::Previous).unwrap();
    assert_eq!(reader.position(), anchor);
}

#[test]
fn same_page_jumps_bookmarks_back_and_book_switches_keep_exact_anchors() {
    let mut reader = reader();
    let origin = reader.position();
    let target = reader.page().lines[1].start;
    assert_ne!(origin, target);
    let id = reader.document.metadata.id.clone();
    reader
        .state
        .toggle_bookmark(&id, target, "Inside page".into());
    assert_eq!(
        reader.dispatch(Action::Bookmark(0)).unwrap(),
        Effect::Redraw
    );
    assert_eq!(reader.position(), target);
    assert_eq!(reader.page().start, origin);
    let mut config = reader.state.layout;
    config.font_size = 30.0;
    reader.dispatch(Action::Reflow(config)).unwrap();
    reader.dispatch(Action::ToggleBookmark).unwrap();
    assert!(reader.state.books[&id].bookmarks.is_empty());
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.position(), origin);
    assert_eq!(reader.dispatch(Action::Back).unwrap(), Effect::Unchanged);
    reader
        .state
        .toggle_bookmark(&id, target, "Inside page".into());
    reader.dispatch(Action::Bookmark(0)).unwrap();
    reader
        .open_document(Document::from_text("Other", "Another book").unwrap())
        .unwrap();
    assert_eq!(reader.state.books[&id].position, target);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    reader
        .open_document(Document::open(root.join("sample.epub")).unwrap())
        .unwrap();
    assert_eq!(reader.position(), target);
    assert_eq!(reader.dispatch(Action::Back).unwrap(), Effect::Unchanged);
    let mut invalid = reader.state.layout;
    invalid.width = 0;
    assert!(reader.dispatch(Action::Reflow(invalid)).is_err());
    assert_eq!(reader.position(), target);
}

#[test]
fn invalid_resume_offsets_normalize_to_a_valid_page_start() {
    use femto_reader::layout::Locator;
    for byte in [1, usize::MAX] {
        let document = Document::from_text("Unicode", "éclair and ordinary text").unwrap();
        let id = document.metadata.id.clone();
        let mut state = ReaderState::default();
        state.set_position(
            &id,
            Locator {
                byte,
                ..Default::default()
            },
            1,
        );
        let fonts = Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        );
        let mut reader = Reader::new(document, fonts, state).unwrap();
        assert_eq!(reader.position(), reader.page().start);
        reader.record_position(2);
        assert_eq!(reader.state.books[&id].position, reader.page().start);
    }
}

#[test]
fn all_rotations_preserve_source_positions_and_reflow_only_the_view_geometry() {
    use femto_reader::state::Rotation;
    let mut reader = reader();
    reader.dispatch(Action::Next).unwrap();
    let position = reader.position();
    for rotation in [
        Rotation::Clockwise,
        Rotation::Inverted,
        Rotation::Counterclockwise,
        Rotation::Portrait,
    ] {
        reader.dispatch(Action::Rotate(rotation)).unwrap();
        assert_eq!(reader.position(), position);
        assert_eq!(reader.state.rotation, rotation);
        assert_eq!(
            (reader.state.layout.width, reader.state.layout.height),
            if rotation.landscape() {
                (800, 480)
            } else {
                (480, 800)
            }
        );
        assert_eq!(reader.render(2).unwrap().len(), 480 * 800);
        assert_eq!(
            reader.dispatch(Action::Rotate(rotation)).unwrap(),
            Effect::Unchanged
        );
    }
}

#[test]
fn cooperative_open_preserves_current_book_until_complete_and_rejects_stale_layout() {
    let mut reader = reader();
    let old = reader.document.metadata.id.clone();
    let position = reader.position();
    let pixels = reader.render(2).unwrap();
    let text = "A new destination paragraph.\n\n".repeat(300);
    let mut opening = reader
        .begin_open(Document::from_text("Destination", &text).unwrap())
        .unwrap();
    assert!(!reader.step_open(&mut opening).unwrap());
    assert_eq!(opening.indexed_pages(), 1);
    assert_eq!(reader.document.metadata.id, old);
    assert_eq!(reader.position(), position);
    assert_eq!(reader.render(2).unwrap(), pixels);
    assert!(reader.finish_open(opening).is_err());
    assert_eq!(reader.document.metadata.id, old);
    let mut opening = reader
        .begin_open(Document::from_text("Destination", &text).unwrap())
        .unwrap();
    let mut config = reader.state.layout;
    config.font_size += 2.0;
    reader.dispatch(Action::Reflow(config)).unwrap();
    assert!(reader.step_open(&mut opening).is_err());
    assert!(reader.finish_open(opening).is_err());
    assert_eq!(reader.document.metadata.id, old);
    let mut opening = reader
        .begin_open(Document::from_text("Destination", &text).unwrap())
        .unwrap();
    reader.dispatch(Action::Next).unwrap();
    let moved = reader.position();
    assert!(reader.step_open(&mut opening).is_err());
    assert!(reader.finish_open(opening).is_err());
    assert_eq!(reader.position(), moved);
}

#[test]
fn cooperative_reflow_matches_synchronous_layout_without_changing_state_early() {
    let mut reader = reader();
    reader.dispatch(Action::Next).unwrap();
    reader.record_position(1);
    let position = reader.position();
    let previous = reader.state.layout;
    let raster = reader.render(2).unwrap();
    let mut next = previous;
    next.font_size += 2.0;
    let mut job = reader.begin_reflow(next).unwrap();
    assert!(!reader.step_reflow(&mut job).unwrap());
    assert_eq!(reader.state.layout, previous);
    assert_eq!(reader.position(), position);
    assert_eq!(reader.render(2).unwrap(), raster);
    assert!(reader.finish_reflow(job).is_err());
    assert_eq!(reader.state.layout, previous);
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut expected = Reader::new(
        Document::open(root.join("sample.epub")).unwrap(),
        Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap()),
        reader.state.clone(),
    )
    .unwrap();
    expected.dispatch(Action::Reflow(next)).unwrap();
    let mut job = reader.begin_reflow(next).unwrap();
    while !reader.step_reflow(&mut job).unwrap() {}
    reader.finish_reflow(job).unwrap();
    assert_eq!(reader.position(), position);
    assert_eq!(reader.state.layout, next);
    while reader.index_step().unwrap() {}
    assert_eq!(reader.page_count(), expected.page_count());
    assert_eq!(reader.render(2).unwrap(), expected.render(2).unwrap());
    let mut stale = reader.begin_reflow(previous).unwrap();
    reader.dispatch(Action::Next).unwrap();
    let moved = reader.position();
    assert!(reader.step_reflow(&mut stale).is_err());
    assert!(reader.finish_reflow(stale).is_err());
    assert_eq!(reader.position(), moved);
    assert_eq!(reader.state.layout, next);
}

#[test]
fn cooperative_file_open_hashes_before_indexing_and_preserves_reader_until_commit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Destination.txt");
    let text = format!("Destination.\n\n{}", " ".repeat(24_576));
    std::fs::write(&path, &text).unwrap();
    let expected = Document::open(&path).unwrap().metadata.id;
    let mut reader = reader();
    let old = reader.document.metadata.id.clone();
    let position = reader.position();
    let pixels = reader.render(2).unwrap();
    let incomplete = reader.begin_open_file(&path).unwrap();
    assert!(reader.finish_open(incomplete).is_err());
    let mut opening = reader.begin_open_file(&path).unwrap();
    for _ in 0..text.len().div_ceil(8192) {
        assert!(!reader.step_open(&mut opening).unwrap());
        assert_eq!(opening.indexed_pages(), 0);
        assert_eq!(reader.document.metadata.id, old);
    }
    assert!(!reader.step_open(&mut opening).unwrap()); // EOF and metadata parsing.
    assert_eq!(opening.indexed_pages(), 0);
    assert_eq!(reader.position(), position);
    assert_eq!(reader.render(2).unwrap(), pixels);
    assert!(reader.step_open(&mut opening).unwrap());
    assert_eq!(opening.indexed_pages(), 1);
    reader.finish_open(opening).unwrap();
    assert_eq!(reader.document.metadata.id, expected);
}

#[test]
fn file_open_rejects_stale_reader_during_hashing_and_reports_deferred_parse_errors() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bad.epub");
    std::fs::write(&path, vec![b'x'; 16_385]).unwrap();
    let mut reader = reader();
    let old = reader.document.metadata.id.clone();
    let mut opening = reader.begin_open_file(&path).unwrap();
    assert!(!reader.step_open(&mut opening).unwrap());
    reader.dispatch(Action::Next).unwrap();
    let moved = reader.position();
    assert!(reader.step_open(&mut opening).is_err());
    assert!(reader.finish_open(opening).is_err());
    let mut opening = reader.begin_open_file(&path).unwrap();
    let mut config = reader.state.layout;
    config.font_size += 2.0;
    reader.dispatch(Action::Reflow(config)).unwrap();
    assert!(reader.step_open(&mut opening).is_err());
    drop(opening);
    let mut opening = reader.begin_open_file(&path).unwrap();
    for _ in 0..3 {
        assert!(!reader.step_open(&mut opening).unwrap());
    }
    assert!(reader.step_open(&mut opening).is_err());
    assert_eq!(reader.document.metadata.id, old);
    assert_eq!(reader.position(), moved);
}

#[test]
fn whole_book_estimate_advances_across_chapters_and_reaches_the_end() {
    let mut reader = reader();
    let first = reader.book_progress_percent().unwrap();
    let mut previous = first;
    loop {
        let current = reader.book_progress_percent().unwrap();
        if reader.position().chapter == 0 && reader.page_index() + 1 == reader.page_count() {
            // Fixture spine XHTML sizes are 5507 and 192 bytes.
            assert_eq!(current, 96);
        }
        assert!(current >= previous);
        previous = current;
        if reader.dispatch(Action::Next).unwrap() == Effect::Unchanged {
            break;
        }
    }
    assert_eq!(previous, 100);
    assert!(first < 100);
    reader.dispatch(Action::Chapter(0)).unwrap();
    assert_eq!(reader.book_progress_percent(), Some(first));
    let position = reader.position();
    let mut layout = reader.state.layout;
    layout.font_size = 26.0;
    reader.dispatch(Action::Reflow(layout)).unwrap();
    assert_eq!(reader.position(), position);
    assert!(reader.book_progress_percent().unwrap() <= 100);
}

#[test]
fn cooperative_rotation_commits_geometry_and_orientation_together() {
    use femto_reader::state::Rotation;
    let mut reader = reader();
    reader.dispatch(Action::Next).unwrap();
    let position = reader.position();
    reader.record_position(1);
    for rotation in [
        Rotation::Clockwise,
        Rotation::Inverted,
        Rotation::Counterclockwise,
        Rotation::Portrait,
    ] {
        let previous = reader.state.clone();
        let pixels = reader.render(2).unwrap();
        let mut cancelled = reader.begin_rotation(rotation).unwrap();
        assert!(!reader.step_reflow(&mut cancelled).unwrap());
        assert_eq!(reader.state.rotation, previous.rotation);
        assert_eq!(reader.state.layout, previous.layout);
        assert_eq!(reader.render(2).unwrap(), pixels);
        assert!(reader.finish_reflow(cancelled).is_err());
        assert_eq!(reader.state.rotation, previous.rotation);
        let mut expected = self::reader();
        expected.dispatch(Action::Next).unwrap();
        expected.dispatch(Action::Rotate(rotation)).unwrap();
        let mut job = reader.begin_rotation(rotation).unwrap();
        let mut steps = 1;
        while !reader.step_reflow(&mut job).unwrap() {
            steps += 1;
        }
        assert!(steps <= expected.page_count());
        reader.finish_reflow(job).unwrap();
        assert_eq!(reader.state.rotation, rotation);
        assert_eq!(reader.state.layout, expected.state.layout);
        assert_eq!(reader.position(), position);
        while reader.index_step().unwrap() {}
        assert_eq!(reader.page_count(), expected.page_count());
        assert_eq!(reader.render(2).unwrap(), expected.render(2).unwrap());
    }
    let mut stale = reader.begin_rotation(Rotation::Clockwise).unwrap();
    // An orientation change with identical dimensions must also stale the job.
    reader.dispatch(Action::Rotate(Rotation::Inverted)).unwrap();
    assert!(reader.step_reflow(&mut stale).is_err());
    assert!(reader.finish_reflow(stale).is_err());
    assert_eq!(reader.state.rotation, Rotation::Inverted);
}

fn complete_navigation(reader: &mut Reader, action: Action) {
    if let Some(mut job) = reader.begin_navigation(action).unwrap() {
        for _ in 0..16_384 {
            if reader.step_navigation(&mut job).unwrap() {
                reader.finish_navigation(job).unwrap();
                return;
            }
        }
        panic!("navigation did not finish");
    } else {
        reader.dispatch(action).unwrap();
    }
}

#[test]
fn cooperative_chapter_jumps_preserve_history_and_match_synchronous_navigation() {
    let mut reader = reader();
    let mut expected = self::reader();
    for action in [
        Action::Next,
        Action::ToggleBookmark,
        Action::Chapter(1),
        Action::Back,
        Action::Contents(1),
        Action::Bookmark(0),
        Action::Back,
        Action::Chapter(0),
    ] {
        complete_navigation(&mut reader, action);
        expected.dispatch(action).unwrap();
        assert_eq!(reader.position(), expected.position());
        while reader.index_step().unwrap() {}
        assert_eq!(reader.page_count(), expected.page_count());
        assert_eq!(reader.render(2).unwrap(), expected.render(2).unwrap());
    }
    // Crossing forward and backward chapter boundaries must not add jump history.
    reader.dispatch(Action::Chapter(1)).unwrap();
    expected.dispatch(Action::Chapter(1)).unwrap();
    for action in [Action::Previous, Action::Next, Action::Back, Action::Back] {
        complete_navigation(&mut reader, action);
        expected.dispatch(action).unwrap();
        assert_eq!(reader.position(), expected.position());
        assert_eq!(reader.render(2).unwrap(), expected.render(2).unwrap());
    }
}

#[test]
fn cancelled_incomplete_and_stale_navigation_do_not_replace_the_active_chapter() {
    let mut reader = reader();
    reader.dispatch(Action::Next).unwrap();
    let return_to = reader.position();
    reader.dispatch(Action::Chapter(1)).unwrap();
    let position = reader.position();
    let pixels = reader.render(2).unwrap();
    let mut incomplete = reader.begin_navigation(Action::Back).unwrap().unwrap();
    assert!(!reader.step_navigation(&mut incomplete).unwrap());
    assert!(reader.finish_navigation(incomplete).is_err());
    assert_eq!(reader.position(), position);
    assert_eq!(reader.render(2).unwrap(), pixels);
    let mut cancelled = reader.begin_navigation(Action::Back).unwrap().unwrap();
    assert!(!reader.step_navigation(&mut cancelled).unwrap());
    drop(cancelled);
    // Cancellation must leave the back target intact.
    complete_navigation(&mut reader, Action::Back);
    assert_eq!(reader.position(), return_to);
    let mut stale = reader
        .begin_navigation(Action::Chapter(1))
        .unwrap()
        .unwrap();
    reader
        .dispatch(Action::Rotate(femto_reader::state::Rotation::Inverted))
        .unwrap();
    assert!(reader.step_navigation(&mut stale).is_err());
    assert!(reader.finish_navigation(stale).is_err());
    assert_eq!(reader.position(), return_to);
    assert!(
        reader
            .begin_navigation(Action::Chapter(usize::MAX))
            .is_err()
    );
    assert_eq!(reader.position(), return_to);
}
