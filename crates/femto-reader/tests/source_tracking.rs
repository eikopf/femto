use femto_reader::document::{Block, Document};

#[test]
fn provenance_matches_retained_blocks_without_changing_reading_text() {
    for file in [
        "sample.epub",
        "whitespace.epub",
        "navigation.epub",
        "lists.epub",
        "kosync-css.epub",
    ] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(file);
        let mut document = Document::open(path).unwrap();
        for index in 0..document.metadata.chapters.len() {
            let (plain, absent) = document.chapter_with_sources(index, false).unwrap();
            assert!(absent.is_empty());
            let (tracked, sources) = document.chapter_with_sources(index, true).unwrap();
            assert!(!sources.is_empty());
            let text = |chapter: &femto_reader::document::Chapter| {
                chapter
                    .blocks
                    .iter()
                    .map(|block| match block {
                        Block::Text(paragraph) => paragraph.text.clone(),
                        Block::Image(image) => image.source.clone(),
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(text(&plain), text(&tracked));
            let xml = document.resource(&tracked.href).unwrap();
            for run in sources {
                let Block::Text(paragraph) = &tracked.blocks[run.start.block] else {
                    panic!("source assigned to image");
                };
                assert_eq!(
                    &paragraph.text[run.start.byte..run.start.byte + run.text.len()],
                    run.text
                );
                assert!(run.source.token.start < run.source.token.end);
                assert!(run.source.token.end <= xml.len());
                assert_eq!(run.source.characters.len(), run.text.chars().count() + 1);
                assert!(
                    run.source
                        .characters
                        .windows(2)
                        .all(|pair| pair[0] < pair[1])
                );
            }
        }
    }
}
