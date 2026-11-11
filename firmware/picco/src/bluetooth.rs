#[cfg(not(esp_idf_bt_bluedroid_enabled))]
compile_error!("The bluetooth feature requires sdkconfig.bluetooth via scripts/build-picco");
#[cfg(not(esp_idf_bt_ble_42_scan_en))]
compile_error!("The Bluetooth scanner requires the BLE 4.2 legacy scan APIs");
#[cfg(any(esp_idf_bt_gattc_cache_nvs_flash, esp_idf_bt_ble_smp_bond_nvs_flash))]
compile_error!("The scan backend requires GATT cache and bond NVS persistence disabled");

mod probe;
mod reconnect;
use reconnect::Reconnecting;

use esp_idf_svc::{
    bt::{
        Ble, BtDriver, BtStatus,
        ble::gap::{BleGapEvent, EspBleGap, GapSearchEvent, ScanDuplicate, ScanParams},
    },
    hal::modem::BluetoothModem,
};
use femto_remote::scan::Inventory;
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

type Gap = EspBleGap<'static, Ble, Arc<BtDriver<'static, Ble>>>;

pub enum Service {
    Scan(Scanner),
    Probe(Reconnecting),
}

impl Service {
    pub fn open(modem: BluetoothModem<'static>) -> Result<Option<Self>, String> {
        if let Some(peer) = probe::load_peer()? {
            return Reconnecting::open(modem, peer).map(|probe| Some(Self::Probe(probe)));
        }
        Scanner::open(modem).map(|scanner| scanner.map(Self::Scan))
    }

    pub fn stop_and_wait(&mut self, timeout: Duration) -> Result<bool, String> {
        let Self::Probe(probe) = self else {
            return Ok(true);
        };
        probe.stop();
        let deadline = Instant::now() + timeout;
        loop {
            if probe.poll()? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn poll(&mut self) -> Result<bool, String> {
        match self {
            Self::Scan(scanner) => scanner.poll(),
            Self::Probe(probe) => probe.poll(),
        }
    }

    pub fn resume_after_sleep(&mut self) {
        if let Self::Probe(probe) = self {
            probe.resume_after_sleep();
        }
    }

    pub fn poll_input(
        &mut self,
        reader: &femto_reader::state::ReaderState,
        allow_turn: bool,
    ) -> Result<Option<femto_reader::remote::PageTurn>, String> {
        match self {
            Self::Scan(_) => Ok(None),
            Self::Probe(probe) => probe.poll_input(reader, allow_turn),
        }
    }
}

#[derive(Default)]
struct State {
    inventory: Inventory,
    configured: bool,
    done: bool,
    failed: bool,
    malformed: u32,
}

pub struct Scanner {
    gap: Gap,
    state: Arc<Mutex<State>>,
    requested: bool,
    deadline: Instant,
}

impl Scanner {
    pub fn open(modem: BluetoothModem<'static>) -> Result<Option<Self>, String> {
        let marker = match std::fs::symlink_metadata("/sdcard/femto/bluetooth-scan") {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("scan marker: {error}")),
        };
        if !marker.is_file() || marker.len() != 0 {
            return Err("Bluetooth scan marker must be an empty regular file".into());
        }
        let driver = Arc::new(BtDriver::new(modem, None).map_err(|e| e.to_string())?);
        let gap = Gap::new(driver).map_err(|e| e.to_string())?;
        let state = Arc::new(Mutex::new(State::default()));
        let callbacks = state.clone();
        gap.subscribe(move |event| {
            let mut state = callbacks.lock().unwrap_or_else(|error| error.into_inner());
            if state.done || state.failed {
                return;
            }
            match event {
                BleGapEvent::ScanParameterConfigured(status) => {
                    state.configured = status == BtStatus::Success;
                    state.failed = status != BtStatus::Success;
                }
                BleGapEvent::ScanStarted(status) => state.failed = status != BtStatus::Success,
                BleGapEvent::ScanStopped(status) => {
                    state.done = true;
                    state.failed = status != BtStatus::Success;
                }
                BleGapEvent::ScanResult(GapSearchEvent::InquiryComplete(_)) => state.done = true,
                BleGapEvent::ScanResult(GapSearchEvent::InquiryResult(result)) => {
                    let bytes = result.ble_adv.unwrap_or_default();
                    let advertising_len = usize::from(result.adv_data_len);
                    let response_len = usize::from(result.scan_rsp_len);
                    if advertising_len > 31
                        || response_len > 31
                        || bytes.len() != advertising_len + response_len
                    {
                        state.malformed = state.malformed.saturating_add(1);
                        return;
                    }
                    let (advertising, response) = bytes.split_at(advertising_len);
                    if state
                        .inventory
                        .record(
                            result.bda.addr(),
                            result.ble_addr_type as u8,
                            result.rssi.clamp(-128, 127) as i8,
                            advertising,
                            response,
                        )
                        .is_err()
                    {
                        state.malformed = state.malformed.saturating_add(1);
                    }
                }
                _ => {}
            }
        })
        .map_err(|e| e.to_string())?;
        let scanner = Self {
            gap,
            state,
            requested: false,
            deadline: Instant::now() + Duration::from_secs(15),
        };
        scanner
            .gap
            .set_scan_params(&ScanParams {
                scan_duplicate: ScanDuplicate::Enable,
                ..ScanParams::default()
            })
            .map_err(|e| e.to_string())?;
        Ok(Some(scanner))
    }

    pub fn poll(&mut self) -> Result<bool, String> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.failed {
            return Err("SDK rejected scan operation".into());
        }
        if state.done {
            let inventory = std::mem::take(&mut state.inventory);
            let malformed = state.malformed;
            drop(state);
            log::info!(
                "Bluetooth scan complete: {} devices, {} excess observations, {} malformed",
                inventory.entries().count(),
                inventory.dropped,
                malformed
            );
            for entry in inventory.entries() {
                log::info!(
                    "BLE {} type={} RSSI={} HID={} name={}",
                    esp_idf_svc::bt::BdAddr::from_bytes(entry.address),
                    entry.address_type,
                    entry.rssi,
                    entry.advertises_hid(),
                    entry.name().unwrap_or("(unnamed)")
                );
            }
            return Ok(true);
        }
        if Instant::now() >= self.deadline {
            state.done = true;
            return Err("scan completion deadline expired".into());
        }
        let start = state.configured && !self.requested;
        drop(state);
        if start {
            self.requested = true;
            self.deadline = Instant::now() + Duration::from_secs(15);
            self.gap.start_scanning(10).map_err(|e| e.to_string())?;
        }
        Ok(false)
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .done = true;
        let _ = self.gap.stop_scanning();
        let _ = self.gap.unsubscribe();
    }
}
