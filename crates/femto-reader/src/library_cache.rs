//! Disposable metadata index; content hashes are verified during each scan.
use crate::{
    Error, Result,
    state::{LibraryEntry, LibraryMetadata, atomic_write},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
struct Index {
    version: u32,
    root: PathBuf,
    entries: Vec<LibraryEntry>,
}
#[derive(Serialize)]
struct SavedIndex<'a> {
    version: u32,
    root: &'a Path,
    entries: &'a [LibraryEntry],
}
struct BoundedBytes {
    bytes: Vec<u8>,
    max: usize,
}
impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.max.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("library cache bytes"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) struct Cache {
    path: PathBuf,
    root: PathBuf,
    entries: BTreeMap<PathBuf, LibraryMetadata>,
    limit: usize,
    loaded: bool,
}
impl Cache {
    pub(crate) fn open(root: &Path, limit: usize) -> (Self, Option<String>) {
        let mut cache = Self {
            path: root.join(".femto-library-cache.json"),
            root: root.to_owned(),
            entries: BTreeMap::new(),
            limit,
            loaded: false,
        };
        let result = cache.load();
        (
            cache,
            result.err().map(|e| format!("library cache ignored: {e}")),
        )
    }
    fn load(&mut self) -> Result<()> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        let max = (self.limit * 4096 + 1024).min(4 * 1024 * 1024);
        let mut bytes = Vec::new();
        file.take(max as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > max {
            return Err(Error::Limit("library cache bytes"));
        }
        let index: Index = serde_json::from_slice(&bytes)?;
        if index.version != 1 || index.root != self.root || index.entries.len() > self.limit {
            return Err(Error::Document(
                "library cache version, root, or count".into(),
            ));
        }
        let mut entries = BTreeMap::new();
        for entry in index.entries {
            if !valid(&entry)
                || !entry.path.starts_with(&self.root)
                || entries.insert(entry.path, entry.metadata).is_some()
            {
                return Err(Error::Document("library cache entry".into()));
            }
        }
        self.entries = entries;
        self.loaded = true;
        Ok(())
    }
    pub(crate) fn get(&self, path: &Path, digest: &str) -> Option<LibraryMetadata> {
        self.entries
            .get(path)
            .filter(|meta| meta.id == digest)
            .cloned()
    }
    pub(crate) fn resume_hint(&self, state: &crate::state::ReaderState) -> Option<PathBuf> {
        let mut candidates: Vec<_> = self
            .entries
            .iter()
            .filter_map(|(path, metadata)| Some((state.books.get(&metadata.id)?.last_opened, path)))
            .collect();
        candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        candidates.into_iter().find_map(|(_, path)| {
            let path = path.canonicalize().ok()?;
            if !path.starts_with(&self.root) || !path.is_file() {
                return None;
            }
            Some(path)
        })
    }
    pub(crate) fn save(&self, entries: &[LibraryEntry]) -> Result<()> {
        if self.loaded
            && entries.len() == self.entries.len()
            && entries
                .iter()
                .all(|entry| self.entries.get(&entry.path) == Some(&entry.metadata))
        {
            return Ok(());
        }
        if entries.len() > self.limit || entries.iter().any(|entry| !valid(entry)) {
            return Err(Error::Limit("library cache entries"));
        }
        let mut output = BoundedBytes {
            bytes: Vec::new(),
            max: (self.limit * 4096 + 1024).min(4 * 1024 * 1024),
        };
        serde_json::to_writer(
            &mut output,
            &SavedIndex {
                version: 1,
                root: &self.root,
                entries,
            },
        )?;
        atomic_write(&self.path, &output.bytes)
    }
}
fn valid(entry: &LibraryEntry) -> bool {
    entry.path.to_str().is_some_and(|path| path.len() <= 1024)
        && entry.metadata.id.len() == 64
        && entry.metadata.id.bytes().all(|b| b.is_ascii_hexdigit())
        && entry.metadata.title.chars().count() <= 256
        && entry.metadata.author.chars().count() <= 256
        && entry.metadata.language.chars().count() <= 32
}
