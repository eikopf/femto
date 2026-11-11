use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::{
    Error, Result,
    document::Document,
    layout::{LayoutConfig, Locator},
};

const STATE_VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 4 * 1024 * 1024;
static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

struct StateBytes {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}
impl Write for StateBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::other("state file limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrontlightSettings {
    pub brightness: u8,
    pub warmth: u8,
}

impl Default for FrontlightSettings {
    fn default() -> Self {
        Self {
            brightness: 0,
            warmth: 4,
        }
    }
}

impl FrontlightSettings {
    pub fn validate(self) -> Result<()> {
        if self.brightness > 10 || self.warmth > 9 {
            return Err(Error::Document("invalid frontlight settings".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bookmark {
    pub location: Locator,
    pub label: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BookState {
    pub position: Locator,
    pub bookmarks: Vec<Bookmark>,
    pub last_opened: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clippings: Vec<crate::clipping::Clipping>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub typography: Option<BookTypography>,
}

/// Book preferences exclude physical viewport dimensions and device rotation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BookTypography {
    pub margin: u32,
    #[serde(deserialize_with = "crate::deserialize_f32")]
    pub font_size: f32,
    #[serde(deserialize_with = "crate::deserialize_f32")]
    pub line_height: f32,
    pub hyphenation: crate::layout::Hyphenation,
    pub text_raster: crate::display::TextRaster,
}

impl BookTypography {
    pub fn current(state: &ReaderState) -> Self {
        Self {
            margin: state.layout.margin,
            font_size: state.layout.font_size,
            line_height: state.layout.line_height,
            hyphenation: state.layout.hyphenation,
            text_raster: state.text_raster,
        }
    }

    pub fn layout(self, viewport: LayoutConfig) -> LayoutConfig {
        LayoutConfig {
            margin: self.margin,
            font_size: self.font_size,
            line_height: self.line_height,
            hyphenation: self.hyphenation,
            ..viewport
        }
    }

    pub fn apply(self, state: &mut ReaderState) {
        state.layout = self.layout(state.layout);
        state.text_raster = self.text_raster;
    }

    fn validate(self) -> Result<()> {
        // Stored preferences may belong to a different viewport; validate their
        // absolute bounds here and the actual reading area when opening a book.
        self.layout(LayoutConfig {
            width: 2048,
            height: 2048,
            ..Default::default()
        })
        .validate()?;
        self.text_raster.validate()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdleSleep {
    Off,
    FiveMinutes,
    #[default]
    TenMinutes,
    ThirtyMinutes,
    SixtyMinutes,
}
impl IdleSleep {
    pub const fn minutes(self) -> u64 {
        match self {
            Self::Off => 0,
            Self::FiveMinutes => 5,
            Self::TenMinutes => 10,
            Self::ThirtyMinutes => 30,
            Self::SixtyMinutes => 60,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rotation {
    #[default]
    Portrait,
    Clockwise,
    Inverted,
    Counterclockwise,
}
impl Rotation {
    pub fn landscape(self) -> bool {
        matches!(self, Self::Clockwise | Self::Counterclockwise)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReaderState {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dictionary_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_bindings: Option<crate::remote::SavedBindings>,
    pub layout: LayoutConfig,
    #[serde(default)]
    pub text_raster: crate::display::TextRaster,
    #[serde(default)]
    pub frontlight: FrontlightSettings,
    #[serde(default)]
    pub idle_sleep: IdleSleep,
    #[serde(default)]
    pub rotation: Rotation,
    pub books: BTreeMap<String, BookState>,
}

impl Default for ReaderState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            dictionary_name: None,
            remote_bindings: None,
            layout: LayoutConfig::default(),
            text_raster: Default::default(),
            frontlight: Default::default(),
            idle_sleep: IdleSleep::default(),
            rotation: Rotation::default(),
            books: BTreeMap::new(),
        }
    }
}

impl ReaderState {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(MAX_STATE_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(Error::Limit("state file"));
        }
        let state: Self = serde_json::from_slice(&bytes)?;
        if state.version != STATE_VERSION {
            return Err(Error::Document("unsupported state version".into()));
        }
        state.layout.validate()?;
        state.text_raster.validate()?;
        state.frontlight.validate()?;
        state.validate_books()?;
        Ok(state)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        if self.version != STATE_VERSION {
            return Err(Error::Document("unsupported state version".into()));
        }
        self.layout.validate()?;
        self.text_raster.validate()?;
        self.frontlight.validate()?;
        self.validate_books()?;
        let bytes = self.encode(true)?;
        atomic_write(path.as_ref(), &bytes)
    }

    pub(crate) fn encode(&self, pretty: bool) -> Result<Vec<u8>> {
        let mut output = StateBytes {
            bytes: Vec::new(),
            limit: MAX_STATE_BYTES as usize,
            exceeded: false,
        };
        let result = if pretty {
            serde_json::to_writer_pretty(&mut output, self)
        } else {
            serde_json::to_writer(&mut output, self)
        };
        if output.exceeded {
            return Err(Error::Limit("state file"));
        }
        result?;
        Ok(output.bytes)
    }

    pub fn set_position(&mut self, id: &str, position: Locator, now: u64) {
        let book = self.books.entry(id.into()).or_default();
        book.position = position;
        book.last_opened = now;
    }

    pub fn typography_for(&self, id: &str) -> BookTypography {
        self.books
            .get(id)
            .and_then(|book| book.typography)
            .unwrap_or_else(|| BookTypography::current(self))
    }

    pub(crate) fn validate_books(&self) -> Result<()> {
        if self
            .dictionary_name
            .as_deref()
            .is_some_and(|name| !crate::dictionary::valid_name(name))
        {
            return Err(Error::Document("invalid dictionary preference".into()));
        }
        for (id, book) in &self.books {
            if book.clippings.len() > crate::clipping::MAX_CLIPPINGS_PER_BOOK {
                return Err(Error::Limit("book clippings"));
            }
            for (index, clipping) in book.clippings.iter().enumerate() {
                clipping.validate(id)?;
                if book.clippings[..index]
                    .iter()
                    .any(|prior| prior.range == clipping.range)
                {
                    return Err(Error::Document("duplicate clipping range".into()));
                }
            }
            if let Some(typography) = book.typography {
                typography.validate()?;
            }
        }
        Ok(())
    }

    /// Adds an excerpt without changing the book's reading position or recency.
    /// Repeated capture of the same range and text retains the original timestamp.
    pub fn add_clipping(&mut self, clipping: crate::clipping::Clipping) -> Result<bool> {
        clipping.validate(&clipping.book_id)?;
        self.validate_books()?;
        if let Some(book) = self.books.get(&clipping.book_id) {
            if let Some(existing) = book.clippings.iter().find(|c| c.range == clipping.range) {
                if existing.text != clipping.text {
                    return Err(Error::Document(
                        "clipping text conflicts with saved range".into(),
                    ));
                }
                return Ok(false);
            }
            if book.clippings.len() >= crate::clipping::MAX_CLIPPINGS_PER_BOOK {
                return Err(Error::Limit("book clippings"));
            }
        }
        self.books
            .entry(clipping.book_id.clone())
            .or_default()
            .clippings
            .push(clipping);
        Ok(true)
    }

    /// Updates only the exact excerpt supplied by the caller.
    pub fn set_clipping_highlight(
        &mut self,
        expected: &crate::clipping::Clipping,
        enabled: bool,
    ) -> Result<()> {
        let record = self
            .books
            .get_mut(&expected.book_id)
            .and_then(|book| book.clippings.iter_mut().find(|record| *record == expected))
            .ok_or_else(|| Error::Document("clipping changed or no longer exists".into()))?;
        record.highlighted = enabled;
        Ok(())
    }

    /// Removes only the exact record previously presented for confirmation.
    pub fn remove_clipping(&mut self, expected: &crate::clipping::Clipping) -> Result<()> {
        let book = self
            .books
            .get_mut(&expected.book_id)
            .ok_or_else(|| Error::Document("clipping no longer exists".into()))?;
        let index = book
            .clippings
            .iter()
            .position(|record| record == expected)
            .ok_or_else(|| Error::Document("clipping changed or no longer exists".into()))?;
        book.clippings.remove(index);
        Ok(())
    }

    pub fn toggle_bookmark(&mut self, id: &str, location: Locator, label: String) -> bool {
        let bookmarks = &mut self.books.entry(id.into()).or_default().bookmarks;
        if let Some(index) = bookmarks.iter().position(|b| b.location == location) {
            bookmarks.remove(index);
            false
        } else {
            bookmarks.push(Bookmark { location, label });
            true
        }
    }
}

/// Replaces a file only after the new contents are durable. The destination and
/// temporary file share a directory so rename cannot cross filesystem boundaries.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .ok_or_else(|| Error::Document("missing state filename".into()))?
        .to_string_lossy();
    let (temp, mut file) = loop {
        let temp = parent.join(format!(
            ".{name}.{}.{}.tmp",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temp) {
            Ok(file) => break (temp, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    };
    let result = (|| -> std::io::Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp, path)?;
        #[cfg(unix)]
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result.map_err(Into::into)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryMetadata {
    pub id: String,
    pub title: String,
    pub author: String,
    pub language: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LibraryEntry {
    pub path: PathBuf,
    pub metadata: LibraryMetadata,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Library {
    pub entries: Vec<LibraryEntry>,
    pub errors: Vec<String>,
    #[serde(default)]
    pub truncated: bool,
}

/// Cooperative folder scan. Dropping the scanner cancels without publishing partial results.
pub struct LibraryScan {
    library: Library,
    directories: Vec<fs::ReadDir>,
    limit: usize,
    visited: usize,
    done: bool,
    cache: Option<crate::library_cache::Cache>,
    hashing: Option<(PathBuf, crate::document::FileHash)>,
    cache_hits: usize,
    traversal_failed: bool,
}

impl LibraryScan {
    pub fn new(root: impl AsRef<Path>, limit: usize) -> Result<Self> {
        if !(1..=4096).contains(&limit) {
            return Err(Error::Limit("library limit"));
        }
        let root = fs::canonicalize(root)?;
        Ok(Self {
            library: Library::default(),
            directories: vec![fs::read_dir(root)?],
            limit,
            visited: 0,
            done: false,
            cache: None,
            hashing: None,
            cache_hits: 0,
            traversal_failed: false,
        })
    }
    /// Uses a disposable metadata index in the library root. Each book's bytes
    /// are hashed before reuse; timestamps alone never authorize a cache hit.
    pub fn cached(root: impl AsRef<Path>, limit: usize) -> Result<Self> {
        let mut scan = Self::new(&root, limit)?;
        let root = fs::canonicalize(root)?;
        let (cache, warning) = crate::library_cache::Cache::open(&root, limit);
        scan.cache = Some(cache);
        if let Some(warning) = warning {
            scan.library.record_error(warning);
        }
        Ok(scan)
    }
    pub fn cache_hits(&self) -> usize {
        self.cache_hits
    }

    /// Returns a cached path hint. Opening must hash and parse the file again.
    pub fn resume_hint(&self, state: &ReaderState) -> Option<PathBuf> {
        self.cache.as_ref()?.resume_hint(state)
    }

    /// Keeps only entries verified before cancellation; the disk cache is unchanged.
    pub fn cancel(mut self) -> Library {
        self.library.truncated = true;
        self.library.entries.sort_by(|a, b| a.path.cmp(&b.path));
        self.library
            .entries
            .sort_by_cached_key(|entry| library_key(&entry.metadata.title));
        self.library
    }

    /// Processes one directory entry or at most 8 KiB of book hashing.
    /// Cache loading, metadata parsing on a miss, and index saving are synchronous.
    pub fn step(&mut self) -> Result<Option<Library>> {
        self.step_for(std::time::Duration::ZERO)
    }

    /// Batches hash reads within the budget. Parsing and filesystem calls can exceed it.
    pub fn step_for(&mut self, budget: std::time::Duration) -> Result<Option<Library>> {
        if self.done {
            return Err(Error::Document("library scan already completed".into()));
        }
        if let Some((path, hashing)) = &mut self.hashing {
            match hashing.step_for(budget) {
                Ok(Some(file)) => {
                    let (path, _) = self.hashing.take().unwrap();
                    if let Some(metadata) = self
                        .cache
                        .as_ref()
                        .and_then(|cache| cache.get(&path, file.id()))
                    {
                        self.cache_hits += 1;
                        self.library.entries.push(LibraryEntry { path, metadata });
                    } else {
                        let result = Document::from_hashed(&path, file);
                        self.record_book(path, result);
                    }
                }
                Ok(None) => (),
                Err(error) => {
                    self.library
                        .record_error(format!("{}: {error}", path.display()));
                    self.hashing = None;
                }
            }
            return Ok(None);
        }
        if self.directories.is_empty() || self.library.truncated {
            self.directories.clear();
            self.done = true;
            // Stable title sorting retains path order for equivalent titles.
            self.library.entries.sort_by(|a, b| a.path.cmp(&b.path));
            self.library
                .entries
                .sort_by_cached_key(|entry| library_key(&entry.metadata.title));
            if !self.library.truncated
                && !self.traversal_failed
                && let Some(cache) = &self.cache
                && let Err(error) = cache.save(&self.library.entries)
            {
                self.library
                    .record_error(format!("library cache save failed: {error}"));
            }
            return Ok(Some(std::mem::take(&mut self.library)));
        }
        let Some(entry) = self.directories.last_mut().unwrap().next() else {
            self.directories.pop();
            return Ok(None);
        };
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                self.visited += 1;
                if self.visited >= 16_384 {
                    self.library.truncated = true;
                }
                self.record_traversal_error(format!("directory entry: {error}"));
                return Ok(None);
            }
        };
        if self.cache.is_some()
            && entry.file_name().to_str().is_some_and(|name| {
                name == ".femto-library-cache.json"
                    || name.starts_with("..femto-library-cache.json.")
            })
        {
            return Ok(None);
        }
        if self.visited >= 16_384 {
            self.library.truncated = true;
            return Ok(None);
        }
        self.visited += 1;
        let path = entry.path();
        let kind = match entry.file_type() {
            Ok(kind) => kind,
            Err(error) => {
                self.record_traversal_error(format!("{}: {error}", path.display()));
                return Ok(None);
            }
        };
        if kind.is_symlink() {
            return Ok(None);
        }
        if kind.is_dir() {
            if self.directories.len() >= 17 {
                self.record_traversal_error(format!(
                    "directory depth exceeded: {}",
                    path.display()
                ));
            } else {
                match fs::read_dir(&path) {
                    Ok(directory) => self.directories.push(directory),
                    Err(error) => {
                        self.record_traversal_error(format!("{}: {error}", path.display()))
                    }
                }
            }
        } else if kind.is_file()
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("epub") || e.eq_ignore_ascii_case("txt"))
        {
            if self.library.entries.len() >= self.limit {
                self.library.truncated = true;
                return Ok(None);
            }
            if self.cache.is_some() {
                match File::open(&path) {
                    Ok(file) => self.hashing = Some((path, crate::document::FileHash::new(file))),
                    Err(error) => self
                        .library
                        .record_error(format!("{}: {error}", path.display())),
                }
            } else {
                self.read_book(path);
            }
        }
        Ok(None)
    }
    fn record_traversal_error(&mut self, error: String) {
        self.traversal_failed = true;
        self.library.record_error(error);
    }
    fn read_book(&mut self, path: PathBuf) {
        let result = Document::open(&path);
        self.record_book(path, result);
    }
    fn record_book(&mut self, path: PathBuf, result: Result<Document>) {
        match result {
            Ok(document) => self.library.entries.push(LibraryEntry {
                path,
                metadata: LibraryMetadata {
                    id: document.metadata.id,
                    title: document.metadata.title.chars().take(256).collect(),
                    author: document.metadata.author.chars().take(256).collect(),
                    language: document.metadata.language.chars().take(32).collect(),
                },
            }),
            Err(error) => self
                .library
                .record_error(format!("{}: {error}", path.display())),
        }
    }
}

impl Library {
    pub fn scan(root: impl AsRef<Path>) -> Result<Self> {
        Self::scan_with_limit(root, 4096)
    }

    pub fn scan_with_limit(root: impl AsRef<Path>, limit: usize) -> Result<Self> {
        let mut scan = LibraryScan::new(root, limit)?;
        loop {
            if let Some(library) = scan.step()? {
                return Ok(library);
            }
        }
    }
    pub fn scan_cached(root: impl AsRef<Path>, limit: usize) -> Result<Self> {
        let mut scan = LibraryScan::cached(root, limit)?;
        loop {
            if let Some(library) = scan.step()? {
                return Ok(library);
            }
        }
    }

    fn record_error(&mut self, error: String) {
        if self.errors.len() < 64 {
            self.errors.push(
                error
                    .chars()
                    .take(512)
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect(),
            );
        }
    }

    pub fn search(&self, query: &str) -> Vec<&LibraryEntry> {
        let query = library_key(query);
        self.entries
            .iter()
            .filter(|entry| {
                library_key(&entry.metadata.title).contains(&query)
                    || library_key(&entry.metadata.author).contains(&query)
                    || library_key(&entry.path.to_string_lossy()).contains(&query)
            })
            .collect()
    }

    pub fn recent<'a>(&'a self, state: &ReaderState) -> Vec<&'a LibraryEntry> {
        let mut entries: Vec<_> = self
            .entries
            .iter()
            .filter(|e| state.books.contains_key(&e.metadata.id))
            .collect();
        entries.sort_by_key(|entry| std::cmp::Reverse(state.books[&entry.metadata.id].last_opened));
        entries
    }
}

fn library_key(text: &str) -> String {
    text.nfc()
        .collect::<String>()
        .to_lowercase()
        .nfc()
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn json_output_limit_applies_to_encoded_bytes_at_the_write_boundary() {
        for text in ["", "plain", "é", "\0\"\\\n"] {
            let expected = serde_json::to_vec(text).unwrap();
            for limit in [expected.len() - 1, expected.len()] {
                let mut output = super::StateBytes {
                    bytes: Vec::new(),
                    limit,
                    exceeded: false,
                };
                let result = serde_json::to_writer(&mut output, text);
                assert!(output.bytes.len() <= limit);
                assert_eq!(output.exceeded, limit < expected.len());
                assert_eq!(result.is_err(), limit < expected.len());
                if result.is_ok() {
                    assert_eq!(output.bytes, expected);
                }
            }
        }
    }

    #[test]
    fn bounded_state_encoding_preserves_compact_and_pretty_formats() {
        let mut state = super::ReaderState::default();
        state.toggle_bookmark("book", Default::default(), "é\n\"quoted\"".into());
        assert_eq!(
            state.encode(false).unwrap(),
            serde_json::to_vec(&state).unwrap()
        );
        assert_eq!(
            state.encode(true).unwrap(),
            serde_json::to_vec_pretty(&state).unwrap()
        );
    }

    use super::*;

    #[test]
    fn library_search_matches_canonical_unicode_without_mutating_sources() {
        let library = Library {
            entries: vec![LibraryEntry {
                path: "books/한.txt".into(),
                metadata: LibraryMetadata {
                    id: "unchanged-content-id".into(),
                    title: "A Cafe\u{301} and Ångström".into(),
                    author: "Zoë".into(),
                    language: "en".into(),
                },
            }],
            ..Library::default()
        };
        let original = serde_json::to_vec(&library).unwrap();
        for query in ["CAFÉ", "a\u{30a}ngstro\u{308}m", "ZOE\u{308}", "한.TXT", ""] {
            let found = library.search(query);
            assert_eq!(found.len(), 1, "{query}");
            assert!(std::ptr::eq(found[0], &library.entries[0]));
        }
        for query in ["cafe", "zoe", "missing"] {
            assert!(library.search(query).is_empty(), "{query}");
        }
        assert_eq!(serde_json::to_vec(&library).unwrap(), original);
    }

    #[test]
    fn library_search_retains_compatibility_and_locale_distinctions() {
        let library = Library {
            entries: vec![LibraryEntry {
                path: "books/entry.txt".into(),
                metadata: LibraryMetadata {
                    id: "id".into(),
                    title: "① ﬃ Straße İ".into(),
                    author: String::new(),
                    language: "de".into(),
                },
            }],
            ..Library::default()
        };
        for query in ["①", "ﬃ", "straße", "i\u{307}"] {
            assert_eq!(library.search(query).len(), 1, "{query}");
        }
        for query in ["1", "ffi", "STRASSE", "ı"] {
            assert!(library.search(query).is_empty(), "{query}");
        }
    }

    #[test]
    fn persistence_round_trip_and_unknown_version_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reader.json");
        let mut state = ReaderState::load(&path).unwrap();
        let position = Locator {
            chapter: 2,
            block: 3,
            byte: 17,
        };
        state.set_position("book", position, 42);
        assert!(state.toggle_bookmark("book", position, "example".into()));
        state.save(&path).unwrap();
        let saved = ReaderState::load(&path).unwrap();
        assert_eq!(saved.books["book"].position, position);
        assert_eq!(saved.books["book"].bookmarks.len(), 1);
        fs::write(&path, br#"{"version":900,"layout":{"width":480,"height":800,"margin":24,"font_size":24,"line_height":1.35},"books":{}}"#).unwrap();
        assert!(ReaderState::load(&path).is_err());
    }

    #[test]
    fn corrupt_state_is_not_replaced_by_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reader.json");
        fs::write(&path, b"{broken").unwrap();
        assert!(ReaderState::load(&path).is_err());
        assert_eq!(fs::read(path).unwrap(), b"{broken");
    }
}
