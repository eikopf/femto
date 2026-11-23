//! Catalog and progress HTTP transport over an existing authenticated tailnet runtime.
use crate::{
    deadline::Limit,
    http::{Stream, Transport, Url},
};
use femto_microlink::{
    host_tcp::{Driver, Shared},
    host_tcp_tls::Connector,
};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

/// Creates request-local adapters on worker threads without retaining a runtime.
#[derive(Clone)]
pub struct Factory {
    create: Arc<dyn Fn() -> io::Result<Box<dyn Transport>> + Send + Sync>,
}
impl Default for Factory {
    fn default() -> Self {
        Self {
            create: Arc::new(|| Err(io::ErrorKind::NotConnected.into())),
        }
    }
}
impl Factory {
    /// Captures one runtime generation. Replacing a runtime requires a new factory.
    pub fn new<D: Driver + Send + 'static>(
        runtime: femto_microlink::application_tcp::ThreadWeak<D>,
    ) -> Self {
        Self {
            create: Arc::new(move || {
                let runtime = runtime.upgrade().ok_or(io::ErrorKind::NotConnected)?;
                Ok(Box::new(Tailnet::new(Shared::new(runtime))?))
            }),
        }
    }
    pub fn transport(&self) -> io::Result<Box<dyn Transport>> {
        (self.create)()
    }
    /// Captures application TLS trust with the runtime generation.
    pub fn with_tls<D: Driver + Send + 'static>(
        runtime: femto_microlink::application_tcp::ThreadWeak<D>,
        tls: Connector,
    ) -> Self {
        Self {
            create: Arc::new(move || {
                let runtime = runtime.upgrade().ok_or(io::ErrorKind::NotConnected)?;
                Ok(Box::new(Tailnet::with_tls(
                    Shared::new(runtime),
                    tls.clone(),
                )))
            }),
        }
    }
    /// Routes configured origins through this generation, with no LAN fallback.
    pub fn routed<L: Transport + 'static>(
        &self,
        lan: L,
        origins: &[String],
    ) -> io::Result<Box<dyn Transport>> {
        if origins.is_empty() {
            return Ok(Box::new(lan));
        }
        if origins.len() > 16 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let origins: Vec<&str> = origins.iter().map(String::as_str).collect();
        Ok(Box::new(crate::routing::Routed::new(
            lan,
            self.transport()?,
            &origins,
        )?))
    }
}

pub struct Tailnet<D: Driver> {
    runtime: Shared<D>,
    tls: Connector,
}
impl<D: Driver> Tailnet<D> {
    pub fn new(runtime: Shared<D>) -> io::Result<Self> {
        Ok(Self {
            runtime,
            tls: Connector::mozilla()?,
        })
    }
    pub fn with_tls(runtime: Shared<D>, tls: Connector) -> Self {
        Self { runtime, tls }
    }
}
impl<D: Driver + 'static> Transport for Tailnet<D> {
    fn connect(
        &mut self,
        url: &Url,
        limit: Limit,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        if cancelled.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if limit.expired() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        // Peer names are resolved only by the authenticated coordinator map.
        let Some(url::Host::Domain(host)) = url.host() else {
            return Err(io::ErrorKind::InvalidInput.into());
        };
        let port = url
            .port_or_known_default()
            .filter(|p| *p != 0)
            .ok_or(io::ErrorKind::InvalidInput)?;
        if url.scheme() == "https" {
            Ok(Box::new(self.tls.connect(
                self.runtime.clone(),
                host,
                port,
                limit.into(),
                cancelled,
            )?))
        } else {
            Ok(Box::new(femto_microlink::host_tcp::Stream::connect(
                self.runtime.clone(),
                host,
                port,
                limit.into(),
                cancelled,
            )?))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use femto_microlink::{
        network::{Error, Network},
        timestamp::EpochStore,
    };
    use std::sync::atomic::AtomicUsize;
    use std::{
        cell::Cell,
        rc::Rc,
        time::{Duration, Instant},
    };

    struct ThreadDriver {
        calls: Arc<AtomicUsize>,
        drops: Arc<AtomicUsize>,
    }
    impl Drop for ThreadDriver {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::Relaxed);
        }
    }
    impl Driver for ThreadDriver {
        type Store = Store;
        fn run<R>(
            &mut self,
            _: impl FnOnce(&mut Network<Store>, u64) -> Result<R, Error>,
        ) -> io::Result<R> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Err(io::ErrorKind::ConnectionRefused.into())
        }
        fn pump(&mut self) -> io::Result<()> {
            panic!("unexpected pump")
        }
    }
    struct Lan(Arc<AtomicUsize>);
    impl Transport for Lan {
        fn connect(
            &mut self,
            _: &Url,
            _: Limit,
            _: Arc<AtomicBool>,
        ) -> io::Result<Box<dyn Stream>> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Err(io::ErrorKind::AddrNotAvailable.into())
        }
    }
    fn attempt(transport: &mut dyn Transport, url: &str) -> io::ErrorKind {
        transport
            .connect(
                &Url::parse(url).unwrap(),
                Limit::Until(Instant::now() + Duration::from_secs(2)),
                Arc::new(AtomicBool::new(false)),
            )
            .err()
            .unwrap()
            .kind()
    }
    #[test]
    fn worker_factory_routes_exact_origins_and_retains_only_active_requests() {
        use femto_microlink::application_tcp::ThreadShared;
        let calls = Arc::new(AtomicUsize::new(0));
        let drops = Arc::new(AtomicUsize::new(0));
        let lan_calls = Arc::new(AtomicUsize::new(0));
        let runtime = ThreadShared::new(ThreadDriver {
            calls: calls.clone(),
            drops: drops.clone(),
        });
        let factory = Factory::new(runtime.downgrade());
        let retained = factory.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker_lan_calls = lan_calls.clone();
        let worker = std::thread::spawn(move || {
            let mut transport = factory
                .routed(Lan(worker_lan_calls), &["http://PEER.example:80".into()])
                .unwrap();
            ready_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            assert_eq!(
                attempt(&mut transport, "http://peer.example/book"),
                io::ErrorKind::ConnectionRefused
            );
            assert_eq!(
                attempt(&mut transport, "http://peer.example:81/book"),
                io::ErrorKind::AddrNotAvailable
            );
        });
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(runtime);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert_eq!(lan_calls.load(Ordering::Relaxed), 1);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert_eq!(
            retained.transport().err().unwrap().kind(),
            io::ErrorKind::NotConnected
        );
        assert_eq!(
            retained
                .routed(Lan(lan_calls.clone()), &["http://peer.example".into()])
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::NotConnected
        );
        assert_eq!(lan_calls.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn unavailable_factory_allows_lan_only_requests() {
        let factory = Factory::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut transport = factory.routed(Lan(calls.clone()), &[]).unwrap();
        assert_eq!(
            attempt(&mut transport, "http://localhost/book"),
            io::ErrorKind::AddrNotAvailable
        );
        assert_eq!(
            factory.transport().err().unwrap().kind(),
            io::ErrorKind::NotConnected
        );
        assert_eq!(
            factory
                .routed(Lan(calls.clone()), &vec!["http://peer.example".into(); 17])
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
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
    struct NoDial(Rc<Cell<usize>>);
    impl Driver for NoDial {
        type Store = Store;
        fn run<R>(
            &mut self,
            _: impl FnOnce(&mut Network<Store>, u64) -> Result<R, Error>,
        ) -> io::Result<R> {
            self.0.set(self.0.get() + 1);
            Err(io::ErrorKind::ConnectionRefused.into())
        }
        fn pump(&mut self) -> io::Result<()> {
            panic!("unexpected transport pump")
        }
    }
    #[test]
    fn validation_precedes_resolution_and_named_targets_use_the_runtime() {
        let calls = Rc::new(Cell::new(0));
        let mut transport = Tailnet::new(Shared::new(NoDial(calls.clone()))).unwrap();
        let limit = Limit::Until(Instant::now() + Duration::from_secs(2));
        let cancelled = Arc::new(AtomicBool::new(false));
        for url in [
            "ftp://peer.example",
            "http://100.64.0.1/",
            "https://[::1]/",
            "http://user@peer.example",
            "http://peer.example/#fragment",
            "http://peer.example:0/",
        ] {
            assert_eq!(
                transport
                    .connect(&Url::parse(url).unwrap(), limit, cancelled.clone())
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
        assert_eq!(calls.get(), 0);
        let url = Url::parse("https://peer.example/feed").unwrap();
        assert_eq!(
            transport
                .connect(&url, Limit::Until(Instant::now()), cancelled.clone())
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::TimedOut
        );
        cancelled.store(true, Ordering::Release);
        assert_eq!(
            transport
                .connect(&url, limit, cancelled.clone())
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(calls.get(), 0);
        cancelled.store(false, Ordering::Release);
        for url in ["http://peer.example/feed", "https://peer.example/feed"] {
            assert_eq!(
                transport
                    .connect(&Url::parse(url).unwrap(), limit, cancelled.clone())
                    .err()
                    .unwrap()
                    .kind(),
                io::ErrorKind::ConnectionRefused
            );
        }
        assert_eq!(calls.get(), 2);
    }
}
