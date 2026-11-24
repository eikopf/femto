//! What a reader does with a recorded page index that no longer matches the
//! layout beyond the page it opens on. docs/navigation.md: "A record whose
//! target page does not reproduce is discarded and the chapter is indexed
//! afresh; other pages of a stale record are not revalidated."
use femto_reader::{
    controller::{Action, Reader},
    document::Document,
    layout::{Font, Fonts},
    page_index::IndexStore,
    state::ReaderState,
};

fn fonts() -> Fonts {
    Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    )
}

fn open(document: Document, store: Option<IndexStore>) -> Reader {
    let other = Document::from_text("other.txt", "Another book.").unwrap();
    let mut reader = Reader::new(other, fonts(), ReaderState::default()).unwrap();
    reader.set_index_store(store);
    let mut opening = reader.begin_open(document).unwrap();
    while !reader.step_open(&mut opening).unwrap() {}
    reader.finish_open(opening).unwrap();
    reader
}

/// Records a chapter, then moves the boundary between its pages `page` and
/// `page + 1` by `shift` bytes, as a build with different line breaking and
/// the same salt would have recorded it.
fn record_with_moved_boundary(directory: &std::path::Path, text: &str, page: usize, shift: u32) {
    let mut reader = open(
        Document::from_text("book.txt", text).unwrap(),
        Some(IndexStore::new(directory, "build")),
    );
    while reader.index_step().unwrap() {}
    assert!(reader.page_count() > page + 3);
    let file = std::fs::read_dir(directory)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut bytes = std::fs::read(&file).unwrap();
    // Twelve header bytes, eight record header bytes, sixteen bytes a page:
    // start block, start byte, end block, end byte.
    let word = |bytes: &[u8], at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
    let end_byte = 20 + 16 * page + 12;
    let next_start_byte = 20 + 16 * (page + 1) + 4;
    assert_eq!(word(&bytes, end_byte), word(&bytes, next_start_byte));
    let moved = (word(&bytes, end_byte) + shift).to_le_bytes();
    bytes[end_byte..end_byte + 4].copy_from_slice(&moved);
    bytes[next_start_byte..next_start_byte + 4].copy_from_slice(&moved);
    std::fs::write(&file, bytes).unwrap();
}

#[test]
fn a_stale_record_beyond_the_opening_page_does_not_block_reading() {
    let text = "A build that breaks one line differently must not strand the reader. ".repeat(600);
    let document = || Document::from_text("book.txt", &text).unwrap();
    let directory = tempfile::tempdir().unwrap();
    record_with_moved_boundary(directory.path(), &text, 2, 1);

    let mut fresh = open(document(), None);
    for _ in 0..5 {
        fresh.dispatch(Action::Next).unwrap();
    }
    // Two sessions, to show that the first failure repairs nothing: how far
    // five page turns get, and the error that stops them.
    let sessions: Vec<(usize, Option<String>)> = (0..2)
        .map(|_| {
            // The opening page reproduces, so the record is accepted.
            let mut reader = open(document(), Some(IndexStore::new(directory.path(), "build")));
            assert_eq!(reader.page_index(), 0);
            let error = (0..5).find_map(|_| reader.dispatch(Action::Next).err());
            if error.is_none() {
                assert_eq!(reader.render(2).unwrap(), fresh.render(2).unwrap());
            }
            (reader.page_index(), error.map(|error| error.to_string()))
        })
        .collect();
    assert_eq!(sessions, [(5, None), (5, None)]);
}
