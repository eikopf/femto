//! Reconnecting host application driver with UDP, DERP and endpoint publication.
use crate::{
    coordinator::Clock,
    host_control_worker::ControlledNetwork,
    host_reconnect as reconnect,
    host_tcp::{Driver, NetworkDriver},
    host_udp::UdpTransport,
    network::Network,
    protocol::key::{Node, PrivateKey},
    store::HostStore,
};
use std::{
    io,
    net::{SocketAddr, SocketAddrV4},
};

pub struct Runtime<C: FnMut() -> Clock> {
    base: NetworkDriver<HostStore, C>,
    dialer: crate::host_relay_worker::Dialer,
    reconnect: reconnect::Controller,
    node_key: PrivateKey<Node>,
    local: Option<SocketAddrV4>,
    transport: TransportConfig,
}
/// Relay trust is independent of coordinator and application trust.
pub struct TransportConfig {
    pub relay_tls: crate::host_tls::TlsConnector,
    pub direct_udp: bool,
}
impl TransportConfig {
    pub fn mozilla() -> io::Result<Self> {
        Ok(Self {
            relay_tls: crate::host_tls::TlsConnector::mozilla(std::time::Duration::from_secs(5))?,
            direct_udp: true,
        })
    }
}
fn control_error(error: crate::host_control_worker::Error) -> io::Error {
    match error {
        crate::host_control_worker::Error::Network(crate::network::Error::Io(error)) => error,
        error => io::Error::new(io::ErrorKind::ConnectionAborted, error),
    }
}
impl<C: FnMut() -> Clock> Driver for Runtime<C> {
    type Store = HostStore;
    fn run<R>(
        &mut self,
        operation: impl FnOnce(
            &mut Network<HostStore>,
            u64,
        ) -> std::result::Result<R, crate::network::Error>,
    ) -> io::Result<R> {
        self.base.run(operation)
    }
    fn pump(&mut self) -> io::Result<()> {
        if !self
            .reconnect
            .poll(
                &mut self.base.owner,
                &mut self.base.relays,
                &mut self.dialer,
                (self.base.clock)(),
            )
            .map_err(control_error)?
        {
            return Ok(());
        }
        let result = self.pump_active();
        if result.is_err() && self.base.owner.stop_reason().is_some() {
            return Ok(());
        }
        result
    }
}
impl<C: FnMut() -> Clock> Runtime<C> {
    /// Exposes terminal versus retryable control failure for application status.
    pub fn stop_reason(&self) -> Option<crate::control_worker::StopReason> {
        self.base.owner.stop_reason()
    }
    /// Cancels this runtime generation and revokes its network authorization.
    /// Retained application adapters cannot resume it. Final drop joins workers.
    pub fn close(&mut self) {
        self.dialer.cancel();
        self.base.relays.close();
        self.base.owner.close();
    }
    /// Takes ownership of an enrolled control stream, UDP socket and reconnect state.
    /// The supplied clock must use the same monotonic origin as enrollment.
    pub fn new(
        owner: ControlledNetwork<HostStore>,
        udp: UdpTransport,
        reconnect: reconnect::Controller,
        node_key: PrivateKey<Node>,
        clock: C,
    ) -> io::Result<Self> {
        Self::with_transport(
            owner,
            udp,
            reconnect,
            node_key,
            clock,
            TransportConfig::mozilla()?,
        )
    }
    /// Uses the ownership and clock contract of `new`, with transport settings
    /// fixed for this runtime generation.
    pub fn with_transport(
        owner: ControlledNetwork<HostStore>,
        udp: UdpTransport,
        reconnect: reconnect::Controller,
        node_key: PrivateKey<Node>,
        clock: C,
        transport: TransportConfig,
    ) -> io::Result<Self> {
        let local = match udp.local_addr()? {
            SocketAddr::V4(address)
                if transport.direct_udp
                    && !address.ip().is_unspecified()
                    && !address.ip().is_loopback() =>
            {
                Some(address)
            }
            _ => None,
        };
        Ok(Self {
            base: NetworkDriver {
                owner,
                udp,
                relays: Default::default(),
                clock,
            },
            dialer: Default::default(),
            reconnect,
            node_key,
            local,
            transport,
        })
    }

    fn pump_active(&mut self) -> io::Result<()> {
        let now = (self.base.clock)();
        self.base
            .owner
            .run(
                || now,
                |network| {
                    self.dialer
                        .poll(network, &mut self.base.relays, now.monotonic_ms)?;
                    if let Some(plan) = self.base.relays.next_plan(network, now.monotonic_ms)? {
                        self.dialer.start_with_tls(
                            plan,
                            self.node_key.clone(),
                            now.monotonic_ms,
                            self.transport.relay_tls.clone(),
                        )?;
                    }
                    Ok(())
                },
            )
            .map_err(control_error)?;
        crate::host_network::poll_with_optional_udp(
            &mut self.base.owner,
            self.transport.direct_udp.then_some(&self.base.udp),
            &mut self.base.relays,
            (self.base.clock)(),
        )
        .map_err(control_error)?;
        let now = (self.base.clock)();
        let (assigned, observed, home) = self
            .base
            .owner
            .run(
                || now,
                |network| {
                    Ok((
                        network.local_address(now.monotonic_ms)?,
                        network.observed_endpoints(now.monotonic_ms)?,
                        self.base.relays.home_region(network, now.monotonic_ms),
                    ))
                },
            )
            .map_err(control_error)?;
        if assigned.is_some() {
            let snapshot = crate::host_endpoints::Snapshot::new(self.local, &observed)?
                .with_home_relay(home)?;
            if self
                .reconnect
                .publish(snapshot, (self.base.clock)().monotonic_ms)?
                == crate::host_endpoints::Progress::Rejected
            {
                self.base.owner.close();
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        Ok(())
    }
}
impl<C: FnMut() -> Clock> Drop for Runtime<C> {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closing_shared_runtime_prevents_reconnect_and_retains_identity_until_last_owner() {
        use std::{
            sync::{
                Arc,
                atomic::{AtomicBool, AtomicUsize, Ordering},
                mpsc,
            },
            time::Duration,
        };
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity");
        let store = HostStore::open(&path).unwrap();
        let identity = store.identity().unwrap().clone();
        let network = Network::new(
            &identity,
            crate::timestamp::TimestampClock::open(store, 100).unwrap(),
            4,
            2,
        )
        .unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let owner = ControlledNetwork::start_with::<crate::host_tls::TlsStream>(
            network,
            cancelled.clone(),
            64 * 1024,
            |cancel| {
                while !cancel.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(io::ErrorKind::Interrupted.into())
            },
        )
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let reconnect = reconnect::Controller::with_setup(
            move |_| {
                count.fetch_add(1, Ordering::SeqCst);
                Err(io::ErrorKind::PermissionDenied.into())
            },
            crate::host_endpoints::Publisher::new(cancelled.clone(), Arc::new(|_| Ok(()))),
        );
        let runtime = Runtime::new(
            owner,
            UdpTransport::bind("127.0.0.1:0".parse().unwrap()).unwrap(),
            reconnect,
            identity.node_key().clone(),
            || Clock {
                unix_ms: 1_735_689_600_000,
                monotonic_ms: 0,
            },
        )
        .unwrap();
        let shared = crate::application_tcp::ThreadShared::new(runtime);
        let mut worker = shared.clone();
        let (start, started) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            started.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(
                worker
                    .run::<()>(|_, _| panic!("closed runtime granted access"))
                    .is_err()
            );
            assert_eq!(
                worker.pump().unwrap_err().kind(),
                io::ErrorKind::ConnectionAborted
            );
            done.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        shared
            .with(|runtime| {
                runtime.close();
                assert_eq!(
                    runtime.stop_reason(),
                    Some(crate::control_worker::StopReason::Cancelled)
                );
                Ok(())
            })
            .unwrap();
        drop(shared);
        assert!(matches!(
            HostStore::open(&path),
            Err(crate::store::StoreError::Locked)
        ));
        assert!(cancelled.load(Ordering::Acquire));
        start.send(()).unwrap();
        completed.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        release.send(()).unwrap();
        thread.join().unwrap();
        HostStore::open(&path).unwrap();
    }

    #[test]
    fn driver_can_be_owned_by_thread_shared_application_workers() {
        fn send<T: Send>() {}
        fn send_sync<T: Send + Sync>() {}
        send::<Runtime<fn() -> Clock>>();
        send_sync::<crate::application_tcp::ThreadShared<Runtime<fn() -> Clock>>>();
    }
}
