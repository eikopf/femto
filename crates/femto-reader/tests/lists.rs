use femto_reader::{
    document::{Block, Document},
    layout::{Font, Fonts, LayoutConfig, paginate, paginate_from, rasterize},
};

#[test]
fn oversized_custom_marker_is_rejected_before_partial_shaping() {
    let fonts = Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    for label in ["x".repeat(4097), "aα".repeat(2000)] {
        let mut document = Document::from_text("Marker limit", "item").unwrap();
        let mut chapter = document.chapter(0).unwrap();
        let Block::Text(p) = &mut chapter.blocks[0] else {
            panic!()
        };
        p.list = Some(femto_reader::document::ListItem {
            depth: 1,
            marker: Some(label.clone()),
            marker_label: label,
        });
        assert!(matches!(
            paginate(&chapter, &fonts, LayoutConfig::default()),
            Err(femto_reader::Error::Limit("line buffer"))
        ));
    }
}

#[test]
fn reversed_lists_count_direct_items_and_preserve_overrides() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut document = Document::open(root.join("list-reversed.epub")).unwrap();
    let chapter = document.chapter(0).unwrap();
    let markers: Vec<_> = chapter
        .blocks
        .iter()
        .filter_map(|block| {
            let Block::Text(p) = block else { panic!() };
            p.list.as_ref().unwrap().marker.as_deref()
        })
        .collect();
    assert_eq!(
        markers,
        [
            "3.",
            "2.",
            "1.",
            "2.",
            "1.",
            "-1.",
            "4.",
            "3.",
            "2.",
            "1.",
            "2.",
            "1.",
            "-2147483648.",
            "-2147483649.",
            "•"
        ]
    );
}

#[test]
fn ordered_list_attributes_override_only_the_current_counter() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut document = Document::open(root.join("list-numbers.epub")).unwrap();
    let chapter = document.chapter(0).unwrap();
    let markers: Vec<_> = chapter
        .blocks
        .iter()
        .filter_map(|block| {
            let Block::Text(p) = block else { panic!() };
            p.list.as_ref().unwrap().marker.as_deref()
        })
        .collect();
    assert_eq!(
        markers,
        [
            "7.",
            "12.",
            "13.",
            "-2.",
            "0.",
            "1.",
            "14.",
            "•",
            "1.",
            "2.",
            "4.",
            "-3.",
            "-2.",
            "-2147483648.",
            "2147483647.",
            "2147483648."
        ]
    );
    assert!(chapter.anchors.is_empty());
    let fonts = Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    let pages = paginate(&chapter, &fonts, LayoutConfig::default()).unwrap();
    let lines: Vec<_> = pages.pages.iter().flat_map(|page| &page.lines).collect();
    let first = lines
        .iter()
        .find(|line| line.text.starts_with("Maximum signed"))
        .unwrap();
    let continuation = lines
        .iter()
        .find(|line| line.text.starts_with("Continuation with"))
        .unwrap();
    assert_eq!(first.glyphs[0].x, continuation.glyphs[0].x);
    assert!(!first.marker_glyphs.is_empty());
    assert!(continuation.marker_glyphs.is_empty());
}

#[test]
fn list_nesting_is_bounded() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut allowed = Document::open(root.join("lists-depth-16.epub")).unwrap();
    assert!(allowed.chapter(0).is_ok());
    let mut excessive = Document::open(root.join("lists-depth-17.epub")).unwrap();
    assert!(
        excessive
            .chapter(0)
            .unwrap_err()
            .to_string()
            .contains("list nesting limit")
    );
}

#[test]
fn list_markers_preserve_source_text_nesting_links_and_page_reconstruction() {
    let mut document = Document::open(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/lists.epub"),
    )
    .unwrap();
    let chapter = document.chapter(0).unwrap();
    let expected = [
        (1, Some("1.")),
        (1, None),
        (2, Some("•")),
        (1, None),
        (1, Some("2.")),
        (1, Some("•")),
        (1, Some("1.")),
    ];
    assert_eq!(chapter.blocks.len(), expected.len());
    for (block, (depth, marker)) in chapter.blocks.iter().zip(expected) {
        let Block::Text(p) = block else { panic!() };
        let item = p.list.as_ref().unwrap();
        assert_eq!((item.depth, item.marker.as_deref()), (depth, marker));
        assert!(!p.text.starts_with("1.") && !p.text.contains('•'));
    }
    let Block::Text(first) = &chapter.blocks[0] else {
        panic!()
    };
    let link = &chapter.links[0];
    assert_eq!(&first.text[link.start.byte..link.end.byte], "linked item");
    assert_eq!(chapter.anchors["first"].byte, 0);
    let fonts = Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    let config = LayoutConfig {
        width: 240,
        height: 240,
        margin: 12,
        ..Default::default()
    };
    let pages = paginate(&chapter, &fonts, config).unwrap();
    let markers: Vec<_> = pages
        .pages
        .iter()
        .flat_map(|p| &p.lines)
        .filter(|l| !l.marker_glyphs.is_empty())
        .collect();
    assert_eq!(markers.len(), 5);
    for line in markers {
        let marker_left = line
            .marker_glyphs
            .iter()
            .map(|g| g.x)
            .fold(f32::INFINITY, f32::min);
        let text_left = line
            .glyphs
            .iter()
            .map(|g| g.x)
            .fold(f32::INFINITY, f32::min);
        if line.start.block == 5 {
            assert!(marker_left > text_left + line.width);
        } else {
            assert!(marker_left < text_left);
        }
    }
    for page in pages.pages.iter().skip(1) {
        let mut reconstructed = None;
        paginate_from(&chapter, &fonts, config, page.start, |page| {
            reconstructed = Some(page);
            Ok(false)
        })
        .unwrap();
        let rebuilt = reconstructed.unwrap();
        assert_eq!(rebuilt.end, page.end);
        for (a, b) in rebuilt.lines.iter().zip(&page.lines) {
            assert_eq!(a.marker_glyphs.len(), b.marker_glyphs.len());
            assert_eq!(a.glyphs[0].x, b.glyphs[0].x);
            for (a, b) in a.marker_glyphs.iter().zip(&b.marker_glyphs) {
                assert_eq!((a.id, a.x, a.y), (b.id, b.x, b.y));
            }
        }
    }
    let page = &pages.pages[0];
    let marked = rasterize(page, &fonts, config).unwrap();
    let mut plain = page.clone();
    for line in &mut plain.lines {
        line.marker_glyphs.clear();
    }
    assert_ne!(marked, rasterize(&plain, &fonts, config).unwrap());
}

#[test]
fn alphabetic_and_roman_lists_preserve_counters_and_item_scope() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut document = Document::open(root.join("list-types.epub")).unwrap();
    let chapter = document.chapter(0).unwrap();
    let paragraphs: Vec<_> = chapter
        .blocks
        .iter()
        .map(|block| {
            let Block::Text(p) = block else { panic!() };
            p
        })
        .collect();
    let markers: Vec<_> = paragraphs
        .iter()
        .filter_map(|p| p.list.as_ref().unwrap().marker.as_deref())
        .collect();
    assert_eq!(
        markers,
        [
            "z.",
            "aa.",
            "IV.",
            "AZ.",
            "ba.",
            "0.",
            "a.",
            "MMMCMXCIX.",
            "4000.",
            "xlix.",
            "ix.",
            "X.",
            "11.",
            "b.",
            "a.",
            "-1.",
            "0.",
            "i.",
            "1.",
            "ZZ.",
            "AAA.",
            "FXSHRXW."
        ]
    );
    for text in ["Continuation.", "Parent tail."] {
        let paragraph = paragraphs.iter().find(|p| p.text == text).unwrap();
        let item = paragraph.list.as_ref().unwrap();
        assert_eq!(item.marker, None);
        assert_eq!(item.marker_label, "aa.");
    }
    let fonts = Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    let config = LayoutConfig {
        width: 320,
        height: 240,
        margin: 12,
        ..Default::default()
    };
    let pages = paginate(&chapter, &fonts, config).unwrap();
    assert_eq!(
        pages
            .pages
            .iter()
            .flat_map(|p| &p.lines)
            .filter(|l| !l.marker_glyphs.is_empty())
            .count(),
        markers.len()
    );
    for page in &pages.pages {
        rasterize(page, &fonts, config).unwrap();
        let mut rebuilt = None;
        paginate_from(&chapter, &fonts, config, page.start, |p| {
            rebuilt = Some(p);
            Ok(false)
        })
        .unwrap();
        let rebuilt = rebuilt.unwrap();
        assert_eq!(rebuilt.end, page.end);
        for (a, b) in rebuilt.lines.iter().zip(&page.lines) {
            assert_eq!(a.marker_glyphs.len(), b.marker_glyphs.len());
            for (a, b) in a.marker_glyphs.iter().zip(&b.marker_glyphs) {
                assert_eq!((a.id, a.x, a.y), (b.id, b.x, b.y));
            }
        }
    }
}

#[test]
fn css_marker_styles_preserve_counters_inheritance_and_source_text() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut doc = Document::open(root.join("list-css.epub")).unwrap();
    let chapter = doc.chapter(0).unwrap();
    let expected = [
        Some("IV."),
        Some("h."),
        None,
        Some("X."),
        Some("◦"),
        Some("•"),
        Some("B."),
        Some("▪"),
        Some("b."),
        Some("a."),
        Some("•"),
        Some("II."),
        Some("IV."),
    ];
    assert_eq!(chapter.blocks.len(), expected.len());
    for (block, expected) in chapter.blocks.iter().zip(expected) {
        let Block::Text(p) = block else { panic!() };
        assert_eq!(
            p.list.as_ref().unwrap().marker.as_deref(),
            expected,
            "{}",
            p.text
        );
        assert!(!p.text.contains('▪') && !p.text.contains('◦'));
    }
    let fonts = Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    let pages = paginate(&chapter, &fonts, LayoutConfig::default()).unwrap();
    let hidden = pages
        .pages
        .iter()
        .flat_map(|page| &page.lines)
        .find(|line| line.text.starts_with("Hidden marker"))
        .unwrap();
    assert!(hidden.marker_glyphs.is_empty());
    assert!(
        pages
            .pages
            .iter()
            .flat_map(|page| &page.lines)
            .any(|line| !line.marker_glyphs.is_empty())
    );
}
