use femto_reader::{
    controller::{Action, Reader},
    document::Document,
    journal::StateJournal,
    layout::{Font, Fonts},
    state::{ReaderState, Rotation},
    ui::ReaderUi,
};

fn reader(state: ReaderState) -> Reader {
    Reader::new(
        Document::from_text(
            "Highlights",
            "office naïve שלום and ordinary words. "
                .repeat(120)
                .as_str(),
        )
        .unwrap(),
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        ),
        state,
    )
    .unwrap()
}

#[test]
fn menu_highlights_persist_in_both_stores_and_follow_reflow() {
    let mut r = reader(ReaderState::default());
    let original = r.render(4).unwrap();
    let position = r.position();
    r.clip_page_text(12).unwrap();
    let id = r.document.metadata.id.clone();
    let clipping = r.state.books[&id].clippings[0].clone();
    assert!(!clipping.highlighted);
    assert!(
        serde_json::to_value(&clipping)
            .unwrap()
            .get("highlighted")
            .is_none()
    );
    let mut ui = ReaderUi::default();
    ui.open();
    ui.choose(&mut r, "This book").unwrap();
    ui.choose(&mut r, "Clippings").unwrap();
    ui.turn(&mut r, true).unwrap();
    ui.select(&mut r).unwrap();
    ui.turn(&mut r, true).unwrap();
    ui.turn(&mut r, true).unwrap();
    ui.select(&mut r).unwrap();
    assert!(r.state.books[&id].clippings[0].highlighted);
    assert_eq!(r.position(), position);
    let highlighted = r.render(4).unwrap();
    assert_ne!(highlighted, original);
    r.clip_page_text(99).unwrap();
    assert!(r.state.books[&id].clippings[0].highlighted);
    assert_eq!(r.state.books[&id].clippings[0].recorded_at, 12);
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("state.json");
    r.state.save(&path).unwrap();
    StateJournal::open(root.path())
        .unwrap()
        .0
        .save(&r.state)
        .unwrap();
    for state in [
        ReaderState::load(&path).unwrap(),
        StateJournal::open(root.path()).unwrap().1,
    ] {
        let mut restored = reader(state);
        assert_eq!(restored.render(4).unwrap(), highlighted);
        for rotation in [Rotation::Portrait, Rotation::Clockwise] {
            restored.dispatch(Action::Rotate(rotation)).unwrap();
            let mut layout = restored.state.layout;
            layout.font_size = 28.0;
            restored.dispatch(Action::Reflow(layout)).unwrap();
            let marked = restored.render(2).unwrap();
            restored.toggle_clipping_highlight(0).unwrap();
            let plain = restored.render(2).unwrap();
            assert_ne!(marked, plain);
            assert!(
                marked
                    .iter()
                    .zip(&plain)
                    .all(|(a, b)| a == b || *a == 255 - *b)
            );
            restored.toggle_clipping_highlight(0).unwrap();
            assert_eq!(restored.render(2).unwrap(), marked);
            assert_eq!(restored.state.books[&id].clippings[0].range, clipping.range);
        }
    }
    ui.select(&mut r).unwrap();
    assert!(!r.state.books[&id].clippings[0].highlighted);
    assert_eq!(r.render(4).unwrap(), original);
}

#[test]
fn pending_selection_over_saved_highlight_does_not_invert_twice() {
    let mut r = reader(ReaderState::default());
    r.clip_page_text(1).unwrap();
    r.toggle_clipping_highlight(0).unwrap();
    let marked = r.render(4).unwrap();
    let line = &r.page().lines[0];
    let g = &line.glyphs[0];
    let point = (
        (g.x + g.advance / 2.0) as u32,
        (line.baseline - line.ascent / 2.0) as u32,
    );
    let range = r.word_at(point.0, point.1).unwrap();
    let mut overlapping =
        femto_reader::clipping::Clipping::capture(&mut r.document, range, 2).unwrap();
    overlapping.highlighted = true;
    r.state.add_clipping(overlapping).unwrap();
    assert_eq!(r.render(4).unwrap(), marked);
    let mut ui = ReaderUi::default();
    ui.open();
    ui.choose(&mut r, "This book").unwrap();
    ui.choose(&mut r, "Clippings").unwrap();
    for _ in 0..3 {
        ui.turn(&mut r, true).unwrap();
    }
    ui.select(&mut r).unwrap();
    ui.tap(&mut r, point.0, point.1).unwrap();
    for _ in 0..2 {
        ui.turn(&mut r, true).unwrap();
    }
    ui.select(&mut r).unwrap();
    let pending = ui.render(&mut r, 4).unwrap();
    let body = ((r.state.layout.height - 32) * r.state.layout.width) as usize;
    assert_eq!(&pending[..body], &marked[..body]);
}

#[test]
fn invalid_highlights_are_omitted_and_can_be_disabled() {
    let mut r = reader(ReaderState::default());
    let plain = r.render(4).unwrap();
    r.clip_page_text(1).unwrap();
    let id = r.document.metadata.id.clone();
    let stale = r.state.books[&id].clippings[0].clone();
    r.state.books.get_mut(&id).unwrap().clippings[0]
        .text
        .push('!');
    assert!(r.toggle_clipping_highlight(0).is_err());
    assert!(r.state.set_clipping_highlight(&stale, true).is_err());
    r.state.books.get_mut(&id).unwrap().clippings[0].highlighted = true;
    assert_eq!(r.render(4).unwrap(), plain);
    assert!(r.warnings().iter().any(|w| w.contains("highlight omitted")));
    let count = r.warnings().len();
    assert_eq!(r.render(4).unwrap(), plain);
    assert_eq!(r.warnings().len(), count);
    r.toggle_clipping_highlight(0).unwrap();
    assert!(!r.state.books[&id].clippings[0].highlighted);
    assert_eq!(r.render(4).unwrap(), plain);
}

#[test]
fn highlights_use_the_retained_chapter_until_the_book_is_reopened() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("book.txt");
    std::fs::write(&path, "Original source words.").unwrap();
    let fonts = || {
        Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
                .unwrap(),
        )
    };
    let mut r = Reader::new(
        Document::open(&path).unwrap(),
        fonts(),
        ReaderState::default(),
    )
    .unwrap();
    r.clip_page_text(1).unwrap();
    r.toggle_clipping_highlight(0).unwrap();
    let marked = r.render(4).unwrap();
    let old_id = r.document.metadata.id.clone();
    std::fs::write(&path, "Replacement source words.").unwrap();
    assert_eq!(r.render(4).unwrap(), marked);
    assert!(!r.warnings().iter().any(|w| w.contains("highlight omitted")));
    let mut reopened = Reader::new(Document::open(&path).unwrap(), fonts(), r.state).unwrap();
    assert_ne!(reopened.document.metadata.id, old_id);
    let mut plain = Reader::new(
        Document::open(&path).unwrap(),
        fonts(),
        ReaderState::default(),
    )
    .unwrap();
    assert_eq!(reopened.render(4).unwrap(), plain.render(4).unwrap());
}

#[test]
fn cross_chapter_highlights_verify_the_full_quote_and_disappear_on_removal() {
    use femto_reader::{clipping::Clipping, document::Block, layout::Locator};
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut r = Reader::new(
        Document::open(fixture.join("sample.epub")).unwrap(),
        Fonts::new(Font::load(fixture.join("DejaVuSerif.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap();
    let first_plain = r.render(4).unwrap();
    let start = r.page().lines[0].start;
    let second = r.document.chapter(1).unwrap();
    let (block, text) = second
        .blocks
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, block)| {
            if let Block::Text(paragraph) = block {
                Some((index, &paragraph.text))
            } else {
                None
            }
        })
        .unwrap();
    let end = Locator {
        chapter: 1,
        block,
        byte: text.len(),
    };
    let clipping = Clipping::capture(&mut r.document, start..end, 17).unwrap();
    assert!(clipping.text.contains('\n'));
    r.state.add_clipping(clipping).unwrap();
    r.toggle_clipping_highlight(0).unwrap();
    let id = r.document.metadata.id.clone();
    let expected = r.state.books[&id].clippings[0].clone();
    let first_marked = r.render(4).unwrap();
    assert_ne!(first_marked, first_plain);
    let position = r.position();
    assert_eq!(r.render(4).unwrap(), first_marked);
    assert_eq!(r.position(), position);

    r.dispatch(Action::Chapter(1)).unwrap();
    let second_position = r.position();
    let second_marked = r.render(4).unwrap();
    r.toggle_clipping_highlight(0).unwrap();
    let second_plain = r.render(4).unwrap();
    assert_ne!(second_marked, second_plain);
    r.toggle_clipping_highlight(0).unwrap();
    assert_eq!(r.render(4).unwrap(), second_marked);
    assert_eq!(r.position(), second_position);

    // The forged suffix represents source in chapter 1 while chapter 0 is visible.
    r.dispatch(Action::Back).unwrap();
    assert_eq!(r.position(), position);
    r.state.books.get_mut(&id).unwrap().clippings[0]
        .text
        .push('!');
    assert_eq!(r.render(4).unwrap(), first_plain);
    assert!(r.warnings().iter().any(|w| w.contains("highlight omitted")));
    r.state.books.get_mut(&id).unwrap().clippings[0] = expected.clone();
    assert_eq!(r.render(4).unwrap(), first_marked);
    r.state.remove_clipping(&expected).unwrap();
    assert_eq!(r.render(4).unwrap(), first_plain);
    assert_eq!(r.position(), position);
    r.dispatch(Action::Chapter(1)).unwrap();
    assert_eq!(r.render(4).unwrap(), second_plain);
    assert!(r.state.books[&id].clippings.is_empty());
}
