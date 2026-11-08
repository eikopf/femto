//! Incrementally prepared StarDict lookup with private, bounded index caches.

use crate::{Error, Result};
use quireink_platform::storage::{DirEntry, Storage, StorageError};
use quireink_stardict::{Builder, Dictionary as Backend};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub use quireink_stardict::Definition;
const MAX_ENTRIES: u32 = 1_000_000;
const MAX_CACHE: usize = 128 * 1024;
const MAX_INDEX: u64 = 256 * 1024 * 1024;

/// One dictionary. Dropping it cancels preparation and discards its private caches.
pub struct Dictionary {
    backend: Backend,
    storage: DictionaryStorage,
    builders: Vec<Builder>,
}

impl Dictionary {
    pub fn open(ifo: impl AsRef<Path>) -> Result<Self> {
        let ifo = ifo.as_ref();
        if ifo.extension().is_none_or(|ext| ext != "ifo") {
            return Err(Error::Document("dictionary requires an .ifo path".into()));
        }
        let mut files = BTreeMap::new();
        for (extension, limit) in [
            ("ifo", 16 * 1024),
            ("idx", MAX_INDEX),
            ("syn", MAX_INDEX),
            ("dict", u32::MAX as u64),
            ("dict.dz", u32::MAX as u64),
        ] {
            let file = match File::open(ifo.with_extension(extension)) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if !file.metadata()?.is_file() || file.metadata()?.len() > limit {
                return Err(Error::Limit("dictionary file size or type"));
            }
            files.insert(format!("book.{extension}"), RefCell::new(file));
        }
        let mut storage = DictionaryStorage {
            files,
            caches: RefCell::new(BTreeMap::new()),
        };
        let backend = Backend::open(&storage, "book.ifo", "cache").map_err(backend_error)?;
        if backend.info.word_count > MAX_ENTRIES || backend.info.syn_count > MAX_ENTRIES {
            return Err(Error::Limit("dictionary entry count"));
        }
        storage.files.remove("book.ifo");
        if storage.files.contains_key("book.dict") {
            storage.files.remove("book.dict.dz");
        }
        let builders = backend.builders(&storage);
        Ok(Self {
            backend,
            storage,
            builders,
        })
    }

    pub fn name(&self) -> &str {
        &self.backend.info.name
    }

    /// Prepares at most one approximately 4 KiB index batch. True means ready.
    pub fn prepare_step(&mut self) -> Result<bool> {
        if let Some(builder) = self.builders.last_mut()
            && builder.step(&self.storage, 4096).map_err(backend_error)?
        {
            self.builders.pop();
        }
        Ok(self.builders.is_empty())
    }

    pub fn is_ready(&self) -> bool {
        self.builders.is_empty()
    }

    pub fn lookup(&self, word: &str) -> Result<Vec<Definition>> {
        if !self.is_ready() {
            return Err(Error::Document("dictionary index not prepared".into()));
        }
        if word.len() > 255 {
            return Err(Error::Limit("dictionary query bytes"));
        }
        self.backend
            .lookup(&self.storage, word)
            .map_err(backend_error)
    }
}

fn backend_error(error: quireink_stardict::Error) -> Error {
    Error::Document(format!("dictionary: {error}"))
}

struct DictionaryStorage {
    files: BTreeMap<String, RefCell<File>>,
    caches: RefCell<BTreeMap<String, Vec<u8>>>,
}

impl DictionaryStorage {
    fn mutate_cache(
        &self,
        path: &str,
        data: &[u8],
        append: bool,
    ) -> std::result::Result<(), StorageError> {
        if !matches!(path, "cache/idx.qbi" | "cache/syn.qbi") {
            return Err(StorageError::Unsupported);
        }
        let mut caches = self.caches.borrow_mut();
        let cache = caches.entry(path.into()).or_default();
        let old = if append { cache.len() } else { 0 };
        if data.len() > MAX_CACHE.saturating_sub(old) {
            return Err(StorageError::Full);
        }
        if !append {
            cache.clear();
        }
        cache
            .try_reserve(data.len())
            .map_err(|_| StorageError::Full)?;
        cache.extend_from_slice(data);
        Ok(())
    }
}

impl Storage for DictionaryStorage {
    fn read_at(
        &self,
        path: &str,
        offset: u64,
        buf: &mut [u8],
    ) -> std::result::Result<usize, StorageError> {
        if let Some(file) = self.files.get(path) {
            let mut file = file.borrow_mut();
            file.seek(SeekFrom::Start(offset))
                .map_err(|_| StorageError::Io)?;
            let mut read = 0;
            while read < buf.len() {
                let count = match file.read(&mut buf[read..]) {
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    result => result.map_err(|_| StorageError::Io)?,
                };
                if count == 0 {
                    break;
                }
                read += count;
            }
            return Ok(read);
        }
        let caches = self.caches.borrow();
        let cache = caches.get(path).ok_or(StorageError::NotFound)?;
        let offset = usize::try_from(offset).map_err(|_| StorageError::Io)?;
        let source = cache.get(offset..).unwrap_or_default();
        let count = source.len().min(buf.len());
        buf[..count].copy_from_slice(&source[..count]);
        Ok(count)
    }
    fn size(&self, path: &str) -> std::result::Result<u64, StorageError> {
        if let Some(file) = self.files.get(path) {
            return file
                .borrow()
                .metadata()
                .map(|m| m.len())
                .map_err(|_| StorageError::Io);
        }
        self.caches
            .borrow()
            .get(path)
            .map(|c| c.len() as u64)
            .ok_or(StorageError::NotFound)
    }
    fn write(&self, path: &str, data: &[u8]) -> std::result::Result<(), StorageError> {
        self.mutate_cache(path, data, false)
    }
    fn append(&self, path: &str, data: &[u8]) -> std::result::Result<(), StorageError> {
        self.mutate_cache(path, data, true)
    }
    fn list(
        &self,
        _: &str,
        _: &mut dyn FnMut(DirEntry<'_>),
    ) -> std::result::Result<(), StorageError> {
        Err(StorageError::Unsupported)
    }
    fn remove(&self, _: &str) -> std::result::Result<(), StorageError> {
        Err(StorageError::Unsupported)
    }
}

pub const MAX_DICTIONARIES: usize = 32;

pub fn valid_name(name: &str) -> bool {
    name.len() <= 255
        && name.ends_with(".ifo")
        && name.len() > 4
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
}

/// Discovers regular .ifo files in one directory without following symlinks.
pub fn discover(directory: impl AsRef<Path>) -> Result<Vec<std::path::PathBuf>> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut found = Vec::new();
    for (index, entry) in entries.enumerate() {
        if index == 4096 {
            return Err(Error::Limit("dictionary directory entries"));
        }
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "ifo") {
            continue;
        }
        let name = entry.file_name();
        if !name.to_str().is_some_and(valid_name) {
            return Err(Error::Document("invalid dictionary filename".into()));
        }
        if found.len() == MAX_DICTIONARIES {
            return Err(Error::Limit("dictionary choices"));
        }
        found.push(path);
    }
    found.sort();
    Ok(found)
}
