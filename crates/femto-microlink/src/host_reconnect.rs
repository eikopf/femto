//! Host control reconnection with joined relay and publication teardown.
use crate::{
    control_session::ControlSession,
    control_worker::{ControlledNetwork, Error, Reconnect, ReconnectProgress},
    coordinator::Clock,
    host_derp::Pool,
    host_endpoints::{Progress, Publisher, Snapshot},
    host_relay_worker::Dialer,
    host_tls::TlsStream,
    protocol::{
        CAPABILITY_VERSION,
        control::{HostInfo, MapRequest, RegisterRequest},
        identity::DeviceIdentity,
        key::{Machine, PublicKey},
    },
    timestamp::EpochStore,
};
use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::AtomicBool,
        mpsc::{self, Receiver},
    },
    time::Duration,
};
type Setup = dyn Fn(Arc<AtomicBool>) -> io::Result<(ControlSession<TlsStream>, MapRequest, Publisher)>
    + Send
    + Sync;

pub struct Controller {
    setup: Arc<Setup>,
    retry: Reconnect,
    publisher: Option<Publisher>,
    publication: Receiver<Publisher>,
}
impl Controller {
    pub fn new(
        host: String,
        address: SocketAddr,
        pinned_key: Option<PublicKey<Machine>>,
        identity: DeviceIdentity,
        hostinfo: HostInfo,
        publisher: Publisher,
    ) -> Self {
        Self::with_tls(
            host,
            address,
            pinned_key,
            identity,
            hostinfo,
            publisher,
            crate::host_tls::TlsConnector::mozilla(Duration::from_secs(60))
                .expect("valid fixed TLS timeout"),
        )
    }
    pub fn with_tls(
        host: String,
        address: SocketAddr,
        pinned_key: Option<PublicKey<Machine>>,
        identity: DeviceIdentity,
        hostinfo: HostInfo,
        publisher: Publisher,
        tls: crate::host_tls::TlsConnector,
    ) -> Self {
        let setup = move |cancel: Arc<AtomicBool>| {
            let key = match pinned_key {
                Some(key) => key,
                None => crate::host_bootstrap::fetch_control_key_with_tls(
                    &host,
                    address,
                    CAPABILITY_VERSION,
                    &tls.with_timeout(Duration::from_secs(10))?,
                    cancel.clone(),
                )?,
            };
            let control_tls = tls.with_timeout(Duration::from_secs(60))?;
            let mut control = crate::host_control::ControlSession::connect_with_tls(
                &host,
                address,
                identity.machine_key(),
                key,
                CAPABILITY_VERSION,
                control_tls,
                cancel,
            )?;
            let request = RegisterRequest::new(
                CAPABILITY_VERSION,
                identity.node_key().public(),
                identity.network_lock_key().public(),
                hostinfo.clone(),
            );
            if control.register(&request)? != crate::enrollment::Outcome::Authorized {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "coordinator did not authorize reconnect",
                ));
            }
            let map = MapRequest::new(
                CAPABILITY_VERSION,
                identity.node_key().public(),
                identity.disco_key().public(),
                hostinfo.clone(),
            )
            .streaming();
            let publisher = control.endpoint_publisher(identity.machine_key(), &map)?;
            Ok((control.into_inner(), map, publisher))
        };
        Self::with_setup(setup, publisher)
    }
    /// Uses caller-authenticated coordinator setup and an initial publisher.
    /// Each setup must register the same identity and return a fresh full-map
    /// request and publisher sharing that session's cancellation flag.
    pub fn with_setup(
        setup: impl Fn(
            Arc<AtomicBool>,
        ) -> io::Result<(ControlSession<TlsStream>, MapRequest, Publisher)>
        + Send
        + Sync
        + 'static,
        publisher: Publisher,
    ) -> Self {
        Self {
            setup: Arc::new(setup),
            retry: Reconnect::default(),
            publisher: Some(publisher),
            publication: mpsc::sync_channel(1).1,
        }
    }
    pub fn poll<S: EpochStore>(
        &mut self,
        owner: &mut ControlledNetwork<S>,
        relays: &mut Pool,
        dialer: &mut Dialer,
        clock: Clock,
    ) -> Result<bool, Error> {
        let teardown = if owner.stop_reason().is_some() {
            relays.close();
            let relay = dialer.close_and_wait(Duration::ZERO);
            if self.publisher.is_none() {
                self.publisher = self.publication.try_recv().ok();
            }
            let publication = self
                .publisher
                .as_mut()
                .is_none_or(|p| p.close_and_wait(Duration::ZERO));
            if publication {
                self.publisher = None;
            }
            relay && publication && owner.worker_finished()
        } else {
            false
        };
        let setup = self.setup.clone();
        let mut replacement = None;
        let progress =
            owner.reconnect_with(&mut self.retry, clock, teardown, 2 * 1024 * 1024, || {
                let (sender, receiver) = mpsc::sync_channel(1);
                replacement = Some(receiver);
                move |cancel| {
                    let (control, map, publisher) = setup(cancel)?;
                    sender
                        .try_send(publisher)
                        .map_err(|_| io::Error::other("publication receiver unavailable"))?;
                    Ok((control, map))
                }
            })?;
        if let Some(receiver) = replacement {
            self.publication = receiver;
        }
        if self.publisher.is_none() {
            self.publisher = self.publication.try_recv().ok();
        }
        Ok(progress == ReconnectProgress::Active)
    }
    pub fn publish(&mut self, snapshot: Snapshot, now_ms: u64) -> io::Result<Progress> {
        self.publisher
            .as_mut()
            .map_or(Ok(Progress::Idle), |publisher| {
                publisher.poll(snapshot, now_ms)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{control_worker::StopReason, network::Network, timestamp::TimestampClock};
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Instant,
    };
    struct Store;
    impl EpochStore for Store {
        type Error = ();
        fn load_epoch(&mut self) -> Result<Option<u64>, ()> {
            Ok(None)
        }
        fn reserve_epoch(&mut self, _: u64) -> Result<(), ()> {
            Ok(())
        }
    }
    fn finished(owner: &ControlledNetwork<Store>) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !owner.worker_finished() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn clock(monotonic_ms: u64) -> Clock {
        Clock {
            unix_ms: 1_735_689_600_000,
            monotonic_ms,
        }
    }
    fn fixture() -> (ControlledNetwork<Store>, Controller, Arc<AtomicUsize>) {
        let network = Network::new(
            &DeviceIdentity::generate().unwrap(),
            TimestampClock::open(Store, 100).unwrap(),
            4,
            2,
        )
        .unwrap();
        fixture_network(network)
    }
    fn fixture_network(
        network: Network<Store>,
    ) -> (ControlledNetwork<Store>, Controller, Arc<AtomicUsize>) {
        let owner = ControlledNetwork::start_with::<TlsStream>(
            network,
            Arc::new(AtomicBool::new(false)),
            2 * 1024 * 1024,
            |_| Err(io::ErrorKind::TimedOut.into()),
        )
        .unwrap();
        finished(&owner);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let controller = Controller {
            setup: Arc::new(move |_| {
                let attempt = count.fetch_add(1, Ordering::SeqCst);
                Err(if attempt == 0 {
                    io::ErrorKind::TimedOut
                } else {
                    io::ErrorKind::PermissionDenied
                }
                .into())
            }),
            retry: Reconnect::default(),
            publisher: None,
            publication: mpsc::sync_channel(1).1,
        };
        (owner, controller, calls)
    }
    #[test]
    fn blocked_and_late_publishers_prevent_replacement_until_joined() {
        for queued in [false, true] {
            let (mut owner, mut controller, calls) = fixture();
            let cancelled = Arc::new(AtomicBool::new(false));
            let (entered_tx, entered_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let release_rx = std::sync::Mutex::new(release_rx);
            let mut publisher = Publisher::new(
                cancelled.clone(),
                Arc::new(move |_| {
                    entered_tx.send(()).unwrap();
                    release_rx.lock().unwrap().recv().unwrap();
                    Err(io::ErrorKind::TimedOut.into())
                }),
            );
            assert_eq!(
                publisher
                    .poll(Snapshot::new(None, &[]).unwrap(), 0)
                    .unwrap(),
                Progress::Pending
            );
            entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            if queued {
                let (sender, receiver) = mpsc::sync_channel(1);
                sender.try_send(publisher).unwrap();
                controller.publication = receiver;
            } else {
                controller.publisher = Some(publisher);
            }
            let mut pool = Pool::default();
            let mut dialer = Dialer::default();
            let (returned_tx, returned_rx) = mpsc::channel();
            let polling = std::thread::spawn(move || {
                let before_deadline = controller.poll(&mut owner, &mut pool, &mut dialer, clock(0));
                let after_deadline =
                    controller.poll(&mut owner, &mut pool, &mut dialer, clock(1000));
                returned_tx
                    .send((
                        before_deadline,
                        after_deadline,
                        owner,
                        controller,
                        pool,
                        dialer,
                    ))
                    .unwrap();
            });
            // Release the deliberately stuck publisher even if polling regresses
            // into a blocking join, so a failure cannot strand the test process.
            let returned = returned_rx.recv_timeout(Duration::from_secs(2));
            release_tx.send(()).unwrap();
            polling.join().unwrap();
            let (before, after, mut owner, mut controller, mut pool, mut dialer) =
                returned.expect("reconnect poll blocked on publisher");
            assert!(!before.unwrap());
            assert!(!after.unwrap());
            assert!(cancelled.load(Ordering::Acquire));
            assert!(controller.publisher.is_some());
            assert!(
                owner.worker_finished(),
                "replacement control worker started before teardown"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            let deadline = Instant::now() + Duration::from_secs(2);
            while calls.load(Ordering::SeqCst) == 0 {
                controller
                    .poll(&mut owner, &mut pool, &mut dialer, clock(1001))
                    .unwrap();
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            finished(&owner);
            assert!(controller.publisher.is_none());
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                owner.stop_reason(),
                Some(StopReason::SetupFailed(io::ErrorKind::TimedOut))
            );
        }
    }

    #[test]
    fn pending_relay_dial_must_join_before_control_replacement() {
        let identity = DeviceIdentity::generate().unwrap();
        let peer = DeviceIdentity::generate().unwrap();
        let mut network =
            Network::new(&identity, TimestampClock::open(Store, 100).unwrap(), 4, 2).unwrap();
        let map = serde_json::json!({
            "Node":{"ID":1,"Key":identity.node_key().public().to_string(),"Addresses":["100.64.0.1/32"],"MachineAuthorized":true},
            "Peers":[{"ID":2,"Key":peer.node_key().public().to_string(),"Addresses":["100.64.0.2/32"],"HomeDERP":7,"MachineAuthorized":true}],
            "DERPMap":{"Regions":{"7":{"RegionID":7,"Nodes":[{"Name":"7a","RegionID":7,"HostName":"relay.example","IPv4":"none","IPv6":"none"}]}}},
            "PacketFilter":[]
        });
        network
            .apply_map(&serde_json::to_vec(&map).unwrap(), clock(0))
            .unwrap();
        let plan = network
            .relay_plan(peer.node_key().public(), 0)
            .unwrap()
            .unwrap();
        let mut dialer = Dialer::default();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        assert!(
            dialer
                .start_with(
                    plan,
                    identity.node_key().clone(),
                    0,
                    128 * 1024,
                    move |_, _, cancelled| {
                        entered_tx.send(cancelled).unwrap();
                        release_rx.recv().unwrap();
                        Err(crate::relay_pool::Error::Closed)
                    }
                )
                .unwrap()
        );
        let cancelled = entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (mut owner, mut controller, calls) = fixture_network(network);
        let mut pool = Pool::default();
        let (returned_tx, returned_rx) = mpsc::channel();
        let polling = std::thread::spawn(move || {
            let before = controller.poll(&mut owner, &mut pool, &mut dialer, clock(0));
            let after = controller.poll(&mut owner, &mut pool, &mut dialer, clock(1000));
            returned_tx
                .send((before, after, owner, controller, pool, dialer))
                .unwrap();
        });
        let returned = returned_rx.recv_timeout(Duration::from_secs(2));
        release_tx.send(()).unwrap();
        polling.join().unwrap();
        let (before, after, mut owner, mut controller, mut pool, mut dialer) =
            returned.expect("reconnect poll blocked on relay dial");
        assert!(!before.unwrap());
        assert!(!after.unwrap());
        assert!(cancelled.load(Ordering::Acquire));
        assert!(
            owner.worker_finished(),
            "replacement control worker started before teardown"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        let deadline = Instant::now() + Duration::from_secs(2);
        while calls.load(Ordering::SeqCst) == 0 {
            controller
                .poll(&mut owner, &mut pool, &mut dialer, clock(1001))
                .unwrap();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        finished(&owner);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(dialer.close_and_wait(Duration::ZERO));
        assert!(
            owner
                .run::<()>(
                    || clock(1001),
                    |_| panic!("failed replacement authorized access")
                )
                .is_err()
        );
    }

    #[test]
    fn host_controller_retries_transient_setup_then_stops_on_auth_failure() {
        let (mut owner, mut controller, calls) = fixture();
        let mut pool = Pool::default();
        let mut dialer = Dialer::default();
        for now in [0, 999] {
            assert!(
                !controller
                    .poll(&mut owner, &mut pool, &mut dialer, clock(now))
                    .unwrap()
            );
            assert_eq!(calls.load(Ordering::SeqCst), 0);
        }
        assert!(
            !controller
                .poll(&mut owner, &mut pool, &mut dialer, clock(1000))
                .unwrap()
        );
        finished(&owner);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for now in [1001, 3000] {
            assert!(
                !controller
                    .poll(&mut owner, &mut pool, &mut dialer, clock(now))
                    .unwrap()
            );
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
        assert!(
            !controller
                .poll(&mut owner, &mut pool, &mut dialer, clock(3001))
                .unwrap()
        );
        finished(&owner);
        assert!(matches!(
            controller.poll(&mut owner, &mut pool, &mut dialer, clock(3002)),
            Err(Error::Stopped(StopReason::SetupFailed(
                io::ErrorKind::PermissionDenied
            )))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(
            owner
                .run::<()>(
                    || clock(3002),
                    |_| panic!("failed registration authorized application access")
                )
                .is_err()
        );
    }
    #[test]
    fn host_controller_does_not_restart_explicitly_closed_owner() {
        let (mut owner, mut controller, calls) = fixture();
        let mut pool = Pool::default();
        let mut dialer = Dialer::default();
        assert!(
            !controller
                .poll(&mut owner, &mut pool, &mut dialer, clock(0))
                .unwrap()
        );
        owner.close();
        assert!(matches!(
            controller.poll(&mut owner, &mut pool, &mut dialer, clock(1000)),
            Err(Error::Stopped(StopReason::Cancelled))
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
