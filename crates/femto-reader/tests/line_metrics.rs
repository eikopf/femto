use femto_reader::{
    document::{Block, Document, Span},
    layout::{Font, Fonts, LayoutConfig, paginate},
};

const FONT: &[u8] = include_bytes!("../../../tests/fixtures/DejaVuSans.ttf");

#[test]
fn oversized_line_reports_an_error_instead_of_crossing_page_bounds() {
    let fonts = Fonts::new(Font::from_bytes(FONT.to_vec()).unwrap());
    let mut document = Document::from_text("Oversized", "BIG").unwrap();
    let mut chapter = document.chapter(0).unwrap();
    let Block::Text(paragraph) = &mut chapter.blocks[0] else {
        panic!()
    };
    paragraph.spans[0].style.scale = 3.0;
    let config = LayoutConfig {
        width: 240,
        height: 120,
        margin: 8,
        font_size: 32.0,
        line_height: 1.0,
        hyphenation: Default::default(),
        footer: 0,
    };
    config.validate().unwrap();
    assert!(matches!(
        paginate(&chapter, &fonts, config),
        Err(femto_reader::Error::Layout(
            "line metrics exceed reading area"
        ))
    ));
}

#[test]
fn tight_lines_contain_accents_descenders_and_shaping_offsets() {
    let fonts = Fonts::new(Font::from_bytes(FONT.to_vec()).unwrap());
    let face = rustybuzz::Face::from_slice(FONT, 0).unwrap();
    let mut document = Document::from_text(
        "Metrics",
        &"A\u{301}\u{302}\u{303}gjpq العربية שָׁלוֹם ".repeat(100),
    )
    .unwrap();
    let config = LayoutConfig {
        width: 240,
        height: 200,
        margin: 12,
        font_size: 24.0,
        line_height: 1.0,
        hyphenation: Default::default(),
        footer: 0,
    };
    let pages = paginate(&document.chapter(0).unwrap(), &fonts, config).unwrap();
    assert!(pages.pages.len() > 1);
    let mut extended_accent = false;
    for page in &pages.pages {
        let mut top = config.margin as f32;
        for line in &page.lines {
            assert!(line.height >= line.ascent + line.descent);
            assert!(top + line.height <= (config.height - config.margin) as f32 + 0.01);
            for glyph in &line.glyphs {
                let factor = glyph.size / face.units_per_em() as f32;
                extended_accent |= line.ascent > face.ascender() as f32 * factor + 0.01;
                if let Some(bounds) =
                    face.glyph_bounding_box(rustybuzz::ttf_parser::GlyphId(glyph.id))
                {
                    let ink_top = glyph.y - f32::from(bounds.y_max) * factor;
                    let ink_bottom = glyph.y - f32::from(bounds.y_min) * factor;
                    assert!(ink_top >= top - 0.01, "ink crosses line top");
                    assert!(
                        ink_bottom <= top + line.height + 0.01,
                        "ink crosses line bottom"
                    );
                }
            }
            top += line.height;
        }
    }
    assert!(extended_accent, "fixture must exceed the face's ascender");
}

#[test]
fn mixed_sizes_share_a_baseline_and_spacing_remains_a_minimum() {
    let fonts = Fonts::new(Font::from_bytes(FONT.to_vec()).unwrap());
    let mut document = Document::from_text("Mixed sizes", "small BIG small").unwrap();
    let mut chapter = document.chapter(0).unwrap();
    let Block::Text(paragraph) = &mut chapter.blocks[0] else {
        panic!()
    };
    let mut large = paragraph.spans[0].style;
    large.scale = 2.0;
    paragraph.spans = vec![
        Span {
            range: 0..6,
            language: None,
            style: Default::default(),
        },
        Span {
            range: 6..9,
            language: None,
            style: large,
        },
        Span {
            range: 9..15,
            language: None,
            style: Default::default(),
        },
    ];
    for spacing in [1.0, 1.35, 2.0] {
        let config = LayoutConfig {
            line_height: spacing,
            ..Default::default()
        };
        let pages = paginate(&chapter, &fonts, config).unwrap();
        let line = &pages.pages[0].lines[0];
        assert!(line.height >= 48.0 * spacing);
        assert!(line.height >= line.ascent + line.descent);
        let baseline =
            config.margin as f32 + line.ascent + (line.height - line.ascent - line.descent) / 2.0;
        assert!(
            line.glyphs
                .iter()
                .all(|glyph| (glyph.y - baseline).abs() < 0.01)
        );
    }
}

#[test]
fn a_line_takes_the_next_word_when_only_shaping_makes_it_fit() {
    let fonts = Fonts::new(Font::from_bytes(FONT.to_vec()).unwrap());
    let lines = |text: &str, width: u32| {
        let chapter = Document::from_text("Fit", text)
            .unwrap()
            .chapter(0)
            .unwrap();
        let config = LayoutConfig {
            width,
            margin: 0,
            hyphenation: femto_reader::layout::Hyphenation::Off,
            ..Default::default()
        };
        let pages = paginate(&chapter, &fonts, config).unwrap().pages;
        let lines = pages.into_iter().flat_map(|page| page.lines);
        lines.collect::<Vec<_>>()
    };
    // Shaped, both are far narrower than their letters one by one: the
    // Arabic letters join, and every pair of the Latin word is kerned.
    for (fitting, next) in [("ببببب ببببب", "ببببب"), ("Yo YoYoYoYoYoYoYoYo", "Yo")]
    {
        let alone = lines(fitting, 2048);
        assert_eq!(alone.len(), 1);
        let column = alone[0].width.ceil() as u32;
        let broken = lines(&format!("{fitting} {next}"), column);
        let texts: Vec<_> = broken.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(texts, [fitting, next]);
    }
}
