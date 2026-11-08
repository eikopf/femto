use femto_reader::{clipping::Clipping, document::Document, layout::Locator, state::ReaderState};
use std::{
    path::Path,
    process::{Command, Output},
};

fn run(book: &Path, state: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_femto"))
        .arg("clippings-export")
        .arg(book)
        .arg("--state")
        .arg(state)
        .output()
        .unwrap()
}

fn run_journal(book: &Path, journal: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_femto"))
        .arg("clippings-export")
        .arg(book)
        .arg("--journal")
        .arg(journal)
        .output()
        .unwrap()
}

#[test]
fn journal_export_selects_newest_valid_slot_without_modifying_the_copy() {
    let root = tempfile::tempdir().unwrap();
    let book_path = root.path().join("book.txt");
    std::fs::write(&book_path, "First paragraph.").unwrap();
    let mut book = Document::open(&book_path).unwrap();
    let mut clipping = Clipping::capture(
        &mut book,
        Locator::default()..Locator {
            chapter: 0,
            block: 0,
            byte: 5,
        },
        42,
    )
    .unwrap();
    clipping.highlighted = true;
    let directory = root.path().join("journal");
    let (mut journal, _) = femto_reader::journal::StateJournal::open(&directory).unwrap();
    let mut state = ReaderState::default();
    journal.save(&state).unwrap();
    state.add_clipping(clipping.clone()).unwrap();
    journal.save(&state).unwrap();
    drop(journal);
    let slot0 = directory.join("state0.bin");
    let slot1 = directory.join("state1.bin");
    let older = std::fs::read(&slot0).unwrap();
    let newer = std::fs::read(&slot1).unwrap();
    let result = run_journal(&book_path, &directory);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        value["clippings"],
        serde_json::to_value([clipping]).unwrap()
    );
    assert_eq!(std::fs::read(&slot0).unwrap(), older);
    assert_eq!(std::fs::read(&slot1).unwrap(), newer);
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 2);

    std::fs::write(&slot1, &newer[..newer.len() / 2]).unwrap();
    let torn = std::fs::read(&slot1).unwrap();
    let result = run_journal(&book_path, &directory);
    assert!(result.status.success());
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["clippings"], serde_json::json!([]));
    assert_eq!(std::fs::read(&slot0).unwrap(), older);
    assert_eq!(std::fs::read(&slot1).unwrap(), torn);

    std::fs::write(&slot0, b"corrupt").unwrap();
    let result = run_journal(&book_path, &directory);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
}

#[test]
fn journal_export_rejects_missing_slots_and_ambiguous_source_arguments() {
    let root = tempfile::tempdir().unwrap();
    let book = root.path().join("book.txt");
    std::fs::write(&book, "Book").unwrap();
    for directory in [root.path().to_path_buf(), root.path().join("missing")] {
        let result = run_journal(&book, &directory);
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
    }
    assert!(!root.path().join("missing").exists());
    for args in [
        vec![],
        vec!["--state", "state.json", "--journal", "journal"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_femto"))
            .arg("clippings-export")
            .arg(&book)
            .args(args)
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(result.stdout.is_empty());
    }
}

#[test]
fn export_verifies_all_quotes_preserves_unicode_and_never_changes_state() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("book.txt");
    std::fs::write(&path, "Ae\u{301} 👩‍💻 שלום\n\nSecond paragraph.").unwrap();
    let mut book = Document::open(&path).unwrap();
    let at = |block, byte| Locator {
        chapter: 0,
        block,
        byte,
    };
    let first = Clipping::capture(&mut book, at(0, 1)..at(1, 6), 42).unwrap();
    let second = Clipping::capture(&mut book, at(1, 0)..at(1, 6), 43).unwrap();
    let mut state = ReaderState::default();
    state.add_clipping(first.clone()).unwrap();
    state.add_clipping(second.clone()).unwrap();
    let state_path = root.path().join("state.json");
    state.save(&state_path).unwrap();
    let before = std::fs::read(&state_path).unwrap();
    let result = run(&path, &state_path);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(json["version"], 1);
    assert_eq!(json["book_id"], book.metadata.id);
    assert_eq!(json["title"], book.metadata.title);
    assert_eq!(json["author"], book.metadata.author);
    assert_eq!(
        json["clippings"],
        serde_json::to_value([first, second]).unwrap()
    );
    assert_eq!(std::fs::read(&state_path).unwrap(), before);
    // A forged second quote must not leak the valid first record or header.
    state.books.get_mut(&book.metadata.id).unwrap().clippings[1]
        .text
        .push('!');
    state.save(&state_path).unwrap();
    let before = std::fs::read(&state_path).unwrap();
    let result = run(&path, &state_path);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert_eq!(std::fs::read(&state_path).unwrap(), before);
}

#[test]
fn empty_book_exports_an_empty_list_and_bad_input_emits_nothing() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("book.txt");
    let state = root.path().join("state.json");
    std::fs::write(&path, "No saved excerpts.").unwrap();
    let result = run(&path, &state);
    assert!(result.status.success());
    let json: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(json["clippings"], serde_json::json!([]));
    assert!(!state.exists());
    std::fs::write(&state, "invalid json").unwrap();
    let result = run(&path, &state);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    std::fs::remove_file(&path).unwrap();
    let result = run(&path, &state);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
}

#[cfg(unix)]
#[test]
fn export_refuses_an_in_use_state_file() {
    let root = tempfile::tempdir().unwrap();
    let book = root.path().join("book.txt");
    let state = root.path().join("state.json");
    std::fs::write(&book, "Book text.").unwrap();
    ReaderState::default().save(&state).unwrap();
    let lock = std::fs::File::create(root.path().join("state.json.femto-lock")).unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    let before = std::fs::read(&state).unwrap();
    let result = run(&book, &state);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(String::from_utf8_lossy(&result.stderr).contains("in use"));
    assert_eq!(std::fs::read(&state).unwrap(), before);
    drop(lock);
    assert!(run(&book, &state).status.success());
}
