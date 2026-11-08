"""Fault-inject the vendored production initialization and rollback bodies."""
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HARNESS = r'''
#![allow(non_upper_case_globals, unused_variables)]
use std::sync::Mutex;
const esp_bluedroid_status_t_ESP_BLUEDROID_STATUS_ENABLED: u32 = 2;
const esp_bluedroid_status_t_ESP_BLUEDROID_STATUS_INITIALIZED: u32 = 1;
const esp_bt_controller_status_t_ESP_BT_CONTROLLER_STATUS_ENABLED: u32 = 2;
const esp_bt_controller_status_t_ESP_BT_CONTROLLER_STATUS_INITED: u32 = 1;
struct Model { controller:u32, host:u32, host_tasks:bool, fault:&'static str, cleanup_fault:&'static str, calls:Vec<&'static str> }
static MODEL: Mutex<Model> = Mutex::new(Model { controller:0, host:0, host_tasks:false, fault:"", cleanup_fault:"", calls:Vec::new() });
fn op(name: &'static str, action: impl FnOnce(&mut Model)) -> i32 {
    let mut m = MODEL.lock().unwrap();
    m.calls.push(name);
    if name == m.fault {
        if name == "host_init" { m.host_tasks = true; }
        return -10;
    }
    if name == m.cleanup_fault { return -20; }
    action(&mut m);
    0
}
unsafe fn esp_bt_controller_init(_: &mut u32) -> i32 { op("controller_init", |m| { assert_eq!(m.controller,0); m.controller=1; }) }
unsafe fn esp_bt_controller_enable(_: u32) -> i32 { op("controller_enable", |m| { assert_eq!(m.controller,1); m.controller=2; }) }
unsafe fn esp_bluedroid_init() -> i32 { op("host_init", |m| { assert_eq!(m.controller,2); assert_eq!(m.host,0); m.host_tasks=true; m.host=1; }) }
unsafe fn esp_bluedroid_enable() -> i32 { op("host_enable", |m| { assert_eq!(m.host,1); m.host=2; }) }
unsafe fn esp_bluedroid_disable() -> i32 { op("host_disable", |m| { assert_eq!(m.host,2); m.host=1; }) }
unsafe fn esp_bluedroid_deinit() -> i32 { op("host_deinit", |m| { assert_eq!(m.host,1); m.host=0; m.host_tasks=false; }) }
unsafe fn esp_bt_controller_disable() -> i32 { op("controller_disable", |m| { assert!(!m.host_tasks); assert_eq!(m.controller,2); m.controller=1; }) }
unsafe fn esp_bt_controller_deinit() -> i32 { op("controller_deinit", |m| { assert!(!m.host_tasks); assert_eq!(m.controller,1); m.controller=0; }) }
unsafe fn esp_bluedroid_get_status() -> u32 { MODEL.lock().unwrap().host }
unsafe fn esp_bt_controller_get_status() -> u32 { MODEL.lock().unwrap().controller }
macro_rules! esp { ($call:expr) => { { let code = $call; if code==0 { Ok::<(),i32>(()) } else { Err(code) } } }; }
macro_rules! info { ($($args:tt)*) => {}; }
macro_rules! warn { ($($args:tt)*) => {}; }
struct M;
impl M { fn mode() -> u32 { 1 } }
__ROLLBACK__
fn init() -> Result<(),i32> { let mut bt_cfg = 0; __INIT__ }
fn reset(fault: &'static str, cleanup_fault: &'static str) {
    *MODEL.lock().unwrap() = Model { controller:0, host:0, host_tasks:false, fault, cleanup_fault, calls:Vec::new() };
}
fn main() {
    for (fault, expected, controller, host) in [
        ("controller_init", vec!["controller_init"], 0, 0),
        ("controller_enable", vec!["controller_init","controller_enable","controller_deinit"], 0, 0),
        ("host_init", vec!["controller_init","controller_enable","host_init"], 2, 0),
        ("host_enable", vec!["controller_init","controller_enable","host_init","host_enable","host_deinit","controller_disable","controller_deinit"], 0, 0),
    ] {
        reset(fault, "");
        assert_eq!(init(), Err(-10));
        let m = MODEL.lock().unwrap();
        assert_eq!(m.calls, expected);
        assert_eq!((m.controller,m.host), (controller,host));
        assert_eq!(m.host_tasks, fault=="host_init");
    }
    // Cleanup failure preserves the original error and cannot free dependencies.
    for (cleanup, controller, host) in [("host_deinit",2,1),("controller_disable",2,0),("controller_deinit",1,0)] {
        reset("host_enable", cleanup);
        assert_eq!(init(), Err(-10));
        let m=MODEL.lock().unwrap();
        assert_eq!((m.controller,m.host), (controller,host));
        assert_eq!(m.calls.last(), Some(&cleanup));
    }
    // A controller initialization rejection must not clean up another owner.
    reset("controller_init", "");
    MODEL.lock().unwrap().controller=2;
    assert_eq!(init(), Err(-10));
    assert_eq!(MODEL.lock().unwrap().controller,2);
    // Successful creation leaves the complete stack alive for BtDriver ownership.
    reset("", "");
    assert_eq!(init(), Ok(()));
    assert_eq!(MODEL.lock().unwrap().calls, ["controller_init","controller_enable","host_init","host_enable"]);
    // Successfully clean an enabled controller, with and without an enabled host.
    for host in [false,true] {
        reset("", "");
        { let mut m=MODEL.lock().unwrap(); m.controller=2; m.host=if host {2} else {0}; m.host_tasks=host; }
        drop(BtInitRollback { host_started:host, host_initialized:host, committed:false });
        let m=MODEL.lock().unwrap();
        assert_eq!((m.controller,m.host,m.host_tasks), (0,0,false));
        let expected = if host { vec!["host_disable","host_deinit","controller_disable","controller_deinit"] }
            else { vec!["controller_disable","controller_deinit"] };
        assert_eq!(m.calls, expected);
    }
    // Defensive cleanup also handles enabled host state and its failure.
    reset("", "host_disable");
    { let mut m=MODEL.lock().unwrap(); m.controller=2; m.host=2; m.host_tasks=true; }
    drop(BtInitRollback { host_started:true, host_initialized:true, committed:false });
    assert_eq!(MODEL.lock().unwrap().calls, ["host_disable"]);
    assert_eq!(MODEL.lock().unwrap().controller,2);
    for host in [false,true] {
        reset("", "");
        { let mut m=MODEL.lock().unwrap(); m.controller=3; m.host=3; }
        drop(BtInitRollback { host_started:host, host_initialized:host, committed:false });
        assert!(MODEL.lock().unwrap().calls.is_empty());
    }
}
'''


class BtInitTests(unittest.TestCase):
    def test_production_initialization_rolls_back_owned_stages_or_retains_uncertain_state(self):
        source = (ROOT / 'vendor/esp-idf-svc/src/bt.rs').read_text()
        start = source.index('        info!("Init bluetooth controller");')
        end = source.index('\n    }', start)
        init = source[start:end]
        start = source.index('struct BtInitRollback {')
        end = source.index("impl<M> Drop for BtDriver", start)
        rollback = source[start:end]
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'init.rs'
            binary = Path(directory) / 'init'
            path.write_text(HARNESS.replace('__INIT__', init).replace('__ROLLBACK__', rollback))
            subprocess.run(['rustc', '--edition=2024', str(path), '-o', str(binary)], check=True)
            subprocess.run([str(binary)], check=True)
