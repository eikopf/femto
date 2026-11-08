#[path = "progress_service.rs"]
mod service;
use femto_reader::{controller::Reader, journal::StateJournal, ui::ReaderUi};
use femto_sync::reader::{Commit, Conflict};
use service::preview;
pub use service::{Jobs, Service};

pub fn poll(
    jobs: &mut Jobs,
    reader: &mut Reader,
    journal: &mut Option<StateJournal>,
    ui: &mut ReaderUi,
    conflict: &mut Option<Conflict>,
) -> bool {
    let Some(completed) = jobs.poll() else {
        return false;
    };
    *conflict = None;
    ui.set_sync_conflict(None, None);
    let result = completed
        .result
        .map_err(|e| e.to_string())
        .and_then(|r| r)
        .and_then(|completion| {
            let store = journal.as_mut().ok_or("Sync requires writable state")?;
            let now = reader
                .state
                .books
                .values()
                .map(|b| b.last_opened)
                .max()
                .unwrap_or(0)
                .saturating_add(1);
            let mut save_failed = false;
            let result = completion.commit(reader, now, |state| {
                let saved = store.save(state);
                save_failed = saved.is_err();
                saved
            });
            if save_failed {
                *journal = None;
                ui.set_sleep_available(false);
            }
            result.map_err(|e| e.to_string())
        });
    match result {
        Ok(Commit::Saved { .. }) => ui.set_sync_status("Progress synchronized"),
        Ok(Commit::Empty) => ui.set_sync_status("No progress to sync"),
        Ok(Commit::Conflict { conflict: pending }) => {
            ui.set_sync_conflict(
                Some(preview(&mut reader.document, pending.local())),
                pending.remote().map(|p| preview(&mut reader.document, p)),
            );
            *conflict = Some(*pending);
            ui.set_sync_status("Progress conflict");
            if ui.is_sync_open() {
                ui.show_error("Progress differs; no update".into());
            }
        }
        Err(error) => {
            ui.set_sync_status("Sync failed or cancelled");
            if ui.is_sync_open() {
                ui.show_error(error);
            }
        }
    }
    // A successful download can change the visible page even outside the sync menu.
    true
}

pub fn reset(
    reset: femto_sync::reader::Reset,
    jobs: &Jobs,
    reader: &mut Reader,
    journal: &mut Option<StateJournal>,
) -> Result<(), String> {
    if jobs.is_busy() {
        return Err("Cancel sync and wait before resetting".into());
    }
    let store = journal.as_mut().ok_or("Reset requires writable state")?;
    let mut save_failed = false;
    let result = reset.commit(reader, |state| {
        let saved = store.save(state);
        save_failed = saved.is_err();
        saved
    });
    if save_failed {
        *journal = None;
    }
    result.map_err(|e| e.to_string())
}
