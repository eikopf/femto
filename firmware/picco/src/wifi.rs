//! Wi-Fi station with networks chosen and remembered on the device.
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::modem::WifiModemPeripheral,
    sys,
    wifi::{
        AuthMethod, ClientConfiguration, Configuration, EspWifi, ScanMethod, config::ScanConfig,
    },
};
use femto_picco_board::wifi::{Action, Credentials, Retry, Saved};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use zeroize::Zeroize;

/// Internal settings key of the remembered networks.
const SETTING: &str = "wifi";
/// Internal settings key of the access point last joined: its channel, its
/// address, then the name of its network. None is secret.
const LAST: &str = "wifi_last";
const LEGACY_FILE: &str = "/sdcard/femto/wifi.json";
const MAX_VISIBLE: usize = 16;

pub struct Visible {
    pub ssid: String,
    pub secured: bool,
    pub signal: i8,
}

type Driver = EspWifi<'static>;
type Watch = esp_idf_svc::eventloop::EspSubscription<'static, esp_idf_svc::eventloop::System>;

pub struct Station {
    /// Creates the driver on first use; the radio stack stays unallocated
    /// while Wi-Fi is off.
    create: Option<Box<dyn FnOnce() -> Result<(Driver, Watch), String>>>,
    driver: Option<Driver>,
    _watch: Option<Watch>,
    /// Set by the driver when a connection attempt or the link ends.
    dropped: Arc<AtomicBool>,
    /// The target is the one access point last joined, taken without a scan
    /// to confirm it is in range or still the strongest.
    direct: bool,
    saved: Saved,
    visible: Vec<Visible>,
    scanning: bool,
    /// The network being joined or joined.
    target: Option<String>,
    online: bool,
    changed: bool,
    clock: Option<crate::clock::Service>,
    next_clock_attempt: Instant,
    retry: Retry,
    started: Instant,
    next_poll: Instant,
    next_scan: Instant,
    /// Whether modem sleep is off; see [`Self::set_attentive`].
    attentive: bool,
}

impl Station {
    pub fn open(modem: impl WifiModemPeripheral + 'static) -> Self {
        let saved = match load() {
            Ok(saved) => saved,
            Err(error) => {
                log::warn!("Saved Wi-Fi networks unavailable: {error}");
                Saved::default()
            }
        };
        let now = Instant::now();
        let dropped = Arc::new(AtomicBool::new(false));
        let observed = dropped.clone();
        let mut station = Self {
            create: Some(Box::new(move || {
                let event_loop = EspSystemEventLoop::take().map_err(|e| e.to_string())?;
                let watch = event_loop
                    .subscribe::<esp_idf_svc::wifi::WifiEvent, _>(move |event| {
                        if let esp_idf_svc::wifi::WifiEvent::StaDisconnected(_) = event {
                            observed.store(true, Ordering::Release);
                        }
                    })
                    .map_err(|e| e.to_string())?;
                let driver = EspWifi::new(modem, event_loop, None).map_err(|e| e.to_string())?;
                // The driver must not keep its own copy of credentials in NVS.
                sys::esp!(unsafe {
                    sys::esp_wifi_set_storage(sys::wifi_storage_t_WIFI_STORAGE_RAM)
                })
                .map_err(|e| e.to_string())?;
                Ok((driver, watch))
            })),
            driver: None,
            _watch: None,
            dropped,
            direct: false,
            saved,
            visible: Vec::new(),
            scanning: false,
            target: None,
            online: false,
            changed: false,
            clock: None,
            next_clock_attempt: now,
            retry: Retry::default(),
            started: now,
            next_poll: now,
            next_scan: now,
            attentive: false,
        };
        if station.saved.enabled()
            && let Err(error) = station.start()
        {
            log::warn!("Wi-Fi unavailable: {error}");
        }
        station
    }

    fn start(&mut self) -> Result<(), String> {
        if self.driver.is_none() {
            let create = self.create.take().ok_or("Wi-Fi driver unavailable")?;
            let (mut driver, watch) = create()?;
            self._watch = Some(watch);
            driver
                .set_configuration(&Configuration::Client(ClientConfiguration::default()))
                .map_err(|e| e.to_string())?;
            self.driver = Some(driver);
        }
        let driver = self.driver.as_mut().expect("driver created");
        if !driver.is_started().map_err(|e| e.to_string())? {
            driver.start().map_err(|e| e.to_string())?;
            // A started driver sleeps between beacons until told otherwise.
            self.attentive = false;
        }
        self.next_scan = Instant::now();
        self.changed = true;
        self.rejoin();
        Ok(())
    }

    /// Goes straight to the access point last joined, on the channel it was
    /// on. The scan that otherwise comes first takes about three seconds, and
    /// waits for the event loop, which a book being opened holds up for
    /// longer. If the access point does not answer, or the link to it is
    /// lost later, [`Self::poll`] scans and chooses again.
    fn rejoin(&mut self) {
        if self.target.is_some() {
            return;
        }
        let Some((point, ssid)) = last() else {
            return;
        };
        if self.apply(&ssid, Some(point)).is_err() {
            return;
        }
        self.dropped.store(false, Ordering::Release);
        let driver = self.driver.as_mut().expect("applied to a driver");
        match driver.connect() {
            Ok(()) => {
                self.retry.begun(0);
                self.direct = true;
            }
            Err(_) => self.target = None,
        }
    }

    /// Keeps the radio awake while network work is in flight. Modem sleep
    /// between beacons loses or delays enough inbound packets on a weak link
    /// to time out name lookups and stall transfers.
    pub fn set_attentive(&mut self, attentive: bool) {
        if self.attentive == attentive || self.driver.is_none() {
            return;
        }
        let mode = if attentive {
            sys::wifi_ps_type_t_WIFI_PS_NONE
        } else {
            sys::wifi_ps_type_t_WIFI_PS_MIN_MODEM
        };
        if unsafe { sys::esp_wifi_set_ps(mode) } == sys::ESP_OK {
            self.attentive = attentive;
        }
    }

    pub fn enabled(&self) -> bool {
        self.saved.enabled()
    }

    pub fn set_enabled(&mut self, enabled: bool) -> Result<(), String> {
        self.saved.set_enabled(enabled);
        store(&self.saved)?;
        if enabled {
            self.start()?;
            // Unless the network last joined is already being rejoined.
            if self.target.is_none() {
                self.scan()?;
            }
            Ok(())
        } else {
            self.target = None;
            self.online = false;
            self.visible.clear();
            self.scanning = false;
            self.changed = true;
            self.stop()
        }
    }

    /// Starts a scan; results arrive through [`Self::poll`].
    pub fn scan(&mut self) -> Result<(), String> {
        let driver = self.driver.as_mut().ok_or("Wi-Fi is off")?;
        driver
            .start_scan(&ScanConfig::default(), false)
            .map_err(|e| format!("Wi-Fi scan: {e}"))?;
        self.scanning = true;
        self.changed = true;
        Ok(())
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning
    }
    pub fn visible(&self) -> &[Visible] {
        &self.visible
    }
    pub fn is_saved(&self, ssid: &str) -> bool {
        self.saved.password(ssid).is_some()
    }
    /// Whether the view of networks or the connection changed since last asked.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// Joins a network, remembering `password` when given and otherwise
    /// using the remembered one.
    pub fn join(&mut self, ssid: &str, password: Option<&str>) -> Result<(), String> {
        if let Some(password) = password {
            self.saved
                .remember(ssid, password)
                .map_err(|error| format!("Invalid network or password: {error:?}"))?;
            store(&self.saved)?;
        }
        self.apply(ssid, None)
    }

    pub fn forget(&mut self, ssid: &str) -> Result<(), String> {
        if self.saved.forget(ssid) {
            store(&self.saved)?;
        }
        if self.target.as_deref() == Some(ssid) {
            self.target = None;
            self.online = false;
            if let Some(driver) = &mut self.driver {
                let _ = driver.disconnect();
            }
        }
        self.changed = true;
        Ok(())
    }

    fn apply(&mut self, ssid: &str, point: Option<Point>) -> Result<(), String> {
        let password = self
            .saved
            .password(ssid)
            .ok_or("Network is not remembered")?;
        let driver = self.driver.as_mut().ok_or("Wi-Fi is off")?;
        let _ = driver.disconnect();
        let mut configuration = Configuration::Client(ClientConfiguration {
            ssid: ssid.try_into().map_err(|_| "Invalid SSID length")?,
            password: password.try_into().map_err(|_| "Invalid password length")?,
            // The weakest security accepted from the access point.
            auth_method: if password.is_empty() {
                AuthMethod::None
            } else {
                AuthMethod::WPA2Personal
            },
            // One access point is joined without listening for others;
            // otherwise the strongest after every channel has been heard.
            bssid: point.map(|point| point.address),
            channel: point.map(|point| point.channel),
            scan_method: match point {
                Some(_) => ScanMethod::FastScan,
                None => ScanMethod::default(),
            },
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
        let now = Instant::now();
        self.target = Some(ssid.into());
        self.direct = false;
        self.online = false;
        self.retry = Retry::default();
        self.started = now;
        self.next_poll = now;
        self.changed = true;
        Ok(())
    }

    fn finish_scan(&mut self) {
        let Some(driver) = &mut self.driver else {
            return;
        };
        if !driver.is_scan_done().unwrap_or(false) {
            return;
        }
        self.scanning = false;
        self.changed = true;
        self.next_scan = Instant::now() + Duration::from_secs(30);
        let found = match driver.get_scan_result() {
            Ok(found) => found,
            Err(error) => {
                log::warn!("Wi-Fi scan results: {error}");
                return;
            }
        };
        let mut visible: Vec<Visible> = Vec::new();
        for point in found {
            let ssid = point.ssid.as_str();
            if ssid.is_empty() || ssid.chars().any(char::is_control) {
                continue;
            }
            let secured = !matches!(point.auth_method, None | Some(AuthMethod::None));
            match visible.iter_mut().find(|known| known.ssid == ssid) {
                Some(known) => known.signal = known.signal.max(point.signal_strength),
                None => visible.push(Visible {
                    ssid: ssid.into(),
                    secured,
                    signal: point.signal_strength,
                }),
            }
        }
        visible.sort_by_key(|network| std::cmp::Reverse(network.signal));
        visible.truncate(MAX_VISIBLE);
        self.visible = visible;
        if self.target.is_none() {
            let choice = self
                .saved
                .choose(self.visible.iter().map(|n| (n.ssid.as_str(), n.signal)))
                .map(str::to_owned);
            if let Some(ssid) = choice
                && let Err(error) = self.apply(&ssid, None)
            {
                log::warn!("Wi-Fi: {error}");
            }
        }
    }

    pub fn poll(&mut self) {
        let now = Instant::now();
        if now < self.next_poll || self.driver.is_none() {
            return;
        }
        self.next_poll = now + Duration::from_millis(250);
        if self.scanning {
            self.finish_scan();
            return;
        }
        if self.direct && self.dropped.swap(false, Ordering::AcqRel) {
            // The access point last joined is not there; see what is.
            self.target = None;
            self.direct = false;
            self.online = false;
            self.next_scan = now;
            self.changed = true;
        }
        if self.target.is_none() {
            // Look for a remembered network now and then while idle.
            if !self.saved.is_empty() && now >= self.next_scan {
                self.next_scan = now + Duration::from_secs(30);
                if let Err(error) = self.scan() {
                    log::warn!("{error}");
                }
            }
            return;
        }
        let driver = self.driver.as_mut().expect("driver checked");
        let elapsed = now
            .duration_since(self.started)
            .as_millis()
            .min(u64::MAX as u128) as u64;
        let up = driver.is_up().unwrap_or(false);
        if up != self.online {
            self.online = up;
            self.changed = true;
            if up {
                remember(self.target.as_deref().expect("target checked"));
            }
        }
        if up && self.clock.is_none() && now >= self.next_clock_attempt {
            self.next_clock_attempt = now + Duration::from_secs(60);
            match crate::clock::Service::start() {
                Ok(clock) => self.clock = Some(clock),
                Err(error) => log::warn!("{error}"),
            }
        }
        if let Some(clock) = &mut self.clock {
            clock.poll();
        }
        match self.retry.poll(elapsed, up) {
            Action::None => {}
            Action::Connect => {
                if let Err(error) = driver.connect() {
                    log::warn!("Wi-Fi connection attempt failed: {error}");
                    self.retry.failed(elapsed);
                }
            }
            Action::Disconnect => {
                let _ = driver.disconnect();
            }
        }
    }

    pub fn address(&self) -> Option<std::net::Ipv4Addr> {
        let driver = self.driver.as_ref()?;
        if !driver.is_up().ok()? {
            return None;
        }
        driver.sta_netif().get_ip_info().ok().map(|info| info.ip)
    }

    /// The joined network, if the link is up.
    pub fn connected(&self) -> Option<&str> {
        self.target.as_deref().filter(|_| self.online)
    }
    pub fn target(&self) -> Option<&str> {
        self.target.as_deref()
    }

    pub fn label(&self) -> &'static str {
        if !self.enabled() || self.driver.is_none() {
            "Wi-Fi off"
        } else if self.target.is_none() {
            "Wi-Fi idle"
        } else if self.retry.is_online() && !crate::clock::ready() {
            "Wi-Fi online, time?"
        } else {
            self.retry.label()
        }
    }

    pub fn stop(&mut self) -> Result<(), String> {
        self.clock.take();
        match &mut self.driver {
            Some(driver) => driver.stop().map_err(|e| format!("Wi-Fi shutdown: {e}")),
            None => Ok(()),
        }
    }

    pub fn resume_after_sleep(&mut self) -> Result<(), String> {
        if !self.enabled() {
            return Ok(());
        }
        self.start().map_err(|e| format!("Wi-Fi restart: {e}"))?;
        self.retry = Retry::default();
        self.started = Instant::now();
        self.next_poll = self.started;
        self.next_clock_attempt = self.started;
        Ok(())
    }
}

/// Reads the remembered networks, first moving a `wifi.json` from the SD
/// card into internal storage.
fn load() -> Result<Saved, String> {
    if let Ok(bytes) = std::fs::read(LEGACY_FILE) {
        let mut saved = match crate::nvs::get(SETTING)? {
            Some(bytes) => Saved::parse(&bytes).map_err(|e| format!("{e:?}"))?,
            None => Saved::default(),
        };
        match Credentials::parse(&bytes) {
            Ok(credentials) => {
                saved
                    .remember(credentials.ssid(), credentials.password())
                    .map_err(|e| format!("{e:?}"))?;
                saved.set_enabled(true);
                store(&saved)?;
                log::info!("Moved the SD card's Wi-Fi network into internal settings");
            }
            Err(error) => log::warn!("Ignoring invalid {LEGACY_FILE}: {error:?}"),
        }
        let mut bytes = bytes;
        bytes.zeroize();
        std::fs::remove_file(LEGACY_FILE)
            .map_err(|e| format!("Cannot remove {LEGACY_FILE}: {e}"))?;
        return Ok(saved);
    }
    match crate::nvs::get(SETTING)? {
        Some(bytes) => Saved::parse(&bytes).map_err(|error| format!("{error:?}")),
        None => Ok(Saved::default()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Point {
    channel: u8,
    address: [u8; 6],
}

/// The access point last joined and the name of its network.
fn last() -> Option<(Point, String)> {
    let bytes = crate::nvs::get(LAST).ok()??;
    let (channel, rest) = bytes.split_first()?;
    let (address, ssid) = rest.split_first_chunk()?;
    let point = Point {
        channel: *channel,
        address: *address,
    };
    Some((point, std::str::from_utf8(ssid).ok()?.to_owned()))
}

/// Records the joined access point, if it changed.
fn remember(ssid: &str) {
    let mut info = sys::wifi_ap_record_t::default();
    if unsafe { sys::esp_wifi_sta_get_ap_info(&mut info) } != sys::ESP_OK {
        return;
    }
    let point = Point {
        channel: info.primary,
        address: info.bssid,
    };
    if last().is_some_and(|(known, name)| known == point && name == ssid) {
        return;
    }
    let mut bytes = vec![point.channel];
    bytes.extend_from_slice(&point.address);
    bytes.extend_from_slice(ssid.as_bytes());
    if let Err(error) = crate::nvs::set(LAST, &bytes) {
        log::warn!("Cannot remember the Wi-Fi access point: {error}");
    }
}

fn store(saved: &Saved) -> Result<(), String> {
    crate::nvs::set(SETTING, &saved.to_bytes())
}
