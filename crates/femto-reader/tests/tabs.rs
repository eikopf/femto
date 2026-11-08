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
fn chapter(text: &str, direction: BaseDirection) -> femto_reader::document::Chapter {
    let mut doc = Document::from_text("Tabs", "x").unwrap();
    let mut chapter = doc.chapter(0).unwrap();
    if let Block::Text(p) = &mut chapter.blocks[0] {
        p.text = text.into();
        p.spans[0].range = 0..text.len();
        p.spans[0].style.direction = direction;
    }
    chapter
}

#[test]
fn tab_stops_follow_paragraph_direction_and_scale_without_glyphs_or_source_changes() {
    let fonts = fonts();
    let face =
        rustybuzz::Face::from_slice(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf"), 0)
            .unwrap();
    for size in [16.0, 24.0, 32.0] {
        let config = LayoutConfig {
            font_size: size,
            ..Default::default()
        };
        let advance = |ch| {
            face.glyph_hor_advance(face.glyph_index(ch).unwrap())
                .unwrap() as f32
                * size
                / face.units_per_em() as f32
        };
        let interval = 8.0 * advance(' ');
        for (text, direction, a, b) in [
            ("\tA\tB", BaseDirection::Ltr, 'A', 'B'),
            ("\tא\tב", BaseDirection::Rtl, 'א', 'ב'),
        ] {
            let chapter = chapter(text, direction);
            let pages = paginate(&chapter, &fonts, config).unwrap();
            let line = &pages.pages[0].lines[0];
            assert_eq!(line.text, text);
            assert_eq!((line.start.byte, line.end.byte), (0, text.len()));
            assert_eq!(line.glyphs.len(), 2);
            assert!(
                line.glyphs
                    .iter()
                    .all(|g| g.id != 0 && text.as_bytes()[g.cluster] != b'\t')
            );
            assert!((line.width - (2.0 * interval + advance(b))).abs() < 0.01);
            let first = line.glyphs.iter().find(|g| g.cluster == 1).unwrap();
            let origin = if direction == BaseDirection::Ltr {
                config.margin as f32
            } else {
                (config.width - config.margin) as f32 - line.width
            };
            let expected = if direction == BaseDirection::Ltr {
                interval
            } else {
                line.width - interval - advance(a)
            };
            assert!((first.x - origin - expected).abs() < 0.01);
        }
    }
}

#[test]
fn consecutive_tabs_and_near_stop_text_advance_to_distinct_stops() {
    let fonts = fonts();
    let config = LayoutConfig::default();
    let face =
        rustybuzz::Face::from_slice(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf"), 0)
            .unwrap();
    let space = face
        .glyph_hor_advance(face.glyph_index(' ').unwrap())
        .unwrap() as f32
        * config.font_size
        / face.units_per_em() as f32;
    for (text, stops) in [("\t\tA", 2.0), ("       i\tA", 2.0)] {
        let pages = paginate(&chapter(text, BaseDirection::Ltr), &fonts, config).unwrap();
        let line = &pages.pages[0].lines[0];
        let a = line.glyphs.last().unwrap();
        assert!((a.x - config.margin as f32 - stops * 8.0 * space).abs() < 0.01);
    }
}

#[test]
fn tabbed_lines_wrap_with_contiguous_offsets_and_do_not_justify_off_their_stops() {
    let fonts = fonts();
    let text = "a\tb c d e f g h i j k l m n o p q r s t u v w x y z";
    let mut chapter = chapter(text, BaseDirection::Ltr);
    let config = LayoutConfig {
        width: 180,
        margin: 16,
        ..Default::default()
    };
    let plain = paginate(&chapter, &fonts, config).unwrap();
    if let Block::Text(p) = &mut chapter.blocks[0] {
        p.spans[0].style.align = TextAlign::Justify;
    }
    let justified = paginate(&chapter, &fonts, config).unwrap();
    let a = &plain.pages[0].lines[0];
    let b = &justified.pages[0].lines[0];
    assert!(text[..a.end.byte].contains('\t'));
    assert!(a.end.byte < text.len());
    assert_eq!(a.width, b.width);
    assert_eq!(
        a.glyphs.iter().map(|g| g.x).collect::<Vec<_>>(),
        b.glyphs.iter().map(|g| g.x).collect::<Vec<_>>()
    );
    let mut end = 0;
    for line in plain.pages.iter().flat_map(|p| &p.lines) {
        assert_eq!(line.start.byte, end);
        assert!(line.end.byte > end);
        end = line.end.byte;
    }
    assert_eq!(end, text.len());
}

#[test]
fn epub_tab_sizes_resolve_inheritance_and_inline_overrides() {
    use femto_reader::document::TabSize::{Px, Spaces};
    let mut doc = Document::open(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/tab-size.epub"),
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
    assert_eq!(paragraphs.len(), 9);
    for (p, expected) in paragraphs.iter().zip([
        Spaces(4.0),
        Px(40.0),
        Spaces(0.0),
        Spaces(2.5),
        Spaces(8.0),
        Spaces(4.0),
        Spaces(4.0),
        Spaces(3.0),
    ]) {
        assert_eq!(p.text, "A\tB");
        assert!(p.spans.iter().all(|s| s.style.tab_size == expected));
    }
    assert_eq!(
        paragraphs[8]
            .spans
            .iter()
            .map(|s| s.style.tab_size)
            .collect::<Vec<_>>(),
        vec![Spaces(4.0), Px(40.0), Spaces(0.0)]
    );
    let config = LayoutConfig::default();
    let fonts = fonts();
    let pages = paginate(&chapter, &fonts, config).unwrap();
    let last = pages
        .pages
        .iter()
        .flat_map(|p| &p.lines)
        .find(|l| l.start.block == 8)
        .unwrap();
    assert_eq!(last.glyphs.len(), 3);
    assert!((last.glyphs[1].x - config.margin as f32 - 40.0).abs() < 0.01);
    assert_eq!(
        last.glyphs.iter().map(|g| g.cluster).collect::<Vec<_>>(),
        vec![0, 2, 4]
    );
}

#[test]
fn zero_and_pixel_tabs_preserve_offsets_and_reject_invalid_intervals() {
    use femto_reader::document::TabSize::{Em, Px, Spaces};
    let fonts = fonts();
    let config = LayoutConfig::default();
    for direction in [BaseDirection::Ltr, BaseDirection::Rtl] {
        let mut chapter = chapter("X\tX", direction);
        for tab_size in [
            Spaces(0.0),
            Px(0.0),
            Em(0.0),
            Px(64.0),
            Em(4.0),
            Spaces(2.5),
        ] {
            if let Block::Text(p) = &mut chapter.blocks[0] {
                p.spans[0].style.tab_size = tab_size;
            }
            let pages = paginate(&chapter, &fonts, config).unwrap();
            let line = &pages.pages[0].lines[0];
            assert_eq!(line.text, "X\tX");
            assert_eq!(line.end.byte, 3);
            assert_eq!(line.glyphs.len(), 2);
            assert!(line.width.is_finite());
            if matches!(tab_size, Spaces(0.0) | Px(0.0) | Em(0.0)) {
                let plain = paginate(&self::chapter("XX", direction), &fonts, config).unwrap();
                assert!((line.width - plain.pages[0].lines[0].width).abs() < 0.01);
            }
        }
        for tab_size in [
            Px(-1.0),
            Em(-1.0),
            Em(f32::NAN),
            Em(f32::MAX),
            Spaces(f32::NAN),
            Px(f32::INFINITY),
            Spaces(f32::MAX),
        ] {
            if let Block::Text(p) = &mut chapter.blocks[0] {
                p.spans[0].style.tab_size = tab_size;
            }
            assert!(paginate(&chapter, &fonts, config).is_err());
        }
    }
}

#[test]
fn em_tab_lengths_are_computed_before_inheritance_and_scale_with_reader_size() {
    use femto_reader::document::TabSize::{Em, Px, Spaces};
    let mut doc = Document::open(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/tab-em.epub"),
    )
    .unwrap();
    let chapter = doc.chapter(0).unwrap();
    let paragraphs: Vec<_> = chapter
        .blocks
        .iter()
        .map(|block| {
            let Block::Text(p) = block else { panic!() };
            p
        })
        .collect();
    assert_eq!(paragraphs.len(), 9);
    for (p, expected) in paragraphs.iter().zip([
        Em(3.0),
        Em(3.0),
        Em(2.0),
        Em(3.0),
        Em(3.0),
        Spaces(8.0),
        Em(0.0),
    ]) {
        assert!(p.spans.iter().all(|s| s.style.tab_size == expected));
    }
    assert_eq!(
        paragraphs[7]
            .spans
            .iter()
            .map(|s| s.style.tab_size)
            .collect::<Vec<_>>(),
        [Em(3.0), Em(3.0), Em(4.0)]
    );
    assert_eq!(paragraphs[8].spans[0].style.tab_size, Em(3.0));
    let fonts = fonts();
    for font_size in [16.0, 24.0, 30.0] {
        let config = LayoutConfig {
            font_size,
            ..Default::default()
        };
        let mut pixels = chapter.clone();
        for block in &mut pixels.blocks {
            let Block::Text(p) = block else { panic!() };
            for span in &mut p.spans {
                if let Em(n) = span.style.tab_size {
                    span.style.tab_size = Px(n * font_size);
                }
            }
        }
        let em_pages = paginate(&chapter, &fonts, config).unwrap();
        let px_pages = paginate(&pixels, &fonts, config).unwrap();
        assert_eq!(em_pages.pages.len(), px_pages.pages.len());
        for (a, b) in em_pages.pages.iter().zip(&px_pages.pages) {
            assert_eq!(a.start, b.start);
            assert_eq!(a.end, b.end);
            for (a, b) in a.lines.iter().zip(&b.lines) {
                assert_eq!(a.text, b.text);
                assert_eq!(a.width, b.width);
                assert_eq!(a.glyphs.len(), b.glyphs.len());
                for (a, b) in a.glyphs.iter().zip(&b.glyphs) {
                    assert_eq!((a.id, a.x, a.y, a.cluster), (b.id, b.x, b.y, b.cluster));
                }
            }
        }
        let lines: Vec<_> = em_pages.pages.iter().flat_map(|p| &p.lines).collect();
        for (block, interval) in [(0, 3.0), (1, 3.0), (2, 2.0), (3, 3.0), (4, 3.0)] {
            let line = lines.iter().find(|l| l.start.block == block).unwrap();
            assert!((line.glyphs[1].x - config.margin as f32 - interval * font_size).abs() < 0.01);
        }
    }
}
