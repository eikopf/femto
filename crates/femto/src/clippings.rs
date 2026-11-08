use femto_reader::{clipping::Clipping, document::Document, state::ReaderState};
use serde::Serialize;
use std::{
    io::{self, Write},
    path::Path,
};

#[derive(Serialize)]
struct Export<'a> {
    version: u32,
    book_id: &'a str,
    title: &'a str,
    author: &'a str,
    clippings: &'a [Clipping],
}

pub fn export(book: &Path, path: &Path) -> anyhow::Result<()> {
    #[cfg(all(unix, not(target_os = "espidf")))]
    let state_file = crate::state_file::StateFile::acquire(path)?;
    #[cfg(all(unix, not(target_os = "espidf")))]
    let path = state_file.path();
    let state = ReaderState::load(path)?;
    output(book, &state)
}

pub fn export_journal(book: &Path, directory: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(
        directory.is_dir(),
        "journal source must be an offline directory"
    );
    anyhow::ensure!(
        directory.join("state0.bin").try_exists()? || directory.join("state1.bin").try_exists()?,
        "journal source has no state slots"
    );
    let (_, state) = femto_reader::journal::StateJournal::open(directory)?;
    output(book, &state)
}

fn output(book: &Path, state: &ReaderState) -> anyhow::Result<()> {
    let mut document = Document::open(book)?;
    let clippings = state
        .books
        .get(&document.metadata.id)
        .map_or(&[][..], |book| book.clippings.as_slice());
    // Validate all quotes before publishing even the export header.
    for clipping in clippings {
        clipping.validate_document(&mut document)?;
    }
    let export = Export {
        version: 1,
        book_id: &document.metadata.id,
        title: &document.metadata.title,
        author: &document.metadata.author,
        clippings,
    };
    let mut output = io::stdout().lock();
    serde_json::to_writer_pretty(&mut output, &export)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}
