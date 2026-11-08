use epub_stream::render_prep::{StyleConfig, Styler};
use femto_reader::{
    document::{Block, Document, Span},
    layout::{Font, Fonts, LayoutConfig, paginate},
};

#[test]
fn xhtml_language_inherits_clears_and_restores_with_xml_precedence() {
    let styled = Styler::new(StyleConfig::default()).style_chapter(
        r#"<html lang="en"><body><p>A<span lang="de">B<em xml:lang="">C</em>D</span>E<span lang="fr" xml:lang="de-CH">F</span><span xml:lang="fr" lang="de">G</span><span lang="f&#114;">H</span></p></body></html>"#,
    ).unwrap();
    let runs: Vec<_> = styled
        .runs()
        .map(|run| (run.text.as_str(), run.language.as_deref()))
        .collect();
    assert_eq!(
        runs,
        vec![
            ("A", Some("en")),
            ("B", Some("de")),
            ("C", Some("")),
            ("D", Some("de")),
            ("E", Some("en")),
            ("F", Some("de-ch")),
            ("G", Some("fr")),
            ("H", Some("fr"))
        ]
    );
    let undeclared = Styler::new(StyleConfig::default())
        .style_chapter("<p>unknown</p>")
        .unwrap();
    assert_eq!(undeclared.runs().next().unwrap().language, None);
}

#[test]
fn invalid_and_oversized_language_attributes_are_rejected() {
    for tag in ["en_US", "en--US", "fr&bogus;", &"x".repeat(64)] {
        assert!(
            Styler::new(StyleConfig::default())
                .style_chapter(&format!("<p lang=\"{tag}\">text</p>"))
                .is_err(),
            "{tag}"
        );
    }
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
fn epub_language_overrides_control_hyphenation_and_metadata_fallback() {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/language.epub");
    let mut doc = Document::open(path).unwrap();
    let chapter = doc.chapter(0).unwrap();
    let expected = ["en", "de-ch", "fr", "", "es", "en"];
    for (block, language) in chapter.blocks.iter().zip(expected) {
        let Block::Text(p) = block else { panic!() };
        assert!(
            p.spans
                .iter()
                .all(|span| span.language.as_deref() == Some(language))
        );
    }
    let pages = paginate(&chapter, &fonts(), config()).unwrap();
    for (block, expected) in [true, true, true, false, true, true]
        .into_iter()
        .enumerate()
    {
        let hyphenated = pages
            .pages
            .iter()
            .flat_map(|page| &page.lines)
            .filter(|line| line.start.block == block)
            .any(|line| line.hyphenated);
        assert_eq!(hyphenated, expected, "block {block}");
    }
    let Block::Text(p) = chapter.blocks.last().unwrap() else {
        panic!()
    };
    let languages: Vec<_> = p
        .spans
        .iter()
        .map(|span| span.language.as_deref().unwrap())
        .collect();
    assert_eq!(languages, ["de", "fr", "", "fr", "de"]);
}

#[test]
fn dictionary_breaks_do_not_cross_a_language_change_inside_a_word() {
    let mut doc = Document::from_text("Mixed", "extensive").unwrap();
    doc.metadata.language = "en".into();
    let mut chapter = doc.chapter(0).unwrap();
    let Block::Text(p) = &mut chapter.blocks[0] else {
        panic!()
    };
    p.spans = vec![
        Span {
            range: 0..2,
            style: Default::default(),
            language: Some("en".into()),
        },
        Span {
            range: 2..9,
            style: Default::default(),
            language: Some("fr".into()),
        },
    ];
    let pages = paginate(&chapter, &fonts(), config()).unwrap();
    assert!(
        pages
            .pages
            .iter()
            .flat_map(|page| &page.lines)
            .all(|line| !line.hyphenated)
    );
}

#[test]
fn shaping_uses_language_specific_font_substitutions() {
    let fonts = fonts();
    let glyphs = |language: &str| {
        let mut document = Document::from_text("Cyrillic", "б").unwrap();
        document.metadata.language = language.into();
        let pages = paginate(&document.chapter(0).unwrap(), &fonts, config()).unwrap();
        pages.pages[0].lines[0]
            .glyphs
            .iter()
            .map(|glyph| glyph.id)
            .collect::<Vec<_>>()
    };
    // DejaVu Serif contains a Serbian localized form of Cyrillic small be.
    let russian = glyphs("ru");
    let serbian = glyphs("sr");
    assert_eq!(russian.len(), 1);
    assert_eq!(serbian.len(), 1);
    assert_ne!(russian, serbian);
    assert_eq!(glyphs("sr-Cyrl"), serbian);
}

#[test]
fn mixed_script_spans_keep_localized_cyrillic_and_latin_ligatures() {
    let fonts = fonts();
    let glyphs = |text: &str| {
        let mut document = Document::from_text("Scripts", text).unwrap();
        document.metadata.language = "sr".into();
        let pages = paginate(
            &document.chapter(0).unwrap(),
            &fonts,
            LayoutConfig {
                width: 800,
                ..config()
            },
        )
        .unwrap();
        pages.pages[0].lines[0].glyphs.clone()
    };
    let cyrillic = glyphs("б")[0].id;
    let latin: Vec<_> = glyphs("office").iter().map(|g| g.id).collect();
    let text = "office б office";
    let mixed = glyphs(text);
    let cyrillic_at = text.find('б').unwrap();
    assert_eq!(
        mixed.iter().find(|g| g.cluster == cyrillic_at).unwrap().id,
        cyrillic
    );
    assert_eq!(
        mixed
            .iter()
            .filter(|g| g.cluster < 6)
            .map(|g| g.id)
            .collect::<Vec<_>>(),
        latin
    );
    let last = text.rfind("office").unwrap();
    assert_eq!(
        mixed
            .iter()
            .filter(|g| g.cluster >= last)
            .map(|g| g.id)
            .collect::<Vec<_>>(),
        latin
    );
    assert!(mixed.iter().all(|g| text.is_char_boundary(g.cluster)));
}

#[test]
fn mixed_rtl_scripts_preserve_arabic_joining_and_visual_order() {
    let mut fonts = fonts();
    fonts.fallback.push(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    let glyphs = |text: &str| {
        let mut document = Document::from_text("RTL scripts", text).unwrap();
        document.metadata.language = "ar".into();
        let pages = paginate(
            &document.chapter(0).unwrap(),
            &fonts,
            LayoutConfig {
                width: 800,
                ..config()
            },
        )
        .unwrap();
        assert!(pages.warnings.is_empty());
        pages.pages[0].lines[0].glyphs.clone()
    };
    let arabic = glyphs("سلام");
    let text = "שלום سلام";
    let mixed = glyphs(text);
    let start = text.find('س').unwrap();
    let joined: Vec<_> = mixed.iter().filter(|g| g.cluster >= start).collect();
    assert_eq!(
        joined.iter().map(|g| g.id).collect::<Vec<_>>(),
        arabic.iter().map(|g| g.id).collect::<Vec<_>>()
    );
    assert!(joined.len() < "سلام".chars().count());
    let arabic_right = joined.iter().map(|g| g.x).fold(f32::NEG_INFINITY, f32::max);
    assert!(
        mixed
            .iter()
            .filter(|g| g.cluster < start)
            .all(|g| g.x >= arabic_right)
    );
    assert!(mixed.iter().all(|g| text.is_char_boundary(g.cluster)));
}
