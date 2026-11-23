//! LAN streams for the shared catalog/progress HTTP client.
use esp_idf_svc::{
    sys,
    tls::{EspTls, Socket},
};
use femto_catalog::{
    deadline::{Bounded, Budget, Limit},
    http::{self, Stream, Url},
};
use std::{
    io::{self, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    os::fd::{AsRawFd, IntoRawFd},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

#[cfg(not(all(esp_idf_mbedtls_certificate_bundle, esp_idf_mbedtls_have_time_date)))]
compile_error!("Device HTTPS requires the certificate bundle and certificate date verification");
#[cfg(esp_idf_esp_tls_insecure)]
compile_error!("Device HTTPS must not enable ESP_TLS_INSECURE");

#[derive(Default)]
pub struct Transport;
impl http::Transport for Transport {
    fn connect(
        &mut self,
        url: &Url,
        limit: Limit,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        Ok(Box::new(connect(url, limit, cancelled)?))
    }
}

pub struct Connection {
    inner: Bounded<Box<dyn Stream + Send>>,
}
impl Connection {
    pub fn into_nonblocking(self) -> io::Result<(Box<dyn Stream + Send>, Arc<AtomicBool>)> {
        self.inner.into_nonblocking()
    }
}
impl femto_microlink::derp::DeadlineIo for Connection {
    fn set_deadline(&mut self, deadline: Instant) {
        self.inner.set_deadline(deadline);
    }
}
impl Read for Connection {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.inner.read(output)
    }
}
impl Write for Connection {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.inner.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
/// Connects a stream that enforces the limit; coordinator callers must supply HTTPS.
pub fn connect(url: &Url, limit: Limit, cancelled: Arc<AtomicBool>) -> io::Result<Connection> {
    connect_inner(url, limit, cancelled, None)
}
/// Connects an explicit relay address while verifying the supplied certificate name.
pub fn connect_tls_at(
    url: &Url,
    address: std::net::SocketAddr,
    name: &str,
    limit: Limit,
    cancelled: Arc<AtomicBool>,
) -> io::Result<Connection> {
    if url.scheme() != "https" || url.port_or_known_default() != Some(address.port()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid TLS endpoint",
        ));
    }
    connect_inner(url, limit, cancelled, Some((address, name)))
}
fn connect_inner(
    url: &Url,
    limit: Limit,
    cancelled: Arc<AtomicBool>,
    peer: Option<(std::net::SocketAddr, &str)>,
) -> io::Result<Connection> {
    let budget = Budget::new(limit, cancelled);
    budget.check()?;
    if url.as_str().len() > 4096
        || !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid request URL",
        ));
    }
    let host = url
        .host_str()
        .filter(|host| !host.is_empty() && host.len() <= 253)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid request host"))?;
    let port = url
        .port_or_known_default()
        .filter(|port| *port != 0)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid request port"))?;
    if url.scheme() == "https" {
        // In the first seconds after waking the clock is not yet set; it is
        // about a second after Wi-Fi connects. That is one wait.
        budget
            .retry(|| crate::clock::require_ready().map_err(|_| io::ErrorKind::WouldBlock.into()))
            .map_err(|error| match error.kind() {
                io::ErrorKind::TimedOut => crate::clock::require_ready().unwrap_err(),
                _ => error,
            })?;
    }
    let certificate_name = peer
        .map(|(_, name)| {
            if name.is_empty() || name.len() > 253 || name.bytes().any(|b| b.is_ascii_control()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid TLS certificate name",
                ));
            }
            std::ffi::CString::new(name).map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidInput, "invalid TLS certificate name")
            })
        })
        .transpose()?;
    let stream = if let Some((address, _)) = peer {
        budget.check()?;
        let stream = TcpStream::connect_timeout(
            &address,
            limit
                .wait()
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(5)),
        )?;
        budget.check()?;
        stream.set_nonblocking(true)?;
        stream
    } else {
        connect_tcp(host, port, &budget)?
    };
    if url.scheme() == "https" {
        let tls = EspTls::adopt(Adopted(Some(stream))).map_err(tls_error)?;
        let config = sys::esp_tls_cfg {
            // The socket is already connected and nonblocking. Skip ESP-TLS's
            // internal connect/select state, as EspAsyncTls::negotiate does.
            non_block: false,
            timeout_ms: 10,
            crt_bundle_attach: Some(sys::esp_crt_bundle_attach),
            skip_common_name: false,
            common_name: certificate_name
                .as_ref()
                .map_or(std::ptr::null(), |name| name.as_ptr()),
            ..Default::default()
        };
        let secured = Instant::now();
        budget.retry(|| {
            crate::clock::require_ready()?;
            // The hostname is length-delimited. Configuration stays alive for
            // every handshake step; the context owns its socket until drop.
            let result = unsafe {
                sys::esp_tls_conn_new_async(
                    host.as_ptr().cast(),
                    host.len() as i32,
                    0,
                    &config,
                    tls.context_handle(),
                )
            };
            match result {
                1 => Ok(()),
                0 => Err(io::ErrorKind::WouldBlock.into()),
                code => Err(tls_error(sys::EspError::from(code).unwrap())),
            }
        })?;
        log::info!(
            "Connection to {host}: TLS {} ms",
            secured.elapsed().as_millis()
        );
        crate::clock::require_ready()?;
        Ok(Connection {
            inner: Bounded::new(Box::new(Tls(tls)), budget),
        })
    } else {
        Ok(Connection {
            inner: Bounded::new(Box::new(stream), budget),
        })
    }
}

fn connect_tcp(host: &str, port: u16, budget: &Budget) -> io::Result<TcpStream> {
    // Platform DNS is synchronous. Recheck the budget after it returns.
    let looked_up = Instant::now();
    // The resolver reports an unanswered query and an unknown name alike; an
    // unanswered one is common on a weak link, so callers may try again.
    let addresses = (host.trim_start_matches('[').trim_end_matches(']'), port)
        .to_socket_addrs()
        .map_err(|error| io::Error::new(io::ErrorKind::HostUnreachable, error))?;
    let lookup = looked_up.elapsed().as_millis();
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no request address");
    for address in addresses.take(8) {
        budget.check()?;
        let timeout = budget
            .limit()
            .wait()
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(5));
        let dialled = Instant::now();
        match TcpStream::connect_timeout(&address, timeout) {
            Ok(stream) => {
                log::info!(
                    "Connection to {host}: lookup {lookup} ms, TCP {} ms",
                    dialled.elapsed().as_millis()
                );
                budget.check()?;
                stream.set_nonblocking(true)?;
                return Ok(stream);
            }
            Err(error) => last = error,
        }
    }
    budget.check()?;
    Err(last)
}

struct Adopted(Option<TcpStream>);
impl Socket for Adopted {
    fn handle(&self) -> i32 {
        self.0.as_ref().expect("owned TLS socket").as_raw_fd()
    }
    fn release(&mut self) -> Result<(), sys::EspError> {
        if let Some(stream) = self.0.take() {
            // ESP-TLS closes the descriptor immediately after this callback.
            let _ = stream.into_raw_fd();
        }
        Ok(())
    }
}

fn tls_error(error: sys::EspError) -> io::Error {
    let code = error.code();
    if code == sys::ESP_TLS_ERR_SSL_WANT_READ as i32
        || code == sys::ESP_TLS_ERR_SSL_WANT_WRITE as i32
        || code == sys::EWOULDBLOCK as i32
    {
        io::Error::new(io::ErrorKind::WouldBlock, "TLS operation pending")
    } else {
        io::Error::other(format!("TLS operation failed: {error}"))
    }
}
/// A failure on an established session is a lost connection: the peer was
/// authenticated by the handshake, and a retry authenticates it again.
fn session_error(error: sys::EspError) -> io::Error {
    let error = tls_error(error);
    if error.kind() == io::ErrorKind::WouldBlock {
        return error;
    }
    log::warn!("{error}");
    io::Error::new(io::ErrorKind::ConnectionReset, error)
}
struct Tls(EspTls<Adopted>);
impl Read for Tls {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        crate::clock::require_ready()?;
        let count = self.0.read(buffer).map_err(session_error)?;
        crate::clock::require_ready()?;
        match count {
            // ESP-TLS combines close_notify and raw TCP EOF. Reject ambiguous EOF;
            // HTTP Content-Length and chunked responses finish before this point.
            0 if !buffer.is_empty() => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "TLS EOF without verifiable framing",
            )),
            count => Ok(count),
        }
    }
}
impl Write for Tls {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        crate::clock::require_ready()?;
        let count = self.0.write(buffer).map_err(session_error)?;
        crate::clock::require_ready()?;
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        crate::clock::require_ready()
    }
}
