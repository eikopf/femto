"""Run production sleep preparation against fallible driver models."""
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HARNESS = r'''
#![allow(unused_variables)]
extern crate self as log;
#[macro_export] macro_rules! warn { ($($args:tt)*) => {}; }
use std::{cell::RefCell, rc::Rc, time::Duration};
struct Model { fault: &'static str, calls: Vec<&'static str> }
type Shared = Rc<RefCell<Model>>;
fn call(model: &Shared, name: &'static str) -> Result<(), String> {
    let mut m = model.borrow_mut();
    m.calls.push(name);
    if m.fault == name { Err(name.into()) } else { Ok(()) }
}
struct Worker(Shared, &'static str);
impl Worker {
    fn close_and_wait(&mut self, _: Duration) -> bool { call(&self.0, self.1).is_ok() }
    fn cancel_and_wait(&mut self, _: Duration) -> bool { call(&self.0, self.1).is_ok() }
}
struct Bluetooth(Shared);
impl Bluetooth {
    fn stop_and_wait(&mut self, _: Duration) -> Result<bool, String> {
        call(&self.0, "bluetooth")?;
        Ok(self.0.borrow().fault != "bluetooth-timeout")
    }
    fn resume_after_sleep(&mut self) { call(&self.0, "bluetooth-resume").unwrap(); }
}
struct Display(Shared);
impl Display {
    fn sleep(&mut self) -> Result<(), String> { call(&self.0, "panel") }
    fn initialize(&mut self) -> Result<(), String> { call(&self.0, "panel-resume") }
}
struct Wifi(Shared);
impl Wifi {
    fn stop(&mut self) -> Result<(), String> { call(&self.0, "wifi") }
    fn resume_after_sleep(&mut self) -> Result<(), String> { call(&self.0, "wifi-resume") }
}
struct Screen {
    tailnet: Worker, catalog: Worker, bluetooth: Option<Bluetooth>,
    display: Display, wifi: Option<Wifi>, fast_count: u8, model: Shared,
}
impl Screen {
    fn set_light(&mut self, _: u8, _: u8) -> Result<(), String> { call(&self.model, "light") }
__PREPARE__
}
#[test]
fn failed_sleep_retains_bluetooth_and_restores_touched_drivers() {
    for fault in ["tailnet", "catalog", "bluetooth-timeout", "bluetooth", "light", "panel", "wifi", ""] {
        let model = Rc::new(RefCell::new(Model { fault, calls: vec![] }));
        let mut screen = Screen {
            tailnet: Worker(model.clone(), "tailnet"), catalog: Worker(model.clone(), "catalog"),
            bluetooth: Some(Bluetooth(model.clone())), display: Display(model.clone()),
            wifi: Some(Wifi(model.clone())), fast_count: 0, model: model.clone(),
        };
        assert_eq!(screen.prepare_sleep().is_ok(), fault.is_empty(), "{fault}");
        assert!(screen.bluetooth.is_some(), "owner must survive until sleep commits");
        let calls = &model.borrow().calls;
        if ["tailnet", "catalog"].contains(&fault) {
            assert!(!calls.contains(&"bluetooth"));
            assert!(!calls.contains(&"light"));
        } else if !fault.is_empty() {
            assert!(calls.contains(&"bluetooth-resume"), "{fault}: {calls:?}");
        } else {
            assert!(!calls.contains(&"bluetooth-resume"));
        }
        assert_eq!(calls.contains(&"panel-resume"), ["panel", "wifi"].contains(&fault));
        assert_eq!(calls.contains(&"wifi-resume"), fault == "wifi");
        if ["panel", "wifi"].contains(&fault) { assert_eq!(screen.fast_count, 5); }
    }
}
'''


class SleepRollbackTests(unittest.TestCase):
    def test_production_sleep_restores_after_failure(self):
        panel = (ROOT / 'firmware/picco/src/panel.rs').read_text()
        prepare = panel.split('    pub fn prepare_sleep', 1)[1].split('    pub fn poll_wifi', 1)[0]
        prepare = '    pub fn prepare_sleep' + prepare
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / 'sleep.rs'
            binary = Path(directory) / 'sleep'
            source.write_text(HARNESS.replace('__PREPARE__', prepare))
            subprocess.run(['rustc', '--edition=2024', '--cfg', 'feature="bluetooth"', '--test', str(source), '-o', str(binary)], check=True)
            subprocess.run([str(binary)], check=True)
