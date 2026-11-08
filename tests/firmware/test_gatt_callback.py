"""Exercise the production callback with ESP-IDF's null unregister payload."""
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HARNESS = r'''
#![allow(non_camel_case_types, non_upper_case_globals)]
use std::sync::Mutex;
type esp_gattc_cb_event_t = u32;
type esp_gatt_if_t = u8;
const esp_gattc_cb_event_t_ESP_GATTC_UNREG_EVT: u32 = 1;
struct esp_ble_gattc_cb_param_t { value: u16 }
#[derive(Debug, PartialEq, Eq)]
enum GattcEvent { ClientUnregistered, Payload(u16) }
impl From<(u32, &esp_ble_gattc_cb_param_t)> for GattcEvent {
    fn from((_, p): (u32, &esp_ble_gattc_cb_param_t)) -> Self { Self::Payload(p.value) }
}
static EVENTS: Mutex<Vec<(u8, GattcEvent)>> = Mutex::new(Vec::new());
struct Singleton;
static SINGLETON: Singleton = Singleton;
impl Singleton { fn call(&self, event: (u8, GattcEvent)) { EVENTS.lock().unwrap().push(event); } }
macro_rules! trace { ($($args:tt)*) => {}; }
macro_rules! warn { ($($args:tt)*) => {}; }
__CALLBACK__
fn main() {
    unsafe { event_handler(1, 4, std::ptr::null_mut()); }
    assert_eq!(*EVENTS.lock().unwrap(), [(4, GattcEvent::ClientUnregistered)]);
    let mut payload = esp_ble_gattc_cb_param_t { value: 37 };
    unsafe { event_handler(2, 5, &mut payload); }
    unsafe { event_handler(2, 6, std::ptr::null_mut()); }
    assert_eq!(*EVENTS.lock().unwrap(), [(4, GattcEvent::ClientUnregistered), (5, GattcEvent::Payload(37))]);
}
'''


class GattCallbackTests(unittest.TestCase):
    def test_production_callback_handles_null_unregister_payload(self):
        source = (ROOT / 'vendor/esp-idf-svc/src/bt/ble/gatt/client.rs').read_text()
        start = source.index('    unsafe extern "C" fn event_handler(')
        end = source.index('\n}\n\nimpl', start)
        production = source[start:end]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'callback.rs'
            binary = Path(directory) / 'callback'
            path.write_text(HARNESS.replace('__CALLBACK__', production))
            subprocess.run(['rustc', '--edition=2024', str(path), '-o', str(binary)], check=True)
            subprocess.run([str(binary)], check=True)
