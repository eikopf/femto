//! Identity ownership over an exclusively held durable record backend.
use crate::{
    protocol::identity::{DeviceIdentity, ENCODED_IDENTITY_LEN, NodeKeyRotation},
    rotation::{PendingRotation, RECORD_LEN as ROTATION_LEN},
    timestamp::EpochStore,
};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;
const MAGIC: &[u8; 8] = b"FEMTOID1";
const BODY: usize = 8 + ENCODED_IDENTITY_LEN + 8;
pub const RECORD_LEN: usize = BODY + 32;

/// The backend owns exclusive access until dropped. It must not expose keys.
/// A successful write durably publishes the complete record; a failed write
/// may leave either version. Recovery must never silently select an older
/// record after a previously successful write. Physical rollback is separate.
pub trait Backend {
    type Error;
    /// Return None only for an absent record. Reject or report oversized data
    /// using the extra byte; never accept a truncated prefix as a complete record.
    fn read(&mut self, output: &mut [u8; RECORD_LEN + 1]) -> Result<Option<usize>, Self::Error>;
    /// Create must refuse an existing record. Replace must refuse an absent one.
    fn write(&mut self, record: &[u8; RECORD_LEN], create: bool) -> Result<(), Self::Error>;
    /// Journal operations share the identity record's exclusive ownership.
    fn read_rotation(
        &mut self,
        output: &mut [u8; ROTATION_LEN + 1],
    ) -> Result<Option<usize>, Self::Error>;
    /// Must refuse replacement of an existing journal and durably publish it.
    fn create_rotation(&mut self, record: &[u8; ROTATION_LEN]) -> Result<(), Self::Error>;
    /// Durably removes the journal. Errors can occur before or after removal.
    fn clear_rotation(&mut self) -> Result<(), Self::Error>;
}
#[derive(Debug, thiserror::Error)]
pub enum Error<E> {
    #[error("identity backend failure")]
    Storage(E),
    #[error("identity record absent; explicit provisioning required")]
    Missing,
    #[error("identity record already exists")]
    Exists,
    #[error("identity record corrupt or unsupported")]
    Corrupt,
    #[error("identity epoch must increase")]
    Rollback,
    #[error("identity handle poisoned after ambiguous write")]
    Poisoned,
    #[error("node-key rotation belongs to a different active key")]
    StaleRotation,
    #[error(transparent)]
    Rotation(#[from] crate::rotation::Error),
    #[error("pending node-key rotation requires reconciliation")]
    RotationPending,
    #[error("candidate node key has not been authorized")]
    UnauthorizedRotation,
}

pub struct IdentityStore<B> {
    backend: B,
    identity: DeviceIdentity,
    epoch: u64,
    poisoned: bool,
    pending: Option<PendingRotation>,
}
impl<B: Backend> IdentityStore<B> {
    pub fn open(mut backend: B) -> Result<Self, Error<B::Error>> {
        let mut bytes = Zeroizing::new([0; RECORD_LEN + 1]);
        let len = backend
            .read(&mut bytes)
            .map_err(Error::Storage)?
            .ok_or(Error::Missing)?;
        if len != RECORD_LEN
            || &bytes[..8] != MAGIC
            || Sha256::digest(&bytes[..BODY]).as_slice() != &bytes[BODY..RECORD_LEN]
        {
            return Err(Error::Corrupt);
        }
        let identity = DeviceIdentity::decode(&bytes[8..8 + ENCODED_IDENTITY_LEN])
            .map_err(|_| Error::Corrupt)?;
        let epoch = u64::from_be_bytes(
            bytes[8 + ENCODED_IDENTITY_LEN..BODY]
                .try_into()
                .map_err(|_| Error::Corrupt)?,
        );
        let mut journal = Zeroizing::new([0; ROTATION_LEN + 1]);
        let pending = match backend
            .read_rotation(&mut journal)
            .map_err(Error::Storage)?
        {
            Some(len) if len <= ROTATION_LEN => {
                Some(PendingRotation::decode(&journal[..len], &identity)?)
            }
            Some(_) => return Err(Error::Corrupt),
            None => None,
        };
        Ok(Self {
            backend,
            identity,
            epoch,
            poisoned: false,
            pending,
        })
    }
    /// Only an explicit new-identity provisioning operation may call create.
    pub fn create(mut backend: B, identity: DeviceIdentity) -> Result<Self, Error<B::Error>> {
        let mut bytes = Zeroizing::new([0; RECORD_LEN + 1]);
        if backend.read(&mut bytes).map_err(Error::Storage)?.is_some() {
            return Err(Error::Exists);
        }
        let mut journal = Zeroizing::new([0; ROTATION_LEN + 1]);
        if backend
            .read_rotation(&mut journal)
            .map_err(Error::Storage)?
            .is_some()
        {
            return Err(Error::RotationPending);
        }
        let mut store = Self {
            backend,
            identity,
            epoch: 0,
            poisoned: false,
            pending: None,
        };
        store.commit(0, true)?;
        Ok(store)
    }
    pub fn identity(&self) -> Result<&DeviceIdentity, Error<B::Error>> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if self.pending.is_some() {
            return Err(Error::RotationPending);
        }
        Ok(&self.identity)
    }
    pub fn begin_node_key_rotation(
        &mut self,
        host: &str,
        port: u16,
    ) -> Result<&PendingRotation, Error<B::Error>> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        if let Some(pending) = &self.pending {
            pending.validate_endpoint(host, port)?;
        } else {
            let pending = PendingRotation::prepare(&self.identity, host, port)?;
            self.poisoned = true;
            self.backend
                .create_rotation(&pending.encode())
                .map_err(Error::Storage)?;
            self.pending = Some(pending);
            self.poisoned = false;
        }
        Ok(self.pending.as_ref().expect("pending rotation installed"))
    }
    /// The caller supplies enrollment from the bound authenticated coordinator.
    /// Main-record commit precedes journal removal to permit restart recovery.
    pub fn complete_node_key_rotation(
        &mut self,
        host: &str,
        port: u16,
        enrollment: &crate::enrollment::Enrollment,
    ) -> Result<(), Error<B::Error>> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let pending = self.pending.as_ref().ok_or(Error::RotationPending)?;
        pending.validate_endpoint(host, port)?;
        if !enrollment.permits(pending.candidate().node_key().public()) {
            return Err(Error::UnauthorizedRotation);
        }
        self.identity = pending.candidate().clone();
        self.commit(self.epoch, false)?;
        self.poisoned = true;
        self.backend.clear_rotation().map_err(Error::Storage)?;
        self.pending = None;
        self.poisoned = false;
        Ok(())
    }
    /// Installs a replacement in the durable record before exposing it through
    /// this handle. The caller must stop users of the previous identity and
    /// coordinate registration separately. No coordinator acceptance is implied.
    pub fn install_node_key_rotation(
        &mut self,
        rotation: &NodeKeyRotation,
    ) -> Result<(), Error<B::Error>> {
        let replacement = self
            .identity()?
            .rotated(rotation)
            .map_err(|_| Error::StaleRotation)?;
        self.identity = replacement;
        self.commit(self.epoch, false)
    }
    fn commit(&mut self, epoch: u64, create: bool) -> Result<(), Error<B::Error>> {
        if self.poisoned {
            return Err(Error::Poisoned);
        }
        let encoded = Zeroizing::new(self.identity.encode());
        let mut record = Zeroizing::new([0; RECORD_LEN]);
        record[..8].copy_from_slice(MAGIC);
        record[8..8 + ENCODED_IDENTITY_LEN].copy_from_slice(&*encoded);
        record[8 + ENCODED_IDENTITY_LEN..BODY].copy_from_slice(&epoch.to_be_bytes());
        let digest = Sha256::digest(&record[..BODY]);
        record[BODY..].copy_from_slice(&digest);
        self.poisoned = true;
        self.backend
            .write(&record, create)
            .map_err(Error::Storage)?;
        self.epoch = epoch;
        self.poisoned = false;
        Ok(())
    }
}
impl<B: Backend> EpochStore for IdentityStore<B> {
    type Error = Error<B::Error>;
    fn load_epoch(&mut self) -> Result<Option<u64>, Self::Error> {
        self.identity()?;
        Ok(Some(self.epoch))
    }
    fn reserve_epoch(&mut self, epoch: u64) -> Result<(), Self::Error> {
        self.identity()?;
        if epoch <= self.epoch {
            return Err(Error::Rollback);
        }
        self.commit(epoch, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timestamp::TimestampClock;
    use std::{cell::RefCell, rc::Rc};
    #[derive(Default)]
    struct State {
        bytes: Option<Vec<u8>>,
        journal: Option<Vec<u8>>,
        journal_failure: u8,
        fail: u8,
        writes: usize,
        held: bool,
    }
    struct Memory(Rc<RefCell<State>>);
    impl Memory {
        fn acquire(state: &Rc<RefCell<State>>) -> Self {
            assert!(!state.borrow().held);
            state.borrow_mut().held = true;
            Self(state.clone())
        }
    }
    impl Drop for Memory {
        fn drop(&mut self) {
            self.0.borrow_mut().held = false;
        }
    }
    impl Backend for Memory {
        type Error = ();
        fn read_rotation(
            &mut self,
            output: &mut [u8; ROTATION_LEN + 1],
        ) -> Result<Option<usize>, ()> {
            Ok(self.0.borrow().journal.as_ref().map(|bytes| {
                let len = bytes.len().min(output.len());
                output[..len].copy_from_slice(&bytes[..len]);
                len
            }))
        }
        fn create_rotation(&mut self, record: &[u8; ROTATION_LEN]) -> Result<(), ()> {
            let mut state = self.0.borrow_mut();
            if state.journal.is_some() || state.journal_failure == 1 {
                return Err(());
            }
            state.journal = Some(record.to_vec());
            if state.journal_failure == 2 {
                return Err(());
            }
            Ok(())
        }
        fn clear_rotation(&mut self) -> Result<(), ()> {
            let mut state = self.0.borrow_mut();
            if state.journal_failure == 3 {
                return Err(());
            }
            state.journal = None;
            if state.journal_failure == 4 {
                return Err(());
            }
            Ok(())
        }
        fn read(&mut self, output: &mut [u8; RECORD_LEN + 1]) -> Result<Option<usize>, ()> {
            let state = self.0.borrow();
            Ok(state.bytes.as_ref().map(|bytes| {
                let n = bytes.len().min(output.len());
                output[..n].copy_from_slice(&bytes[..n]);
                n
            }))
        }
        fn write(&mut self, record: &[u8; RECORD_LEN], create: bool) -> Result<(), ()> {
            let mut state = self.0.borrow_mut();
            state.writes += 1;
            if create == state.bytes.is_some() || state.fail == 1 {
                return Err(());
            }
            state.bytes = Some(record.to_vec());
            if state.fail == 2 {
                return Err(());
            }
            Ok(())
        }
    }
    fn state() -> Rc<RefCell<State>> {
        Rc::new(RefCell::new(State::default()))
    }
    #[test]
    fn journal_failures_before_and_after_each_write_recover_without_new_candidates() {
        for failure in 0..=6 {
            let state = state();
            let original = DeviceIdentity::generate().unwrap();
            let old = original.node_key().public();
            let mut store = IdentityStore::create(Memory::acquire(&state), original).unwrap();
            store.reserve_epoch(123).unwrap();
            if failure <= 2 {
                state.borrow_mut().journal_failure = failure;
            }
            let begun = store.begin_node_key_rotation("control.example", 443);
            if failure == 1 || failure == 2 {
                assert!(matches!(begun, Err(Error::Storage(()))));
                assert!(matches!(store.identity(), Err(Error::Poisoned)));
                drop(store);
                state.borrow_mut().journal_failure = 0;
                store = IdentityStore::open(Memory::acquire(&state)).unwrap();
                if failure == 1 {
                    assert_eq!(store.identity().unwrap().node_key().public(), old);
                } else {
                    assert!(matches!(store.identity(), Err(Error::RotationPending)));
                }
            }
            let candidate = store
                .begin_node_key_rotation("control.example", 443)
                .unwrap()
                .candidate()
                .node_key()
                .public();
            assert!(matches!(store.load_epoch(), Err(Error::RotationPending)));
            assert!(matches!(
                store.reserve_epoch(124),
                Err(Error::RotationPending)
            ));
            assert!(store.begin_node_key_rotation("other.example", 443).is_err());
            let mut enrollment = crate::enrollment::Enrollment::default();
            enrollment
                .apply(old, 200, br#"{"MachineAuthorized":true}"#)
                .unwrap();
            assert!(matches!(
                store.complete_node_key_rotation("control.example", 443, &enrollment),
                Err(Error::UnauthorizedRotation)
            ));
            enrollment
                .apply(candidate, 200, br#"{"MachineAuthorized":true}"#)
                .unwrap();
            if failure == 3 || failure == 4 {
                state.borrow_mut().journal_failure = failure;
            }
            if failure == 5 || failure == 6 {
                state.borrow_mut().fail = failure - 4;
            }
            let finished = store.complete_node_key_rotation("control.example", 443, &enrollment);
            if failure >= 3 {
                assert!(matches!(finished, Err(Error::Storage(()))));
                assert!(matches!(store.identity(), Err(Error::Poisoned)));
                assert!(matches!(store.load_epoch(), Err(Error::Poisoned)));
            } else {
                finished.unwrap();
            }
            drop(store);
            state.borrow_mut().journal_failure = 0;
            state.borrow_mut().fail = 0;
            let mut reopened = IdentityStore::open(Memory::acquire(&state)).unwrap();
            if state.borrow().journal.is_some() {
                assert!(matches!(reopened.identity(), Err(Error::RotationPending)));
                assert_eq!(
                    reopened
                        .begin_node_key_rotation("CONTROL.EXAMPLE", 443)
                        .unwrap()
                        .candidate()
                        .node_key()
                        .public(),
                    candidate
                );
                reopened
                    .complete_node_key_rotation("control.example", 443, &enrollment)
                    .unwrap();
            }
            assert_eq!(reopened.identity().unwrap().node_key().public(), candidate);
            assert_eq!(reopened.load_epoch().unwrap(), Some(123));
            assert!(state.borrow().journal.is_none());
        }
    }

    #[test]
    fn orphan_corrupt_and_oversized_journals_cannot_be_ignored() {
        let state = state();
        let original = DeviceIdentity::generate().unwrap();
        let journal = PendingRotation::prepare(&original, "control.example", 443)
            .unwrap()
            .encode();
        state.borrow_mut().journal = Some(journal.to_vec());
        assert!(matches!(
            IdentityStore::create(Memory::acquire(&state), original.clone()),
            Err(Error::RotationPending)
        ));
        assert_eq!(state.borrow().writes, 0);
        state.borrow_mut().journal = None;
        drop(IdentityStore::create(Memory::acquire(&state), original).unwrap());
        for len in [0, ROTATION_LEN - 1, ROTATION_LEN + 1, ROTATION_LEN * 2] {
            let mut bad = journal.to_vec();
            bad.resize(len, 0);
            state.borrow_mut().journal = Some(bad);
            assert!(IdentityStore::open(Memory::acquire(&state)).is_err());
        }
        let mut bad = journal.to_vec();
        bad[72] ^= 1;
        state.borrow_mut().journal = Some(bad);
        assert!(IdentityStore::open(Memory::acquire(&state)).is_err());
        assert_eq!(state.borrow().writes, 1);
    }

    #[test]
    fn rotation_is_durable_preserves_identity_and_epoch_and_rejects_stale_work() {
        let state = state();
        let original = DeviceIdentity::generate().unwrap();
        let rotation = original.prepare_node_key_rotation().unwrap();
        let competing = original.prepare_node_key_rotation().unwrap();
        let mut store = IdentityStore::create(Memory::acquire(&state), original.clone()).unwrap();
        store.reserve_epoch(123).unwrap();
        store.install_node_key_rotation(&rotation).unwrap();
        let current = store.identity().unwrap();
        assert_eq!(current.node_key().public(), rotation.replacement().public());
        assert_eq!(
            current.machine_key().public(),
            original.machine_key().public()
        );
        assert_eq!(current.disco_key().public(), original.disco_key().public());
        assert_eq!(
            current.network_lock_key().public(),
            original.network_lock_key().public()
        );
        assert_eq!(current.backend_log_id(), original.backend_log_id());
        let writes = state.borrow().writes;
        for stale in [&rotation, &competing] {
            assert!(matches!(
                store.install_node_key_rotation(stale),
                Err(Error::StaleRotation)
            ));
        }
        assert_eq!(state.borrow().writes, writes);
        drop(store);
        let mut reopened = IdentityStore::open(Memory::acquire(&state)).unwrap();
        assert_eq!(
            reopened.identity().unwrap().node_key().public(),
            rotation.replacement().public()
        );
        assert_eq!(reopened.load_epoch().unwrap(), Some(123));
        assert!(matches!(reopened.reserve_epoch(123), Err(Error::Rollback)));
        reopened.reserve_epoch(124).unwrap();
    }

    #[test]
    fn ambiguous_rotation_writes_poison_both_key_and_timestamp_access() {
        for failure in [1, 2] {
            let state = state();
            let original = DeviceIdentity::generate().unwrap();
            let rotation = original.prepare_node_key_rotation().unwrap();
            let mut store =
                IdentityStore::create(Memory::acquire(&state), original.clone()).unwrap();
            store.reserve_epoch(123).unwrap();
            state.borrow_mut().fail = failure;
            assert!(matches!(
                store.install_node_key_rotation(&rotation),
                Err(Error::Storage(()))
            ));
            assert!(matches!(store.identity(), Err(Error::Poisoned)));
            assert!(matches!(store.load_epoch(), Err(Error::Poisoned)));
            let writes = state.borrow().writes;
            assert!(matches!(
                store.install_node_key_rotation(&rotation),
                Err(Error::Poisoned)
            ));
            assert!(matches!(store.reserve_epoch(124), Err(Error::Poisoned)));
            assert_eq!(state.borrow().writes, writes);
            drop(store);
            state.borrow_mut().fail = 0;
            let mut reopened = IdentityStore::open(Memory::acquire(&state)).unwrap();
            let expected = if failure == 1 {
                original.node_key().public()
            } else {
                rotation.replacement().public()
            };
            assert_eq!(reopened.identity().unwrap().node_key().public(), expected);
            assert_eq!(reopened.load_epoch().unwrap(), Some(123));
        }
    }

    #[test]
    fn provisioning_is_explicit_and_reopen_keeps_keys_and_monotonic_time() {
        let state = state();
        assert!(matches!(
            IdentityStore::open(Memory::acquire(&state)),
            Err(Error::Missing)
        ));
        assert_eq!(state.borrow().writes, 0);
        let identity = DeviceIdentity::generate().unwrap();
        let public = identity.node_key().public();
        let store = IdentityStore::create(Memory::acquire(&state), identity.clone()).unwrap();
        let mut clock = TimestampClock::open(store, 100).unwrap();
        let first = clock.next_timestamp().unwrap();
        drop(clock);
        assert!(matches!(
            IdentityStore::create(Memory::acquire(&state), identity),
            Err(Error::Exists)
        ));
        let store = IdentityStore::open(Memory::acquire(&state)).unwrap();
        assert_eq!(store.identity().unwrap().node_key().public(), public);
        let mut clock = TimestampClock::open(store, 1).unwrap();
        assert!(clock.next_timestamp().unwrap() > first);
    }
    #[test]
    fn failed_writes_poison_identity_and_epoch_access_before_or_after_commit() {
        for failure in [1, 2] {
            let state = state();
            let mut store =
                IdentityStore::create(Memory::acquire(&state), DeviceIdentity::generate().unwrap())
                    .unwrap();
            store.reserve_epoch(10).unwrap();
            assert!(matches!(store.reserve_epoch(10), Err(Error::Rollback)));
            state.borrow_mut().fail = failure;
            assert!(matches!(store.reserve_epoch(11), Err(Error::Storage(()))));
            let writes = state.borrow().writes;
            assert!(matches!(store.identity(), Err(Error::Poisoned)));
            assert!(matches!(store.load_epoch(), Err(Error::Poisoned)));
            assert!(matches!(store.reserve_epoch(12), Err(Error::Poisoned)));
            assert_eq!(state.borrow().writes, writes);
            drop(store);
            state.borrow_mut().fail = 0;
            let mut reopened = IdentityStore::open(Memory::acquire(&state)).unwrap();
            assert_eq!(
                reopened.load_epoch().unwrap(),
                Some(if failure == 1 { 10 } else { 11 })
            );
            let mut clock = TimestampClock::open(reopened, 0).unwrap();
            assert!(clock.next_timestamp().is_ok());
        }
    }
    #[test]
    fn failed_initial_provisioning_leaves_no_usable_handle() {
        for failure in [1, 2] {
            let state = state();
            state.borrow_mut().fail = failure;
            assert!(matches!(
                IdentityStore::create(Memory::acquire(&state), DeviceIdentity::generate().unwrap()),
                Err(Error::Storage(()))
            ));
            assert!(!state.borrow().held);
            assert_eq!(state.borrow().writes, 1);
            state.borrow_mut().fail = 0;
            let recovered = IdentityStore::open(Memory::acquire(&state));
            if failure == 1 {
                assert!(matches!(recovered, Err(Error::Missing)));
            } else {
                assert!(recovered.is_ok());
            }
        }
    }
    #[cfg(all(unix, not(target_os = "espidf")))]
    #[test]
    fn record_encoding_matches_host_store() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("identity");
        let mut host = crate::store::HostStore::open(&directory).unwrap();
        host.reserve_epoch(123).unwrap();
        let public = host.identity().unwrap().node_key().public();
        drop(host);
        let state = state();
        state.borrow_mut().bytes = Some(std::fs::read(directory.join("identity.bin")).unwrap());
        let mut portable = IdentityStore::open(Memory::acquire(&state)).unwrap();
        assert_eq!(portable.identity().unwrap().node_key().public(), public);
        assert_eq!(portable.load_epoch().unwrap(), Some(123));
        portable.reserve_epoch(124).unwrap();
        drop(portable);
        std::fs::write(
            directory.join("identity.bin"),
            state.borrow().bytes.as_ref().unwrap(),
        )
        .unwrap();
        let mut host = crate::store::HostStore::open(directory).unwrap();
        assert_eq!(host.identity().unwrap().node_key().public(), public);
        assert_eq!(host.load_epoch().unwrap(), Some(124));
    }
    #[test]
    fn corruption_and_size_errors_never_write_or_regenerate_keys() {
        let state = state();
        drop(
            IdentityStore::create(Memory::acquire(&state), DeviceIdentity::generate().unwrap())
                .unwrap(),
        );
        let valid = state.borrow().bytes.clone().unwrap();
        for byte in 0..RECORD_LEN {
            let mut bad = valid.clone();
            bad[byte] ^= 1;
            state.borrow_mut().bytes = Some(bad);
            assert!(matches!(
                IdentityStore::open(Memory::acquire(&state)),
                Err(Error::Corrupt)
            ));
        }
        for size in [0, RECORD_LEN - 1, RECORD_LEN + 1, RECORD_LEN * 2] {
            let mut bad = valid.clone();
            bad.resize(size, 0);
            state.borrow_mut().bytes = Some(bad);
            assert!(matches!(
                IdentityStore::open(Memory::acquire(&state)),
                Err(Error::Corrupt)
            ));
        }
        assert_eq!(state.borrow().writes, 1);
    }
}
