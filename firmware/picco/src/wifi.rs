use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::modem::WifiModemPeripheral,
    sys,
    wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi},
};
use femto_picco_board::wifi::{Action, Credentials, MAX_CONFIG_BYTES, Retry};
use std::{
    fs::File,
    io::Read,
    time::{Duration, Instant},
};
use zeroize::{Zeroize, Zeroizing};

pub struct Station {
    clock: Option<crate::clock::Service>,
    next_clock_attempt: Instant,
    driver: EspWifi<'static>,
    retry: Retry,
    started: Instant,
    next_poll: Instant,
}

impl Station {
    pub fn open(modem: impl WifiModemPeripheral + 'static) -> Result<Option<Self>, String> {
        let file = match File::open("/sdcard/femto/wifi.json") {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Cannot open Wi-Fi configuration".into()),
        };
        if !file
            .metadata()
            .map_err(|_| "Cannot inspect Wi-Fi configuration")?
            .is_file()
        {
            return Err("Wi-Fi configuration must be a regular file".into());
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.take((MAX_CONFIG_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read Wi-Fi configuration")?;
        let credentials = Credentials::parse(&bytes)
            .map_err(|error| format!("Invalid Wi-Fi configuration: {error:?}"))?;
        let event_loop = EspSystemEventLoop::take().map_err(|e| e.to_string())?;
        let mut driver = EspWifi::new(modem, event_loop, None).map_err(|e| e.to_string())?;
        // Credentials must not modify the vendor NVS partition.
        sys::esp!(unsafe { sys::esp_wifi_set_storage(sys::wifi_storage_t_WIFI_STORAGE_RAM) })
            .map_err(|e| e.to_string())?;
        let mut configuration = Configuration::Client(ClientConfiguration {
            ssid: credentials
                .ssid()
                .try_into()
                .map_err(|_| "Invalid SSID length")?,
            password: credentials
                .password()
                .try_into()
                .map_err(|_| "Invalid password length")?,
            auth_method: AuthMethod::WPA2Personal,
            ..Default::default()
        });
        let configured = driver.set_configuration(&configuration);
        // Zero bytes preserve UTF-8 validity. Native SDK copies remain until driver teardown.
        if let Configuration::Client(config) = &mut configuration {
            unsafe {
                config.password.as_mut_str().as_bytes_mut().zeroize();
                config.ssid.as_mut_str().as_bytes_mut().zeroize();
            }
        }
        configured.map_err(|e| e.to_string())?;
        driver.start().map_err(|e| e.to_string())?;
        let now = Instant::now();
        Ok(Some(Self {
            clock: None,
            next_clock_attempt: now,
            driver,
            retry: Retry::default(),
            started: now,
            next_poll: now,
        }))
    }

    pub fn poll(&mut self) {
        let now = Instant::now();
        if now < self.next_poll {
            return;
        }
        self.next_poll = now + Duration::from_millis(250);
        let elapsed = now
            .duration_since(self.started)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let up = self.driver.is_up().unwrap_or(false);
        if up && self.clock.is_none() && now >= self.next_clock_attempt {
            self.next_clock_attempt = now + Duration::from_secs(60);
            match crate::clock::Service::start() {
                Ok(clock) => self.clock = Some(clock),
                Err(error) => log::warn!("{error}"),
            }
        }
        match self.retry.poll(elapsed, up) {
            Action::None => {}
            Action::Connect => {
                if let Err(error) = self.driver.connect() {
                    log::warn!("Wi-Fi connection attempt failed: {error}");
                    self.retry.failed(elapsed);
                }
            }
            Action::Disconnect => {
                let _ = self.driver.disconnect();
            }
        }
    }

    pub fn address(&self) -> Option<std::net::Ipv4Addr> {
        if !self.driver.is_up().ok()? {
            return None;
        }
        self.driver
            .sta_netif()
            .get_ip_info()
            .ok()
            .map(|info| info.ip)
    }

    pub fn label(&self) -> &'static str {
        if self.retry.is_online() && !crate::clock::ready() {
            "Wi-Fi online, time?"
        } else {
            self.retry.label()
        }
    }

    pub fn stop(&mut self) -> Result<(), String> {
        self.clock.take();
        self.driver
            .stop()
            .map_err(|e| format!("Wi-Fi shutdown: {e}"))
    }

    pub fn resume_after_sleep(&mut self) -> Result<(), String> {
        if !self.driver.is_started().map_err(|e| e.to_string())? {
            self.driver
                .start()
                .map_err(|e| format!("Wi-Fi restart: {e}"))?;
        }
        self.retry = Retry::default();
        self.started = Instant::now();
        self.next_poll = self.started;
        self.next_clock_attempt = self.started;
        Ok(())
    }
}
