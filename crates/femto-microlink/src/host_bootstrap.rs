//! Host TLS connector for the portable coordinator key exchange.
use crate::{
    bootstrap,
    derp::DeadlineIo,
    host_tls::TlsConnector,
    protocol::key::{Machine, PublicKey},
    relay::Certificate,
};
use std::{
    io,
    net::SocketAddr,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
pub fn fetch_control_key(
    host: &str,
    address: SocketAddr,
    version: u16,
    timeout: Duration,
    cancelled: Arc<AtomicBool>,
) -> io::Result<PublicKey<Machine>> {
    fetch_control_key_with_tls(
        host,
        address,
        version,
        &TlsConnector::mozilla(timeout)?,
        cancelled,
    )
}
pub fn fetch_control_key_with_tls(
    host: &str,
    address: SocketAddr,
    version: u16,
    tls: &TlsConnector,
    cancelled: Arc<AtomicBool>,
) -> io::Result<PublicKey<Machine>> {
    bootstrap::authority(host, address.port())?;
    let mut stream = tls.connect(address, host, &Certificate::Name(host.into()), cancelled)?;
    stream.set_deadline(Instant::now() + tls.timeout());
    bootstrap::exchange(stream, host, address.port(), version)
}
