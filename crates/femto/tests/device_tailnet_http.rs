use femto_catalog::http::{Transport as _, Url};
use femto_microlink::{
    application_tcp::Driver,
    network::{Error, Network},
    timestamp::EpochStore,
};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
pub struct Store;
impl EpochStore for Store {
    type Error = ();
    fn load_epoch(&mut self) -> Result<Option<u64>, ()> {
        Ok(None)
    }
    fn reserve_epoch(&mut self, _: u64) -> Result<(), ()> {
        Ok(())
    }
}
#[derive(Clone)]
pub struct NoDial(Arc<AtomicUsize>);
impl Driver for NoDial {
    type Store = Store;
    fn run<R>(
        &mut self,
        _: impl FnOnce(&mut Network<Store>, u64) -> Result<R, Error>,
    ) -> io::Result<R> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Err(io::ErrorKind::ConnectionRefused.into())
    }
    fn pump(&mut self) -> io::Result<()> {
        panic!("unexpected pump")
    }
}
mod tailnet_menu {
    pub type Driver = super::NoDial;
    pub enum Session {
        Absent,
        Awaited,
        Open(Driver),
    }
    pub type Lookup = std::sync::Arc<dyn Fn() -> Session + Send + Sync>;
}
#[path = "../../../firmware/picco/src/tailnet_http.rs"]
mod tailnet_http;
#[test]
fn validation_precedes_peer_resolution_and_unavailable_tailnet_never_dials() {
    let calls = Arc::new(AtomicUsize::new(0));
    let driver = NoDial(calls.clone());
    let mut transport = tailnet_http::Transport(Arc::new(move || {
        tailnet_menu::Session::Open(driver.clone())
    }));
    let deadline = Instant::now() + Duration::from_secs(1);
    let cancel = Arc::new(AtomicBool::new(false));
    for url in [
        "ftp://peer.example",
        "http://100.64.0.1",
        "http://user@peer.example",
        "http://peer.example/#fragment",
        "http://peer.example:0",
    ] {
        assert_eq!(
            transport
                .connect(
                    &Url::parse(url).unwrap(),
                    femto_catalog::deadline::Limit::Until(deadline),
                    cancel.clone()
                )
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
    assert_eq!(
        transport
            .connect(
                &Url::parse("https://peer.example/").unwrap(),
                femto_catalog::deadline::Limit::Until(deadline),
                cancel.clone()
            )
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::ConnectionRefused
    );
    let url = Url::parse("http://peer.example/feed").unwrap();
    assert_eq!(
        transport
            .connect(
                &url,
                femto_catalog::deadline::Limit::Until(Instant::now()),
                cancel.clone()
            )
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::TimedOut
    );
    cancel.store(true, Ordering::Release);
    assert_eq!(
        transport
            .connect(
                &url,
                femto_catalog::deadline::Limit::Until(deadline),
                cancel.clone()
            )
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::Interrupted
    );
    cancel.store(false, Ordering::Release);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        transport
            .connect(
                &url,
                femto_catalog::deadline::Limit::Until(deadline),
                cancel.clone()
            )
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::ConnectionRefused
    );
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        tailnet_http::Transport(Arc::new(|| tailnet_menu::Session::Absent))
            .connect(
                &url,
                femto_catalog::deadline::Limit::Until(deadline),
                cancel
            )
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::NotConnected
    );
}

#[test]
fn an_awaited_session_is_waited_for_within_the_limit() {
    use femto_catalog::deadline::Limit;
    let url = Url::parse("http://peer.example/").unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let (driver, looks) = (NoDial(calls.clone()), AtomicUsize::new(0));
    let mut transport = tailnet_http::Transport(Arc::new(move || {
        if looks.fetch_add(1, Ordering::Relaxed) < 3 {
            tailnet_menu::Session::Awaited
        } else {
            tailnet_menu::Session::Open(driver.clone())
        }
    }));
    let limit = Limit::Stall(Duration::from_secs(5));
    let error = transport
        .connect(&url, limit, Arc::default())
        .err()
        .unwrap();
    assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    let mut never = tailnet_http::Transport(Arc::new(|| tailnet_menu::Session::Awaited));
    let limit = Limit::Stall(Duration::from_millis(50));
    let error = never.connect(&url, limit, Arc::default()).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    let cancelled = Arc::new(AtomicBool::new(true));
    let limit = Limit::Stall(Duration::from_secs(5));
    let error = never.connect(&url, limit, cancelled).err().unwrap();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
}

mod tailnet_tls {
    pub struct Tls;
    impl Tls {
        pub fn connect<D: femto_microlink::application_tcp::Driver>(
            _: femto_microlink::application_tcp::Stream<D>,
            _: &str,
        ) -> std::io::Result<femto_microlink::application_tcp::Stream<D>> {
            panic!("refused TCP connection cannot enter TLS setup")
        }
    }
}
