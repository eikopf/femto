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
