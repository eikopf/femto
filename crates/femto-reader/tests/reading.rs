use std::path::PathBuf;

use femto_reader::{
    document::{Block, Document},
    layout::{Font, Fonts, LayoutConfig, paginate, rasterize},
    state::{Library, ReaderState},
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}
fn fonts() -> Fonts {
    let mut fonts = Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf").to_vec())
            .unwrap(),
    );
    fonts.fallback.push(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    );
    fonts
}

#[test]
fn epub_metadata_styles_pagination_and_reflow() {
    let mut doc = Document::open(fixture("sample.epub")).unwrap();
    assert_eq!(doc.metadata.title, "Femto reading sample");
    assert_eq!(doc.metadata.chapters.len(), 2);
    assert_eq!(doc.metadata.contents.len(), 2);
    let chapter = doc.chapter(0).unwrap();
    assert!(chapter.blocks.iter().any(|b| matches!(b, Block::Text(p) if p.text.contains("with bold text, italic text, and ordinary punctuation."))));
    assert!(
        chapter
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Text(p) if p.spans.iter().any(|s| s.style.bold)))
    );
    assert!(
        chapter
            .blocks
            .iter()
            .any(|b| matches!(b, Block::Text(p) if p.spans.iter().any(|s| s.style.italic)))
    );
    let fonts = fonts();
    let config = LayoutConfig::default();
    let small = paginate(&chapter, &fonts, config).unwrap();
    assert!(small.warnings.is_empty(), "{:?}", small.warnings);
    assert!(small.pages.len() >= 3);
    let anchor = small.pages[2].start;
    let large = paginate(
        &chapter,
        &fonts,
        LayoutConfig {
            font_size: 32.0,
            ..config
        },
    )
    .unwrap();
    assert!(large.pages.len() > small.pages.len());
    let page = &large.pages[large.page_for(anchor)];
    assert!(page.start <= anchor && page.end >= anchor);
    for page in &small.pages {
        assert!(page.start < page.end);
        for line in &page.lines {
            let Block::Text(p) = &chapter.blocks[line.start.block] else {
                panic!("not text")
            };
            assert!(p.text.is_char_boundary(line.start.byte));
            assert!(p.text.is_char_boundary(line.end.byte));
            assert!(line.width <= (config.width - config.margin * 2) as f32 + 1.0);
            assert_eq!(line.text, p.text[line.start.byte..line.end.byte].trim_end());
        }
    }
    let pixels = rasterize(&small.pages[0], &fonts, config).unwrap();
    assert_eq!(pixels.len(), 480 * 800);
    let ink = pixels.iter().filter(|pixel| **pixel < 128).count();
    assert!(ink > 1000 && ink < pixels.len() / 2);
    assert!(pixels[..480 * 10].iter().all(|pixel| *pixel == 255));
}

#[test]
fn every_text_byte_is_consumed_once_across_page_boundaries() {
    let mut doc = Document::open(fixture("sample.txt")).unwrap();
    let chapter = doc.chapter(0).unwrap();
    let pages = paginate(&chapter, &fonts(), LayoutConfig::default()).unwrap();
    for (block, content) in chapter.blocks.iter().enumerate() {
        let Block::Text(paragraph) = content else {
            continue;
        };
        let mut offset = 0;
        for line in pages
            .pages
            .iter()
            .flat_map(|p| &p.lines)
            .filter(|l| l.start.block == block)
        {
            assert_eq!(line.start.byte, offset);
            assert!(line.end.byte > offset);
            offset = line.end.byte;
        }
        assert_eq!(offset, paragraph.text.len());
    }
}

#[test]
fn library_search_and_recent_use_content_identity() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["sample.epub", "sample.txt"] {
        std::fs::copy(fixture(name), dir.path().join(name)).unwrap();
    }
    let library = Library::scan(dir.path()).unwrap();
    assert_eq!(library.entries.len(), 2);
    assert!(library.errors.is_empty());
    assert_eq!(library.search("contributors").len(), 1);
    let mut state = ReaderState::default();
    let entry = &library.entries[1];
    state.set_position(&entry.metadata.id, Default::default(), 123);
    assert_eq!(library.recent(&state)[0].metadata.id, entry.metadata.id);
}

#[test]
fn invalid_viewports_are_errors() {
    for config in [
        LayoutConfig {
            margin: u32::MAX,
            ..Default::default()
        },
        LayoutConfig {
            font_size: f32::NAN,
            ..Default::default()
        },
        LayoutConfig {
            height: 1,
            ..Default::default()
        },
    ] {
        assert!(config.validate().is_err());
    }
}
