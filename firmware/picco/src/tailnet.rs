//! Device enrollment and authenticated map-worker ownership.
use femto_microlink::{
    control_session::Endpoint,
    control_worker::{ControlledNetwork, Reconnect, ReconnectProgress, StopReason},
    coordinator::Clock,
    device_config::Config,
    device_enrollment::{Context, Followup},
    endpoints::{Progress as PublicationProgress, Publisher, Snapshot},
    enrollment::Outcome,
    network::Network,
    protocol::{
        CAPABILITY_VERSION,
        control::{HostInfo, MapRequest},
    },
    socket_network,
    socket_udp::UdpTransport,
    timestamp::TimestampClock,
};
use std::{
    io,
    net::Ipv4Addr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub enum Registration {
    Outcome(Outcome),
    Failed,
    Retrying,
}
struct Setup {
    config: Config,
    identity: femto_microlink::protocol::identity::DeviceIdentity,
    enrollment: Context,
    followup: Option<Followup>,
    authorized: AtomicBool,
}
fn setup_worker(
    setup: Arc<Setup>,
    sender: SyncSender<Registration>,
    publish_sender: SyncSender<Publisher>,
) -> impl FnOnce(
    Arc<AtomicBool>,
) -> io::Result<(
    femto_microlink::control_session::ControlSession<crate::network::Connection>,
    MapRequest,
)> + Send {
    move |cancel| {
        let config = &setup.config;
        let identity = &setup.identity;
        let worker_enrollment = &setup.enrollment;
        let previously_authorized = setup.authorized.load(Ordering::Acquire);
        let mut reported = false;
        let result = (|| {
            let control_key = crate::control::fetch_control_key(
                &config.host,
                config.port,
                CAPABILITY_VERSION,
                Instant::now() + Duration::from_secs(15),
                cancel.clone(),
            )?;
            let endpoint = Endpoint {
                host: config.host.clone(),
                port: config.port,
                control_key,
                version: CAPABILITY_VERSION,
            };
            let mut control = crate::control::connect_session(
                &endpoint,
                identity.machine_key(),
                Duration::from_secs(60),
                cancel.clone(),
            )?;
            let mut host = HostInfo::esp32(&config.hostname, "");
            host.ipn_version = format!("femto/{}", env!("CARGO_PKG_VERSION"));
            host.device_model = "BOOX Picco".into();
            let request = worker_enrollment
                .registration(
                    identity.network_lock_key().public(),
                    host.clone(),
                    if previously_authorized {
                        None
                    } else {
                        config.auth_key()
                    },
                    if previously_authorized {
                        None
                    } else {
                        setup.followup.as_ref()
                    },
                )
                .map_err(io::Error::other)?;
            let outcome = control.register(&request)?;
            let authorized = matches!(outcome, Outcome::Authorized);
            if authorized {
                setup.authorized.store(true, Ordering::Release);
            }
            sender
                .try_send(Registration::Outcome(outcome))
                .map_err(|_| io::Error::other("registration receiver unavailable"))?;
            reported = true;
            if !authorized {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "registration not authorized",
                ));
            }
            let request = MapRequest::new(
                CAPABILITY_VERSION,
                identity.node_key().public(),
                identity.disco_key().public(),
                host,
            )
            .streaming();
            let machine = identity.machine_key().clone();
            let publication_endpoint = endpoint.clone();
            let publisher = control.endpoint_publisher(&request, 128 * 1024, move |cancel| {
                crate::clock::require_ready()?;
                crate::control::connect_session(
                    &publication_endpoint,
                    &machine,
                    Duration::from_secs(5),
                    cancel,
                )
            })?;
            publish_sender
                .try_send(publisher)
                .map_err(|_| io::Error::other("endpoint publisher receiver unavailable"))?;
            Ok((control, request))
        })();
        if result.is_err() && !reported {
            let retryable = result
                .as_ref()
                .err()
                .is_some_and(|error: &io::Error| StopReason::SetupFailed(error.kind()).retryable());
            let _ = sender.try_send(if retryable {
                Registration::Retrying
            } else {
                Registration::Failed
            });
        }
        result
    }
}
pub struct Runtime {
    local_address: femto_microlink::endpoints::LocalAddress,
    publisher: Option<Publisher>,
    publication: Receiver<Publisher>,
    owner: ControlledNetwork<crate::identity::Store>,
    udp: UdpTransport,
    relays: crate::relay_transport::Pool,
    dialer: femto_microlink::relay_worker::Dialer<crate::relay_transport::Tls>,
    node_key: femto_microlink::protocol::key::PrivateKey<femto_microlink::protocol::key::Node>,
    enrollment: Context,
    registration: Receiver<Registration>,
    started: Instant,
    assigned: bool,
    setup: Arc<Setup>,
    retry: Reconnect,
    reconnecting: bool,
    stopped: bool,
}
impl Runtime {
    /// The caller explicitly opens or provisions the store before starting.
    pub fn start(config: Config, store: crate::identity::Store) -> Result<Self, String> {
        Self::start_with_followup(config, store, None)
    }
    pub fn start_with_followup(
        config: Config,
        store: crate::identity::Store,
        followup: Option<Followup>,
    ) -> Result<Self, String> {
        config.validate()?;
        crate::clock::require_ready().map_err(|e| e.to_string())?;
        let identity = store.identity().map_err(|e| e.to_string())?.clone();
        let enrollment = Context::new(&config, &identity);
        if let Some(followup) = &followup {
            enrollment.validate(followup)?;
        }

        let node_key = identity.node_key().clone();
        let unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_secs();
        let timestamps = TimestampClock::open(store, unix).map_err(|e| e.to_string())?;
        let network = Network::new(&identity, timestamps, 64, 4).map_err(|e| e.to_string())?;
        let udp = UdpTransport::bind("0.0.0.0:0".parse().unwrap()).map_err(|e| e.to_string())?;
        let started = Instant::now();
        let (sender, registration) = mpsc::sync_channel(1);
        let (publish_sender, publication) = mpsc::sync_channel(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let setup = Arc::new(Setup {
            config,
            identity,
            enrollment: enrollment.clone(),
            followup,
            authorized: AtomicBool::new(false),
        });
        let owner = ControlledNetwork::start_with(
            network,
            cancel,
            128 * 1024,
            setup_worker(setup.clone(), sender, publish_sender),
        )
        .map_err(|e| e.to_string())?;
        Ok(Self {
            local_address: Default::default(),
            publisher: None,
            publication,
            owner,
            udp,
            relays: Default::default(),
            dialer: Default::default(),
            node_key,
            enrollment,
            registration,
            started,
            assigned: false,
            setup,
            retry: Reconnect::default(),
            reconnecting: false,
            stopped: false,
        })
    }
    pub fn set_local_address_source(&mut self, source: femto_microlink::endpoints::LocalAddress) {
        self.local_address = source;
    }
    pub fn approval(&self, url: femto_microlink::enrollment::ApprovalUrl) -> Followup {
        self.enrollment.followup(url)
    }
    pub fn registration(&mut self) -> Option<Registration> {
        self.registration.try_recv().ok()
    }
    pub fn reconnecting(&self) -> bool {
        self.reconnecting
    }
    fn drain_auxiliary(&mut self) -> bool {
        self.relays.close();
        let relay = self.dialer.close_and_wait(Duration::ZERO);
        if self.publisher.is_none() {
            self.publisher = self.publication.try_recv().ok();
        }
        let publication = self
            .publisher
            .as_mut()
            .is_none_or(|publisher| publisher.close_and_wait(Duration::ZERO));
        if publication {
            self.publisher = None;
        }
        relay && publication && self.owner.worker_finished()
    }
    /// Applies authenticated maps and polls bounded UDP traffic.
    /// Address availability does not establish peer connectivity.
    pub fn poll(&mut self) -> Result<Option<Ipv4Addr>, String> {
        if self.stopped {
            return Err("tailnet runtime stopped".into());
        }
        if let Err(error) = crate::clock::require_ready() {
            self.cancel();
            return Err(error.to_string());
        }
        let started = self.started;
        let clock = || Clock {
            unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|v| v.as_millis().min(i64::MAX as u128) as i64)
                .unwrap_or(0),
            monotonic_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        };
        let teardown = if self.owner.stop_reason().is_some() {
            self.drain_auxiliary()
        } else {
            false
        };
        let mut replacement = None;
        let setup = self.setup.clone();
        let progress =
            self.owner
                .reconnect_with(&mut self.retry, clock(), teardown, 128 * 1024, || {
                    let (sender, registration) = mpsc::sync_channel(1);
                    let (publish_sender, publication) = mpsc::sync_channel(1);
                    replacement = Some((registration, publication));
                    setup_worker(setup, sender, publish_sender)
                });
        if let Some((registration, publication)) = replacement {
            self.registration = registration;
            self.publication = publication;
            self.assigned = false;
        }
        match progress {
            Ok(ReconnectProgress::Active) => self.reconnecting = false,
            Ok(_) => {
                self.reconnecting = true;
                return Ok(None);
            }
            Err(error) => {
                self.cancel();
                return Err(error.to_string());
            }
        }
        let result = (|| {
            let plan = self.owner.run(clock, |network| {
                let now = clock().monotonic_ms;
                self.dialer.poll(network, &mut self.relays, now)?;
                self.relays.next_plan(network, now)
            })?;
            if let Some(plan) = plan {
                let started = self.started;
                self.dialer
                    .start_with(
                        plan,
                        self.node_key.clone(),
                        clock().monotonic_ms,
                        128 * 1024,
                        move |plan, key, cancelled| {
                            let deadline = Instant::now() + Duration::from_secs(30);
                            let addresses = femto_microlink::relay_worker::addresses(plan.node())?;
                            let mut last = femto_microlink::relay_pool::Error::Closed;
                            for address in addresses.into_iter().take(2) {
                                if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                                    break;
                                }
                                let remaining = deadline.saturating_duration_since(Instant::now());
                                if remaining.is_zero() {
                                    break;
                                }
                                match crate::relay_transport::connect(
                                    plan.clone(),
                                    address,
                                    key.clone(),
                                    remaining,
                                    cancelled.clone(),
                                    started.elapsed().as_millis().min(u64::MAX as u128) as u64,
                                ) {
                                    Ok(connection) => return Ok(connection),
                                    Err(error) => last = error,
                                }
                            }
                            Err(last)
                        },
                    )
                    .map_err(|error| {
                        femto_microlink::control_worker::Error::Network(error.into())
                    })?;
            }
            socket_network::poll_with_relays(
                &mut self.owner,
                &self.udp,
                &mut self.relays,
                clock(),
            )?;
            if self.publisher.is_none() {
                self.publisher = self.publication.try_recv().ok();
            }
            self.owner.run(clock, |network| {
                let now = clock().monotonic_ms;
                let assigned = network.local_address(now)?;
                if let Some(publisher) = &mut self.publisher {
                    if assigned.is_some() {
                        self.assigned = true;
                        let observed = network.observed_endpoints(now)?;
                        let local = self.local_address.endpoint(self.udp.local_addr()?.port());
                        let home = self.relays.home_region(network, now);
                        let snapshot = Snapshot::new(local, &observed)?.with_home_relay(home)?;
                        match publisher.poll(snapshot, now)? {
                            PublicationProgress::Rejected | PublicationProgress::Cancelled => {
                                return Err(io::Error::new(
                                    io::ErrorKind::PermissionDenied,
                                    "endpoint publication closed",
                                )
                                .into());
                            }
                            #[cfg(target_os = "espidf")]
                            PublicationProgress::Failed => {
                                log::warn!("Endpoint publication failed; retry pending")
                            }
                            #[cfg(target_os = "espidf")]
                            PublicationProgress::Published => {
                                log::info!("Endpoint snapshot published")
                            }
                            _ => {}
                        }
                    } else if self.assigned {
                        publisher.cancel();
                        return Err(io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "local tailnet authorization withdrawn",
                        )
                        .into());
                    }
                }
                Ok(assigned)
            })
        })();
        if result.is_err() {
            if self.owner.stop_reason().is_some() {
                self.reconnecting = true;
                return Ok(None);
            }
            self.cancel();
        }
        result.map_err(|e| e.to_string())
    }
    fn cancel(&mut self) {
        self.stopped = true;
        self.reconnecting = false;
        if let Some(publisher) = &self.publisher {
            publisher.cancel();
        }
        self.owner.close();
        self.udp.close();
        self.relays.close();
        self.dialer.cancel();
    }
    pub fn close_and_wait(&mut self, timeout: Duration) -> bool {
        self.cancel();
        let started = Instant::now();
        let control = self.owner.close_and_wait(timeout);
        let relay = self
            .dialer
            .close_and_wait(timeout.saturating_sub(started.elapsed()));
        if self.publisher.is_none() {
            self.publisher = self.publication.try_recv().ok();
        }
        let publication = self.publisher.as_mut().is_none_or(|publisher| {
            publisher.close_and_wait(timeout.saturating_sub(started.elapsed()))
        });
        control && relay && publication
    }
}
impl Drop for Runtime {
    fn drop(&mut self) {
        self.cancel();
    }
}

impl femto_microlink::application_tcp::Driver for Runtime {
    type Store = crate::identity::Store;
    fn run<R>(
        &mut self,
        operation: impl FnOnce(
            &mut Network<Self::Store>,
            u64,
        ) -> Result<R, femto_microlink::network::Error>,
    ) -> io::Result<R> {
        if let Err(error) = crate::clock::require_ready() {
            self.cancel();
            return Err(error);
        }
        let started = self.started;
        let clock = || Clock {
            unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|v| v.as_millis().min(i64::MAX as u128) as i64)
                .unwrap_or(0),
            monotonic_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        };
        self.owner
            .run(clock, |network| operation(network, clock().monotonic_ms))
            .map_err(|error| match error {
                femto_microlink::control_worker::Error::Network(
                    femto_microlink::network::Error::Io(error),
                ) => error,
                error => io::Error::new(io::ErrorKind::ConnectionAborted, error),
            })
    }
    fn pump(&mut self) -> io::Result<()> {
        self.poll()
            .map(|_| ())
            .map_err(|error| io::Error::new(io::ErrorKind::ConnectionAborted, error))
    }
}
