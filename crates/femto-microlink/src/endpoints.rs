//! Bounded endpoint publication on a separate authenticated control connection.
use crate::protocol::control::MapRequest;
use std::{
    collections::BTreeMap,
    io,
    net::{Ipv4Addr, SocketAddrV4},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

/// Platform-owned IPv4 hint shared with a runtime without taking its lock.
/// This value never grants peer or application authorization.
#[derive(Clone, Default)]
pub struct LocalAddress(Arc<AtomicU32>);
impl LocalAddress {
    /// Missing, disconnected, or unusable interface addresses withdraw the hint.
    pub fn set(&self, address: Option<Ipv4Addr>) {
        let address = address.filter(valid_ip).map(u32::from).unwrap_or(0);
        self.0.store(address, Ordering::Release);
    }
    pub fn endpoint(&self, bound_port: u16) -> Option<SocketAddrV4> {
        let ip = Ipv4Addr::from(self.0.load(Ordering::Acquire));
        (bound_port != 0 && valid_ip(&ip)).then(|| SocketAddrV4::new(ip, bound_port))
    }
}
fn valid_ip(ip: &Ipv4Addr) -> bool {
    !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast() && !ip.is_loopback()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot(Vec<(SocketAddrV4, u8)>, Option<u64>);
impl Snapshot {
    pub fn new(local: Option<SocketAddrV4>, observed: &[SocketAddrV4]) -> io::Result<Self> {
        if observed.len() > 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "endpoint capacity",
            ));
        }
        let mut endpoints = BTreeMap::new();
        for (address, kind) in local
            .into_iter()
            .map(|a| (a, 1))
            .chain(observed.iter().copied().map(|a| (a, 2)))
        {
            let ip = address.ip();
            if address.port() == 0 || !valid_ip(ip) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid published endpoint",
                ));
            }
            endpoints.insert(address, kind);
        }
        Ok(Self(endpoints.into_iter().collect(), None))
    }
    /// Supply only a currently established, authorized home relay region.
    /// None explicitly withdraws the previous home advertisement.
    pub fn with_home_relay(mut self, region: Option<u64>) -> io::Result<Self> {
        if region.is_some_and(|id| id == 0 || id > crate::relay::MAX_REGION_ID) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "home relay region",
            ));
        }
        self.1 = region;
        Ok(self)
    }
    pub(crate) fn body(&self, template: &MapRequest) -> io::Result<Vec<u8>> {
        let mut request = template.clone();
        request.stream = false;
        request.keep_alive = false;
        request.omit_peers = true;
        request.map_session_handle.clear();
        request.map_session_seq = 0;
        let mut value = serde_json::to_value(request).map_err(io::Error::other)?;
        // Explicit empty arrays withdraw expired observations rather than omitting the update.
        value["Endpoints"] = serde_json::json!(
            self.0
                .iter()
                .map(|(address, _)| address.to_string())
                .collect::<Vec<_>>()
        );
        value["EndpointTypes"] =
            serde_json::json!(self.0.iter().map(|(_, kind)| *kind).collect::<Vec<_>>());
        // The pinned protocol type narrows this field to u16. Preserve the
        // validated directory ID without truncation, including explicit withdrawal.
        if !value["Hostinfo"]["NetInfo"].is_object() {
            value["Hostinfo"]["NetInfo"] = serde_json::json!({});
        }
        value["Hostinfo"]["NetInfo"]["PreferredDERP"] = serde_json::json!(self.1.unwrap_or(0));
        serde_json::to_vec(&value).map_err(io::Error::other)
    }
}

type SendUpdate = Arc<dyn Fn(&Snapshot) -> io::Result<()> + Send + Sync>;
struct Job {
    snapshot: Snapshot,
    handle: JoinHandle<io::Result<()>>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Progress {
    Idle,
    Pending,
    Published,
    Failed,
    Rejected,
    Cancelled,
}

/// Dropping the publisher cancels its shared control session and in-flight I/O.
pub struct Publisher {
    send: SendUpdate,
    stack_bytes: usize,
    cancelled: Arc<AtomicBool>,
    job: Option<Job>,
    published: Option<Snapshot>,
    next_ms: u64,
    last_ms: u64,
}
impl Publisher {
    #[cfg(test)]
    pub(crate) fn new(cancelled: Arc<AtomicBool>, send: SendUpdate) -> Self {
        Self::with_sender(cancelled, 2 * 1024 * 1024, send).expect("valid host endpoint stack")
    }
    pub(crate) fn with_sender(
        cancelled: Arc<AtomicBool>,
        stack_bytes: usize,
        send: SendUpdate,
    ) -> io::Result<Self> {
        if !(64 * 1024..=2 * 1024 * 1024).contains(&stack_bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "endpoint worker stack",
            ));
        }
        Ok(Self {
            send,
            stack_bytes,
            cancelled,
            job: None,
            published: None,
            next_ms: 0,
            last_ms: 0,
        })
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    /// A timeout retains the worker. The caller must retain the publisher too.
    pub fn close_and_wait(&mut self, timeout: Duration) -> bool {
        self.cancel();
        let started = Instant::now();
        while self
            .job
            .as_ref()
            .is_some_and(|job| !job.handle.is_finished())
        {
            if started.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        if let Some(job) = self.job.take() {
            let _ = job.handle.join();
        }
        true
    }
    /// One job at a time; snapshots coalesce while the prior request completes.
    pub fn poll(&mut self, snapshot: Snapshot, now_ms: u64) -> io::Result<Progress> {
        if now_ms < self.last_ms {
            self.cancelled.store(true, Ordering::Release);
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "endpoint clock moved backwards",
            ));
        }
        self.last_ms = now_ms;
        if self.cancelled.load(Ordering::Acquire) {
            return Ok(Progress::Cancelled);
        }
        if let Some(job) = &self.job {
            if !job.handle.is_finished() {
                return Ok(Progress::Pending);
            }
            let job = self.job.take().unwrap();
            self.next_ms = now_ms.saturating_add(5000);
            return match job.handle.join() {
                Ok(Ok(())) => {
                    self.published = Some(job.snapshot);
                    Ok(Progress::Published)
                }
                Ok(Err(error)) if error.kind() == io::ErrorKind::PermissionDenied => {
                    self.cancelled.store(true, Ordering::Release);
                    Ok(Progress::Rejected)
                }
                _ => Ok(Progress::Failed),
            };
        }
        if self.published.as_ref() == Some(&snapshot) || now_ms < self.next_ms {
            return Ok(Progress::Idle);
        }
        let send = self.send.clone();
        let cancel = self.cancelled.clone();
        let sent = snapshot.clone();
        let handle = std::thread::Builder::new()
            .name("femto-endpoints".into())
            .stack_size(self.stack_bytes)
            .spawn(move || {
                if cancel.load(Ordering::Acquire) {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "endpoint publication cancelled",
                    ));
                }
                send(&sent)
            })?;
        self.job = Some(Job { snapshot, handle });
        Ok(Progress::Pending)
    }
}
impl Drop for Publisher {
    fn drop(&mut self) {
        self.cancel();
        if let Some(job) = self.job.take() {
            let _ = job.handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{control::HostInfo, identity::DeviceIdentity};
    use std::sync::{Mutex, mpsc};
    fn snapshot(last: u8) -> Snapshot {
        Snapshot::new(None, &[SocketAddrV4::new([192, 0, 2, last].into(), 41641)]).unwrap()
    }
    fn completed(publisher: &mut Publisher, snapshot: Snapshot, now: u64) -> Progress {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let status = publisher.poll(snapshot.clone(), now).unwrap();
            if status != Progress::Pending {
                return status;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    #[test]
    fn local_address_updates_share_the_bound_udp_port_and_withdraw_invalid_hints() {
        let local = LocalAddress::default();
        let runtime = local.clone();
        let socket = crate::socket_udp::UdpTransport::bind("0.0.0.0:0".parse().unwrap()).unwrap();
        let port = socket.local_addr().unwrap().port();
        assert_ne!(port, 0);
        assert_eq!(runtime.endpoint(port), None);
        local.set(Some("192.168.44.9".parse().unwrap()));
        assert_eq!(
            runtime.endpoint(port),
            Some(SocketAddrV4::new([192, 168, 44, 9].into(), port))
        );
        assert_eq!(runtime.endpoint(0), None);
        let identity = DeviceIdentity::generate().unwrap();
        let template = MapRequest::new(
            100,
            identity.node_key().public(),
            identity.disco_key().public(),
            HostInfo::esp32("test", "test"),
        );
        let original = Snapshot::new(runtime.endpoint(port), &[]).unwrap();
        let wire: serde_json::Value =
            serde_json::from_slice(&original.body(&template).unwrap()).unwrap();
        assert_eq!(
            wire["Endpoints"],
            serde_json::json!([format!("192.168.44.9:{port}")])
        );
        assert_eq!(wire["EndpointTypes"], serde_json::json!([1]));
        local.set(Some("10.20.30.40".parse().unwrap()));
        assert_ne!(
            Snapshot::new(runtime.endpoint(port), &[]).unwrap(),
            original
        );
        for address in [
            None,
            Some(Ipv4Addr::UNSPECIFIED),
            Some(Ipv4Addr::LOCALHOST),
            Some(Ipv4Addr::BROADCAST),
            Some(Ipv4Addr::new(224, 0, 0, 1)),
        ] {
            local.set(Some("192.168.1.2".parse().unwrap()));
            local.set(address);
            assert_eq!(runtime.endpoint(port), None);
            let wire: serde_json::Value = serde_json::from_slice(
                &Snapshot::new(runtime.endpoint(port), &[])
                    .unwrap()
                    .body(&template)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(wire["Endpoints"], serde_json::json!([]));
            assert_eq!(wire["EndpointTypes"], serde_json::json!([]));
        }
    }

    #[test]
    fn timed_shutdown_retains_and_joins_the_in_flight_worker() {
        let (ready, started) = mpsc::channel();
        let (release, held) = mpsc::channel();
        let held = Mutex::new(held);
        let done = Arc::new(AtomicBool::new(false));
        let completed = done.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut publisher = Publisher::with_sender(
            cancelled.clone(),
            128 * 1024,
            Arc::new(move |_| {
                ready.send(()).unwrap();
                held.lock().unwrap().recv().unwrap();
                completed.store(true, Ordering::Release);
                Ok(())
            }),
        )
        .unwrap();
        publisher.poll(snapshot(1), 0).unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!publisher.close_and_wait(Duration::ZERO));
        assert!(cancelled.load(Ordering::Acquire));
        assert!(publisher.job.is_some());
        assert_eq!(
            publisher.poll(snapshot(2), 5000).unwrap(),
            Progress::Cancelled
        );
        assert!(!done.load(Ordering::Acquire));
        release.send(()).unwrap();
        assert!(publisher.close_and_wait(Duration::from_secs(2)));
        assert!(done.load(Ordering::Acquire));
        assert!(publisher.job.is_none());
    }

    #[test]
    fn drop_cancels_and_joins_and_stack_sizes_are_bounded() {
        for stack in [0, 65535, 2 * 1024 * 1024 + 1] {
            assert!(
                Publisher::with_sender(
                    Arc::new(AtomicBool::new(false)),
                    stack,
                    Arc::new(|_| Ok(()))
                )
                .is_err()
            );
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancelled.clone();
        let done = Arc::new(AtomicBool::new(false));
        let completed = done.clone();
        let (ready, started) = mpsc::channel();
        let mut publisher = Publisher::new(
            cancelled,
            Arc::new(move |_| {
                ready.send(()).unwrap();
                while !worker_cancel.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                completed.store(true, Ordering::Release);
                Ok(())
            }),
        );
        publisher.poll(snapshot(1), 0).unwrap();
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(publisher);
        assert!(done.load(Ordering::Acquire));
    }

    #[test]
    fn home_advertisement_preserves_large_ids_and_explicitly_withdraws() {
        let identity = DeviceIdentity::generate().unwrap();
        let template = MapRequest::new(
            100,
            identity.node_key().public(),
            identity.disco_key().public(),
            HostInfo::esp32("femto-test", "test").with_preferred_derp(7),
        );
        let empty = Snapshot::new(None, &[]).unwrap();
        for region in [
            None,
            Some(7),
            Some(70000),
            Some(crate::relay::MAX_REGION_ID),
        ] {
            let snapshot = empty.clone().with_home_relay(region).unwrap();
            let value: serde_json::Value =
                serde_json::from_slice(&snapshot.body(&template).unwrap()).unwrap();
            assert_eq!(
                value["Hostinfo"]["NetInfo"]["PreferredDERP"],
                region.unwrap_or(0)
            );
            assert_eq!(value["Hostinfo"]["Hostname"], "femto-test");
            assert_eq!(value["Endpoints"], serde_json::json!([]));
            assert_eq!(snapshot == empty, region.is_none());
        }
        for region in [0, crate::relay::MAX_REGION_ID + 1, u64::MAX] {
            assert!(empty.clone().with_home_relay(Some(region)).is_err());
        }
    }
    #[test]
    fn home_changes_publish_even_when_endpoints_do_not_change() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let calls = sent.clone();
        let mut publisher = Publisher::new(
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |snapshot| {
                calls.lock().unwrap().push(snapshot.clone());
                Ok(())
            }),
        );
        let empty = Snapshot::new(None, &[]).unwrap();
        let home = empty.clone().with_home_relay(Some(70000)).unwrap();
        assert_eq!(publisher.poll(home.clone(), 0).unwrap(), Progress::Pending);
        assert_eq!(
            completed(&mut publisher, home.clone(), 1),
            Progress::Published
        );
        assert_eq!(publisher.poll(empty.clone(), 5000).unwrap(), Progress::Idle);
        assert_eq!(
            publisher.poll(empty.clone(), 5001).unwrap(),
            Progress::Pending
        );
        assert_eq!(
            completed(&mut publisher, empty.clone(), 5002),
            Progress::Published
        );
        assert_eq!(*sent.lock().unwrap(), [home, empty]);
    }
    #[test]
    fn endpoint_wire_request_normalizes_sources_and_explicitly_withdraws() {
        let identity = DeviceIdentity::generate().unwrap();
        let template = MapRequest::new(
            100,
            identity.node_key().public(),
            identity.disco_key().public(),
            HostInfo::esp32("femto-test", "test"),
        )
        .streaming();
        let address = "192.0.2.1:41641".parse().unwrap();
        let snapshot = Snapshot::new(Some(address), &[address, address]).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&snapshot.body(&template).unwrap()).unwrap();
        assert_eq!(value["Endpoints"], serde_json::json!(["192.0.2.1:41641"]));
        assert_eq!(value["EndpointTypes"], serde_json::json!([2]));
        assert_eq!(value["Stream"], false);
        assert_eq!(value["KeepAlive"], false);
        assert_eq!(value["OmitPeers"], true);
        let cleared: serde_json::Value =
            serde_json::from_slice(&Snapshot::new(None, &[]).unwrap().body(&template).unwrap())
                .unwrap();
        assert_eq!(cleared["Endpoints"], serde_json::json!([]));
        assert_eq!(cleared["EndpointTypes"], serde_json::json!([]));
        assert!(template.stream);
        for bad in [
            "0.0.0.0:1",
            "127.0.0.1:1",
            "224.0.0.1:1",
            "255.255.255.255:1",
            "192.0.2.1:0",
        ] {
            assert!(Snapshot::new(Some(bad.parse().unwrap()), &[]).is_err());
        }
        assert!(Snapshot::new(None, &[address; 9]).is_err());
    }
    #[test]
    fn one_worker_coalesces_changes_and_paces_retries() {
        let (release, receiver) = mpsc::channel();
        let receiver = Mutex::new(receiver);
        let sent = Arc::new(Mutex::new(Vec::new()));
        let calls = sent.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut publisher = Publisher::new(
            cancel,
            Arc::new(move |snapshot| {
                calls.lock().unwrap().push(snapshot.clone());
                receiver.lock().unwrap().recv().unwrap()
            }),
        );
        assert_eq!(publisher.poll(snapshot(1), 0).unwrap(), Progress::Pending);
        assert_eq!(publisher.poll(snapshot(2), 1).unwrap(), Progress::Pending);
        release
            .send(Err(io::Error::other("temporary failure")))
            .unwrap();
        assert_eq!(completed(&mut publisher, snapshot(2), 2), Progress::Failed);
        assert_eq!(publisher.poll(snapshot(2), 5001).unwrap(), Progress::Idle);
        assert_eq!(
            publisher.poll(snapshot(2), 5002).unwrap(),
            Progress::Pending
        );
        release.send(Ok(())).unwrap();
        assert_eq!(
            completed(&mut publisher, snapshot(2), 5003),
            Progress::Published
        );
        assert_eq!(publisher.poll(snapshot(2), 10003).unwrap(), Progress::Idle);
        assert_eq!(*sent.lock().unwrap(), [snapshot(1), snapshot(2)]);
    }
    #[test]
    fn authorization_rejection_cancels_the_shared_session() {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut publisher = Publisher::new(
            cancel.clone(),
            Arc::new(|_| Err(io::Error::new(io::ErrorKind::PermissionDenied, "rejected"))),
        );
        assert_eq!(publisher.poll(snapshot(1), 0).unwrap(), Progress::Pending);
        assert_eq!(
            completed(&mut publisher, snapshot(1), 1),
            Progress::Rejected
        );
        assert!(cancel.load(Ordering::Acquire));
        assert_eq!(
            publisher.poll(snapshot(2), 5001).unwrap(),
            Progress::Cancelled
        );
    }
    #[test]
    fn expired_observations_publish_an_empty_replacement_and_drop_cancels() {
        let cancel = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let sent = calls.clone();
        let mut publisher = Publisher::new(
            cancel.clone(),
            Arc::new(move |snapshot| {
                sent.lock().unwrap().push(snapshot.clone());
                Ok(())
            }),
        );
        publisher.poll(snapshot(1), 0).unwrap();
        assert_eq!(
            completed(&mut publisher, snapshot(1), 1),
            Progress::Published
        );
        let empty = Snapshot::new(None, &[]).unwrap();
        publisher.poll(empty.clone(), 5001).unwrap();
        assert_eq!(
            completed(&mut publisher, empty.clone(), 5002),
            Progress::Published
        );
        assert_eq!(*calls.lock().unwrap(), [snapshot(1), empty]);
        drop(publisher);
        assert!(cancel.load(Ordering::Acquire));
    }
    #[test]
    fn cancellation_and_clock_regression_prevent_publication() {
        let cancel = Arc::new(AtomicBool::new(true));
        let mut publisher = Publisher::new(cancel.clone(), Arc::new(|_| panic!("must not send")));
        assert_eq!(
            publisher.poll(snapshot(1), 10).unwrap(),
            Progress::Cancelled
        );
        cancel.store(false, Ordering::Release);
        assert!(publisher.poll(snapshot(1), 9).is_err());
        assert!(cancel.load(Ordering::Acquire));
    }
}
