//! Document-bound excerpts with source ranges and bounded serialized fields.
use crate::{Error, Result, document::Document, layout::Locator, selection};
use serde::{Deserialize, Serialize};
use std::ops::Range;

pub const MAX_CLIPPINGS_PER_BOOK: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clipping {
    pub version: u32,
    pub book_id: String,
    pub range: Range<Locator>,
    pub text: String,
    pub recorded_at: u64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub highlighted: bool,
}

impl Clipping {
    pub fn capture(document: &mut Document, range: Range<Locator>, now: u64) -> Result<Self> {
        let text = selection::extract(document, range.clone())?;
        let clipping = Self {
            version: 1,
            book_id: document.metadata.id.clone(),
            range,
            text,
            recorded_at: now,
            highlighted: false,
        };
        clipping.validate(&document.metadata.id)?;
        Ok(clipping)
    }

    /// Validates stored fields without reading the source document.
    pub fn validate(&self, book_id: &str) -> Result<()> {
        if self.version != 1
            || self.book_id != book_id
            || self.book_id.len() != 64
            || !self
                .book_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || self.range.start >= self.range.end
            || self.range.end.chapter - self.range.start.chapter
                >= selection::MAX_SELECTION_CHAPTERS
            || self.text.is_empty()
            || self.text.len() > selection::MAX_SELECTION_BYTES
        {
            return Err(Error::Document("invalid clipping".into()));
        }
        Ok(())
    }

    /// Validates an excerpt wholly within the reader's retained parsed chapter.
    pub(crate) fn validate_chapter(
        &self,
        book_id: &str,
        chapter: &crate::document::Chapter,
    ) -> Result<()> {
        self.validate(book_id)?;
        if !selection::matches_chapter(chapter, self.range.clone(), &self.text)? {
            return Err(Error::Document(
                "clipping text does not match source".into(),
            ));
        }
        Ok(())
    }

    /// Rechecks identity, grapheme endpoints and exact excerpt bytes against the book.
    pub fn validate_document(&self, document: &mut Document) -> Result<()> {
        self.validate(&document.metadata.id)?;
        if !selection::matches_document(document, self.range.clone(), &self.text)? {
            return Err(Error::Document(
                "clipping text does not match source".into(),
            ));
        }
        Ok(())
    }
}

fn is_false(value: &bool) -> bool {
    !value
}
