use femto_reader::{
    document::{BaseDirection, Block, Document, TextIndent},
    layout::{Font, Fonts, LayoutConfig, paginate, paginate_from},
};

fn fonts() -> Fonts {
    Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    )
}

#[test]
fn excessive_indent_keeps_a_usable_column_and_invalid_values_fail() {
    let fonts = fonts();
    let mut doc = Document::from_text("Bounds", "words with spaces").unwrap();
    let mut chapter = doc.chapter(0).unwrap();
    let config = LayoutConfig::default();
    let Block::Text(p) = &mut chapter.blocks[0] else {
        panic!()
    };
    p.spans[0].style.indent = TextIndent::Percent(10000.0);
    let pages = paginate(&chapter, &fonts, config).unwrap();
    let line = &pages.pages[0].lines[0];
    assert!(line.width <= config.font_size);
    assert_eq!(
        line.glyphs[0].x,
        (config.width - config.margin) as f32 - config.font_size
    );
    for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let Block::Text(p) = &mut chapter.blocks[0] else {
            panic!()
        };
        p.spans[0].style.indent = TextIndent::Pixels(invalid);
        assert!(paginate(&chapter, &fonts, config).is_err());
    }
}

#[test]
fn stylesheet_inheritance_resets_and_inline_boundaries_preserve_block_indent() {
    let mut doc = Document::open(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/indent.epub"),
    )
    .unwrap();
    let chapter = doc.chapter(0).unwrap();
    let expected = [
        TextIndent::Em(2.0),
        TextIndent::Em(3.0),
        TextIndent::Pixels(0.0),
        TextIndent::Percent(10.0),
        TextIndent::Pixels(-3.0),
        TextIndent::Em(2.0),
        TextIndent::Em(2.0),
        TextIndent::Pixels(24.0),
        TextIndent::Em(-3.0),
        TextIndent::Percent(-10.0),
    ];
    assert_eq!(chapter.blocks.len(), expected.len());
    for (block, expected) in chapter.blocks.iter().zip(expected) {
        let Block::Text(p) = block else { panic!() };
        assert!(
            p.spans.iter().all(|span| span.style.indent == expected),
            "{p:?}"
        );
    }
}

#[test]
fn first_line_indents_follow_direction_and_do_not_repeat_after_page_resume() {
    let fonts = fonts();
    let config = LayoutConfig {
        width: 240,
        height: 160,
        margin: 12,
        ..Default::default()
    };
    for (direction, text) in [
        (BaseDirection::Ltr, "words with spaces "),
        (BaseDirection::Rtl, "שלום עולם "),
    ] {
        for (indent, expected) in [
            (TextIndent::Pixels(-6.0), -6.0),
            (TextIndent::Em(-0.25), -6.0),
            (TextIndent::Percent(-2.5), -5.4),
            (TextIndent::Pixels(-10000.0), -12.0),
            (TextIndent::Pixels(30.0), 30.0),
            (TextIndent::Em(2.0), 48.0),
            (TextIndent::Percent(25.0), 54.0),
        ] {
            let mut doc = Document::from_text("Indent", &text.repeat(90)).unwrap();
            let mut chapter = doc.chapter(0).unwrap();
            let Block::Text(p) = &mut chapter.blocks[0] else {
                panic!()
            };
            p.spans[0].style.indent = indent;
            p.spans[0].style.direction = direction;
            let pages = paginate(&chapter, &fonts, config).unwrap();
            assert!(pages.pages.len() > 1);
            for line in pages.pages.iter().flat_map(|page| &page.lines) {
                let inset = if line.start.byte == 0 { expected } else { 0.0 };
                let left = line
                    .glyphs
                    .iter()
                    .map(|glyph| glyph.x)
                    .fold(f32::INFINITY, f32::min);
                if direction == BaseDirection::Ltr {
                    assert!((left - config.margin as f32 - inset).abs() < 0.01);
                } else {
                    assert!(
                        (left + line.width - (config.width - config.margin) as f32 + inset).abs()
                            < 0.01
                    );
                }
                assert!(line.width <= (config.width - 2 * config.margin) as f32 - inset + 0.01);
            }
            let second = &pages.pages[1];
            let mut resumed = Vec::new();
            paginate_from(&chapter, &fonts, config, second.start, |page| {
                resumed.push(page);
                Ok(false)
            })
            .unwrap();
            let reconstructed = &resumed[0];
            assert_eq!(reconstructed.end, second.end);
            for (a, b) in reconstructed.lines.iter().zip(&second.lines) {
                assert_eq!(
                    (a.start, a.width, a.baseline),
                    (b.start, b.width, b.baseline)
                );
                assert_eq!(a.glyphs[0].x, b.glyphs[0].x);
            }
        }
    }
}

#[test]
fn negative_list_indent_keeps_markers_separate_and_inside_the_viewport() {
    let fonts = fonts();
    let config = LayoutConfig {
        width: 240,
        height: 160,
        margin: 12,
        ..Default::default()
    };
    for (direction, text) in [
        (BaseDirection::Ltr, "words with spaces "),
        (BaseDirection::Rtl, "שלום עולם "),
    ] {
        let mut doc = Document::from_text("List indent", &text.repeat(20)).unwrap();
        let mut chapter = doc.chapter(0).unwrap();
        let Block::Text(p) = &mut chapter.blocks[0] else {
            panic!()
        };
        p.list = Some(femto_reader::document::ListItem {
            depth: 1,
            marker: Some("1.".into()),
            marker_label: "1.".into(),
        });
        p.spans[0].style.direction = direction;
        p.spans[0].style.indent = TextIndent::Pixels(-10000.0);
        let pages = paginate(&chapter, &fonts, config).unwrap();
        let line = &pages.pages[0].lines[0];
        let bounds = |glyphs: &[femto_reader::layout::Glyph]| {
            glyphs.iter().fold(
                (f32::INFINITY, f32::NEG_INFINITY),
                |(left, right), glyph| (left.min(glyph.x), right.max(glyph.x + glyph.advance)),
            )
        };
        let (text_left, text_right) = bounds(&line.glyphs);
        let (marker_left, marker_right) = bounds(&line.marker_glyphs);
        assert!(marker_left >= -0.01 && marker_right <= config.width as f32 + 0.01);
        assert!(text_left >= -0.01 && text_right <= config.width as f32 + 0.01);
        let gap = if direction == BaseDirection::Ltr {
            text_left - marker_right
        } else {
            marker_left - text_right
        };
        assert!((gap - config.font_size * 0.3).abs() < 0.01, "{gap}");
        assert!(pages.pages[0].lines[1].marker_glyphs.is_empty());
    }
}
