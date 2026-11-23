//! Single background relay dial with cancellation and bounded retry pacing.
use crate::{
    network::{Network, RelayPlan},
    protocol::key::{Node, PrivateKey},
    relay::{Address, RelayNode},
    relay_pool::{AuthorizedConnection, Pool, Transport},
    timestamp::EpochStore,
};
use std::{
    io,
    net::{SocketAddr, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

struct Job<T: Transport> {
    plan: RelayPlan,
    cancelled: Arc<AtomicBool>,
    handle: JoinHandle<Result<AuthorizedConnection<T>, crate::relay_pool::Error>>,
}

pub struct Dialer<T: Transport> {
    job: Option<Job<T>>,
    next_ms: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    Idle,
    Pending,
    Connected,
    Failed,
    Stale,
}
impl<T: Transport> Default for Dialer<T> {
    fn default() -> Self {
        Self {
            job: None,
            next_ms: 0,
        }
    }
}
impl<T: Transport + Send + 'static> Dialer<T> {
    /// Start only with a plan obtained inside the current controlled operation.
    /// DNS and connection setup run outside the network owner's lock.
    pub fn start_with(
        &mut self,
        plan: RelayPlan,
        key: PrivateKey<Node>,
        now_ms: u64,
        stack_bytes: usize,
        setup: impl FnOnce(
            RelayPlan,
            PrivateKey<Node>,
            Arc<AtomicBool>,
        ) -> Result<AuthorizedConnection<T>, crate::relay_pool::Error>
        + Send
        + 'static,
    ) -> io::Result<bool> {
        if self.job.is_some() || now_ms < self.next_ms {
            return Ok(false);
        }
        // Deriving the public key is a scalar multiplication, about 50 ms on
        // the reader; callers offer a plan on every poll, so it is done only
        // when a setup would start.
        if key.public() != plan.local_node() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "relay identity mismatch",
            ));
        }
        if !(64 * 1024..=2 * 1024 * 1024).contains(&stack_bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "relay worker stack size",
            ));
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancelled.clone();
        let worker_plan = plan.clone();
        let handle = thread::Builder::new()
            .name("femto-relay-dial".into())
            .stack_size(stack_bytes)
            .spawn(move || setup(worker_plan, key, worker_cancel))?;
        self.job = Some(Job {
            plan,
            cancelled,
            handle,
        });
        self.next_ms = now_ms.saturating_add(5000);
        Ok(true)
    }

    /// Call inside ControlledNetwork::run before polling the relay pool.
    pub fn poll<S: EpochStore>(
        &mut self,
        network: &mut Network<S>,
        pool: &mut Pool<T>,
        now_ms: u64,
    ) -> Result<Progress, crate::network::Error> {
        network.tick(now_ms)?;
        let Some(job) = &self.job else {
            return Ok(Progress::Idle);
        };
        let valid = network.validate_relay_plan(&job.plan, now_ms)?;
        if !valid {
            job.cancelled.store(true, Ordering::Release);
        }
        // A cancelled DNS lookup retains the slot until it exits. Replacement
        // jobs cannot accumulate threads while the system resolver is stalled.
        if !job.handle.is_finished() {
            return Ok(Progress::Pending);
        }
        let job = self.job.take().expect("finished job");
        let cancelled = job.cancelled.load(Ordering::Acquire);
        let result = job.handle.join().ok();
        self.next_ms = now_ms.saturating_add(5000);
        if !valid || cancelled {
            return Ok(Progress::Stale);
        }
        let progress = match result {
            Some(Ok(connection)) => {
                if pool.insert(connection, network, now_ms).is_ok() {
                    Progress::Connected
                } else {
                    Progress::Failed
                }
            }
            _ => Progress::Failed,
        };
        match progress {
            // A relay that connected shows the network works, so the next
            // region starts at once; the pause paces attempts that fail.
            Progress::Connected => self.next_ms = now_ms,
            Progress::Failed => pool.setup_failed(job.plan, now_ms),
            _ => (),
        }
        Ok(progress)
    }
}
impl<T: Transport> Dialer<T> {
    pub fn cancel(&mut self) {
        if let Some(job) = &self.job {
            job.cancelled.store(true, Ordering::Release);
        }
    }
    /// Retains a blocked worker on timeout. The caller must keep its platform
    /// services alive until this returns true or the destructor has joined.
    pub fn close_and_wait(&mut self, timeout: Duration) -> bool {
        self.cancel();
        let started = std::time::Instant::now();
        while self
            .job
            .as_ref()
            .is_some_and(|job| !job.handle.is_finished())
        {
            if started.elapsed() >= timeout {
                return false;
            }
            thread::sleep(Duration::from_millis(5).min(timeout.saturating_sub(started.elapsed())));
        }
        if let Some(job) = self.job.take() {
            let _ = job.handle.join();
        }
        true
    }
}
impl<T: Transport> Drop for Dialer<T> {
    fn drop(&mut self) {
        self.cancel();
        if let Some(job) = self.job.take() {
            let _ = job.handle.join();
        }
    }
}

pub fn addresses(node: &RelayNode) -> io::Result<Vec<SocketAddr>> {
    let mut result = Vec::new();
    if let Address::Fixed(ip) = node.ipv4 {
        result.push(SocketAddr::new(ip.into(), node.tls_port));
    }
    if let Address::Fixed(ip) = node.ipv6 {
        result.push(SocketAddr::new(ip.into(), node.tls_port));
    }
    // Fixed overrides avoid dependence on DNS, including a stalled resolver.
    if !result.is_empty() {
        return Ok(result);
    }
    if matches!(node.ipv4, Address::Dns) || matches!(node.ipv6, Address::Dns) {
        for address in (node.host_name.as_str(), node.tls_port).to_socket_addrs()? {
            if (address.is_ipv4() && matches!(node.ipv4, Address::Dns)
                || address.is_ipv6() && matches!(node.ipv6, Address::Dns))
                && !result.contains(&address)
            {
                result.push(address);
                if result.len() == 8 {
                    break;
                }
            }
        }
    }
    if result.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            "no enabled relay addresses",
        ));
    }
    Ok(result)
}

#[cfg(all(test, unix, not(target_os = "espidf")))]
mod tests {
    use super::*;
    type Dialer = super::Dialer<crate::host_tls::NonblockingTls>;
    type Pool = crate::host_derp::Pool;
    use crate::{
        coordinator::Clock, protocol::identity::DeviceIdentity, timestamp::TimestampClock,
    };
    use serde_json::json;
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
    fn setup() -> (DeviceIdentity, Network<Store>, RelayPlan) {
        let identity = DeviceIdentity::generate().unwrap();
        let peer = DeviceIdentity::generate().unwrap();
        let mut network =
            Network::new(&identity, TimestampClock::open(Store, 100).unwrap(), 4, 2).unwrap();
        let map = json!({"Node":{"ID":1,"Key":identity.node_key().public().to_string(),"Addresses":["100.64.0.1/32"],"MachineAuthorized":true},"Peers":[{"ID":2,"Key":peer.node_key().public().to_string(),"Addresses":["100.64.0.2/32"],"HomeDERP":7,"MachineAuthorized":true}],"DERPMap":{"Regions":{"7":{"RegionID":7,"Nodes":[{"Name":"7a","RegionID":7,"HostName":"relay.example","IPv4":"none","IPv6":"none"}]}}},"PacketFilter":[]});
        network
            .apply_map(
                &serde_json::to_vec(&map).unwrap(),
                Clock {
                    unix_ms: 1_735_689_600_000,
                    monotonic_ms: 0,
                },
            )
            .unwrap();
        let plan = network
            .relay_plan(peer.node_key().public(), 0)
            .unwrap()
            .unwrap();
        (identity, network, plan)
    }
    fn wait_finished(dialer: &Dialer) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !dialer.job.as_ref().unwrap().handle.is_finished() {
            assert!(std::time::Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn shutdown_timeout_retains_job_and_drop_joins_cancelled_setup() {
        let (identity, _, plan) = setup();
        let (release, wait) = std::sync::mpsc::sync_channel(1);
        let mut dialer = Dialer::default();
        assert!(
            dialer
                .start_with(
                    plan.clone(),
                    identity.node_key().clone(),
                    0,
                    128 * 1024,
                    move |_, _, _| {
                        wait.recv().unwrap();
                        Err(crate::relay_pool::Error::Closed)
                    }
                )
                .unwrap()
        );
        assert!(!dialer.close_and_wait(Duration::ZERO));
        assert!(dialer.job.is_some());
        assert!(
            dialer
                .job
                .as_ref()
                .unwrap()
                .cancelled
                .load(Ordering::Acquire)
        );
        release.send(()).unwrap();
        assert!(dialer.close_and_wait(Duration::from_secs(2)));
        assert!(dialer.job.is_none());

        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = finished.clone();
        assert!(
            dialer
                .start_with(
                    plan,
                    identity.node_key().clone(),
                    5000,
                    128 * 1024,
                    move |_, _, cancel| {
                        while !cancel.load(Ordering::Acquire) {
                            thread::sleep(Duration::from_millis(1));
                        }
                        worker_finished.store(true, Ordering::Release);
                        Err(crate::relay_pool::Error::Closed)
                    }
                )
                .unwrap()
        );
        drop(dialer);
        assert!(finished.load(Ordering::Acquire));
    }
    #[test]
    fn stale_job_keeps_slot_until_exit_and_paces_retry() {
        let (identity, mut network, plan) = setup();
        let cancelled = Arc::new(AtomicBool::new(false));
        let (release, wait) = std::sync::mpsc::sync_channel(1);
        let mut dialer = Dialer {
            job: Some(Job {
                plan: plan.clone(),
                cancelled: cancelled.clone(),
                handle: thread::spawn(move || {
                    wait.recv().unwrap();
                    Err(crate::host_derp::Error::Closed)
                }),
            }),
            next_ms: 0,
        };
        let mut pool = Pool::default();
        assert_eq!(
            dialer.poll(&mut network, &mut pool, 0).unwrap(),
            Progress::Pending
        );
        assert!(!cancelled.load(Ordering::Acquire));
        network.disconnect();
        assert_eq!(
            dialer.poll(&mut network, &mut pool, 1).unwrap(),
            Progress::Pending
        );
        assert!(cancelled.load(Ordering::Acquire));
        assert!(
            !dialer
                .start(plan.clone(), identity.node_key().clone(), 1)
                .unwrap()
        );
        release.send(()).unwrap();
        wait_finished(&dialer);
        assert_eq!(
            dialer.poll(&mut network, &mut pool, 2).unwrap(),
            Progress::Stale
        );
        assert!(pool.is_empty());
        assert!(
            !dialer
                .start(plan, identity.node_key().clone(), 5001)
                .unwrap()
        );
    }
    #[test]
    fn completed_failure_cools_endpoint_but_cancelled_setup_does_not() {
        for cancel in [false, true] {
            let (identity, mut network, _) = setup();
            network.apply_map(br#"{"DERPMap":{"Regions":{"7":{"RegionID":7,"Nodes":[{"Name":"7a","RegionID":7,"HostName":"relay.example"}]}}}}"#, Clock {unix_ms:1_735_689_600_001, monotonic_ms:1}).unwrap();
            let mut pool = Pool::default();
            let mut dialer = Dialer::default();
            let plan = pool.next_plan(&mut network, 1).unwrap().unwrap();
            assert!(
                dialer
                    .start_with(
                        plan,
                        identity.node_key().clone(),
                        1,
                        128 * 1024,
                        |_, _, _| Err(crate::relay_pool::Error::Closed)
                    )
                    .unwrap()
            );
            if cancel {
                dialer.cancel();
            }
            wait_finished(&dialer);
            assert_eq!(
                dialer.poll(&mut network, &mut pool, 2).unwrap(),
                if cancel {
                    Progress::Stale
                } else {
                    Progress::Failed
                }
            );
            assert_eq!(
                pool.next_plan(&mut network, 5002).unwrap().is_some(),
                cancel
            );
            assert!(
                pool.next_plan(&mut network, 30_002)
                    .unwrap()
                    .unwrap()
                    .is_home()
            );
        }
    }
    #[test]
    fn failures_identity_mismatch_and_drop_cancellation() {
        let (identity, mut network, plan) = setup();
        let mut dialer = Dialer::default();
        let wrong = DeviceIdentity::generate().unwrap();
        assert!(
            dialer
                .start(plan.clone(), wrong.node_key().clone(), 0)
                .is_err()
        );
        assert!(dialer.job.is_none());
        let cancelled = Arc::new(AtomicBool::new(false));
        dialer.job = Some(Job {
            plan: plan.clone(),
            cancelled: cancelled.clone(),
            handle: thread::spawn(|| Err(crate::host_derp::Error::Closed)),
        });
        wait_finished(&dialer);
        assert_eq!(
            dialer.poll(&mut network, &mut Pool::default(), 0).unwrap(),
            Progress::Failed
        );
        assert!(
            !dialer
                .start(plan.clone(), identity.node_key().clone(), 4999)
                .unwrap()
        );
        dialer.job = Some(Job {
            plan,
            cancelled: cancelled.clone(),
            handle: thread::spawn(|| Err(crate::host_derp::Error::Closed)),
        });
        drop(dialer);
        assert!(cancelled.load(Ordering::Acquire));
    }
    #[test]
    fn real_worker_reaps_failed_setup_without_dialing_disabled_addresses() {
        let (identity, mut network, plan) = setup();
        let mut dialer = Dialer::default();
        assert!(dialer.start(plan, identity.node_key().clone(), 0).unwrap());
        wait_finished(&dialer);
        assert_eq!(
            dialer.poll(&mut network, &mut Pool::default(), 1).unwrap(),
            Progress::Failed
        );
        assert!(dialer.job.is_none());
    }
    #[test]
    fn fixed_addresses_do_not_resolve_hostname_and_disabled_families_fail() {
        let (_, _, plan) = setup();
        let mut node = plan.node().clone();
        node.host_name = "does-not-exist.invalid".into();
        node.ipv4 = Address::Fixed("192.0.2.7".parse().unwrap());
        assert_eq!(
            addresses(&node).unwrap(),
            vec!["192.0.2.7:443".parse::<SocketAddr>().unwrap()]
        );
        node.ipv6 = Address::Fixed("2001:db8::7".parse().unwrap());
        assert_eq!(addresses(&node).unwrap().len(), 2);
        node.ipv4 = Address::Disabled;
        node.ipv6 = Address::Disabled;
        assert_eq!(
            addresses(&node).unwrap_err().kind(),
            io::ErrorKind::AddrNotAvailable
        );
    }
}
