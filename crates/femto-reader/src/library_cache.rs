//! Disposable metadata index with cheap warm-open stamps and explicit hash verification.
use crate::{
    Error, Result,
    state::{LibraryEntry, LibraryMetadata, atomic_write},
};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeSeq};
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
    entries: Vec<CachedEntry>,
}
#[derive(Serialize, Deserialize)]
struct CachedEntry {
    #[serde(flatten)]
    entry: LibraryEntry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stamp: Option<FileStamp>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FileStamp {
    size: u64,
    modified_seconds: u64,
    modified_nanos: u32,
}
impl FileStamp {
    pub(crate) fn read(file: &File) -> Option<Self> {
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() {
            return None;
        }
        let modified = metadata
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        Some(Self {
            size: metadata.len(),
            modified_seconds: modified.as_secs(),
            modified_nanos: modified.subsec_nanos(),
        })
    }
}
#[derive(Serialize)]
struct SavedEntry<'a> {
    path: &'a Path,
    metadata: &'a LibraryMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    stamp: Option<&'a FileStamp>,
}
#[derive(Serialize)]
struct SavedIndex<'a> {
    version: u32,
    root: &'a Path,
    entries: SavedEntries<'a>,
}
enum EntrySource<'a> {
    Slice(&'a [LibraryEntry]),
    Map(&'a BTreeMap<PathBuf, LibraryMetadata>),
}
struct SavedEntries<'a> {
    source: EntrySource<'a>,
    stamps: &'a BTreeMap<PathBuf, FileStamp>,
}
impl Serialize for SavedEntries<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let len = match self.source {
            EntrySource::Slice(entries) => entries.len(),
            EntrySource::Map(entries) => entries.len(),
        };
        let mut seq = serializer.serialize_seq(Some(len))?;
        match self.source {
            EntrySource::Slice(entries) => {
                for entry in entries {
                    seq.serialize_element(&SavedEntry {
                        path: &entry.path,
                        metadata: &entry.metadata,
                        stamp: self.stamps.get(&entry.path),
                    })?;
                }
            }
            EntrySource::Map(entries) => {
                for (path, metadata) in entries {
                    seq.serialize_element(&SavedEntry {
                        path,
                        metadata,
                        stamp: self.stamps.get(path),
                    })?;
                }
            }
        }
        seq.end()
    }
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
pub(crate) struct OpenedBook {
    pub(crate) path: PathBuf,
    pub(crate) file: File,
    pub(crate) identity: Option<(String, FileStamp)>,
}

pub(crate) struct Cache {
    path: PathBuf,
    root: PathBuf,
    entries: BTreeMap<PathBuf, LibraryMetadata>,
    stamps: BTreeMap<PathBuf, FileStamp>,
    limit: usize,
    loaded: bool,
    changed: bool,
}
impl Cache {
    pub(crate) fn open(root: &Path, limit: usize) -> (Self, Option<String>) {
        let mut cache = Self {
            path: root.join(".femto-library-cache.json"),
            root: root.to_owned(),
            entries: BTreeMap::new(),
            stamps: BTreeMap::new(),
            limit,
            loaded: false,
            changed: false,
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
        if !matches!(index.version, 1 | 2)
            || index.root != self.root
            || index.entries.len() > self.limit
        {
            return Err(Error::Document(
                "library cache version, root, or count".into(),
            ));
        }
        let mut entries = BTreeMap::new();
        let mut stamps = BTreeMap::new();
        for cached in index.entries {
            let entry = cached.entry;
            if index.version == 2
                && let Some(stamp) = cached.stamp
            {
                stamps.insert(entry.path.clone(), stamp);
            }
            if !valid(&entry)
                || !safe_path(&self.root, &entry.path)
                || entries.insert(entry.path, entry.metadata).is_some()
            {
                return Err(Error::Document("library cache entry".into()));
            }
        }
        self.entries = entries;
        self.stamps = stamps;
        self.loaded = true;
        self.changed = index.version != 2;
        Ok(())
    }
    pub(crate) fn into_snapshot(self) -> crate::state::Library {
        let mut entries: Vec<_> = self
            .entries
            .into_iter()
            .map(|(path, metadata)| LibraryEntry { path, metadata })
            .collect();
        entries.sort_by_cached_key(|entry| crate::state::library_key(&entry.metadata.title));
        crate::state::Library {
            entries,
            ..Default::default()
        }
    }

    pub(crate) fn remember_stamp(&mut self, path: &Path, stamp: Option<FileStamp>) {
        if self.stamps.get(path).copied() != stamp {
            self.changed = true;
            self.stamps.remove(path);
            if let Some(stamp) = stamp {
                self.stamps.insert(path.to_owned(), stamp);
            }
        }
    }

    pub(crate) fn open_book(root: &Path, path: &Path, limit: usize) -> Result<OpenedBook> {
        let root = root.canonicalize()?;
        // Validate the actual target before opening; cache strings alone never
        // authorize access outside the selected library.
        let path = path.canonicalize()?;
        if !safe_path(&root, &path) {
            return Err(Error::Document("book is outside library root".into()));
        }
        if !std::fs::metadata(&path)?.is_file() {
            return Err(Error::Document("book is not a regular file".into()));
        }
        let file = File::open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(Error::Document("book is not a regular file".into()));
        }
        if !(1..=4096).contains(&limit) {
            return Err(Error::Limit("library limit"));
        }
        let (cache, _) = Self::open(&root, limit);
        let id = FileStamp::read(&file).and_then(|stamp| {
            (cache.stamps.get(&path) == Some(&stamp))
                .then(|| cache.entries.get(&path).map(|m| (m.id.clone(), stamp)))
                .flatten()
        });
        Ok(OpenedBook {
            path,
            file,
            identity: id,
        })
    }

    /// Deletes a book file inside the library and drops it from the cache.
    pub(crate) fn delete_book(root: &Path, path: &Path, limit: usize) -> Result<()> {
        if !(1..=4096).contains(&limit) {
            return Err(Error::Limit("library limit"));
        }
        let root = root.canonicalize()?;
        let path = path.canonicalize()?;
        if !safe_path(&root, &path) {
            return Err(Error::Document("book is outside library root".into()));
        }
        if !std::fs::metadata(&path)?.is_file() {
            return Err(Error::Document("book is not a regular file".into()));
        }
        std::fs::remove_file(&path)?;
        let (mut cache, _) = Self::open(&root, limit);
        if cache.entries.remove(&path).is_some() {
            cache.stamps.remove(&path);
            cache.write(EntrySource::Map(&cache.entries))?;
        }
        Ok(())
    }

    pub(crate) fn record_open(
        root: &Path,
        path: &Path,
        metadata: &crate::document::Metadata,
        stamp: Option<FileStamp>,
        limit: usize,
    ) -> Result<()> {
        if !(1..=4096).contains(&limit) {
            return Err(Error::Limit("library limit"));
        }
        let root = root.canonicalize()?;
        if !safe_path(&root, path) {
            return Err(Error::Document("book is outside library root".into()));
        }
        let (mut cache, _) = Self::open(&root, limit);
        if !cache.entries.contains_key(path) && cache.entries.len() == limit {
            // Eviction affects only metadata. Retain the selected book for resume.
            if let Some((evicted, _)) = cache.entries.pop_first() {
                cache.stamps.remove(&evicted);
            }
            cache.changed = true;
        }
        let metadata = LibraryMetadata {
            id: metadata.id.clone(),
            title: metadata.title.chars().take(256).collect(),
            author: metadata.author.chars().take(256).collect(),
            language: metadata.language.chars().take(32).collect(),
        };
        if cache.entries.get(path) != Some(&metadata) {
            cache.entries.insert(path.to_owned(), metadata);
            cache.changed = true;
        }
        cache.remember_stamp(path, stamp);
        if cache.loaded && !cache.changed {
            return Ok(());
        }
        cache.write(EntrySource::Map(&cache.entries))
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
            && !self.changed
            && entries.len() == self.entries.len()
            && entries
                .iter()
                .all(|entry| self.entries.get(&entry.path) == Some(&entry.metadata))
        {
            return Ok(());
        }
        if entries.len() > self.limit
            || entries
                .iter()
                .any(|entry| !valid(entry) || !safe_path(&self.root, &entry.path))
        {
            return Err(Error::Limit("library cache entries"));
        }
        self.write(EntrySource::Slice(entries))
    }
    fn write(&self, source: EntrySource<'_>) -> Result<()> {
        let mut output = BoundedBytes {
            bytes: Vec::new(),
            max: (self.limit * 4096 + 1024).min(4 * 1024 * 1024),
        };
        serde_json::to_writer(
            &mut output,
            &SavedIndex {
                version: 2,
                root: &self.root,
                entries: SavedEntries {
                    source,
                    stamps: &self.stamps,
                },
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

fn safe_path(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
        && !path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
}
