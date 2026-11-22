//! Explicit reader controls for device tailnet ownership.
use femto_microlink::{
    device_config::Config, enrollment::Outcome, protocol::identity::DeviceIdentity,
};
use femto_reader::ui::{ReaderUi, TailnetEvent};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

pub type Driver = femto_microlink::application_tcp::ThreadShared<crate::tailnet::Runtime>;

pub struct Manager {
    local_address: femto_microlink::endpoints::LocalAddress,
    runtime: Option<Driver>,
    rotation: Option<crate::tailnet_rotation::Job>,
    rotation_followup: Option<femto_microlink::device_enrollment::Followup>,
    followup: Option<femto_microlink::device_enrollment::Followup>,
    /// Reads the current configuration each time a session starts, so
    /// edited settings apply to the next connection.
    source: Box<dyn Fn() -> Result<Config, String>>,
    stopping: bool,
    rows: Vec<String>,
}
impl Default for Manager {
    fn default() -> Self {
        Self::new("/sdcard/femto/tailnet.json")
    }
}
impl Manager {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        Self::with_source(move || load(&path))
    }
    pub fn with_source(source: impl Fn() -> Result<Config, String> + 'static) -> Self {
        Self {
            local_address: Default::default(),
            runtime: None,
            rotation: None,
            rotation_followup: None,
            followup: None,
            source: Box::new(source),
            stopping: false,
            rows: vec!["Disconnected".into()],
        }
    }
    pub fn set_local_address(&self, address: Option<std::net::Ipv4Addr>) {
        self.local_address.set(address);
    }
    pub fn event(&mut self, event: TailnetEvent, ui: &mut ReaderUi) -> Result<(), String> {
        ui.open_tailnet();
        let result = match event {
            TailnetEvent::Open => Ok(()),
            TailnetEvent::Connect | TailnetEvent::Provision => {
                self.start(event == TailnetEvent::Provision)
            }
            TailnetEvent::Renew => self.renew(),
            TailnetEvent::Stop => {
                self.followup = None;
                self.rotation_followup = None;
                self.rows = vec!["Disconnecting".into()];
                if self.close_and_wait(Duration::ZERO) {
                    self.rows = vec!["Disconnected".into()];
                }
                Ok(())
            }
        };
        ui.set_tailnet_status(&self.rows);
        result
    }
    fn start(&mut self, provision: bool) -> Result<(), String> {
        if self.runtime.is_some() || self.rotation.is_some() {
            return Err("Disconnect the current tailnet session first".into());
        }
        let config = (self.source)()?;
        crate::clock::require_ready().map_err(|_| "Tailnet needs Wi-Fi and synchronized time")?;
        let store = if provision {
            crate::identity::provision(DeviceIdentity::generate().map_err(|e| e.to_string())?)?
        } else {
            crate::identity::open()?
        };
        let mut runtime = if let Some(followup) = &self.followup {
            crate::tailnet::Runtime::start_with_followup(config, store, Some(followup.clone()))?
        } else {
            crate::tailnet::Runtime::start(config, store)?
        };
        runtime.set_local_address_source(self.local_address.clone());
        self.runtime = Some(Driver::new(runtime));
        self.stopping = false;
        self.rows = vec!["Connecting".into()];
        Ok(())
    }
    fn renew(&mut self) -> Result<(), String> {
        if self.runtime.is_some() || self.rotation.is_some() {
            return Err("Disconnect the current tailnet session first".into());
        }
        self.rotation = Some(crate::tailnet_rotation::Job::start(
            (self.source)()?,
            self.rotation_followup.clone(),
        )?);
        self.followup = None;
        self.stopping = false;
        self.rows = vec!["Renewing node key".into()];
        Ok(())
    }
    pub fn poll(&mut self, ui: &mut ReaderUi) -> bool {
        let previous = self.rows.clone();
        if self.stopping {
            if self.close_and_wait(Duration::ZERO)
                && self.rows.first().is_some_and(|s| s == "Disconnecting")
            {
                self.rows = vec!["Disconnected".into()];
            }
        } else if let Some(rotation) = &mut self.rotation {
            if let Some(report) = rotation.poll() {
                match report {
                    Ok(report) => {
                        self.rotation_followup = report.followup;
                        self.rows = match report.outcome {
                            Outcome::Authorized => vec![
                                "Node key renewed".into(),
                                "Connect existing identity".into(),
                            ],
                            Outcome::AwaitingAuthorization(url) => {
                                let mut rows = vec!["Renewal approval required".into()];
                                if let Some(url) = url {
                                    rows.push("Open on another device:".into());
                                    rows.extend(
                                        url.as_str()
                                            .as_bytes()
                                            .chunks(32)
                                            .map(|b| String::from_utf8_lossy(b).into_owned()),
                                    );
                                }
                                rows.push("Renew / retry after approval".into());
                                rows
                            }
                            Outcome::Expired => vec![
                                "Renewal key expired".into(),
                                "Renew / retry to reconcile".into(),
                            ],
                            Outcome::Rejected => {
                                vec!["Renewal rejected".into(), "Pending key retained".into()]
                            }
                        };
                    }
                    Err(()) => {
                        self.rows =
                            vec!["Renewal failed".into(), "Renew / retry to reconcile".into()]
                    }
                }
                self.stopping = true;
                self.close_and_wait(Duration::ZERO);
            }
        } else if let Some(runtime) = self.runtime.clone() {
            if let Err(error) = runtime.with(|runtime| {
                self.update(runtime);
                Ok(())
            }) && error.kind() != std::io::ErrorKind::WouldBlock
            {
                self.rows = vec!["Tailnet connection closed".into()];
                self.stopping = true;
            }
            if self.stopping {
                self.close_and_wait(Duration::ZERO);
            }
        }
        let changed = self.rows != previous;
        if changed {
            ui.set_tailnet_status(&self.rows);
        }
        changed && ui.is_tailnet_open()
    }
    fn update(&mut self, runtime: &mut crate::tailnet::Runtime) {
        if let Some(registration) = runtime.registration() {
            match registration {
                crate::tailnet::Registration::Outcome(Outcome::Authorized) => {
                    self.followup = None;
                    self.rows = vec!["Registered; waiting for map".into()];
                }
                crate::tailnet::Registration::Outcome(Outcome::AwaitingAuthorization(url)) => {
                    self.rows = vec!["Approval required".into()];
                    if let Some(url) = url {
                        self.followup = Some(runtime.approval(url.clone()));
                        self.rows.push("Open on another device:".into());
                        // URL serialization is ASCII; every character remains visible.
                        self.rows.extend(
                            url.as_str()
                                .as_bytes()
                                .chunks(32)
                                .map(|b| String::from_utf8_lossy(b).into_owned()),
                        );
                    }
                    self.rows.push("Reconnect after approval".into());
                    self.stopping = true;
                }
                crate::tailnet::Registration::Outcome(Outcome::Expired) => {
                    self.followup = None;
                    self.rows = vec!["Node key expired".into()];
                    self.stopping = true;
                }
                crate::tailnet::Registration::Outcome(Outcome::Rejected) => {
                    self.followup = None;
                    self.rows = vec!["Registration rejected".into()];
                    self.stopping = true;
                }
                crate::tailnet::Registration::Retrying => {
                    self.rows = vec!["Reconnecting".into()];
                }
                crate::tailnet::Registration::Failed => {
                    self.rows = vec!["Tailnet setup failed".into()];
                    self.stopping = true;
                }
            }
        }
        if !self.stopping {
            match runtime.poll() {
                Ok(Some(address)) => {
                    self.rows = vec![
                        format!("Address: {address}"),
                        "Peer connectivity unverified".into(),
                    ]
                }
                Ok(None) => {
                    if runtime.reconnecting() {
                        self.rows = vec!["Reconnecting".into()];
                    } else if self
                        .rows
                        .first()
                        .is_some_and(|row| row.starts_with("Address:"))
                    {
                        self.rows = vec!["Waiting for assigned address".into()];
                    }
                }
                Err(error) => {
                    #[cfg(target_os = "espidf")]
                    log::warn!("Tailnet closed: {error}");
                    self.rows = vec!["Tailnet connection closed".into(), error];
                    self.stopping = true;
                }
            }
        }
    }
    /// A request retains this runtime generation; later reconnects do not replace it.
    pub fn driver(&self) -> Option<Driver> {
        if self.stopping {
            None
        } else {
            self.runtime.clone()
        }
    }
    /// Keep the manager and platform services alive after a timeout.
    pub fn close_and_wait(&mut self, timeout: Duration) -> bool {
        self.stopping = true;
        let started = Instant::now();
        if let Some(rotation) = &mut self.rotation {
            if !rotation.close_and_wait(timeout) {
                return false;
            }
            self.rotation = None;
        }
        if let Some(runtime) = &self.runtime {
            loop {
                let remaining = timeout.saturating_sub(started.elapsed());
                match runtime.with_cleanup(|runtime| Ok(runtime.close_and_wait(remaining))) {
                    Ok(true) => break,
                    Ok(false) => return false,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if started.elapsed() >= timeout {
                            return false;
                        }
                        std::thread::sleep(
                            Duration::from_millis(5).min(timeout.saturating_sub(started.elapsed())),
                        );
                    }
                    Err(_) => return false,
                }
            }
        }
        self.runtime = None;
        self.stopping = false;
        if self.rows.first().is_some_and(|s| {
            s.starts_with("Address:") || s == "Connecting" || s == "Registered; waiting for map"
        }) {
            self.rows = vec!["Disconnected".into()];
        }
        true
    }
}
impl Drop for Manager {
    fn drop(&mut self) {
        while !self.close_and_wait(Duration::from_secs(1)) {}
    }
}

fn load(path: &Path) -> Result<Config, String> {
    let file = File::open(path).map_err(|_| "Cannot open /femto/tailnet.json on SD")?;
    if !file
        .metadata()
        .map_err(|_| "Cannot inspect tailnet configuration")?
        .is_file()
    {
        return Err("Tailnet configuration must be a regular file".into());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(8193)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read tailnet configuration")?;
    Ok(Config::parse(&bytes)?)
}
