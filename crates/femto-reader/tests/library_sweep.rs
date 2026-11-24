//! Opens every book of a directory and lays out every chapter, to find what
//! books in circulation need that the fixtures do not. It needs a library,
//! so it is not part of the routine checks:
//!
//! ```text
//! FEMTO_SWEEP_DIR=/path/to/books cargo test --release -p femto-reader \
//!     --test library_sweep -- --ignored --nocapture
//! ```
use femto_reader::{
    document::{Chapter, Document},
    image,
    layout::{Font, Fonts, LayoutConfig, Locator, paginate_from},
};
use std::{panic, path::Path, time::Instant};

/// Pages of a chapter laid out as the reader does it: one after another,
/// keeping none.
fn pages(chapter: &Chapter, fonts: &Fonts) -> femto_reader::Result<usize> {
    let mut pages = 0;
    let start = Locator {
        chapter: chapter.index,
        ..Default::default()
    };
    paginate_from(chapter, fonts, LayoutConfig::default(), start, |_| {
        pages += 1;
        Ok(true)
    })?;
    Ok(pages)
}

fn sweep(path: &Path, fonts: &Fonts) -> Result<String, String> {
    let mut book = Document::open(path).map_err(|e| format!("open: {e}"))?;
    let cover = match book.cover() {
        None => "no cover".to_string(),
        Some(bytes) => match image::decode_within(&bytes, image::Limits::ALONE) {
            Ok(cover) => format!("cover {}x{}", cover.size.width, cover.size.height),
            Err(error) => format!("cover refused ({error})"),
        },
    };
    let chapters = book.metadata.chapters.len();
    let (mut total, mut warnings, mut slowest) = (0, 0, 0);
    for index in 0..chapters {
        let started = Instant::now();
        let chapter = book
            .chapter(index)
            .map_err(|e| format!("chapter {index}: {e}"))?;
        warnings += chapter.warnings.len();
        total += pages(&chapter, fonts).map_err(|e| format!("layout {index}: {e}"))?;
        slowest = slowest.max(started.elapsed().as_millis());
    }
    Ok(format!(
        "{chapters} chapters, {total} pages, {warnings} warnings, slowest chapter {slowest} ms, {cover}"
    ))
}

#[test]
#[ignore = "needs a library; set FEMTO_SWEEP_DIR"]
fn every_book_opens_and_every_chapter_lays_out() {
    let directory = std::env::var("FEMTO_SWEEP_DIR").expect("FEMTO_SWEEP_DIR names a directory");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let fonts = Fonts::new(Font::load(fixtures.join("DejaVuSerif.ttf")).unwrap());
    let mut books: Vec<_> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "epub" || e == "txt"))
        .collect();
    books.sort();
    panic::set_hook(Box::new(|_| {}));
    let mut failed = 0;
    for path in &books {
        let name = path.file_name().unwrap().to_string_lossy();
        let outcome = panic::catch_unwind(panic::AssertUnwindSafe(|| sweep(path, &fonts)));
        match outcome {
            Ok(Ok(summary)) => println!("ok   {name}: {summary}"),
            Ok(Err(error)) => {
                failed += 1;
                println!("FAIL {name}: {error}");
            }
            Err(_) => {
                failed += 1;
                println!("FAIL {name}: panicked");
            }
        }
    }
    let _ = panic::take_hook();
    assert_eq!(failed, 0, "{failed} of {} books failed", books.len());
}
