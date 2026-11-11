"""Run the production reconnect owner with a host model of driver ownership."""
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HARNESS = r'''
#![allow(dead_code, non_snake_case, non_upper_case_globals)]
extern crate self as esp_idf_svc;
extern crate self as femto_remote;
extern crate self as femto_reader;
extern crate self as log;
#[macro_export] macro_rules! info { ($($args:tt)*) => {}; }
#[path="__POLICY__"] pub mod reconnect;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
static LIVE: AtomicUsize = AtomicUsize::new(0);
static OPENS: AtomicUsize = AtomicUsize::new(0);
static RELEASED: AtomicBool = AtomicBool::new(true);
static KEEP_OCCUPIED: AtomicBool = AtomicBool::new(false);
static FAIL_OPEN: AtomicBool = AtomicBool::new(false);
static STEALS: AtomicUsize = AtomicUsize::new(0);
pub mod discovery { #[derive(Clone, Copy)] pub struct Peer; }
pub mod state { pub struct ReaderState; }
pub mod remote { pub struct PageTurn; }
pub mod hal { pub mod modem {
    pub struct BluetoothModem<'a>(pub std::marker::PhantomData<&'a mut ()>);
    impl BluetoothModem<'_> { pub unsafe fn steal() -> Self {
        assert_eq!(crate::LIVE.load(crate::SeqCst), 0);
        assert!(crate::RELEASED.load(crate::SeqCst));
        crate::STEALS.fetch_add(1, crate::SeqCst);
        Self(std::marker::PhantomData)
    } }
} }
pub mod sys {
    pub const esp_bluedroid_status_t_ESP_BLUEDROID_STATUS_UNINITIALIZED: u32 = 0;
    pub const esp_bt_controller_status_t_ESP_BT_CONTROLLER_STATUS_IDLE: u32 = 0;
    pub unsafe fn esp_bluedroid_get_status() -> u32 { u32::from(!crate::RELEASED.load(crate::SeqCst)) }
    pub unsafe fn esp_bt_controller_get_status() -> u32 { u32::from(!crate::RELEASED.load(crate::SeqCst)) }
}
mod bluetooth {
    pub mod probe {
        pub struct Probe { pub done: bool, pub fail: bool, pub stopped: bool }
        impl Probe {
            pub fn open(_: crate::hal::modem::BluetoothModem<'static>, _: crate::discovery::Peer) -> Result<Self,String> {
                assert_eq!(crate::LIVE.load(crate::SeqCst), 0);
                if crate::FAIL_OPEN.load(crate::SeqCst) { return Err("init failed".into()); }
                crate::LIVE.fetch_add(1, crate::SeqCst);
                crate::OPENS.fetch_add(1, crate::SeqCst);
                crate::RELEASED.store(false, crate::SeqCst);
                Ok(Self { done:false, fail:false, stopped:false })
            }
            pub fn stop(&mut self) { self.stopped = true; }
            pub fn poll(&mut self) -> Result<bool,String> {
                if self.fail { Err("shutdown failed".into()) } else { Ok(self.done) }
            }
            pub fn is_ready(&self) -> bool { !self.done && !self.stopped }
            pub fn poll_input(&mut self, _: &crate::state::ReaderState, _: bool) -> Result<Option<crate::remote::PageTurn>,String> { Ok(None) }
        }
        impl Drop for Probe { fn drop(&mut self) {
            assert_eq!(crate::LIVE.fetch_sub(1, crate::SeqCst), 1);
            crate::RELEASED.store(!crate::KEEP_OCCUPIED.load(crate::SeqCst), crate::SeqCst);
        } }
    }
    mod owner {
__OWNER__
        #[test]
        fn production_owner_orders_teardown_and_retry_and_cancels_sleep() {
            use std::time::Duration;
            use crate::{OPENS,LIVE,STEALS,KEEP_OCCUPIED,RELEASED,FAIL_OPEN,SeqCst};
            let open = || Reconnecting::open(crate::hal::modem::BluetoothModem(std::marker::PhantomData), Peer).unwrap();
            let mut owner = open();
            let base = owner.origin + Duration::from_secs(1);
            assert!(!owner.poll_at(base).unwrap());
            assert_eq!(LIVE.load(SeqCst), 1);
            owner.attempt.as_mut().unwrap().done = true;
            assert!(!owner.poll_at(base).unwrap());
            assert_eq!(LIVE.load(SeqCst), 0);
            assert!(!owner.poll_at(base + Duration::from_millis(1999)).unwrap());
            assert_eq!(OPENS.load(SeqCst), 1);
            assert!(!owner.poll_at(base + Duration::from_secs(2)).unwrap());
            assert_eq!(OPENS.load(SeqCst), 2);
            assert_eq!(STEALS.load(SeqCst), 1);
            owner.stop();
            assert!(owner.attempt.as_ref().unwrap().stopped);
            assert!(!owner.poll_at(base + Duration::from_secs(3)).unwrap());
            owner.attempt.as_mut().unwrap().done = true;
            assert!(owner.poll_at(base + Duration::from_secs(4)).unwrap());
            assert!(owner.poll_at(base + Duration::from_secs(200)).unwrap());
            drop(owner);
            // Aborted sleep resumes retries only after the old owner releases the SDK.
            for completed in [false, true] {
                let mut owner = open();
                owner.stop();
                if completed {
                    owner.attempt.as_mut().unwrap().done = true;
                    assert!(owner.poll_at(owner.origin).unwrap());
                }
                let opens = OPENS.load(SeqCst);
                owner.resume_after_sleep();
                if !completed {
                    assert!(!owner.poll_at(owner.origin + Duration::from_secs(50)).unwrap());
                    assert_eq!(OPENS.load(SeqCst), opens);
                    owner.attempt.as_mut().unwrap().done = true;
                    assert!(!owner.poll_at(owner.origin + Duration::from_secs(51)).unwrap());
                }
                assert!(!owner.poll_at(owner.origin + Duration::from_secs(100)).unwrap());
                assert_eq!(OPENS.load(SeqCst), opens + 1);
                assert_eq!(LIVE.load(SeqCst), 1);
                drop(owner);
            }
            // Cancelling during backoff creates no replacement driver.
            let mut owner = open();
            owner.attempt.as_mut().unwrap().done = true;
            owner.poll_at(owner.origin + Duration::from_secs(1)).unwrap();
            owner.stop();
            assert!(owner.poll_at(owner.origin + Duration::from_secs(200)).unwrap());
            drop(owner);
            // A failed acknowledgement drain must not retry.
            let mut owner = open();
            owner.attempt.as_mut().unwrap().fail = true;
            owner.stop();
            assert!(owner.poll_at(owner.origin).is_err());
            owner.resume_after_sleep();
            assert!(owner.retry.is_stopped());
            drop(owner);
            // A driver that did not release the SDK cannot be replaced.
            let mut owner = open();
            KEEP_OCCUPIED.store(true, SeqCst);
            owner.attempt.as_mut().unwrap().done = true;
            assert!(owner.poll_at(owner.origin).is_err());
            assert!(owner.retry.is_stopped());
            drop(owner);
            KEEP_OCCUPIED.store(false, SeqCst);
            RELEASED.store(true, SeqCst);
            // Reinitialization failure is terminal, not an unbounded init loop.
            let mut owner = open();
            owner.attempt.as_mut().unwrap().done = true;
            owner.poll_at(owner.origin + Duration::from_secs(1)).unwrap();
            FAIL_OPEN.store(true, SeqCst);
            assert!(owner.poll_at(owner.origin + Duration::from_secs(10)).is_err());
            assert!(owner.poll_at(owner.origin + Duration::from_secs(200)).unwrap());
            assert_eq!(LIVE.load(SeqCst), 0);
        }
    }
}
'''


class BleReconnectTests(unittest.TestCase):
    def test_production_owner_orders_driver_lifetimes_and_stops_on_failure(self):
        owner = (ROOT / 'firmware/picco/src/bluetooth/reconnect.rs').read_text()
        policy = ROOT / 'crates/femto-remote/src/reconnect.rs'
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / 'reconnect.rs'
            binary = Path(directory) / 'reconnect'
            source.write_text(HARNESS.replace('__OWNER__', owner).replace('__POLICY__', str(policy)))
            subprocess.run(['rustc', '--edition=2024', '--test', str(source), '-o', str(binary)], check=True)
            subprocess.run([str(binary)], check=True)
