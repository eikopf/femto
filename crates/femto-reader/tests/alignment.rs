use femto_reader::{
    document::{Block, Document, TextAlign},
    layout::{Font, Fonts, LayoutConfig, paginate},
};
#[test]
fn epub_alignment_survives_stylesheets_inheritance_and_inline_content() {
    let mut doc = Document::open(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/alignment.epub"),
    )
    .unwrap();
    let chapter = doc.chapter(0).unwrap();
    let paragraphs: Vec<_> = chapter
        .blocks
        .iter()
        .filter_map(|block| match block {
            Block::Text(p) => Some(p),
            _ => None,
        })
        .collect();
    assert_eq!(paragraphs.len(), 6);
    for (paragraph, expected) in paragraphs.iter().zip([
        TextAlign::Center,
        TextAlign::Right,
        TextAlign::Left,
        TextAlign::End,
        TextAlign::Start,
        TextAlign::Center,
    ]) {
        assert!(
            paragraph
                .spans
                .iter()
                .all(|span| span.style.align == expected)
        );
    }
}
#[test]
fn alignment_translates_glyphs_without_changing_breaks_or_shaping() {
    let fonts = Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    let config = LayoutConfig::default();
    for (text, rtl) in [("Some short English text", false), ("שלום עולם", true)] {
        let mut doc = Document::from_text("Alignment", text).unwrap();
        let mut chapter = doc.chapter(0).unwrap();
        if let Block::Text(p) = &mut chapter.blocks[0] {
            p.spans[0].style.align = TextAlign::Left;
        }
        let base = paginate(&chapter, &fonts, config).unwrap();
        let base = &base.pages[0].lines[0];
        let space = (config.width - 2 * config.margin) as f32 - base.width;
        for (align, fraction) in [
            (TextAlign::Left, 0.0),
            (TextAlign::Center, 0.5),
            (TextAlign::Right, 1.0),
            (TextAlign::Start, if rtl { 1.0 } else { 0.0 }),
            (TextAlign::End, if rtl { 0.0 } else { 1.0 }),
        ] {
            if let Block::Text(p) = &mut chapter.blocks[0] {
                p.spans[0].style.align = align;
            }
            let pages = paginate(&chapter, &fonts, config).unwrap();
            let line = &pages.pages[0].lines[0];
            assert_eq!(
                (line.start, line.end, line.width),
                (base.start, base.end, base.width)
            );
            assert_eq!(line.glyphs.len(), base.glyphs.len());
            for (actual, original) in line.glyphs.iter().zip(&base.glyphs) {
                assert_eq!(
                    (actual.id, actual.cluster, actual.y),
                    (original.id, original.cluster, original.y)
                );
                assert!((actual.x - original.x - space * fraction).abs() < 0.001);
            }
        }
    }
}

#[test]
fn justification_expands_visual_spaces_and_preserves_source_and_shaping() {
    let fonts = Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    let config = LayoutConfig {
        width: 240,
        ..Default::default()
    };
    let available = (config.width - 2 * config.margin) as f32;
    for text in [
        "One two three four five six seven eight nine ten eleven twelve.",
        "שלום עולם שלום עולם שלום עולם שלום עולם שלום עולם",
        "English שלום world עולם English שלום world עולם English",
    ] {
        let mut doc = Document::from_text("Justified", text).unwrap();
        let mut chapter = doc.chapter(0).unwrap();
        let base = paginate(&chapter, &fonts, config).unwrap();
        for block in &mut chapter.blocks {
            if let Block::Text(p) = block {
                for span in &mut p.spans {
                    span.style.align = TextAlign::Justify;
                }
            }
        }
        let justified = paginate(&chapter, &fonts, config).unwrap();
        let original: Vec<_> = base.pages.iter().flat_map(|p| &p.lines).collect();
        let lines: Vec<_> = justified.pages.iter().flat_map(|p| &p.lines).collect();
        assert_eq!(lines.len(), original.len());
        assert!(lines.len() > 1);
        let mut expanded = false;
        for (index, (line, base)) in lines.iter().zip(&original).enumerate() {
            assert_eq!(
                (line.start, line.end, &line.text),
                (base.start, base.end, &base.text)
            );
            assert_eq!(line.glyphs.len(), base.glyphs.len());
            if index + 1 == lines.len() {
                assert_eq!(line.width, base.width);
                for (a, b) in line.glyphs.iter().zip(&base.glyphs) {
                    assert_eq!(a.x, b.x);
                }
            } else if line.text.trim().contains(' ') {
                assert!((line.width - available).abs() < 0.001);
                expanded = true;
            }
            for (a, b) in line.glyphs.iter().zip(&base.glyphs) {
                assert_eq!(
                    (a.id, a.cluster, a.font, a.y),
                    (b.id, b.cluster, b.font, b.y)
                );
            }
        }
        assert!(expanded);
    }
}

#[test]
fn hard_breaks_and_nonbreaking_spaces_are_not_stretched() {
    let fonts = Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    for text in [
        "one two\nthree four five six seven",
        "one\u{a0}two\nthree four five six seven",
        "one\u{a0}two three four five six seven",
    ] {
        let mut doc = Document::from_text("Break", "placeholder").unwrap();
        let mut chapter = doc.chapter(0).unwrap();
        if let Block::Text(p) = &mut chapter.blocks[0] {
            p.text = text.into();
            p.spans[0].range = 0..text.len();
        }
        let config = LayoutConfig {
            width: 180,
            ..Default::default()
        };
        let base = paginate(&chapter, &fonts, config).unwrap();
        if let Block::Text(p) = &mut chapter.blocks[0] {
            p.spans[0].style.align = TextAlign::Justify;
        }
        let justified = paginate(&chapter, &fonts, config).unwrap();
        assert_eq!(
            justified.pages[0].lines[0].width,
            base.pages[0].lines[0].width
        );
    }
}
