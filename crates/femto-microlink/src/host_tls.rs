//! Unix host TLS transport. ESP-IDF uses a separate platform adapter.
use crate::relay::Certificate;
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme,
    StreamOwned,
    client::{
        WebPkiServerVerifier,
        danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    },
    crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature},
    pki_types::{CertificateDer, ServerName, UnixTime},
};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct TlsConnector {
    roots: Arc<RootCertStore>,
    timeout: Duration,
}
impl TlsConnector {
    pub(crate) fn timeout(&self) -> Duration {
        self.timeout
    }
    /// Retains the same trust roots with a separately bounded operation timeout.
    pub fn with_timeout(&self, timeout: Duration) -> io::Result<Self> {
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            roots: self.roots.clone(),
            timeout,
        })
    }
    pub fn connect_derp(
        &self,
        node: &crate::relay::RelayNode,
        address: SocketAddr,
        key: crate::protocol::key::PrivateKey<crate::protocol::key::Node>,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<crate::derp::RelaySession<TlsStream>> {
        let stream = self.connect_relay(node, address, cancelled)?;
        let host = if node.host_name.contains(':') {
            format!("[{}]", node.host_name)
        } else {
            node.host_name.clone()
        };
        let authority = format!("{host}:{}", node.tls_port);
        crate::derp::RelaySession::connect(stream, &authority, key, self.timeout)
            .map_err(io::Error::other)
    }
    /// Uses a relay descriptor and an address resolved by the caller. Fixed
    /// overrides and disabled address families are enforced before dialing.
    pub fn connect_relay(
        &self,
        node: &crate::relay::RelayNode,
        address: SocketAddr,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<TlsStream> {
        if !node.allows_address(address) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "address is not an enabled relay target",
            ));
        }
        self.connect(address, &node.host_name, &node.certificate, cancelled)
    }
    pub fn mozilla(timeout: Duration) -> io::Result<Self> {
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        Self::with_roots(roots, timeout)
    }
    /// Uses one explicitly supplied DER trust anchor, at most 16 KiB.
    /// This replaces Mozilla roots for this connector only.
    pub fn with_der_root(bytes: &[u8], timeout: Duration) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > 16 * 1024 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut roots = RootCertStore::empty();
        roots
            .add(CertificateDer::from(bytes.to_vec()))
            .map_err(invalid)?;
        Self::with_roots(roots, timeout)
    }
    pub fn with_roots(roots: RootCertStore, timeout: Duration) -> io::Result<Self> {
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TLS timeout must be positive and at most 60 seconds",
            ));
        }
        Ok(Self {
            roots: Arc::new(roots),
            timeout,
        })
    }
    /// Connects a previously resolved numeric address. SNI and certificate
    /// identity remain separate to support coordinator CertName overrides.
    /// DNS resolution and address selection are the caller's responsibility.
    pub fn connect(
        &self,
        address: SocketAddr,
        sni: &str,
        identity: &Certificate,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<TlsStream> {
        let sni = ServerName::try_from(sni.to_owned()).map_err(invalid)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let verifier = match identity {
            Certificate::Name(name) => Verifier {
                name: Some(ServerName::try_from(name.clone()).map_err(invalid)?),
                pin: None,
                pki: Some(
                    WebPkiServerVerifier::builder_with_provider(
                        self.roots.clone(),
                        provider.clone(),
                    )
                    .build()
                    .map_err(invalid)?,
                ),
                provider: provider.clone(),
            },
            Certificate::Sha256(pin) => Verifier {
                name: None,
                pin: Some(*pin),
                pki: None,
                provider: provider.clone(),
            },
        };
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(invalid)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(verifier))
            .with_no_client_auth();
        config.resumption = rustls::client::Resumption::disabled();
        let mut connection = ClientConnection::new(Arc::new(config), sni).map_err(invalid)?;
        connection.set_buffer_limit(Some(16 * 1024));
        let deadline = Instant::now() + self.timeout;
        check_cancel(&cancelled)?;
        let tcp = TcpStream::connect_timeout(&address, self.timeout)?;
        tcp.set_nodelay(true)?;
        let mut socket = DeadlineSocket {
            tcp,
            deadline,
            cancelled,
        };
        while connection.is_handshaking() {
            connection.complete_io(&mut socket)?;
        }
        check_cancel(&socket.cancelled)?;
        Ok(TlsStream {
            stream: StreamOwned::new(connection, socket),
            timeout: self.timeout,
            failed: false,
            operation_deadline: None,
        })
    }
}
fn invalid(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error)
}
fn check_cancel(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "TLS operation cancelled",
        ))
    } else {
        Ok(())
    }
}
#[derive(Debug)]
struct Verifier {
    name: Option<ServerName<'static>>,
    pin: Option<[u8; 32]>,
    pki: Option<Arc<WebPkiServerVerifier>>,
    provider: Arc<CryptoProvider>,
}
impl ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _sni: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if let (Some(pki), Some(name)) = (&self.pki, &self.name) {
            return pki.verify_server_cert(cert, intermediates, name, ocsp, now);
        }
        if self
            .pin
            .is_some_and(|pin| Sha256::digest(cert.as_ref()).as_slice() == pin)
        {
            return Ok(ServerCertVerified::assertion());
        }
        Err(rustls::Error::General(
            "relay certificate fingerprint mismatch".into(),
        ))
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}
struct DeadlineSocket {
    tcp: TcpStream,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}
impl DeadlineSocket {
    fn remaining(&self) -> io::Result<Duration> {
        check_cancel(&self.cancelled)?;
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .map(|d| d.min(Duration::from_millis(100)))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::TimedOut, "TLS operation deadline exceeded")
            })
    }
}
impl Read for DeadlineSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            self.tcp.set_read_timeout(Some(self.remaining()?))?;
            match self.tcp.read(buf) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
}
impl Write for DeadlineSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            self.tcp.set_write_timeout(Some(self.remaining()?))?;
            match self.tcp.write(buf) {
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::Interrupted
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.remaining()?;
        self.tcp.flush()
    }
}
/// Returned only after certificate and handshake-signature verification.
/// Each Read/Write/flush has a total deadline. An error poisons the stream.
pub struct TlsStream {
    stream: StreamOwned<ClientConnection, DeadlineSocket>,
    timeout: Duration,
    failed: bool,
    operation_deadline: Option<Instant>,
}
impl crate::derp::DeadlineIo for TlsStream {
    fn set_deadline(&mut self, deadline: Instant) {
        self.operation_deadline = Some(deadline);
    }
}
impl TlsStream {
    /// Switches a verified connection permanently to nonblocking I/O. The
    /// caller must enforce frame/operation deadlines in this mode.
    pub fn into_nonblocking(self) -> io::Result<NonblockingTls> {
        if self.failed {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "TLS stream failed",
            ));
        }
        check_cancel(&self.stream.sock.cancelled)?;
        self.stream.sock.tcp.set_nonblocking(true)?;
        Ok(NonblockingTls {
            connection: self.stream.conn,
            tcp: self.stream.sock.tcp,
            cancelled: self.stream.sock.cancelled,
            failed: false,
        })
    }
    fn begin(&mut self) -> io::Result<()> {
        if self.failed {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "TLS stream failed",
            ));
        }
        if let Err(e) = check_cancel(&self.stream.sock.cancelled) {
            self.failed = true;
            return Err(e);
        }
        self.stream.sock.deadline = Instant::now() + self.timeout;
        if let Some(deadline) = self.operation_deadline {
            self.stream.sock.deadline = self.stream.sock.deadline.min(deadline);
            if Instant::now() >= deadline {
                self.failed = true;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "TLS enclosing operation deadline",
                ));
            }
        }
        Ok(())
    }
    fn result<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        if result.is_err() {
            self.failed = true;
        }
        result
    }
}
/// Nonblocking operations after certificate and handshake verification.
/// Existing Rustls record state and buffered plaintext survive conversion.
pub struct NonblockingTls {
    connection: ClientConnection,
    tcp: TcpStream,
    cancelled: Arc<AtomicBool>,
    failed: bool,
}
impl NonblockingTls {
    pub(crate) fn check(&mut self) -> io::Result<()> {
        if self.failed {
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        if let Err(error) = check_cancel(&self.cancelled) {
            self.close();
            return Err(error);
        }
        Ok(())
    }
    fn finish<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        if result.is_err() {
            self.close();
        }
        result
    }
    /// Drains plaintext or performs one socket read and TLS processing step.
    /// None means pending. Some(0) on a nonempty buffer means clean TLS EOF.
    pub fn receive(&mut self, buffer: &mut [u8]) -> io::Result<Option<usize>> {
        self.check()?;
        if buffer.is_empty() {
            return Ok(Some(0));
        }
        let limit = buffer.len().min(16384);
        match self.connection.reader().read(&mut buffer[..limit]) {
            Ok(count) => {
                if count == 0 {
                    self.close();
                }
                return Ok(Some(count));
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
            Err(error) => return self.finish(Err(error)),
        }
        match self.connection.read_tls(&mut self.tcp) {
            Ok(_) => (),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return self.finish(Err(error)),
        }
        if let Err(error) = self.connection.process_new_packets() {
            return self.finish(Err(io::Error::new(io::ErrorKind::InvalidData, error)));
        }
        match self.connection.reader().read(&mut buffer[..limit]) {
            Ok(count) => {
                if count == 0 {
                    self.close();
                }
                Ok(Some(count))
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => self.finish(Err(error)),
        }
    }
    /// Accepts up to 16 KiB into Rustls's bounded output buffer. Zero means
    /// backpressure for nonempty input. Acceptance does not mean transmission.
    pub fn queue(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.check()?;
        let result = self
            .connection
            .writer()
            .write(&bytes[..bytes.len().min(16384)]);
        self.finish(result)
    }
    /// Performs at most one socket write. True means Rustls has no queued TLS
    /// output; false means the caller must poll again. Reads may queue TLS replies.
    pub fn flush_pending(&mut self) -> io::Result<bool> {
        self.check()?;
        if !self.connection.wants_write() {
            return Ok(true);
        }
        match self.connection.write_tls(&mut self.tcp) {
            Ok(0) => self.finish(Err(io::ErrorKind::WriteZero.into())),
            Ok(_) => Ok(!self.connection.wants_write()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(false)
            }
            Err(error) => self.finish(Err(error)),
        }
    }
    pub fn close(&mut self) {
        self.failed = true;
        let _ = self.tcp.shutdown(std::net::Shutdown::Both);
    }
}
impl Read for TlsStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.begin()?;
        let result = self.stream.read(buf);
        self.result(result)
    }
}
impl Write for TlsStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.begin()?;
        let result = self.stream.write(buf);
        self.result(result)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.begin()?;
        let result = self.stream.flush();
        self.result(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::{
        ServerConfig, ServerConnection,
        pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer},
    };
    use std::{net::TcpListener, thread};
    fn certificate(expired: bool) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["certificate.example".into()]).unwrap();
        if expired {
            params.not_before = rcgen::date_time_ymd(2020, 1, 1);
            params.not_after = rcgen::date_time_ymd(2021, 1, 1);
        }
        let cert = params.self_signed(&key).unwrap();
        (
            cert.der().clone(),
            PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
    }
    fn exchange(
        cert: CertificateDer<'static>,
        key: PrivateKeyDer<'static>,
        roots: RootCertStore,
        identity: Certificate,
    ) -> bool {
        let tls13 = exchange_version(
            cert.clone(),
            key.clone_key(),
            roots.clone(),
            identity.clone(),
            &rustls::version::TLS13,
        );
        let tls12 = exchange_version(cert, key, roots, identity, &rustls::version::TLS12);
        assert_eq!(tls13, tls12);
        tls13
    }
    fn exchange_version(
        cert: CertificateDer<'static>,
        key: PrivateKeyDer<'static>,
        roots: RootCertStore,
        identity: Certificate,
        version: &'static rustls::SupportedProtocolVersion,
    ) -> bool {
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[version])
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![cert], key)
                .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            tcp.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
            let conn = ServerConnection::new(Arc::new(config)).unwrap();
            let mut stream = StreamOwned::new(conn, tcp);
            let mut request = [0; 4];
            if stream.read_exact(&mut request).is_ok() {
                assert_eq!(&request, b"ping");
                assert_eq!(stream.conn.server_name(), Some("dial.example"));
                stream.write_all(b"pong").unwrap();
                stream.flush().unwrap();
            }
        });
        let connector = TlsConnector::with_roots(roots, Duration::from_secs(1)).unwrap();
        assert!(connector.with_timeout(Duration::ZERO).is_err());
        assert!(connector.with_timeout(Duration::from_secs(61)).is_err());
        let connector = connector
            .clone()
            .with_timeout(Duration::from_secs(2))
            .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let result = connector.connect(addr, "dial.example", &identity, cancelled.clone());
        let accepted = match result {
            Ok(mut stream) => {
                stream.write_all(b"ping").unwrap();
                stream.flush().unwrap();
                let mut response = [0; 4];
                stream.read_exact(&mut response).unwrap();
                assert_eq!(&response, b"pong");
                cancelled.store(true, Ordering::Release);
                assert_eq!(
                    stream.write(b"x").unwrap_err().kind(),
                    io::ErrorKind::ConnectionAborted
                );
                cancelled.store(false, Ordering::Release);
                assert!(stream.read(&mut response).is_err());
                true
            }
            Err(_) => false,
        };
        worker.join().unwrap();
        accepted
    }

    #[test]
    fn nonblocking_tls_preserves_records_backpressure_and_terminal_states() {
        for version in [&rustls::version::TLS13, &rustls::version::TLS12] {
            for finish_mode in 0..3 {
                nonblocking_exchange(version, finish_mode);
            }
        }
    }
    fn nonblocking_exchange(version: &'static rustls::SupportedProtocolVersion, finish_mode: u8) {
        let (cert, key) = certificate(false);
        let mut roots = RootCertStore::empty();
        roots.add(cert.clone()).unwrap();
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[version])
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![cert], key)
                .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (count_tx, count_rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let (mut tcp, _) = listener.accept().unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            tcp.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut connection = ServerConnection::new(Arc::new(config)).unwrap();
            while connection.is_handshaking() {
                connection.complete_io(&mut tcp).unwrap();
            }
            let mut stream = StreamOwned::new(connection, tcp);
            stream.write_all(b"hi").unwrap();
            stream.flush().unwrap();
            let count = count_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let mut payload = vec![0; count];
            stream.read_exact(&mut payload).unwrap();
            assert!(payload.iter().all(|byte| *byte == 0x5a));
            stream.conn.writer().write_all(b"pong").unwrap();
            if finish_mode != 2 {
                stream.conn.send_close_notify();
            }
            while stream.conn.wants_write() {
                stream.conn.write_tls(&mut stream.sock).unwrap();
            }
        });
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut blocking = TlsConnector::with_roots(roots, Duration::from_secs(2))
            .unwrap()
            .connect(
                address,
                "certificate.example",
                &Certificate::Name("certificate.example".into()),
                cancelled.clone(),
            )
            .unwrap();
        let mut prefix = [0; 1];
        blocking.read_exact(&mut prefix).unwrap();
        assert_eq!(&prefix, b"h");
        let mut tls = blocking.into_nonblocking().unwrap();
        let mut buffer = [0; 2];
        assert_eq!(tls.receive(&mut buffer).unwrap(), Some(1));
        assert_eq!(buffer[0], b'i');
        assert!(tls.receive(&mut buffer).unwrap().is_none());
        let mut accepted = 0;
        for _ in 0..64 {
            let count = tls.queue(&[0x5a; 32768]).unwrap();
            accepted += count;
            if count == 0 {
                break;
            }
        }
        assert!((1..=16384).contains(&accepted));
        assert_eq!(tls.queue(b"x").unwrap(), 0);
        count_tx.send(accepted).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !tls.flush_pending().unwrap() {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        let mut received = Vec::new();
        loop {
            match tls.receive(&mut buffer) {
                Ok(Some(0)) => {
                    assert_eq!(finish_mode, 0);
                    break;
                }
                Ok(Some(count)) => {
                    received.extend_from_slice(&buffer[..count]);
                    if finish_mode == 1 {
                        cancelled.store(true, Ordering::Release);
                        assert_eq!(
                            tls.receive(&mut buffer).unwrap_err().kind(),
                            io::ErrorKind::ConnectionAborted
                        );
                        cancelled.store(false, Ordering::Release);
                        break;
                    }
                }
                Ok(None) => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => {
                    assert_eq!(finish_mode, 2);
                    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
                    break;
                }
            }
        }
        assert_eq!(
            received,
            if finish_mode == 1 {
                b"po".to_vec()
            } else {
                b"pong".to_vec()
            }
        );
        assert!(tls.queue(b"after termination").is_err());
        assert!(tls.flush_pending().is_err());
        assert!(tls.receive(&mut buffer).is_err());
        worker.join().unwrap();
    }
    #[test]
    fn tls_derp_handshake_packets_ping_and_oversize_failure() {
        derp_exchange(0);
        derp_exchange(1);
    }
    #[test]
    fn authorized_relay_drops_queued_output_before_socket_write() {
        derp_exchange(2);
    }
    #[test]
    fn relay_pool_dispatches_and_removes_failed_connections() {
        derp_exchange(3);
    }
    #[test]
    fn home_relay_signals_preference_and_withdraws_on_failure_or_revocation() {
        derp_exchange(4);
        derp_exchange(5);
    }
    #[test]
    fn authenticated_restart_frame_schedules_reconnect() {
        derp_exchange(6);
    }
    fn derp_exchange(mode: u8) {
        use crate::protocol::{
            derp::Event,
            key::{Node, PrivateKey, PublicKey},
        };
        use crate::relay::{Address, RelayNode};
        use crypto_box::{
            PublicKey as BoxPublic, SalsaBox, SecretKey,
            aead::{Aead, generic_array::GenericArray},
        };
        fn frame<W: Write>(io: &mut W, kind: u8, body: &[u8]) {
            io.write_all(&[kind]).unwrap();
            io.write_all(&(body.len() as u32).to_be_bytes()).unwrap();
            io.write_all(body).unwrap();
            io.flush().unwrap();
        }
        fn read_frame<R: Read>(io: &mut R) -> (u8, Vec<u8>) {
            let mut header = [0; 5];
            io.read_exact(&mut header).unwrap();
            let len = u32::from_be_bytes(header[1..].try_into().unwrap()) as usize;
            assert!(len < 4096);
            let mut body = vec![0; len];
            io.read_exact(&mut body).unwrap();
            (header[0], body)
        }
        let identity = crate::protocol::identity::DeviceIdentity::generate().unwrap();
        let client_key = identity.node_key().clone();
        let client_public = client_key.public();
        let peer = PublicKey::<Node>::from_bytes([3; 32]);
        let (cert, key) = certificate(false);
        let mut roots = RootCertStore::empty();
        roots.add(cert.clone()).unwrap();
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![cert], key)
                .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let worker = thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            tcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            tcp.set_write_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut io = StreamOwned::new(ServerConnection::new(Arc::new(config)).unwrap(), tcp);
            let mut http = Vec::new();
            while !http.ends_with(b"\r\n\r\n") {
                let mut b = [0];
                io.read_exact(&mut b).unwrap();
                http.push(b[0]);
                assert!(http.len() < 8192);
            }
            assert!(http.starts_with(b"GET /derp HTTP/1.1\r\n"));
            io.write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: DERP\r\nConnection: Upgrade\r\n\r\n",
            )
            .unwrap();
            let server = PrivateKey::<Node>::from_bytes([2; 32]);
            let mut greeting = crate::protocol::derp::MAGIC.to_vec();
            greeting.extend_from_slice(server.public().as_bytes());
            frame(&mut io, 1, &greeting);
            let (kind, info) = read_frame(&mut io);
            assert_eq!(kind, 2);
            assert_eq!(&info[..32], client_public.as_bytes());
            let cipher = SalsaBox::new(
                &BoxPublic::from(*client_public.as_bytes()),
                &SecretKey::from(*server.as_bytes()),
            );
            let decrypted = cipher
                .decrypt(GenericArray::from_slice(&info[32..56]), &info[56..])
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&decrypted).unwrap()["version"],
                2
            );
            let nonce = [0; 24];
            let sealed = cipher
                .encrypt(
                    GenericArray::from_slice(&nonce),
                    br#"{"version":2}"#.as_slice(),
                )
                .unwrap();
            let mut response = nonce.to_vec();
            response.extend(sealed);
            let mut packet = peer.as_bytes().to_vec();
            if mode >= 2 {
                let mut forged = [0; 92];
                forged[..4].copy_from_slice(&2u32.to_le_bytes());
                packet.extend_from_slice(&forged);
            } else {
                packet.extend_from_slice(b"incoming");
            }
            let mut startup = Vec::new();
            for (kind, body) in [(3, response.as_slice()), (5, packet.as_slice())] {
                startup.push(kind);
                startup.extend_from_slice(&(body.len() as u32).to_be_bytes());
                startup.extend_from_slice(body);
            }
            io.write_all(&startup).unwrap();
            io.flush().unwrap();
            if mode >= 4 {
                assert_eq!(read_frame(&mut io), (7, vec![1]));
            }
            if mode == 2 || mode == 5 {
                match io.read(&mut [0; 1]) {
                    Ok(0) => (),
                    Err(error) => assert!(matches!(
                        error.kind(),
                        io::ErrorKind::UnexpectedEof
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::ConnectionAborted
                    )),
                    Ok(_) => panic!("revoked relay output reached the server"),
                }
                return;
            }
            let (kind, packet) = read_frame(&mut io);
            assert_eq!(kind, 4);
            assert_eq!(&packet[..32], peer.as_bytes());
            if mode >= 3 {
                assert_eq!(packet.len(), 32 + 148);
                assert_eq!(&packet[32..36], &[1, 0, 0, 0]);
            } else {
                assert_eq!(&packet[32..], b"outgoing");
            }
            if mode == 1 {
                let (kind, packet) = read_frame(&mut io);
                assert_eq!(kind, 4);
                assert_eq!(&packet[32..], b"second");
            }
            frame(&mut io, 0x12, b"12345678");
            let (kind, pong) = read_frame(&mut io);
            assert_eq!(kind, 0x13);
            assert_eq!(pong, b"12345678");
            if mode == 6 {
                let mut restart = 30_000u32.to_be_bytes().to_vec();
                restart.extend_from_slice(&10_000u32.to_be_bytes());
                frame(&mut io, 0x15, &restart);
            } else {
                io.write_all(&[5, 255, 255, 255, 255]).unwrap();
                io.flush().unwrap();
            }
        });
        let node = RelayNode {
            name: "test".into(),
            host_name: "dial.example".into(),
            certificate: Certificate::Name("certificate.example".into()),
            ipv4: Address::Dns,
            ipv6: Address::Disabled,
            tls_port: address.port(),
            stun_port: None,
            stun_only: false,
        };
        let connector = TlsConnector::with_roots(roots, Duration::from_secs(2)).unwrap();
        if mode >= 2 {
            struct Store;
            impl crate::timestamp::EpochStore for Store {
                type Error = ();
                fn load_epoch(&mut self) -> Result<Option<u64>, ()> {
                    Ok(None)
                }
                fn reserve_epoch(&mut self, _: u64) -> Result<(), ()> {
                    Ok(())
                }
            }
            let mut network = crate::network::Network::new(
                &identity,
                crate::timestamp::TimestampClock::open(Store, 100).unwrap(),
                4,
                2,
            )
            .unwrap();
            let map = serde_json::json!({"Node":{"ID":1,"Key":client_public.to_string(),"Addresses":["100.64.0.1/32"],"MachineAuthorized":true},"Peers":[{"ID":2,"Key":peer.to_string(),"Addresses":["100.64.0.2/32"],"HomeDERP":7,"MachineAuthorized":true}],"DERPMap":{"Regions":{"7":{"RegionID":7,"Nodes":[{"Name":"test","RegionID":7,"HostName":"dial.example","CertName":"certificate.example","DERPPort":address.port()}]}}},"PacketFilter":[]});
            let clock = |n| crate::coordinator::Clock {
                unix_ms: 1_735_689_600_000 + n as i64,
                monotonic_ms: n,
            };
            network
                .apply_map(&serde_json::to_vec(&map).unwrap(), clock(0))
                .unwrap();
            let plan = if mode >= 4 {
                network.home_relay_plan(0).unwrap().unwrap()
            } else {
                network.relay_plan(peer, 0).unwrap().unwrap()
            };
            let mut pool = crate::host_derp::Pool::default();
            assert_eq!(pool.home_region(&mut network, 0), None);
            assert!(pool.next_plan(&mut network, 0).unwrap().unwrap().is_home());
            assert!(
                crate::host_derp::AuthorizedConnection::connect(
                    &connector,
                    plan.clone(),
                    address,
                    PrivateKey::from_bytes([9; 32]),
                    Arc::new(AtomicBool::new(false)),
                    0
                )
                .is_err()
            );
            let connection = crate::host_derp::AuthorizedConnection::connect(
                &connector,
                plan,
                address,
                client_key,
                Arc::new(AtomicBool::new(false)),
                0,
            )
            .unwrap();
            if mode == 4 {
                // A completed connection may arrive after unchanged control updates.
                network
                    .apply_map(br#"{"KeepAlive":true}"#, clock(0))
                    .unwrap();
            }
            pool.insert(connection, &mut network, 0).unwrap();
            assert!(pool.contains_region(7));
            assert_eq!(pool.home_region(&mut network, 0), (mode >= 4).then_some(7));
            if mode >= 4 {
                assert!(pool.next_plan(&mut network, 0).unwrap().is_none());
            } else {
                assert!(pool.next_plan(&mut network, 0).unwrap().unwrap().is_home());
            }
            if mode == 4 {
                network
                    .apply_map(&serde_json::to_vec(&map).unwrap(), clock(0))
                    .unwrap();
                assert_eq!(pool.home_region(&mut network, 0), Some(7));
                assert!(pool.next_plan(&mut network, 0).unwrap().is_none());
            }
            network.initiate(peer, 0).unwrap();
            assert!(
                pool.flush_one(&mut network, 0, |_, _| panic!("unexpected direct route"))
                    .unwrap()
            );
            if mode == 2 || mode == 5 {
                network
                    .apply_map(br#"{"PacketFilter":[]}"#, clock(1))
                    .unwrap();
                assert_eq!(pool.poll(&mut network, 1).unwrap().removed, 1);
            } else {
                let started = Instant::now();
                let (mut events, mut rejected, mut removed) = (0, 0, 0);
                while !pool.is_empty() {
                    assert!(started.elapsed() < Duration::from_secs(2));
                    let stats = pool
                        .poll(&mut network, started.elapsed().as_millis() as u64)
                        .unwrap();
                    events += stats.events;
                    rejected += stats.rejected_packets;
                    assert_eq!(
                        stats.handshake_responses, 0,
                        "forged responses must not count"
                    );
                    removed += stats.removed;
                    thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(
                    (events, rejected, removed),
                    (if mode == 6 { 3 } else { 2 }, 1, 1)
                );
                assert!(
                    network
                        .receive_ip(started.elapsed().as_millis() as u64)
                        .unwrap()
                        .is_none()
                );
            }
            assert!(pool.is_empty());
            assert_eq!(pool.home_region(&mut network, 10_000), None);
            assert_eq!(
                pool.next_plan(&mut network, 10_000).unwrap().is_some(),
                mode == 2 || mode == 5
            );
            worker.join().unwrap();
            return;
        }
        let session = connector
            .connect_derp(&node, address, client_key, Arc::new(AtomicBool::new(false)))
            .unwrap();
        if mode == 1 {
            let mut connection = crate::host_derp::Connection::from_session(session, 0).unwrap();
            let started = Instant::now();
            let next = |connection: &mut crate::host_derp::Connection| {
                loop {
                    assert!(started.elapsed() < Duration::from_secs(2));
                    match connection.poll(started.elapsed().as_millis() as u64) {
                        Ok(Some(event)) => return Ok(event),
                        Ok(None) => thread::sleep(Duration::from_millis(1)),
                        Err(error) => return Err(error),
                    }
                }
            };
            assert_eq!(
                next(&mut connection).unwrap(),
                Event::Packet {
                    source: peer,
                    payload: b"incoming".to_vec()
                }
            );
            connection
                .send(peer, b"outgoing", started.elapsed().as_millis() as u64)
                .unwrap();
            connection
                .send(peer, b"second", started.elapsed().as_millis() as u64)
                .unwrap();
            assert!(matches!(
                connection.send(peer, b"overflow", started.elapsed().as_millis() as u64),
                Err(crate::host_derp::Error::Busy)
            ));
            assert_eq!(next(&mut connection).unwrap(), Event::Ping(*b"12345678"));
            assert!(next(&mut connection).is_err());
            assert!(
                connection
                    .send(peer, b"closed", started.elapsed().as_millis() as u64)
                    .is_err()
            );
        } else {
            let mut session = session;
            assert_eq!(
                session.receive().unwrap(),
                Event::Packet {
                    source: peer,
                    payload: b"incoming".to_vec()
                }
            );
            session.send(peer, b"outgoing").unwrap();
            assert_eq!(session.receive().unwrap(), Event::Ping(*b"12345678"));
            assert!(session.receive().is_err());
            assert!(matches!(session.receive(), Err(crate::derp::Error::State)));
        }
        worker.join().unwrap();
    }

    #[test]
    fn relay_overrides_are_checked_before_dialing() {
        use crate::relay::{Address, RelayNode};
        let connector = TlsConnector::mozilla(Duration::from_secs(1)).unwrap();
        let node = RelayNode {
            name: "test".into(),
            host_name: "relay.example".into(),
            certificate: Certificate::Name("relay.example".into()),
            ipv4: Address::Disabled,
            ipv6: Address::Disabled,
            tls_port: 443,
            stun_port: None,
            stun_only: false,
        };
        for variant in [
            node.clone(),
            RelayNode {
                ipv4: Address::Fixed("192.0.2.1".parse().unwrap()),
                ..node.clone()
            },
            RelayNode {
                ipv4: Address::Dns,
                stun_only: true,
                ..node.clone()
            },
            RelayNode {
                ipv4: Address::Dns,
                tls_port: 8443,
                ..node.clone()
            },
        ] {
            let result = connector.connect_relay(
                &variant,
                "127.0.0.1:443".parse().unwrap(),
                Arc::new(AtomicBool::new(false)),
            );
            assert_eq!(result.err().unwrap().kind(), io::ErrorKind::InvalidInput);
        }
    }

    #[test]
    fn validates_name_chain_and_expiry_without_changing_sni() {
        for (expected, expired, trusted, accept) in [
            ("certificate.example", false, true, true),
            ("wrong.example", false, true, false),
            ("certificate.example", false, false, false),
            ("certificate.example", true, true, false),
        ] {
            let (cert, key) = certificate(expired);
            let mut roots = RootCertStore::empty();
            roots
                .add(if trusted {
                    cert.clone()
                } else {
                    certificate(false).0
                })
                .unwrap();
            assert_eq!(
                exchange(cert, key, roots, Certificate::Name(expected.into())),
                accept
            );
        }
    }
    #[test]
    fn exact_certificate_pin_accepts_self_signed_and_rejects_other_certificates() {
        for correct in [true, false] {
            let (cert, key) = certificate(false);
            let pin: [u8; 32] = if correct {
                Sha256::digest(cert.as_ref()).into()
            } else {
                [0; 32]
            };
            assert_eq!(
                exchange(cert, key, RootCertStore::empty(), Certificate::Sha256(pin)),
                correct
            );
        }
    }
    #[test]
    fn stalled_handshake_has_deadline_and_observes_cancellation() {
        for cancel in [false, true] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let cancelled = Arc::new(AtomicBool::new(false));
            let flag = cancelled.clone();
            let (release, done) = std::sync::mpsc::channel();
            let worker = thread::spawn(move || {
                let (_socket, _) = listener.accept().unwrap();
                if cancel {
                    flag.store(true, Ordering::Release);
                }
                let _ = done.recv_timeout(Duration::from_secs(2));
            });
            let connector = TlsConnector::mozilla(Duration::from_millis(150)).unwrap();
            let result = connector.connect(
                addr,
                "dial.example",
                &Certificate::Sha256([1; 32]),
                cancelled,
            );
            assert!(result.is_err());
            let kind = result.err().unwrap().kind();
            assert_eq!(
                kind,
                if cancel {
                    io::ErrorKind::ConnectionAborted
                } else {
                    io::ErrorKind::TimedOut
                }
            );
            release.send(()).unwrap();
            worker.join().unwrap();
        }
    }
}
