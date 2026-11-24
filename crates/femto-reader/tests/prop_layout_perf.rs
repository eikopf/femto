//! Discovery: layout time on adversarial paragraphs.
use femto_reader::{
    document::Document,
    layout::{Font, Fonts, LayoutConfig, Locator, paginate_from},
};
use std::time::Instant;

fn fonts() -> Fonts {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    Fonts::new(Font::load(root.join("DejaVuSans.ttf")).unwrap())
}

fn time(text: &str) -> (usize, f64) {
    let mut document = Document::from_text("t", text).unwrap();
    let chapter = document.chapter(0).unwrap();
    let fonts = fonts();
    let started = Instant::now();
    let mut pages = 0;
    let result = paginate_from(
        &chapter,
        &fonts,
        LayoutConfig::default(),
        Locator::default(),
        |_| {
            pages += 1;
            Ok(true)
        },
    );
    let elapsed = started.elapsed().as_secs_f64();
    if let Err(error) = result {
        eprintln!("error after {pages} pages: {error}");
    }
    (pages, elapsed)
}

/// Soft hyphens are break opportunities like any other, so a run of them
/// should cost about what the same run costs with zero-width spaces.
#[test]
fn soft_hyphen_runs_lay_out_in_time_comparable_to_other_break_opportunities() {
    let (_, control) = time(&"ab\u{200b}".repeat(600));
    let (pages, soft) = time(&"ab\u{ad}".repeat(600));
    assert!(pages > 0);
    assert!(
        soft < 20.0 * control + 0.05,
        "{soft:.3} s with soft hyphens, {control:.3} s with zero-width spaces"
    );
}

/// Layout of one unbreakable word should grow linearly with its length:
/// four times the text in about four times the time.
#[test]
fn unbreakable_words_lay_out_in_linear_time() {
    let (_, small) = time(&"a".repeat(128 * 1024));
    let (_, large) = time(&"a".repeat(512 * 1024));
    assert!(
        large < 6.0 * small,
        "128 KiB took {small:.3} s and 512 KiB {large:.3} s"
    );
}

/// A grapheme is never split, so one longer than the line buffer cannot be
/// laid out; the whole chapter then fails to open.
#[test]
#[ignore = "ambiguous: one grapheme of more than 4096 bytes makes its chapter unreadable"]
fn a_grapheme_longer_than_the_line_buffer_is_still_paginated() {
    let (pages, _) = time(&format!("before a{} after", "\u{301}".repeat(2100)));
    assert!(pages > 0);
}

#[test]
#[ignore = "measurement"]
fn measure() {
    let sizes = std::env::var("PERF_KIB").unwrap_or("4,8,16".into());
    for size in sizes.split(',').map(|size| size.parse::<usize>().unwrap()) {
        let n = size * 1024;
        let prose = "lorem ipsum dolor sit amet ".repeat(n / 27);
        let word = "a".repeat(n);
        let shy = "ab\u{ad}".repeat(n / 4);
        let nbsp = "ab\u{a0}".repeat(n / 4);
        let marks = format!("a{}", "\u{301}".repeat(n / 2));
        for (name, text) in [
            ("prose", &prose),
            ("word", &word),
            ("shy", &shy),
            ("nbsp", &nbsp),
            ("marks", &marks),
        ] {
            if std::env::var("PERF_ONLY").is_ok_and(|only| !only.contains(name)) {
                continue;
            }
            let (pages, seconds) = time(text);
            eprintln!("{size:4} KiB {name:6} {pages:6} pages {seconds:8.3} s");
        }
    }
}
