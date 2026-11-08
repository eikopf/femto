//! Host TLS connection and endpoint publication around the shared control session.
use crate::{
    control_session::{self, Endpoint},
    host_tls::{TlsConnector, TlsStream},
    protocol::{
        control::MapRequest,
        key::{Machine, PrivateKey, PublicKey},
    },
    relay::Certificate,
};
use std::{
    io,
    net::SocketAddr,
    ops::{Deref, DerefMut},
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
pub type HostMapStream<'session, 'map, T = crate::coordinator::Coordinator> =
    control_session::MapStream<'session, 'map, T, TlsStream>;
pub struct ControlSession {
    tls: TlsConnector,
    inner: control_session::ControlSession<TlsStream>,
    host: String,
    address: SocketAddr,
    machine: PublicKey<Machine>,
    control_key: PublicKey<Machine>,
    version: u16,
}
impl Deref for ControlSession {
    type Target = control_session::ControlSession<TlsStream>;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
impl DerefMut for ControlSession {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}
impl ControlSession {
    pub fn into_inner(self) -> control_session::ControlSession<TlsStream> {
        self.inner
    }
    pub fn connect(
        host: &str,
        address: SocketAddr,
        machine: &PrivateKey<Machine>,
        control_key: PublicKey<Machine>,
        version: u16,
        timeout: Duration,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        Self::connect_with_tls(
            host,
            address,
            machine,
            control_key,
            version,
            TlsConnector::mozilla(timeout)?,
            cancelled,
        )
    }
    pub fn connect_with_tls(
        host: &str,
        address: SocketAddr,
        machine: &PrivateKey<Machine>,
        control_key: PublicKey<Machine>,
        version: u16,
        connector: TlsConnector,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        crate::bootstrap::authority(host, address.port())?;
        if control_key == PublicKey::zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid control key",
            ));
        }
        let tls = connector.connect(
            address,
            host,
            &Certificate::Name(host.into()),
            cancelled.clone(),
        )?;
        let endpoint = Endpoint {
            host: host.into(),
            port: address.port(),
            control_key,
            version,
        };
        let inner = control_session::ControlSession::upgrade(
            tls,
            &endpoint,
            machine,
            connector.timeout(),
            cancelled,
        )?;
        Ok(Self {
            tls: connector,
            inner,
            host: host.into(),
            address,
            machine: machine.public(),
            control_key,
            version,
        })
    }
    /// Creates a publication worker only for the registered node and same machine identity.
    pub fn endpoint_publisher(
        &self,
        machine: &PrivateKey<Machine>,
        request: &MapRequest,
    ) -> io::Result<crate::host_endpoints::Publisher> {
        if !self.inner.permits_node(request.node_key) || machine.public() != self.machine {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "endpoint publication requires the registered identity",
            ));
        }
        let host = self.host.clone();
        let address = self.address;
        let control_key = self.control_key;
        let version = self.version;
        let machine = machine.clone();
        let tls = self.tls.with_timeout(Duration::from_secs(5))?;
        self.inner
            .endpoint_publisher(request, 2 * 1024 * 1024, move |cancelled| {
                Self::connect_with_tls(
                    &host,
                    address,
                    &machine,
                    control_key,
                    version,
                    tls.clone(),
                    cancelled,
                )
                .map(|control| control.inner)
            })
    }
}
