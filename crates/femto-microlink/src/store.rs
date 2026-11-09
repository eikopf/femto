//! Unix host persistence. Device NVS remains a separate EpochStore backend.
use crate::{
    protocol::identity::{DeviceIdentity, ENCODED_IDENTITY_LEN, IdentityError, NodeKeyRotation},
    timestamp::EpochStore,
};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"FEMTOID1";
const BODY: usize = 8 + ENCODED_IDENTITY_LEN + 8;
const LENGTH: usize = BODY + 32;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("identity store is already in use")]
    Locked,
    #[error("identity store record is corrupt or unsupported")]
    Corrupt,
    #[error("identity store permissions or file type are unsafe")]
    Permissions,
    #[error("timestamp epoch must increase")]
    Rollback,
    #[error("identity store had an ambiguous write failure; reopen before reuse")]
    Poisoned,
    #[error(transparent)]
    Rotation(#[from] crate::rotation::Error),
    #[error("pending node-key rotation requires reconciliation")]
    RotationPending,
    #[error("candidate node key has not been authorized")]
    UnauthorizedRotation,
}

/// Identity and handshake epoch are one atomic record, held under a process lock.
/// The directory must be private to this process user and on a local filesystem.
pub struct HostStore {
    directory: PathBuf,
    _lock: File,
    identity: DeviceIdentity,
    epoch: u64,
    poisoned: bool,
    pending: Option<crate::rotation::PendingRotation>,
}

impl HostStore {
    pub fn open(directory: impl AsRef<Path>) -> Result<Self, StoreError> {
        let directory = directory.as_ref();
        match fs::DirBuilder::new().mode(0o700).create(directory) {
            Ok(()) => {
                let parent = directory
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                File::open(parent)?.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        check_path(directory, true)?;
        let lock_path = directory.join("owner.lock");
        let existing_lock = match fs::symlink_metadata(&lock_path) {
            Ok(_) => {
                check_path(&lock_path, false)?;
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error.into()),
        };
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&lock_path)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                StoreError::Locked
            } else {
                error.into()
            }
        })?;
        let record = directory.join("identity.bin");
        let (identity, epoch, fresh) = match fs::symlink_metadata(&record) {
            Ok(_) => {
                check_path(&record, false)?;
                let mut bytes = Zeroizing::new([0; LENGTH + 1]);
                let length = read_record(File::open(&record)?, &mut bytes[..])?;
                let (identity, epoch) = decode(&bytes[..length])?;
                (identity, epoch, false)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if existing_lock {
                    return Err(StoreError::Corrupt);
                }
                (DeviceIdentity::generate()?, 0, true)
            }
            Err(error) => return Err(error.into()),
        };
        let pending_path = directory.join("rotation.bin");
        let pending = match fs::symlink_metadata(&pending_path) {
            Ok(_) => {
                check_path(&pending_path, false)?;
                let mut bytes = Zeroizing::new([0; crate::rotation::RECORD_LEN + 1]);
                let length = read_record(File::open(&pending_path)?, &mut bytes[..])?;
                Some(crate::rotation::PendingRotation::decode(
                    &bytes[..length],
                    &identity,
                )?)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let mut store = Self {
            directory: directory.to_owned(),
            _lock: lock,
            identity,
            epoch,
            poisoned: false,
            pending,
        };
        if fresh {
            store.commit(0)?;
        }
        Ok(store)
    }

    pub fn identity(&self) -> Result<&DeviceIdentity, StoreError> {
        if self.poisoned {
            return Err(StoreError::Poisoned);
        }
        if self.pending.is_some() {
            return Err(StoreError::RotationPending);
        }
        Ok(&self.identity)
    }

    /// Reuses a durably prepared candidate after restart. Ordinary identity and
    /// timestamp access remain blocked until authenticated reconciliation.
    pub fn begin_node_key_rotation(
        &mut self,
        host: &str,
        port: u16,
    ) -> Result<&crate::rotation::PendingRotation, StoreError> {
        if self.poisoned {
            return Err(StoreError::Poisoned);
        }
        if let Some(pending) = &self.pending {
            pending.validate_endpoint(host, port)?;
        } else {
            let pending = crate::rotation::PendingRotation::prepare(&self.identity, host, port)?;
            let bytes = pending.encode();
            self.poisoned = true;
            let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
            temporary.write_all(&*bytes)?;
            temporary.as_file().sync_all()?;
            temporary
                .persist_noclobber(self.directory.join("rotation.bin"))
                .map_err(|error| error.error)?;
            File::open(&self.directory)?.sync_all()?;
            self.pending = Some(pending);
            self.poisoned = false;
        }
        Ok(self.pending.as_ref().expect("pending rotation installed"))
    }

    /// Completes only after an authenticated registration permits the candidate.
    /// Identity commit precedes journal removal; a retained journal is retryable.
    pub fn complete_node_key_rotation(
        &mut self,
        host: &str,
        port: u16,
        enrollment: &crate::enrollment::Enrollment,
    ) -> Result<(), StoreError> {
        if self.poisoned {
            return Err(StoreError::Poisoned);
        }
        let pending = self.pending.as_ref().ok_or(StoreError::RotationPending)?;
        pending.validate_endpoint(host, port)?;
        if !enrollment.permits(pending.candidate().node_key().public()) {
            return Err(StoreError::UnauthorizedRotation);
        }
        self.identity = pending.candidate().clone();
        self.commit(self.epoch)?;
        self.poisoned = true;
        fs::remove_file(self.directory.join("rotation.bin"))?;
        File::open(&self.directory)?.sync_all()?;
        self.pending = None;
        self.poisoned = false;
        Ok(())
    }

    /// Persists a replacement key without changing machine identity or epoch.
    /// The caller must stop old runtime users and coordinate registration;
    /// this operation alone does not establish coordinator authorization.
    pub fn install_node_key_rotation(
        &mut self,
        rotation: &NodeKeyRotation,
    ) -> Result<(), StoreError> {
        let replacement = self.identity()?.rotated(rotation)?;
        self.identity = replacement;
        self.commit(self.epoch)
    }

    fn commit(&mut self, epoch: u64) -> Result<(), StoreError> {
        if self.poisoned {
            return Err(StoreError::Poisoned);
        }
        let encoded = Zeroizing::new(self.identity.encode());
        let mut bytes = Zeroizing::new(Vec::with_capacity(LENGTH));
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&*encoded);
        bytes.extend_from_slice(&epoch.to_be_bytes());
        let digest = Sha256::digest(&*bytes);
        bytes.extend_from_slice(&digest);
        // Every write failure is treated as ambiguous, including a directory
        // sync failure after rename. No further timestamp may use this handle.
        let result = (|| -> Result<(), StoreError> {
            let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
            temporary
                .as_file()
                .set_permissions(fs::Permissions::from_mode(0o600))?;
            temporary.write_all(&bytes)?;
            temporary.as_file().sync_all()?;
            temporary
                .persist(self.directory.join("identity.bin"))
                .map_err(|error| error.error)?;
            File::open(&self.directory)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        } else {
            self.epoch = epoch;
        }
        result
    }
}

impl EpochStore for HostStore {
    type Error = StoreError;
    fn load_epoch(&mut self) -> Result<Option<u64>, StoreError> {
        self.identity()?;
        Ok(Some(self.epoch))
    }
    fn reserve_epoch(&mut self, epoch: u64) -> Result<(), StoreError> {
        self.identity()?;
        if epoch <= self.epoch {
            return Err(StoreError::Rollback);
        }
        self.commit(epoch)
    }
}

// Fixed storage avoids reallocating buffers that contain private keys. The
// caller includes one extra byte so oversized records cannot pass decoding.
fn read_record(mut reader: impl Read, output: &mut [u8]) -> std::io::Result<usize> {
    let mut length = 0;
    while length < output.len() {
        match reader.read(&mut output[length..]) {
            Ok(0) => break,
            Ok(count) => length += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(length)
}

fn check_path(path: &Path, directory: bool) -> Result<(), StoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink()
        || metadata.permissions().mode() & 0o077 != 0
        || (if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file()
        })
    {
        return Err(StoreError::Permissions);
    }
    Ok(())
}

fn decode(bytes: &[u8]) -> Result<(DeviceIdentity, u64), StoreError> {
    if bytes.len() != LENGTH
        || &bytes[..8] != MAGIC
        || Sha256::digest(&bytes[..BODY]).as_slice() != &bytes[BODY..]
    {
        return Err(StoreError::Corrupt);
    }
    let identity = DeviceIdentity::decode(&bytes[8..8 + ENCODED_IDENTITY_LEN])?;
    let epoch = u64::from_be_bytes(
        bytes[8 + ENCODED_IDENTITY_LEN..BODY]
            .try_into()
            .map_err(|_| StoreError::Corrupt)?,
    );
    Ok((identity, epoch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timestamp::TimestampClock;

    #[test]
    fn fixed_record_reads_preserve_fragmented_and_oversized_detection() {
        struct Fragmented<'a>(&'a [u8], bool);
        impl Read for Fragmented<'_> {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                self.1 = !self.1;
                if self.1 {
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                let count = output.len().min(self.0.len()).min(3);
                output[..count].copy_from_slice(&self.0[..count]);
                self.0 = &self.0[count..];
                Ok(count)
            }
        }
        for length in [0, LENGTH - 1, LENGTH, LENGTH + 1, LENGTH * 2] {
            let input = vec![0x42; length];
            let mut output = [0; LENGTH + 1];
            let count = read_record(Fragmented(&input, false), &mut output).unwrap();
            assert_eq!(count, length.min(output.len()));
            assert!(output[..count].iter().all(|byte| *byte == 0x42));
        }
    }

    #[test]
    fn pending_rotation_survives_restart_and_requires_candidate_authorization() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("identity");
        let mut store = HostStore::open(&path).unwrap();
        let old = store.identity().unwrap().node_key().public();
        store.reserve_epoch(123).unwrap();
        let candidate = store
            .begin_node_key_rotation("Control.Example", 443)
            .unwrap()
            .candidate()
            .node_key()
            .public();
        assert!(matches!(store.identity(), Err(StoreError::RotationPending)));
        assert!(matches!(
            store.reserve_epoch(124),
            Err(StoreError::RotationPending)
        ));
        drop(store);
        let mut store = HostStore::open(&path).unwrap();
        assert!(store.begin_node_key_rotation("other.example", 443).is_err());
        assert!(
            store
                .begin_node_key_rotation("control.example", 8443)
                .is_err()
        );
        let pending = store
            .begin_node_key_rotation("control.example", 443)
            .unwrap();
        assert_eq!(pending.candidate().node_key().public(), candidate);
        assert_eq!(pending.old_node_key(), old);
        let mut enrollment = crate::enrollment::Enrollment::default();
        enrollment
            .apply(old, 200, br#"{"MachineAuthorized":true}"#)
            .unwrap();
        assert!(matches!(
            store.complete_node_key_rotation("control.example", 443, &enrollment),
            Err(StoreError::UnauthorizedRotation)
        ));
        enrollment
            .apply(candidate, 200, br#"{"MachineAuthorized":true}"#)
            .unwrap();
        store
            .complete_node_key_rotation("control.example", 443, &enrollment)
            .unwrap();
        assert!(!path.join("rotation.bin").exists());
        drop(store);
        let mut store = HostStore::open(path).unwrap();
        assert_eq!(store.identity().unwrap().node_key().public(), candidate);
        assert_eq!(store.load_epoch().unwrap(), Some(123));
    }

    #[test]
    fn retained_journal_after_identity_commit_can_be_reconciled() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("identity");
        let mut store = HostStore::open(&path).unwrap();
        store.reserve_epoch(123).unwrap();
        let candidate = store
            .begin_node_key_rotation("control.example", 443)
            .unwrap()
            .candidate()
            .node_key()
            .public();
        let pending = Zeroizing::new(fs::read(path.join("rotation.bin")).unwrap());
        fs::remove_file(path.join("rotation.bin")).unwrap();
        fs::create_dir(path.join("rotation.bin")).unwrap();
        let mut enrollment = crate::enrollment::Enrollment::default();
        enrollment
            .apply(candidate, 200, br#"{"MachineAuthorized":true}"#)
            .unwrap();
        assert!(
            store
                .complete_node_key_rotation("control.example", 443, &enrollment)
                .is_err()
        );
        assert!(matches!(store.identity(), Err(StoreError::Poisoned)));
        assert!(matches!(store.load_epoch(), Err(StoreError::Poisoned)));
        drop(store);
        fs::remove_dir(path.join("rotation.bin")).unwrap();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path.join("rotation.bin"))
            .unwrap();
        file.write_all(&pending).unwrap();
        drop(file);
        let mut store = HostStore::open(&path).unwrap();
        assert_eq!(
            store
                .begin_node_key_rotation("control.example", 443)
                .unwrap()
                .candidate()
                .node_key()
                .public(),
            candidate
        );
        assert!(matches!(store.identity(), Err(StoreError::RotationPending)));
        store
            .complete_node_key_rotation("control.example", 443, &enrollment)
            .unwrap();
        assert_eq!(store.identity().unwrap().node_key().public(), candidate);
        assert_eq!(store.load_epoch().unwrap(), Some(123));
    }

    #[test]
    fn failed_pending_write_poisoning_and_corrupt_journal_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("identity");
        let mut store = HostStore::open(&path).unwrap();
        fs::create_dir(path.join("rotation.bin")).unwrap();
        assert!(
            store
                .begin_node_key_rotation("control.example", 443)
                .is_err()
        );
        assert!(matches!(store.identity(), Err(StoreError::Poisoned)));
        drop(store);
        fs::remove_dir(path.join("rotation.bin")).unwrap();
        let mut store = HostStore::open(&path).unwrap();
        store
            .begin_node_key_rotation("control.example", 443)
            .unwrap();
        drop(store);
        let mut bytes = Zeroizing::new(fs::read(path.join("rotation.bin")).unwrap());
        bytes[72] ^= 1;
        fs::write(path.join("rotation.bin"), &*bytes).unwrap();
        assert!(matches!(
            HostStore::open(&path),
            Err(StoreError::Rotation(_))
        ));
    }

    #[test]
    fn node_rotation_reopens_with_the_same_machine_and_timestamp_epoch() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("identity");
        let mut store = HostStore::open(&path).unwrap();
        let original = store.identity().unwrap().clone();
        let rotation = original.prepare_node_key_rotation().unwrap();
        store.reserve_epoch(123).unwrap();
        store.install_node_key_rotation(&rotation).unwrap();
        let bytes = Zeroizing::new(fs::read(path.join("identity.bin")).unwrap());
        assert!(matches!(
            store.install_node_key_rotation(&rotation),
            Err(StoreError::Identity(IdentityError::StaleRotation))
        ));
        assert_eq!(fs::read(path.join("identity.bin")).unwrap(), *bytes);
        drop(store);
        let mut store = HostStore::open(&path).unwrap();
        let identity = store.identity().unwrap();
        assert_eq!(
            identity.node_key().public(),
            rotation.replacement().public()
        );
        assert_eq!(
            identity.machine_key().public(),
            original.machine_key().public()
        );
        assert_eq!(identity.disco_key().public(), original.disco_key().public());
        assert_eq!(
            identity.network_lock_key().public(),
            original.network_lock_key().public()
        );
        assert_eq!(identity.backend_log_id(), original.backend_log_id());
        assert_eq!(store.load_epoch().unwrap(), Some(123));
        store.reserve_epoch(124).unwrap();
    }

    #[test]
    fn failed_rotation_never_exposes_an_uncommitted_key() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("identity");
        let mut store = HostStore::open(&path).unwrap();
        let original = store.identity().unwrap().node_key().public();
        let rotation = store
            .identity()
            .unwrap()
            .prepare_node_key_rotation()
            .unwrap();
        store.reserve_epoch(123).unwrap();
        fs::rename(path.join("identity.bin"), path.join("saved.bin")).unwrap();
        fs::create_dir(path.join("identity.bin")).unwrap();
        assert!(store.install_node_key_rotation(&rotation).is_err());
        assert!(matches!(store.identity(), Err(StoreError::Poisoned)));
        assert!(matches!(store.load_epoch(), Err(StoreError::Poisoned)));
        assert!(matches!(
            store.install_node_key_rotation(&rotation),
            Err(StoreError::Poisoned)
        ));
        drop(store);
        fs::remove_dir(path.join("identity.bin")).unwrap();
        fs::rename(path.join("saved.bin"), path.join("identity.bin")).unwrap();
        let mut reopened = HostStore::open(path).unwrap();
        assert_eq!(reopened.identity().unwrap().node_key().public(), original);
        assert_eq!(reopened.load_epoch().unwrap(), Some(123));
    }

    #[test]
    fn missing_record_in_an_existing_store_does_not_rotate_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("identity");
        drop(HostStore::open(&path).unwrap());
        fs::remove_file(path.join("identity.bin")).unwrap();
        assert!(matches!(HostStore::open(&path), Err(StoreError::Corrupt)));
        assert!(!path.join("identity.bin").exists());
    }

    #[test]
    fn identity_and_timestamps_survive_reopen_and_wall_clock_rollback() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("identity");
        let store = HostStore::open(&path).unwrap();
        let node = store.identity().unwrap().node_key().public();
        let machine = store.identity().unwrap().machine_key().public();
        let mut clock = TimestampClock::open(store, 1_000_000).unwrap();
        let mut last = clock.next_timestamp().unwrap();
        for _ in 0..70 {
            last = clock.next_timestamp().unwrap();
        }
        drop(clock);
        let store = HostStore::open(&path).unwrap();
        assert_eq!(store.identity().unwrap().node_key().public(), node);
        assert_eq!(store.identity().unwrap().machine_key().public(), machine);
        let mut clock = TimestampClock::open(store, 500).unwrap();
        assert!(clock.next_timestamp().unwrap() > last);
        assert_eq!(
            fs::metadata(path.join("identity.bin"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn exclusive_owner_and_corruption_never_replace_an_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("identity");
        let store = HostStore::open(&path).unwrap();
        assert!(matches!(HostStore::open(&path), Err(StoreError::Locked)));
        drop(store);
        let file = path.join("identity.bin");
        let mut bytes = fs::read(&file).unwrap();
        bytes[42] ^= 1;
        fs::write(&file, &bytes).unwrap();
        assert!(matches!(HostStore::open(&path), Err(StoreError::Corrupt)));
        assert_eq!(fs::read(&file).unwrap(), bytes);
    }

    #[test]
    fn unsafe_paths_and_permissions_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("identity");
        drop(HostStore::open(&path).unwrap());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(matches!(
            HostStore::open(&path),
            Err(StoreError::Permissions)
        ));
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_file(path.join("owner.lock")).unwrap();
        std::os::unix::fs::symlink(path.join("absent"), path.join("owner.lock")).unwrap();
        assert!(matches!(
            HostStore::open(&path),
            Err(StoreError::Permissions)
        ));
        assert!(!path.join("absent").exists());
    }

    #[test]
    fn failed_write_poisoning_requires_reopen_without_epoch_rollback() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("identity");
        let mut store = HostStore::open(&path).unwrap();
        store.reserve_epoch(10).unwrap();
        assert!(matches!(store.reserve_epoch(10), Err(StoreError::Rollback)));
        fs::rename(path.join("identity.bin"), path.join("saved.bin")).unwrap();
        fs::create_dir(path.join("identity.bin")).unwrap();
        assert!(store.reserve_epoch(11).is_err());
        assert!(matches!(store.reserve_epoch(12), Err(StoreError::Poisoned)));
        assert!(matches!(store.identity(), Err(StoreError::Poisoned)));
        drop(store);
        fs::remove_dir(path.join("identity.bin")).unwrap();
        fs::rename(path.join("saved.bin"), path.join("identity.bin")).unwrap();
        let mut store = HostStore::open(&path).unwrap();
        assert_eq!(store.load_epoch().unwrap(), Some(10));
        store.reserve_epoch(11).unwrap();
    }
}
