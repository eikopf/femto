use femto_reader::{
    document::{Block, Document},
    layout::{Font, Fonts, LayoutConfig, paginate},
};

#[test]
fn epub_whitespace_survives_inline_boundaries_and_preserves_line_locations() {
    let mut doc = Document::open(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/whitespace.epub"),
    )
    .unwrap();
    let chapter = doc.chapter(0).unwrap();
    let paragraphs: Vec<_> = chapter
        .blocks
        .iter()
        .filter_map(|b| match b {
            Block::Text(p) => Some(p),
            _ => None,
        })
        .collect();
    let expected = [
        "A bold word tail",
        "a b c",
        "  first\n\n    second   bold",
        "first\nsecond word\nthird",
        "a   b\nc",
        "a b c",
        "A link tail",
    ];
    assert_eq!(paragraphs.len(), expected.len());
    for (p, expected) in paragraphs.iter().zip(expected) {
        assert_eq!(p.text, expected);
        assert_eq!(p.spans.first().unwrap().range.start, 0);
        assert_eq!(p.spans.last().unwrap().range.end, p.text.len());
        for pair in p.spans.windows(2) {
            assert_eq!(pair[0].range.end, pair[1].range.start);
        }
    }
    let link = &chapter.links[0];
    assert_eq!(&paragraphs[6].text[link.start.byte..link.end.byte], "link");
    assert_eq!(link.label, "link");
    assert_eq!(doc.resolve_link(&chapter.href, "#linked").unwrap().block, 6);
    let fonts = Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    let pages = paginate(&chapter, &fonts, LayoutConfig::default()).unwrap();
    let lines: Vec<_> = pages
        .pages
        .iter()
        .flat_map(|p| &p.lines)
        .filter(|l| l.start.block == 2)
        .collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].text, "  first");
    assert_eq!(lines[1].text, "");
    assert_eq!(lines[2].text, "    second   bold");
    assert_eq!(lines[0].end.byte, 8);
    assert_eq!(lines[1].start.byte, 8);
    assert_eq!(lines[1].end.byte, 9);
    assert_eq!(lines[2].start.byte, 9);
}
