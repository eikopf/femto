//! Records on the card for a reader with no console attached: faults in
//! `crash.log`, and battery and power figures in `power.log`. Each entry is
//! one line in the data directory.
use esp_idf_svc::sys;
use std::{
    fmt::Arguments,
    fs,
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};

const DIRECTORY: &str = "/sdcard/femto";
/// Past this size a log becomes the previous one and a new one starts.
const KEPT: u64 = 64 * 1024;

/// Appends an entry with the firmware version, the time since start and,
/// when the clock is set, the time of day. Fails silently: this runs while
/// the reader is already failing, and the card may be what failed.
fn append(name: &str, entry: Arguments) {
    let (log, previous) = (
        format!("{DIRECTORY}/{name}.log"),
        format!("{DIRECTORY}/{name}.old"),
    );
    if fs::metadata(&log).is_ok_and(|log| log.len() > KEPT) {
        // FAT rename does not replace an existing file.
        let _ = fs::remove_file(&previous);
        let _ = fs::rename(&log, previous);
    }
    let Ok(mut log) = fs::OpenOptions::new().create(true).append(true).open(log) else {
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

fn record(entry: Arguments) {
    append("crash", entry);
}

/// Battery and power figures over time, from which battery life can be
/// read without instruments: one line at start, every ten minutes awake,
/// and before sleep.
pub struct PowerLog {
    at: std::time::Instant,
    /// Microseconds since start, and of them in light sleep, at the last line.
    seen: (i64, u64),
}
impl PowerLog {
    const EVERY: std::time::Duration = std::time::Duration::from_secs(600);
    pub fn start(battery: Option<femto_picco_board::battery::Sample>) -> Self {
        let mut log = Self {
            at: std::time::Instant::now(),
            seen: (0, 0),
        };
        log.line("start", battery);
        log
    }
    pub fn poll(&mut self, battery: Option<femto_picco_board::battery::Sample>) {
        if self.at.elapsed() >= Self::EVERY {
            self.line("awake", battery);
        }
    }
    /// The share of the time since the last line that the processor spent
    /// in light sleep is the figure that says what reading costs.
    pub fn line(&mut self, event: &str, battery: Option<femto_picco_board::battery::Sample>) {
        let now = (
            unsafe { sys::esp_timer_get_time() },
            crate::power::light_sleep_micros().unwrap_or(0),
        );
        let (elapsed, slept) = (now.0 - self.seen.0, now.1.saturating_sub(self.seen.1));
        self.seen = now;
        self.at = std::time::Instant::now();
        let asleep = if elapsed > 0 {
            100 * slept / elapsed as u64
        } else {
            0
        };
        match battery {
            Some(battery) => append(
                "power",
                format_args!(
                    "{event}, battery {} mV {}% {:?}, light sleep {asleep}%",
                    battery.millivolts.unwrap_or(0),
                    battery.percentage.unwrap_or(0),
                    battery.power,
                ),
            ),
            None => append(
                "power",
                format_args!("{event}, battery unread, light sleep {asleep}%"),
            ),
        }
    }
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
