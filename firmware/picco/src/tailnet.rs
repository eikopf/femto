//! Device enrollment and authenticated map-worker ownership.
use femto_microlink::{
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
/// Set from the USB console: sessions opened afterwards exchange no direct
/// datagrams, which leaves only relayed paths to test.
pub static RELAY_ONLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Internal-storage name of the home relay region last connected, followed
/// by the name of the Wi-Fi network it was connected from.
#[cfg(target_os = "espidf")]
const HOME_RELAY: &str = "home_relay";
/// The home relay last connected from the network the reader is on. Which
/// relay is nearest depends on where the reader reaches the internet, so on
/// another network the session measures again.
fn stored_home() -> Option<u64> {
    #[cfg(target_os = "espidf")]
    {
        let stored = crate::nvs::get(HOME_RELAY).ok()??;
        let (region, network) = stored.split_first_chunk()?;
        (network == crate::wifi::last_network()?.as_bytes()).then(|| u64::from_le_bytes(*region))
    }
    #[cfg(not(target_os = "espidf"))]
    None
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
            let (endpoint, mut control) = crate::control::open_session(
                &config.host,
                config.port,
                CAPABILITY_VERSION,
                identity.machine_key(),
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
                let session = crate::control::connect_session(
                    &publication_endpoint,
                    &machine,
                    Duration::from_secs(15),
                    cancel,
                );
                #[cfg(target_os = "espidf")]
                if let Err(error) = &session {
                    log::warn!("Endpoint publication connection: {error}");
                }
                session
            })?;
            publish_sender
                .try_send(publisher)
                .map_err(|_| io::Error::other("endpoint publisher receiver unavailable"))?;
            Ok((control, request))
        })();
        #[cfg(target_os = "espidf")]
        if let Err(error) = &result {
            log::warn!("Tailnet setup: {error}");
        }
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
    /// When the coordinator assigned the local address.
    assigned: Option<u64>,
    setup: Arc<Setup>,
    retry: Reconnect,
    reconnecting: bool,
    stopped: bool,
    next_diagnostics_ms: u64,
    /// The home relay region last logged.
    #[cfg(target_os = "espidf")]
    home: Option<u64>,
    /// The peer paths last logged.
    #[cfg(target_os = "espidf")]
    paths: Vec<(
        femto_microlink::protocol::key::PublicKey<femto_microlink::protocol::key::Node>,
        Option<femto_microlink::discovery::Route>,
        bool,
    )>,
    /// The last poll moved no packet on an established session.
    quiet: bool,
    /// Endpoints have been published since the address was assigned.
    announced: bool,
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
        let mut network = Network::new(&identity, timestamps, 64, 4).map_err(|e| e.to_string())?;
        network.set_previous_home(stored_home());
        let mut udp =
            UdpTransport::bind("0.0.0.0:0".parse().unwrap()).map_err(|e| e.to_string())?;
        if RELAY_ONLY.load(std::sync::atomic::Ordering::Relaxed) {
            udp.mute();
        }
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
            assigned: None,
            setup,
            retry: Reconnect::default(),
            reconnecting: false,
            stopped: false,
            next_diagnostics_ms: 30_000,
            #[cfg(target_os = "espidf")]
            home: None,
            #[cfg(target_os = "espidf")]
            paths: Vec::new(),
            quiet: false,
            announced: false,
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
    /// Whether the last poll found an established session with nothing to
    /// do, so that the next one can wait.
    pub fn quiet(&self) -> bool {
        self.quiet && !self.reconnecting
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
            self.assigned = None;
            self.announced = false;
        }
        match progress {
            Ok(ReconnectProgress::Active) => self.reconnecting = false,
            Ok(_) => {
                self.reconnecting = true;
                return Ok(None);
            }
            Err(error) => {
                let detail = self.owner.rejection().unwrap_or_default().to_owned();
                self.cancel();
                return Err(format!("{error} {detail}"));
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
            let stats = socket_network::poll_with_relays(
                &mut self.owner,
                &self.udp,
                &mut self.relays,
                clock(),
            )?;
            self.quiet = self.assigned.is_some()
                && stats.received + stats.sent + stats.relay_events + stats.stun_sent == 0;
            if self.publisher.is_none() {
                self.publisher = self.publication.try_recv().ok();
            }
            self.owner.run(clock, |network| {
                let now = clock().monotonic_ms;
                let assigned = network.local_address(now)?;
                #[cfg(target_os = "espidf")]
                for trace in network.take_trace() {
                    log::info!("Tailnet packet: {trace:?}");
                }
                #[cfg(target_os = "espidf")]
                {
                    let paths = network.paths(now)?;
                    if paths != self.paths {
                        for (peer, route, session) in &paths {
                            log::info!(
                                "Tailnet peer {:02x}{:02x}: {route:?}, session {session}",
                                peer.as_bytes()[0],
                                peer.as_bytes()[1],
                            );
                        }
                        self.paths = paths;
                    }
                }
                if now >= self.next_diagnostics_ms {
                    self.next_diagnostics_ms = now.saturating_add(30_000);
                    #[cfg(target_os = "espidf")]
                    log::info!("Tailnet diagnostics: {:?}", network.diagnostics());
                }
                if let Some(publisher) = &mut self.publisher {
                    if assigned.is_some() {
                        self.assigned.get_or_insert(now);
                        let observed = network.observed_endpoints(now)?;
                        let local = self.local_address.endpoint(self.udp.local_addr()?.port());
                        let home = self.relays.home_region(network, now);
                        #[cfg(target_os = "espidf")]
                        if home != self.home {
                            log::info!("Tailnet home relay: {home:?}");
                            self.home = home;
                            if let Some(region) = home
                                && stored_home() != home
                                && let Some(network) = crate::wifi::last_network()
                            {
                                // Unstored, the next session measures again.
                                let mut stored = region.to_le_bytes().to_vec();
                                stored.extend_from_slice(network.as_bytes());
                                let _ = crate::nvs::set(HOME_RELAY, &stored);
                            }
                        }
                        let snapshot = Snapshot::new(local, &observed)?.with_home_relay(home)?;
                        // Each publication costs a control connection, and
                        // peers answer through the home relay, so the first
                        // one waits for it unless none is coming.
                        let progress = if home.is_none()
                            && !self.announced
                            && self.relays.home_expected(network, now)?
                        {
                            PublicationProgress::Idle
                        } else {
                            publisher.poll(snapshot, now)?
                        };
                        match progress {
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
                            PublicationProgress::Published => {
                                self.announced = true;
                                #[cfg(target_os = "espidf")]
                                log::info!("Endpoint snapshot published")
                            }
                            _ => {}
                        }
                    } else if self.assigned.is_some() {
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
