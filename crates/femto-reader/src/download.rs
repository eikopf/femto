//! Bounded staging and validation before platform-specific no-replacement publication.
use crate::{
    Error, Result,
    document::{Document, Metadata},
};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
static NEXT: AtomicU32 = AtomicU32::new(0);
struct Temporary {
    path: PathBuf,
    file: Option<File>,
}
impl Drop for Temporary {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
    }
}
pub struct Staged {
    temporary: Temporary,
    bytes: u64,
}
pub struct Ready {
    temporary: Temporary,
    destination: PathBuf,
    pub metadata: Metadata,
    pub bytes: u64,
}
impl Staged {
    pub fn new(directory: &Path) -> Result<Self> {
        for _ in 0..1024 {
            let id = loop {
                let old = NEXT.load(Ordering::Relaxed);
                let next = old
                    .checked_add(1)
                    .ok_or(Error::Limit("download staging IDs"))?;
                if NEXT
                    .compare_exchange_weak(old, next, Ordering::Relaxed, Ordering::Relaxed)
                    .is_ok()
                {
                    break old;
                }
            };
            let path = directory.join(format!(".femto-download-{id:08x}.part"));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        temporary: Temporary {
                            path,
                            file: Some(file),
                        },
                        bytes: 0,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(Error::Limit("download staging collisions"))
    }
    pub fn validate(mut self) -> Result<Ready> {
        let mut file = self.temporary.file.take().expect("staging file owned");
        file.flush()?;
        file.sync_all()?;
        drop(file);
        let mut document = Document::open_epub(&self.temporary.path)?;
        document.chapter(0)?;
        let metadata = document.metadata.clone();
        drop(document);
        if metadata.id.len() != 64 || !metadata.id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Document("invalid book content hash".into()));
        }
        let destination = self
            .temporary
            .path
            .parent()
            .expect("staging parent")
            .join(format!("book-{}.epub", metadata.id));
        Ok(Ready {
            temporary: self.temporary,
            destination,
            metadata,
            bytes: self.bytes,
        })
    }
}
impl Write for Staged {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > MAX_BYTES - self.bytes {
            return Err(io::Error::other("download size limit"));
        }
        let count = self
            .temporary
            .file
            .as_mut()
            .expect("staging file owned")
            .write(bytes)?;
        self.bytes += count as u64;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.temporary
            .file
            .as_mut()
            .expect("staging file owned")
            .flush()
    }
}
impl Ready {
    /// The callback must publish atomically without replacing an existing path.
    /// All validation handles are closed; the caller must keep storage mounted.
    pub fn publish(self, publish: impl FnOnce(&Path, &Path) -> io::Result<()>) -> Result<PathBuf> {
        publish(&self.temporary.path, &self.destination)?;
        Ok(self.destination.clone())
    }
}
