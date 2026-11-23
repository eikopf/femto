//! Coordinator key bootstrap over the device's certificate-verified TLS adapter.
use femto_catalog::{
    deadline::{Budget, Limit},
    http::{Transport, Url},
};
use femto_microlink::{
    bootstrap,
    protocol::key::{Machine, PublicKey},
};
use std::{
    io,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

pub fn fetch_control_key(
    host: &str,
    port: u16,
    version: u16,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
) -> io::Result<PublicKey<Machine>> {
    let authority = bootstrap::authority(host, port)?;
    let url = Url::parse(&format!("https://{authority}/")).map_err(io::Error::other)?;
    let limit = Limit::Until(deadline);
    let budget = Budget::new(limit, cancelled.clone());
    budget.check()?;
    let stream = crate::network::Transport.connect(&url, limit, cancelled)?;
    let key = bootstrap::exchange(stream, host, port, version)?;
    budget.check()?;
    crate::clock::require_ready()?;
    Ok(key)
}

/// The endpoint key must come from authenticated bootstrap for this host.
pub fn connect_session(
    endpoint: &femto_microlink::control_session::Endpoint,
    machine: &femto_microlink::protocol::key::PrivateKey<Machine>,
    timeout: std::time::Duration,
    cancelled: Arc<AtomicBool>,
) -> io::Result<femto_microlink::control_session::ControlSession<crate::network::Connection>> {
    let stream = dial(endpoint, timeout, cancelled.clone())?;
    upgrade(stream, endpoint, machine, timeout, cancelled)
}

fn dial(
    endpoint: &femto_microlink::control_session::Endpoint,
    timeout: std::time::Duration,
    cancelled: Arc<AtomicBool>,
) -> io::Result<crate::network::Connection> {
    let authority = bootstrap::authority(&endpoint.host, endpoint.port)?;
    let url = Url::parse(&format!("https://{authority}/")).map_err(io::Error::other)?;
    crate::network::connect(&url, Limit::Until(Instant::now() + timeout), cancelled)
}

fn upgrade(
    stream: crate::network::Connection,
    endpoint: &femto_microlink::control_session::Endpoint,
    machine: &femto_microlink::protocol::key::PrivateKey<Machine>,
    timeout: std::time::Duration,
    cancelled: Arc<AtomicBool>,
) -> io::Result<femto_microlink::control_session::ControlSession<crate::network::Connection>> {
    let session = femto_microlink::control_session::ControlSession::upgrade(
        stream, endpoint, machine, timeout, cancelled,
    )?;
    crate::clock::require_ready()?;
    Ok(session)
}

/// Internal-storage name of the last coordinator key that opened a session.
const STORED_KEY: &str = "control_key";

/// The stored key, when it was stored for this coordinator.
fn stored_key(authority: &str) -> Option<PublicKey<Machine>> {
    let stored = crate::nvs::get(STORED_KEY).ok()??;
    let (key, owner) = stored.split_first_chunk::<32>()?;
    (owner == authority.as_bytes() && *key != [0; 32]).then(|| PublicKey::from_bytes(*key))
}

/// Opens a control session, fetching the coordinator's key only when needed.
///
/// The key that opened a session is kept in internal storage, so a later
/// join skips the connection that fetches it. A connection that cannot be
/// made is not the key's fault and fails as it is. When the coordinator
/// answers but the stored key opens no session, the key is fetched; a
/// different answer is tried and, if it works, stored. The same answer means
/// the key was not at fault either, and the first error stands.
pub fn open_session(
    host: &str,
    port: u16,
    version: u16,
    machine: &femto_microlink::protocol::key::PrivateKey<Machine>,
    cancelled: Arc<AtomicBool>,
) -> io::Result<(
    femto_microlink::control_session::Endpoint,
    femto_microlink::control_session::ControlSession<crate::network::Connection>,
)> {
    let authority = bootstrap::authority(host, port)?;
    let endpoint = |control_key| femto_microlink::control_session::Endpoint {
        host: host.to_owned(),
        port,
        control_key,
        version,
    };
    let timeout = std::time::Duration::from_secs(60);
    let stored = stored_key(&authority);
    let failure = match &stored {
        Some(key) => {
            let endpoint = endpoint(*key);
            let stream = dial(&endpoint, timeout, cancelled.clone())?;
            match upgrade(stream, &endpoint, machine, timeout, cancelled.clone()) {
                Ok(session) => return Ok((endpoint, session)),
                Err(error) => Some(error),
            }
        }
        None => None,
    };
    let fetched = fetch_control_key(
        host,
        port,
        version,
        Instant::now() + std::time::Duration::from_secs(15),
        cancelled.clone(),
    )?;
    if let Some(error) = failure
        && stored.as_ref().map(PublicKey::as_bytes) == Some(fetched.as_bytes())
    {
        return Err(error);
    }
    let endpoint = endpoint(fetched);
    let session = connect_session(&endpoint, machine, timeout, cancelled)?;
    let mut record = endpoint.control_key.as_bytes().to_vec();
    record.extend_from_slice(authority.as_bytes());
    // A failed write costs the next join one connection, nothing else.
    let _ = crate::nvs::set(STORED_KEY, &record);
    Ok((endpoint, session))
}
