//! Device settings in the internal NVS partition, under femto's own namespace.
//!
//! Credentials kept here do not leave the device with the SD card. The
//! partition is not encrypted: flash encryption is disabled on the Picco, so
//! a full flash read over USB still reveals them.
use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition};
use std::sync::OnceLock;
use zeroize::Zeroizing;

const NAMESPACE: &str = "femto_cfg";
const MAX_VALUE_BYTES: usize = 4000;

/// The default partition can be taken once per process; every user shares
/// this handle. Corrupt, full or newer storage is refused rather than
/// erased, because the partition also holds vendor data.
pub fn partition() -> Result<EspDefaultNvsPartition, String> {
    static PARTITION: OnceLock<Result<EspDefaultNvsPartition, String>> = OnceLock::new();
    PARTITION
        .get_or_init(|| EspDefaultNvsPartition::take_with(false).map_err(|e| e.to_string()))
        .clone()
}

fn open(write: bool) -> Result<EspDefaultNvs, String> {
    EspDefaultNvs::new(partition()?, NAMESPACE, write).map_err(|e| e.to_string())
}

/// Reads a setting. A missing namespace or key is `None`.
pub fn get(key: &str) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    // A read-only open fails while the namespace does not exist yet.
    let Ok(nvs) = open(false) else {
        return Ok(None);
    };
    let Some(length) = nvs.blob_len(key).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    if length > MAX_VALUE_BYTES {
        return Err(format!("Setting {key} exceeds {MAX_VALUE_BYTES} bytes"));
    }
    let mut value = Zeroizing::new(vec![0; length]);
    let read = nvs
        .get_blob(key, &mut value)
        .map_err(|e| e.to_string())?
        .map(<[u8]>::len);
    if read != Some(length) {
        return Err(format!("Setting {key} changed during read"));
    }
    Ok(Some(value))
}

pub fn set(key: &str, value: &[u8]) -> Result<(), String> {
    if value.len() > MAX_VALUE_BYTES {
        return Err(format!("Setting {key} exceeds {MAX_VALUE_BYTES} bytes"));
    }
    open(true)?.set_blob(key, value).map_err(|e| e.to_string())
}
