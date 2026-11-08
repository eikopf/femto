use femto_reader::{
    controller::{Action, Reader},
    document::Document,
    layout::{Font, Fonts},
    state::ReaderState,
};
use std::path::PathBuf;
fn reader() -> Reader {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    Reader::new(
        Document::open(root.join("semantic-notes.epub")).unwrap(),
        Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap()
}

#[test]
fn long_semantic_note_preview_excludes_neighbors_and_preserves_follow_back() {
    let mut reader = reader();
    let position = reader.position();
    let before = serde_json::to_value(&reader.state).unwrap();
    let mut preview = reader.preview_link(0, reader.state.layout).unwrap();
    assert!(preview.page_count() > 2);
    assert_eq!(preview.page_index(), 0);
    let mut text = String::new();
    loop {
        for line in &preview.page().lines {
            text.push_str(&line.text);
            text.push('\n');
        }
        assert_eq!(reader.render_preview(&preview, 4).unwrap().len(), 480 * 800);
        if preview.page_index() + 1 == preview.page_count() {
            break;
        }
        reader.turn_preview(&mut preview, true).unwrap();
    }
    assert!(text.contains("FIRST_NOTE_START"));
    assert!(text.contains("FIRST_NOTE_END"));
    assert!(!text.contains("EXCLUDED"), "{text}");
    assert!(!text.contains("INLINE_NOTE"));
    assert_eq!(reader.position(), position);
    assert_eq!(serde_json::to_value(&reader.state).unwrap(), before);
    let target = preview.page().start;
    let mut job = reader.begin_follow_preview(&preview).unwrap().unwrap();
    while !reader.step_navigation(&mut job).unwrap() {}
    reader.finish_navigation(job).unwrap();
    assert_eq!(reader.position(), target);
    let mut following = String::new();
    loop {
        for line in &reader.page().lines {
            following.push_str(&line.text);
        }
        if reader.page_index() + 1 == reader.page_count() {
            break;
        }
        reader.dispatch(Action::Next).unwrap();
    }
    assert!(following.contains("EXCLUDED_TRAILER"));
    reader.dispatch(Action::Back).unwrap();
    assert_eq!(reader.position(), position);
}

#[test]
fn inline_note_keeps_original_offsets_and_wrong_namespace_has_no_boundary() {
    let mut reader = reader();
    let target = reader
        .document
        .resolve_link("text/one.xhtml", "two.xhtml#inline")
        .unwrap();
    let chapter = reader.document.chapter(target.chapter).unwrap();
    assert_eq!(chapter.notes.len(), 3);
    let target = chapter.anchors["inline"];
    assert!(target.byte > 0);
    assert!(chapter.notes.iter().any(|range| range.contains(&target)));
    assert!(
        !chapter
            .notes
            .iter()
            .any(|range| range.contains(&chapter.anchors["wrong"]))
    );
    let preview = reader.preview_link(1, reader.state.layout).unwrap();
    assert_eq!(preview.page_count(), 1);
    let text = preview
        .page()
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<String>();
    assert_eq!(text.trim(), "INLINE_NOTE café styled content");
    assert_eq!(preview.page().lines[0].start, target);
    reader.follow_preview(&preview).unwrap();
    assert_eq!(reader.position(), target);
    reader.dispatch(Action::Back).unwrap();
    let ordinary = reader.preview_link(2, reader.state.layout).unwrap();
    assert!(ordinary.page_count() > 2);
}

#[test]
fn inline_note_direction_matches_isolated_text_across_page_reconstruction() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let fonts = || Fonts::new(Font::load(root.join("DejaVuSans.ttf")).unwrap());
    let mut state = ReaderState::default();
    state.layout.hyphenation = femto_reader::layout::Hyphenation::Off;
    let mut reader = Reader::new(
        Document::open(root.join("note-context.epub")).unwrap(),
        fonts(),
        state.clone(),
    )
    .unwrap();
    let origin = reader.position();
    for (index, text) in [
        "English note context. ".repeat(80),
        "שלום עולם ".repeat(160),
    ]
    .into_iter()
    .enumerate()
    {
        let mut expected = Reader::new(
            Document::from_text("Isolated note", text.trim_end()).unwrap(),
            fonts(),
            state.clone(),
        )
        .unwrap();
        let mut preview = reader.preview_link(index, reader.state.layout).unwrap();
        assert!(preview.page().start.byte > 0);
        assert!(preview.page_count() > 1);
        assert_eq!(preview.page_count(), expected.page_count());
        loop {
            assert!(
                reader.render_preview(&preview, 4).unwrap() == expected.render(4).unwrap(),
                "note {index}, page {}",
                preview.page_index()
            );
            if preview.page_index() + 1 == preview.page_count() {
                break;
            }
            reader.turn_preview(&mut preview, true).unwrap();
            expected.dispatch(Action::Next).unwrap();
        }
        while preview.page_index() > 0 {
            reader.turn_preview(&mut preview, false).unwrap();
            expected.dispatch(Action::Previous).unwrap();
            assert!(reader.render_preview(&preview, 4).unwrap() == expected.render(4).unwrap());
        }
        let target = preview.page().start;
        reader.follow_preview(&preview).unwrap();
        assert_eq!(reader.position(), target);
        reader.dispatch(Action::Back).unwrap();
        assert_eq!(reader.position(), origin);
    }
}
