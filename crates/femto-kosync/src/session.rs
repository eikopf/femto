//! Explicit reader actions. Workers never write reader state.
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
    state::ReaderState,
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
enum Output {
    Message(&'static str),
    Position {
        target: Locator,
        origin: Origin,
        budget: Budget,
    },
}
pub struct Session {
    settings: Settings,
    jobs: Worker<Output>,
    pending: Option<u64>,
    navigation: Option<(Navigating, Budget)>,
}
impl Session {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            jobs: Worker::default(),
            pending: None,
            navigation: None,
        }
    }
    pub fn is_busy(&self) -> bool {
        self.jobs.is_busy() || self.navigation.is_some()
    }
    pub fn cancel_and_wait(&mut self, timeout: Duration) -> bool {
        self.pending = None;
        self.navigation = None;
        self.jobs.cancel_and_wait(timeout)
    }
    pub fn event(
        &mut self,
        event: KosyncEvent,
        reader: &Reader,
        ui: &mut ReaderUi,
        factory: TransportFactory,
    ) -> Result<(), String> {
        match event {
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
            _ => (),
        }
        if self.is_busy() {
            return Err("KOReader request is still running".into());
        }
        let origin = Origin::current(reader);
        let path = if matches!(event, KosyncEvent::Push | KosyncEvent::Pull) {
            Some(
                reader
                    .document
                    .source_path()
                    .ok_or("Sync requires a file-backed EPUB")?
                    .to_owned(),
            )
        } else {
            None
        };
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
                    match event {
                        KosyncEvent::Auth => {
                            settings
                                .client
                                .authorize(&mut transport, limit, cancelled)?;
                            Ok(Output::Message("Account verified"))
                        }
                        KosyncEvent::Register => {
                            settings.client.register(&mut transport, limit, cancelled)?;
                            Ok(Output::Message("Account created"))
                        }
                        KosyncEvent::Push | KosyncEvent::Pull => {
                            let path = path.ok_or(crate::Error::Invalid("book path"))?;
                            let file = open_book(&path)?;
                            let mut book =
                                Book::from_file(&path, file, settings.checksum, limit, &cancelled)?;
                            if book.document.metadata.id != origin.book {
                                return Err(crate::Error::Invalid("book changed on disk"));
                            }
                            if matches!(event, KosyncEvent::Push) {
                                let mut state = ReaderState::default();
                                state.set_position(&origin.book, origin.position, 0);
                                let progress = book.prepare_push(&state, &settings)?;
                                settings.client.push(
                                    &mut transport,
                                    &progress,
                                    limit,
                                    cancelled,
                                )?;
                                Ok(Output::Message("Reading position uploaded"))
                            } else {
                                let Some(remote) = settings.client.pull(
                                    &mut transport,
                                    &book.id,
                                    limit,
                                    cancelled,
                                )?
                                else {
                                    return Ok(Output::Message("No remote reading position"));
                                };
                                let target = book.resolve_remote(&remote)?;
                                Ok(Output::Position {
                                    target,
                                    origin,
                                    budget,
                                })
                            }
                        }
                        _ => unreachable!(),
                    }
                };
                exchange().map_err(|error| io::Error::other(error).into())
            })
            .map_err(|error| error.to_string())?;
        self.pending = Some(id);
        ui.set_kosync_view("Connecting", true);
        Ok(())
    }
    /// Indexes at most one destination page per poll. Publication uses current
    /// annotations and settings, never the worker's earlier snapshot.
    pub fn poll(
        &mut self,
        reader: &mut Reader,
        ui: &mut ReaderUi,
        now: u64,
        save: impl FnOnce(&ReaderState) -> femto_reader::Result<()>,
    ) -> bool {
        if !ui.is_kosync_open() {
            self.pending = None;
            self.navigation = None;
            self.jobs.cancel();
        }
        let mut changed = false;
        if let Some(done) = self.jobs.poll() {
            if self.pending.take() != Some(done.id) {
                if ui.is_kosync_open() {
                    ui.set_kosync_view("Ready", false);
                    return true;
                }
                return false;
            }
            changed = true;
            match done.result {
                Ok(Output::Message(message)) => ui.set_kosync_view(message, false),
                Ok(Output::Position {
                    target,
                    origin,
                    budget,
                }) => {
                    match origin
                        .check(reader)
                        .and_then(|()| reader.begin_location(target))
                    {
                        Ok(job) => {
                            self.navigation = Some((job, budget));
                            ui.set_kosync_view("Preparing reading position", true);
                        }
                        Err(_) => ui.set_kosync_view("Position changed; download again", false),
                    }
                }
                Err(error) => {
                    ui.set_kosync_view("Request failed; position unchanged", false);
                    ui.show_error(error.to_string());
                }
            }
        }
        if let Some((mut job, budget)) = self.navigation.take() {
            match budget
                .check()
                .map_err(femto_reader::Error::from)
                .and_then(|()| reader.step_navigation(&mut job))
            {
                Ok(false) => self.navigation = Some((job, budget)),
                Ok(true) => {
                    let result = reader.finish_navigation_saved(job, now, |state| {
                        budget.check()?;
                        save(state)
                    });
                    ui.set_kosync_view(
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
                    ui.set_kosync_view("Download cancelled or stale", false);
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
