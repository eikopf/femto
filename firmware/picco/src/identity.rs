//! Explicit tailnet identity provisioning in the existing NVS partition.
use esp_idf_svc::{handle::RawHandle, nvs::EspDefaultNvs, sys};
use femto_microlink::{
    identity_store::{Backend, IdentityStore, RECORD_LEN},
    protocol::identity::DeviceIdentity,
    rotation::RECORD_LEN as ROTATION_LEN,
};
use zeroize::Zeroizing;

const NAMESPACE: &str = "femto_tailnet";
const KEY: &str = "identity";
pub type Store = IdentityStore<NvsBackend>;

pub struct NvsBackend {
    nvs: EspDefaultNvs,
}
impl NvsBackend {
    fn acquire(provision: bool) -> Result<Self, String> {
        // Refuse corrupt/full/newer NVS instead of erasing vendor storage.
        let partition = crate::nvs::partition()?;
        if !provision {
            // Read-only open requires an existing namespace and cannot create it.
            let existing = EspDefaultNvs::new(partition.clone(), NAMESPACE, false)
                .map_err(|e| e.to_string())?;
            if existing.blob_len(KEY).map_err(|e| e.to_string())?.is_none() {
                return Err("Tailnet identity missing; explicit provisioning required".into());
            }
            drop(existing);
        }
        let nvs = EspDefaultNvs::new(partition, NAMESPACE, true).map_err(|e| e.to_string())?;
        Ok(Self { nvs })
    }
}

/// Opens existing keys only. No automatic creation, migration, or repair.
pub fn open() -> Result<Store, String> {
    let backend = crate::threads::on_internal_stack(|| NvsBackend::acquire(false))?;
    IdentityStore::open(backend).map_err(|e| e.to_string())
}

/// Call only from an explicit new-device provisioning operation.
pub fn provision(identity: DeviceIdentity) -> Result<Store, String> {
    let backend = crate::threads::on_internal_stack(|| NvsBackend::acquire(true))?;
    IdentityStore::create(backend, identity).map_err(|e| e.to_string())
}

impl Backend for NvsBackend {
    type Error = String;
    fn read_rotation(
        &mut self,
        output: &mut [u8; ROTATION_LEN + 1],
    ) -> Result<Option<usize>, String> {
        crate::threads::on_internal_stack(|| {
            let Some(length) = self.nvs.blob_len("rotation").map_err(|e| e.to_string())? else {
                return Ok(None);
            };
            if length > ROTATION_LEN {
                return Err("Tailnet rotation record exceeds size limit".into());
            }
            let bytes = self
                .nvs
                .get_blob("rotation", output)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "Tailnet rotation disappeared during read".to_string())?;
            if bytes.len() != length {
                return Err("Tailnet rotation length changed during read".into());
            }
            Ok(Some(length))
        })
    }
    fn create_rotation(&mut self, record: &[u8; ROTATION_LEN]) -> Result<(), String> {
        crate::threads::on_internal_stack(|| {
            if self
                .nvs
                .blob_len("rotation")
                .map_err(|e| e.to_string())?
                .is_some()
            {
                return Err("Tailnet rotation already exists".into());
            }
            check(unsafe {
                sys::nvs_set_blob(
                    self.nvs.handle(),
                    c"rotation".as_ptr(),
                    record.as_ptr().cast(),
                    record.len(),
                )
            })?;
            check(unsafe { sys::nvs_commit(self.nvs.handle()) })?;
            let mut verified = Zeroizing::new([0; ROTATION_LEN + 1]);
            if self.read_rotation(&mut verified)? != Some(ROTATION_LEN)
                || &verified[..ROTATION_LEN] != record
            {
                return Err("Tailnet rotation readback failed".into());
            }
            Ok(())
        })
    }
    fn clear_rotation(&mut self) -> Result<(), String> {
        crate::threads::on_internal_stack(|| {
            check(unsafe { sys::nvs_erase_key(self.nvs.handle(), c"rotation".as_ptr()) })?;
            check(unsafe { sys::nvs_commit(self.nvs.handle()) })?;
            if self
                .nvs
                .blob_len("rotation")
                .map_err(|e| e.to_string())?
                .is_some()
            {
                return Err("Tailnet rotation removal readback failed".into());
            }
            Ok(())
        })
    }
    fn read(&mut self, output: &mut [u8; RECORD_LEN + 1]) -> Result<Option<usize>, String> {
        crate::threads::on_internal_stack(|| {
            let Some(length) = self.nvs.blob_len(KEY).map_err(|e| e.to_string())? else {
                return Ok(None);
            };
            if length > RECORD_LEN {
                return Err("Tailnet identity record exceeds size limit".into());
            }
            let bytes = self
                .nvs
                .get_blob(KEY, output)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "Tailnet identity disappeared during read".to_string())?;
            if bytes.len() != length {
                return Err("Tailnet identity length changed during read".into());
            }
            Ok(Some(length))
        })
    }
    fn write(&mut self, record: &[u8; RECORD_LEN], create: bool) -> Result<(), String> {
        crate::threads::on_internal_stack(|| {
            let exists = self.nvs.blob_len(KEY).map_err(|e| e.to_string())?.is_some();
            if create == exists {
                return Err("Tailnet identity create/replace precondition failed".into());
            }
            // EspNvs::set_blob erases first. Update directly to retain NVS's blob
            // replacement protocol, then commit before reporting durable success.
            check(unsafe {
                sys::nvs_set_blob(
                    self.nvs.handle(),
                    c"identity".as_ptr(),
                    record.as_ptr().cast(),
                    record.len(),
                )
            })?;
            check(unsafe { sys::nvs_commit(self.nvs.handle()) })?;
            let mut verified = Zeroizing::new([0; RECORD_LEN + 1]);
            if self.read(&mut verified)? != Some(RECORD_LEN) || &verified[..RECORD_LEN] != record {
                return Err("Tailnet identity readback failed".into());
            }
            Ok(())
        })
    }
}
fn check(code: i32) -> Result<(), String> {
    if code == 0 {
        Ok(())
    } else {
        Err(format!("NVS identity operation failed: {code}"))
    }
}
