//! Owned snapshots for a worker exchange and single-owner reader/journal commit.
use crate::{Choice, Error, Outcome, Request};
use femto_catalog::{auth::BasicAuth, deadline::Budget, http};
use femto_reader::{
    controller::{Effect, Reader},
    document::Document,
    progress::{Progress, SyncAction, SyncCheckpoint},
    state::ReaderState,
};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

#[derive(Debug)]
pub struct Session {
    source: String,
    local: Progress,
    checkpoint: Option<SyncCheckpoint>,
    resolution: Option<(Choice, Option<(Progress, http::StrongEtag)>)>,
}
impl Session {
    /// Captures state without chapter I/O. Exchange validates locations in its
    /// owned document before transport access. A changed source requires reset.
    pub fn prepare(reader: &Reader, source: &str) -> Result<Self, Error> {
        let source = crate::source_url(source)?.to_string();
        let local = reader.progress_snapshot();
        let checkpoint = reader
            .state
            .books
            .get(&local.book_id)
            .and_then(|b| b.sync.clone());
        if let Some(checkpoint) = &checkpoint {
            checkpoint.validate(&local.book_id)?;
            if checkpoint.source != source {
                return Err(Error::CheckpointSource);
            }
        }
        Ok(Self {
            source,
            local,
            checkpoint,
            resolution: None,
        })
    }

    /// The worker owns its document; the live reader remains with the UI thread.
    pub fn exchange(
        self,
        document: &mut Document,
        transport: &mut impl http::Transport,
        auth: Option<&BasicAuth>,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Completion, Error> {
        let budget = Budget::new(deadline, cancelled.clone());
        let request = Request {
            source: &self.source,
            local: Some(&self.local),
            baseline: self.checkpoint.as_ref().map(|c| &c.baseline),
            auth,
            deadline,
            cancelled,
        };
        let outcome = match &self.resolution {
            None => request.execute(document, transport)?,
            Some((choice, remote)) => request.resolve(
                document,
                transport,
                *choice,
                remote.as_ref().map(|(p, t)| (p, t)),
            )?,
        };
        Ok(Completion {
            session: self,
            outcome,
            budget,
        })
    }
}

pub struct Completion {
    session: Session,
    outcome: Outcome,
    budget: Budget,
}
#[derive(Debug)]
pub enum Commit {
    Empty,
    Conflict { conflict: Box<Conflict> },
    Saved { action: SyncAction, effect: Effect },
}
impl Completion {
    /// The persist callback must save the whole state through the current owner.
    /// After a persistence error, the caller must recover its store before retrying.
    pub fn commit(
        self,
        reader: &mut Reader,
        now: u64,
        persist: impl FnOnce(&ReaderState) -> femto_reader::Result<()>,
    ) -> Result<Commit, Error> {
        self.budget.check()?;
        let current = reader
            .state
            .books
            .get(&self.session.local.book_id)
            .and_then(|b| b.sync.as_ref());
        if current != self.session.checkpoint.as_ref() {
            return Err(Error::CheckpointChanged);
        }
        if reader.progress_snapshot() != self.session.local {
            return Err(
                femto_reader::Error::Document("local progress changed during sync".into()).into(),
            );
        }
        match self.outcome {
            Outcome::Empty => Ok(Commit::Empty),
            Outcome::Conflict { remote, etag } => Ok(Commit::Conflict {
                conflict: Box::new(Conflict {
                    session: self.session,
                    remote: remote.zip(etag),
                }),
            }),
            Outcome::Synchronized {
                action,
                baseline,
                etag,
            } => {
                let checkpoint = SyncCheckpoint {
                    source: self.session.source,
                    baseline,
                    etag: etag.as_str().into(),
                };
                let effect =
                    reader.commit_progress(&self.session.local, checkpoint, now, |state| {
                        self.budget.check()?;
                        persist(state)
                    })?;
                // Once saved, finish the in-memory commit even if cancellation arrives.
                Ok(Commit::Saved { action, effect })
            }
        }
    }
}

#[derive(Debug)]
pub struct Conflict {
    session: Session,
    remote: Option<(Progress, http::StrongEtag)>,
}
impl Conflict {
    pub fn source(&self) -> &str {
        &self.session.source
    }
    pub fn local(&self) -> &Progress {
        &self.session.local
    }
    pub fn remote(&self) -> Option<&Progress> {
        self.remote.as_ref().map(|(p, _)| p)
    }
    pub fn choose(mut self, reader: &Reader, choice: Choice) -> Result<Session, Error> {
        let current = reader
            .state
            .books
            .get(&self.session.local.book_id)
            .and_then(|b| b.sync.as_ref());
        if current != self.session.checkpoint.as_ref() {
            return Err(Error::CheckpointChanged);
        }
        if reader.progress_snapshot() != self.session.local {
            return Err(
                femto_reader::Error::Document("local progress changed; sync again".into()).into(),
            );
        }
        self.session.resolution = Some((choice, self.remote));
        Ok(self.session)
    }
}

/// Captures the book and checkpoint displayed by a reset prompt.
pub struct Reset {
    local: Progress,
    checkpoint: SyncCheckpoint,
}
impl Reset {
    pub fn prepare(reader: &Reader) -> Option<Self> {
        let local = reader.progress_snapshot();
        let checkpoint = reader.state.books.get(&local.book_id)?.sync.clone()?;
        Some(Self { local, checkpoint })
    }

    /// Removes only the captured checkpoint after persistence succeeds.
    pub fn commit(
        self,
        reader: &mut Reader,
        persist: impl FnOnce(&ReaderState) -> femto_reader::Result<()>,
    ) -> Result<(), Error> {
        if reader.progress_snapshot() != self.local {
            return Err(femto_reader::Error::Document(
                "book or position changed; reopen reset prompt".into(),
            )
            .into());
        }
        let current = reader
            .state
            .books
            .get(&self.local.book_id)
            .and_then(|b| b.sync.as_ref());
        if current != Some(&self.checkpoint) {
            return Err(Error::CheckpointChanged);
        }
        let mut state = reader.state.clone();
        state.books.get_mut(&self.local.book_id).unwrap().sync = None;
        persist(&state)?;
        reader.state = state;
        Ok(())
    }
}
