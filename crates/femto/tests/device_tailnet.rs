//! Device startup orchestration with an in-memory store and failing connector.
use femto_microlink::{
    device_config::Config,
    identity_store::{Backend, IdentityStore, RECORD_LEN},
    protocol::{
        identity::DeviceIdentity,
        key::{Machine, PrivateKey, PublicKey},
    },
};
use std::{
    cell::RefCell,
    io,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
mod clock {
    pub fn require_ready() -> std::io::Result<()> {
        Ok(())
    }
}
struct LocalStream(Rc<()>);
impl io::Read for LocalStream {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }
}
impl io::Write for LocalStream {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl femto_microlink::derp::DeadlineIo for LocalStream {
    fn set_deadline(&mut self, _: Instant) {
        let _ = Rc::strong_count(&self.0);
    }
}
mod network {
    pub type Connection = super::LocalStream;
}
mod control {
    use super::*;
    pub static RETRY_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    pub fn fetch_control_key(
        host: &str,
        _: u16,
        _: u16,
        _: Instant,
        _: Arc<AtomicBool>,
    ) -> io::Result<PublicKey<Machine>> {
        if host == "retry.example" {
            let call = RETRY_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Err(if call < 2 {
                io::ErrorKind::TimedOut
            } else {
                io::ErrorKind::PermissionDenied
            }
            .into());
        }
        Err(io::Error::other("injected bootstrap failure"))
    }
    pub fn connect_session(
        _: &femto_microlink::control_session::Endpoint,
        _: &PrivateKey<Machine>,
        _: Duration,
        _: Arc<AtomicBool>,
    ) -> io::Result<femto_microlink::control_session::ControlSession<LocalStream>> {
        panic!("failed bootstrap must not proceed to session setup")
    }
}
pub struct Memory(Rc<RefCell<Option<Vec<u8>>>>);
impl Backend for Memory {
    type Error = &'static str;
    fn read_rotation(
        &mut self,
        _: &mut [u8; femto_microlink::rotation::RECORD_LEN + 1],
    ) -> Result<Option<usize>, Self::Error> {
        Ok(None)
    }
    fn create_rotation(
        &mut self,
        _: &[u8; femto_microlink::rotation::RECORD_LEN],
    ) -> Result<(), Self::Error> {
        Err("rotation unavailable in startup fixture")
    }
    fn clear_rotation(&mut self) -> Result<(), Self::Error> {
        Err("rotation unavailable in startup fixture")
    }
    fn read(&mut self, out: &mut [u8; RECORD_LEN + 1]) -> Result<Option<usize>, Self::Error> {
        Ok(self.0.borrow().as_ref().map(|b| {
            out[..b.len()].copy_from_slice(b);
            b.len()
        }))
    }
    fn write(&mut self, b: &[u8; RECORD_LEN], create: bool) -> Result<(), Self::Error> {
        let mut record = self.0.borrow_mut();
        if create == record.is_some() {
            return Err("existence mismatch");
        }
        *record = Some(b.to_vec());
        Ok(())
    }
}
mod identity {
    pub type Store = super::IdentityStore<super::Memory>;
    pub fn open() -> Result<Store, String> {
        Err("injected identity open failure".into())
    }
}
#[path = "../../../firmware/picco/src/tailnet.rs"]
mod tailnet;
#[path = "../../../firmware/picco/src/tailnet_rotation.rs"]
mod tailnet_rotation;
#[test]
fn renewal_open_failure_is_reported_and_worker_is_joined() {
    let config = Config::parse(
        br#"{"version":1,"host":"control.example","port":443,"hostname":"femto-test"}"#,
    )
    .unwrap();
    let mut job = tailnet_rotation::Job::start(config, None).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(report) = job.poll() {
            match report {
                Err(()) => break,
                Ok(report) => panic!(
                    "unexpected renewal result: {:?}, continuation {}",
                    report.outcome,
                    report.followup.is_some()
                ),
            }
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(job.close_and_wait(Duration::from_secs(1)));
}

#[test]
fn startup_failure_is_reported_without_authorization_and_shutdown_joins() {
    let bytes = Rc::new(RefCell::new(None));
    let keys = DeviceIdentity::generate().unwrap();
    let public = keys.node_key().public();
    let store = IdentityStore::create(Memory(bytes.clone()), keys).unwrap();
    let config = Config::parse(
        br#"{"version":1,"host":"control.example","port":443,"hostname":"femto-test"}"#,
    )
    .unwrap();
    let mut runtime = tailnet::Runtime::start(config, store).unwrap();
    let source = femto_microlink::endpoints::LocalAddress::default();
    source.set(Some("192.168.1.2".parse().unwrap()));
    runtime.set_local_address_source(source);
    let followup = runtime.approval(
        femto_microlink::enrollment::ApprovalUrl::parse("https://login.example/token").unwrap(),
    );
    let start = Instant::now();
    loop {
        match runtime.registration() {
            Some(tailnet::Registration::Failed) => break,
            Some(tailnet::Registration::Retrying) => panic!("terminal error retried"),
            Some(tailnet::Registration::Outcome(outcome)) => {
                panic!("unexpected outcome: {outcome:?}")
            }
            None => {
                assert!(start.elapsed() < Duration::from_secs(2));
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    assert!(runtime.close_and_wait(Duration::from_secs(2)));
    assert!(runtime.poll().is_err());
    use femto_microlink::application_tcp::Driver;
    assert!(
        runtime
            .run::<()>(|_, _| panic!("closed runtime must deny application access"))
            .is_err()
    );
    assert!(runtime.pump().is_err());
    drop(runtime);
    let mut reopened = IdentityStore::open(Memory(bytes)).unwrap();
    assert_eq!(reopened.identity().unwrap().node_key().public(), public);
    assert!(
        femto_microlink::timestamp::EpochStore::load_epoch(&mut reopened)
            .unwrap()
            .unwrap()
            > 0
    );
    let changed = Config::parse(
        br#"{"version":1,"host":"other.example","port":443,"hostname":"femto-test"}"#,
    )
    .unwrap();
    assert!(tailnet::Runtime::start_with_followup(changed, reopened, Some(followup)).is_err());
}

mod relay_transport {
    pub type Tls = femto_microlink::host_tls::NonblockingTls;
    pub type Pool = femto_microlink::host_derp::Pool;
    pub fn connect(
        _: femto_microlink::network::RelayPlan,
        _: std::net::SocketAddr,
        _: femto_microlink::protocol::key::PrivateKey<femto_microlink::protocol::key::Node>,
        _: std::time::Duration,
        _: std::sync::Arc<std::sync::atomic::AtomicBool>,
        _: u64,
    ) -> Result<femto_microlink::host_derp::AuthorizedConnection, femto_microlink::relay_pool::Error>
    {
        panic!("failed coordinator setup must not dial relays")
    }
}

#[test]
fn transient_setup_reconnects_with_same_store_then_stops_on_permission_failure() {
    use std::sync::atomic::Ordering;
    control::RETRY_CALLS.store(0, Ordering::SeqCst);
    let bytes = Rc::new(RefCell::new(None));
    let store =
        IdentityStore::create(Memory(bytes.clone()), DeviceIdentity::generate().unwrap()).unwrap();
    let config = Config::parse(
        br#"{"version":1,"host":"retry.example","port":443,"hostname":"femto-retry"}"#,
    )
    .unwrap();
    let mut runtime = tailnet::Runtime::start(config, store).unwrap();
    let record = bytes.borrow().clone();
    let start = Instant::now();
    let mut attempts = Vec::new();
    let mut observed = 0;
    let mut retry_reports = 0;
    let mut saw_reconnecting = false;
    loop {
        let count = control::RETRY_CALLS.load(Ordering::SeqCst);
        if count > observed {
            attempts.push(start.elapsed());
            observed = count;
        }
        if let Some(report) = runtime.registration() {
            match report {
                tailnet::Registration::Retrying => retry_reports += 1,
                tailnet::Registration::Failed => {}
                tailnet::Registration::Outcome(_) => panic!("bootstrap failure registered"),
            }
        }
        if runtime.poll().is_err() {
            let count = control::RETRY_CALLS.load(Ordering::SeqCst);
            if count > observed {
                attempts.push(start.elapsed());
            }
            break;
        }
        saw_reconnecting |= runtime.reconnecting();
        // A session without an address is never left unpolled.
        assert!(!runtime.quiet());
        assert!(start.elapsed() < Duration::from_secs(8));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(control::RETRY_CALLS.load(Ordering::SeqCst), 3);
    assert_eq!(attempts.len(), 3);
    assert!(attempts[1] - attempts[0] >= Duration::from_millis(950));
    assert!(attempts[2] - attempts[1] >= Duration::from_millis(1950));
    assert_eq!(retry_reports, 2);
    assert!(saw_reconnecting);
    assert_eq!(
        *bytes.borrow(),
        record,
        "reconnect reopened or rewrote the timestamp owner"
    );
    assert!(runtime.close_and_wait(Duration::from_secs(2)));
    assert!(runtime.poll().is_err());
    assert_eq!(control::RETRY_CALLS.load(Ordering::SeqCst), 3);
}
