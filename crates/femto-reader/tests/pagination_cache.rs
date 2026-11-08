use femto_reader::{
    controller::{Action, Reader},
    document::Document,
    layout::{Font, Fonts, LayoutConfig, Locator, Page, paginate, paginate_from, rasterize},
    pagination::PageCache,
    state::ReaderState,
};

fn fonts() -> Fonts {
    Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    )
}
fn assert_page(a: &Page, b: &Page) {
    assert_eq!((a.start, a.end), (b.start, b.end));
    assert_eq!(a.lines.len(), b.lines.len());
    assert_eq!(a.images.len(), b.images.len());
    for (a, b) in a.lines.iter().zip(&b.lines) {
        assert_eq!(
            (a.start, a.end, &a.text, a.width, a.height, a.hyphenated),
            (b.start, b.end, &b.text, b.width, b.height, b.hyphenated)
        );
        assert_eq!(a.glyphs.len(), b.glyphs.len());
        assert_eq!(
            (a.baseline, a.ascent, a.descent),
            (b.baseline, b.ascent, b.descent)
        );
        for (a, b) in a.glyphs.iter().zip(&b.glyphs) {
            assert_eq!(
                (a.id, a.font, a.size, a.x, a.y, a.cluster),
                (b.id, b.font, b.size, b.x, b.y, b.cluster)
            );
        }
    }
    for (a, b) in a.images.iter().zip(&b.images) {
        assert_eq!(
            (&a.source, a.x, a.y, a.size.width, a.size.height, a.location),
            (&b.source, b.x, b.y, b.size.width, b.size.height, b.location)
        );
    }
}

#[test]
fn indexed_pages_match_eager_layout_in_both_directions_and_at_saved_positions() {
    let fonts = fonts();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    for name in [
        "sample.epub",
        "images.epub",
        "image-limits.epub",
        "whitespace.epub",
        "tab-size.epub",
        "tab-em.epub",
        "direction.epub",
        "styled-brackets.epub",
        "wrapped-brackets.txt",
    ] {
        let mut document = Document::open(root.join(name)).unwrap();
        let chapter = document.chapter(0).unwrap();
        let config = LayoutConfig {
            width: 240,
            height: 160,
            margin: 12,
            ..Default::default()
        };
        let eager = paginate(&chapter, &fonts, config).unwrap();
        let target = eager.pages.last().unwrap().start;
        let eager_cache = PageCache::new(chapter.clone(), &fonts, config, target).unwrap();
        let mut builder =
            femto_reader::pagination::PageIndexer::new(chapter, config, target).unwrap();
        while !builder.step(&fonts).unwrap() {}
        let mut cache = builder.finish().unwrap();
        assert_eq!(cache.bounds(), eager_cache.bounds());
        assert_eq!(cache.bounds().len(), eager.pages.len());
        assert_page(cache.page(), eager.pages.last().unwrap());
        for index in (0..eager.pages.len()).rev().chain(0..eager.pages.len()) {
            cache.select(index, &fonts).unwrap();
            assert_page(cache.page(), &eager.pages[index]);
        }
        let old = cache.page().clone();
        assert!(cache.select(eager.pages.len(), &fonts).is_err());
        assert_page(cache.page(), &old);
    }
}

#[test]
fn reader_navigates_beyond_the_eager_chapter_cache_limit() {
    let fonts = fonts();
    let text = (0..800)
        .map(|n| format!("Line {n}\n\n"))
        .collect::<String>();
    let mut document = Document::from_text("Large chapter", &text).unwrap();
    let config = LayoutConfig {
        width: 160,
        height: 120,
        margin: 8,
        font_size: 24.0,
        line_height: 1.35,
        hyphenation: Default::default(),
    };
    let chapter = document.chapter(0).unwrap();
    assert!(paginate(&chapter, &fonts, config).is_err());
    let mut state = ReaderState {
        layout: config,
        ..Default::default()
    };
    let mut reader = Reader::new(document, fonts, state.clone()).unwrap();
    assert!(reader.page_count() > 256);
    let first = reader.render(2).unwrap();
    for _ in 1..reader.page_count() {
        reader.dispatch(Action::Next).unwrap();
    }
    let last = reader.position();
    reader.record_position(1);
    state = reader.state.clone();
    let restored = Reader::new(
        Document::from_text("Large chapter", &text).unwrap(),
        self::fonts(),
        state,
    )
    .unwrap();
    assert_eq!(restored.position(), last);
    assert_eq!(restored.page_index(), reader.page_count() - 1);
    for _ in 1..reader.page_count() {
        reader.dispatch(Action::Previous).unwrap();
    }
    assert_eq!(reader.render(2).unwrap(), first);
}

#[test]
fn streaming_stop_does_not_layout_later_blocks_and_empty_chapters_have_a_page() {
    let fonts = fonts();
    let mut doc = Document::from_text("Streaming", "one\n\ntwo\n\nthree\n\nfour\n\nx\tz").unwrap();
    let mut chapter = doc.chapter(0).unwrap();
    let config = LayoutConfig {
        width: 160,
        height: 120,
        margin: 8,
        ..Default::default()
    };
    if let femto_reader::document::Block::Text(p) = &mut chapter.blocks[4] {
        p.spans[0].style.tab_size = femto_reader::document::TabSize::Px(f32::NAN);
    }
    assert!(paginate(&chapter, &fonts, config).is_err());
    let mut count = 0;
    paginate_from(&chapter, &fonts, config, Locator::default(), |_| {
        count += 1;
        Ok(false)
    })
    .unwrap();
    assert_eq!(count, 1);
    chapter.blocks.clear();
    let cache = PageCache::new(chapter, &fonts, config, Locator::default()).unwrap();
    assert_eq!(cache.bounds().len(), 1);
    assert!(cache.page().is_empty());
    assert!(
        rasterize(cache.page(), &fonts, config)
            .unwrap()
            .iter()
            .all(|v| *v == 255)
    );
}

#[test]
fn cooperative_index_yields_one_page_and_matches_the_complete_index() {
    use femto_reader::pagination::PageIndexer;
    let fonts = fonts();
    let mut doc = Document::open(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sample.epub"),
    )
    .unwrap();
    let chapter = doc.chapter(0).unwrap();
    let config = LayoutConfig {
        width: 240,
        height: 160,
        margin: 12,
        ..Default::default()
    };
    let eager = paginate(&chapter, &fonts, config).unwrap();
    let target = eager.pages[eager.pages.len() / 2].start;
    let mut builder = PageIndexer::new(chapter, config, target).unwrap();
    assert!(!builder.is_finished());
    for index in 0..eager.pages.len() {
        assert_eq!(builder.page_count(), index);
        assert_eq!(
            builder.step(&fonts).unwrap(),
            index + 1 == eager.pages.len()
        );
        assert_eq!(builder.page_count(), index + 1);
    }
    assert!(builder.step(&fonts).unwrap());
    assert_eq!(builder.page_count(), eager.pages.len());
    let mut cache = builder.finish().unwrap();
    assert_eq!(cache.page().start, target);
    for (index, page) in eager.pages.iter().enumerate() {
        cache.select(index, &fonts).unwrap();
        assert_page(cache.page(), page);
    }
}
