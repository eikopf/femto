use esp_idf_svc::sntp::{EspSntp, SntpConf};
use femto_picco_board::clock::{Gate, valid_server};
use std::{
    fs::File,
    io::{self, Read},
    sync::{Mutex, OnceLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

static GATE: Mutex<Gate> = Mutex::new(Gate::new());
static START: OnceLock<Instant> = OnceLock::new();
fn monotonic_ms() -> u64 {
    START
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
pub fn ready() -> bool {
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        clear();
        return false;
    };
    GATE.lock()
        .map(|mut gate| gate.ready(monotonic_ms(), now.as_secs()))
        .unwrap_or(false)
}
fn clear() {
    if let Ok(mut gate) = GATE.lock() {
        gate.clear();
    }
}
pub fn require_ready() -> io::Result<()> {
    if ready() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "Clock not synchronized; retry HTTPS after time sync",
        ))
    }
}
pub struct Service {
    sntp: Option<EspSntp<'static>>,
    /// When the server was last asked, and how long to wait for its answer.
    asked: Instant,
    patience: std::time::Duration,
}
impl Service {
    pub fn start() -> Result<Self, String> {
        let server = match File::open("/sdcard/femto/time-server") {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(257)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "Cannot read time server")?;
                if bytes.len() > 256 {
                    return Err("Time server file exceeds 256 bytes".into());
                }
                let server = std::str::from_utf8(&bytes)
                    .map_err(|_| "Invalid time server")?
                    .trim();
                if !valid_server(server) {
                    return Err("Invalid time server".into());
                }
                server.to_owned()
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => "0.pool.ntp.org".into(),
            Err(_) => return Err("Cannot open time server".into()),
        };
        clear();
        let mut config = SntpConf::default();
        config.servers.fill("");
        config.servers[0] = &server;
        let sntp = EspSntp::new_with_callback(&config, |sample| {
            if let Ok(mut gate) = GATE.lock() {
                gate.synchronized(monotonic_ms(), sample.as_secs());
            }
            log::info!("Clock set from the time server");
        })
        .map_err(|e| format!("Time service initialization: {e}"))?;
        Ok(Self {
            sntp: Some(sntp),
            asked: Instant::now(),
            patience: FIRST_PATIENCE,
        })
    }
    /// Asks again when the answer is overdue. An answer takes well under a
    /// second, and one lost packet is common on a weak link, but the
    /// platform client waits fifteen seconds before it repeats a query, and
    /// until then nothing that needs the time can start. The wait doubles, so
    /// a silent server is asked five times in its first minute.
    pub fn poll(&mut self) {
        if ready() || self.asked.elapsed() < self.patience {
            return;
        }
        unsafe { esp_idf_svc::sys::sntp_restart() };
        self.asked = Instant::now();
        self.patience *= 2;
    }
}
const FIRST_PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);
impl Drop for Service {
    fn drop(&mut self) {
        // Unsubscribe first so an in-flight callback cannot restore readiness.
        self.sntp.take();
        clear();
    }
}
