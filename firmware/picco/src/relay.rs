//! Verified TLS and authenticated DERP setup for a selected relay descriptor.
use femto_catalog::{deadline::Budget, http::Url};
use femto_microlink::{
    derp::RelaySession,
    protocol::key::{Node, PrivateKey},
    relay::{Certificate, RelayNode},
};
use std::{
    io,
    net::SocketAddr,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

/// The network owner must validate its relay plan again before releasing packets.
pub fn connect(
    node: &RelayNode,
    address: SocketAddr,
    key: PrivateKey<Node>,
    timeout: Duration,
    cancelled: Arc<AtomicBool>,
) -> io::Result<RelaySession<crate::network::Connection>> {
    if !node.allows_address(address) || timeout.is_zero() || timeout > Duration::from_secs(60) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid relay target or timeout",
        ));
    }
    let Certificate::Name(name) = &node.certificate else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "device DERP certificate pins are not implemented",
        ));
    };
    let authority = femto_microlink::bootstrap::authority(&node.host_name, node.tls_port)?;
    let url = Url::parse(&format!("https://{authority}/")).map_err(io::Error::other)?;
    let deadline = Instant::now() + timeout;
    let budget = Budget::new(deadline, cancelled.clone());
    budget.check()?;
    let stream = crate::network::connect_tls_at(&url, address, name, deadline, cancelled)?;
    let session =
        RelaySession::connect(stream, &authority, key, timeout).map_err(io::Error::other)?;
    budget.check()?;
    crate::clock::require_ready()?;
    Ok(session)
}
