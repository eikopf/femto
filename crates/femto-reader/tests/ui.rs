use femto_reader::{
    controller::{Effect, Reader},
    document::Document,
    layout::{Font, Fonts},
    state::ReaderState,
    ui::ReaderUi,
};

fn reader() -> Reader {
    Reader::new(
        Document::from_text(
            "Menus",
            include_str!("../../../tests/fixtures/device-reader.txt"),
        )
        .unwrap(),
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        ),
        ReaderState::default(),
    )
    .unwrap()
}

#[test]
fn clipping_menu_captures_visible_source_and_reads_it_without_moving_the_book() {
    let mut reader = reader();
    reader
        .dispatch(femto_reader::controller::Action::Next)
        .unwrap();
    let position = reader.position();
    let pixels = reader.render(4).unwrap();
    let range = reader.page().lines.first().unwrap().start..reader.page().lines.last().unwrap().end;
    let expected = femto_reader::selection::extract(&mut reader.document, range.clone()).unwrap();
    let id = reader.document.metadata.id.clone();
    let mut ui = ReaderUi::default();
    go(&mut ui, &mut reader, &["This book", "Clippings"]);
    ui.select(&mut reader).unwrap(); // Save page text.
    let saved = &reader.state.books[&id].clippings;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].range, range);
    assert_eq!(saved[0].text, expected);
    let first = saved[0].clone();
    ui.select(&mut reader).unwrap(); // Repeated capture is idempotent.
    assert_eq!(
        reader.state.books[&id].clippings,
        std::slice::from_ref(&first)
    );
    ui.turn(&mut reader, true).unwrap();
    let list = ui.render(&mut reader, 4).unwrap();
    ui.select(&mut reader).unwrap();
    let detail = ui.render(&mut reader, 4).unwrap();
    assert_ne!(list, detail);
    ui.turn(&mut reader, true).unwrap();
    // Back returns to the row that was opened, through each enclosing menu.
    ui.back(&mut reader).unwrap();
    assert_eq!(ui.render(&mut reader, 4).unwrap(), list);
    ui.back(&mut reader).unwrap();
    ui.back(&mut reader).unwrap();
    ui.back(&mut reader).unwrap();
    assert!(!ui.is_open());
    assert_eq!(reader.position(), position);
    assert_eq!(reader.render(4).unwrap(), pixels);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("reader.json");
    reader.record_position(100);
    reader.state.save(&path).unwrap();
    let saved = ReaderState::load(path).unwrap();
    assert_eq!(saved.books[&id].clippings, [first]);
    assert_eq!(saved.books[&id].position, position);
}

/// Opens the root menu and activates each labelled entry in turn.
fn go(ui: &mut ReaderUi, reader: &mut Reader, path: &[&str]) {
    ui.open();
    for label in path {
        ui.choose(reader, label).unwrap();
    }
}

fn finish_preview(ui: &mut ReaderUi, reader: &mut Reader) {
    for _ in 0..16_384 {
        if !ui.is_preview_opening() {
            return;
        }
        ui.poll_preview(reader);
    }
    assert!(!ui.is_preview_opening());
}

fn open_clipping_delete(ui: &mut ReaderUi, reader: &mut Reader) {
    go(ui, reader, &["This book", "Clippings"]);
    ui.turn(reader, true).unwrap();
    ui.select(reader).unwrap();
    ui.select(reader).unwrap();
}

fn open_word_selection(ui: &mut ReaderUi, reader: &mut Reader) {
    go(ui, reader, &["This book", "Clippings"]);
    let count = reader
        .state
        .books
        .get(&reader.document.metadata.id)
        .map_or(0, |b| b.clippings.len());
    for _ in 0..=count {
        ui.turn(reader, true).unwrap();
    }
    ui.select(reader).unwrap();
}

fn open_button_selection(ui: &mut ReaderUi, reader: &mut Reader) {
    open_word_selection(ui, reader);
    ui.back(reader).unwrap();
    let count = reader
        .state
        .books
        .get(&reader.document.metadata.id)
        .map_or(0, |book| book.clippings.len());
    for _ in 0..count + 2 {
        ui.turn(reader, true).unwrap();
    }
    ui.select(reader).unwrap();
}

#[test]
fn button_selection_preserves_unicode_source_order_and_extends_across_paragraphs() {
    let mut reader = Reader::new(
        Document::from_text("Words", "office naïve שלום office.\n\nLast paragraph.").unwrap(),
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        ),
        ReaderState::default(),
    )
    .unwrap();
    let before = serde_json::to_value(&reader.state).unwrap();
    let anchor = reader.position();
    let pixels = reader.render(4).unwrap();
    let mut ui = ReaderUi::default();
    open_button_selection(&mut ui, &mut reader);
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap(); // Combining sequence in the second word.
    ui.select(&mut reader).unwrap(); // Cancel.
    assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
    for (index, expected) in [(1, "naïve"), (2, "שלום"), (3, "office")] {
        open_button_selection(&mut ui, &mut reader);
        for _ in 0..index {
            ui.turn(&mut reader, true).unwrap();
        }
        ui.select(&mut reader).unwrap();
        ui.turn(&mut reader, true).unwrap();
        ui.select(&mut reader).unwrap();
        let saved = reader.state.books[&reader.document.metadata.id]
            .clippings
            .last()
            .unwrap();
        assert_eq!(saved.text, expected);
        if index == 3 {
            assert!(saved.range.start.byte > 0);
        }
    }
    let before_extend = serde_json::to_value(&reader.state).unwrap();
    open_button_selection(&mut ui, &mut reader);
    for _ in 0..5 {
        ui.turn(&mut reader, true).unwrap();
    }
    ui.select(&mut reader).unwrap(); // paragraph
    for _ in 0..2 {
        ui.turn(&mut reader, true).unwrap();
    }
    ui.select(&mut reader).unwrap(); // Extend, then select office at the start.
    ui.select(&mut reader).unwrap();
    assert_eq!(serde_json::to_value(&reader.state).unwrap(), before_extend);
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    let saved = reader.state.books[&reader.document.metadata.id]
        .clippings
        .last()
        .unwrap();
    assert_eq!(saved.text, "office naïve שלום office.\n\nLast paragraph");
    assert_eq!((saved.range.start.block, saved.range.start.byte), (0, 0));
    assert_eq!((saved.range.end.block, saved.range.end.byte), (1, 14));
    assert_eq!(reader.position(), anchor);
    assert_eq!(reader.render(4).unwrap(), pixels);
}

#[test]
fn button_selection_rejects_stale_page_before_pick_or_save() {
    for after_pick in [false, true] {
        let mut reader = reader();
        let mut ui = ReaderUi::default();
        open_button_selection(&mut ui, &mut reader);
        if after_pick {
            ui.select(&mut reader).unwrap();
        }
        reader
            .dispatch(femto_reader::controller::Action::Next)
            .unwrap();
        let before = serde_json::to_value(&reader.state).unwrap();
        if after_pick {
            ui.turn(&mut reader, true).unwrap();
        }
        ui.select(&mut reader).unwrap();
        assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
        assert_eq!(ui.error_message(), Some("document: word selection expired"));
        ui.render(&mut reader, 4).unwrap();
    }
}

#[test]
fn button_word_menu_scrolls_without_turning_the_reading_page() {
    let text = (0..80).map(|n| format!("word{n} ")).collect::<String>();
    let mut reader = Reader::new(
        Document::from_text("Many words", &text).unwrap(),
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        ),
        ReaderState::default(),
    )
    .unwrap();
    let anchor = reader.position();
    let mut ui = ReaderUi::default();
    open_button_selection(&mut ui, &mut reader);
    let first = ui.render(&mut reader, 4).unwrap();
    for _ in 0..24 {
        ui.turn(&mut reader, true).unwrap();
    }
    assert_ne!(ui.render(&mut reader, 4).unwrap(), first);
    ui.select(&mut reader).unwrap();
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    let saved = &reader.state.books[&reader.document.metadata.id].clippings[0];
    assert_eq!(saved.text, "word24");
    assert_eq!(saved.range.start.byte, text.find("word24").unwrap());
    assert_eq!(reader.position(), anchor);
}

#[test]
fn selection_extends_in_either_source_direction_across_paragraphs() {
    for reverse in [false, true] {
        let mut reader = Reader::new(
            Document::from_text("Range", "Alpha beta.\n\nGamma delta.").unwrap(),
            Fonts::new(
                Font::from_bytes(
                    include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec(),
                )
                .unwrap(),
            ),
            ReaderState::default(),
        )
        .unwrap();
        let point = |block, byte| {
            reader
                .page()
                .lines
                .iter()
                .find_map(|line| {
                    if line.start.block != block {
                        return None;
                    }
                    line.glyphs
                        .iter()
                        .find(|g| g.cluster == byte && g.advance > 0.0)
                        .map(|g| {
                            (
                                (g.x + g.advance / 2.0) as u32,
                                (line.baseline - line.ascent / 2.0) as u32,
                            )
                        })
                })
                .unwrap()
        };
        let alpha = point(0, 0);
        let beta = point(0, 6);
        let delta = point(1, 6);
        let (first, last) = if reverse {
            (delta, alpha)
        } else {
            (alpha, delta)
        };
        let anchor = reader.position();
        let before = serde_json::to_value(&reader.state).unwrap();
        let mut ui = ReaderUi::default();
        open_word_selection(&mut ui, &mut reader);
        ui.tap(&mut reader, first.0, first.1).unwrap();
        for target in [last, beta] {
            ui.turn(&mut reader, true).unwrap();
            ui.turn(&mut reader, true).unwrap();
            ui.select(&mut reader).unwrap();
            assert_eq!(ui.tap(&mut reader, 0, 0).unwrap(), Effect::Unchanged);
            ui.tap(&mut reader, target.0, target.1).unwrap();
            ui.render(&mut reader, 4).unwrap();
            assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
        }
        ui.turn(&mut reader, true).unwrap();
        ui.select(&mut reader).unwrap();
        let saved = &reader.state.books[&reader.document.metadata.id].clippings[0];
        assert_eq!(saved.text, "Alpha beta.\n\nGamma delta");
        assert_eq!((saved.range.start.block, saved.range.start.byte), (0, 0));
        assert_eq!((saved.range.end.block, saved.range.end.byte), (1, 11));
        assert_eq!(reader.position(), anchor);
    }
}

#[test]
fn pending_selection_inverts_source_glyphs_and_pans_without_state_changes() {
    for word in ["office", "naïve", "שלום"] {
        for levels in [2, 4] {
            let mut reader = Reader::new(
                Document::from_text("Selection", &format!("{word} other words")).unwrap(),
                Fonts::new(
                    Font::from_bytes(
                        include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec(),
                    )
                    .unwrap(),
                ),
                ReaderState::default(),
            )
            .unwrap();
            let width = reader.state.layout.width as usize;
            let height = reader.state.layout.height as usize;
            let body = (height - 32) * width;
            let line = &reader.page().lines[0];
            let glyph = line
                .glyphs
                .iter()
                .find(|g| g.cluster == 0 && g.advance > 0.0)
                .unwrap();
            let point = (
                (glyph.x + glyph.advance / 2.0) as u32,
                (line.baseline - line.ascent / 2.0) as u32,
            );
            let mut mask = vec![false; width * height];
            for g in line
                .glyphs
                .iter()
                .filter(|g| g.cluster < word.len() && g.advance > 0.0)
            {
                for y in (line.baseline - line.ascent).floor().max(0.0) as usize
                    ..((line.baseline + line.descent).ceil() as usize).min(height)
                {
                    for x in g.x.floor().max(0.0) as usize
                        ..((g.x + g.advance).ceil() as usize).min(width)
                    {
                        mask[y * width + x] = true;
                    }
                }
            }
            assert!(mask.iter().any(|v| *v));
            let original = reader.render(levels).unwrap();
            let state = serde_json::to_value(&reader.state).unwrap();
            let mut ui = ReaderUi::default();
            open_word_selection(&mut ui, &mut reader);
            ui.tap(&mut reader, point.0, point.1).unwrap();
            ui.turn(&mut reader, true).unwrap();
            ui.turn(&mut reader, true).unwrap();
            ui.select(&mut reader).unwrap();
            let selected = ui.render(&mut reader, levels).unwrap();
            for i in 0..body {
                assert_eq!(
                    selected[i],
                    if mask[i] {
                        255 - original[i]
                    } else {
                        original[i]
                    },
                    "{word}, {levels} levels, pixel {i}"
                );
            }
            assert_ne!(&selected[..body], &original[..body]);
            assert!(selected.iter().all(|p| if levels == 2 {
                [0, 255].contains(p)
            } else {
                [0, 85, 170, 255].contains(p)
            }));
            ui.turn(&mut reader, true).unwrap();
            let panned = ui.render(&mut reader, levels).unwrap();
            assert_eq!(
                &panned[..(height - 64) * width],
                &selected[32 * width..body]
            );
            assert_eq!(serde_json::to_value(&reader.state).unwrap(), state);
            ui.back(&mut reader).unwrap();
            assert_eq!(reader.render(levels).unwrap(), original);
            assert_eq!(serde_json::to_value(&reader.state).unwrap(), state);
        }
    }
}

#[test]
fn cancelling_extension_discards_the_pending_range() {
    let mut reader = reader();
    let line = &reader.page().lines[0];
    let g = &line.glyphs[0];
    let (x, y) = (
        (g.x + g.advance / 2.0) as u32,
        (line.baseline - line.ascent / 2.0) as u32,
    );
    let before = serde_json::to_value(&reader.state).unwrap();
    let mut ui = ReaderUi::default();
    open_word_selection(&mut ui, &mut reader);
    ui.tap(&mut reader, x, y).unwrap();
    ui.turn(&mut reader, true).unwrap();
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    ui.back(&mut reader).unwrap();
    assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
    open_word_selection(&mut ui, &mut reader);
    ui.tap(&mut reader, x, y).unwrap();
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    assert!(
        !reader.state.books[&reader.document.metadata.id].clippings[0]
            .text
            .contains(' ')
    );
}

#[test]
fn explicit_word_selection_previews_cancels_and_saves_without_navigation() {
    let mut reader = reader();
    let line = &reader.page().lines[0];
    let glyph = &line.glyphs[0];
    let x = (glyph.x + glyph.advance / 2.0) as u32;
    let y = (line.baseline - line.ascent / 2.0) as u32;
    let range = reader.word_at(x, y).unwrap();
    let text = femto_reader::selection::extract(&mut reader.document, range.clone()).unwrap();
    let before = serde_json::to_value(&reader.state).unwrap();
    let anchor = reader.position();
    let pixels = reader.render(4).unwrap();
    let mut ui = ReaderUi::default();
    open_word_selection(&mut ui, &mut reader);
    let picking = ui.render(&mut reader, 4).unwrap();
    let body = ((reader.state.layout.height - 32) * reader.state.layout.width) as usize;
    assert_eq!(&picking[..body], &pixels[..body]);
    assert_ne!(&picking[body..], &pixels[body..]);
    assert_eq!(ui.tap(&mut reader, 0, 0).unwrap(), Effect::Unchanged);
    assert_eq!(ui.turn(&mut reader, true).unwrap(), Effect::Redraw);
    let panned = ui.render(&mut reader, 4).unwrap();
    assert_eq!(
        &panned[..body],
        &pixels[32 * reader.state.layout.width as usize..]
    );
    assert_eq!(ui.turn(&mut reader, true).unwrap(), Effect::Unchanged);
    assert_eq!(ui.turn(&mut reader, false).unwrap(), Effect::Redraw);
    assert_eq!(ui.render(&mut reader, 4).unwrap(), picking);
    ui.tap(&mut reader, x, y).unwrap();
    assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
    ui.render(&mut reader, 4).unwrap();
    ui.select(&mut reader).unwrap(); // Cancel is initially selected.
    assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
    open_word_selection(&mut ui, &mut reader);
    ui.tap(&mut reader, x, y).unwrap();
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    let saved = &reader.state.books[&reader.document.metadata.id].clippings;
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].range, range);
    assert_eq!(saved[0].text, text);
    assert_eq!(reader.position(), anchor);
    assert_eq!(reader.render(4).unwrap(), pixels);
    open_word_selection(&mut ui, &mut reader);
    let height = reader.state.layout.height;
    ui.tap(&mut reader, 10, height - 1).unwrap(); // Footer cancels.
    assert_eq!(
        reader.state.books[&reader.document.metadata.id]
            .clippings
            .len(),
        1
    );
}

#[test]
fn word_selection_rejects_reader_changes_before_pick_or_save() {
    for after_pick in [false, true] {
        let mut reader = reader();
        let line = &reader.page().lines[0];
        let glyph = &line.glyphs[0];
        let x = (glyph.x + glyph.advance / 2.0) as u32;
        let y = (line.baseline - line.ascent / 2.0) as u32;
        let mut ui = ReaderUi::default();
        open_word_selection(&mut ui, &mut reader);
        if after_pick {
            ui.tap(&mut reader, x, y).unwrap();
        }
        reader
            .dispatch(femto_reader::controller::Action::Next)
            .unwrap();
        let before = serde_json::to_value(&reader.state).unwrap();
        if after_pick {
            ui.turn(&mut reader, true).unwrap();
            ui.select(&mut reader).unwrap();
        } else {
            ui.tap(&mut reader, x, y).unwrap();
        }
        assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
        assert_eq!(ui.error_message(), Some("document: word selection expired"));
    }
}

#[test]
fn panning_exposes_footer_covered_word_targets_without_reflow() {
    let mut state = ReaderState::default();
    state.layout.margin = 0;
    let mut reader = Reader::new(
        Document::from_text("Bottom words", &"selectable words ".repeat(500)).unwrap(),
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        ),
        state,
    )
    .unwrap();
    let height = reader.state.layout.height;
    let (x, source_y) = reader
        .page()
        .lines
        .iter()
        .find_map(|line| {
            let y = (line.baseline + line.descent - 1.0) as u32;
            (y >= height - 32 && y < height).then(|| {
                let glyph = &line.glyphs[0];
                ((glyph.x + glyph.advance / 2.0) as u32, y)
            })
        })
        .expect("fixture must place a word in the covered strip");
    let range = reader.word_at(x, source_y).unwrap();
    let anchor = reader.position();
    let before = reader.render(2).unwrap();
    let mut ui = ReaderUi::default();
    open_word_selection(&mut ui, &mut reader);
    ui.turn(&mut reader, true).unwrap();
    let panned = ui.render(&mut reader, 2).unwrap();
    let body = ((height - 32) * reader.state.layout.width) as usize;
    assert_eq!(
        &panned[..body],
        &before[32 * reader.state.layout.width as usize..]
    );
    ui.tap(&mut reader, x, source_y - 32).unwrap();
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    assert_eq!(
        reader.state.books[&reader.document.metadata.id].clippings[0].range,
        range
    );
    assert_eq!(reader.position(), anchor);
    assert_eq!(reader.render(2).unwrap(), before);
}

#[test]
fn clipping_source_action_returns_to_the_saved_text_and_allows_back() {
    let mut reader = reader();
    reader.clip_page_text(42).unwrap();
    let target = reader.state.books[&reader.document.metadata.id].clippings[0]
        .range
        .start;
    reader
        .dispatch(femto_reader::controller::Action::Next)
        .unwrap();
    let origin = reader.position();
    assert_ne!(origin, target);
    let mut ui = ReaderUi::default();
    go(&mut ui, &mut reader, &["This book", "Clippings"]);
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    ui.turn(&mut reader, true).unwrap(); // Go to source follows Delete excerpt.
    ui.select(&mut reader).unwrap();
    assert!(!ui.is_open());
    assert_eq!(reader.position(), target);
    ui.back(&mut reader).unwrap();
    assert_eq!(reader.position(), origin);
}

#[test]
fn clipping_delete_requires_confirmation_and_persists_without_moving_reading() {
    let mut reader = reader();
    reader.clip_page_text(42).unwrap();
    let before = serde_json::to_value(&reader.state).unwrap();
    let anchor = reader.position();
    let pixels = reader.render(4).unwrap();
    let mut ui = ReaderUi::default();
    open_clipping_delete(&mut ui, &mut reader);
    let confirmation = ui.render(&mut reader, 4).unwrap();
    assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
    ui.select(&mut reader).unwrap(); // Keep is selected by default.
    assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
    open_clipping_delete(&mut ui, &mut reader);
    ui.back(&mut reader).unwrap();
    assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
    open_clipping_delete(&mut ui, &mut reader);
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    let id = reader.document.metadata.id.clone();
    assert!(reader.state.books[&id].clippings.is_empty());
    assert_ne!(ui.render(&mut reader, 4).unwrap(), confirmation);
    assert_eq!(reader.position(), anchor);
    assert_eq!(reader.render(4).unwrap(), pixels);
    let root = tempfile::tempdir().unwrap();
    let host = root.path().join("state.json");
    reader.state.save(&host).unwrap();
    let (mut journal, _) = femto_reader::journal::StateJournal::open(root.path()).unwrap();
    journal.save(&reader.state).unwrap();
    assert!(
        ReaderState::load(host).unwrap().books[&id]
            .clippings
            .is_empty()
    );
    assert!(
        femto_reader::journal::StateJournal::open(root.path())
            .unwrap()
            .1
            .books[&id]
            .clippings
            .is_empty()
    );
}

#[test]
fn clipping_delete_rejects_a_changed_record_or_active_book() {
    for change_book in [false, true] {
        let mut reader = reader();
        reader.clip_page_text(42).unwrap();
        let id = reader.document.metadata.id.clone();
        let mut ui = ReaderUi::default();
        open_clipping_delete(&mut ui, &mut reader);
        if change_book {
            reader
                .open_document(Document::from_text("Other", "Other text.").unwrap())
                .unwrap();
        } else {
            reader.state.books.get_mut(&id).unwrap().clippings[0].recorded_at = 43;
        }
        let before = serde_json::to_value(&reader.state).unwrap();
        ui.turn(&mut reader, true).unwrap();
        ui.select(&mut reader).unwrap();
        assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
        assert_eq!(reader.state.books[&id].clippings.len(), 1);
        ui.render(&mut reader, 4).unwrap();
    }
}

fn finish_open(ui: &mut ReaderUi, reader: &mut Reader) {
    for _ in 0..16_384 {
        if !ui.is_opening() {
            return;
        }
        ui.poll_open(reader);
    }
    assert!(!ui.is_opening(), "opening did not finish");
}

fn finish_reflow(ui: &mut ReaderUi, reader: &mut Reader) {
    for _ in 0..16_384 {
        if !ui.is_reflowing() {
            return;
        }
        ui.poll_reflow(reader);
    }
    assert!(!ui.is_reflowing());
}

#[test]
fn center_tap_and_button_selection_open_and_close_the_same_menu() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    assert_eq!(ui.tap(&mut reader, 240, 400).unwrap(), Effect::Redraw);
    assert!(ui.is_open());
    let pixels = ui.render(&mut reader, 2).unwrap();
    assert_eq!(pixels.len(), 480 * 800);
    assert!(pixels.iter().all(|p| matches!(p, 0 | 255)));
    assert!(
        pixels[100 * 480..200 * 480]
            .iter()
            .filter(|p| **p == 0)
            .count()
            > 30_000
    );
    ui.select(&mut reader).unwrap();
    assert!(!ui.is_open());
    ui.select(&mut reader).unwrap();
    assert!(ui.is_open());
    ui.tap(&mut reader, 240, 50).unwrap();
    assert!(!ui.is_open());
}

#[test]
fn typography_sliders_and_toggles_set_values_by_tap_and_step_by_selection() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    go(&mut ui, &mut reader, &["Typography"]);
    assert_eq!(reader.state.layout.font_size, 24.0);
    // Text size has twelve values across the row; 24 px is the sixth.
    for (x, size) in [
        (260, 26.0),
        (220, 24.0),
        (479, 40.0),
        (0, 14.0),
        (220, 24.0),
    ] {
        ui.tap(&mut reader, x, 150).unwrap();
        assert!(ui.is_reflowing());
        finish_reflow(&mut ui, &mut reader);
        assert_eq!(reader.state.layout.font_size, size);
    }
    assert_eq!(ui.tap(&mut reader, 220, 150).unwrap(), Effect::Redraw);
    assert!(!ui.is_reflowing());
    ui.tap(&mut reader, 240, 450).unwrap();
    assert_eq!(reader.state.text_raster.stroke, 0.2);
    ui.tap(&mut reader, 240, 550).unwrap();
    assert_eq!(reader.state.text_raster.coverage_gamma, 0.85);
    // Selection steps the highlighted slider and wraps.
    for expected in [0.7, 1.0, 0.85] {
        ui.select(&mut reader).unwrap();
        assert_eq!(reader.state.text_raster.coverage_gamma, expected);
    }
    ui.tap(&mut reader, 400, 750).unwrap();
    ui.tap(&mut reader, 240, 150).unwrap();
    assert!(!reader.state.text_raster.hinting);
    ui.tap(&mut reader, 240, 250).unwrap();
    assert!(reader.state.text_raster.fractional);
    reader.state.text_raster.validate().unwrap();
    assert!(ui.is_open());
    assert_eq!(ui.render(&mut reader, 4).unwrap().len(), 384_000);
    ui.tap(&mut reader, 240, 50).unwrap();
    ui.tap(&mut reader, 240, 50).unwrap();
    assert!(!ui.is_open());
}

#[test]
fn hyphenation_menu_reflow_cancels_and_persists_without_moving_source_anchor() {
    use femto_reader::layout::Hyphenation;
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    go(&mut ui, &mut reader, &["Typography"]);
    let position = reader.position();
    let pixels = reader.render(2).unwrap();
    ui.tap(&mut reader, 240, 650).unwrap(); // Automatic hyphenation.
    assert!(ui.is_reflowing());
    assert_eq!(reader.state.layout.hyphenation, Hyphenation::Book);
    ui.back(&mut reader).unwrap();
    assert!(!ui.is_reflowing());
    assert_eq!(reader.render(2).unwrap(), pixels);
    for expected in [Hyphenation::Off, Hyphenation::English] {
        ui.tap(&mut reader, 240, 650).unwrap();
        assert!(ui.is_reflowing());
        finish_reflow(&mut ui, &mut reader);
        assert_eq!(reader.state.layout.hyphenation, expected);
        assert_eq!(reader.position(), position);
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.json");
    reader.state.save(&path).unwrap();
    assert_eq!(
        ReaderState::load(&path).unwrap().layout.hyphenation,
        Hyphenation::English
    );
}

#[test]
fn bookmarks_and_empty_lists_are_navigable_without_touch() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.select(&mut reader).unwrap();
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap(); // Empty TXT contents.
    assert!(ui.is_open());
    assert_eq!(ui.select(&mut reader).unwrap(), Effect::Unchanged);
    ui.render(&mut reader, 2).unwrap();
    ui.back(&mut reader).unwrap();
    ui.turn(&mut reader, true).unwrap(); // Back selected Contents again.
    ui.select(&mut reader).unwrap(); // Bookmarks.
    ui.select(&mut reader).unwrap(); // Toggle bookmark.
    assert!(!ui.is_open());
    assert_eq!(
        reader.state.books[&reader.document.metadata.id]
            .bookmarks
            .len(),
        1
    );
    ui.turn(&mut reader, true).unwrap();
    assert_eq!(reader.page_index(), 1);
    go(&mut ui, &mut reader, &["Bookmarks"]);
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    assert_eq!(reader.page_index(), 0);
    assert!(!ui.is_open());
}

#[test]
fn library_switching_preserves_positions_and_keeps_failed_targets_in_the_menu() {
    use femto_reader::state::Library;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("shelf")).unwrap();
    let first = dir.path().join("Alpha.txt");
    let second = dir.path().join("shelf/Beta.txt");
    std::fs::write(
        &first,
        include_str!("../../../tests/fixtures/device-reader.txt"),
    )
    .unwrap();
    std::fs::write(&second, "A different book. ".repeat(500)).unwrap();
    let mut reader = reader();
    let first_id = reader.document.metadata.id.clone();
    let mut ui = ReaderUi::default();
    ui.turn(&mut reader, true).unwrap();
    let position = reader.page().start;
    reader.record_position(10);
    ui.set_library(Library::scan(dir.path()).unwrap());
    ui.open_library();
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader); // All books.
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader); // Beta.
    assert!(!ui.is_open());
    assert_ne!(reader.document.metadata.id, first_id);
    assert_eq!(reader.state.books[&first_id].position, position);
    reader.record_position(11);
    ui.open_library();
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader); // Recent.
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader); // Alpha is the older entry.
    assert_eq!(reader.document.metadata.id, first_id);
    assert_eq!(reader.page().start, position);
    ui.open_library();
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader);
    ui.turn(&mut reader, true).unwrap();
    std::fs::remove_file(&second).unwrap();
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader);
    assert!(ui.is_open());
    assert_eq!(reader.document.metadata.id, first_id);
    assert_eq!(reader.page().start, position);
    ui.render(&mut reader, 2).unwrap();
    ui.back(&mut reader).unwrap(); // Library.
    ui.turn(&mut reader, true).unwrap();
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader); // Folders.
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader); // Root folder contains Alpha only.
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader);
    assert!(!ui.is_open());
    assert_eq!(reader.document.metadata.id, first_id);
}

#[test]
fn frontlight_sliders_set_levels_by_tap_and_step_by_selection() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    go(&mut ui, &mut reader, &["Frontlight"]);
    let plain = ui.render(&mut reader, 2).unwrap();
    // Each slider has one tap target per value across the row.
    for (x, brightness) in [(0, 0), (479, 10), (240, 5), (43, 0), (44, 1)] {
        ui.tap(&mut reader, x, 150).unwrap();
        assert_eq!(reader.state.frontlight.brightness, brightness);
        assert!(ui.take_light_request());
    }
    for (x, warmth) in [(479, 9), (0, 0), (240, 5)] {
        ui.tap(&mut reader, x, 250).unwrap();
        assert_eq!(reader.state.frontlight.warmth, warmth);
    }
    assert_eq!(reader.state.frontlight.brightness, 1);
    assert_ne!(ui.render(&mut reader, 2).unwrap(), plain);
    // Selection steps the highlighted slider up and wraps to zero.
    for expected in [6, 7, 8, 9, 0, 1] {
        ui.select(&mut reader).unwrap();
        assert_eq!(reader.state.frontlight.warmth, expected);
    }
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    assert_eq!(reader.state.frontlight.brightness, 0);
    assert!(ui.take_light_request());
    assert!(!ui.take_light_request());
    assert!(ui.is_open());
    assert_eq!(ui.render(&mut reader, 2).unwrap().len(), 480 * 800);
}

#[test]
fn swipes_page_menus_and_power_sleeps_only_while_reading() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    // Without a way to sleep the power button opens the menu.
    ui.power(&mut reader).unwrap();
    assert!(ui.is_open() && !ui.take_sleep_request());
    ui.back(&mut reader).unwrap();
    ui.set_sleep_available(true);
    ui.power(&mut reader).unwrap();
    assert!(!ui.is_open() && ui.take_sleep_request());
    assert_eq!(ui.render_sleep(&mut reader, 2).unwrap().len(), 480 * 800);

    ui.open();
    let first = ui.render(&mut reader, 2).unwrap();
    assert_eq!(ui.swipe(&mut reader, false).unwrap(), Effect::Unchanged);
    assert_eq!(ui.swipe(&mut reader, true).unwrap(), Effect::Redraw);
    let second = ui.render(&mut reader, 2).unwrap();
    assert_ne!(first, second);
    assert_eq!(ui.swipe(&mut reader, true).unwrap(), Effect::Unchanged);
    while ui.swipe(&mut reader, false).unwrap() == Effect::Redraw {}
    assert_eq!(ui.render(&mut reader, 2).unwrap(), first);
    // Inside a menu the power button selects: the first row resumes reading.
    ui.power(&mut reader).unwrap();
    assert!(!ui.is_open() && !ui.take_sleep_request());

    let page = reader.page_index();
    ui.swipe(&mut reader, true).unwrap();
    assert_eq!(reader.page_index(), page + 1);
}

#[test]
fn search_keyboard_filters_and_opens_books_without_losing_the_query() {
    use femto_reader::state::Library;
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.set_library(Library::scan(&fixture).unwrap());
    ui.open_library();
    ui.tap(&mut reader, 240, 450).unwrap();
    assert!(ui.is_editing());
    ui.tap(&mut reader, 40, 150).unwrap(); // a
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader); // b
    ui.tap(&mut reader, 180, 750).unwrap(); // space
    assert_eq!(ui.query(), "ab ");
    ui.tap(&mut reader, 300, 750).unwrap(); // delete
    assert_eq!(ui.query(), "ab");
    // The layer key switches to capitals, then symbols, then back.
    ui.tap(&mut reader, 60, 750).unwrap();
    ui.tap(&mut reader, 40, 150).unwrap();
    assert_eq!(ui.query(), "abA");
    ui.tap(&mut reader, 60, 750).unwrap();
    ui.tap(&mut reader, 40, 150).unwrap();
    assert_eq!(ui.query(), "abA.");
    ui.tap(&mut reader, 60, 750).unwrap();
    ui.tap(&mut reader, 40, 150).unwrap();
    assert_eq!(ui.query(), "abA.a");
    for _ in 0..5 {
        ui.tap(&mut reader, 300, 750).unwrap();
    }
    assert_eq!(ui.query(), "");
    for c in "SAMPLE.EPUB".chars() {
        ui.type_char(c);
    }
    for levels in [2, 4] {
        let pixels = ui.render(&mut reader, levels).unwrap();
        assert_eq!(pixels.len(), 480 * 800);
        assert!(pixels.iter().all(|p| if levels == 2 {
            matches!(p, 0 | 255)
        } else {
            matches!(p, 0 | 85 | 170 | 255)
        }));
    }
    ui.tap(&mut reader, 420, 750).unwrap(); // find
    assert!(!ui.is_editing());
    ui.back(&mut reader).unwrap();
    assert!(ui.is_editing());
    assert_eq!(ui.query(), "SAMPLE.EPUB");
    ui.submit_search();
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader);
    assert!(!ui.is_open());
    assert_eq!(
        reader.document.metadata.id,
        Document::open(fixture.join("sample.epub"))
            .unwrap()
            .metadata
            .id
    );
}

#[test]
fn normalized_library_query_opens_the_original_filename() {
    use femto_reader::state::Library;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("Cafe\u{301}.txt");
    std::fs::write(&path, "The selected book has a decomposed filename.").unwrap();
    let expected = Document::open(&path).unwrap().metadata.id;
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.set_library(Library::scan(directory.path()).unwrap());
    ui.open_library();
    ui.tap(&mut reader, 240, 450).unwrap();
    assert!(ui.is_editing());
    for c in "CAFÉ".chars() {
        ui.type_char(c);
    }
    ui.submit_search();
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader);
    assert!(!ui.is_open());
    assert_eq!(reader.document.metadata.id, expected);
    assert!(path.exists());
}

#[test]
fn search_text_bounds_grapheme_deletion_and_empty_results() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    assert_eq!(ui.type_char('x'), Effect::Unchanged);
    ui.open_library();
    ui.tap(&mut reader, 240, 450).unwrap();
    for c in "e\u{301}👩\u{200d}💻".chars() {
        ui.type_char(c);
    }
    ui.delete_char();
    assert_eq!(ui.query(), "e\u{301}");
    ui.delete_char();
    assert_eq!(ui.query(), "");
    for _ in 0..31 {
        ui.type_char('é');
    }
    assert_eq!(ui.type_char('€'), Effect::Unchanged);
    assert_eq!(ui.type_char('é'), Effect::Redraw);
    assert_eq!(ui.type_char('a'), Effect::Unchanged);
    assert_eq!(ui.type_char('\n'), Effect::Unchanged);
    assert_eq!(ui.query().len(), 64);
    ui.render(&mut reader, 2).unwrap();
    ui.submit_search();
    assert_eq!(ui.select(&mut reader).unwrap(), Effect::Unchanged);
    ui.render(&mut reader, 2).unwrap();
    ui.back(&mut reader).unwrap();
    assert_eq!(ui.query().len(), 64);
    ui.back(&mut reader).unwrap();
    assert!(!ui.is_editing());
}

#[test]
fn rescan_publishes_atomically_and_cancel_or_failure_preserves_the_library() {
    use femto_reader::state::Library;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("old.txt"), "Old book").unwrap();
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.configure_library_scan(dir.path().to_path_buf(), 256)
        .unwrap();
    ui.set_library(Library::scan(dir.path()).unwrap());
    ui.open_library();
    std::fs::write(dir.path().join("new.txt"), "New book").unwrap();
    ui.tap(&mut reader, 240, 550).unwrap();
    assert!(ui.is_scanning());
    assert_eq!(ui.poll_scan(), Effect::Unchanged);
    ui.render(&mut reader, 2).unwrap();
    ui.back(&mut reader).unwrap();
    assert!(!ui.is_scanning());
    assert_eq!(ui.poll_scan(), Effect::Unchanged);
    ui.tap(&mut reader, 240, 150).unwrap(); // all books, still just old
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader);
    assert_eq!(reader.document.metadata.title, "old");
    ui.open_library();
    ui.tap(&mut reader, 240, 550).unwrap();
    for _ in 0..10 {
        ui.poll_scan();
    }
    assert!(!ui.is_scanning());
    ui.tap(&mut reader, 240, 150).unwrap();
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader);
    assert_eq!(reader.document.metadata.title, "new");
    std::fs::remove_dir_all(dir.path()).unwrap();
    ui.open_library();
    ui.tap(&mut reader, 240, 550).unwrap();
    assert!(!ui.is_scanning());
    ui.render(&mut reader, 2).unwrap();
    // The old listing survives failure; selecting its now-missing file leaves the current book open.
    ui.tap(&mut reader, 240, 150).unwrap();
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader);
    assert!(ui.is_open());
    assert_eq!(reader.document.metadata.title, "new");
}

#[test]
fn scan_diagnostics_are_readable_through_paged_details() {
    use femto_reader::state::Library;
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.set_library(Library {
        errors: vec!["An unreadable book. ".repeat(20)],
        ..Default::default()
    });
    ui.open_library();
    ui.tap(&mut reader, 240, 650).unwrap();
    ui.select(&mut reader).unwrap();
    let first = ui.render(&mut reader, 2).unwrap();
    ui.tap(&mut reader, 400, 750).unwrap();
    let second = ui.render(&mut reader, 2).unwrap();
    assert_ne!(first, second);
    ui.back(&mut reader).unwrap();
    ui.back(&mut reader).unwrap();
    ui.render(&mut reader, 2).unwrap();
}

#[test]
fn device_status_row_does_not_change_reading_position_or_settings() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    let before = reader.position();
    let state = serde_json::to_vec(&reader.state).unwrap();
    ui.set_device_status("75% 4000mV charging");
    ui.open();
    for _ in 0..8 {
        ui.turn(&mut reader, true).unwrap();
    }
    let image = ui.render(&mut reader, 2).unwrap();
    assert_eq!(image.len(), 480 * 800);
    assert_eq!(ui.select(&mut reader).unwrap(), Effect::Unchanged);
    assert!(ui.is_open());
    assert_eq!(reader.position(), before);
    assert_eq!(serde_json::to_vec(&reader.state).unwrap(), state);
}

#[test]
fn sleep_menu_emits_one_request_without_changing_saved_frontlight_or_position() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.set_sleep_available(true);
    ui.open();
    for _ in 0..8 {
        ui.turn(&mut reader, true).unwrap();
    }
    let state = serde_json::to_vec(&reader.state).unwrap();
    let before = reader.position();
    assert!(!ui.take_sleep_request());
    assert_eq!(ui.select(&mut reader).unwrap(), Effect::Unchanged);
    assert!(ui.take_sleep_request());
    assert!(!ui.take_sleep_request());
    assert_eq!(reader.position(), before);
    assert_eq!(serde_json::to_vec(&reader.state).unwrap(), state);
}

#[test]
fn catalog_menu_emits_events_and_uses_existing_touch_pagination() {
    use femto_reader::ui::CatalogEvent;
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.set_catalog_available(true);
    ui.open();
    for _ in 0..4 {
        ui.turn(&mut reader, true).unwrap();
    }
    ui.select(&mut reader).unwrap();
    assert_eq!(ui.take_catalog_event(), Some(CatalogEvent::Open));
    assert_eq!(ui.take_catalog_event(), None);
    let rows = (0..8).map(|n| format!("Catalog {n}")).collect::<Vec<_>>();
    ui.set_catalog_view("Available books", &rows).unwrap();
    assert!(ui.is_catalog_open());
    ui.tap(&mut reader, 400, 750).unwrap();
    ui.tap(&mut reader, 240, 150).unwrap();
    assert_eq!(ui.take_catalog_event(), Some(CatalogEvent::Activate(6)));
    ui.tap(&mut reader, 240, 50).unwrap();
    assert_eq!(ui.take_catalog_event(), Some(CatalogEvent::Back));
    assert!(ui.is_catalog_open());
    assert_eq!(reader.page_index(), 0);
    assert!(
        ui.set_catalog_view("Oversize", &vec![String::new(); 2049])
            .is_err()
    );
    ui.set_catalog_view("Empty", &[]).unwrap();
    ui.select(&mut reader).unwrap();
    assert_eq!(ui.take_catalog_event(), None);
    ui.back(&mut reader).unwrap();
    assert_eq!(ui.take_catalog_event(), Some(CatalogEvent::Back));
    ui.open();
    assert!(!ui.is_catalog_open());
}

#[test]
fn catalog_search_keyboard_preserves_library_query_and_emits_submission() {
    use femto_reader::ui::CatalogEvent;
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.open_library();
    for _ in 0..3 {
        ui.turn(&mut reader, true).unwrap();
    }
    ui.select(&mut reader).unwrap();
    ui.type_char('l');
    assert_eq!(ui.query(), "l");
    ui.open_catalog_search();
    assert!(ui.is_catalog_open() && ui.is_editing());
    assert_eq!(ui.query(), "");
    ui.tap(&mut reader, 10, 150).unwrap();
    assert_eq!(ui.query(), "a");
    for _ in 0..70 {
        ui.type_char('b');
    }
    assert_eq!(ui.query().len(), 64);
    for _ in 0..64 {
        ui.tap(&mut reader, 300, 750).unwrap();
    }
    assert_eq!(ui.query(), "");
    ui.type_char('c');
    ui.tap(&mut reader, 420, 750).unwrap();
    assert_eq!(ui.take_catalog_event(), Some(CatalogEvent::Search));
    assert_eq!(ui.query(), "c");
    let before = ui.render(&mut reader, 2).unwrap();
    ui.show_error("Invalid search".into());
    let after = ui.render(&mut reader, 2).unwrap();
    assert_ne!(before, after);
    ui.tap(&mut reader, 240, 50).unwrap();
    assert_eq!(ui.take_catalog_event(), Some(CatalogEvent::Back));
    ui.open();
    assert_eq!(ui.query(), "l");
}

#[test]
fn automatic_sleep_menu_selects_timeout_without_requesting_immediate_sleep() {
    use femto_reader::state::IdleSleep;
    let mut reader = reader();
    let before = reader.position();
    let mut ui = ReaderUi::default();
    ui.set_sleep_available(true);
    go(&mut ui, &mut reader, &["Settings", "Automatic sleep"]);
    ui.select(&mut reader).unwrap();
    assert_eq!(reader.state.idle_sleep, IdleSleep::Off);
    for expected in [
        IdleSleep::FiveMinutes,
        IdleSleep::TenMinutes,
        IdleSleep::ThirtyMinutes,
        IdleSleep::SixtyMinutes,
    ] {
        ui.turn(&mut reader, true).unwrap();
        ui.select(&mut reader).unwrap();
        assert_eq!(reader.state.idle_sleep, expected);
    }
    assert!(!ui.take_sleep_request());
    assert_eq!(reader.position(), before);
}

#[test]
fn tailnet_menu_is_opt_in_and_emits_explicit_actions() {
    use femto_reader::ui::TailnetEvent;
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.set_tailnet_available(true);
    ui.open();
    ui.choose(&mut reader, "Settings").unwrap();
    ui.choose(&mut reader, "Tailnet").unwrap();
    assert_eq!(ui.take_tailnet_event(), Some(TailnetEvent::Open));
    ui.open_tailnet();
    ui.set_tailnet_status(&[
        "Approval required".into(),
        "https://example.com/approve".into(),
    ]);
    ui.select(&mut reader).unwrap();
    assert_eq!(ui.take_tailnet_event(), Some(TailnetEvent::Connect));
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    assert_eq!(ui.take_tailnet_event(), Some(TailnetEvent::Provision));
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    assert_eq!(ui.take_tailnet_event(), Some(TailnetEvent::Stop));
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    assert_eq!(ui.take_tailnet_event(), Some(TailnetEvent::Renew));
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    assert_eq!(ui.take_tailnet_event(), None);
    assert_eq!(ui.render(&mut reader, 2).unwrap().len(), 480 * 800);
    ui.back(&mut reader).unwrap();
    assert!(!ui.is_tailnet_open());
}

fn landscape_reader(width: u32, height: u32) -> Reader {
    let mut reader = reader();
    let mut layout = reader.state.layout;
    layout.width = width;
    layout.height = height;
    reader
        .dispatch(femto_reader::controller::Action::Reflow(layout))
        .unwrap();
    reader
}
#[test]
fn landscape_catalog_pages_and_footer_edges_reach_all_entries() {
    use femto_reader::ui::CatalogEvent;
    let mut reader = landscape_reader(800, 480);
    let mut ui = ReaderUi::default();
    ui.set_catalog_view(
        "Landscape",
        &(0..13).map(|i| format!("Book {i}")).collect::<Vec<_>>(),
    )
    .unwrap();
    for page in 0..4 {
        assert_eq!(ui.render(&mut reader, 2).unwrap().len(), 800 * 480);
        for row in 0..4 {
            let index = page * 4 + row;
            ui.tap(&mut reader, 100, (row as u32 + 1) * 80).unwrap();
            assert_eq!(
                ui.take_catalog_event(),
                (index < 13).then_some(CatalogEvent::Activate(index))
            );
        }
        if page < 3 {
            ui.tap(&mut reader, 799, 479).unwrap();
        }
    }
    ui.tap(&mut reader, 0, 479).unwrap();
    ui.tap(&mut reader, 0, 80).unwrap();
    assert_eq!(ui.take_catalog_event(), Some(CatalogEvent::Activate(8)));
    ui.tap(&mut reader, 799, 0).unwrap();
    assert_eq!(ui.take_catalog_event(), Some(CatalogEvent::Back));
}
#[test]
fn landscape_keyboard_letters_digits_and_actions_share_rendered_cells() {
    use femto_reader::ui::CatalogEvent;
    let mut reader = landscape_reader(800, 480);
    let mut ui = ReaderUi::default();
    ui.open_catalog_search();
    let before = ui.render(&mut reader, 2).unwrap();
    for (x, y) in [(0, 80), (799, 319), (799, 399)] {
        ui.tap(&mut reader, x, y).unwrap();
    }
    assert_eq!(ui.query(), "a09");
    assert_ne!(ui.render(&mut reader, 2).unwrap(), before);
    ui.tap(&mut reader, 200, 479).unwrap();
    assert_eq!(ui.query(), "a09 ");
    ui.tap(&mut reader, 400, 479).unwrap();
    assert_eq!(ui.query(), "a09");
    for _ in 0..3 {
        ui.tap(&mut reader, 400, 479).unwrap();
    }
    assert_eq!(ui.query(), "");
    ui.tap(&mut reader, 799, 479).unwrap();
    assert_eq!(ui.take_catalog_event(), Some(CatalogEvent::Search));
    ui.tap(&mut reader, 0, 79).unwrap();
    assert_eq!(ui.take_catalog_event(), Some(CatalogEvent::Back));
}
#[test]
fn odd_viewport_sizes_cover_the_bottom_edge_and_ignore_unused_keys() {
    use femto_reader::ui::CatalogEvent;
    let mut reader = landscape_reader(803, 483);
    let mut ui = ReaderUi::default();
    ui.open_catalog_search();
    assert_eq!(ui.render(&mut reader, 2).unwrap().len(), 803 * 483);
    ui.tap(&mut reader, 803, 482).unwrap();
    assert_eq!(ui.take_catalog_event(), None);
    ui.tap(&mut reader, 802, 482).unwrap();
    assert_eq!(ui.take_catalog_event(), Some(CatalogEvent::Search));
    let mut reader = landscape_reader(801, 561);
    ui.open_catalog_search();
    assert_eq!(ui.render(&mut reader, 2).unwrap().len(), 801 * 561);
    ui.tap(&mut reader, 800, 479).unwrap();
    assert_eq!(ui.query(), "");
    assert_eq!(ui.take_catalog_event(), None);
}

#[test]
fn orientation_menu_emits_changes_only_when_enabled() {
    use femto_reader::state::Rotation;
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.set_rotation_available(true);
    go(&mut ui, &mut reader, &["Settings", "Screen orientation"]);
    for rotation in [
        Rotation::Portrait,
        Rotation::Clockwise,
        Rotation::Inverted,
        Rotation::Counterclockwise,
    ] {
        ui.select(&mut reader).unwrap();
        assert_eq!(ui.take_rotation_request(), Some(rotation));
        assert_eq!(reader.state.rotation, Rotation::Portrait);
        reader
            .dispatch(femto_reader::controller::Action::Rotate(rotation))
            .unwrap();
        assert_eq!(ui.render(&mut reader, 2).unwrap().len(), 480 * 800);
        reader
            .dispatch(femto_reader::controller::Action::Rotate(Rotation::Portrait))
            .unwrap();
        ui.turn(&mut reader, true).unwrap();
    }
}

#[test]
fn library_opening_can_be_cancelled_before_it_replaces_the_current_book() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("Destination.txt"),
        "Destination paragraph.\n\n".repeat(400),
    )
    .unwrap();
    let mut reader = reader();
    let original = reader.document.metadata.id.clone();
    let position = reader.position();
    let pixels = reader.render(2).unwrap();
    let mut ui = ReaderUi::default();
    ui.set_library(femto_reader::state::Library::scan(directory.path()).unwrap());
    for (cancel_by_back, polls) in [(true, 1), (false, 1), (true, 2), (false, 2)] {
        ui.open_library();
        ui.select(&mut reader).unwrap();
        ui.select(&mut reader).unwrap();
        assert!(ui.is_opening());
        for _ in 0..polls {
            assert_eq!(ui.poll_open(&mut reader), Effect::Unchanged);
        }
        assert!(ui.is_opening());
        assert_eq!(reader.document.metadata.id, original);
        assert_eq!(ui.render(&mut reader, 2).unwrap().len(), 480 * 800);
        if !cancel_by_back {
            // Other input waits for the pending book.
            ui.select(&mut reader).unwrap();
            assert!(ui.is_opening());
        }
        ui.back(&mut reader).unwrap();
        assert!(!ui.is_opening());
        assert!(ui.is_open()); // The book list never left the screen.
        assert_eq!(ui.poll_open(&mut reader), Effect::Unchanged);
        assert_eq!(reader.position(), position);
        assert_eq!(reader.render(2).unwrap(), pixels);
    }
    ui.open_library();
    ui.select(&mut reader).unwrap();
    ui.select(&mut reader).unwrap();
    finish_open(&mut ui, &mut reader);
    assert!(!ui.is_open());
    assert_ne!(reader.document.metadata.id, original);
}

#[test]
fn resizing_keeps_the_page_until_done_and_back_abandons_it() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    let before = reader.state.layout;
    let position = reader.position();
    let raster = reader.render(2).unwrap();
    for back in [true, false] {
        ui.resize(&reader, 2.0).unwrap();
        assert!(ui.is_reflowing());
        assert_eq!(ui.poll_reflow(&mut reader), Effect::Unchanged);
        assert_eq!(reader.state.layout, before);
        if !back {
            // Other input waits for the pending layout.
            ui.select(&mut reader).unwrap();
            assert!(ui.is_reflowing() && !ui.is_open());
        }
        // The page stays visible with the busy mark until Back abandons the change.
        assert_ne!(ui.render(&mut reader, 2).unwrap(), raster);
        ui.back(&mut reader).unwrap();
        assert!(!ui.is_reflowing());
        assert!(!ui.is_open());
        assert_eq!(reader.position(), position);
        assert_eq!(ui.render(&mut reader, 2).unwrap(), raster);
    }
    ui.resize(&reader, 2.0).unwrap();
    finish_reflow(&mut ui, &mut reader);
    assert!(!ui.is_open());
    assert_eq!(reader.state.layout.font_size, before.font_size + 2.0);
    assert_eq!(reader.position(), position);
}

#[test]
fn link_menu_preview_closes_without_navigation_and_follows_explicitly() {
    use femto_reader::controller::Action;
    for (width, height) in [(480, 800), (800, 480), (803, 483)] {
        let mut reader = Reader::new(
            Document::open(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/supplemental-notes.epub"),
            )
            .unwrap(),
            Fonts::new(
                Font::from_bytes(
                    include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec(),
                )
                .unwrap(),
            ),
            ReaderState::default(),
        )
        .unwrap();
        let mut config = reader.state.layout;
        config.width = width;
        config.height = height;
        reader.dispatch(Action::Reflow(config)).unwrap();
        let origin = reader.position();
        let mut ui = ReaderUi::default();
        go(&mut ui, &mut reader, &["This book", "Links on this page"]);
        ui.select(&mut reader).unwrap();
        assert_eq!(ui.resize(&reader, 2.0).unwrap(), Effect::Unchanged);
        finish_preview(&mut ui, &mut reader);
        assert_eq!(ui.resize(&reader, 2.0).unwrap(), Effect::Unchanged);
        assert!(!ui.is_reflowing());
        let pixels = ui.render(&mut reader, 4).unwrap();
        assert_eq!(pixels.len(), (width * height) as usize);
        assert!(pixels.iter().all(|p| [0, 85, 170, 255].contains(p)));
        assert_eq!(reader.position(), origin);
        ui.tap(&mut reader, 1, height - 1).unwrap();
        assert_eq!(reader.position(), origin);
        assert_eq!(reader.dispatch(Action::Back).unwrap(), Effect::Unchanged);
        ui.select(&mut reader).unwrap();
        finish_preview(&mut ui, &mut reader);
        let preview_pixels = ui.render(&mut reader, 4).unwrap();
        for back in [true, false] {
            ui.tap(&mut reader, width - 1, height - 1).unwrap();
            assert!(ui.is_navigating());
            assert_eq!(reader.position(), origin);
            if !back {
                // Other input waits for the pending move.
                ui.select(&mut reader).unwrap();
                assert!(ui.is_navigating());
            }
            ui.back(&mut reader).unwrap();
            assert!(!ui.is_navigating());
            assert_eq!(ui.render(&mut reader, 4).unwrap(), preview_pixels);
            assert_eq!(reader.dispatch(Action::Back).unwrap(), Effect::Unchanged);
        }
        ui.tap(&mut reader, width - 1, height - 1).unwrap();
        finish_navigation(&mut ui, &mut reader);
        assert!(!ui.is_open());
        assert!(reader.position().chapter >= reader.document.metadata.chapters.len());
        reader.dispatch(Action::Back).unwrap();
        assert_eq!(reader.position(), origin);
    }
}

#[test]
fn tapping_note_references_previews_and_closes_to_reading_while_ordinary_links_jump() {
    use femto_reader::controller::Action;
    let mut reader = Reader::new(
        Document::open(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/noterefs.epub"),
        )
        .unwrap(),
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        ),
        ReaderState::default(),
    )
    .unwrap();
    let origin = reader.position();
    let mut ui = ReaderUi::default();
    let coordinates = |reader: &Reader, index: usize| {
        (0..reader.state.layout.height)
            .find_map(|y| {
                (0..reader.state.layout.width)
                    .find_map(|x| (reader.link_at(x, y) == Some(index)).then_some((x, y)))
            })
            .expect("visible link")
    };
    for index in [0, 1] {
        let (x, y) = coordinates(&reader, index);
        ui.tap(&mut reader, x, y).unwrap();
        assert!(ui.is_preview_opening() && !ui.is_open());
        assert_eq!(reader.position(), origin);
        finish_preview(&mut ui, &mut reader);
        assert!(ui.is_open());
        ui.render(&mut reader, 4).unwrap();
        ui.back(&mut reader).unwrap();
        assert!(!ui.is_open());
        assert_eq!(reader.dispatch(Action::Back).unwrap(), Effect::Unchanged);
    }
    for index in [5, 6] {
        let (x, y) = coordinates(&reader, index);
        assert!(ui.tap(&mut reader, x, y).is_err());
        assert!(!ui.is_open());
        assert_eq!(reader.position(), origin);
    }
    let (x, y) = coordinates(&reader, 2);
    ui.tap(&mut reader, x, y).unwrap();
    assert!(ui.is_navigating());
    assert_eq!(reader.position(), origin);
    ui.back(&mut reader).unwrap();
    assert!(!ui.is_open());
    assert_eq!(reader.dispatch(Action::Back).unwrap(), Effect::Unchanged);
    ui.tap(&mut reader, x, y).unwrap();
    finish_navigation(&mut ui, &mut reader);
    assert!(!ui.is_open());
    assert_ne!(reader.position(), origin);
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.position(), origin);
    let (x, y) = coordinates(&reader, 0);
    ui.tap(&mut reader, x, y).unwrap();
    finish_preview(&mut ui, &mut reader);
    ui.select(&mut reader).unwrap();
    finish_navigation(&mut ui, &mut reader);
    assert!(!ui.is_open());
    assert_ne!(reader.position(), origin);
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.position(), origin);
}

#[test]
fn pending_preview_waits_for_input_and_back_abandons_it() {
    let mut reader = Reader::new(
        Document::open(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/navigation.epub"),
        )
        .unwrap(),
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        ),
        ReaderState::default(),
    )
    .unwrap();
    let origin = reader.position();
    let original_pixels = reader.render(2).unwrap();
    let mut ui = ReaderUi::default();
    for action in 0..3 {
        go(&mut ui, &mut reader, &["This book", "Links on this page"]);
        ui.select(&mut reader).unwrap();
        assert!(ui.is_preview_opening());
        assert_eq!(ui.poll_preview(&mut reader), Effect::Unchanged);
        ui.render(&mut reader, 2).unwrap();
        if action > 0 {
            // Other input waits for the pending preview.
            ui.select(&mut reader).unwrap();
            ui.turn(&mut reader, action == 1).unwrap();
            assert!(ui.is_preview_opening());
        }
        ui.back(&mut reader).unwrap();
        assert!(!ui.is_preview_opening());
        assert_eq!(ui.poll_preview(&mut reader), Effect::Unchanged);
        assert_eq!(reader.position(), origin);
        assert_eq!(reader.render(2).unwrap(), original_pixels);
    }
}

#[test]
fn rotation_reflow_can_cancel_without_changing_the_active_view() {
    use femto_reader::state::Rotation;
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    let before = reader.state.layout;
    let position = reader.position();
    let pixels = reader.render(2).unwrap();
    assert_eq!(
        ui.rotate(&reader, Rotation::Portrait).unwrap(),
        Effect::Unchanged
    );
    for back in [true, false] {
        ui.rotate(&reader, Rotation::Clockwise).unwrap();
        assert!(ui.is_reflowing());
        assert_eq!(ui.poll_reflow(&mut reader), Effect::Unchanged);
        assert_eq!(reader.state.rotation, Rotation::Portrait);
        assert_eq!(reader.state.layout, before);
        assert_eq!(
            ui.rotate(&reader, Rotation::Inverted).unwrap(),
            Effect::Unchanged
        );
        assert_eq!(ui.resize(&reader, 2.0).unwrap(), Effect::Unchanged);
        if !back {
            ui.select(&mut reader).unwrap();
            assert!(ui.is_reflowing());
        }
        ui.back(&mut reader).unwrap();
        assert!(!ui.is_reflowing());
        assert_eq!(reader.render(2).unwrap(), pixels);
        assert_eq!(reader.position(), position);
    }
    ui.rotate(&reader, Rotation::Clockwise).unwrap();
    finish_reflow(&mut ui, &mut reader);
    assert_eq!(reader.state.rotation, Rotation::Clockwise);
    assert_eq!(reader.state.layout.width, before.height);
    assert_eq!(reader.state.layout.height, before.width);
    assert_eq!(reader.position(), position);
    assert!(ui.render(&mut reader, 2).unwrap().contains(&0));
}

fn finish_navigation(ui: &mut ReaderUi, reader: &mut Reader) {
    for _ in 0..16_384 {
        if !ui.is_navigating() {
            return;
        }
        ui.poll_navigation(reader);
    }
    assert!(!ui.is_navigating());
}

#[test]
fn a_sync_question_is_not_asked_where_a_pending_page_turn_would_replace_it() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut reader = Reader::new(
        Document::open(root.join("navigation.epub")).unwrap(),
        Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap();
    let mut ui = ReaderUi::default();
    // Forward to the turn that leaves the first chapter.
    for _ in 0..64 {
        ui.turn(&mut reader, true).unwrap();
        if ui.is_navigating() {
            break;
        }
    }
    assert!(ui.is_navigating() && !ui.is_open());
    // The turn would close the question when it finished, so it is refused.
    assert!(!ui.ask_sync_conflict(40, Some(10)));
    assert!(!ui.is_open());
    finish_navigation(&mut ui, &mut reader);
    assert_eq!(reader.position().chapter, 1);
    // On a page at rest it is asked, and not again over itself.
    assert!(ui.ask_sync_conflict(40, Some(10)));
    assert!(ui.is_open());
    assert!(!ui.ask_sync_conflict(50, Some(10)));
    ui.choose(&mut reader, "Go to 40%").unwrap();
    assert!(ui.take_kosync_event().is_some());
}

#[test]
fn chapter_menu_and_boundary_taps_keep_the_view_until_navigation_finishes() {
    use femto_reader::controller::Action;
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut reader = Reader::new(
        Document::open(root.join("sample.epub")).unwrap(),
        Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap();
    let mut ui = ReaderUi::default();
    reader.dispatch(Action::Chapter(1)).unwrap();
    let before = reader.position();
    let pixels = reader.render(2).unwrap();
    go(&mut ui, &mut reader, &["This book", "Chapters"]);
    for back in [true, false] {
        ui.select(&mut reader).unwrap(); // First chapter.
        assert!(ui.is_navigating());
        assert_eq!(ui.poll_navigation(&mut reader), Effect::Unchanged);
        assert_eq!(reader.position(), before);
        assert_eq!(reader.render(2).unwrap(), pixels);
        assert_eq!(ui.resize(&reader, 2.0).unwrap(), Effect::Unchanged);
        if !back {
            // Other input waits for the pending move; the list stays visible.
            ui.select(&mut reader).unwrap();
            ui.turn(&mut reader, true).unwrap();
            assert!(ui.is_navigating());
        }
        ui.back(&mut reader).unwrap();
        assert!(!ui.is_navigating());
        assert!(ui.is_open()); // The chapter list was never replaced.
    }
    ui.select(&mut reader).unwrap();
    finish_navigation(&mut ui, &mut reader);
    assert!(!ui.is_open());
    assert_eq!(reader.position().chapter, 0);
    ui.back(&mut reader).unwrap();
    assert!(ui.is_navigating());
    finish_navigation(&mut ui, &mut reader);
    assert_eq!(reader.position(), before);
    ui.tap(&mut reader, 0, 799).unwrap(); // Previous chapter's final page.
    assert!(ui.is_navigating());
    finish_navigation(&mut ui, &mut reader);
    assert_eq!(reader.position().chapter, 0);
    assert_eq!(reader.page_index() + 1, reader.page_count());
    ui.turn(&mut reader, true).unwrap();
    assert!(ui.is_navigating());
    // The page stays visible while the next chapter is indexed; Back abandons it.
    assert!(!ui.is_open());
    ui.back(&mut reader).unwrap();
    assert!(!ui.is_navigating());
    assert_eq!(reader.position().chapter, 0);
}

#[test]
fn stale_preview_follow_returns_to_a_renderable_menu_on_poll_or_cancellation() {
    use femto_reader::controller::Action;
    for mode in 0..3 {
        let root =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        let mut reader = Reader::new(
            Document::open(root.join("supplemental-notes.epub")).unwrap(),
            Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap()),
            ReaderState::default(),
        )
        .unwrap();
        let mut ui = ReaderUi::default();
        go(&mut ui, &mut reader, &["This book", "Links on this page"]);
        ui.select(&mut reader).unwrap();
        finish_preview(&mut ui, &mut reader);
        ui.select(&mut reader).unwrap();
        assert!(ui.is_navigating());
        let mut layout = reader.state.layout;
        layout.font_size += 2.0;
        reader.dispatch(Action::Reflow(layout)).unwrap();
        let position = reader.position();
        let pixels = reader.render(4).unwrap();
        match mode {
            0 => {
                assert_eq!(ui.poll_navigation(&mut reader), Effect::Redraw);
            }
            1 => {
                ui.back(&mut reader).unwrap();
            }
            _ => {
                ui.select(&mut reader).unwrap();
                assert!(ui.is_navigating());
                assert_eq!(ui.poll_navigation(&mut reader), Effect::Redraw);
            }
        }
        assert!(!ui.is_navigating());
        assert!(ui.is_open());
        assert!(ui.render(&mut reader, 4).unwrap().contains(&0));
        ui.back(&mut reader).unwrap();
        assert!(!ui.is_open());
        assert_eq!(reader.position(), position);
        assert_eq!(reader.render(4).unwrap(), pixels);
        assert_eq!(reader.dispatch(Action::Back).unwrap(), Effect::Unchanged);
    }
}

#[test]
fn book_typography_reset_is_cancellable_atomic_and_persisted() {
    use femto_reader::controller::Action;
    use femto_reader::state::BookTypography;
    let mut reader = reader();
    reader.dispatch(Action::Next).unwrap();
    let mut next = reader.state.layout;
    next.font_size = 30.0;
    next.line_height = 1.6;
    next.margin = 32;
    next.hyphenation = femto_reader::layout::Hyphenation::German;
    reader.dispatch(Action::Reflow(next)).unwrap();
    reader.state.text_raster.stroke = 0.4;
    reader.state.text_raster.coverage_gamma = 0.7;
    reader.state.text_raster.fractional = true;
    reader.state.text_raster.hinting = false;
    reader.record_position(123);
    let position = reader.position();
    let original = serde_json::to_value(&reader.state).unwrap();
    let pixels = reader.render(4).unwrap();
    let mut ui = ReaderUi::default();
    go(&mut ui, &mut reader, &["Typography"]);
    ui.tap(&mut reader, 400, 750).unwrap(); // Second page.
    ui.tap(&mut reader, 240, 350).unwrap(); // Reset book typography.
    assert!(ui.is_reflowing());
    ui.poll_reflow(&mut reader);
    ui.back(&mut reader).unwrap();
    assert!(!ui.is_reflowing());
    assert_eq!(serde_json::to_value(&reader.state).unwrap(), original);
    assert_eq!(reader.render(4).unwrap(), pixels);
    ui.select(&mut reader).unwrap(); // The selection is still on the reset row.
    assert!(ui.is_reflowing());
    finish_reflow(&mut ui, &mut reader);
    let defaults = BookTypography::current(&ReaderState::default());
    assert_eq!(BookTypography::current(&reader.state), defaults);
    assert_eq!(reader.position(), position);
    reader.record_position(124);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reader.json");
    reader.state.save(&path).unwrap();
    let loaded = ReaderState::load(path).unwrap();
    assert_eq!(
        loaded.books[&reader.document.metadata.id].typography,
        Some(defaults)
    );
    // Resetting defaults again changes nothing.
    assert_eq!(ui.tap(&mut reader, 240, 350).unwrap(), Effect::Unchanged);
    assert!(!ui.is_reflowing());
}

#[test]
fn spacing_and_margin_sliders_cancel_and_persist_as_book_preferences() {
    use femto_reader::{controller::Action, state::BookTypography};
    let mut reader = reader();
    reader.dispatch(Action::Next).unwrap();
    let position = reader.position();
    let mut ui = ReaderUi::default();
    go(&mut ui, &mut reader, &["Typography"]);
    let pixels = reader.render(4).unwrap();
    // Line spacing has five values across its row.
    ui.tap(&mut reader, 48 + 96 * 3, 250).unwrap();
    assert!(ui.is_reflowing());
    assert_eq!(reader.state.layout.line_height, 1.35);
    ui.back(&mut reader).unwrap();
    assert_eq!(reader.render(4).unwrap(), pixels);
    for (index, expected) in [(3, 1.6), (4, 2.0), (0, 1.0), (1, 1.15), (2, 1.35)] {
        ui.tap(&mut reader, 48 + 96 * index, 250).unwrap();
        assert!(ui.is_reflowing());
        finish_reflow(&mut ui, &mut reader);
        assert_eq!(reader.state.layout.line_height, expected);
        assert_eq!(reader.position(), position);
    }
    // Margins have seven values.
    let margin_x = |index: u32| 34 + 480 * index / 7;
    ui.tap(&mut reader, margin_x(4), 350).unwrap();
    assert!(ui.is_reflowing());
    ui.back(&mut reader).unwrap();
    assert_eq!(reader.state.layout.margin, 24);
    for (index, expected) in [(4, 32), (6, 48), (0, 0), (1, 8), (3, 24), (4, 32)] {
        ui.tap(&mut reader, margin_x(index), 350).unwrap();
        assert!(ui.is_reflowing());
        finish_reflow(&mut ui, &mut reader);
        assert_eq!(reader.state.layout.margin, expected);
        assert_eq!(reader.position(), position);
    }
    reader.record_position(456);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reader.json");
    reader.state.save(&path).unwrap();
    let loaded = ReaderState::load(&path).unwrap();
    assert_eq!(
        loaded.books[&reader.document.metadata.id].typography,
        Some(BookTypography::current(&reader.state))
    );
}

#[test]
fn spacing_and_margin_controls_skip_presets_that_do_not_fit() {
    use femto_reader::controller::Action;
    let mut reader = reader();
    // One hundred pixels of height leaves room for three 24px lines only at 1.0.
    let config = femto_reader::layout::LayoutConfig {
        width: 480,
        height: 100,
        margin: 8,
        line_height: 1.0,
        ..reader.state.layout
    };
    reader.dispatch(Action::Reflow(config)).unwrap();
    let mut ui = ReaderUi::default();
    ui.open();
    // Button selection is independent of how many rows fit on a menu page.
    for _ in 0..5 {
        ui.turn(&mut reader, true).unwrap();
    }
    ui.select(&mut reader).unwrap(); // Typography.
    ui.turn(&mut reader, true).unwrap(); // Line spacing.
    ui.select(&mut reader).unwrap(); // 1.15 still fits.
    assert!(ui.is_reflowing());
    finish_reflow(&mut ui, &mut reader);
    assert_eq!(reader.state.layout.line_height, 1.15);
    ui.select(&mut reader).unwrap(); // Skip 1.35, 1.6, 2.0, wrap to 1.0.
    finish_reflow(&mut ui, &mut reader);
    assert_eq!(reader.state.layout.line_height, 1.0);
    ui.turn(&mut reader, true).unwrap(); // Margins.
    ui.select(&mut reader).unwrap(); // Larger margins cannot fit; wrap to zero.
    finish_reflow(&mut ui, &mut reader);
    assert_eq!(reader.state.layout.margin, 0);
    reader.state.layout.validate().unwrap();
}

fn dictionary_selection(word: &str) -> (Reader, ReaderUi) {
    let mut reader = Reader::new(
        Document::from_text("Lookup", word).unwrap(),
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        ),
        ReaderState::default(),
    )
    .unwrap();
    let mut ui = ReaderUi::default();
    ui.set_dictionary_path(Some(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/dictionary/example.ifo"
        )
        .into(),
    ));
    open_button_selection(&mut ui, &mut reader);
    ui.select(&mut reader).unwrap();
    (reader, ui)
}

fn start_dictionary(ui: &mut ReaderUi, reader: &mut Reader) {
    ui.choose(reader, "Look up selection").unwrap();
}

#[test]
fn dictionary_lookup_returns_to_selection_without_changing_reader_state() {
    for word in ["font", "fonts", "missing"] {
        let (mut reader, mut ui) = dictionary_selection(word);
        let before = serde_json::to_value(&reader.state).unwrap();
        let anchor = reader.position();
        let pixels = reader.render(4).unwrap();
        let preview = ui.render(&mut reader, 4).unwrap();
        start_dictionary(&mut ui, &mut reader);
        assert!(ui.is_dictionary_opening());
        for _ in 0..10 {
            ui.poll_dictionary(&mut reader);
            if !ui.is_dictionary_opening() {
                break;
            }
        }
        assert!(!ui.is_dictionary_opening());
        assert_eq!(ui.error_message(), None);
        assert_ne!(ui.render(&mut reader, 4).unwrap(), preview);
        assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
        ui.select(&mut reader).unwrap(); // Back to selection, on the lookup row.
        for _ in 0..3 {
            ui.turn(&mut reader, false).unwrap();
        }
        assert_eq!(ui.render(&mut reader, 4).unwrap(), preview);
        ui.turn(&mut reader, true).unwrap();
        ui.select(&mut reader).unwrap(); // Save the original source selection.
        assert_eq!(
            reader.state.books[&reader.document.metadata.id].clippings[0].text,
            word
        );
        assert_eq!(reader.position(), anchor);
        assert_eq!(reader.render(4).unwrap(), pixels);
    }
}

#[test]
fn dictionary_preparation_cancels_and_rejects_expired_selection() {
    let (mut reader, mut ui) = dictionary_selection("font");
    let before = serde_json::to_value(&reader.state).unwrap();
    let preview = ui.render(&mut reader, 4).unwrap();
    start_dictionary(&mut ui, &mut reader);
    assert_ne!(ui.render(&mut reader, 4).unwrap(), preview);
    ui.back(&mut reader).unwrap();
    assert!(!ui.is_dictionary_opening());
    assert_eq!(ui.poll_dictionary(&mut reader), Effect::Unchanged);
    // The selection stays on the lookup row of the unchanged panel.
    for _ in 0..3 {
        ui.turn(&mut reader, false).unwrap();
    }
    assert_eq!(ui.render(&mut reader, 4).unwrap(), preview);
    for _ in 0..3 {
        ui.turn(&mut reader, true).unwrap();
    }
    ui.select(&mut reader).unwrap();
    assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
    start_dictionary(&mut ui, &mut reader);
    reader.state.layout.font_size += 2.0;
    assert_eq!(ui.poll_dictionary(&mut reader), Effect::Redraw);
    assert!(!ui.is_dictionary_opening());
    assert_eq!(ui.error_message(), Some("document: word selection expired"));
    assert!(reader.state.books.values().all(|b| b.clippings.is_empty()));
}

#[test]
fn dictionary_configuration_change_cancels_pending_job() {
    let (mut reader, mut ui) = dictionary_selection("font");
    start_dictionary(&mut ui, &mut reader);
    ui.set_dictionary_path(None);
    assert!(!ui.is_dictionary_opening());
    assert_eq!(ui.poll_dictionary(&mut reader), Effect::Unchanged);
    ui.render(&mut reader, 4).unwrap();
    // The selection is still on the lookup row; move up to Save.
    ui.turn(&mut reader, false).unwrap();
    ui.turn(&mut reader, false).unwrap();
    ui.select(&mut reader).unwrap();
    assert_eq!(
        reader.state.books[&reader.document.metadata.id].clippings[0].text,
        "font"
    );
}

#[test]
fn dictionary_reuses_prepared_indexes_and_releases_them_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large.ifo");
    let mut index = Vec::new();
    for n in 0..1000 {
        index.extend_from_slice(format!("word{n:04}").as_bytes());
        index.push(0);
        index.extend_from_slice(&0u32.to_be_bytes());
        index.extend_from_slice(&10u32.to_be_bytes());
    }
    std::fs::write(path.with_extension("idx"), &index).unwrap();
    std::fs::write(path.with_extension("dict"), b"definition").unwrap();
    std::fs::write(&path, format!("StarDict's dict ifo file\nversion=2.4.2\nbookname=Large\nwordcount=1000\nidxfilesize={}\nsametypesequence=m\n", index.len())).unwrap();
    let (mut reader, mut ui) = dictionary_selection("word0500");
    ui.set_dictionary_path(Some(path.clone()));
    let before = serde_json::to_value(&reader.state).unwrap();
    let finish = |ui: &mut ReaderUi, reader: &mut Reader| {
        for step in 1..32 {
            ui.poll_dictionary(reader);
            if !ui.is_dictionary_opening() {
                assert!(ui.is_dictionary_result(), "{:?}", ui.error_message());
                return step;
            }
        }
        panic!("dictionary did not finish");
    };
    start_dictionary(&mut ui, &mut reader);
    let first_steps = finish(&mut ui, &mut reader);
    assert!(first_steps > 1);
    let result = ui.render(&mut reader, 4).unwrap();
    ui.back(&mut reader).unwrap();
    start_dictionary(&mut ui, &mut reader);
    assert_eq!(finish(&mut ui, &mut reader), 1);
    assert_eq!(ui.render(&mut reader, 4).unwrap(), result);
    ui.back(&mut reader).unwrap();
    start_dictionary(&mut ui, &mut reader);
    ui.back(&mut reader).unwrap(); // Cancel a lookup that already owns prepared indexes.
    ui.select(&mut reader).unwrap(); // The selection is still on the lookup row.
    assert_eq!(finish(&mut ui, &mut reader), 1);
    ui.back(&mut reader).unwrap();
    std::fs::write(path.with_extension("dict"), b"").unwrap();
    start_dictionary(&mut ui, &mut reader);
    ui.poll_dictionary(&mut reader);
    assert!(!ui.is_dictionary_opening());
    assert!(!ui.is_dictionary_result());
    assert!(ui.error_message().is_some());
    std::fs::write(path.with_extension("dict"), b"definition").unwrap();
    start_dictionary(&mut ui, &mut reader);
    assert_eq!(finish(&mut ui, &mut reader), first_steps);
    ui.release_dictionary();
    start_dictionary(&mut ui, &mut reader);
    assert_eq!(finish(&mut ui, &mut reader), first_steps);
    ui.set_dictionary_path(Some(path)); // Reconfiguration, even to the same path, invalidates.
    start_dictionary(&mut ui, &mut reader);
    assert_eq!(finish(&mut ui, &mut reader), first_steps);
    assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
}

#[test]
fn dictionary_chooser_persists_and_restores_the_selected_filename() {
    use femto_reader::{dictionary::discover, journal::StateJournal};
    let dir = tempfile::tempdir().unwrap();
    let source = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/dictionary"
    ));
    for name in ["a", "b"] {
        for extension in ["ifo", "idx", "dict", "syn"] {
            std::fs::copy(
                source.join(format!("example.{extension}")),
                dir.path().join(format!("{name}.{extension}")),
            )
            .unwrap();
        }
        let path = dir.path().join(format!("{name}.ifo"));
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("Femto test dictionary", &format!("Dictionary {name}"));
        std::fs::write(path, text).unwrap();
    }
    let (mut reader, mut ui) = dictionary_selection("font");
    let before = serde_json::to_value(&reader.state).unwrap();
    ui.configure_dictionaries(discover(dir.path()).unwrap(), None)
        .unwrap();
    let lookup = |ui: &mut ReaderUi, reader: &mut Reader| {
        open_button_selection(ui, reader);
        ui.select(reader).unwrap();
        start_dictionary(ui, reader);
        for _ in 0..16 {
            ui.poll_dictionary(reader);
            if !ui.is_dictionary_opening() {
                break;
            }
        }
        assert!(ui.is_dictionary_result(), "{:?}", ui.error_message());
        ui.render(reader, 4).unwrap()
    };
    let first = lookup(&mut ui, &mut reader);
    go(&mut ui, &mut reader, &["Settings", "Dictionary"]);
    ui.turn(&mut reader, true).unwrap();
    ui.select(&mut reader).unwrap();
    assert_eq!(reader.state.dictionary_name.as_deref(), Some("b.ifo"));
    let second = lookup(&mut ui, &mut reader);
    assert_ne!(first, second);
    let mut after = serde_json::to_value(&reader.state).unwrap();
    after.as_object_mut().unwrap().remove("dictionary_name");
    assert_eq!(after, before);
    let state_path = dir.path().join("state.json");
    reader.state.save(&state_path).unwrap();
    let restored = ReaderState::load(&state_path).unwrap();
    assert_eq!(restored.dictionary_name.as_deref(), Some("b.ifo"));
    let (mut store, _) = StateJournal::open(dir.path()).unwrap();
    store.save(&reader.state).unwrap();
    assert_eq!(
        StateJournal::open(dir.path()).unwrap().1.dictionary_name,
        restored.dictionary_name
    );
    let mut next_ui = ReaderUi::default();
    next_ui
        .configure_dictionaries(
            discover(dir.path()).unwrap(),
            restored.dictionary_name.as_deref(),
        )
        .unwrap();
    assert_eq!(lookup(&mut next_ui, &mut reader), second);
    assert!(
        next_ui
            .configure_dictionaries(vec![dir.path().join("a.ifo"); 2], None)
            .is_err()
    );
    assert_eq!(lookup(&mut next_ui, &mut reader), second);
    next_ui
        .configure_dictionaries(discover(dir.path()).unwrap(), Some("missing.ifo"))
        .unwrap();
    assert_eq!(lookup(&mut next_ui, &mut reader), first);
    for name in ["../escape.ifo", "bad\nname.ifo", ".ifo"] {
        reader.state.dictionary_name = Some(name.into());
        assert!(reader.state.save(&state_path).is_err());
    }
}

#[test]
fn missing_next_chapter_keeps_reader_usable_and_shows_error() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing-chapter.epub");
    let mut bytes = include_bytes!("../../../tests/fixtures/sample.epub").to_vec();
    // Rename the ZIP local and central directory entries without changing the OPF.
    let name = b"EPUB/two.xhtml";
    let mut renamed = 0;
    for offset in 0..=bytes.len() - name.len() {
        if &bytes[offset..offset + name.len()] == name {
            bytes[offset..offset + name.len()].copy_from_slice(b"EPUB/bad.xhtml");
            renamed += 1;
        }
    }
    assert_eq!(renamed, 2);
    std::fs::write(&path, bytes).unwrap();
    let mut reader = Reader::new(
        Document::open(path).unwrap(),
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        ),
        ReaderState::default(),
    )
    .unwrap();
    let mut ui = ReaderUi::default();
    for _ in 1..reader.page_count() {
        ui.turn(&mut reader, true).unwrap();
    }
    let before = reader.position();
    let pixels = reader.render(2).unwrap();
    assert_eq!(ui.turn(&mut reader, true).unwrap(), Effect::Redraw);
    assert!(ui.is_open());
    assert!(ui.error_message().unwrap().contains("BOOK_RESOURCE_READ"));
    assert_eq!(reader.position(), before);
    assert_eq!(reader.render(2).unwrap(), pixels);
    ui.render(&mut reader, 2).unwrap();
    ui.back(&mut reader).unwrap();
    assert!(!ui.is_open());
    ui.turn(&mut reader, false).unwrap();
    assert!(reader.position() < before);
}

#[test]
fn device_panel_reports_activation_and_text_entry_to_the_frontend() {
    use femto_reader::ui::{DeviceEvent, DeviceRow, DeviceRowKind};
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.set_device_available(true);
    ui.open();
    // The entry is offered in the root menu and opens on request only.
    while ui.swipe(&mut reader, true).unwrap() == Effect::Redraw {}
    let rows = |password: &str| {
        vec![
            DeviceRow {
                label: "Wi-Fi".into(),
                kind: DeviceRowKind::Toggle(false),
            },
            DeviceRow {
                label: "Status".into(),
                kind: DeviceRowKind::Info,
            },
            DeviceRow {
                label: "Password".into(),
                kind: DeviceRowKind::Text {
                    value: password.into(),
                    secret: true,
                    limit: 8,
                },
            },
            DeviceRow {
                label: "Server".into(),
                kind: DeviceRowKind::Text {
                    value: "ab".into(),
                    secret: false,
                    limit: 8,
                },
            },
        ]
    };
    ui.set_device_view("Network", rows("")).unwrap();
    assert!(ui.is_device_open());
    let unset = ui.render(&mut reader, 2).unwrap();
    ui.tap(&mut reader, 240, 150).unwrap();
    assert_eq!(ui.take_device_event(), Some(DeviceEvent::Activate(0)));
    ui.tap(&mut reader, 240, 250).unwrap();
    assert_eq!(ui.take_device_event(), None);
    // A secret starts empty, respects its limit and is reported on Done.
    ui.tap(&mut reader, 240, 350).unwrap();
    assert!(ui.is_editing());
    assert_eq!(ui.query(), "");
    for c in "secret-and-more".chars() {
        ui.type_char(c);
    }
    assert_eq!(ui.query(), "secret-a");
    ui.tap(&mut reader, 420, 750).unwrap();
    assert_eq!(
        ui.take_device_event(),
        Some(DeviceEvent::Text(2, "secret-a".into()))
    );
    assert!(ui.is_device_open() && !ui.is_editing());
    ui.set_device_view("Network", rows("secret-a")).unwrap();
    // The list shows only whether a secret is set.
    assert_ne!(ui.render(&mut reader, 2).unwrap(), unset);
    // Other text starts from its value; Back discards the edit.
    ui.tap(&mut reader, 240, 450).unwrap();
    assert_eq!(ui.query(), "ab");
    ui.type_char('c');
    ui.back(&mut reader).unwrap();
    assert_eq!(ui.take_device_event(), None);
    assert!(ui.is_device_open() && !ui.is_editing());
    ui.back(&mut reader).unwrap();
    assert_eq!(ui.take_device_event(), Some(DeviceEvent::Back));
    ui.close_device();
    assert!(ui.is_open() && !ui.is_device_open());
}

#[test]
fn deleting_a_book_requires_confirmation_and_spares_the_open_book() {
    use femto_reader::state::Library;
    let dir = tempfile::tempdir().unwrap();
    for name in ["alpha", "beta"] {
        std::fs::write(dir.path().join(format!("{name}.txt")), name).unwrap();
    }
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    // Deletion needs the library folder to check the file lies inside it.
    ui.set_library(Library::scan(dir.path()).unwrap());
    ui.open_library();
    assert!(ui.choose(&mut reader, "Delete a book").is_err());
    ui.configure_library_scan(dir.path().to_path_buf(), 256)
        .unwrap();
    ui.set_library(Library::scan_cached(dir.path(), 256).unwrap());
    ui.open_library();
    ui.choose(&mut reader, "Delete a book").unwrap();
    ui.choose(&mut reader, "alpha").unwrap();
    ui.choose(&mut reader, "Keep book").unwrap();
    assert!(dir.path().join("alpha.txt").exists());
    ui.choose(&mut reader, "alpha").unwrap();
    ui.choose(&mut reader, "Delete this book").unwrap();
    assert_eq!(ui.error_message(), None);
    assert!(!dir.path().join("alpha.txt").exists());
    assert!(dir.path().join("beta.txt").exists());
    assert!(ui.choose(&mut reader, "alpha").is_err());
    let cached = Library::load_cached(dir.path(), 256).unwrap();
    assert_eq!(cached.entries.len(), 1);
    assert_eq!(cached.entries[0].metadata.title, "beta");

    // The open book stays on the card.
    ui.back(&mut reader).unwrap();
    ui.choose(&mut reader, "All books").unwrap();
    ui.choose(&mut reader, "beta").unwrap();
    finish_open(&mut ui, &mut reader);
    ui.open_library();
    ui.choose(&mut reader, "Delete a book").unwrap();
    ui.choose(&mut reader, "beta").unwrap();
    ui.choose(&mut reader, "Delete this book").unwrap();
    assert_eq!(ui.error_message(), Some("document: this book is open"));
    assert!(dir.path().join("beta.txt").exists());
}

#[test]
fn update_panel_shows_status_and_acts_only_on_an_action() {
    use femto_reader::ui::{UpdateStep, UpdateView};
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    go(&mut ui, &mut reader, &["Settings"]);
    assert!(ui.choose(&mut reader, "Firmware update").is_err());
    assert!(ui.choose(&mut reader, "Version").is_err());
    ui.set_firmware("femto-1");
    let view = |step| UpdateView {
        status: "Version 2 is available".into(),
        step,
    };
    assert!(ui.set_update(Some(view(UpdateStep::Action("Download 7.1 MB".into())))));
    go(&mut ui, &mut reader, &["Settings", "Version femto-1"]);
    ui.choose(&mut reader, "Firmware update").unwrap();
    assert!(ui.is_update_open());
    // The panel opens on its action; Select starts it.
    ui.choose(&mut reader, "Version 2 is available").unwrap();
    assert!(!ui.take_update_request());
    ui.choose(&mut reader, "Download 7.1 MB").unwrap();
    assert!(ui.take_update_request());
    assert!(!ui.take_update_request());
    // Work in progress is shown in the same row and cannot be activated.
    let working = view(UpdateStep::Working {
        label: "Downloaded 3.5 of 7.1 MB".into(),
        percent: Some(49),
    });
    assert!(ui.set_update(Some(working.clone())));
    assert!(!ui.set_update(Some(working)));
    ui.choose(&mut reader, "Downloaded 3.5 of 7.1 MB").unwrap();
    assert!(!ui.take_update_request());
    ui.render(&mut reader, 2).unwrap();
    assert!(!reader.state.check_updates);
    ui.choose(&mut reader, "Check automatically").unwrap();
    assert!(reader.state.check_updates);
    assert!(!ui.take_update_request());
    ui.back(&mut reader).unwrap();
    assert!(!ui.is_update_open());
    ui.choose(&mut reader, "Firmware update").unwrap();
}

#[test]
fn font_menu_requests_a_family_and_adopts_it_after_reflow() {
    use femto_reader::layout::{Font, Fonts};
    let sans = || {
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
                .unwrap(),
        )
    };
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    go(&mut ui, &mut reader, &["Typography"]);
    assert!(ui.choose(&mut reader, "Font:").is_err());
    ui.set_font_families(vec!["Sans".into(), "Slab".into()]);
    go(&mut ui, &mut reader, &["Typography", "Font: Default"]);
    // The current family is not requested again.
    ui.choose(&mut reader, "> Default").unwrap();
    assert_eq!(ui.take_font_request(), None);
    ui.choose(&mut reader, "Sans").unwrap();
    assert_eq!(ui.take_font_request(), Some(Some("Sans".into())));
    let position = reader.position();
    let before = ui.render(&mut reader, 2).unwrap();
    ui.set_fonts(&reader, sans(), Some("Sans".into())).unwrap();
    assert_eq!(reader.state.font_family, None);
    finish_reflow(&mut ui, &mut reader);
    assert_eq!(reader.state.font_family.as_deref(), Some("Sans"));
    assert_eq!(reader.position(), position);
    // The menu stays on the chooser and is redrawn in the new face.
    assert_ne!(ui.render(&mut reader, 2).unwrap(), before);
    ui.choose(&mut reader, "> Sans").unwrap();
    assert_eq!(ui.take_font_request(), None);
    ui.choose(&mut reader, "Default").unwrap();
    assert_eq!(ui.take_font_request(), Some(None));
}

#[test]
fn reading_gestures_step_the_light_open_its_panel_and_are_configurable() {
    use femto_reader::{state::GestureAction, ui::Gesture};
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    reader.state.frontlight.brightness = 9;
    // Light steps leave the page alone and stop at the ends of the range.
    for expected in [10, 10] {
        assert_eq!(
            ui.gesture(&mut reader, Gesture::SwipeUp).unwrap(),
            Effect::Unchanged
        );
        assert_eq!(reader.state.frontlight.brightness, expected);
    }
    assert!(ui.take_light_request());
    ui.gesture(&mut reader, Gesture::SwipeUp).unwrap();
    assert!(!ui.take_light_request());
    ui.gesture(&mut reader, Gesture::SwipeDown).unwrap();
    assert_eq!(reader.state.frontlight.brightness, 9);
    assert!(ui.take_light_request());

    // Hold opens the frontlight controls; Back returns straight to the page.
    assert_eq!(
        ui.gesture(&mut reader, Gesture::Hold).unwrap(),
        Effect::Redraw
    );
    assert!(ui.is_open());
    // Menus ignore reading gestures.
    ui.gesture(&mut reader, Gesture::SwipeDown).unwrap();
    assert_eq!(reader.state.frontlight.brightness, 9);
    ui.choose(&mut reader, "Turn off").unwrap();
    assert_eq!(reader.state.frontlight.brightness, 0);
    ui.back(&mut reader).unwrap();
    assert!(!ui.is_open());

    // Each activation selects the next action.
    ui.set_gestures_available(true);
    go(&mut ui, &mut reader, &["Settings", "Gestures"]);
    ui.choose(&mut reader, "Touch and hold: frontlight")
        .unwrap();
    ui.choose(&mut reader, "Touch and hold: bookmark").unwrap();
    assert_eq!(reader.state.gestures.hold, GestureAction::Menu);
    ui.choose(&mut reader, "Touch and hold: menu").unwrap();
    ui.choose(&mut reader, "Touch and hold: nothing").unwrap();
    assert_eq!(reader.state.gestures.hold, GestureAction::Brighter);
    ui.choose(&mut reader, "Swipe up: brighter").unwrap();
    assert_eq!(reader.state.gestures.swipe_up, GestureAction::Dimmer);
    // The frontlight panel reached through the menu returns to the menu.
    ui.open();
    ui.choose(&mut reader, "Frontlight").unwrap();
    ui.back(&mut reader).unwrap();
    assert!(ui.is_open());
}

#[test]
fn sleep_page_shows_the_cover_centered_and_falls_back_to_text() {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut covered = Reader::new(
        Document::open(fixtures.join("cover.epub")).unwrap(),
        Fonts::new(Font::load(fixtures.join("DejaVuSerif.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap();
    let ui = ReaderUi::default();
    let pixels = ui.render_sleep(&mut covered, 4).unwrap();
    // The 320x240 gradient runs from black to white inside a white page.
    let at = |x: usize, y: usize| pixels[y * 480 + x];
    assert_eq!(at(10, 10), 255);
    assert_eq!(at(240, 100), 255);
    assert_eq!(at(82, 282), 0);
    assert!((0..800).all(|y| at(79, y) == 255 && at(400, y) == 255));
    // A book without a cover names itself instead, however long its name.
    let mut plain = reader();
    plain.document.metadata.title =
        "The Song of the Cell: An Exploration of Medicine and the New Human ".repeat(3);
    plain.document.metadata.author =
        "An Author With A Remarkably Long Name, And Another; ".repeat(3);
    let text = ui.render_sleep(&mut plain, 4).unwrap();
    assert!(text.contains(&0));
    assert!(text[700 * 480..].iter().all(|pixel| *pixel == 255));
}

#[test]
fn status_line_takes_the_rows_below_the_text_and_shows_progress_and_battery() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.set_battery(Some(60));
    ui.choose(&mut reader, "Settings").unwrap();
    ui.choose(&mut reader, "Status line").unwrap();
    while ui.is_reflowing() {
        ui.poll_reflow(&mut reader);
    }
    assert_eq!(reader.state.layout.footer, 32);
    while ui.is_open() {
        ui.back(&mut reader).unwrap();
    }
    let shown = ui.render(&mut reader, 2).unwrap();
    // The text ends above the line and leaves a gap.
    assert!(shown[..768 * 480].contains(&0));
    assert!(
        shown[768 * 480..778 * 480]
            .iter()
            .all(|pixel| *pixel == 255)
    );
    let ink = |columns: std::ops::Range<usize>| {
        (778..800)
            .flat_map(|y| columns.clone().map(move |x| y * 480 + x))
            .filter(|i| shown[*i] == 0)
            .count()
    };
    assert!(ink(24..120) > 10, "progress at the left");
    assert!(ink(360..456) > 40, "battery at the right");
    assert_eq!(ink(160..320), 0);
}

#[test]
fn go_to_moves_to_a_share_of_the_book_and_back_returns() {
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    let start = reader.position();
    ui.choose(&mut reader, "This book").unwrap();
    ui.choose(&mut reader, "Go to").unwrap();
    ui.choose(&mut reader, "50%").unwrap();
    assert!(!ui.is_open());
    let middle = reader.position();
    assert!(middle > start);
    let percent = reader.book_progress_percent().unwrap();
    assert!((40..=60).contains(&percent), "{percent}");
    reader
        .dispatch(femto_reader::controller::Action::Back)
        .unwrap();
    assert_eq!(reader.position(), start);
}
