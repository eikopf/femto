//! Compiles the device adapter against an injected NVS API; no flash is accessed.
extern crate self as esp_idf_svc;
use femto_microlink::{protocol::identity::DeviceIdentity, timestamp::EpochStore};
use std::cell::RefCell;
enum Mutation {
    Set(bool, Vec<u8>),
    RemoveRotation,
}
#[derive(Default)]
struct State {
    taken: bool,
    namespace: bool,
    record: Option<Vec<u8>>,
    rotation: Option<Vec<u8>>,
    pending: Option<Mutation>,
    init_failure: bool,
    set_failure: bool,
    commit_failure: bool,
    bad_readback: bool,
    writes: usize,
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
pub mod threads {
    /// The host has no stack placement to arrange.
    pub fn on_internal_stack<R>(operation: impl FnOnce() -> R) -> R {
        operation()
    }
}
pub mod handle {
    pub trait RawHandle {
        fn handle(&self) -> usize;
    }
}
pub mod nvs {
    use super::*;
    use std::rc::Rc;
    pub struct Token;
    impl Drop for Token {
        fn drop(&mut self) {
            STATE.with_borrow_mut(|s| s.taken = false);
        }
    }
    #[derive(Clone)]
    pub struct EspDefaultNvsPartition(Rc<Token>);
    impl EspDefaultNvsPartition {
        pub fn take_with(reinit: bool) -> Result<Self, &'static str> {
            assert!(!reinit, "must never erase NVS on initialization failure");
            STATE.with_borrow_mut(|s| {
                if s.init_failure || s.taken {
                    return Err("NVS unavailable");
                }
                s.taken = true;
                Ok(Self(Rc::new(Token)))
            })
        }
    }
    /// Stands in for the firmware's shared partition handle. Each call
    /// takes the partition anew so that the exclusivity checks still apply.
    pub fn partition() -> Result<EspDefaultNvsPartition, String> {
        EspDefaultNvsPartition::take_with(false).map_err(str::to_string)
    }
    pub struct EspDefaultNvs {
        _partition: EspDefaultNvsPartition,
    }
    impl EspDefaultNvs {
        pub fn new(
            partition: EspDefaultNvsPartition,
            namespace: &str,
            writable: bool,
        ) -> Result<Self, &'static str> {
            assert_eq!(namespace, "femto_tailnet");
            let _ = Rc::strong_count(&partition.0);
            STATE.with_borrow_mut(|s| {
                if !writable && !s.namespace {
                    return Err("namespace absent");
                }
                s.namespace = true;
                Ok(Self {
                    _partition: partition,
                })
            })
        }
        pub fn blob_len(&self, key: &str) -> Result<Option<usize>, &'static str> {
            Ok(STATE.with_borrow(|s| match key {
                "identity" => s.record.as_ref().map(Vec::len),
                "rotation" => s.rotation.as_ref().map(Vec::len),
                _ => panic!("unexpected NVS key"),
            }))
        }
        pub fn get_blob<'a>(
            &self,
            key: &str,
            output: &'a mut [u8],
        ) -> Result<Option<&'a [u8]>, &'static str> {
            STATE.with_borrow(|s| {
                let record = match key {
                    "identity" => &s.record,
                    "rotation" => &s.rotation,
                    _ => panic!("unexpected NVS key"),
                };
                let Some(record) = record else {
                    return Ok(None);
                };
                if record.len() > output.len() {
                    return Err("buffer too small");
                }
                output[..record.len()].copy_from_slice(record);
                if s.bad_readback && s.writes > 0 {
                    output[0] ^= 1;
                }
                Ok(Some(&output[..record.len()]))
            })
        }
    }
    impl handle::RawHandle for EspDefaultNvs {
        fn handle(&self) -> usize {
            1
        }
    }
}
#[allow(clippy::missing_safety_doc)]
pub mod sys {
    use super::*;
    use std::ffi::{CStr, c_char, c_void};
    pub unsafe fn nvs_set_blob(
        handle: usize,
        key: *const c_char,
        data: *const c_void,
        len: usize,
    ) -> i32 {
        assert_eq!(handle, 1);
        let rotation = match unsafe { CStr::from_ptr(key) }.to_bytes() {
            b"identity" => false,
            b"rotation" => true,
            _ => panic!("unexpected NVS key"),
        };
        let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), len) };
        STATE.with_borrow_mut(|s| {
            s.writes += 1;
            if s.set_failure {
                return -1;
            }
            s.pending = Some(Mutation::Set(rotation, bytes.to_vec()));
            0
        })
    }
    pub unsafe fn nvs_erase_key(handle: usize, key: *const c_char) -> i32 {
        assert_eq!(handle, 1);
        assert_eq!(unsafe { CStr::from_ptr(key) }.to_bytes(), b"rotation");
        STATE.with_borrow_mut(|s| {
            s.writes += 1;
            if s.set_failure || s.rotation.is_none() {
                return -1;
            }
            s.pending = Some(Mutation::RemoveRotation);
            0
        })
    }
    pub unsafe fn nvs_commit(handle: usize) -> i32 {
        assert_eq!(handle, 1);
        STATE.with_borrow_mut(|s| {
            match s.pending.take() {
                Some(Mutation::Set(false, bytes)) => s.record = Some(bytes),
                Some(Mutation::Set(true, bytes)) => s.rotation = Some(bytes),
                Some(Mutation::RemoveRotation) => s.rotation = None,
                None => {}
            }
            if s.commit_failure { -2 } else { 0 }
        })
    }
}
#[path = "../../../firmware/picco/src/identity.rs"]
mod identity;

#[test]
fn nvs_rotation_journal_reopens_and_completion_preserves_epoch() {
    let mut owner = identity::provision(DeviceIdentity::generate().unwrap()).unwrap();
    owner.reserve_epoch(123).unwrap();
    let old = owner.identity().unwrap().node_key().public();
    let candidate = owner
        .begin_node_key_rotation("control.example", 443)
        .unwrap()
        .candidate()
        .node_key()
        .public();
    assert!(owner.identity().is_err());
    drop(owner);
    let mut owner = identity::open().unwrap();
    assert!(owner.begin_node_key_rotation("other.example", 443).is_err());
    let pending = owner
        .begin_node_key_rotation("control.example", 443)
        .unwrap();
    assert_eq!(pending.old_node_key(), old);
    assert_eq!(pending.candidate().node_key().public(), candidate);
    let mut enrollment = femto_microlink::enrollment::Enrollment::default();
    enrollment
        .apply(candidate, 200, br#"{"MachineAuthorized":true}"#)
        .unwrap();
    owner
        .complete_node_key_rotation("control.example", 443, &enrollment)
        .unwrap();
    drop(owner);
    assert!(STATE.with_borrow(|s| s.rotation.is_none()));
    let mut owner = identity::open().unwrap();
    assert_eq!(owner.identity().unwrap().node_key().public(), candidate);
    assert_eq!(owner.load_epoch().unwrap(), Some(123));
}

#[test]
fn journal_set_commit_and_readback_errors_poison_the_nvs_owner() {
    for failure in 0..3 {
        STATE.with_borrow_mut(|s| *s = State::default());
        let mut owner = identity::provision(DeviceIdentity::generate().unwrap()).unwrap();
        STATE.with_borrow_mut(|s| {
            s.set_failure = failure == 0;
            s.commit_failure = failure == 1;
            s.bad_readback = failure == 2;
        });
        assert!(
            owner
                .begin_node_key_rotation("control.example", 443)
                .is_err()
        );
        assert!(owner.identity().is_err());
        assert!(owner.load_epoch().is_err());
        drop(owner);
        STATE.with_borrow_mut(|s| {
            s.set_failure = false;
            s.commit_failure = false;
            s.bad_readback = false;
        });
        let owner = identity::open().unwrap();
        assert_eq!(owner.identity().is_ok(), failure == 0);
        drop(owner);
    }
}

#[test]
fn provision_reopen_and_exclusive_owner_preserve_identity() {
    assert!(identity::open().is_err());
    assert!(!STATE.with_borrow(|s| s.namespace));
    let keys = DeviceIdentity::generate().unwrap();
    let public = keys.node_key().public();
    let mut owner = identity::provision(keys.clone()).unwrap();
    assert!(identity::open().is_err());
    owner.reserve_epoch(100).unwrap();
    drop(owner);
    assert!(identity::provision(keys).is_err());
    let mut owner = identity::open().unwrap();
    assert_eq!(owner.identity().unwrap().node_key().public(), public);
    assert_eq!(owner.load_epoch().unwrap(), Some(100));
}
#[test]
fn write_and_readback_failures_poison_owner() {
    for failure in 0..3 {
        STATE.with_borrow_mut(|s| *s = State::default());
        let mut owner = identity::provision(DeviceIdentity::generate().unwrap()).unwrap();
        STATE.with_borrow_mut(|s| {
            s.set_failure = failure == 0;
            s.commit_failure = failure == 1;
            s.bad_readback = failure == 2;
        });
        assert!(owner.reserve_epoch(1).is_err());
        assert!(owner.identity().is_err());
        let writes = STATE.with_borrow(|s| s.writes);
        assert!(owner.reserve_epoch(2).is_err());
        assert_eq!(STATE.with_borrow(|s| s.writes), writes);
        drop(owner);
    }
}
#[test]
fn initialization_corruption_and_missing_record_never_reprovision() {
    STATE.with_borrow_mut(|s| s.init_failure = true);
    assert!(identity::provision(DeviceIdentity::generate().unwrap()).is_err());
    assert_eq!(STATE.with_borrow(|s| s.writes), 0);
    STATE.with_borrow_mut(|s| {
        s.init_failure = false;
        s.namespace = true;
    });
    assert!(identity::open().is_err());
    for len in [0, 208, 209, 210, 4096] {
        STATE.with_borrow_mut(|s| s.record = Some(vec![0; len]));
        assert!(identity::open().is_err());
        assert_eq!(STATE.with_borrow(|s| s.writes), 0);
    }
}
