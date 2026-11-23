use esp_idf_svc::sys;
use femto_picco_board::console::{CHUNK_BYTES, Command, Parser, crc32};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

/// File commands are confined to the device data directory.
const ROOT: &str = "/sdcard/femto";
const CHUNK_TIMEOUT_MS: u32 = 5000;
/// How often an unanswered acknowledgement is repeated.
const ACK_REPEAT_MS: u32 = 1000;

pub struct Console {
    parser: Parser,
    enabled: bool,
}

impl Console {
    pub fn open() -> Self {
        let driver_ready = unsafe {
            if sys::usb_serial_jtag_is_driver_installed() {
                true
            } else {
                let mut config = sys::usb_serial_jtag_driver_config_t {
                    tx_buffer_size: 256,
                    // Holds one upload chunk between acknowledgements.
                    rx_buffer_size: 2 * CHUNK_BYTES as u32,
                };
                sys::usb_serial_jtag_driver_install(&mut config) == sys::ESP_OK
            }
        };
        // ESP-IDF 5.5.2 nonblocking VFS reads inspect the driver's RX ring.
        // The default polling console has no ring and always reports no data.
        if driver_ready {
            unsafe { sys::esp_vfs_usb_serial_jtag_use_driver() };
        }
        let enabled = unsafe {
            let flags = sys::fcntl(0, sys::F_GETFL as _);
            driver_ready
                && flags >= 0
                && sys::fcntl(0, sys::F_SETFL as _, flags | sys::O_NONBLOCK as i32) == 0
        };
        log::info!("USB console enabled={enabled}");
        Self {
            parser: Parser::default(),
            enabled,
        }
    }

    pub fn poll(&mut self) -> Option<Command> {
        if !self.enabled {
            return None;
        }
        // Bound work per reader tick and retain unread commands in the USB FIFO.
        for _ in 0..32 {
            let mut byte = 0u8;
            let count = unsafe { sys::read(0, (&mut byte as *mut u8).cast(), 1) };
            if count != 1 {
                return None;
            }
            if let Some(command) = self.parser.push(byte) {
                return Some(command);
            }
        }
        None
    }

    /// Runs a file command to completion and reports the outcome on the console.
    pub fn files(&mut self, command: &Command) {
        let result = match command {
            Command::Put { len, crc, path } => put(*len, *crc, &target(path)),
            Command::List(path) => list(&target(path)),
            Command::Show(path) => show(&target(path)),
            Command::Remove(path) => fs::remove_file(target(path)).map_err(|e| e.to_string()),
            _ => return,
        };
        match result {
            Ok(()) => log::info!("USB file: done"),
            Err(error) => log::warn!("USB file: error {error}"),
        }
    }
}

fn target(path: &str) -> PathBuf {
    Path::new(ROOT).join(path)
}

fn list(directory: &Path) -> Result<(), String> {
    for entry in fs::read_dir(directory)
        .map_err(|e| e.to_string())?
        .take(256)
    {
        let entry = entry.map_err(|e| e.to_string())?;
        let metadata = entry.metadata().map_err(|e| e.to_string())?;
        log::info!(
            "USB ls: {} {} {}",
            if metadata.is_dir() { 'd' } else { 'f' },
            metadata.len(),
            entry.file_name().to_string_lossy()
        );
    }
    Ok(())
}

/// Prints the first lines of a text file, such as the crash record. Lines
/// are cut to what a log line carries.
fn show(file: &Path) -> Result<(), String> {
    use std::io::{BufRead, Read};
    const SHOWN: u64 = 16 * 1024;
    let file = fs::File::open(file).map_err(|e| e.to_string())?;
    for line in std::io::BufReader::new(file.take(SHOWN)).split(b'\n') {
        let line = line.map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&line[..line.len().min(200)]);
        log::info!("USB cat: {}", text.trim_end());
    }
    Ok(())
}

/// Stop-and-wait transfer: the driver drops bytes when its ring overflows,
/// so the host sends one chunk per acknowledgement. Log lines from other
/// threads can break an acknowledgement apart on the wire, so it is
/// repeated while the chunk it asks for has not begun to arrive. A broken
/// one can only name an earlier offset, which the host ignores.
fn put(len: u32, crc: u32, path: &Path) -> Result<(), String> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    let received = (|| {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut file = fs::File::create(&partial).map_err(|e| e.to_string())?;
        let mut chunk = [0u8; CHUNK_BYTES];
        let (mut total, mut sum) = (0u32, 0u32);
        while total < len {
            let wanted = ((len - total) as usize).min(CHUNK_BYTES);
            let mut filled = 0;
            let started = std::time::Instant::now();
            let mut asked = None;
            while filled < wanted {
                let waited = started.elapsed().as_millis() as u32;
                if waited > CHUNK_TIMEOUT_MS {
                    return Err(format!("timeout after {} bytes", total as usize + filled));
                }
                if filled == 0 && asked != Some(waited / ACK_REPEAT_MS) {
                    asked = Some(waited / ACK_REPEAT_MS);
                    log::info!("USB put: ack {total}");
                }
                filled += unsafe {
                    sys::usb_serial_jtag_read_bytes(
                        chunk[filled..].as_mut_ptr().cast(),
                        (wanted - filled) as u32,
                        10,
                    )
                } as usize;
            }
            file.write_all(&chunk[..wanted])
                .map_err(|e| e.to_string())?;
            sum = crc32(sum, &chunk[..wanted]);
            total += wanted as u32;
        }
        if sum != crc {
            return Err(format!("checksum {sum:08x} does not match"));
        }
        file.sync_all().map_err(|e| e.to_string())
    })();
    let published = received.and_then(|()| {
        // FAT rename does not replace an existing file.
        let _ = fs::remove_file(path);
        fs::rename(&partial, path).map_err(|e| e.to_string())
    });
    if published.is_err() {
        let _ = fs::remove_file(&partial);
    }
    published
}

/// Writes the framebuffer as hexadecimal lines for host-side inspection.
pub fn dump_frame(plane: &[u8]) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "USB frame: begin {}", plane.len());
    for row in plane.chunks(64) {
        let mut line = String::with_capacity(2 * row.len());
        for byte in row {
            line.push_str(&format!("{byte:02x}"));
        }
        let _ = writeln!(out, "USB frame: {line}");
    }
    let _ = writeln!(out, "USB frame: end");
}
