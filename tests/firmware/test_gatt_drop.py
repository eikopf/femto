"""Run the vendored destructor against a host model of the pinned SDK callback API."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]

HARNESS = r'''
#![allow(non_camel_case_types, dead_code)]
use std::{borrow::Borrow, marker::PhantomData, sync::Mutex};
type esp_gattc_cb_event_t = u32;
type esp_gatt_if_t = u8;
#[repr(C)] struct esp_ble_gattc_cb_param_t { unused: u8 }
type Callback = unsafe extern "C" fn(esp_gattc_cb_event_t, esp_gatt_if_t, *mut esp_ble_gattc_cb_param_t);
static EVENTS: Mutex<Vec<&str>> = Mutex::new(Vec::new());
static CALLBACK: Mutex<Option<Callback>> = Mutex::new(None);
trait BleEnabled {}
struct Ble;
impl BleEnabled for Ble {}
struct BtDriver<'d, M: BleEnabled>(PhantomData<&'d M>);
impl<M: BleEnabled> Drop for BtDriver<'_, M> {
    fn drop(&mut self) { EVENTS.lock().unwrap().push("driver"); }
}
struct EspGattc<'d, M: BleEnabled, T: Borrow<BtDriver<'d, M>>> {
    driver: T,
    marker: PhantomData<&'d M>,
}
impl<'d, M: BleEnabled, T: Borrow<BtDriver<'d, M>>> EspGattc<'d, M, T> {
    fn unsubscribe(&self) -> Result<(), ()> {
        EVENTS.lock().unwrap().push("unsubscribe");
        Ok(())
    }
}
struct Singleton;
static SINGLETON: Singleton = Singleton;
impl Singleton {
    fn release(&self) -> Result<(), ()> {
        EVENTS.lock().unwrap().push("release");
        Ok(())
    }
}
unsafe fn esp_ble_gattc_register_callback(callback: Option<Callback>) -> i32 {
    if callback.is_none() { return -1; } // ESP-IDF 5.5.2 esp_gattc_api.c.
    *CALLBACK.lock().unwrap() = callback;
    EVENTS.lock().unwrap().push("replace callback");
    0
}
macro_rules! esp {
    ($call:expr) => { if $call == 0 { Ok::<(), ()>(()) } else { Err(()) } };
}
__PRODUCTION_DROP__
fn main() {
    let client = EspGattc { driver: BtDriver::<Ble>(PhantomData), marker: PhantomData };
    drop(client);
    assert_eq!(*EVENTS.lock().unwrap(), ["unsubscribe", "replace callback", "release", "driver"]);
    let callback = CALLBACK.lock().unwrap().unwrap();
    // A late callback needs no owner and does not dereference the event pointer.
    unsafe { callback(0, 0, std::ptr::null_mut()); }
    assert_eq!(EVENTS.lock().unwrap().len(), 4);
}
'''


class GattDropTests(unittest.TestCase):
    def test_production_destructor_replaces_callback_before_driver_release(self):
        source = (ROOT / "vendor/esp-idf-svc/src/bt/ble/gatt/client.rs").read_text()
        start = source.index("impl<'d, M, T> Drop for EspGattc")
        end = source.index("unsafe impl<'d, M, T> Send for EspGattc", start)
        production = source[start:end]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, body, success in (
                ("patched", production, True),
                ("upstream_null", production.replace("Some(discard_event)", "None"), False),
            ):
                path = root / f"{name}.rs"
                executable = root / name
                path.write_text(HARNESS.replace("__PRODUCTION_DROP__", body))
                compile_result = subprocess.run(
                    ["rustc", "--edition", "2021", str(path), "-o", str(executable)],
                    capture_output=True, text=True, timeout=60)
                self.assertEqual(compile_result.returncode, 0, compile_result.stderr)
                result = subprocess.run([str(executable)], capture_output=True, text=True,
                                        timeout=10, env=dict(os.environ, RUST_BACKTRACE="0"))
                self.assertEqual(result.returncode == 0, success, result.stderr)
                if not success:
                    self.assertIn("called `Result::unwrap()` on an `Err` value", result.stderr)
