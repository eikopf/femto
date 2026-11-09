use crate::{
    DocumentId, Error, Progress, RemoteProgress,
    config::{Checksum, Settings},
};
use femto_reader::{document::Document, layout::Locator, state::ReaderState};
use std::{
    fs::File,
    io,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

pub struct Book {
    pub document: Document,
    pub id: DocumentId,
}
impl Book {
    pub fn from_file(
        path: &Path,
        mut file: File,
        checksum: Checksum,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Self, Error> {
        boundary(deadline, cancelled)?;
        if !file.metadata()?.is_file() {
            return Err(Error::Invalid("book file"));
        }
        let id = match checksum {
            Checksum::Binary => DocumentId::sampled(&mut file)?,
            Checksum::Filename => DocumentId::filename(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .ok_or(Error::Invalid("UTF-8 book filename"))?,
            )?,
        };
        let document = Document::from_file(path, file, || boundary(deadline, cancelled))?;
        boundary(deadline, cancelled)?;
        Ok(Self { document, id })
    }
    pub fn prepare_push(
        &mut self,
        state: &ReaderState,
        settings: &Settings,
    ) -> Result<Progress, Error> {
        let location = state
            .books
            .get(&self.document.metadata.id)
            .map(|book| book.position)
            .ok_or(Error::Invalid("book has no saved reading position"))?;
        let progress = self.document.koreader_position(location)?;
        let percentage = self.document.source_progress(location)?;
        let result = Progress {
            document: self.id.clone(),
            progress,
            percentage,
            device: settings.device.clone(),
            device_id: settings.device_id.clone(),
        };
        result.validate()?;
        Ok(result)
    }
    pub fn resolve_remote(&mut self, remote: &RemoteProgress) -> Result<Locator, Error> {
        if remote.position.document != self.id {
            return Err(Error::Invalid("remote document identity"));
        }
        remote.position.validate()?;
        Ok(self
            .document
            .resolve_koreader_position(&remote.position.progress)?)
    }
}
pub fn boundary(deadline: Instant, cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "KOReader operation cancelled",
        ));
    }
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "KOReader operation deadline",
        ));
    }
    Ok(())
}
