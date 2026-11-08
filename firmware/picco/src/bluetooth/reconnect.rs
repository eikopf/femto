use super::probe::Probe;
use esp_idf_svc::{hal::modem::BluetoothModem, sys};
use femto_remote::{discovery::Peer, reconnect::Retry};
use std::time::Instant;

pub struct Reconnecting {
    attempt: Option<Probe>,
    peer: Peer,
    retry: Retry,
    origin: Instant,
}

impl Reconnecting {
    pub fn open(modem: BluetoothModem<'static>, peer: Peer) -> Result<Self, String> {
        Ok(Self {
            attempt: Some(Probe::open(modem, peer)?),
            peer,
            retry: Retry::default(),
            origin: Instant::now(),
        })
    }

    pub fn stop(&mut self) {
        self.retry.stop();
        if let Some(attempt) = &mut self.attempt {
            attempt.stop();
        }
    }

    pub fn poll(&mut self) -> Result<bool, String> {
        self.poll_at(Instant::now())
    }

    fn poll_at(&mut self, now: Instant) -> Result<bool, String> {
        let elapsed = now
            .saturating_duration_since(self.origin)
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if let Some(attempt) = &mut self.attempt {
            // Any failed cleanup is terminal. Do not start a new SDK generation.
            let finished = match attempt.poll() {
                Ok(finished) => finished,
                Err(error) => {
                    self.retry.stop();
                    return Err(error);
                }
            };
            self.retry.ready(elapsed, attempt.is_ready());
            if finished {
                // Probe owns the last driver reference. Drop deinitializes the
                // host and controller, including the callback task and queues.
                drop(self.attempt.take());
                if !stack_released() {
                    self.retry.stop();
                    return Err("Bluetooth stack was not fully released".into());
                }
                let released = self.origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                self.retry.released(elapsed.max(released));
            }
        }
        if self.attempt.is_none() && self.retry.is_stopped() {
            return Ok(true);
        }
        if self.retry.take_attempt(elapsed) {
            if !stack_released() {
                self.retry.stop();
                return Err("Bluetooth stack occupied before retry".into());
            }
            // SAFETY: This owner consumed the sole modem token at construction.
            // The previous Probe and driver have been destroyed, SDK status is
            // uninitialized, and this service cannot coexist with the scanner.
            // No other code may create a Bluetooth driver while it is alive.
            let modem = unsafe { BluetoothModem::steal() };
            match Probe::open(modem, self.peer) {
                Ok(attempt) => {
                    log::info!("Bluetooth reconnect attempt");
                    self.attempt = Some(attempt);
                }
                Err(error) => {
                    self.retry.stop();
                    return Err(error);
                }
            }
        }
        Ok(false)
    }

    pub fn poll_input(
        &mut self,
        reader: &femto_reader::state::ReaderState,
        allow_turn: bool,
    ) -> Result<Option<femto_reader::remote::PageTurn>, String> {
        match &mut self.attempt {
            Some(attempt) => attempt.poll_input(reader, allow_turn),
            None => Ok(None),
        }
    }
}

fn stack_released() -> bool {
    unsafe {
        sys::esp_bluedroid_get_status()
            == sys::esp_bluedroid_status_t_ESP_BLUEDROID_STATUS_UNINITIALIZED
            && sys::esp_bt_controller_get_status()
                == sys::esp_bt_controller_status_t_ESP_BT_CONTROLLER_STATUS_IDLE
    }
}
