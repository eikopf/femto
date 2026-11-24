//! Discovery properties for pagination: coverage, fit, determinism, and
//! agreement between eager, resumed and cooperative pagination.
use femto_reader::{
    document::{
        BaseDirection, Block, Chapter, Hyphens, ListItem, Paragraph, Span, TextAlign, TextIndent,
        TextStyle,
    },
    layout::{Font, Fonts, Hyphenation, LayoutConfig, Locator, Page, paginate_from},
    pagination::{PageCache, PageIndexer},
};
use proptest::prelude::*;

fn font(name: &str) -> Font {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    Font::load(root.join(name)).unwrap()
}

fn fonts(serif: bool) -> Fonts {
    if serif {
        let mut fonts = Fonts::new(font("DejaVuSerif.ttf"));
        fonts.fallback.push(font("DejaVuSans.ttf"));
        fonts
    } else {
        Fonts::new(font("DejaVuSans.ttf"))
    }
}

fn atom() -> impl Strategy<Value = String> {
    prop_oneof![
        8 => "[a-z]{1,9}",
        6 => Just(" ".to_string()),
        2 => "[A-Za-z]{10,30}",
        1 => "[a-zA-Z0-9]{60,200}",
        1 => Just("  ".to_string()),
        1 => Just("\u{ad}".to_string()),
        1 => Just("hy\u{ad}phen\u{ad}ation".to_string()),
        1 => Just("e\u{301}".to_string()),
        1 => Just("a\u{300}\u{301}\u{302}\u{303}".to_string()),
        1 => "[\u{5d0}-\u{5ea}]{1,8}",
        1 => "[\u{627}-\u{64a}]{1,8}",
        1 => "[\u{4e00}-\u{4e20}]{1,6}",
        1 => Just("\n".to_string()),
        1 => Just("\t".to_string()),
        1 => Just("\u{a0}".to_string()),
        1 => Just("\u{200b}".to_string()),
        1 => Just("\u{2028}".to_string()),
        1 => Just("👩\u{200d}👩\u{200d}👧".to_string()),
        1 => Just("(".to_string()),
        1 => Just(")".to_string()),
        1 => Just("-".to_string()),
        1 => Just("international".to_string()),
        1 => Just("Donaudampfschifffahrtsgesellschaft".to_string()),
        1 => Just("AVAVAVAVAV".to_string()),
        1 => Just("fifflffi".to_string()),
        1 => Just("\r".to_string()),
        1 => Just("\u{202e}".to_string()),
        1 => Just("\u{2067}".to_string()),
        1 => Just("\u{2069}".to_string()),
    ]
}

fn style() -> impl Strategy<Value = TextStyle> {
    (
        prop_oneof![
            4 => Just(TextAlign::Start),
            1 => Just(TextAlign::End),
            1 => Just(TextAlign::Left),
            1 => Just(TextAlign::Center),
            1 => Just(TextAlign::Right),
            2 => Just(TextAlign::Justify),
        ],
        prop_oneof![
            4 => Just(BaseDirection::Auto),
            1 => Just(BaseDirection::Ltr),
            1 => Just(BaseDirection::Rtl),
        ],
        prop_oneof![
            3 => Just(Hyphens::Auto),
            1 => Just(Hyphens::Manual),
            1 => Just(Hyphens::None),
        ],
        prop_oneof![
            4 => Just(TextIndent::Pixels(0.0)),
            1 => (-40.0f32..80.0).prop_map(TextIndent::Pixels),
            1 => (-3.0f32..6.0).prop_map(TextIndent::Em),
            1 => (-50.0f32..150.0).prop_map(TextIndent::Percent),
        ],
        prop_oneof![5 => Just(1.0f32), 1 => 0.5f32..2.0],
        any::<bool>(),
    )
        .prop_map(|(align, direction, hyphens, indent, scale, bold)| TextStyle {
            align,
            direction,
            hyphens,
            indent,
            scale,
            bold,
            ..Default::default()
        })
}

/// A paragraph of one or more spans. Empty and whitespace-only texts occur.
fn paragraph() -> impl Strategy<Value = Paragraph> {
    (
        prop::collection::vec((prop::collection::vec(atom(), 0..40), style()), 1..3),
        prop::option::weighted(0.15, (0usize..5, "[0-9•]{1,3}")),
        any::<bool>(),
    )
        .prop_map(|(parts, list, same_style)| {
            let mut text = String::new();
            let mut spans = Vec::new();
            let first = parts[0].1;
            for (atoms, style) in parts {
                let start = text.len();
                for atom in atoms {
                    text.push_str(&atom);
                }
                if text.len() > start {
                    spans.push(Span {
                        language: Some("en".into()),
                        range: start..text.len(),
                        style: if same_style { first } else { style },
                    });
                }
            }
            // The parser drops paragraphs whose text trims to nothing.
            if text.trim().is_empty() {
                let start = text.len();
                text.push('x');
                spans.push(Span {
                    language: Some("en".into()),
                    range: start..text.len(),
                    style: first,
                });
            }
            if same_style && spans.len() == 2 {
                spans[0].range.end = spans[1].range.end;
                spans.pop();
            }
            Paragraph {
                list: list.map(|(depth, label)| ListItem {
                    depth,
                    marker: Some(label.clone()),
                    marker_label: label,
                }),
                text,
                spans,
            }
        })
}

fn chapter() -> impl Strategy<Value = Chapter> {
    prop::collection::vec(paragraph(), 0..8).prop_map(|paragraphs| Chapter {
        language: "en".into(),
        index: 3,
        href: "c.xhtml".into(),
        blocks: paragraphs.into_iter().map(Block::Text).collect(),
        warnings: vec![],
        anchors: Default::default(),
        notes: vec![],
        links: vec![],
    })
}

fn hyphenation() -> impl Strategy<Value = Hyphenation> {
    prop_oneof![
        Just(Hyphenation::Book),
        Just(Hyphenation::Off),
        Just(Hyphenation::English),
        Just(Hyphenation::German),
    ]
}

/// Mostly valid configurations with boundary dimensions.
fn config() -> impl Strategy<Value = LayoutConfig> {
    (
        prop_oneof![Just(80u32), Just(81), 80u32..700, Just(2048)],
        prop_oneof![Just(80u32), 80u32..500, Just(2048)],
        prop_oneof![Just(0u32), 0u32..30, 0u32..300],
        prop_oneof![Just(0u32), 0u32..60, 0u32..300],
        prop_oneof![Just(8.0f32), 8.0f32..40.0, Just(96.0)],
        prop_oneof![Just(1.0f32), 1.0f32..3.0, Just(3.0)],
        hyphenation(),
    )
        .prop_map(
            |(width, height, margin, footer, font_size, line_height, hyphenation)| LayoutConfig {
                width,
                height,
                margin,
                footer,
                font_size,
                line_height,
                hyphenation,
            },
        )
}

fn all_pages(
    chapter: &Chapter,
    fonts: &Fonts,
    config: LayoutConfig,
    start: Locator,
) -> femto_reader::Result<Vec<Page>> {
    let mut pages = Vec::new();
    paginate_from(chapter, fonts, config, start, |page| {
        pages.push(page);
        Ok(true)
    })?;
    Ok(pages)
}

/// Everything observable about a page, bitwise for floats.
fn fingerprint(page: &Page) -> String {
    format!("{page:?}")
}

fn text_of(chapter: &Chapter, block: usize) -> &str {
    match &chapter.blocks[block] {
        Block::Text(paragraph) => &paragraph.text,
        Block::Image(_) => "",
    }
}

/// Lines cover every byte of every nonempty block once, in order.
fn check_coverage(chapter: &Chapter, pages: &[Page]) -> Result<(), TestCaseError> {
    let mut expected = Locator {
        chapter: chapter.index,
        block: 0,
        byte: 0,
    };
    let skip_done = |mut at: Locator| {
        while at.block < chapter.blocks.len() && at.byte == text_of(chapter, at.block).len() {
            at.block += 1;
            at.byte = 0;
        }
        at
    };
    expected = skip_done(expected);
    for (index, page) in pages.iter().enumerate() {
        if page.lines.is_empty() {
            prop_assert!(
                pages.len() == 1 && expected.block == chapter.blocks.len(),
                "empty page {index} of {} in a chapter with text",
                pages.len()
            );
            continue;
        }
        prop_assert_eq!(page.start, page.lines[0].start, "page {} start", index);
        prop_assert_eq!(
            page.end,
            page.lines.last().unwrap().end,
            "page {} end",
            index
        );
        for line in &page.lines {
            prop_assert_eq!(line.start, expected, "gap or overlap before a line");
            prop_assert!(line.end.byte > line.start.byte, "empty line {:?}", line);
            prop_assert_eq!(line.end.block, line.start.block);
            let text = text_of(chapter, line.start.block);
            prop_assert!(line.end.byte <= text.len());
            prop_assert!(text.is_char_boundary(line.end.byte));
            expected = skip_done(line.end);
        }
    }
    prop_assert_eq!(expected.block, chapter.blocks.len(), "text after last page");
    for pair in pages.windows(2) {
        prop_assert!(pair[0].start < pair[1].start);
        prop_assert!(pair[0].end <= pair[1].start);
    }
    Ok(())
}

/// A line of more than one grapheme is no wider than the column, and its
/// glyphs stay between the page edges.
fn check_width(
    chapter: &Chapter,
    pages: &[Page],
    config: LayoutConfig,
) -> Result<(), TestCaseError> {
    use unicode_segmentation::UnicodeSegmentation;
    let available = (config.width - 2 * config.margin) as f32;
    for line in pages.iter().flat_map(|page| &page.lines) {
        let Block::Text(paragraph) = &chapter.blocks[line.start.block] else {
            continue;
        };
        let plain = paragraph.list.is_none()
            && paragraph
                .spans
                .first()
                .is_none_or(|span| span.style.indent == TextIndent::Pixels(0.0));
        // A negative indent may reach into the margin.
        let limit = if plain {
            available
        } else {
            available + config.margin as f32
        };
        if line.text.graphemes(true).count() > 1 {
            prop_assert!(
                line.width <= limit + 0.01,
                "line {:?} is {} wide in a column of {}",
                line.text,
                line.width,
                limit
            );
            if plain {
                for glyph in &line.glyphs {
                    prop_assert!(
                        glyph.x >= config.margin as f32 - 0.01
                            && glyph.x + glyph.advance.max(0.0)
                                <= (config.width - config.margin) as f32 + 0.01,
                        "glyph at {} + {} outside the column of line {:?}",
                        glyph.x,
                        glyph.advance,
                        line.text
                    );
                }
            }
        }
    }
    Ok(())
}

/// Lines stay inside the reading area vertically and do not overlap.
fn check_fit(pages: &[Page], config: LayoutConfig) -> Result<(), TestCaseError> {
    let top = config.margin as f32;
    let bottom = (config.height - config.margin.max(config.footer)) as f32;
    for page in pages {
        let mut previous = top;
        let mut block = page.start.block;
        for line in &page.lines {
            let half = (line.height - (line.ascent + line.descent)) / 2.0;
            let line_top = line.baseline - line.ascent - half;
            // Each finished block is followed by a gap of 0.4 em.
            previous += (line.start.block - block) as f32 * config.font_size * 0.4;
            block = line.start.block;
            prop_assert!(
                (line_top - previous).abs() < 0.01,
                "line top {line_top} after {previous}"
            );
            previous = line_top + line.height;
            prop_assert!(
                previous <= bottom + 0.01,
                "line bottom {previous} beyond {bottom}"
            );
        }
    }
    Ok(())
}

fn check_chapter(chapter: &Chapter, fonts: &Fonts, config: LayoutConfig, picks: &[usize]) -> Result<(), TestCaseError> {
    let start = Locator {
        chapter: chapter.index,
        ..Default::default()
    };
    let pages = match all_pages(chapter, fonts, config, start) {
        Ok(pages) => pages,
        Err(error) => {
            prop_assert!(
                config.validate().is_err()
                    || matches!(
                        error.to_string().as_str(),
                        "invalid layout: line metrics exceed reading area"
                            | "invalid layout: list marker exceeds reading width"
                    ),
                "valid layout failed: {error}"
            );
            return Ok(());
        }
    };
    prop_assert!(config.validate().is_ok());
    prop_assert!(!pages.is_empty());
    check_coverage(chapter, &pages)?;
    check_fit(&pages, config)?;
    check_width(chapter, &pages, config)?;

    // Deterministic.
    let again = all_pages(chapter, fonts, config, start).unwrap();
    prop_assert_eq!(pages.len(), again.len());
    for (a, b) in pages.iter().zip(&again) {
        prop_assert_eq!(fingerprint(a), fingerprint(b));
    }

    // Resuming at any page start reproduces the rest.
    for &pick in picks {
        let from = pick % pages.len();
        let resumed = all_pages(chapter, fonts, config, pages[from].start).unwrap();
        prop_assert_eq!(resumed.len(), pages.len() - from, "resumed at page {}", from);
        for (a, b) in pages[from..].iter().zip(&resumed) {
            prop_assert_eq!(fingerprint(a), fingerprint(b), "resumed at page {}", from);
        }
    }

    // The cooperative indexer finds the same bounds and target page.
    let target = pages[picks[0] % pages.len()].start;
    let mut job = PageIndexer::new(chapter.clone(), config, target).unwrap();
    while !job.is_finished() {
        job.step(fonts).unwrap();
    }
    let mut cache = job.finish().unwrap();
    prop_assert_eq!(cache.bounds().len(), pages.len());
    for (bounds, page) in cache.bounds().iter().zip(&pages) {
        prop_assert_eq!((bounds.start, bounds.end), (page.start, page.end));
    }
    prop_assert_eq!(
        fingerprint(cache.page()),
        fingerprint(&pages[picks[0] % pages.len()])
    );
    for &pick in picks {
        let index = pick % pages.len();
        prop_assert_eq!(cache.page_for(pages[index].start), index);
        cache.select(index, fonts).unwrap();
        prop_assert_eq!(fingerprint(cache.page()), fingerprint(&pages[index]));
    }

    // A partial index grows into the same pages.
    let mut job = PageIndexer::new(chapter.clone(), config, target)
        .unwrap()
        .partial();
    while !job.step(fonts).unwrap() {}
    let mut cache = job.finish().unwrap();
    prop_assert_eq!(
        fingerprint(cache.page()),
        fingerprint(&pages[picks[0] % pages.len()])
    );
    for &pick in picks {
        let index = pick % pages.len();
        cache.index_until(pages[index].start, fonts).unwrap();
        prop_assert_eq!(cache.page_for(pages[index].start), index);
        cache.select(index, fonts).unwrap();
        prop_assert_eq!(fingerprint(cache.page()), fingerprint(&pages[index]));
    }
    while cache.extend(fonts).unwrap() {}
    prop_assert!(cache.is_complete());
    prop_assert_eq!(cache.bounds().len(), pages.len());
    for (bounds, page) in cache.bounds().iter().zip(&pages) {
        prop_assert_eq!((bounds.start, bounds.end), (page.start, page.end));
    }

    // The eager cache agrees too.
    let cache = PageCache::new(chapter.clone(), fonts, config, target).unwrap();
    prop_assert_eq!(
        fingerprint(cache.page()),
        fingerprint(&pages[picks[0] % pages.len()])
    );
    Ok(())
}

fn cases() -> u32 {
    std::env::var("PROP_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(64)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), max_shrink_iters: 2000, ..ProptestConfig::default() })]

    #[test]
    fn pagination_covers_fits_and_resumes(
        chapter in chapter(),
        config in config(),
        serif in any::<bool>(),
        picks in prop::collection::vec(0usize..1000, 1..5),
    ) {
        let fonts = fonts(serif);
        check_chapter(&chapter, &fonts, config, &picks)?;
    }

    /// Small pages force many page breaks inside paragraphs.
    #[test]
    fn small_pages_cover_fit_and_resume(
        chapter in chapter(),
        width in 80u32..200,
        height in 80u32..160,
        margin in 0u32..8,
        font_size in 8.0f32..16.0,
        hyphenation in hyphenation(),
        serif in any::<bool>(),
        picks in prop::collection::vec(0usize..1000, 1..5),
    ) {
        let config = LayoutConfig { width, height, margin, footer: 0, font_size, line_height: 1.2, hyphenation };
        let fonts = fonts(serif);
        check_chapter(&chapter, &fonts, config, &picks)?;
    }
}
