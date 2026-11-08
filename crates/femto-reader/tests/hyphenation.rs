use femto_reader::{
    document::{Block, Document},
    layout::{Font, Fonts, LayoutConfig, paginate},
};
fn fonts() -> Fonts {
    Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
            .unwrap(),
    )
}
fn config(width: u32) -> LayoutConfig {
    LayoutConfig {
        width,
        height: 240,
        margin: 12,
        font_size: 24.0,
        ..Default::default()
    }
}

#[test]
fn canonical_accents_use_dictionary_breaks_with_original_source_offsets() {
    use unicode_normalization::UnicodeNormalization;
    use unicode_segmentation::UnicodeSegmentation;
    let fonts = fonts();
    for (language, word) in [
        ("pt", "extraordinário"),
        ("de", "Überraschungen"),
        ("fr", "développement"),
    ] {
        let composed = format!("{word} {word} {word}");
        let decomposed: String = composed.nfd().collect();
        let mut reference = None;
        for text in [&composed, &decomposed] {
            let mut document = Document::from_text("Accents", text).unwrap();
            document.metadata.language = language.into();
            let chapter = document.chapter(0).unwrap();
            let pages = paginate(&chapter, &fonts, config(116)).unwrap();
            let wide = paginate(&chapter, &fonts, config(800)).unwrap();
            for page in &pages.pages {
                femto_reader::layout::paginate_from(
                    &chapter,
                    &fonts,
                    config(116),
                    page.start,
                    |rebuilt| {
                        assert_eq!(rebuilt.start, page.start);
                        assert_eq!(rebuilt.end, page.end);
                        assert_eq!(
                            femto_reader::layout::rasterize(&rebuilt, &fonts, config(116)).unwrap(),
                            femto_reader::layout::rasterize(page, &fonts, config(116)).unwrap()
                        );
                        Ok(false)
                    },
                )
                .unwrap();
                let containing = &wide.pages[wide.page_for(page.start)];
                assert!(containing.start <= page.start && containing.end >= page.start);
            }
            let mut end = 0;
            let mut lines = Vec::new();
            for line in pages.pages.iter().flat_map(|page| &page.lines) {
                assert_eq!(line.start.byte, end);
                end = line.end.byte;
                assert!(
                    end == text.len() || text.grapheme_indices(true).any(|(byte, _)| byte == end)
                );
                assert_eq!(line.text, text[line.start.byte..end].trim_end());
                for glyph in &line.glyphs {
                    assert!((line.start.byte..end).contains(&glyph.cluster));
                    assert!(text.is_char_boundary(glyph.cluster));
                }
                lines.push((line.text.nfc().collect::<String>(), line.hyphenated));
            }
            assert_eq!(end, text.len());
            assert!(
                lines.iter().any(|(_, hyphenated)| *hyphenated),
                "{language}"
            );
            if let Some(expected) = &reference {
                assert_eq!(&lines, expected, "{language}");
            } else {
                reference = Some(lines);
            }
        }
    }
}

#[test]
fn automatic_hyphenation_override_preserves_source_and_author_controls() {
    use femto_reader::{document::Hyphens, layout::Hyphenation};
    let fonts = fonts();
    let mut document =
        Document::from_text("Unknown language", "extraordinario extraordinario").unwrap();
    let mut chapter = document.chapter(0).unwrap();
    let has_hyphens = |chapter: &femto_reader::document::Chapter, mode| {
        let mut layout = config(116);
        layout.hyphenation = mode;
        paginate(chapter, &fonts, layout)
            .unwrap()
            .pages
            .iter()
            .flat_map(|page| &page.lines)
            .any(|line| line.hyphenated)
    };
    assert!(!has_hyphens(&chapter, Hyphenation::Book));
    assert!(has_hyphens(&chapter, Hyphenation::Spanish));
    assert_eq!(chapter.language, "und");
    let Block::Text(paragraph) = &mut chapter.blocks[0] else {
        panic!()
    };
    assert_eq!(paragraph.text, "extraordinario extraordinario");
    for span in &mut paragraph.spans {
        span.style.hyphens = Hyphens::Manual;
    }
    assert!(!has_hyphens(&chapter, Hyphenation::Spanish));
    document.metadata.language = "es".into();
    let chapter = document.chapter(0).unwrap();
    assert!(has_hyphens(&chapter, Hyphenation::Book));
    assert!(!has_hyphens(&chapter, Hyphenation::Off));
    let soft = Document::from_text("Soft", "exten\u{ad}sive")
        .unwrap()
        .chapter(0)
        .unwrap();
    assert!(has_hyphens(&soft, Hyphenation::Off));
}

#[test]
fn saved_layout_without_hyphenation_uses_book_language_and_unknown_modes_fail() {
    use femto_reader::layout::Hyphenation;
    let mut json = serde_json::to_value(config(116)).unwrap();
    json.as_object_mut().unwrap().remove("hyphenation");
    assert_eq!(
        serde_json::from_value::<LayoutConfig>(json.clone())
            .unwrap()
            .hyphenation,
        Hyphenation::Book
    );
    json["hyphenation"] = serde_json::json!("unknown");
    assert!(serde_json::from_value::<LayoutConfig>(json).is_err());
}
#[test]
fn dictionary_breaks_fit_and_keep_source_locations_across_reflow() {
    let fonts = fonts();
    for (language, word) in [
        ("en-US", "extensive"),
        ("de-CH", "Silbentrennung"),
        ("FR", "extraordinaire"),
        ("ES-mx", "extraordinario"),
        ("it-IT", "straordinario"),
        ("nl-NL", "lettergrepen"),
        ("pt-BR", "extraordinário"),
    ] {
        let mut document =
            Document::from_text("Hyphenation", &format!("{word} {word} {word}")).unwrap();
        document.metadata.language = language.into();
        let chapter = document.chapter(0).unwrap();
        assert_eq!(chapter.language, language);
        let Block::Text(paragraph) = &chapter.blocks[0] else {
            panic!()
        };
        let narrow = paginate(&chapter, &fonts, config(116)).unwrap();
        let mut byte = 0;
        let mut hyphenated = 0;
        for line in narrow.pages.iter().flat_map(|page| &page.lines) {
            assert_eq!(line.start.byte, byte);
            byte = line.end.byte;
            assert_eq!(line.text, paragraph.text[line.start.byte..byte].trim_end());
            assert!(line.width <= 92.0);
            for glyph in &line.glyphs {
                assert!((line.start.byte..line.end.byte).contains(&glyph.cluster));
                assert!(paragraph.text.is_char_boundary(glyph.cluster));
            }
            hyphenated += usize::from(line.hyphenated);
        }
        assert_eq!(byte, paragraph.text.len());
        assert!(hyphenated > 0, "{language}");
        let wide = paginate(&chapter, &fonts, config(800)).unwrap();
        assert!(
            !wide
                .pages
                .iter()
                .flat_map(|page| &page.lines)
                .any(|line| line.hyphenated)
        );
        let position = narrow.pages[0].lines[0].end;
        let page = &wide.pages[wide.page_for(position)];
        assert!(page.start <= position && page.end >= position);
    }
}
#[test]
fn hyphen_width_is_measured_and_unknown_languages_keep_emergency_breaks() {
    let fonts = fonts();
    let mut document = Document::from_text("Hyphenation", "extensive").unwrap();
    document.metadata.language = "en".into();
    let pages = paginate(&document.chapter(0).unwrap(), &fonts, config(116)).unwrap();
    let first = &pages.pages[0].lines[0];
    assert!(first.hyphenated);
    assert!(matches!(first.text.as_str(), "ex" | "exten"));
    let glyphs_with_hyphen = first.glyphs.len();
    let prefix = Document::from_text("Prefix", &first.text)
        .unwrap()
        .chapter(0)
        .unwrap();
    let plain = paginate(&prefix, &fonts, config(116)).unwrap();
    assert_eq!(glyphs_with_hyphen, plain.pages[0].lines[0].glyphs.len() + 1);
    assert!(first.width > plain.pages[0].lines[0].width);
    document.metadata.language = "und".into();
    let pages = paginate(&document.chapter(0).unwrap(), &fonts, config(116)).unwrap();
    assert!(
        pages
            .pages
            .iter()
            .flat_map(|page| &page.lines)
            .all(|line| !line.hyphenated)
    );
}
#[test]
fn combining_sequences_and_oversized_words_remain_bounded() {
    let fonts = fonts();
    let text = format!("{} {}", "e\u{301}".repeat(90), "a".repeat(5000));
    let mut document = Document::from_text("Limits", &text).unwrap();
    document.metadata.language = "en".into();
    let chapter = document.chapter(0).unwrap();
    let pages = paginate(&chapter, &fonts, config(116)).unwrap();
    for line in pages.pages.iter().flat_map(|page| &page.lines) {
        assert!(!line.hyphenated);
        assert!(!line.text.starts_with('\u{301}'));
        assert!(line.width <= 92.0);
    }
}

#[test]
fn soft_hyphen_is_invisible_until_used_and_keeps_offsets() {
    let fonts = fonts();
    let text = "ex\u{ad}tensive";
    let chapter = Document::from_text("Soft hyphen", text)
        .unwrap()
        .chapter(0)
        .unwrap();
    let wide = paginate(&chapter, &fonts, config(800)).unwrap();
    let plain = Document::from_text("Plain", "extensive")
        .unwrap()
        .chapter(0)
        .unwrap();
    let plain = paginate(&plain, &fonts, config(800)).unwrap();
    let first = &wide.pages[0].lines[0];
    assert!(!first.hyphenated);
    assert_eq!(first.width, plain.pages[0].lines[0].width);
    assert_eq!(
        first
            .glyphs
            .iter()
            .map(|g| (g.id, g.font, g.x))
            .collect::<Vec<_>>(),
        plain.pages[0].lines[0]
            .glyphs
            .iter()
            .map(|g| (g.id, g.font, g.x))
            .collect::<Vec<_>>()
    );
    let narrow = paginate(&chapter, &fonts, config(116)).unwrap();
    let first = &narrow.pages[0].lines[0];
    assert!(first.hyphenated);
    assert_eq!(first.text, "ex\u{ad}");
    assert_eq!(first.end.byte, 4);
    assert_eq!(first.glyphs.last().unwrap().cluster, 2);
    assert_eq!(narrow.pages[0].lines[1].start.byte, 4);
    assert!(first.width <= 92.0);
}

#[test]
fn soft_hyphen_does_not_force_a_break_when_only_the_unbroken_word_fits() {
    let fonts = fonts();
    // The 'i' is narrower than the hyphen in this font. A fitting unbroken
    // word must win over its overflowing discretionary break.
    let prefix = Document::from_text("Measure", "WWWi")
        .unwrap()
        .chapter(0)
        .unwrap();
    let measurement = paginate(&prefix, &fonts, config(800)).unwrap();
    let width = measurement.pages[0].lines[0].width.ceil() as u32 + 24;
    let chapter = Document::from_text("Soft", "WWW\u{ad}i")
        .unwrap()
        .chapter(0)
        .unwrap();
    let pages = paginate(&chapter, &fonts, config(width)).unwrap();
    assert_eq!(pages.pages[0].lines.len(), 1);
    assert!(!pages.pages[0].lines[0].hyphenated);
    assert_eq!(
        pages.pages[0].lines[0].width,
        measurement.pages[0].lines[0].width
    );
}

#[test]
fn soft_hyphens_at_end_and_style_boundaries_preserve_visibility() {
    let fonts = fonts();
    for text in ["end\u{ad}", "end\u{ad} ", "end\u{ad}\n"] {
        let chapter = Document::from_text("End", text)
            .unwrap()
            .chapter(0)
            .unwrap();
        let pages = paginate(&chapter, &fonts, config(800)).unwrap();
        assert!(!pages.pages[0].lines[0].hyphenated);
    }
    let mut chapter = Document::from_text("Styled", "ex\u{ad}tensive")
        .unwrap()
        .chapter(0)
        .unwrap();
    let Block::Text(p) = &mut chapter.blocks[0] else {
        panic!()
    };
    p.spans = vec![
        femto_reader::document::Span {
            language: None,
            range: 0..2,
            style: Default::default(),
        },
        femto_reader::document::Span {
            language: None,
            range: 2..4,
            style: femto_reader::document::TextStyle {
                scale: 1.5,
                ..Default::default()
            },
        },
        femto_reader::document::Span {
            language: None,
            range: 4..p.text.len(),
            style: Default::default(),
        },
    ];
    let pages = paginate(&chapter, &fonts, config(116)).unwrap();
    let line = &pages.pages[0].lines[0];
    assert!(line.hyphenated);
    assert_eq!(line.glyphs.last().unwrap().size, 36.0);
}

#[test]
fn rtl_soft_hyphen_follows_visual_direction_and_fallback_font() {
    let mut fonts = fonts();
    fonts.fallback.push(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    let chapter = Document::from_text("RTL", "אב\u{ad}גדהוזחטיכלמנסעפצקרשת")
        .unwrap()
        .chapter(0)
        .unwrap();
    let pages = paginate(&chapter, &fonts, config(116)).unwrap();
    let line = &pages.pages[0].lines[0];
    assert!(line.hyphenated);
    let hyphen = line.glyphs.iter().find(|g| g.cluster == 4).unwrap();
    assert!(
        line.glyphs
            .iter()
            .filter(|g| g.cluster != 4)
            .all(|g| g.x >= hyphen.x)
    );
    assert_eq!(hyphen.font, 4);
    assert!(pages.warnings.is_empty());
    assert!(line.width <= 92.0);
}

#[test]
fn mixed_direction_soft_hyphen_stays_with_its_rtl_word() {
    let mut fonts = fonts();
    fonts.fallback.push(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    let text = "A אב\u{ad}גדהוזחטיכלמנסעפצקרשת";
    let chapter = Document::from_text("Mixed", text)
        .unwrap()
        .chapter(0)
        .unwrap();
    let pages = paginate(&chapter, &fonts, config(116)).unwrap();
    let line = &pages.pages[0].lines[0];
    assert!(line.hyphenated);
    let hyphen = line.glyphs.iter().find(|g| g.cluster == 6).unwrap();
    assert!(
        line.glyphs
            .iter()
            .filter(|g| (2..6).contains(&g.cluster))
            .all(|g| g.x >= hyphen.x)
    );
    assert_eq!(hyphen.font, 4);
}

#[test]
fn unbroken_soft_hyphens_preserve_ligatures_kerning_and_rtl_raster() {
    let mut fonts = fonts();
    fonts.fallback.push(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    for (soft, plain) in [
        ("of\u{ad}fice", "office"),
        ("A\u{ad}V", "AV"),
        ("אב\u{ad}גד", "אבגד"),
    ] {
        let config = config(240);
        let soft = Document::from_text("Soft", soft)
            .unwrap()
            .chapter(0)
            .unwrap();
        let plain = Document::from_text("Plain", plain)
            .unwrap()
            .chapter(0)
            .unwrap();
        let soft = paginate(&soft, &fonts, config).unwrap();
        let plain = paginate(&plain, &fonts, config).unwrap();
        assert_eq!(
            femto_reader::layout::rasterize(&soft.pages[0], &fonts, config).unwrap(),
            femto_reader::layout::rasterize(&plain.pages[0], &fonts, config).unwrap()
        );
        assert!(soft.warnings.is_empty());
    }
}

#[test]
fn leading_soft_hyphens_do_not_create_an_empty_or_hyphen_only_line() {
    let fonts = fonts();
    let text = "\u{ad}\u{ad}extensive";
    let chapter = Document::from_text("Leading", text)
        .unwrap()
        .chapter(0)
        .unwrap();
    let pages = paginate(&chapter, &fonts, config(116)).unwrap();
    let first = &pages.pages[0].lines[0];
    assert!(!first.hyphenated);
    assert!(first.text.contains('e'));
    assert_eq!(first.start.byte, 0);
    let last = pages.pages.last().unwrap().lines.last().unwrap();
    assert_eq!(last.end.byte, text.len());
}
