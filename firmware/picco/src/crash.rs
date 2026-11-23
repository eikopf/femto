//! A record of faults on the card, for a reader that fails with no console
//! attached. Each entry is one line of `crash.log` in the data directory.
use esp_idf_svc::sys;
use std::{
    fmt::Arguments,
    fs,
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};

const LOG: &str = "/sdcard/femto/crash.log";
const PREVIOUS: &str = "/sdcard/femto/crash.old";
/// Past this size the log becomes the previous one and a new one starts.
const KEPT: u64 = 64 * 1024;

/// Appends an entry with the firmware version, the time since start and,
/// when the clock is set, the time of day. Fails silently: this runs while
/// the reader is already failing, and the card may be what failed.
fn record(entry: Arguments) {
    if fs::metadata(LOG).is_ok_and(|log| log.len() > KEPT) {
        // FAT rename does not replace an existing file.
        let _ = fs::remove_file(PREVIOUS);
        let _ = fs::rename(LOG, PREVIOUS);
    }
    let Ok(mut log) = fs::OpenOptions::new().create(true).append(true).open(LOG) else {
        return;
    };
    let up = unsafe { sys::esp_timer_get_time() } / 1_000_000;
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |time| time.as_secs());
    let _ = writeln!(
        log,
        "{} up {up} s, unix {}: {entry}",
        crate::update::running(),
        if crate::clock::ready() { unix } else { 0 },
    );
    let _ = log.sync_all();
}

/// Records every panic before it is reported as usual.
pub fn install() {
    let usual = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let thread = std::thread::current();
        record(format_args!(
            "panic in {}: {panic}",
            thread.name().unwrap_or("unnamed thread")
        ));
        usual(panic);
    }));
}

/// Records that the reader stopped with an error it could not handle.
pub fn stopped(error: &str) {
    record(format_args!("stopped: {error}"));
}

/// Records a start that followed a fault. Called once the card is mounted.
/// A fault the panic hook could not record, such as a watchdog or a memory
/// access, leaves only this.
pub fn note_restart() {
    let reason = match unsafe { sys::esp_reset_reason() } {
        sys::esp_reset_reason_t_ESP_RST_PANIC => "a panic or processor exception",
        sys::esp_reset_reason_t_ESP_RST_INT_WDT => "the interrupt watchdog",
        sys::esp_reset_reason_t_ESP_RST_TASK_WDT => "the task watchdog",
        sys::esp_reset_reason_t_ESP_RST_BROWNOUT => "a brownout",
        // Power-on, deep sleep, a requested restart, and the watchdog
        // reset the update tools use.
        _ => return,
    };
    log::warn!("Restarted after {reason}");
    record(format_args!("restarted after {reason}"));
}
