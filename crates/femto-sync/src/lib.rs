//! Transport-independent progress exchange. Persistence remains with the caller.
pub mod reader;
use femto_catalog::{
    auth::BasicAuth,
    deadline::Budget,
    http::{self, Precondition, StrongEtag},
};
use femto_reader::{
    document::Document,
    progress::{Progress, SyncAction, sync_action},
};
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Reader(#[from] femto_reader::Error),
    #[error(transparent)]
    Http(#[from] http::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("invalid progress source URL")]
    Source,
    #[error("sync checkpoint belongs to another source")]
    CheckpointSource,
    #[error("sync checkpoint changed during exchange")]
    CheckpointChanged,
    #[error("sync requires a strong remote ETag")]
    MissingEtag,
    #[error("remote changed during sync; retrieve again before deciding")]
    RemoteChanged,
    #[error("sync upload failed; remote commit may be unknown: {0}")]
    Upload(http::Error),
}

#[derive(Debug)]
pub enum Outcome {
    Empty,
    Conflict {
        remote: Option<Progress>,
        etag: Option<StrongEtag>,
    },
    Synchronized {
        action: SyncAction,
        baseline: Progress,
        etag: StrongEtag,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Local,
    Remote,
}

/// Snapshots must belong to the supplied document. The caller binds the baseline
/// to this source and local state, and checks local state again before applying.
pub struct Request<'a> {
    pub source: &'a str,
    pub local: Option<&'a Progress>,
    pub baseline: Option<&'a Progress>,
    pub auth: Option<&'a BasicAuth>,
    pub deadline: Instant,
    pub cancelled: Arc<AtomicBool>,
}
impl Request<'_> {
    /// Retrieves before comparing, performs at most one conditional PUT, and
    /// returns a proposed baseline without modifying local state or a checkpoint.
    pub fn execute(
        self,
        document: &mut Document,
        transport: &mut impl http::Transport,
    ) -> Result<Outcome, Error> {
        self.run(document, transport, None)
    }

    /// A resolution applies only to the remote record and ETag shown to the user.
    /// A fresh GET that differs returns another conflict without writing.
    pub fn resolve(
        self,
        document: &mut Document,
        transport: &mut impl http::Transport,
        choice: Choice,
        observed: Option<(&Progress, &StrongEtag)>,
    ) -> Result<Outcome, Error> {
        self.run(document, transport, Some((choice, observed)))
    }

    fn run(
        self,
        document: &mut Document,
        transport: &mut impl http::Transport,
        resolution: Option<(Choice, Option<(&Progress, &StrongEtag)>)>,
    ) -> Result<Outcome, Error> {
        let budget = Budget::new(self.deadline, self.cancelled.clone());
        budget.check()?;
        let url = source_url(self.source)?;
        let observed = resolution.and_then(|(_, observed)| observed.map(|(record, _)| record));
        for record in [self.local, self.baseline, observed].into_iter().flatten() {
            budget.check()?;
            record.validate_document(document)?;
        }
        budget.check()?;
        let source = url.as_str();
        let remote = match http::fetch_progress_exact(
            transport,
            source,
            self.deadline,
            self.cancelled.clone(),
            self.auth,
        ) {
            Ok(response) => {
                if response.source != source {
                    return Err(Error::Source);
                }
                let record = Progress::decode(&response.body)?;
                record.validate_document(document)?;
                Some((record, response.etag.ok_or(Error::MissingEtag)?))
            }
            Err(http::Error::Status(404)) => None,
            Err(error) => return Err(error.into()),
        };
        budget.check()?;
        let mut action = sync_action(
            self.baseline.map(|p| p.position),
            self.local.map(|p| p.position),
            remote.as_ref().map(|(p, _)| p.position),
        );
        if let Some((choice, observed)) = resolution {
            let fresh = remote.as_ref().map(|(record, tag)| (record, tag));
            action = if fresh != observed {
                SyncAction::Conflict
            } else {
                match choice {
                    Choice::Local if self.local.is_some() => {
                        if remote.is_some() {
                            SyncAction::Upload
                        } else {
                            SyncAction::Create
                        }
                    }
                    Choice::Remote if remote.is_some() => SyncAction::Download,
                    _ => SyncAction::Conflict,
                }
            };
        }
        let (baseline, etag) = match action {
            SyncAction::Empty => return Ok(Outcome::Empty),
            SyncAction::Conflict => {
                let (remote, etag) = match remote {
                    Some((record, tag)) => (Some(record), Some(tag)),
                    None => (None, None),
                };
                return Ok(Outcome::Conflict { remote, etag });
            }
            SyncAction::Download | SyncAction::Converged => remote.unwrap(),
            SyncAction::Create | SyncAction::Upload => {
                let local = self.local.unwrap();
                let condition = match &remote {
                    None => Precondition::Create,
                    Some((_, tag)) => Precondition::Match(tag),
                };
                let etag = match http::put_progress(
                    transport,
                    source,
                    self.deadline,
                    self.cancelled.clone(),
                    self.auth,
                    &serde_json::to_vec(local)?,
                    condition,
                ) {
                    Ok(tag) => tag,
                    Err(http::Error::Status(412)) => return Err(Error::RemoteChanged),
                    Err(error) => return Err(Error::Upload(error)),
                };
                (local.clone(), etag)
            }
        };
        budget.check()?;
        Ok(Outcome::Synchronized {
            action,
            baseline,
            etag,
        })
    }
}

fn source_url(source: &str) -> Result<url::Url, Error> {
    if source.len() > 4096 {
        return Err(Error::Source);
    }
    let url = url::Url::parse(source).map_err(|_| Error::Source)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::Source);
    }
    Ok(url)
}

#[cfg(test)]
mod tests;
