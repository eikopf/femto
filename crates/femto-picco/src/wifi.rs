//! Opt-in station credentials and a bounded, nonblocking reconnect schedule.
use alloc::string::String;
use serde::Deserialize;
use zeroize::Zeroizing;

pub const MAX_CONFIG_BYTES: usize = 1024;
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Size,
    Format,
    Version,
    Ssid,
    Password,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    ssid: Zeroizing<String>,
    password: Zeroizing<String>,
}
pub struct Credentials(Wire);
impl Credentials {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_CONFIG_BYTES {
            return Err(Error::Size);
        }
        let wire: Wire = serde_json::from_slice(bytes).map_err(|_| Error::Format)?;
        if wire.version != 1 {
            return Err(Error::Version);
        }
        if wire.ssid.is_empty() || wire.ssid.len() > 32 || wire.ssid.chars().any(char::is_control) {
            return Err(Error::Ssid);
        }
        if !(8..=63).contains(&wire.password.len())
            || !wire.password.bytes().all(|b| (0x20..=0x7e).contains(&b))
        {
            return Err(Error::Password);
        }
        Ok(Self(wire))
    }
    pub fn ssid(&self) -> &str {
        &self.0.ssid
    }
    pub fn password(&self) -> &str {
        &self.0.password
    }
}
/// Networks remembered on the device, most recently joined first.
pub const MAX_NETWORKS: usize = 8;
const MAX_SAVED_BYTES: usize = 2048;

#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedNetwork {
    ssid: String,
    /// Empty for an open network.
    password: Zeroizing<String>,
}
#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedWire {
    version: u8,
    enabled: bool,
    networks: alloc::vec::Vec<SavedNetwork>,
}
pub struct Saved(SavedWire);

fn validate(ssid: &str, password: &str) -> Result<(), Error> {
    if ssid.is_empty() || ssid.len() > 32 || ssid.chars().any(char::is_control) {
        return Err(Error::Ssid);
    }
    if !password.is_empty()
        && (!(8..=63).contains(&password.len())
            || !password.bytes().all(|b| (0x20..=0x7e).contains(&b)))
    {
        return Err(Error::Password);
    }
    Ok(())
}

impl Default for Saved {
    fn default() -> Self {
        Self(SavedWire {
            version: 1,
            enabled: false,
            networks: alloc::vec::Vec::new(),
        })
    }
}

impl Saved {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() > MAX_SAVED_BYTES {
            return Err(Error::Size);
        }
        let wire: SavedWire = serde_json::from_slice(bytes).map_err(|_| Error::Format)?;
        if wire.version != 1 {
            return Err(Error::Version);
        }
        if wire.networks.len() > MAX_NETWORKS {
            return Err(Error::Size);
        }
        for network in &wire.networks {
            validate(&network.ssid, &network.password)?;
        }
        Ok(Self(wire))
    }
    pub fn to_bytes(&self) -> Zeroizing<alloc::vec::Vec<u8>> {
        Zeroizing::new(serde_json::to_vec(&self.0).expect("plain strings serialize"))
    }
    pub fn enabled(&self) -> bool {
        self.0.enabled
    }
    pub fn set_enabled(&mut self, enabled: bool) {
        self.0.enabled = enabled;
    }
    pub fn is_empty(&self) -> bool {
        self.0.networks.is_empty()
    }
    pub fn password(&self, ssid: &str) -> Option<&str> {
        self.0
            .networks
            .iter()
            .find(|network| network.ssid == ssid)
            .map(|network| network.password.as_str())
    }
    /// Remembers a network as the most recent, replacing an entry with the
    /// same name and dropping the least recent beyond [`MAX_NETWORKS`].
    pub fn remember(&mut self, ssid: &str, password: &str) -> Result<(), Error> {
        validate(ssid, password)?;
        self.forget(ssid);
        self.0.networks.insert(
            0,
            SavedNetwork {
                ssid: ssid.into(),
                password: Zeroizing::new(password.into()),
            },
        );
        self.0.networks.truncate(MAX_NETWORKS);
        Ok(())
    }
    pub fn forget(&mut self, ssid: &str) -> bool {
        let before = self.0.networks.len();
        self.0.networks.retain(|network| network.ssid != ssid);
        before != self.0.networks.len()
    }
    /// The remembered network with the strongest signal among those visible.
    pub fn choose<'a>(&self, visible: impl IntoIterator<Item = (&'a str, i8)>) -> Option<&str> {
        visible
            .into_iter()
            .filter(|(ssid, _)| self.password(ssid).is_some())
            .max_by_key(|(_, signal)| *signal)
            .and_then(|(ssid, _)| {
                self.0
                    .networks
                    .iter()
                    .find(|network| network.ssid == ssid)
                    .map(|network| network.ssid.as_str())
            })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Connect,
    Disconnect,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Waiting,
    Connecting,
    Online,
    Disabled,
}
pub struct Retry {
    phase: Phase,
    next: u64,
    attempts: u8,
    last: u64,
}
impl Default for Retry {
    fn default() -> Self {
        Self {
            phase: Phase::Waiting,
            next: 0,
            attempts: 0,
            last: 0,
        }
    }
}
impl Retry {
    fn wait(&mut self, now: u64) {
        self.phase = Phase::Waiting;
        self.next =
            now.saturating_add((5000u64 << self.attempts.saturating_sub(1).min(4)).min(60000));
    }
    pub fn failed(&mut self, now: u64) {
        self.wait(now);
    }
    pub fn poll(&mut self, now: u64, up: bool) -> Action {
        if now < self.last {
            self.phase = Phase::Disabled;
            return Action::Disconnect;
        }
        self.last = now;
        if self.phase == Phase::Disabled {
            return Action::None;
        }
        if up {
            self.phase = Phase::Online;
            self.attempts = 0;
            return Action::None;
        }
        match self.phase {
            Phase::Online => {
                self.wait(now);
                Action::Disconnect
            }
            Phase::Waiting if now >= self.next => {
                self.phase = Phase::Connecting;
                self.attempts = self.attempts.saturating_add(1);
                self.next = now.saturating_add(30000);
                Action::Connect
            }
            Phase::Connecting if now >= self.next => {
                self.wait(now);
                Action::Disconnect
            }
            _ => Action::None,
        }
    }
    pub fn is_online(&self) -> bool {
        self.phase == Phase::Online
    }
    pub fn label(&self) -> &'static str {
        match self.phase {
            Phase::Waiting => "Wi-Fi retry",
            Phase::Connecting => "Wi-Fi connecting",
            Phase::Online => "Wi-Fi online",
            Phase::Disabled => "Wi-Fi stopped",
        }
    }
}
