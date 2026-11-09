use femto_reader::{
    document::{Block, Document},
    layout::Locator,
};
fn document(name: &str) -> Document {
    Document::open(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name),
    )
    .unwrap()
}
#[test]
fn sample_positions_use_engine_paths_and_roundtrip_unicode_boundaries() {
    let mut document = document("sample.epub");
    let chapter = document.chapter(0).unwrap();
    let first = Locator {
        chapter: 0,
        block: 0,
        byte: 0,
    };
    assert_eq!(
        document.koreader_position(first).unwrap(),
        "/body[1]/DocFragment[1]/body[1]/h1[1]/text()[1].0"
    );
    assert_eq!(
        document
            .resolve_koreader_position("/body/DocFragment[1]/body/h1/text().0")
            .unwrap(),
        first
    );
    for (index, block) in chapter.blocks.iter().enumerate().take(5) {
        let Block::Text(paragraph) = block else {
            continue;
        };
        for (byte, ch) in paragraph.text.char_indices() {
            if ch.is_whitespace() {
                continue;
            }
            let location = Locator {
                chapter: 0,
                block: index,
                byte,
            };
            let pointer = document.koreader_position(location).unwrap();
            assert_eq!(
                document.resolve_koreader_position(&pointer).unwrap(),
                location,
                "{pointer}"
            );
        }
    }
}
#[test]
fn invalid_and_non_epub_positions_fail_without_state_mutation() {
    let mut document = document("sample.epub");
    for position in [
        "7",
        "/body/DocFragment[999]/body/p/text().0",
        "/body/DocFragment[1]/body/p[999]/text().0",
        "/body/DocFragment[1]/body/h1/text().99999",
    ] {
        assert!(document.resolve_koreader_position(position).is_err());
    }
    let mut text = Document::from_text("Text", "Sample").unwrap();
    assert!(text.koreader_position(Locator::default()).is_err());
}
