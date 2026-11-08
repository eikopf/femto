use epub_stream::{
    css::{Hyphens as CssHyphens, parse_inline_style},
    render_prep::{ChapterStylesheets, StyleConfig, Styler, StylesheetSource},
};
use femto_reader::{
    document::{Block, Document, Hyphens},
    layout::{Font, Fonts, LayoutConfig, paginate},
};

#[test]
fn cascade_inheritance_initial_unset_and_invalid_declarations() {
    let mut styler = Styler::new(StyleConfig::default());
    styler
        .load_stylesheets(&ChapterStylesheets {
            sources: vec![StylesheetSource {
                href: "style.css".into(),
                css: ".automatic { hyphens: auto } .off { hyphens: none }".into(),
            }],
        })
        .unwrap();
    let chapter = styler.style_chapter(r#"<body style="hyphens: none"><p>A<span class="automatic">B<em style="hyphens: initial">C</em>D</span>E<span class="automatic" style="hyphens: inherit">F</span><span class="automatic" style="hyphens: unset">G</span><span style="hyphens: manual; hyphens: bogus">H</span></p></body>"#).unwrap();
    let policies: Vec<_> = chapter.runs().map(|run| run.style.hyphens).collect();
    assert_eq!(
        policies,
        [
            CssHyphens::None,
            CssHyphens::Auto,
            CssHyphens::Manual,
            CssHyphens::Auto,
            CssHyphens::None,
            CssHyphens::None,
            CssHyphens::None,
            CssHyphens::Manual
        ]
        .map(Some)
    );
    assert_eq!(
        parse_inline_style("hyphens: AUTO").unwrap().hyphens,
        Some(CssHyphens::Auto)
    );
    assert!(parse_inline_style("hyphens: bogus").unwrap().is_empty());
}
fn fonts() -> Fonts {
    Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
            .unwrap(),
    )
}
fn config() -> LayoutConfig {
    LayoutConfig {
        width: 116,
        height: 400,
        margin: 12,
        ..Default::default()
    }
}
#[test]
fn policies_control_dictionary_and_soft_hyphens_without_changing_source() {
    let fonts = fonts();
    for policy in [Hyphens::None, Hyphens::Manual, Hyphens::Auto] {
        for text in ["extensive", "ex\u{ad}tensive"] {
            let mut doc = Document::from_text("Policy", text).unwrap();
            doc.metadata.language = "en".into();
            let mut chapter = doc.chapter(0).unwrap();
            let Block::Text(p) = &mut chapter.blocks[0] else {
                panic!()
            };
            p.spans[0].style.hyphens = policy;
            let pages = paginate(&chapter, &fonts, config()).unwrap();
            let hyphenated = pages
                .pages
                .iter()
                .flat_map(|p| &p.lines)
                .any(|line| line.hyphenated);
            assert_eq!(
                hyphenated,
                policy == Hyphens::Auto || (policy == Hyphens::Manual && text.contains('\u{ad}'))
            );
            let mut end = 0;
            for line in pages.pages.iter().flat_map(|p| &p.lines) {
                assert_eq!(line.start.byte, end);
                end = line.end.byte;
                assert_eq!(line.text, text[line.start.byte..end]);
                assert!(line.width <= 92.0);
            }
            assert_eq!(end, text.len());
        }
    }
}
#[test]
fn none_preserves_visible_hyphens_and_nonhyphenating_emergency_wraps() {
    let mut doc = Document::from_text("Visible", "ex-tensive").unwrap();
    doc.metadata.language = "en".into();
    let mut chapter = doc.chapter(0).unwrap();
    let Block::Text(p) = &mut chapter.blocks[0] else {
        panic!()
    };
    p.spans[0].style.hyphens = Hyphens::None;
    let pages = paginate(&chapter, &fonts(), config()).unwrap();
    let first = &pages.pages[0].lines[0];
    assert_eq!(first.text, "ex-");
    assert!(!first.hyphenated);
}
#[test]
fn epub_stylesheet_and_inline_override_reach_the_paginator() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/hyphenation.epub");
    let mut doc = Document::open(path).unwrap();
    let chapter = doc.chapter(0).unwrap();
    let policies = [
        Hyphens::None,
        Hyphens::Manual,
        Hyphens::Auto,
        Hyphens::None,
        Hyphens::None,
    ];
    assert_eq!(chapter.blocks.len(), policies.len());
    for (block, expected) in chapter.blocks.iter().zip(policies) {
        let Block::Text(p) = block else { panic!() };
        assert!(p.spans.iter().all(|span| span.style.hyphens == expected));
    }
    let pages = paginate(&chapter, &fonts(), config()).unwrap();
    for (block, expected) in [false, true, true, false, false].into_iter().enumerate() {
        assert_eq!(
            pages
                .pages
                .iter()
                .flat_map(|p| &p.lines)
                .filter(|l| l.start.block == block)
                .any(|l| l.hyphenated),
            expected
        );
    }
}

#[test]
fn xml_soft_hyphen_entities_do_not_break_ligatures_between_identical_styles() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/hyphenation.epub");
    let mut doc = Document::open(path).unwrap();
    let chapter = doc.chapter(0).unwrap();
    let Block::Text(p) = chapter.blocks.last().unwrap() else {
        panic!()
    };
    assert_eq!(p.text, "of\u{ad}fice");
    assert_eq!(p.spans.len(), 1);
    let fonts = fonts();
    let config = LayoutConfig {
        width: 800,
        ..config()
    };
    let pages = paginate(&chapter, &fonts, config).unwrap();
    let line = pages
        .pages
        .iter()
        .flat_map(|p| &p.lines)
        .find(|l| l.start.block == 4)
        .unwrap();
    let plain = Document::from_text("Plain", "office")
        .unwrap()
        .chapter(0)
        .unwrap();
    let plain = paginate(&plain, &fonts, config).unwrap();
    assert_eq!(line.width, plain.pages[0].lines[0].width);
    assert_eq!(
        line.glyphs.iter().map(|g| g.id).collect::<Vec<_>>(),
        plain.pages[0].lines[0]
            .glyphs
            .iter()
            .map(|g| g.id)
            .collect::<Vec<_>>()
    );
}
