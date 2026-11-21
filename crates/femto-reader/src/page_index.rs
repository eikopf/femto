//! Persistent chapter page boundaries, so a chapter that was indexed once
//! opens without laying out the pages before the reading position.
//!
//! One file per book holds the chapters indexed under one layout key. A key
//! covers everything that determines page boundaries besides the book itself.
use crate::{
    layout::{Fonts, LayoutConfig, Locator},
    pagination::PageBounds,
};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MAGIC: &[u8; 4] = b"FPI1";
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PAGES: usize = 16_384;

/// FNV-1a, stable across builds and platforms.
pub(crate) fn fnv(state: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(state, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
pub(crate) const FNV_START: u64 = 0xcbf2_9ce4_8422_2325;

#[derive(Clone)]
pub struct IndexStore {
    directory: PathBuf,
    salt: u64,
}

impl IndexStore {
    /// `salt` must change whenever layout code may change, for example with
    /// the application build; stale files are then ignored and replaced.
    pub fn new(directory: impl Into<PathBuf>, salt: &str) -> Self {
        Self {
            directory: directory.into(),
            salt: fnv(FNV_START, salt.as_bytes()),
        }
    }

    fn key(&self, fonts: &Fonts, config: LayoutConfig) -> u64 {
        let mut hash = fnv(self.salt, &fonts.digest().to_le_bytes());
        for field in [
            config.width,
            config.height,
            config.margin,
            config.font_size.to_bits(),
            config.line_height.to_bits(),
            config.hyphenation as u32,
        ] {
            hash = fnv(hash, &field.to_le_bytes());
        }
        hash
    }

    fn path(&self, book: &str) -> PathBuf {
        let name: String = book
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(32)
            .collect();
        self.directory.join(format!("{name}.pages"))
    }

    /// Chapters recorded for `book` under the key, as raw records.
    fn records(&self, book: &str, key: u64) -> Vec<(u32, Vec<u8>)> {
        let read = || -> io::Result<Vec<u8>> {
            let file = std::fs::File::open(self.path(book))?;
            let mut data = Vec::new();
            file.take(MAX_FILE_BYTES + 1).read_to_end(&mut data)?;
            Ok(data)
        };
        let Ok(data) = read() else {
            return Vec::new();
        };
        let mut records = Vec::new();
        let header = data.len() as u64 <= MAX_FILE_BYTES
            && data.len() >= 12
            && &data[..4] == MAGIC
            && data[4..12] == key.to_le_bytes();
        let mut rest = if header { &data[12..] } else { &[][..] };
        while rest.len() >= 8 {
            let chapter = u32::from_le_bytes(rest[..4].try_into().expect("four bytes"));
            let count = u32::from_le_bytes(rest[4..8].try_into().expect("four bytes")) as usize;
            let Some(bytes) = count
                .checked_mul(16)
                .filter(|bytes| *bytes <= rest.len() - 8)
            else {
                // A truncated file keeps the complete records before the damage.
                break;
            };
            records.push((chapter, rest[8..8 + bytes].to_vec()));
            rest = &rest[8 + bytes..];
        }
        records
    }

    pub(crate) fn load(
        &self,
        book: &str,
        fonts: &Fonts,
        config: LayoutConfig,
        chapter: usize,
    ) -> Option<Vec<PageBounds>> {
        let (_, record) = self
            .records(book, self.key(fonts, config))
            .into_iter()
            .find(|(index, _)| *index as usize == chapter)?;
        let word =
            |bytes: &[u8]| u32::from_le_bytes(bytes.try_into().expect("four bytes")) as usize;
        let locator = |bytes: &[u8]| Locator {
            chapter,
            block: word(&bytes[..4]),
            byte: word(&bytes[4..8]),
        };
        Some(
            record
                .as_chunks::<16>()
                .0
                .iter()
                .map(|page| PageBounds {
                    start: locator(&page[..8]),
                    end: locator(&page[8..]),
                })
                .collect(),
        )
    }

    /// Records a complete chapter index, replacing files of other layout keys.
    pub(crate) fn store(
        &self,
        book: &str,
        fonts: &Fonts,
        config: LayoutConfig,
        chapter: usize,
        pages: &[PageBounds],
    ) -> io::Result<()> {
        let narrow = |value: usize| u32::try_from(value).ok();
        let (Some(index), true) = (narrow(chapter), pages.len() <= MAX_PAGES) else {
            return Ok(());
        };
        let mut record = Vec::with_capacity(16 * pages.len());
        for page in pages {
            for value in [
                page.start.block,
                page.start.byte,
                page.end.block,
                page.end.byte,
            ] {
                // Positions beyond the format's range leave the chapter unrecorded.
                let Some(value) = narrow(value) else {
                    return Ok(());
                };
                record.extend_from_slice(&value.to_le_bytes());
            }
        }
        let key = self.key(fonts, config);
        let mut records = self.records(book, key);
        records.retain(|(other, _)| *other != index);
        records.push((index, record));
        let mut data = Vec::new();
        data.extend_from_slice(MAGIC);
        data.extend_from_slice(&key.to_le_bytes());
        for (chapter, record) in &records {
            data.extend_from_slice(&chapter.to_le_bytes());
            data.extend_from_slice(&((record.len() / 16) as u32).to_le_bytes());
            data.extend_from_slice(record);
        }
        if data.len() as u64 > MAX_FILE_BYTES {
            return Ok(());
        }
        std::fs::create_dir_all(&self.directory)?;
        let path = self.path(book);
        let partial = path.with_extension("pages.part");
        write_all(&partial, &data)?;
        // FAT rename does not replace an existing file.
        let _ = std::fs::remove_file(&path);
        std::fs::rename(&partial, &path)
    }
}

fn write_all(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut file = std::fs::File::create(path)?;
    file.write_all(data)?;
    file.sync_all()
}
