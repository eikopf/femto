use femto_reader::{
    document::{BaseDirection, Block, Document, TextAlign},
    layout::{Font, Fonts, LayoutConfig, paginate},
};
fn fonts() -> Fonts {
    Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    )
}
#[test]
fn epub_block_direction_resolves_markup_css_inheritance_and_inline_boundaries() {
    use BaseDirection::*;
    let mut document = Document::open(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/direction.epub"),
    )
    .unwrap();
    let chapter = document.chapter(0).unwrap();
    let paragraphs: Vec<_> = chapter
        .blocks
        .iter()
        .filter_map(|b| {
            if let Block::Text(p) = b {
                Some(p)
            } else {
                None
            }
        })
        .collect();
    let expected = [
        Rtl, Ltr, Ltr, Ltr, Rtl, Ltr, Auto, Rtl, Ltr, Ltr, Rtl, Rtl, Ltr,
    ];
    assert_eq!(paragraphs.len(), expected.len());
    for (p, direction) in paragraphs.iter().zip(expected) {
        assert!(!p.spans.is_empty());
        assert!(
            p.spans.iter().all(|s| s.style.direction == direction),
            "{}: expected {direction:?}",
            p.text
        );
    }
    assert_eq!(paragraphs[7].text, "Inline stays RTL");
}
#[test]
fn explicit_base_direction_controls_logical_alignment_for_latin_text() {
    let fonts = fonts();
    let config = LayoutConfig::default();
    let mut doc = Document::from_text("Direction", "Latin text").unwrap();
    let mut chapter = doc.chapter(0).unwrap();
    let original = paginate(&chapter, &fonts, config).unwrap();
    let original = &original.pages[0].lines[0];
    let free = (config.width - 2 * config.margin) as f32 - original.width;
    for (direction, align, shift) in [
        (BaseDirection::Auto, TextAlign::Start, 0.0),
        (BaseDirection::Rtl, TextAlign::Start, free),
        (BaseDirection::Rtl, TextAlign::End, 0.0),
        (BaseDirection::Ltr, TextAlign::End, free),
    ] {
        if let Block::Text(p) = &mut chapter.blocks[0] {
            p.spans[0].style.direction = direction;
            p.spans[0].style.align = align;
        }
        let pages = paginate(&chapter, &fonts, config).unwrap();
        let line = &pages.pages[0].lines[0];
        assert_eq!(
            (line.start, line.end, line.width),
            (original.start, original.end, original.width)
        );
        for (glyph, base) in line.glyphs.iter().zip(&original.glyphs) {
            assert_eq!((glyph.id, glyph.cluster), (base.id, base.cluster));
            assert!((glyph.x - base.x - shift).abs() < 0.001);
        }
    }
}
#[test]
fn mixed_runs_reorder_without_inserting_source_characters() {
    let fonts = fonts();
    let config = LayoutConfig::default();
    let text = "abc אבג";
    let mut doc = Document::from_text("Bidi", text).unwrap();
    let mut chapter = doc.chapter(0).unwrap();
    for (direction, expected) in [
        (BaseDirection::Ltr, vec![0, 1, 2, 3, 8, 6, 4]),
        (BaseDirection::Rtl, vec![8, 6, 4, 3, 0, 1, 2]),
    ] {
        if let Block::Text(p) = &mut chapter.blocks[0] {
            p.spans[0].style.direction = direction;
            p.spans[0].style.align = TextAlign::Left;
        }
        let pages = paginate(&chapter, &fonts, config).unwrap();
        let line = &pages.pages[0].lines[0];
        assert_eq!(line.start.byte, 0);
        assert_eq!(line.end.byte, text.len());
        assert_eq!(
            line.glyphs.iter().map(|g| g.cluster).collect::<Vec<_>>(),
            expected
        );
        assert_eq!(line.text, text);
    }
}
