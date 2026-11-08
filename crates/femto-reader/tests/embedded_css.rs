use femto_reader::document::{Block, Document, Hyphens, TextAlign, TextIndent};

#[test]
fn embedded_and_linked_sheets_share_order_and_inline_overrides() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/embedded-css.epub");
    let mut document = Document::open(&path).unwrap();
    let chapter = document.chapter(0).unwrap();
    assert_eq!(chapter.blocks.len(), 3);
    for (block, (indent, align)) in chapter.blocks.iter().zip([
        (TextIndent::Pixels(0.0), TextAlign::Left),
        (TextIndent::Pixels(36.0), TextAlign::Right),
        (TextIndent::Percent(10.0), TextAlign::Left),
    ]) {
        let Block::Text(p) = block else { panic!() };
        assert!(p.spans.iter().all(|s| s.style.indent == indent
            && s.style.align == align
            && s.style.hyphens == Hyphens::None));
        assert!(!p.text.contains("text-indent"));
    }
    let mut book =
        epub_stream::book::EpubBook::from_reader(std::fs::File::open(path).unwrap()).unwrap();
    let sources = book.chapter_stylesheets(0).unwrap();
    assert_eq!(sources.sources.len(), 4);
    assert!(sources.sources[1].href.contains("#embedded-style-"));
    assert!(sources.sources[3].css.contains("10%"));
    let mut scratch = Vec::new();
    assert_eq!(
        sources,
        book.chapter_stylesheets_with_scratch(0, Default::default(), &mut scratch)
            .unwrap()
    );
    let total: usize = sources.sources.iter().map(|source| source.css.len()).sum();
    assert!(
        sources
            .sources
            .iter()
            .all(|source| source.css.len() < total - 1)
    );
    let limits = epub_stream::render_prep::StyleLimits {
        max_css_bytes: total - 1,
        ..Default::default()
    };
    assert!(book.chapter_stylesheets_with_options(0, limits).is_err());
    assert!(
        book.chapter_stylesheets_with_scratch(0, limits, &mut scratch)
            .is_err()
    );
    let mut options = epub_stream::render_prep::RenderPrepOptions::default();
    options.memory.max_css_bytes = total - 1;
    assert!(book.chapter_styled_runs_with_options(0, options).is_err());
    let limits = epub_stream::render_prep::StyleLimits {
        max_css_bytes: total,
        ..Default::default()
    };
    assert_eq!(
        book.chapter_stylesheets_with_options(0, limits).unwrap(),
        sources
    );
}
