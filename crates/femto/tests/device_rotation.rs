//! Actual device renewal worker with durable host storage and injected control.
//! No TLS, Noise exchange, NVS, or independent coordinator is exercised here.
use femto_microlink::{
    device_config::Config,
    device_enrollment::Followup,
    enrollment::{Enrollment, Outcome},
    protocol::{
        control::RegisterRequest,
        identity::DeviceIdentity,
        key::{Machine, PrivateKey, PublicKey},
    },
    rotation::PendingRotation,
    store::HostStore,
    timestamp::EpochStore,
};
use std::{
    io,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
enum Mode {
    Approval,
    Authorized,
    CommitFailure,
    Held,
}
struct Fixture {
    directory: PathBuf,
    mode: Mode,
    requests: Vec<serde_json::Value>,
}
static FIXTURE: Mutex<Option<Fixture>> = Mutex::new(None);
static ENTERED: AtomicBool = AtomicBool::new(false);
static RELEASE: AtomicBool = AtomicBool::new(false);
mod clock {
    pub fn require_ready() -> std::io::Result<()> {
        Ok(())
    }
}
mod identity {
    pub fn open() -> Result<super::HostStore, String> {
        let directory = super::FIXTURE
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .directory
            .clone();
        super::HostStore::open(directory).map_err(|e| e.to_string())
    }
}
mod control {
    use super::*;
    pub fn fetch_control_key(
        _: &str,
        _: u16,
        _: u16,
        _: Instant,
        _: Arc<AtomicBool>,
    ) -> io::Result<PublicKey<Machine>> {
        Ok(DeviceIdentity::generate().unwrap().machine_key().public())
    }
    pub struct Session {
        cancelled: Arc<AtomicBool>,
    }
    pub fn connect_session(
        _: &femto_microlink::control_session::Endpoint,
        _: &PrivateKey<Machine>,
        _: Duration,
        cancelled: Arc<AtomicBool>,
    ) -> io::Result<Session> {
        Ok(Session { cancelled })
    }
    impl Session {
        pub fn register_rotation(
            &mut self,
            pending: &PendingRotation,
            request: &RegisterRequest,
            commit: impl FnOnce(&Enrollment) -> io::Result<()>,
        ) -> io::Result<Outcome> {
            assert_eq!(request.node_key, pending.candidate().node_key().public());
            assert_eq!(request.old_node_key, pending.old_node_key());
            let (mode, directory) = {
                let mut fixture = FIXTURE.lock().unwrap();
                let fixture = fixture.as_mut().unwrap();
                fixture
                    .requests
                    .push(serde_json::to_value(request).unwrap());
                (fixture.mode, fixture.directory.clone())
            };
            if matches!(mode, Mode::Held) {
                ENTERED.store(true, Ordering::Release);
                let deadline = Instant::now() + Duration::from_secs(3);
                while !RELEASE.load(Ordering::Acquire) {
                    assert!(Instant::now() < deadline);
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            if self.cancelled.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let mut enrollment = Enrollment::default();
            let response = if matches!(mode, Mode::Approval) {
                br#"{"AuthURL":"https://login.example/approval"}"#.as_slice()
            } else {
                br#"{"MachineAuthorized":true}"#.as_slice()
            };
            let outcome = enrollment.apply(request.node_key, 200, response).unwrap();
            if outcome == Outcome::Authorized {
                if matches!(mode, Mode::CommitFailure) {
                    std::fs::rename(directory.join("identity.bin"), directory.join("saved.bin"))?;
                    std::fs::create_dir(directory.join("identity.bin"))?;
                }
                commit(&enrollment)?;
            }
            Ok(outcome)
        }
        pub fn close(&mut self) {}
    }
}
#[path = "../../../firmware/picco/src/tailnet_rotation.rs"]
mod tailnet_rotation;

fn config(host: &str) -> Config {
    Config::parse(format!(r#"{{"version":1,"host":"{host}","port":443,"hostname":"femto-test","auth_key":"fixture-auth"}}"#).as_bytes()).unwrap()
}
fn run(host: &str, followup: Option<Followup>) -> Result<tailnet_rotation::Report, ()> {
    let mut job = tailnet_rotation::Job::start(config(host), followup).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let result = loop {
        if let Some(report) = job.poll() {
            break report;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(job.close_and_wait(Duration::from_secs(1)));
    result
}
fn mode(mode: Mode) {
    FIXTURE.lock().unwrap().as_mut().unwrap().mode = mode;
}

#[test]
fn worker_approval_commit_failure_restart_and_cancellation_preserve_journal_ownership() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("identity");
    let mut store = HostStore::open(&directory).unwrap();
    let old = store.identity().unwrap().node_key().public();
    store.reserve_epoch(123).unwrap();
    drop(store);
    *FIXTURE.lock().unwrap() = Some(Fixture {
        directory: directory.clone(),
        mode: Mode::Approval,
        requests: Vec::new(),
    });
    let report = run("control.example", None).unwrap();
    assert!(matches!(
        report.outcome,
        Outcome::AwaitingAuthorization(Some(_))
    ));
    let followup = report.followup.unwrap();
    let mut store = HostStore::open(&directory).unwrap();
    assert!(store.identity().is_err());
    let pending = store
        .begin_node_key_rotation("control.example", 443)
        .unwrap();
    assert_eq!(pending.old_node_key(), old);
    let candidate = pending.candidate().node_key().public();
    drop(store);
    assert!(run("other.example", Some(followup.clone())).is_err());
    mode(Mode::CommitFailure);
    assert!(run("control.example", Some(followup.clone())).is_err());
    assert!(directory.join("rotation.bin").exists());
    std::fs::remove_dir(directory.join("identity.bin")).unwrap();
    std::fs::rename(directory.join("saved.bin"), directory.join("identity.bin")).unwrap();
    mode(Mode::Authorized);
    assert_eq!(
        run("control.example", Some(followup)).unwrap().outcome,
        Outcome::Authorized
    );
    let mut store = HostStore::open(&directory).unwrap();
    assert_eq!(store.identity().unwrap().node_key().public(), candidate);
    assert_eq!(store.load_epoch().unwrap(), Some(123));
    assert!(!directory.join("rotation.bin").exists());
    drop(store);
    {
        let fixture = FIXTURE.lock().unwrap();
        let requests = &fixture.as_ref().unwrap().requests;
        assert_eq!(requests.len(), 3);
        assert!(requests[0].get("Auth").is_some());
        for request in &requests[1..] {
            assert!(request.get("Auth").is_none());
            assert_eq!(request["Followup"], "https://login.example/approval");
            assert_eq!(request["NodeKey"], candidate.to_string());
        }
    }
    mode(Mode::Held);
    let mut job = tailnet_rotation::Job::start(config("control.example"), None).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !ENTERED.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!job.close_and_wait(Duration::ZERO));
    assert!(HostStore::open(&directory).is_err());
    RELEASE.store(true, Ordering::Release);
    assert!(job.close_and_wait(Duration::from_secs(1)));
    assert!(job.poll().unwrap().is_err());
    let mut store = HostStore::open(&directory).unwrap();
    let pending = store
        .begin_node_key_rotation("control.example", 443)
        .unwrap();
    assert_eq!(pending.old_node_key(), candidate);
    assert_ne!(pending.candidate().node_key().public(), candidate);
    assert!(store.identity().is_err());
}
