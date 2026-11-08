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
}
#[path = "../../../firmware/picco/src/tailnet_http.rs"]
mod tailnet_http;
#[test]
fn validation_precedes_peer_resolution_and_unavailable_tailnet_never_dials() {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut transport = tailnet_http::Transport(Some(NoDial(calls.clone())));
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
                .connect(&Url::parse(url).unwrap(), deadline, cancel.clone())
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
                deadline,
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
            .connect(&url, Instant::now(), cancel.clone())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::TimedOut
    );
    cancel.store(true, Ordering::Release);
    assert_eq!(
        transport
            .connect(&url, deadline, cancel.clone())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::Interrupted
    );
    cancel.store(false, Ordering::Release);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        transport
            .connect(&url, deadline, cancel.clone())
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::ConnectionRefused
    );
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(
        tailnet_http::Transport(None)
            .connect(&url, deadline, cancel)
            .err()
            .unwrap()
            .kind(),
        io::ErrorKind::NotConnected
    );
}

mod clock {
    pub fn require_ready() -> std::io::Result<()> {
        Ok(())
    }
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
