//! Cancellable host registration using the durable identity and rotation store.
use crate::{
    enrollment::{ApprovalUrl, Outcome},
    host_control::ControlSession,
    protocol::{
        CAPABILITY_VERSION,
        control::{HostInfo, RegisterRequest},
        identity::DeviceIdentity,
        key::{Machine, PublicKey},
    },
    rotation::PendingRotation,
    store::HostStore,
};
use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Registration inputs. Credential values are consumed into the request.
pub struct Options {
    pub host: String,
    pub address: SocketAddr,
    pub pinned_key: Option<PublicKey<Machine>>,
    pub host_info: HostInfo,
    pub auth_key: Option<String>,
    pub followup: Option<String>,
    pub rotate_node_key: bool,
}
pub struct Registration {
    pub control: ControlSession,
    pub identity: DeviceIdentity,
    pub outcome: Outcome,
}

pub type ApplicationRuntime =
    crate::host_runtime::Runtime<Box<dyn FnMut() -> crate::coordinator::Clock + Send>>;

pub enum Connection {
    /// Registration succeeded. Peer-map authorization may still be pending.
    Connected(Box<ApplicationRuntime>),
    /// No runtime is retained for an unauthorized registration outcome.
    NotAuthorized(Outcome),
}

/// Registers and starts an application runtime, consuming the identity-store lock.
/// This blocking operation belongs on a worker. The returned runtime must be pumped.
/// The flag cancels setup. Close the returned runtime explicitly to stop reconnects.
pub fn connect(
    store: HostStore,
    options: Options,
    bind: SocketAddr,
    cancelled: Arc<AtomicBool>,
) -> io::Result<Connection> {
    connect_with_tls(
        store,
        options,
        bind,
        cancelled,
        crate::host_tls::TlsConnector::mozilla(Duration::from_secs(60))?,
    )
}
/// Uses explicit coordinator trust for bootstrap, registration, publication and reconnect.
/// Relay and application TLS retain their own independently configured trust.
pub fn connect_with_tls(
    store: HostStore,
    options: Options,
    bind: SocketAddr,
    cancelled: Arc<AtomicBool>,
    tls: crate::host_tls::TlsConnector,
) -> io::Result<Connection> {
    connect_with_transport(
        store,
        options,
        bind,
        cancelled,
        tls,
        crate::host_runtime::TransportConfig::mozilla()?,
    )
}
/// Selects coordinator trust and independent relay/UDP settings before enrollment.
pub fn connect_with_transport(
    mut store: HostStore,
    options: Options,
    bind: SocketAddr,
    cancelled: Arc<AtomicBool>,
    tls: crate::host_tls::TlsConnector,
    transport: crate::host_runtime::TransportConfig,
) -> io::Result<Connection> {
    options.validate()?;
    if cancelled.load(Ordering::Acquire) {
        return Err(io::ErrorKind::Interrupted.into());
    }
    // Bind failures must not register a node or prepare a rotation.
    let udp = crate::host_udp::UdpTransport::bind(bind)?;
    let host = options.host.clone();
    let address = options.address;
    let pinned_key = options.pinned_key;
    let host_info = options.host_info.clone();
    let Registration {
        mut control,
        identity,
        outcome,
    } = register_with_tls(&mut store, options, cancelled.clone(), &tls)?;
    if outcome != Outcome::Authorized {
        control.close();
        return Ok(Connection::NotAuthorized(outcome));
    }
    if cancelled.load(Ordering::Acquire) {
        control.close();
        return Err(io::ErrorKind::Interrupted.into());
    }
    let started = Instant::now();
    let unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs();
    let timestamps =
        crate::timestamp::TimestampClock::open(store, unix_seconds).map_err(io::Error::other)?;
    let network =
        crate::network::Network::new(&identity, timestamps, 256, 8).map_err(io::Error::other)?;
    let map = crate::protocol::control::MapRequest::new(
        CAPABILITY_VERSION,
        identity.node_key().public(),
        identity.disco_key().public(),
        host_info.clone(),
    )
    .streaming();
    let publisher = control.endpoint_publisher(identity.machine_key(), &map)?;
    let reconnect = crate::host_reconnect::Controller::with_tls(
        host,
        address,
        pinned_key,
        identity.clone(),
        host_info,
        publisher,
        tls,
    );
    let owner =
        crate::host_control_worker::ControlledNetwork::start(control.into_inner(), map, network)
            .map_err(io::Error::other)?;
    let clock: Box<dyn FnMut() -> crate::coordinator::Clock + Send> =
        Box::new(move || crate::coordinator::Clock {
            unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|value| i64::try_from(value.as_millis()).ok())
                .unwrap_or(-1),
            monotonic_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        });
    let mut runtime = crate::host_runtime::Runtime::with_transport(
        owner,
        udp,
        reconnect,
        identity.node_key().clone(),
        clock,
        transport,
    )?;
    if cancelled.load(Ordering::Acquire) {
        runtime.close();
        return Err(io::ErrorKind::Interrupted.into());
    }
    Ok(Connection::Connected(Box::new(runtime)))
}
struct Prepared {
    identity: DeviceIdentity,
    pending: Option<PendingRotation>,
    request: RegisterRequest,
}
impl Options {
    fn validate(&self) -> io::Result<()> {
        crate::bootstrap::authority(&self.host, self.address.port())?;
        if self.pinned_key == Some(PublicKey::zero())
            || (self.auth_key.is_some() && self.followup.is_some())
            || self.host_info.hostname.is_empty()
            || self.host_info.hostname.len() > 63
            || !self
                .host_info
                .hostname
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if let Some(value) = &self.auth_key
            && (value.is_empty()
                || value.len() > 4096
                || value
                    .bytes()
                    .any(|b| b.is_ascii_whitespace() || b.is_ascii_control()))
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        if let Some(value) = &self.followup {
            ApprovalUrl::parse(value).map_err(|_| io::ErrorKind::InvalidInput)?;
        }
        Ok(())
    }
    fn prepare(&mut self, store: &mut HostStore, cancelled: &AtomicBool) -> io::Result<Prepared> {
        self.validate()?;
        if cancelled.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let pending = if self.rotate_node_key {
            Some(
                store
                    .begin_node_key_rotation(&self.host, self.address.port())
                    .map_err(io::Error::other)?
                    .clone(),
            )
        } else {
            None
        };
        let identity = match &pending {
            Some(pending) => pending.candidate().clone(),
            None => store.identity().map_err(io::Error::other)?.clone(),
        };
        let mut request = RegisterRequest::new(
            CAPABILITY_VERSION,
            identity.node_key().public(),
            identity.network_lock_key().public(),
            self.host_info.clone(),
        );
        if let Some(pending) = &pending {
            request = request.rotating_from(pending.old_node_key());
        }
        if let Some(value) = self.auth_key.take() {
            request = request.with_auth_key(value);
        }
        if let Some(value) = self.followup.take() {
            request = request.with_followup(value);
        }
        Ok(Prepared {
            identity,
            pending,
            request,
        })
    }
}

/// Runs blocking TLS/Noise registration; call from a worker for interactive clients.
/// Cancellation leaves a prepared rotation available for authenticated resumption.
/// Awaiting approval, expiry and rejection are returned without granting map access.
pub fn register(
    store: &mut HostStore,
    options: Options,
    cancelled: Arc<AtomicBool>,
) -> io::Result<Registration> {
    register_with_tls(
        store,
        options,
        cancelled,
        &crate::host_tls::TlsConnector::mozilla(Duration::from_secs(60))?,
    )
}
pub fn register_with_tls(
    store: &mut HostStore,
    mut options: Options,
    cancelled: Arc<AtomicBool>,
    tls: &crate::host_tls::TlsConnector,
) -> io::Result<Registration> {
    let prepared = options.prepare(store, &cancelled)?;
    let key = match options.pinned_key {
        Some(key) => key,
        None => crate::host_bootstrap::fetch_control_key_with_tls(
            &options.host,
            options.address,
            CAPABILITY_VERSION,
            &tls.with_timeout(Duration::from_secs(10))?,
            cancelled.clone(),
        )?,
    };
    let mut control = ControlSession::connect_with_tls(
        &options.host,
        options.address,
        prepared.identity.machine_key(),
        key,
        CAPABILITY_VERSION,
        tls.with_timeout(Duration::from_secs(60))?,
        cancelled,
    )?;
    let outcome = match &prepared.pending {
        Some(pending) => control.register_rotation(pending, &prepared.request, |authorized| {
            store
                .complete_node_key_rotation(&options.host, options.address.port(), authorized)
                .map_err(io::Error::other)
        })?,
        None => control.register(&prepared.request)?,
    };
    Ok(Registration {
        control,
        identity: prepared.identity,
        outcome,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn options() -> Options {
        Options {
            host: "control.example".into(),
            address: "127.0.0.1:443".parse().unwrap(),
            pinned_key: None,
            host_info: HostInfo {
                ipn_version: "femto-test".into(),
                backend_log_id: String::new(),
                os: "linux".into(),
                os_version: String::new(),
                device_model: "test".into(),
                hostname: "femto-test".into(),
                machine: "test".into(),
                architecture: "test".into(),
                no_logs_no_support: true,
                userspace: true,
                net_info: None,
            },
            auth_key: None,
            followup: None,
            rotate_node_key: true,
        }
    }
    #[test]
    fn connection_failures_release_store_without_preparing_rotation() {
        fn assert_send<T: Send>() {}
        assert_send::<Connection>();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity");
        let store = HostStore::open(&path).unwrap();
        let node = store.identity().unwrap().node_key().public();
        let occupied = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let bind = occupied.local_addr().unwrap();
        assert_eq!(
            connect(store, options(), bind, Arc::new(AtomicBool::new(false)))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::AddrInUse
        );
        let store = HostStore::open(&path).unwrap();
        assert_eq!(store.identity().unwrap().node_key().public(), node);
        assert!(!path.join("rotation.bin").exists());
        assert_eq!(
            connect(store, options(), bind, Arc::new(AtomicBool::new(true)))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Interrupted
        );
        let store = HostStore::open(&path).unwrap();
        assert_eq!(store.identity().unwrap().node_key().public(), node);
        assert!(!path.join("rotation.bin").exists());
    }
    #[test]
    fn invalid_inputs_and_cancellation_do_not_prepare_rotation_or_connect() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity");
        let mut store = HostStore::open(&path).unwrap();
        let node = store.identity().unwrap().node_key().public();
        for case in 0..8 {
            let mut config = options();
            match case {
                0 => config.host = "https://control.example".into(),
                1 => config.pinned_key = Some(PublicKey::zero()),
                2 => config.host_info.hostname = "bad hostname".into(),
                3 => config.auth_key = Some("two tokens".into()),
                4 => config.auth_key = Some("x".repeat(4097)),
                5 => config.followup = Some("http://control.example/approve".into()),
                6 => {
                    config.auth_key = Some("test-token".into());
                    config.followup = Some("https://control.example/approve".into());
                }
                _ => (),
            }
            let result = register(&mut store, config, Arc::new(AtomicBool::new(case == 7)));
            assert_eq!(
                result.err().unwrap().kind(),
                if case == 7 {
                    io::ErrorKind::Interrupted
                } else {
                    io::ErrorKind::InvalidInput
                }
            );
            assert_eq!(store.identity().unwrap().node_key().public(), node);
            assert!(!path.join("rotation.bin").exists());
        }
    }
    #[test]
    fn prepared_rotation_resumes_same_candidate_and_rejects_other_coordinator() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity");
        let mut store = HostStore::open(&path).unwrap();
        let original = store.identity().unwrap().node_key().public();
        let cancelled = AtomicBool::new(false);
        let first = options().prepare(&mut store, &cancelled).unwrap();
        let candidate = first.identity.node_key().public();
        assert_ne!(candidate, original);
        assert_eq!(first.pending.unwrap().old_node_key(), original);
        drop(store);
        let mut store = HostStore::open(&path).unwrap();
        let second = options().prepare(&mut store, &cancelled).unwrap();
        assert_eq!(second.identity.node_key().public(), candidate);
        let mut ordinary = options();
        ordinary.rotate_node_key = false;
        assert!(ordinary.prepare(&mut store, &cancelled).is_err());
        let mut other = options();
        other.host = "other.example".into();
        assert!(other.prepare(&mut store, &cancelled).is_err());
        assert_eq!(
            options()
                .prepare(&mut store, &cancelled)
                .unwrap()
                .identity
                .node_key()
                .public(),
            candidate
        );
    }
}
