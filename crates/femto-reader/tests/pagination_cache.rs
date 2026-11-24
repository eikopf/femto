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
        footer: 0,
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

#[test]
fn prepared_pages_match_fresh_layout_and_rendering() {
    let fonts = fonts();
    let config = LayoutConfig::default();
    let text = "Prepared pages must equal freshly selected pages. ".repeat(400);
    let chapter = || {
        Document::from_text("prepared.txt", &text)
            .unwrap()
            .chapter(0)
            .unwrap()
    };
    let mut prepared = PageCache::new(chapter(), &fonts, config, Locator::default()).unwrap();
    let mut fresh = PageCache::new(chapter(), &fonts, config, Locator::default()).unwrap();
    assert!(prepared.bounds().len() > 3);
    assert!(prepared.prepare(1, &fonts).unwrap());
    assert!(!prepared.prepare(1, &fonts).unwrap());
    assert!(!prepared.prepare(0, &fonts).unwrap());
    for index in [1, 0, 2, 3, 1] {
        prepared.select(index, &fonts).unwrap();
        fresh.select(index, &fonts).unwrap();
        assert_page(prepared.page(), fresh.page());
    }
    assert!(prepared.prepare(99_999, &fonts).is_err());

    let reader = || {
        Reader::new(
            Document::from_text("prepared.txt", &text).unwrap(),
            self::fonts(),
            ReaderState::default(),
        )
        .unwrap()
    };
    let (mut prepared, mut fresh) = (reader(), reader());
    assert!(!prepared.prepare_turn(false, 2).unwrap());
    // Layout, then rendering, then nothing left to do.
    assert!(prepared.prepare_turn(true, 2).unwrap());
    assert!(prepared.prepare_turn(true, 2).unwrap());
    assert!(!prepared.prepare_turn(true, 2).unwrap());
    assert_eq!(prepared.page_index(), 0);
    assert_eq!(prepared.render(2).unwrap(), fresh.render(2).unwrap());
    for action in [Action::Next, Action::Next, Action::Previous] {
        prepared.dispatch(action).unwrap();
        fresh.dispatch(action).unwrap();
        assert_eq!(prepared.page_index(), fresh.page_index());
        assert_eq!(prepared.render(2).unwrap(), fresh.render(2).unwrap());
        prepared.prepare_turn(true, 2).unwrap();
        prepared.prepare_turn(false, 2).unwrap();
    }
}

#[test]
fn opening_shows_the_saved_page_before_the_chapter_is_indexed_and_reuses_a_recorded_index() {
    use femto_reader::page_index::IndexStore;
    let text = "Opening must not wait for pages nobody is looking at yet. ".repeat(900);
    let document = || Document::from_text("lazy.txt", &text).unwrap();
    let mut complete = Reader::new(document(), fonts(), ReaderState::default()).unwrap();
    let pages = complete.page_count();
    assert!(pages > 12);
    let open = |state: ReaderState, store: Option<IndexStore>| {
        // Switch books in a running reader, as the library does.
        let other = Document::from_text("other.txt", "Another book.").unwrap();
        let mut reader = Reader::new(other, fonts(), state).unwrap();
        reader.set_index_store(store);
        let mut opening = reader.begin_open(document()).unwrap();
        let mut steps = 1;
        while !reader.step_open(&mut opening).unwrap() {
            steps += 1;
        }
        reader.finish_open(opening).unwrap();
        (reader, steps)
    };

    // From the chapter start, two pages suffice: the target and its successor.
    let (mut lazy, steps) = open(ReaderState::default(), None);
    assert_eq!(steps, 2);
    assert!(!lazy.is_indexed());
    assert_eq!((lazy.page_index(), lazy.page_count()), (0, 2));
    for index in 0..pages {
        assert_eq!(lazy.page_index(), index);
        assert_eq!(lazy.render(2).unwrap(), complete.render(2).unwrap());
        if index + 1 < pages {
            // Turning past the indexed pages lays the next one out on demand.
            assert_eq!(
                lazy.dispatch(Action::Next).unwrap(),
                femto_reader::controller::Effect::Redraw
            );
            complete.dispatch(Action::Next).unwrap();
        }
    }
    assert!(lazy.page_count() == pages);

    // A saved position inside the chapter needs the pages before it once.
    for _ in 0..5 {
        complete.dispatch(Action::Previous).unwrap();
    }
    let saved = complete.page_index();
    complete.record_position(1);
    let saved_state = complete.state.clone();
    let state = || saved_state.clone();
    let (mut first, steps) = open(state(), None);
    assert_eq!(steps, saved + 2);
    assert_eq!(first.page_index(), saved);
    assert_eq!(first.render(2).unwrap(), complete.render(2).unwrap());

    // Background steps finish the index and record it; a later opening lays
    // out only the saved page and can turn backwards at once.
    let directory = tempfile::tempdir().unwrap();
    let store = || Some(IndexStore::new(directory.path(), "build-1"));
    first.set_index_store(store());
    while first.index_step().unwrap() {}
    assert!(first.is_indexed());
    assert_eq!(first.page_count(), pages);
    let (mut recorded, _) = open(state(), store());
    assert!(recorded.is_indexed());
    assert_eq!(
        (recorded.page_index(), recorded.page_count()),
        (saved, pages)
    );
    assert_eq!(recorded.render(2).unwrap(), complete.render(2).unwrap());
    recorded.dispatch(Action::Previous).unwrap();
    complete.dispatch(Action::Previous).unwrap();
    assert_eq!(recorded.render(2).unwrap(), complete.render(2).unwrap());

    // Another build or another layout ignores the record.
    let other = Some(IndexStore::new(directory.path(), "build-2"));
    assert!(!open(state(), other).0.is_indexed());
    let mut resized = state();
    resized.layout.font_size += 2.0;
    resized.books.clear();
    assert!(!open(resized, store()).0.is_indexed());
}

#[test]
fn a_recorded_index_that_stops_matching_layout_is_replaced_and_reading_continues() {
    use femto_reader::page_index::IndexStore;
    let text = "A build that breaks one line differently must not strand the reader. ".repeat(600);
    let directory = tempfile::tempdir().unwrap();
    let open = |state: ReaderState, recorded: bool| {
        let other = Document::from_text("other.txt", "Another book.").unwrap();
        let mut reader = Reader::new(other, fonts(), state).unwrap();
        reader.set_index_store(recorded.then(|| IndexStore::new(directory.path(), "build")));
        let mut opening = reader
            .begin_open(Document::from_text("book.txt", &text).unwrap())
            .unwrap();
        while !reader.step_open(&mut opening).unwrap() {}
        reader.finish_open(opening).unwrap();
        reader
    };
    let mut fresh = open(ReaderState::default(), false);
    let mut recording = open(ReaderState::default(), true);
    while recording.index_step().unwrap() {}
    let pages = recording.page_count();
    assert!(pages > 8);
    let file = std::fs::read_dir(directory.path())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let sound = std::fs::read(&file).unwrap();

    // Move the boundary between the third and fourth pages by one byte, as a
    // build with other line breaking and the same salt would have recorded
    // it. Twelve header bytes, eight record header bytes, sixteen bytes a
    // page: start block, start byte, end block, end byte.
    let mut stale = sound.clone();
    let (end, next_start) = (20 + 16 * 2 + 12, 20 + 16 * 3 + 4);
    assert_eq!(stale[end..end + 4], stale[next_start..next_start + 4]);
    let moved = u32::from_le_bytes(stale[end..end + 4].try_into().unwrap()) + 1;
    stale[end..end + 4].copy_from_slice(&moved.to_le_bytes());
    stale[next_start..next_start + 4].copy_from_slice(&moved.to_le_bytes());

    // The opening page reproduces, so the record is accepted, in every
    // session. Turning onto the damaged page continues with a fresh index.
    for _ in 0..2 {
        std::fs::write(&file, &stale).unwrap();
        let mut reader = open(ReaderState::default(), true);
        assert!(reader.is_indexed());
        for index in 1..=5 {
            reader.dispatch(Action::Next).unwrap();
            fresh.dispatch(Action::Next).unwrap();
            assert_eq!(reader.page_index(), index);
            assert_eq!(reader.render(2).unwrap(), fresh.render(2).unwrap());
        }
        // The fresh index replaces the record once it is complete.
        assert!(!reader.is_indexed());
        while reader.index_step().unwrap() {}
        assert_eq!(reader.page_count(), pages);
        assert_eq!(std::fs::read(&file).unwrap(), sound);
        while fresh.page_index() > 0 {
            fresh.dispatch(Action::Previous).unwrap();
        }
    }

    // Preparing a turn while idle, as the Picco does, meets the damage first.
    std::fs::write(&file, &stale).unwrap();
    let mut reader = open(ReaderState::default(), true);
    reader.dispatch(Action::Next).unwrap();
    while reader.prepare_turn(true, 2).unwrap() || reader.prepare_turn(false, 2).unwrap() {}
    assert_eq!(reader.page_index(), 1);
    for index in 1..=2 {
        assert_eq!(reader.page_index(), index);
        fresh.dispatch(Action::Next).unwrap();
        assert_eq!(reader.render(2).unwrap(), fresh.render(2).unwrap());
        reader.dispatch(Action::Next).unwrap();
    }

    // Turning backwards from a saved position beyond the damage.
    std::fs::write(&file, &stale).unwrap();
    while fresh.page_index() < 5 {
        fresh.dispatch(Action::Next).unwrap();
    }
    fresh.record_position(1);
    let mut reader = open(fresh.state.clone(), true);
    assert_eq!(reader.page_index(), 5);
    for index in (0..5).rev() {
        reader.dispatch(Action::Previous).unwrap();
        fresh.dispatch(Action::Previous).unwrap();
        assert_eq!(reader.page_index(), index);
    }
    assert_eq!(reader.render(2).unwrap(), fresh.render(2).unwrap());

    // A recorded page that starts outside the chapter cannot be laid out at
    // all; the index is replaced just the same.
    let mut outside = sound.clone();
    let last = 20 + 16 * (pages - 1);
    outside[last..last + 4].copy_from_slice(&0x00ff_ffff_u32.to_le_bytes());
    std::fs::write(&file, &outside).unwrap();
    let mut reader = open(ReaderState::default(), true);
    assert_eq!(reader.page_count(), pages);
    for index in 1..pages {
        reader.dispatch(Action::Next).unwrap();
        fresh.dispatch(Action::Next).unwrap();
        assert_eq!(reader.page_index(), index);
    }
    assert_eq!(reader.page_count(), pages);
    assert_eq!(reader.render(2).unwrap(), fresh.render(2).unwrap());
}
