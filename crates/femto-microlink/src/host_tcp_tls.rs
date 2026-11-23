//! Certificate-verified TLS over managed tailnet TCP streams.
use crate::host_tcp::{Driver, Stream};
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned, pki_types::ServerName};
use std::{
    io::{self, Read, Write},
    sync::{Arc, atomic::AtomicBool},
};

#[derive(Clone)]
pub struct Connector {
    config: Arc<ClientConfig>,
}
impl Connector {
    /// Uses one explicit DER trust anchor, at most 16 KiB, with hostname validation.
    pub fn with_der_root(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > 16 * 1024 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut roots = RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(bytes.to_vec()))
            .map_err(|_| io::ErrorKind::InvalidInput)?;
        Self::with_roots(roots)
    }
    pub fn mozilla() -> io::Result<Self> {
        Self::with_roots(RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        })
    }
    /// Custom trust roots retain normal certificate chain and hostname validation.
    pub fn with_roots(roots: RootCertStore) -> io::Result<Self> {
        let mut config =
            ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .map_err(io::Error::other)?
                .with_root_certificates(roots)
                .with_no_client_auth();
        config.resumption = rustls::client::Resumption::disabled();
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self {
            config: Arc::new(config),
        })
    }
    pub fn connect<D: Driver>(
        &self,
        driver: D,
        name: &str,
        port: u16,
        limit: crate::application_tcp::Limit,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<TlsStream<D>> {
        let identity =
            ServerName::try_from(name.to_owned()).map_err(|_| io::ErrorKind::InvalidInput)?;
        let mut connection =
            ClientConnection::new(self.config.clone(), identity).map_err(io::Error::other)?;
        connection.set_buffer_limit(Some(16 * 1024));
        let mut socket = Stream::connect(driver, name, port, limit, cancelled)?;
        while connection.is_handshaking() {
            socket.authorize()?;
            connection.complete_io(&mut socket)?;
        }
        socket.authorize()?;
        Ok(TlsStream {
            inner: Some(StreamOwned::new(connection, socket)),
        })
    }
}
pub struct TlsStream<D: Driver> {
    inner: Option<StreamOwned<ClientConnection, Stream<D>>>,
}
impl<D: Driver> TlsStream<D> {
    fn operation<R>(
        &mut self,
        operation: impl FnOnce(&mut StreamOwned<ClientConnection, Stream<D>>) -> io::Result<R>,
    ) -> io::Result<R> {
        let result = (|| {
            let inner = self.inner.as_mut().ok_or(io::ErrorKind::NotConnected)?;
            // TLS may satisfy a read from plaintext buffered before a control update.
            inner.sock.authorize()?;
            let result = operation(inner)?;
            inner.sock.authorize()?;
            Ok(result)
        })();
        if result.is_err() {
            self.inner = None;
        }
        result
    }
}
impl<D: Driver> Read for TlsStream<D> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let mut staging = vec![0; bytes.len().min(16 * 1024)];
        let count = self.operation(|inner| inner.read(&mut staging))?;
        bytes[..count].copy_from_slice(&staging[..count]);
        Ok(count)
    }
}
impl<D: Driver> Write for TlsStream<D> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.operation(|inner| inner.write(bytes))
    }
    fn flush(&mut self) -> io::Result<()> {
        self.operation(|inner| inner.flush())
    }
}
