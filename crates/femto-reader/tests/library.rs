use femto_reader::state::Library;

#[test]
fn canonical_title_order_and_path_ties_survive_cache_and_directory_reordering() {
    let dir = tempfile::tempdir().unwrap();
    let names = ["éz.txt", "b/e\u{301}a.txt", "z.txt", "a/Éa.txt"];
    let write = |name| {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, path.file_name().unwrap().to_str().unwrap()).unwrap();
    };
    for name in names {
        write(name);
    }
    let expected = ["z.txt", "a/Éa.txt", "b/e\u{301}a.txt", "éz.txt"].map(std::path::Path::new);
    let check = |library: &Library| {
        let actual: Vec<_> = library
            .entries
            .iter()
            .map(|entry| {
                assert_eq!(
                    entry.metadata.title,
                    entry.path.file_stem().unwrap().to_str().unwrap()
                );
                assert_eq!(
                    std::fs::read_to_string(&entry.path).unwrap(),
                    entry.path.file_name().unwrap().to_str().unwrap()
                );
                entry.path.strip_prefix(dir.path()).unwrap()
            })
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(library.search("éa").len(), 2);
    };
    check(&Library::scan(dir.path()).unwrap());
    let (cold, hits) = cached(dir.path(), 32);
    assert_eq!(hits, 0);
    check(&cold);
    for name in names {
        std::fs::remove_file(dir.path().join(name)).unwrap();
    }
    for name in names.into_iter().rev() {
        write(name);
    }
    let (warm, hits) = cached(dir.path(), 32);
    assert_eq!(hits, 4);
    check(&warm);
}

#[cfg(unix)]
#[test]
fn unreadable_subdirectory_retains_siblings_and_preserves_cache_until_recovery() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let blocked = dir.path().join("blocked");
    std::fs::create_dir(&blocked).unwrap();
    std::fs::write(blocked.join("hidden.txt"), "Hidden").unwrap();
    std::fs::write(dir.path().join("visible.txt"), "Visible").unwrap();
    assert_eq!(cached(dir.path(), 32).0.entries.len(), 2);
    let cache = dir.path().join(".femto-library-cache.json");
    let before = std::fs::read(&cache).unwrap();
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o0)).unwrap();
    if std::fs::read_dir(&blocked).is_ok() {
        std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700)).unwrap();
        eprintln!("permission test unavailable: process bypasses directory permissions");
        return;
    }
    let plain = Library::scan(dir.path());
    let indexed = Library::scan_cached(dir.path(), 32);
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700)).unwrap();
    for library in [plain.unwrap(), indexed.unwrap()] {
        assert_eq!(library.entries.len(), 1);
        assert_eq!(library.entries[0].metadata.title, "visible");
        assert!(!library.truncated);
        assert_eq!(library.errors.len(), 1);
        assert!(library.errors[0].contains("blocked"));
    }
    assert_eq!(std::fs::read(&cache).unwrap(), before);
    let (recovered, hits) = cached(dir.path(), 32);
    assert_eq!(recovered.entries.len(), 2);
    assert!(recovered.errors.is_empty());
    assert_eq!(hits, 2);
    assert_eq!(std::fs::read(&cache).unwrap(), before);
}

#[test]
fn depth_limit_keeps_accessible_books_without_replacing_complete_cache() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("known.txt"), "Known").unwrap();
    cached(dir.path(), 32);
    let cache = dir.path().join(".femto-library-cache.json");
    let before = std::fs::read(&cache).unwrap();
    let mut nested = dir.path().to_path_buf();
    for _ in 0..18 {
        nested.push("deep");
    }
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("hidden.txt"), "Hidden").unwrap();
    std::fs::write(dir.path().join("new.txt"), "New").unwrap();
    let (partial, hits) = cached(dir.path(), 32);
    assert_eq!(partial.entries.len(), 2);
    assert_eq!(hits, 1);
    assert_eq!(partial.errors.len(), 1);
    assert!(partial.errors[0].contains("directory depth exceeded"));
    assert_eq!(std::fs::read(&cache).unwrap(), before);
    std::fs::remove_dir_all(dir.path().join("deep")).unwrap();
    let (recovered, hits) = cached(dir.path(), 32);
    assert_eq!(recovered.entries.len(), 2);
    assert!(recovered.errors.is_empty());
    assert_eq!(hits, 1);
    assert_ne!(std::fs::read(&cache).unwrap(), before);
    assert_eq!(cached(dir.path(), 32).1, 2);
}

#[test]
fn limited_scan_retains_available_books_and_reports_truncation() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["Alpha", "Beta", "Gamma"] {
        std::fs::write(dir.path().join(format!("{name}.txt")), name).unwrap();
    }
    let partial = Library::scan_with_limit(dir.path(), 2).unwrap();
    assert_eq!(partial.entries.len(), 2);
    assert!(partial.truncated);
    assert!(
        partial
            .entries
            .windows(2)
            .all(|pair| pair[0].metadata.title <= pair[1].metadata.title)
    );
    let all = Library::scan(dir.path()).unwrap();
    assert_eq!(all.entries.len(), 3);
    assert!(!all.truncated);
    assert_eq!(all.search("beta").len(), 1);
    assert!(Library::scan_with_limit(dir.path(), 0).is_err());
    assert!(Library::scan_with_limit(dir.path(), 4097).is_err());
}

#[test]
fn corrupt_book_diagnostics_are_bounded_and_valid_books_remain_available() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..80 {
        std::fs::write(dir.path().join(format!("bad-{index}.epub")), b"not a ZIP").unwrap();
    }
    std::fs::write(dir.path().join("valid.txt"), b"Still readable").unwrap();
    let library = Library::scan(dir.path()).unwrap();
    assert_eq!(library.entries.len(), 1);
    assert_eq!(library.errors.len(), 64);
    assert!(
        library
            .errors
            .iter()
            .all(|error| error.chars().count() <= 512)
    );
    assert_eq!(library.entries[0].metadata.title, "valid");
}

#[test]
fn cooperative_scan_has_bounded_steps_and_releases_cancelled_traversal() {
    use femto_reader::state::LibraryScan;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    std::fs::write(dir.path().join("nested/book.txt"), "Nested book").unwrap();
    let mut cancelled = LibraryScan::new(dir.path(), 2).unwrap();
    assert!(cancelled.step().unwrap().is_none());
    drop(cancelled);
    let mut scan = LibraryScan::new(dir.path(), 1).unwrap();
    let result = (0..8).find_map(|_| scan.step().unwrap()).unwrap();
    assert_eq!(result.entries.len(), 1);
    assert!(!result.truncated);
    assert!(scan.step().is_err());
}

fn cached(root: &std::path::Path, limit: usize) -> (Library, usize) {
    let mut scan = femto_reader::state::LibraryScan::cached(root, limit).unwrap();
    for _ in 0..1000 {
        if let Some(library) = scan.step().unwrap() {
            return (library, scan.cache_hits());
        }
    }
    panic!("scan did not complete");
}

#[test]
fn persistent_cache_reuses_metadata_but_hashes_same_size_same_timestamp_edits() {
    let dir = tempfile::tempdir().unwrap();
    let book = dir.path().join("book.txt");
    std::fs::write(&book, b"first").unwrap();
    let (cold, hits) = cached(dir.path(), 32);
    assert_eq!(hits, 0);
    assert!(cold.errors.is_empty());
    let (warm, hits) = cached(dir.path(), 32);
    assert_eq!(hits, 1);
    assert_eq!(warm.entries[0].metadata.id, cold.entries[0].metadata.id);
    let modified = std::fs::metadata(&book).unwrap().modified().unwrap();
    std::fs::write(&book, b"other").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&book)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    let (changed, hits) = cached(dir.path(), 32);
    assert_eq!(hits, 0);
    assert_ne!(changed.entries[0].metadata.id, cold.entries[0].metadata.id);
    std::fs::remove_file(&book).unwrap();
    std::fs::write(dir.path().join("new.txt"), b"new").unwrap();
    let (updated, hits) = cached(dir.path(), 32);
    assert_eq!(hits, 0);
    assert_eq!(updated.entries.len(), 1);
    assert_eq!(updated.entries[0].metadata.title, "new");
}

#[test]
fn cancelled_and_truncated_scans_preserve_the_previous_cache() {
    use femto_reader::state::LibraryScan;
    let dir = tempfile::tempdir().unwrap();
    let book = dir.path().join("book.txt");
    std::fs::write(&book, vec![b'a'; 64 * 1024]).unwrap();
    let mut scan = LibraryScan::cached(dir.path(), 32).unwrap();
    assert!(scan.step().unwrap().is_none());
    assert!(scan.step().unwrap().is_none());
    drop(scan);
    let cache = dir.path().join(".femto-library-cache.json");
    assert!(!cache.exists());
    cached(dir.path(), 32);
    let before = std::fs::read(&cache).unwrap();
    std::fs::write(dir.path().join("second.txt"), b"second").unwrap();
    let mut cancelled = LibraryScan::cached(dir.path(), 32).unwrap();
    for _ in 0..3 {
        assert!(cancelled.step().unwrap().is_none());
    }
    drop(cancelled);
    assert_eq!(std::fs::read(&cache).unwrap(), before);
    let (partial, _) = cached(dir.path(), 1);
    assert!(partial.truncated);
    assert_eq!(std::fs::read(&cache).unwrap(), before);
}

#[test]
fn corrupt_or_unwritable_cache_does_not_hide_books() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("book.txt"), b"readable").unwrap();
    let cache = dir.path().join(".femto-library-cache.json");
    for bytes in [
        b"broken".as_slice(),
        b"{\"version\":999,\"root\":\"/wrong\",\"entries\":[]}",
        &vec![b'x'; 6000],
    ] {
        std::fs::write(&cache, bytes).unwrap();
        let (library, hits) = cached(dir.path(), 1);
        assert_eq!(hits, 0);
        assert_eq!(library.entries.len(), 1);
        assert!(
            library
                .errors
                .iter()
                .any(|error| error.contains("cache ignored"))
        );
    }
    std::fs::remove_file(&cache).unwrap();
    std::fs::create_dir(&cache).unwrap();
    let (library, _) = cached(dir.path(), 32);
    assert_eq!(library.entries.len(), 1);
    assert!(
        library
            .errors
            .iter()
            .any(|error| error.contains("cache save failed"))
    );
}

#[test]
fn epub_cache_survives_reopen_and_does_not_consume_the_book_limit() {
    let dir = tempfile::tempdir().unwrap();
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sample.epub");
    std::fs::copy(fixture, dir.path().join("sample.epub")).unwrap();
    let (cold, _) = cached(dir.path(), 1);
    assert!(!cold.truncated);
    let (warm, hits) = cached(dir.path(), 1);
    assert_eq!(hits, 1);
    assert!(!warm.truncated);
    assert!(warm.errors.is_empty());
    assert_eq!(
        warm.entries[0].metadata.title,
        cold.entries[0].metadata.title
    );
    assert_eq!(
        warm.entries[0].metadata.author,
        cold.entries[0].metadata.author
    );
    assert_eq!(
        warm.entries[0].metadata.language,
        cold.entries[0].metadata.language
    );
}

#[test]
fn cancelling_keeps_verified_entries_and_a_separate_cached_resume_hint() {
    use femto_reader::{
        document::Document,
        state::{BookState, LibraryScan, ReaderState},
    };
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("arbitrary-name.txt");
    let second = dir.path().join("other.txt");
    std::fs::write(&first, "first contents").unwrap();
    std::fs::write(&second, "second contents").unwrap();
    let (library, _) = cached(dir.path(), 32);
    let mut state = ReaderState::default();
    for entry in &library.entries {
        state.books.insert(
            entry.metadata.id.clone(),
            BookState {
                last_opened: if entry.path == first { 20 } else { 10 },
                ..BookState::default()
            },
        );
    }
    let scan = LibraryScan::cached(dir.path(), 32).unwrap();
    assert_eq!(scan.resume_hint(&state), Some(first.clone()));
    let partial = scan.cancel();
    assert!(partial.entries.is_empty());
    assert!(partial.truncated);
    let cache = dir.path().join(".femto-library-cache.json");
    let before = std::fs::read(&cache).unwrap();
    let old_id = Document::open(&first).unwrap().metadata.id;
    std::fs::write(&first, "changed content").unwrap();
    let scan = LibraryScan::cached(dir.path(), 32).unwrap();
    let hint = scan.resume_hint(&state).unwrap();
    assert_ne!(Document::open(hint).unwrap().metadata.id, old_id);
    scan.cancel();
    assert_eq!(std::fs::read(&cache).unwrap(), before);
    std::fs::remove_file(first).unwrap();
    let scan = LibraryScan::cached(dir.path(), 32).unwrap();
    assert_eq!(scan.resume_hint(&state), Some(second));
}

#[cfg(unix)]
#[test]
fn cached_resume_hint_cannot_follow_a_replaced_symlink_outside_the_library() {
    use femto_reader::state::{BookState, LibraryScan, ReaderState};
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let book = dir.path().join("book.txt");
    std::fs::write(&book, "book").unwrap();
    let (library, _) = cached(dir.path(), 32);
    let mut state = ReaderState::default();
    state
        .books
        .insert(library.entries[0].metadata.id.clone(), BookState::default());
    std::fs::remove_file(&book).unwrap();
    let target = outside.path().join("outside.txt");
    std::fs::write(&target, "outside").unwrap();
    std::os::unix::fs::symlink(target, book).unwrap();
    let scan = LibraryScan::cached(dir.path(), 32).unwrap();
    assert!(scan.resume_hint(&state).is_none());
}
