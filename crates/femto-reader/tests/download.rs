use femto_reader::download::{MAX_BYTES, Staged};
use std::{
    fs,
    io::{self, Write},
};
const BOOK: &[u8] = include_bytes!("../../../tests/fixtures/sample.epub");
#[test]
fn validated_publication_never_replaces_existing_book() {
    let root = tempfile::tempdir().unwrap();
    let mut first = Staged::new(root.path()).unwrap();
    first.write_all(BOOK).unwrap();
    let ready = first.validate().unwrap();
    assert_eq!(ready.bytes, BOOK.len() as u64);
    let path = ready.publish(|src, dst| fs::hard_link(src, dst)).unwrap();
    assert_eq!(fs::read(&path).unwrap(), BOOK);
    assert!(
        path.file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("book-")
    );
    let mut second = Staged::new(root.path()).unwrap();
    second.write_all(BOOK).unwrap();
    let second = second.validate().unwrap();
    fs::write(&path, b"preserve existing file").unwrap();
    assert!(second.publish(|src, dst| fs::hard_link(src, dst)).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"preserve existing file");
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
}
#[test]
fn errors_and_abandoned_results_remove_only_owned_staging_files() {
    let root = tempfile::tempdir().unwrap();
    let stale = root.path().join(".femto-download-stale.part");
    fs::write(&stale, b"preserve").unwrap();
    let mut invalid = Staged::new(root.path()).unwrap();
    invalid.write_all(b"not an epub").unwrap();
    assert!(invalid.validate().is_err());
    let mut partial = Staged::new(root.path()).unwrap();
    partial.write_all(b"partial").unwrap();
    drop(partial);
    let mut abandoned = Staged::new(root.path()).unwrap();
    abandoned.write_all(BOOK).unwrap();
    drop(abandoned.validate().unwrap());
    let mut failed = Staged::new(root.path()).unwrap();
    failed.write_all(BOOK).unwrap();
    assert!(
        failed
            .validate()
            .unwrap()
            .publish(|_, _| Err(io::Error::other("injected publication failure")))
            .is_err()
    );
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    assert_eq!(fs::read(stale).unwrap(), b"preserve");
}
#[test]
fn oversized_single_write_is_rejected_before_storage_changes() {
    let root = tempfile::tempdir().unwrap();
    let mut staged = Staged::new(root.path()).unwrap();
    assert!(staged.write(&vec![0; MAX_BYTES as usize + 1]).is_err());
    let file = fs::read_dir(root.path()).unwrap().next().unwrap().unwrap();
    assert_eq!(file.metadata().unwrap().len(), 0);
    drop(staged);
    assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
}
