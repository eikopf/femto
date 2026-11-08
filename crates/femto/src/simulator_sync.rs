//! Simulator state-file owner for the shared progress worker and menu events.
#[path = "../../../firmware/picco/src/progress_service.rs"]
mod service;
use femto_reader::{
    controller::Reader,
    state::ReaderState,
    ui::{ReaderUi, SyncEvent},
};
use femto_sync::{
    Choice,
    reader::{Commit, Conflict, Reset},
};
use std::path::{Path, PathBuf};

pub struct Sync {
    service: service::Service,
    jobs: service::Jobs,
    conflict: Option<Conflict>,
    reset: Option<Reset>,
    initial: (String, PathBuf),
    state: PathBuf,
    save_failed: bool,
    transport: femto_catalog::tailnet::Factory,
}
impl Sync {
    pub fn set_transport(&mut self, transport: femto_catalog::tailnet::Factory) {
        self.transport = transport;
    }
    pub fn load(
        root: &Path,
        book: PathBuf,
        reader: &Reader,
        state: PathBuf,
        transport: femto_catalog::tailnet::Factory,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            service: service::Service::load(root)?
                .ok_or_else(|| anyhow::anyhow!("sync directory has no progress.url"))?,
            jobs: Default::default(),
            conflict: None,
            reset: None,
            initial: (reader.document.metadata.id.clone(), book),
            state,
            save_failed: false,
            transport,
        })
    }
    fn persist(path: &Path, failed: &mut bool, state: &ReaderState) -> femto_reader::Result<()> {
        let result = state.save(path);
        *failed |= result.is_err();
        result
    }
    fn event(
        &mut self,
        event: SyncEvent,
        reader: &mut Reader,
        ui: &mut ReaderUi,
    ) -> Result<(), String> {
        match event {
            SyncEvent::Open => {
                self.reset = None;
                if self
                    .conflict
                    .as_ref()
                    .is_some_and(|c| c.local().book_id != reader.document.metadata.id)
                {
                    self.conflict = None;
                    ui.set_sync_conflict(None, None);
                    ui.set_sync_status("Ready");
                }
                ui.open_sync();
            }
            SyncEvent::Cancel => {
                self.jobs.cancel();
                ui.set_sync_status(if self.jobs.is_busy() {
                    "Cancelling sync"
                } else {
                    "No sync running"
                });
            }
            SyncEvent::Reset => {
                if self.jobs.is_busy() {
                    return Err("Cancel sync and wait before resetting".into());
                }
                self.reset = Reset::prepare(reader);
                if self.reset.is_some() {
                    ui.open_sync_reset();
                } else {
                    ui.set_sync_status("No saved sync history");
                }
            }
            SyncEvent::CancelReset => {
                self.reset = None;
                ui.open_sync();
            }
            SyncEvent::ConfirmReset => {
                if self.jobs.is_busy() {
                    return Err("Cancel sync and wait before resetting".into());
                }
                let reset = self.reset.take().ok_or("Reset prompt expired")?;
                reset
                    .commit(reader, |state| {
                        Self::persist(&self.state, &mut self.save_failed, state)
                    })
                    .map_err(|e| e.to_string())?;
                self.conflict = None;
                ui.set_sync_conflict(None, None);
                ui.open_sync();
                ui.set_sync_status("Sync history forgotten");
            }
            SyncEvent::Start | SyncEvent::UseLocal | SyncEvent::UseRemote => {
                if self.jobs.is_busy() {
                    return Err("Sync already running".into());
                }
                let path = ui
                    .book_path(&reader.document.metadata.id)
                    .or_else(|| {
                        (self.initial.0 == reader.document.metadata.id)
                            .then(|| self.initial.1.clone())
                    })
                    .ok_or("Book missing from library; rescan")?;
                let resolution = match event {
                    SyncEvent::UseLocal | SyncEvent::UseRemote => Some((
                        self.conflict.take().ok_or("Conflict expired; sync again")?,
                        if event == SyncEvent::UseLocal {
                            Choice::Local
                        } else {
                            Choice::Remote
                        },
                    )),
                    _ => {
                        self.conflict = None;
                        None
                    }
                };
                ui.set_sync_conflict(None, None);
                let tailnet = self.service.tailnet;
                let transport = self.transport.clone();
                self.service
                    .start(reader, path, &mut self.jobs, resolution, move || {
                        if tailnet {
                            return transport.transport();
                        }
                        Ok(Box::new(super::host_http::HostTransport { endpoint: None }))
                    })
                    .map_err(|e| e.to_string())?;
                ui.set_sync_status("Synchronizing progress");
            }
        }
        Ok(())
    }
    pub fn poll(&mut self, reader: &mut Reader, ui: &mut ReaderUi) -> anyhow::Result<bool> {
        let mut changed = false;
        if let Some(event) = ui.take_sync_event() {
            if let Err(message) = self.event(event, reader, ui) {
                ui.show_error(message);
            }
            changed = true;
        }
        if let Some(completed) = self.jobs.poll() {
            self.conflict = None;
            ui.set_sync_conflict(None, None);
            let now = reader
                .state
                .books
                .values()
                .map(|b| b.last_opened)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            let result = completed
                .result
                .map_err(|e| e.to_string())
                .and_then(|r| r)
                .and_then(|completion| {
                    completion
                        .commit(reader, now, |state| {
                            Self::persist(&self.state, &mut self.save_failed, state)
                        })
                        .map_err(|e| e.to_string())
                });
            match result {
                Ok(Commit::Saved { .. }) => ui.set_sync_status("Progress synchronized"),
                Ok(Commit::Empty) => ui.set_sync_status("No progress to sync"),
                Ok(Commit::Conflict { conflict }) => {
                    ui.set_sync_conflict(
                        Some(service::preview(&mut reader.document, conflict.local())),
                        conflict
                            .remote()
                            .map(|p| service::preview(&mut reader.document, p)),
                    );
                    self.conflict = Some(*conflict);
                    ui.set_sync_status("Progress conflict");
                }
                Err(message) => {
                    ui.set_sync_status("Sync failed or cancelled");
                    if ui.is_sync_open() {
                        ui.show_error(message);
                    }
                }
            }
            changed = true;
        }
        anyhow::ensure!(
            !self.save_failed,
            "Sync state save failed; reopen the state file before continuing"
        );
        Ok(changed)
    }
}
