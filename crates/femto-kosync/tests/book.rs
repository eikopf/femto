#![cfg(feature = "reader")]
use femto_kosync::{
    book::Book,
    config::{Checksum, Settings},
};
use femto_reader::{document::Document, layout::Locator, state::ReaderState};
use std::{
    fs,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}
#[test]
fn book_identity_and_parser_share_the_opened_handle_after_path_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("book.epub");
    fs::copy(fixture("sample.epub"), &path).unwrap();
    let file = fs::File::open(&path).unwrap();
    fs::rename(&path, directory.path().join("old.epub")).unwrap();
    fs::copy(fixture("navigation.epub"), &path).unwrap();
    let mut book = Book::from_file(
        &path,
        file,
        Checksum::Binary,
        femto_catalog::deadline::Limit::Until(Instant::now() + Duration::from_secs(5)),
        &AtomicBool::new(false),
    )
    .unwrap();
    let original = Document::open(fixture("sample.epub")).unwrap();
    assert_eq!(book.document.metadata.id, original.metadata.id);
    assert_ne!(
        book.document.metadata.id,
        Document::open(&path).unwrap().metadata.id
    );
    let id =
        femto_kosync::DocumentId::sampled(&mut fs::File::open(fixture("sample.epub")).unwrap())
            .unwrap();
    assert_eq!(book.id, id);
    let settings =
        Settings::from_json(br#"{"username":"test","password":"test","device_id":"fixture"}"#)
            .unwrap();
    let mut state = ReaderState::default();
    assert!(book.prepare_push(&state, &settings).is_err());
    state.set_position(
        &original.metadata.id,
        Locator {
            chapter: 0,
            block: 0,
            byte: 3,
        },
        1,
    );
    let progress = book.prepare_push(&state, &settings).unwrap();
    assert_eq!(
        progress.progress,
        "/body[1]/DocFragment[1]/body[1]/h1[1]/text()[1].3"
    );
    assert_eq!(progress.document, id);
}
#[test]
fn cancelled_book_preparation_does_not_parse_or_hash() {
    let path = fixture("sample.epub");
    assert!(
        Book::from_file(
            &path,
            fs::File::open(&path).unwrap(),
            Checksum::Binary,
            femto_catalog::deadline::Limit::Until(Instant::now() + Duration::from_secs(5)),
            &AtomicBool::new(true)
        )
        .is_err()
    );
}
