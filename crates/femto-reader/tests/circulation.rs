//! Books as they are found: what the library sweep turned up, kept small.
use femto_reader::{
    document::{Block, Document},
    layout::Locator,
};

fn book() -> Document {
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    // The mimetype entry ends with a line break.
    Document::open(fixtures.join("circulation.epub")).unwrap()
}

#[test]
fn html_entities_resolve_in_chapter_text() {
    let chapter = book().chapter(0).unwrap();
    let Block::Text(first) = &chapter.blocks[0] else {
        panic!("text expected");
    };
    assert_eq!(
        first.text,
        "Na\u{ef}ve\u{a0}caf\u{e9} \u{201c}quoted\u{201d}."
    );
}

#[test]
fn sync_positions_map_in_a_chapter_that_starts_with_a_byte_order_mark() {
    let mut book = book();
    let chapter = book.chapter(0).unwrap();
    for (block, content) in chapter.blocks.iter().enumerate() {
        let Block::Text(paragraph) = content else {
            continue;
        };
        for (byte, _) in paragraph.text.char_indices() {
            let location = Locator {
                chapter: 0,
                block,
                byte,
            };
            let pointer = book.koreader_position(location).unwrap();
            assert_eq!(book.resolve_koreader_position(&pointer).unwrap(), location);
        }
    }
}

#[test]
fn a_position_with_no_place_in_the_source_is_given_as_one_before_it() {
    let mut book = book();
    // A chapter with nothing in it has only its start.
    assert!(book.chapter(1).unwrap().blocks.is_empty());
    let start = Locator {
        chapter: 1,
        ..Default::default()
    };
    let pointer = book.koreader_position(start).unwrap();
    assert_eq!(pointer, "/body/DocFragment[2]/body");
    assert_eq!(book.resolve_koreader_position(&pointer).unwrap(), start);
    // A byte inside a character is not a position, and is refused.
    assert!(
        book.koreader_position(Locator {
            chapter: 0,
            block: 1,
            byte: 1,
        })
        .is_err()
    );
}
