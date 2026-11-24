//! Reader actions, asked for from the menu or made automatically. Workers
//! never write reader state.
use crate::{
    book::Book,
    config::{Route, Settings},
};
use femto_catalog::{
    deadline::{Budget, Limit},
    http::Transport,
    worker::Worker,
};
use femto_reader::{
    controller::{Navigating, Reader},
    layout::Locator,
    state::{ReaderState, SyncConflict, Synced},
    ui::{KosyncEvent, ReaderUi},
};
use std::{fs::File, io, sync::Arc, time::Duration};

pub type TransportFactory = Arc<dyn Fn(&[String]) -> io::Result<Box<dyn Transport>> + Send + Sync>;
struct Origin {
    book: String,
    position: Locator,
}
impl Origin {
    fn current(reader: &Reader) -> Self {
        Self {
            book: reader.document.metadata.id.clone(),
            position: reader.position(),
        }
    }
    fn check(&self, reader: &Reader) -> femto_reader::Result<()> {
        if self.book != reader.document.metadata.id || self.position != reader.position() {
            return Err(femto_reader::Error::Document(
                "reading position changed during sync".into(),
            ));
        }
        Ok(())
    }
}
/// Who wrote the server's record of a book, and when.
type Record = (String, u64);
enum Output {
    Message(&'static str),
    /// Nothing differs.
    Level,
    /// This reader's position reached the server.
    Pushed {
        origin: Origin,
        timestamp: u64,
    },
    /// The server holds another device's position, at `percent` of the
    /// book. With `ask`, this reader has moved as well, and which position
    /// to keep is the user's to say.
    Remote {
        target: Locator,
        record: Record,
        percent: u8,
        ask: bool,
        origin: Origin,
        budget: Budget,
    },
}
#[derive(Clone, Copy)]
enum Task {
    Auth,
    Register,
    Push,
    Pull,
    /// Compares this reader with the server and brings them level.
    /// `leaving` when the reader is about to sleep, and so can upload its
    /// position but neither move nor ask.
    Reconcile {
        leaving: bool,
    },
}
/// What the book needs to become level with the server.
enum Step {
    Nothing,
    Push,
    Take,
    Ask,
}
/// Decides from what has moved since the reader and the server last agreed.
/// A record this reader wrote is never news, whatever its time.
fn step(
    synced: Option<&Synced>,
    local: Locator,
    remote: Option<(&str, u64)>,
    own_device: &str,
    policy: SyncConflict,
    leaving: bool,
) -> Step {
    let local_moved = synced.is_none_or(|synced| synced.position != local);
    let remote_moved = remote.is_some_and(|record| {
        record.0 != own_device
            && synced.is_none_or(|synced| (synced.device_id.as_str(), synced.timestamp) != record)
    });
    match (remote_moved, local_moved) {
        (false, false) => Step::Nothing,
        (false, true) => Step::Push,
        // Left for when the book is next opened.
        (true, _) if leaving => Step::Nothing,
        (true, false) => Step::Take,
        (true, true) => match policy {
            SyncConflict::Ask => Step::Ask,
            SyncConflict::Remote => Step::Take,
            SyncConflict::Local => Step::Push,
        },
    }
}
struct Conflict {
    book: String,
    target: Locator,
    record: Record,
    budget: Budget,
}
pub struct Session {
    settings: Settings,
    jobs: Worker<Output>,
    pending: Option<u64>,
    navigation: Option<(Navigating, Budget, Record)>,
    /// The request in flight was not asked for from the sync menu, and
    /// reports nothing to it.
    quiet: bool,
    /// The position the user is being asked about, and once chosen.
    conflict: Option<Conflict>,
    accepted: Option<Conflict>,
    /// How the last request ended, for a log; the menu shows its own.
    outcome: Option<String>,
}
impl Session {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            jobs: Worker::default(),
            pending: None,
            navigation: None,
            quiet: false,
            conflict: None,
            accepted: None,
            outcome: None,
        }
    }
    pub fn is_busy(&self) -> bool {
        self.jobs.is_busy() || self.navigation.is_some() || self.accepted.is_some()
    }
    pub fn cancel_and_wait(&mut self, timeout: Duration) -> bool {
        self.pending = None;
        self.navigation = None;
        self.jobs.cancel_and_wait(timeout)
    }
    /// Whether this reader has moved since it last agreed with the server.
    pub fn has_unsent(&self, reader: &Reader) -> bool {
        reader
            .state
            .books
            .get(&reader.document.metadata.id)
            .and_then(|book| book.synced.as_ref())
            .is_none_or(|synced| synced.position != reader.position())
    }
    /// Starts bringing the open book level with the server without being
    /// asked from the menu. False when nothing was started.
    pub fn reconcile(&mut self, reader: &Reader, factory: TransportFactory, leaving: bool) -> bool {
        if self.is_busy() || self.conflict.is_some() {
            return false;
        }
        let started = self
            .submit(Task::Reconcile { leaving }, reader, factory)
            .is_ok();
        self.quiet = started;
        started
    }
    pub fn event(
        &mut self,
        event: KosyncEvent,
        reader: &Reader,
        ui: &mut ReaderUi,
        factory: TransportFactory,
    ) -> Result<(), String> {
        let task = match event {
            KosyncEvent::Back => {
                self.pending = None;
                self.navigation = None;
                self.jobs.cancel();
                ui.close_kosync();
                return Ok(());
            }
            KosyncEvent::Open => {
                ui.set_kosync_view(
                    if self.is_busy() {
                        "Waiting for request to stop"
                    } else {
                        "Ready"
                    },
                    self.is_busy(),
                );
                return Ok(());
            }
            KosyncEvent::TakeRemote => {
                // Moving needs the reader; the next poll has it.
                self.accepted = self.conflict.take();
                self.quiet = true;
                return Ok(());
            }
            KosyncEvent::KeepLocal => {
                if self.conflict.take().is_none() {
                    return Ok(());
                }
                self.submit(Task::Push, reader, factory)?;
                self.quiet = true;
                return Ok(());
            }
            KosyncEvent::Push => Task::Push,
            KosyncEvent::Pull => Task::Pull,
            KosyncEvent::Auth => Task::Auth,
            KosyncEvent::Register => Task::Register,
        };
        if self.is_busy() {
            return Err("KOReader request is still running".into());
        }
        self.submit(task, reader, factory)?;
        self.quiet = false;
        ui.set_kosync_view("Connecting", true);
        Ok(())
    }
    fn submit(
        &mut self,
        task: Task,
        reader: &Reader,
        factory: TransportFactory,
    ) -> Result<(), String> {
        let origin = Origin::current(reader);
        let path = if matches!(task, Task::Auth | Task::Register) {
            None
        } else {
            Some(
                reader
                    .document
                    .source_path()
                    .ok_or("Sync requires a file-backed EPUB")?
                    .to_owned(),
            )
        };
        let synced = reader
            .state
            .books
            .get(&origin.book)
            .and_then(|book| book.synced.clone());
        let policy = reader.state.sync_conflict;
        let settings = self.settings.clone();
        let limit = Limit::SILENCE;
        let id = self
            .jobs
            .submit(limit, move |budget, cancelled| {
                let exchange = || -> Result<Output, crate::Error> {
                    let origins = if settings.route == Route::Tailnet {
                        vec![
                            url::Url::parse(settings.client.server())
                                .map_err(|_| crate::Error::Invalid("server URL"))?
                                .origin()
                                .ascii_serialization(),
                        ]
                    } else {
                        vec![]
                    };
                    let mut transport = factory(&origins)?;
                    match task {
                        Task::Auth => {
                            settings
                                .client
                                .authorize(&mut transport, limit, cancelled)?;
                            return Ok(Output::Message("Account verified"));
                        }
                        Task::Register => {
                            settings.client.register(&mut transport, limit, cancelled)?;
                            return Ok(Output::Message("Account created"));
                        }
                        _ => (),
                    }
                    let path = path.ok_or(crate::Error::Invalid("book path"))?;
                    let file = open_book(&path)?;
                    let mut book =
                        Book::from_file(&path, file, settings.checksum, limit, &cancelled)?;
                    if book.document.metadata.id != origin.book {
                        return Err(crate::Error::Invalid("book changed on disk"));
                    }
                    let remote = if matches!(task, Task::Push) {
                        None
                    } else {
                        settings
                            .client
                            .pull(&mut transport, &book.id, limit, cancelled.clone())?
                    };
                    let step = match task {
                        Task::Push => Step::Push,
                        Task::Pull if remote.is_none() => {
                            return Ok(Output::Message("No remote reading position"));
                        }
                        Task::Pull => Step::Take,
                        Task::Reconcile { leaving } => step(
                            synced.as_ref(),
                            origin.position,
                            remote
                                .as_ref()
                                .map(|r| (r.position.device_id.as_str(), r.timestamp)),
                            &settings.device_id,
                            policy,
                            leaving,
                        ),
                        Task::Auth | Task::Register => unreachable!(),
                    };
                    let take = |book: &mut Book| {
                        let remote = remote
                            .as_ref()
                            .expect("a step that takes a remote position");
                        let record = (remote.position.device_id.clone(), remote.timestamp);
                        book.resolve_remote(remote).map(|target| (target, record))
                    };
                    match step {
                        Step::Nothing => Ok(Output::Level),
                        Step::Push => {
                            let mut state = ReaderState::default();
                            state.set_position(&origin.book, origin.position, 0);
                            let progress = book.prepare_push(&state, &settings)?;
                            let receipt = settings.client.push(
                                &mut transport,
                                &progress,
                                limit,
                                cancelled,
                            )?;
                            Ok(Output::Pushed {
                                origin,
                                timestamp: receipt.timestamp,
                            })
                        }
                        Step::Take | Step::Ask => {
                            let percent = remote.as_ref().map_or(0.0, |r| r.position.percentage);
                            let (target, record) = take(&mut book)?;
                            Ok(Output::Remote {
                                target,
                                record,
                                percent: (percent * 100.0) as u8,
                                ask: matches!(step, Step::Ask),
                                origin,
                                budget,
                            })
                        }
                    }
                };
                exchange().map_err(|error| io::Error::other(error).into())
            })
            .map_err(|error| error.to_string())?;
        self.pending = Some(id);
        Ok(())
    }
    /// How the last finished request ended, once.
    pub fn take_outcome(&mut self) -> Option<String> {
        self.outcome.take()
    }
    /// Reports to the sync menu, unless the request did not come from it.
    fn report(&mut self, ui: &mut ReaderUi, status: &str, busy: bool) {
        if !busy {
            self.outcome = Some(status.into());
        }
        if !self.quiet {
            ui.set_kosync_view(status, busy);
        }
    }
    /// Indexes at most one destination page per poll. Publication uses current
    /// annotations and settings, never the worker's earlier snapshot.
    pub fn poll(
        &mut self,
        reader: &mut Reader,
        ui: &mut ReaderUi,
        now: u64,
        mut save: impl FnMut(&ReaderState) -> femto_reader::Result<()>,
    ) -> bool {
        if !self.quiet && !ui.is_kosync_open() {
            self.pending = None;
            self.navigation = None;
            self.jobs.cancel();
        }
        let mut changed = false;
        if let Some(done) = self.jobs.poll() {
            if self.pending.take() != Some(done.id) {
                if !self.quiet && ui.is_kosync_open() {
                    ui.set_kosync_view("Ready", false);
                    return true;
                }
                return false;
            }
            changed = !self.quiet;
            match done.result {
                Ok(Output::Message(message)) => self.report(ui, message, false),
                Ok(Output::Level) => self.outcome = Some("Nothing to exchange".into()),
                Ok(Output::Pushed { origin, timestamp }) => {
                    // What was sent is what agrees, wherever the reader is now.
                    if let Some(book) = reader.state.books.get_mut(&origin.book) {
                        book.synced = Some(Synced {
                            position: origin.position,
                            device_id: self.settings.device_id.clone(),
                            timestamp,
                        });
                        // Kept at once: a reader that forgot it would take
                        // its own upload for a position it has yet to send.
                        let _ = save(&reader.state);
                    }
                    self.report(ui, "Reading position uploaded", false);
                }
                Ok(Output::Remote {
                    target,
                    record,
                    percent,
                    ask,
                    origin,
                    budget,
                }) => {
                    let same_book = origin.book == reader.document.metadata.id;
                    let moved = origin.check(reader).is_err();
                    // A reader that moved while the answer was on its way has
                    // moved as well as the other device.
                    let policy = reader.state.sync_conflict;
                    let ask = self.quiet && (ask || (moved && policy == SyncConflict::Ask));
                    let take = if self.quiet {
                        same_book && !(moved && policy == SyncConflict::Local)
                    } else {
                        !moved
                    };
                    if ask {
                        // Not over a menu the user is in; it is asked when
                        // the book is next opened.
                        if same_book && !ui.is_open() {
                            self.conflict = Some(Conflict {
                                book: origin.book,
                                target,
                                record,
                                budget,
                            });
                            ui.ask_sync_conflict(percent, reader.book_progress_percent());
                            self.outcome = Some("Asked which position to keep".into());
                            changed = true;
                        }
                    } else if let Some(job) =
                        take.then(|| reader.begin_location(target).ok()).flatten()
                    {
                        self.navigation = Some((job, budget, record));
                        self.report(ui, "Preparing reading position", true);
                    } else {
                        self.report(ui, "Position changed; download again", false);
                    }
                }
                Err(error) => {
                    self.report(ui, "Request failed; position unchanged", false);
                    self.outcome = Some(format!("Request failed: {error}"));
                    if !self.quiet {
                        ui.show_error(error.to_string());
                    }
                }
            }
        }
        if let Some(conflict) = self.accepted.take()
            && conflict.book == reader.document.metadata.id
            && let Ok(job) = reader.begin_location(conflict.target)
        {
            self.navigation = Some((job, conflict.budget, conflict.record));
        }
        if let Some((mut job, budget, record)) = self.navigation.take() {
            match budget
                .check()
                .map_err(femto_reader::Error::from)
                .and_then(|()| reader.step_navigation(&mut job))
            {
                Ok(false) => self.navigation = Some((job, budget, record)),
                Ok(true) => {
                    let result = reader.finish_navigation_saved(job, now, |state| {
                        budget.check()?;
                        save(state)
                    });
                    if result.is_ok() {
                        let position = reader.position();
                        if let Some(book) = reader.state.books.get_mut(&reader.document.metadata.id)
                        {
                            book.synced = Some(Synced {
                                position,
                                device_id: record.0,
                                timestamp: record.1,
                            });
                        }
                        let _ = save(&reader.state);
                    }
                    self.report(
                        ui,
                        if result.is_ok() {
                            "Reading position downloaded"
                        } else {
                            "Cannot save; position unchanged"
                        },
                        false,
                    );
                    changed = true;
                }
                Err(_) => {
                    self.report(ui, "Download cancelled or stale", false);
                    changed = true;
                }
            }
        }
        changed
    }
}
fn open_book(path: &std::path::Path) -> io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(all(unix, not(target_os = "espidf")))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_follows_from_which_side_has_moved_since_the_last_agreement() {
        let here = Locator::default();
        let there = Locator { block: 7, ..here };
        let synced = Synced {
            position: here,
            device_id: "phone".into(),
            timestamp: 10,
        };
        let step = |synced: Option<&Synced>, local, remote, policy, leaving| {
            std::mem::discriminant(&step(synced, local, remote, "reader", policy, leaving))
        };
        let is = |step: Step| std::mem::discriminant(&step);
        use SyncConflict::{Ask, Local, Remote};
        let same = Some(("phone", 10));
        let newer = Some(("phone", 11));
        // Nothing moved; only this reader; only the other device.
        assert!(step(Some(&synced), here, same, Ask, false) == is(Step::Nothing));
        assert!(step(Some(&synced), there, same, Ask, false) == is(Step::Push));
        assert!(step(Some(&synced), here, newer, Ask, false) == is(Step::Take));
        // Both moved: the setting decides.
        assert!(step(Some(&synced), there, newer, Ask, false) == is(Step::Ask));
        assert!(step(Some(&synced), there, newer, Remote, false) == is(Step::Take));
        assert!(step(Some(&synced), there, newer, Local, false) == is(Step::Push));
        // A record this reader wrote is not news, even with a new time.
        assert!(step(Some(&synced), there, Some(("reader", 99)), Ask, false) == is(Step::Push));
        // Before sleep the reader uploads, but neither moves nor asks.
        assert!(step(Some(&synced), there, same, Ask, true) == is(Step::Push));
        assert!(step(Some(&synced), here, newer, Ask, true) == is(Step::Nothing));
        assert!(step(Some(&synced), there, newer, Local, true) == is(Step::Nothing));
        // A book never synced has moved; a server without it takes it.
        assert!(step(None, here, None, Ask, false) == is(Step::Push));
        assert!(step(None, here, newer, Ask, false) == is(Step::Ask));
    }

    /// Every combination of a small domain against the table of
    /// docs/koreader-sync.md, with "moved" worked out from the prose there
    /// rather than from `step`.
    #[test]
    fn every_small_case_follows_the_documented_table() {
        let places = [
            Locator::default(),
            Locator {
                block: 7,
                ..Locator::default()
            },
        ];
        let writers = ["reader", "phone", "tablet", ""];
        let times = [10, 11];
        let mut checkpoints = vec![None];
        let mut records = vec![None];
        for writer in writers {
            for time in times {
                records.push(Some((writer, time)));
                for position in places {
                    checkpoints.push(Some(Synced {
                        position,
                        device_id: writer.into(),
                        timestamp: time,
                    }));
                }
            }
        }
        let mut seen = [0usize; 4];
        for synced in &checkpoints {
            for local in places {
                for remote in &records {
                    for policy in [SyncConflict::Ask, SyncConflict::Remote, SyncConflict::Local] {
                        for leaving in [false, true] {
                            // "The reader has moved if its position differs
                            // from that one"; never synced counts as moved.
                            let reader = match synced {
                                None => true,
                                Some(synced) => synced.position != local,
                            };
                            // "The other side has moved if the server's
                            // record was written by another device and is
                            // not that record."
                            let other = match (remote, synced) {
                                (None, _) => false,
                                (Some((writer, _)), _) if *writer == "reader" => false,
                                (Some(_), None) => true,
                                (Some((writer, time)), Some(synced)) => {
                                    !(synced.device_id == *writer && synced.timestamp == *time)
                                }
                            };
                            let expected = match (reader, other, leaving) {
                                (false, false, _) => 0,
                                (true, false, _) => 1,
                                (_, true, true) => 0,
                                (false, true, false) => 2,
                                (true, true, false) => match policy {
                                    SyncConflict::Ask => 3,
                                    SyncConflict::Remote => 2,
                                    SyncConflict::Local => 1,
                                },
                            };
                            let actual = match step(
                                synced.as_ref(),
                                local,
                                *remote,
                                "reader",
                                policy,
                                leaving,
                            ) {
                                Step::Nothing => 0,
                                Step::Push => 1,
                                Step::Take => 2,
                                Step::Ask => 3,
                            };
                            assert_eq!(
                                actual, expected,
                                "{synced:?} {local:?} {remote:?} {policy:?} leaving={leaving}"
                            );
                            seen[expected] += 1;
                        }
                    }
                }
            }
        }
        assert!(seen.iter().all(|&count| count > 20), "{seen:?}");
    }
}
