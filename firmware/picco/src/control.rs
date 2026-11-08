//! Coordinator key bootstrap over the device's certificate-verified TLS adapter.
use femto_catalog::{
    deadline::Budget,
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
    let budget = Budget::new(deadline, cancelled.clone());
    budget.check()?;
    let stream = crate::network::Transport.connect(&url, deadline, cancelled)?;
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
    let authority = bootstrap::authority(&endpoint.host, endpoint.port)?;
    let url = Url::parse(&format!("https://{authority}/")).map_err(io::Error::other)?;
    let stream = crate::network::connect(&url, Instant::now() + timeout, cancelled.clone())?;
    let session = femto_microlink::control_session::ControlSession::upgrade(
        stream, endpoint, machine, timeout, cancelled,
    )?;
    crate::clock::require_ready()?;
    Ok(session)
}
