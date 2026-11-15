use esp_idf_svc::sys;
use femto_picco_board::console::{Command, Parser};

pub struct Console {
    parser: Parser,
    enabled: bool,
}

impl Console {
    pub fn open() -> Self {
        let driver_ready = unsafe {
            if sys::usb_serial_jtag_is_driver_installed() {
                true
            } else {
                let mut config = sys::usb_serial_jtag_driver_config_t {
                    tx_buffer_size: 256,
                    rx_buffer_size: 256,
                };
                sys::usb_serial_jtag_driver_install(&mut config) == sys::ESP_OK
            }
        };
        // ESP-IDF 5.5.2 nonblocking VFS reads inspect the driver's RX ring.
        // The default polling console has no ring and always reports no data.
        if driver_ready {
            unsafe { sys::esp_vfs_usb_serial_jtag_use_driver() };
        }
        let enabled = unsafe {
            let flags = sys::fcntl(0, sys::F_GETFL as _);
            driver_ready
                && flags >= 0
                && sys::fcntl(0, sys::F_SETFL as _, flags | sys::O_NONBLOCK as i32) == 0
        };
        log::info!("USB console enabled={enabled}");
        Self {
            parser: Parser::default(),
            enabled,
        }
    }

    pub fn poll(&mut self) -> Option<Command> {
        if !self.enabled {
            return None;
        }
        // Bound work per reader tick and retain unread commands in the USB FIFO.
        for _ in 0..32 {
            let mut byte = 0u8;
            let count = unsafe { sys::read(0, (&mut byte as *mut u8).cast(), 1) };
            if count != 1 {
                return None;
            }
            if let Some(command) = self.parser.push(byte) {
                return Some(command);
            }
        }
        None
    }
}
