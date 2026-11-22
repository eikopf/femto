//! Firmware updates from the SD card.
//!
//! The stock recovery application in the factory slot installs
//! `/sdcard/update/firmware.bin` into the single application slot when it
//! finds the pending marker beside it. It validates the image, keeps the
//! marker until installation succeeds and then boots the application, so an
//! interrupted installation resumes at the next start. Femto only stages the
//! package and restarts into recovery.
use esp_idf_svc::sys;
use std::{fs, io::Read, path::Path};

/// Where a USB push leaves a package; the console writes below the data directory.
const PUSHED: &str = "/sdcard/femto/firmware.bin";
const PACKAGE: &str = "/sdcard/update/firmware.bin";
const MARKER: &str = "/sdcard/update/.ota_pending";
/// Image and segment headers precede the application description.
const DESCRIPTION: usize = 32;
const VERSION_PREFIX: &str = "femto-";

fn text(field: &[u8]) -> &str {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    core::str::from_utf8(&field[..end]).unwrap_or("")
}

/// The version of a femto application image, read from its description.
fn version(path: &Path) -> Option<String> {
    let mut head = [0u8; DESCRIPTION + 48];
    fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    let description = &head[DESCRIPTION..];
    let valid = head[0] == sys::ESP_IMAGE_HEADER_MAGIC as u8
        && description[..4] == sys::ESP_APP_DESC_MAGIC_WORD.to_le_bytes();
    let version = text(&description[16..]);
    (valid && version.starts_with(VERSION_PREFIX)).then(|| version.into())
}

/// The version of the running application.
pub fn running() -> String {
    let description = unsafe { &*sys::esp_app_get_description() };
    let bytes = description.version.map(|c| c as u8);
    text(&bytes).into()
}

/// The version of a package waiting on the card.
pub fn waiting() -> Option<String> {
    [PUSHED, PACKAGE]
        .iter()
        .find_map(|path| version(Path::new(path)))
}

/// Hands the waiting package to recovery and restarts into it.
/// Returns only when that could not be arranged; the application is untouched.
pub fn install() -> String {
    let staged = (|| -> Result<(), String> {
        let package = Path::new(PACKAGE);
        if version(Path::new(PUSHED)).is_some() {
            fs::create_dir_all(package.parent().unwrap_or(package)).map_err(|e| e.to_string())?;
            // FAT rename does not replace an existing file.
            let _ = fs::remove_file(package);
            fs::rename(PUSHED, package).map_err(|e| e.to_string())?;
        }
        version(package).ok_or("No femto update on the card")?;
        fs::File::create(MARKER)
            .and_then(|marker| marker.sync_all())
            .map_err(|e| e.to_string())?;
        let recovery = unsafe {
            sys::esp_partition_find_first(
                sys::esp_partition_type_t_ESP_PARTITION_TYPE_APP,
                sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_FACTORY,
                core::ptr::null(),
            )
        };
        if recovery.is_null() {
            return Err("No recovery application on this device".into());
        }
        // Verifies the recovery image before selecting it.
        sys::esp!(unsafe { sys::esp_ota_set_boot_partition(recovery) }).map_err(|e| e.to_string())
    })();
    match staged {
        Ok(()) => unsafe { sys::esp_restart() },
        Err(error) => {
            let _ = fs::remove_file(MARKER);
            error
        }
    }
}
