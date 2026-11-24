//! Property test of the identity store under write failures and restarts.
//!
//! From `docs/identity-storage.md`: identity and epoch are committed
//! together; a failed write leaves the old or the new record and poisons the
//! handle; reopening always works and never yields an older epoch or a key
//! that was not written; handshake timestamps never repeat or go back, even
//! when the wall clock does.
use femto_microlink::{
    enrollment::Enrollment,
    identity_store::{Backend, Error, IdentityStore, RECORD_LEN},
    protocol::{
        identity::{DeviceIdentity, ENCODED_IDENTITY_LEN},
        key::{Node, PublicKey},
    },
    rotation::RECORD_LEN as ROTATION_LEN,
    timestamp::{EpochStore, TimestampClock},
};
use proptest::prelude::*;
use std::{cell::RefCell, rc::Rc, time::SystemTime};

#[derive(Clone, Copy, Debug, PartialEq)]
enum Fault {
    /// The write fails and nothing is stored.
    Before,
    /// The write is stored and then reported as failed.
    After,
}
#[derive(Default)]
struct Disk {
    record: Option<Vec<u8>>,
    journal: Option<Vec<u8>>,

    /// Fails one coming write of any kind, after letting this many pass.
    fault: Option<(Fault, usize)>,
    writes: usize,
}
struct Memory(Rc<RefCell<Disk>>);
impl Memory {
    fn write(&mut self, apply: impl FnOnce(&mut Disk)) -> Result<(), ()> {
        let mut disk = self.0.borrow_mut();
        disk.writes += 1;
        let due = match disk.fault.take() {
            Some((fault, 0)) => Some(fault),
            Some((fault, skip)) => {
                disk.fault = Some((fault, skip - 1));
                None
            }
            None => None,
        };
        match due {
            Some(Fault::Before) => Err(()),
            Some(Fault::After) => {
                apply(&mut disk);
                Err(())
            }
            None => {
                apply(&mut disk);
                Ok(())
            }
        }
    }
}
fn copy(from: &Option<Vec<u8>>, to: &mut [u8]) -> Option<usize> {
    from.as_ref().map(|bytes| {
        let n = bytes.len().min(to.len());
        to[..n].copy_from_slice(&bytes[..n]);
        n
    })
}
impl Backend for Memory {
    type Error = ();
    fn read(&mut self, output: &mut [u8; RECORD_LEN + 1]) -> Result<Option<usize>, ()> {
        Ok(copy(&self.0.borrow().record, output))
    }
    fn write(&mut self, record: &[u8; RECORD_LEN], create: bool) -> Result<(), ()> {
        if create == self.0.borrow().record.is_some() {
            return Err(());
        }
        self.write(|disk| disk.record = Some(record.to_vec()))
    }
    fn read_rotation(&mut self, output: &mut [u8; ROTATION_LEN + 1]) -> Result<Option<usize>, ()> {
        Ok(copy(&self.0.borrow().journal, output))
    }
    fn create_rotation(&mut self, record: &[u8; ROTATION_LEN]) -> Result<(), ()> {
        if self.0.borrow().journal.is_some() {
            return Err(());
        }
        self.write(|disk| disk.journal = Some(record.to_vec()))
    }
    fn clear_rotation(&mut self) -> Result<(), ()> {
        self.write(|disk| disk.journal = None)
    }
}

/// What the stored record says, read without the store.
fn stored(disk: &Rc<RefCell<Disk>>) -> (PublicKey<Node>, PublicKey<Node>, u64) {
    let disk = disk.borrow();
    let record = disk.record.as_ref().expect("record present");
    assert_eq!(record.len(), RECORD_LEN);
    let identity = DeviceIdentity::decode(&record[8..8 + ENCODED_IDENTITY_LEN]).unwrap();
    let epoch = u64::from_be_bytes(
        record[8 + ENCODED_IDENTITY_LEN..16 + ENCODED_IDENTITY_LEN]
            .try_into()
            .unwrap(),
    );
    // The machine key is compared as a node key only to reuse one key type.
    let machine = PublicKey::<Node>::from_bytes(*identity.machine_key().public().as_bytes());
    (identity.node_key().public(), machine, epoch)
}

#[derive(Clone, Debug)]
enum Op {
    /// Fail a coming write, after letting this many pass.
    Fail(Fault, usize),
    /// Reserve this far beyond the largest epoch asked for so far (0: not beyond).
    Reserve(u64),
    /// A boot with this wall clock that takes this many timestamps.
    Boot {
        wall: u64,
        count: usize,
    },
    Begin {
        host: bool,
    },
    Complete {
        host: bool,
        authorized: bool,
    },
    Install,
    Reopen,
}
fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (prop_oneof![Just(Fault::Before), Just(Fault::After)], prop_oneof![3 => Just(0usize), 2 => Just(1), 1 => Just(2)])
            .prop_map(|(fault, skip)| Op::Fail(fault, skip)),
        2 => prop_oneof![Just(0u64), Just(1), Just(7)].prop_map(Op::Reserve),
        3 => (prop_oneof![Just(0u64), Just(5), Just(1_000), Just(1_001), Just(1 << 40)],
              prop_oneof![Just(0usize), Just(1), Just(59), Just(60), Just(61), Just(125)])
            .prop_map(|(wall, count)| Op::Boot { wall, count }),
        3 => prop::bool::weighted(0.85).prop_map(|host| Op::Begin { host }),
        3 => (prop::bool::weighted(0.85), prop::bool::weighted(0.85))
            .prop_map(|(host, authorized)| Op::Complete { host, authorized }),
        1 => Just(Op::Install),
        2 => Just(Op::Reopen),
    ]
}
fn host(first: bool) -> &'static str {
    if first {
        "control.example"
    } else {
        "other.example"
    }
}

#[derive(Default, Debug)]
struct Seen {
    poisoned: usize,
    pending: usize,
    completed: usize,
    installed: usize,
    timestamps: usize,
}

fn run(ops: &[Op]) -> Result<Seen, TestCaseError> {
    let mut seen = Seen::default();
    let disk = Rc::new(RefCell::new(Disk::default()));
    let original = DeviceIdentity::generate().unwrap();
    let machine = PublicKey::<Node>::from_bytes(*original.machine_key().public().as_bytes());
    let mut store = Some(IdentityStore::create(Memory(disk.clone()), original).unwrap());
    // The epoch on disk after the previous step; the largest ever asked for.
    let mut durable = 0u64;
    let mut asked = 0u64;
    // Node keys the record may hold: those written, or possibly written.
    let mut possible = vec![stored(&disk).0];
    // The key the record must hold: the last one whose write was confirmed.
    let mut last: Option<SystemTime> = None;
    let mut candidate: Option<PublicKey<Node>> = None;
    // The coordinator the journal on disk was made for.
    let mut journal_host: Option<bool> = None;

    for op in ops {
        let writes = disk.borrow().writes;
        let was_poisoned = matches!(
            store.as_ref().map(|s| s.identity().map(|_| ())),
            Some(Err(Error::Poisoned))
        );
        match op {
            Op::Fail(fault, skip) => disk.borrow_mut().fault = Some((*fault, *skip)),
            Op::Reopen => store = None,
            Op::Reserve(beyond) => {
                let handle = store.as_mut().unwrap();
                let epoch = asked + beyond;
                let before = stored(&disk).2;
                let result = handle.reserve_epoch(epoch);
                match &result {
                    Ok(()) => {
                        prop_assert!(epoch > before, "reservation did not increase the epoch");
                        prop_assert_eq!(stored(&disk).2, epoch);
                    }
                    Err(Error::Rollback) => prop_assert!(epoch <= before),
                    Err(_) => (),
                }
                if !matches!(
                    result,
                    Err(Error::Rollback | Error::Poisoned | Error::RotationPending)
                ) {
                    asked = asked.max(epoch);
                }
            }
            Op::Boot { wall, count } => {
                let handle = store.take().unwrap();
                let floor = stored(&disk).2;
                // A boot may reserve up to the wall clock plus one epoch per sixty timestamps.
                asked = asked.max(floor.max(*wall) + 1 + (*count as u64) / 60);
                // Nothing in the way: the boot and every timestamp must succeed.
                let healthy = handle.identity().is_ok() && disk.borrow().fault.is_none();
                let opened = TimestampClock::open(handle, *wall);
                prop_assert!(opened.is_ok() || !healthy, "boot failed on a healthy store");
                if let Ok(mut clock) = opened {
                    prop_assert!(stored(&disk).2 > floor, "boot reused an epoch");
                    for _ in 0..*count {
                        let Ok(stamp) = clock.next_timestamp() else {
                            prop_assert!(!healthy, "timestamp refused on a healthy store");
                            break;
                        };
                        prop_assert!(
                            last.is_none_or(|last| stamp > last),
                            "timestamp repeated or went back"
                        );
                        last = Some(stamp);
                        seen.timestamps += 1;
                    }
                    store = Some(clock.into_store());
                }
            }
            Op::Begin { host: first } => {
                let handle = store.as_mut().unwrap();
                let had_journal = disk.borrow().journal.is_some();
                let result = handle.begin_node_key_rotation(host(*first), 443);
                if !had_journal && disk.borrow().journal.is_some() {
                    journal_host = Some(*first);
                }
                if let Ok(pending) = result {
                    let key = pending.candidate().node_key().public();
                    // A pending rotation keeps its candidate; no second one is made.
                    prop_assert!(
                        candidate.is_none_or(|known| known == key),
                        "a new candidate replaced the pending one"
                    );
                    prop_assert!(
                        disk.borrow().journal.is_some(),
                        "candidate exposed before its journal was stored"
                    );
                    candidate = Some(key);
                    seen.pending += 1;
                }
            }
            Op::Complete {
                host: first,
                authorized,
            } => {
                let handle = store.as_mut().unwrap();
                let mut enrollment = Enrollment::default();
                if let (true, Some(key)) = (*authorized, candidate) {
                    enrollment
                        .apply(key, 200, br#"{"MachineAuthorized":true}"#)
                        .unwrap();
                }
                if let Some(key) = candidate {
                    // The write may land even when it is reported as failed.
                    if *authorized && journal_host == Some(*first) && !was_poisoned {
                        possible.push(key);
                    }
                }
                let result = handle.complete_node_key_rotation(host(*first), 443, &enrollment);
                if result.is_ok() {
                    prop_assert!(
                        *authorized && journal_host == Some(*first),
                        "rotation completed without authorization"
                    );
                    let key = candidate.take().unwrap();
                    prop_assert_eq!(stored(&disk).0, key);
                    prop_assert!(disk.borrow().journal.is_none());
                    possible = vec![key];
                    seen.completed += 1;
                }
            }
            Op::Install => {
                let handle = store.as_mut().unwrap();
                if let Ok(identity) = handle.identity() {
                    let rotation = identity.prepare_node_key_rotation().unwrap();
                    let key = rotation.replacement().public();
                    possible.push(key);
                    if handle.install_node_key_rotation(&rotation).is_ok() {
                        prop_assert_eq!(stored(&disk).0, key);
                        possible = vec![key];
                        seen.installed += 1;
                    }
                }
            }
        }
        if was_poisoned {
            prop_assert_eq!(disk.borrow().writes, writes, "a poisoned handle wrote");
        }

        // A fresh start always works, whatever failed before.
        if was_poisoned {
            // One step on a poisoned handle is enough; start again.
            store = None;
        }
        if disk.borrow().journal.is_none() {
            candidate = None;
            journal_host = None;
        }
        let handle = match store.take() {
            Some(handle) => handle,
            None => {
                let fault = disk.borrow_mut().fault.take();
                let reopened = IdentityStore::open(Memory(disk.clone()));
                disk.borrow_mut().fault = fault;
                prop_assert!(
                    reopened.is_ok(),
                    "reopen failed: {:?}",
                    reopened.err().map(|e| e.to_string())
                );
                let reopened = reopened.unwrap();
                // A journal on disk is the one whose candidate was handed out.
                reopened
            }
        };
        let (node, stored_machine, epoch) = stored(&disk);
        prop_assert_eq!(stored_machine, machine, "machine key changed");
        prop_assert!(
            possible.contains(&node),
            "record holds a node key that was never written"
        );
        prop_assert!(
            epoch >= durable,
            "epoch on disk went back from {} to {}",
            durable,
            epoch
        );
        durable = epoch;
        prop_assert!(
            epoch <= asked,
            "epoch {} was never asked for (max {})",
            epoch,
            asked
        );
        // What a usable handle shows is what the record holds.
        match handle.identity() {
            Ok(identity) => {
                prop_assert_eq!(identity.node_key().public(), node);
                prop_assert!(disk.borrow().journal.is_none());
            }
            Err(Error::Poisoned) => seen.poisoned += 1,
            Err(Error::RotationPending) => prop_assert!(disk.borrow().journal.is_some()),
            Err(error) => prop_assert!(false, "unexpected identity error: {error}"),
        }
        store = Some(handle);
    }
    Ok(seen)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]
    #[test]
    fn store_survives_failed_writes_and_restarts(ops in prop::collection::vec(op(), 1..30)) {
        run(&ops)?;
    }
}

/// What the generator reaches. Run with `--ignored --nocapture`.
#[test]
#[ignore = "prints generator coverage"]
fn store_generator_coverage() {
    use proptest::{strategy::ValueTree, test_runner::TestRunner};
    let mut runner = TestRunner::deterministic();
    let mut total = Seen::default();
    for _ in 0..300 {
        let ops = prop::collection::vec(op(), 1..30)
            .new_tree(&mut runner)
            .unwrap()
            .current();
        let seen = run(&ops).unwrap();
        total.poisoned += seen.poisoned;
        total.pending += seen.pending;
        total.completed += seen.completed;
        total.installed += seen.installed;
        total.timestamps += seen.timestamps;
    }
    println!("{total:?}");
}
