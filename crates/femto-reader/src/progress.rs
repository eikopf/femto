//! Portable progress records and optimistic conflict detection for exact book contents.
use crate::{
    Error, Result,
    document::{Block, Document},
    layout::Locator,
    state::ReaderState,
};
use serde::{Deserialize, Serialize};

pub const MAX_PROGRESS_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncCheckpoint {
    pub source: String,
    pub baseline: Progress,
    pub etag: String,
}
impl SyncCheckpoint {
    pub fn validate(&self, book_id: &str) -> Result<()> {
        self.baseline.validate()?;
        if self.baseline.book_id != book_id
            || self.source.is_empty()
            || self.source.len() > 4096
            || self.source.chars().any(char::is_control)
            || !(2..=256).contains(&self.etag.len())
            || !self.etag.starts_with('"')
            || !self.etag.ends_with('"')
            || !self.etag.as_bytes()[1..self.etag.len() - 1]
                .iter()
                .all(|b| *b == 0x21 || (0x23..=0x7e).contains(b))
        {
            return Err(Error::Document("invalid sync checkpoint".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Progress {
    pub version: u32,
    pub book_id: String,
    pub position: Locator,
    pub recorded_at: u64,
}
#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Applied,
    Unchanged,
    Conflict { local: Progress },
}
impl Progress {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_PROGRESS_BYTES {
            return Err(Error::Limit("progress record"));
        }
        let record: Self = serde_json::from_slice(bytes)?;
        record.validate()?;
        Ok(record)
    }
    fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.book_id.len() != 64
            || !self
                .book_id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(Error::Document(
                "unsupported progress version or book identity".into(),
            ));
        }
        Ok(())
    }
    pub fn from_state(state: &ReaderState, book_id: &str) -> Option<Self> {
        state.books.get(book_id).map(|book| Self {
            version: 1,
            book_id: book_id.into(),
            position: book.position,
            recorded_at: book.last_opened,
        })
    }
    pub fn validate_document(&self, document: &mut Document) -> Result<()> {
        self.validate()?;
        if self.book_id != document.metadata.id
            || self.position.chapter >= document.document_count()
        {
            return Err(Error::Document(
                "progress does not match book contents".into(),
            ));
        }
        let chapter = document.chapter(self.position.chapter)?;
        let valid = match chapter.blocks.get(self.position.block) {
            Some(Block::Text(paragraph)) => paragraph.text.is_char_boundary(self.position.byte),
            Some(Block::Image(_)) => self.position.byte == 0,
            None => {
                chapter.blocks.is_empty() && self.position.block == 0 && self.position.byte == 0
            }
        };
        if !valid {
            return Err(Error::Document("invalid progress location".into()));
        }
        Ok(())
    }
    /// Expected is the local snapshot captured before exchange, not a remote timestamp.
    pub fn apply(
        &self,
        expected: Option<&Self>,
        state: &mut ReaderState,
        document: &mut Document,
        now: u64,
    ) -> Result<Outcome> {
        self.validate_document(document)?;
        if let Some(expected) = expected {
            expected.validate_document(document)?;
        }
        let current = Self::from_state(state, &self.book_id);
        if current
            .as_ref()
            .is_some_and(|local| local.position == self.position)
        {
            return Ok(Outcome::Unchanged);
        }
        if let Some(local) = current.as_ref()
            && Some(local) != expected
        {
            return Ok(Outcome::Conflict {
                local: local.clone(),
            });
        }
        // A missing local entry cannot match a supplied expected snapshot.
        if current.is_none() && expected.is_some() {
            return Err(Error::Document("expected progress no longer exists".into()));
        }
        state.set_position(&self.book_id, self.position, now);
        Ok(Outcome::Applied)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncAction {
    Empty,
    Create,
    Upload,
    Download,
    Converged,
    Conflict,
}

/// Compare source positions against the last shared position, never wall-clock order.
pub fn sync_action(
    baseline: Option<Locator>,
    local: Option<Locator>,
    remote: Option<Locator>,
) -> SyncAction {
    use SyncAction::*;
    match (local, remote) {
        (None, None) => {
            if baseline.is_none() {
                Empty
            } else {
                Conflict
            }
        }
        (Some(_), None) => {
            if baseline.is_none() {
                Create
            } else {
                Conflict
            }
        }
        (None, Some(_)) => {
            if baseline.is_none() {
                Download
            } else {
                Conflict
            }
        }
        (Some(local), Some(remote)) if local == remote => Converged,
        (Some(local), Some(remote)) => match baseline {
            Some(base) if local == base => Download,
            Some(base) if remote == base => Upload,
            _ => Conflict,
        },
    }
}
