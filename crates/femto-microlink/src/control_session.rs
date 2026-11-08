//! Coordinator Noise/HTTP2 session over a caller-authenticated deadline stream.
use crate::{
    derp::DeadlineIo,
    protocol::{
        client::{ControlConnection, ControlStream},
        control::{MapRequest, RegisterRequest},
        h2::Response,
        key::{Challenge, Machine, PrivateKey, PublicKey},
    },
};
use std::{
    io::{self, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
fn control_io(error: crate::protocol::client::ControlClientError) -> io::Error {
    use crate::protocol::client::ControlClientError;
    match error {
        ControlClientError::Io(error) => error,
        ControlClientError::ConnectionClosed => io::Error::new(io::ErrorKind::UnexpectedEof, error),
        error => io::Error::new(io::ErrorKind::InvalidData, error),
    }
}

const REQUEST_LIMIT: usize = 64 * 1024;
const WIRE_BUDGET: usize = 2 * 1024 * 1024;
struct Budget {
    deadline: Instant,
    remaining: usize,
}
struct Limited<S> {
    tls: S,
    budget: Arc<Mutex<Budget>>,
}
impl<S: DeadlineIo> Limited<S> {
    fn limit(&mut self, length: usize) -> io::Result<usize> {
        let budget = self
            .budget
            .lock()
            .map_err(|_| io::Error::other("poisoned control budget"))?;
        if budget.remaining == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "control byte budget exhausted",
            ));
        }
        if Instant::now() >= budget.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "control operation budget exhausted",
            ));
        }
        self.tls.set_deadline(budget.deadline);
        Ok(length.min(budget.remaining))
    }
    fn charge(&self, n: usize) -> io::Result<()> {
        let mut budget = self
            .budget
            .lock()
            .map_err(|_| io::Error::other("poisoned control budget"))?;
        budget.remaining = budget
            .remaining
            .checked_sub(n)
            .ok_or_else(|| io::Error::other("control byte budget"))?;
        Ok(())
    }
}
impl<S: DeadlineIo> Read for Limited<S> {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        if b.is_empty() {
            return Ok(0);
        }
        let limit = self.limit(b.len())?;
        let n = self.tls.read(&mut b[..limit])?;
        self.charge(n)?;
        Ok(n)
    }
}
impl<S: DeadlineIo> Write for Limited<S> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        if b.is_empty() {
            return Ok(0);
        }
        let limit = self.limit(b.len())?;
        let n = self.tls.write(&b[..limit])?;
        self.charge(n)?;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.limit(0)?;
        self.tls.flush()
    }
}

pub struct ControlSession<S: DeadlineIo> {
    endpoint: Endpoint,
    machine: PublicKey<Machine>,
    connection: Option<ControlConnection<Limited<S>>>,
    budget: Arc<Mutex<Budget>>,
    timeout: Duration,
    cancelled: Arc<AtomicBool>,
    enrollment: crate::enrollment::Enrollment,
}
#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub control_key: PublicKey<Machine>,
    pub version: u16,
}
impl<S: DeadlineIo> ControlSession<S> {
    pub fn cancellation(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }
    pub fn permits_node(
        &self,
        node: crate::protocol::key::PublicKey<crate::protocol::key::Node>,
    ) -> bool {
        self.enrollment.permits(node)
            && self.connection.is_some()
            && !self.cancelled.load(Ordering::Acquire)
    }
    /// The stream must authenticate endpoint.host and enforce cancellation.
    /// The control key must come from authenticated bootstrap. This does not enroll.
    pub fn upgrade(
        tls: S,
        endpoint: &Endpoint,
        machine: &PrivateKey<Machine>,
        timeout: Duration,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Self> {
        let authority = crate::bootstrap::authority(&endpoint.host, endpoint.port)?;
        if endpoint.control_key == PublicKey::zero() || cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid control key or cancelled session",
            ));
        }
        let budget = Arc::new(Mutex::new(Budget {
            deadline: Instant::now() + timeout,
            remaining: WIRE_BUDGET,
        }));
        let connection = ControlConnection::upgrade(
            Limited {
                tls,
                budget: budget.clone(),
            },
            authority,
            machine,
            endpoint.control_key,
            endpoint.version,
        )
        .map_err(control_io)?;
        if cancelled.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "control session cancelled",
            ));
        }
        Ok(Self {
            endpoint: endpoint.clone(),
            machine: machine.public(),
            connection: Some(connection),
            budget,
            timeout,
            cancelled,
            enrollment: crate::enrollment::Enrollment::default(),
        })
    }
    pub fn challenge(&self) -> io::Result<PublicKey<Challenge>> {
        self.connection
            .as_ref()
            .map(|c| c.early_noise().node_key_challenge)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "control session closed"))
    }
    /// Creates an identity-bound publisher after successful registration.
    /// The connector constructs its TLS/session on the publication thread and
    /// must use the supplied cancellation token and authenticated endpoint.
    pub fn endpoint_publisher<T: DeadlineIo + 'static>(
        &self,
        request: &crate::protocol::control::MapRequest,
        stack_bytes: usize,
        connect: impl Fn(Arc<AtomicBool>) -> io::Result<ControlSession<T>> + Send + Sync + 'static,
    ) -> io::Result<crate::endpoints::Publisher> {
        if !self.permits_node(request.node_key) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "endpoint publication requires registration",
            ));
        }
        let endpoint = self.endpoint.clone();
        let machine = self.machine;
        let request = request.clone();
        let cancel = self.cancelled.clone();
        crate::endpoints::Publisher::with_sender(
            cancel.clone(),
            stack_bytes,
            Arc::new(move |snapshot| {
                let mut control = connect(cancel.clone())?;
                control.check_publication_binding(&endpoint, machine, &cancel)?;
                let response = control.post_json("/machine/map", &snapshot.body(&request)?)?;
                if response.status != 200 {
                    return Err(io::Error::new(
                        if matches!(response.status, 401 | 403) {
                            io::ErrorKind::PermissionDenied
                        } else {
                            io::ErrorKind::Other
                        },
                        "endpoint update rejected",
                    ));
                }
                Ok(())
            }),
        )
    }

    fn check_publication_binding(
        &self,
        endpoint: &Endpoint,
        machine: PublicKey<Machine>,
        cancel: &Arc<AtomicBool>,
    ) -> io::Result<()> {
        if self.endpoint != *endpoint
            || self.machine != machine
            || !Arc::ptr_eq(&self.cancelled, cancel)
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "endpoint publication session mismatch",
            ));
        }
        Ok(())
    }
    // Keep raw requests private so callers cannot change registration state
    // without clearing the local authorization gate.
    pub(crate) fn post_json(&mut self, path: &str, body: &[u8]) -> io::Result<Response> {
        self.check_cancel()?;
        if body.len() > REQUEST_LIMIT
            || path.len() > 128
            || !path.starts_with('/')
            || !path.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid control request",
            ));
        }
        serde_json::from_slice::<serde_json::Value>(body)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        *self
            .budget
            .lock()
            .map_err(|_| io::Error::other("poisoned control budget"))? = Budget {
            deadline: Instant::now() + self.timeout,
            remaining: WIRE_BUDGET,
        };
        let connection = self
            .connection
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "control session closed"))?;
        match connection.request("POST", path, body, &[], REQUEST_LIMIT) {
            Ok(response) => {
                self.check_cancel()?;
                Ok(response)
            }
            Err(error) => {
                self.close();
                Err(control_io(error))
            }
        }
    }
    /// Registers over this authenticated connection. The caller owns the auth
    /// key in the request; only the serialized temporary is zeroized here.
    /// Rotation uses register_rotation so durable completion gates map access.
    pub fn register(
        &mut self,
        request: &RegisterRequest,
    ) -> io::Result<crate::enrollment::Outcome> {
        self.enrollment.clear();
        if request.node_key == PublicKey::zero() || request.old_node_key != PublicKey::zero() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid node key or unsupported rotation",
            ));
        }
        let body = zeroize::Zeroizing::new(serde_json::to_vec(request).map_err(io::Error::other)?);
        let response = self.post_json("/machine/register", &body)?;
        self.enrollment
            .apply(request.node_key, response.status, &response.body)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
    /// Registers a persisted candidate. The callback must durably complete its
    /// journal before returning success; map/publication access stays revoked
    /// until then. Pending or rejected outcomes never invoke the callback.
    pub fn register_rotation(
        &mut self,
        pending: &crate::rotation::PendingRotation,
        request: &RegisterRequest,
        commit: impl FnOnce(&crate::enrollment::Enrollment) -> io::Result<()>,
    ) -> io::Result<crate::enrollment::Outcome> {
        self.enrollment.clear();
        self.validate_rotation_request(pending, request)?;
        let body = zeroize::Zeroizing::new(serde_json::to_vec(request).map_err(io::Error::other)?);
        let response = self.post_json("/machine/register", &body)?;
        self.finish_rotation_registration(request.node_key, response.status, &response.body, commit)
    }
    fn validate_rotation_request(
        &self,
        pending: &crate::rotation::PendingRotation,
        request: &RegisterRequest,
    ) -> io::Result<()> {
        pending
            .validate_endpoint(&self.endpoint.host, self.endpoint.port)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let candidate = pending.candidate();
        if candidate.machine_key().public() != self.machine
            || request.version != self.endpoint.version
            || request.node_key != candidate.node_key().public()
            || request.old_node_key != pending.old_node_key()
            || request.network_lock_key != candidate.network_lock_key().public()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rotation request identity mismatch",
            ));
        }
        Ok(())
    }
    fn finish_rotation_registration(
        &mut self,
        node: crate::protocol::key::PublicKey<crate::protocol::key::Node>,
        status: u16,
        body: &[u8],
        commit: impl FnOnce(&crate::enrollment::Enrollment) -> io::Result<()>,
    ) -> io::Result<crate::enrollment::Outcome> {
        self.enrollment.clear();
        let mut authorized = crate::enrollment::Enrollment::default();
        let outcome = authorized
            .apply(node, status, body)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        self.check_cancel()?;
        if outcome == crate::enrollment::Outcome::Authorized {
            commit(&authorized)?;
            self.check_cancel()?;
            self.enrollment = authorized;
        }
        Ok(outcome)
    }
    /// Starts one map stream after successful registration of the same key.
    /// The guard owns this connection and the map-target borrow; dropping it
    /// closes transport and disconnects the coordinator or full network owner.
    pub fn start_map<'session, 'map, T: crate::map_session::MapTarget>(
        &'session mut self,
        request: &MapRequest,
        coordinator: &'map mut T,
    ) -> io::Result<MapStream<'session, 'map, T, S>> {
        let maps = crate::map_session::MapSession::new(coordinator);
        if !self.enrollment.permits(request.node_key) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "node has not completed registration",
            ));
        }
        let body = serde_json::to_vec(request).map_err(io::Error::other)?;
        if !request.stream
            || !request.keep_alive
            || body.len() > REQUEST_LIMIT
            || !request.map_session_handle.is_empty()
            || request.map_session_seq != 0
            || request.omit_peers
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "map request must request a fresh streaming full map with keepalives",
            ));
        }
        self.reset_budget()?;
        let connection = self
            .connection
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "control session closed"))?;
        let remote = match connection.start_json_stream("/machine/map", request, &[]) {
            Ok(remote) => remote,
            Err(error) => {
                self.close();
                return Err(control_io(error));
            }
        };
        Ok(MapStream {
            control: self,
            maps,
            remote,
            closed: false,
        })
    }
    fn check_cancel(&mut self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            self.close();
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "control session cancelled",
            ));
        }
        Ok(())
    }
    fn reset_budget(&mut self) -> io::Result<()> {
        self.check_cancel()?;
        *self
            .budget
            .lock()
            .map_err(|_| io::Error::other("poisoned control budget"))? = Budget {
            deadline: Instant::now() + self.timeout,
            remaining: WIRE_BUDGET,
        };
        Ok(())
    }
    pub fn close(&mut self) {
        self.enrollment.clear();
        self.connection = None;
    }
}

pub struct MapStream<'session, 'map, T: crate::map_session::MapTarget, S: DeadlineIo> {
    control: &'session mut ControlSession<S>,
    maps: crate::map_session::MapSession<'map, T>,
    remote: ControlStream,
    closed: bool,
}
impl<T: crate::map_session::MapTarget, S: DeadlineIo> MapStream<'_, '_, T, S> {
    pub fn target(&mut self) -> &mut T {
        self.maps.target()
    }
    /// The clock callback runs after the network wait, so expiry translation
    /// does not use a timestamp captured before a long-poll read.
    pub fn next(
        &mut self,
        clock: impl FnOnce() -> crate::coordinator::Clock,
    ) -> io::Result<crate::map_session::Progress> {
        let result = self.next_inner(clock);
        if result.is_err() || matches!(result, Ok(crate::map_session::Progress::Ended)) {
            self.close();
        }
        result
    }
    fn next_inner(
        &mut self,
        clock: impl FnOnce() -> crate::coordinator::Clock,
    ) -> io::Result<crate::map_session::Progress> {
        if self.closed {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "map stream closed",
            ));
        }
        self.control.reset_budget()?;
        let connection =
            self.control.connection.as_mut().ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotConnected, "control session closed")
            })?;
        let event = connection
            .read_stream_event(&mut self.remote)
            .map_err(control_io)?;
        self.control.check_cancel()?;
        self.maps.apply(event, clock()).map_err(|error| {
            let kind = if matches!(error, crate::map_session::Error::Status(401 | 403)) {
                io::ErrorKind::PermissionDenied
            } else {
                io::ErrorKind::InvalidData
            };
            io::Error::new(kind, error)
        })
    }
    pub fn close(&mut self) {
        self.control.cancelled.store(true, Ordering::Release);
        self.maps.close();
        self.control.close();
        self.closed = true;
    }
}
impl<S: DeadlineIo> MapStream<'_, '_, crate::coordinator::Coordinator, S> {
    pub fn coordinator(&mut self) -> &mut crate::coordinator::Coordinator {
        self.target()
    }
}
impl<T: crate::map_session::MapTarget, S: DeadlineIo> Drop for MapStream<'_, '_, T, S> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    use crate::protocol::{control::HostInfo, identity::DeviceIdentity};
    struct Unused(std::io::Cursor<Vec<u8>>);
    impl Read for Unused {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            self.0.read(b)
        }
    }
    impl Write for Unused {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.write(b)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    impl DeadlineIo for Unused {
        fn set_deadline(&mut self, _: Instant) {}
    }
    #[test]
    fn control_errors_preserve_io_and_reject_protocol_failures() {
        use crate::protocol::client::ControlClientError;
        for kind in [
            io::ErrorKind::TimedOut,
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::PermissionDenied,
        ] {
            assert_eq!(
                control_io(ControlClientError::Io(io::Error::new(kind, "transport"))).kind(),
                kind
            );
        }
        assert_eq!(
            control_io(ControlClientError::ConnectionClosed).kind(),
            io::ErrorKind::UnexpectedEof
        );
        for error in [
            ControlClientError::WrongUpgradeProtocol,
            ControlClientError::InvalidEarlyNoise,
            ControlClientError::NoiseFrameTooLarge(65536),
            ControlClientError::StreamReset(1),
        ] {
            assert_eq!(control_io(error).kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn byte_exhaustion_is_distinct_from_a_transport_timeout() {
        let budget = Arc::new(Mutex::new(Budget {
            deadline: Instant::now() + Duration::from_secs(1),
            remaining: 0,
        }));
        let mut stream = Limited {
            tls: Unused(io::Cursor::new(vec![])),
            budget: budget.clone(),
        };
        assert_eq!(
            stream.limit(1).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        *budget.lock().unwrap() = Budget {
            deadline: Instant::now(),
            remaining: 10,
        };
        assert_eq!(stream.limit(1).unwrap_err().kind(), io::ErrorKind::TimedOut);
    }

    fn closed() -> ControlSession<Unused> {
        let identity = DeviceIdentity::generate().unwrap();
        ControlSession {
            endpoint: Endpoint {
                host: "control.example".into(),
                port: 443,
                control_key: identity.machine_key().public(),
                version: 100,
            },
            machine: identity.machine_key().public(),
            connection: None,
            budget: Arc::new(Mutex::new(Budget {
                deadline: Instant::now(),
                remaining: 0,
            })),
            timeout: Duration::from_secs(1),
            cancelled: Arc::new(AtomicBool::new(false)),
            enrollment: Default::default(),
        }
    }
    #[test]
    fn rotation_request_checks_endpoint_machine_and_all_key_fields_before_io() {
        let identity = DeviceIdentity::generate().unwrap();
        let pending =
            crate::rotation::PendingRotation::prepare(&identity, "control.example", 443).unwrap();
        let candidate = pending.candidate();
        let mut control = closed();
        control.machine = identity.machine_key().public();
        let request = || {
            RegisterRequest::new(
                100,
                candidate.node_key().public(),
                candidate.network_lock_key().public(),
                HostInfo::esp32("test", ""),
            )
            .rotating_from(pending.old_node_key())
        };
        control
            .validate_rotation_request(&pending, &request())
            .unwrap();
        for change in 0..6 {
            let mut invalid = request();
            match change {
                0 => invalid.node_key = identity.node_key().public(),
                1 => invalid.old_node_key = PublicKey::zero(),
                2 => {
                    invalid.network_lock_key = DeviceIdentity::generate()
                        .unwrap()
                        .network_lock_key()
                        .public()
                }
                3 => invalid.version += 1,
                4 => control.endpoint.port = 8443,
                _ => control.machine = DeviceIdentity::generate().unwrap().machine_key().public(),
            }
            let error = control
                .register_rotation(&pending, &invalid, |_| panic!("invalid request committed"))
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            control.endpoint.port = 443;
        }
    }

    #[test]
    fn rotation_authorization_requires_successful_commit_and_survives_no_cancellation() {
        let node = DeviceIdentity::generate().unwrap().node_key().public();
        let mut control = closed();
        for body in [
            br#"{"AuthURL":"https://login.example/token"}"#.as_slice(),
            br#"{"NodeKeyExpired":true}"#,
            br#"{"Error":"rejected"}"#,
            b"{}",
        ] {
            control
                .finish_rotation_registration(node, 200, body, |_| panic!("unauthorized commit"))
                .unwrap();
            assert!(!control.enrollment.permits(node));
        }
        let authorized = br#"{"MachineAuthorized":true}"#;
        assert!(
            control
                .finish_rotation_registration(node, 200, authorized, |_| Err(io::Error::other(
                    "storage failure"
                )))
                .is_err()
        );
        assert!(!control.enrollment.permits(node));
        control
            .finish_rotation_registration(node, 200, authorized, |proof| {
                assert!(proof.permits(node));
                Ok(())
            })
            .unwrap();
        assert!(control.enrollment.permits(node));
        assert!(
            control
                .finish_rotation_registration(node, 200, b"bad JSON", |_| panic!(
                    "malformed response committed"
                ))
                .is_err()
        );
        assert!(!control.enrollment.permits(node));
        let cancellation = control.cancelled.clone();
        assert!(
            control
                .finish_rotation_registration(node, 200, authorized, |_| {
                    cancellation.store(true, Ordering::Release);
                    Ok(())
                })
                .is_err()
        );
        assert!(!control.enrollment.permits(node));
        assert!(
            control
                .finish_rotation_registration(node, 200, authorized, |_| panic!(
                    "cancelled response committed"
                ))
                .is_err()
        );
    }

    #[test]
    fn unregistered_session_cannot_create_publication_worker() {
        let control = closed();
        let identity = DeviceIdentity::generate().unwrap();
        let request = MapRequest::new(
            100,
            identity.node_key().public(),
            identity.disco_key().public(),
            HostInfo::esp32("test", "test"),
        );
        let error = control
            .endpoint_publisher(
                &request,
                128 * 1024,
                |_| -> io::Result<ControlSession<Unused>> {
                    panic!("unregistered publisher connected")
                },
            )
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
    #[test]
    fn publication_connection_is_bound_to_endpoint_machine_and_cancellation_generation() {
        let control = closed();
        let endpoint = control.endpoint.clone();
        let machine = control.machine;
        let cancel = control.cancelled.clone();
        control
            .check_publication_binding(&endpoint, machine, &cancel)
            .unwrap();
        for change in 0..4 {
            let mut different = endpoint.clone();
            match change {
                0 => different.host = "other.example".into(),
                1 => different.port = 8443,
                2 => different.version += 1,
                _ => {
                    different.control_key =
                        DeviceIdentity::generate().unwrap().machine_key().public()
                }
            }
            assert!(
                control
                    .check_publication_binding(&different, machine, &cancel)
                    .is_err()
            );
        }
        assert!(
            control
                .check_publication_binding(
                    &endpoint,
                    DeviceIdentity::generate().unwrap().machine_key().public(),
                    &cancel
                )
                .is_err()
        );
        assert!(
            control
                .check_publication_binding(&endpoint, machine, &Arc::new(AtomicBool::new(false)))
                .is_err()
        );
    }
}
