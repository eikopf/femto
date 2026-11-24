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
