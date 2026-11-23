//! Reader tailnet controls with injected identity and runtime ownership.
use femto_microlink::{
    device_config::Config,
    enrollment::{ApprovalUrl, Outcome},
    protocol::identity::DeviceIdentity,
};
use femto_reader::ui::{ReaderUi, TailnetEvent};
use std::{cell::RefCell, time::Duration};
#[derive(Default)]
struct State {
    clock_bad: bool,
    starts: usize,
    followups: usize,
    identity: Option<DeviceIdentity>,
    opened: usize,
    provisioned: usize,
    closes: usize,
    joined: bool,
    drop_count: usize,
    outcome: Option<tailnet::Registration>,
    renewals: usize,
    renewal_followups: usize,
    renewal: Option<Result<tailnet_rotation::Report, ()>>,
}
mod tailnet_rotation {
    use super::*;
    pub struct Report {
        pub outcome: Outcome,
        pub followup: Option<femto_microlink::device_enrollment::Followup>,
    }
    pub struct Job;
    impl Job {
        pub fn start(
            config: Config,
            followup: Option<femto_microlink::device_enrollment::Followup>,
        ) -> Result<Self, String> {
            config.validate()?;
            clock::require_ready().map_err(|e| e.to_string())?;
            STATE.with_borrow_mut(|s| {
                s.renewals += 1;
                s.renewal_followups += usize::from(followup.is_some());
            });
            Ok(Self)
        }
        pub fn poll(&mut self) -> Option<Result<Report, ()>> {
            STATE.with_borrow_mut(|s| s.renewal.take())
        }
        pub fn close_and_wait(&mut self, _: Duration) -> bool {
            STATE.with_borrow_mut(|s| {
                s.closes += 1;
                s.joined
            })
        }
    }
}
thread_local! { static STATE: RefCell<State> = RefCell::new(State::default()); }
mod clock {
    pub fn require_ready() -> std::io::Result<()> {
        if super::STATE.with_borrow(|s| s.clock_bad) {
            return Err(std::io::Error::other("clock unavailable"));
        }
        Ok(())
    }
}
mod identity {
    use super::*;
    pub struct Store;
    impl femto_microlink::timestamp::EpochStore for Store {
        type Error = ();
        fn load_epoch(&mut self) -> Result<Option<u64>, ()> {
            Ok(None)
        }
        fn reserve_epoch(&mut self, _: u64) -> Result<(), ()> {
            Ok(())
        }
    }
    pub fn open() -> Result<Store, String> {
        STATE.with_borrow_mut(|s| s.opened += 1);
        Ok(Store)
    }
    pub fn provision(_: DeviceIdentity) -> Result<Store, String> {
        STATE.with_borrow_mut(|s| s.provisioned += 1);
        Ok(Store)
    }
}
mod tailnet {
    use super::*;
    pub enum Registration {
        Outcome(Outcome),
        Failed,
        Retrying,
    }
    pub struct Runtime {
        local: femto_microlink::endpoints::LocalAddress,
        context: femto_microlink::device_enrollment::Context,
    }
    impl Runtime {
        pub fn reconnecting(&self) -> bool {
            false
        }
        pub fn quiet(&self) -> bool {
            false
        }
        pub fn start(config: Config, store: identity::Store) -> Result<Self, String> {
            Self::start_with_followup(config, store, None)
        }
        pub fn start_with_followup(
            config: Config,
            _: identity::Store,
            followup: Option<femto_microlink::device_enrollment::Followup>,
        ) -> Result<Self, String> {
            STATE.with_borrow_mut(|s| {
                let identity = s
                    .identity
                    .get_or_insert_with(|| DeviceIdentity::generate().unwrap());
                let context = femto_microlink::device_enrollment::Context::new(&config, identity);
                if let Some(followup) = &followup {
                    context.validate(followup)?;
                    s.followups += 1;
                }
                s.starts += 1;
                Ok(Self {
                    context,
                    local: Default::default(),
                })
            })
        }
        pub fn set_local_address_source(
            &mut self,
            source: femto_microlink::endpoints::LocalAddress,
        ) {
            self.local = source;
        }
        pub fn local_endpoint(&self) -> Option<std::net::SocketAddrV4> {
            self.local.endpoint(41641)
        }
        pub fn approval(&self, url: ApprovalUrl) -> femto_microlink::device_enrollment::Followup {
            self.context.followup(url)
        }
        pub fn registration(&mut self) -> Option<Registration> {
            STATE.with_borrow_mut(|s| s.outcome.take())
        }
        pub fn poll(&mut self) -> Result<Option<std::net::Ipv4Addr>, String> {
            Ok(None)
        }
        pub fn close_and_wait(&mut self, _: Duration) -> bool {
            STATE.with_borrow_mut(|s| {
                s.closes += 1;
                s.joined
            })
        }
    }
    impl femto_microlink::application_tcp::Driver for Runtime {
        type Store = identity::Store;
        fn run<R>(
            &mut self,
            _: impl FnOnce(
                &mut femto_microlink::network::Network<Self::Store>,
                u64,
            ) -> Result<R, femto_microlink::network::Error>,
        ) -> std::io::Result<R> {
            Err(std::io::ErrorKind::ConnectionAborted.into())
        }
        fn pump(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl Drop for Runtime {
        fn drop(&mut self) {
            STATE.with_borrow_mut(|s| {
                assert!(s.joined);
                s.drop_count += 1;
            });
        }
    }
}
#[path = "../../../firmware/picco/src/tailnet_menu.rs"]
mod tailnet_menu;
fn configuration() -> tempfile::TempDir {
    STATE.with_borrow_mut(|s| *s = State::default());
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("tailnet.json"),
        br#"{"version":1,"host":"control.example","port":443,"hostname":"femto-test"}"#,
    )
    .unwrap();
    dir
}
#[test]
fn renewal_approval_retries_and_disconnect_clears_only_its_in_memory_continuation() {
    let dir = configuration();
    let path = dir.path().join("tailnet.json");
    let config = Config::parse(&std::fs::read(&path).unwrap()).unwrap();
    let pending = femto_microlink::rotation::PendingRotation::prepare(
        &DeviceIdentity::generate().unwrap(),
        &config.host,
        config.port,
    )
    .unwrap();
    let context =
        femto_microlink::device_enrollment::Context::for_rotation(&config, &pending).unwrap();
    let url = ApprovalUrl::parse("https://login.example/renew-token").unwrap();
    let mut manager = tailnet_menu::Manager::new(path);
    let mut ui = ReaderUi::default();
    STATE.with_borrow_mut(|s| s.joined = true);
    manager.event(TailnetEvent::Renew, &mut ui).unwrap();
    STATE.with_borrow_mut(|s| {
        s.renewal = Some(Ok(tailnet_rotation::Report {
            outcome: Outcome::AwaitingAuthorization(Some(url.clone())),
            followup: Some(context.followup(url)),
        }))
    });
    manager.poll(&mut ui);
    manager.event(TailnetEvent::Renew, &mut ui).unwrap();
    assert_eq!(STATE.with_borrow(|s| s.renewal_followups), 1);
    manager.event(TailnetEvent::Stop, &mut ui).unwrap();
    manager.event(TailnetEvent::Renew, &mut ui).unwrap();
    assert_eq!(STATE.with_borrow(|s| s.renewal_followups), 1);
    manager.close_and_wait(Duration::ZERO);
}

#[test]
fn renewal_blocks_connection_until_its_worker_has_joined() {
    let dir = configuration();
    let mut manager = tailnet_menu::Manager::new(dir.path().join("tailnet.json"));
    let mut ui = ReaderUi::default();
    manager.event(TailnetEvent::Renew, &mut ui).unwrap();
    assert_eq!(STATE.with_borrow(|s| s.renewals), 1);
    assert!(manager.driver().is_none());
    assert!(manager.event(TailnetEvent::Connect, &mut ui).is_err());
    assert!(manager.event(TailnetEvent::Renew, &mut ui).is_err());
    STATE.with_borrow_mut(|s| {
        s.renewal = Some(Ok(tailnet_rotation::Report {
            outcome: Outcome::Authorized,
            followup: None,
        }))
    });
    manager.poll(&mut ui);
    assert!(!manager.close_and_wait(Duration::ZERO));
    assert!(manager.event(TailnetEvent::Connect, &mut ui).is_err());
    STATE.with_borrow_mut(|s| s.joined = true);
    manager.poll(&mut ui);
    manager.event(TailnetEvent::Connect, &mut ui).unwrap();
    assert!(manager.event(TailnetEvent::Renew, &mut ui).is_err());
    assert!(manager.close_and_wait(Duration::ZERO));
}

#[test]
fn a_routed_service_opens_one_session_without_showing_the_panel() {
    let dir = configuration();
    let mut manager = tailnet_menu::Manager::new(dir.path().join("tailnet.json"));
    let ui = ReaderUi::default();
    manager.ensure().unwrap();
    manager.ensure().unwrap();
    assert_eq!(STATE.with_borrow(|s| (s.starts, s.opened)), (1, 1));
    assert!(manager.driver().is_some());
    assert!(!ui.is_tailnet_open());
    STATE.with_borrow_mut(|s| s.joined = true);
    assert!(manager.close_and_wait(Duration::ZERO));
}
#[test]
fn a_service_asking_before_the_clock_is_set_gets_its_session_when_it_is() {
    use tailnet_menu::Session;
    let dir = configuration();
    let mut manager = tailnet_menu::Manager::new(dir.path().join("tailnet.json"));
    let mut ui = ReaderUi::default();
    let session = manager.session();
    assert!(matches!(session(), Session::Absent));
    STATE.with_borrow_mut(|s| s.clock_bad = true);
    manager.ensure().unwrap();
    manager.poll(&mut ui);
    assert!(matches!(session(), Session::Awaited));
    assert_eq!(STATE.with_borrow(|s| s.starts), 0);
    STATE.with_borrow_mut(|s| s.clock_bad = false);
    manager.poll(&mut ui);
    let Session::Open(driver) = session() else {
        panic!("session not open");
    };
    drop(driver);
    assert_eq!(STATE.with_borrow(|s| (s.starts, s.opened)), (1, 1));
    STATE.with_borrow_mut(|s| s.joined = true);
    assert!(manager.close_and_wait(Duration::ZERO));
    assert!(matches!(session(), Session::Absent));
    // Without a configuration nothing is awaited.
    std::fs::remove_file(dir.path().join("tailnet.json")).unwrap();
    STATE.with_borrow_mut(|s| s.clock_bad = true);
    assert!(manager.ensure().is_err());
    assert!(matches!(session(), Session::Absent));
    STATE.with_borrow_mut(|s| s.clock_bad = false);
}
#[test]
fn explicit_actions_preserve_runtime_until_shutdown_finishes() {
    let dir = configuration();
    let mut manager = tailnet_menu::Manager::new(dir.path().join("tailnet.json"));
    let mut ui = ReaderUi::default();
    manager.event(TailnetEvent::Open, &mut ui).unwrap();
    assert_eq!(STATE.with_borrow(|s| s.starts), 0);
    manager.event(TailnetEvent::Connect, &mut ui).unwrap();
    assert!(manager.event(TailnetEvent::Provision, &mut ui).is_err());
    manager.event(TailnetEvent::Stop, &mut ui).unwrap();
    assert_eq!(STATE.with_borrow(|s| s.drop_count), 0);
    assert!(!manager.close_and_wait(Duration::ZERO));
    STATE.with_borrow_mut(|s| s.joined = true);
    assert!(manager.poll(&mut ui));
    assert_eq!(STATE.with_borrow(|s| s.drop_count), 1);
    manager.event(TailnetEvent::Provision, &mut ui).unwrap();
    assert_eq!(STATE.with_borrow(|s| (s.opened, s.provisioned)), (1, 1));
    assert!(manager.close_and_wait(Duration::ZERO));
}
#[test]
fn invalid_configuration_never_opens_or_provisions_keys() {
    let dir = configuration();
    for bytes in [
        vec![b'x'; 8193],
        b"{}".to_vec(),
        b"{\"auth_key\":\"secret\"}".to_vec(),
    ] {
        std::fs::write(dir.path().join("tailnet.json"), bytes).unwrap();
        let mut manager = tailnet_menu::Manager::new(dir.path().join("tailnet.json"));
        let error = manager
            .event(TailnetEvent::Provision, &mut ReaderUi::default())
            .unwrap_err();
        assert!(!error.contains("secret"));
    }
    assert_eq!(
        STATE.with_borrow(|s| (s.starts, s.opened, s.provisioned)),
        (0, 0, 0)
    );
}
#[test]
fn approval_and_setup_failure_stop_without_automatic_reconnect() {
    for outcome in [
        tailnet::Registration::Outcome(Outcome::AwaitingAuthorization(Some(
            ApprovalUrl::parse("https://control.example/approve/token").unwrap(),
        ))),
        tailnet::Registration::Failed,
    ] {
        let dir = configuration();
        let mut manager = tailnet_menu::Manager::new(dir.path().join("tailnet.json"));
        let mut ui = ReaderUi::default();
        manager.event(TailnetEvent::Connect, &mut ui).unwrap();
        STATE.with_borrow_mut(|s| s.outcome = Some(outcome));
        assert!(manager.poll(&mut ui));
        assert_eq!(STATE.with_borrow(|s| s.drop_count), 0);
        STATE.with_borrow_mut(|s| s.joined = true);
        manager.poll(&mut ui);
        manager.poll(&mut ui);
        assert_eq!(STATE.with_borrow(|s| (s.starts, s.drop_count)), (1, 1));
    }
}

#[test]
fn clock_and_missing_configuration_fail_before_identity_creation() {
    let dir = configuration();
    let mut manager = tailnet_menu::Manager::new(dir.path().join("tailnet.json"));
    STATE.with_borrow_mut(|s| s.clock_bad = true);
    assert!(
        manager
            .event(TailnetEvent::Provision, &mut ReaderUi::default())
            .is_err()
    );
    STATE.with_borrow_mut(|s| s.clock_bad = false);
    std::fs::remove_file(dir.path().join("tailnet.json")).unwrap();
    assert!(
        manager
            .event(TailnetEvent::Provision, &mut ReaderUi::default())
            .is_err()
    );
    assert_eq!(
        STATE.with_borrow(|s| (s.starts, s.opened, s.provisioned)),
        (0, 0, 0)
    );
}

#[test]
fn approval_retry_is_bound_and_disconnect_discards_it() {
    let dir = configuration();
    let path = dir.path().join("tailnet.json");
    let original = std::fs::read(&path).unwrap();
    let mut manager = tailnet_menu::Manager::new(&path);
    let mut ui = ReaderUi::default();
    manager.event(TailnetEvent::Connect, &mut ui).unwrap();
    STATE.with_borrow_mut(|s| {
        s.outcome = Some(tailnet::Registration::Outcome(
            Outcome::AwaitingAuthorization(Some(
                ApprovalUrl::parse("https://login.example/token").unwrap(),
            )),
        ));
        s.joined = true;
    });
    manager.poll(&mut ui);
    std::fs::write(
        &path,
        String::from_utf8(original.clone())
            .unwrap()
            .replace("control.example", "changed.example"),
    )
    .unwrap();
    assert!(manager.event(TailnetEvent::Connect, &mut ui).is_err());
    assert_eq!(STATE.with_borrow(|s| s.starts), 1);
    std::fs::write(&path, original).unwrap();
    manager.event(TailnetEvent::Connect, &mut ui).unwrap();
    assert_eq!(STATE.with_borrow(|s| s.followups), 1);
    manager.event(TailnetEvent::Stop, &mut ui).unwrap();
    manager.event(TailnetEvent::Connect, &mut ui).unwrap();
    assert_eq!(STATE.with_borrow(|s| (s.starts, s.followups)), (3, 1));
    manager.close_and_wait(Duration::ZERO);
}

#[test]
fn request_handles_retain_the_stopped_generation_until_dropped() {
    let dir = configuration();
    let mut manager = tailnet_menu::Manager::new(dir.path().join("tailnet.json"));
    let mut ui = ReaderUi::default();
    assert!(manager.driver().is_none());
    manager.event(TailnetEvent::Connect, &mut ui).unwrap();
    let request = manager.driver().unwrap();
    let weak = request.downgrade();
    request
        .with(|_| {
            assert!(!manager.close_and_wait(Duration::ZERO));
            Ok(())
        })
        .unwrap();
    manager.event(TailnetEvent::Stop, &mut ui).unwrap();
    assert!(manager.driver().is_none());
    STATE.with_borrow_mut(|s| s.joined = true);
    assert!(manager.close_and_wait(Duration::ZERO));
    assert_eq!(STATE.with_borrow(|s| s.drop_count), 0);
    use femto_microlink::application_tcp::Driver;
    assert!(
        request
            .clone()
            .run::<()>(|_, _| panic!("stopped request cannot access network"))
            .is_err()
    );
    assert!(weak.upgrade().is_some());
    drop(request);
    assert!(weak.upgrade().is_none());
    assert_eq!(STATE.with_borrow(|s| s.drop_count), 1);
}

#[test]
fn platform_address_changes_are_visible_without_taking_the_runtime_lock() {
    let dir = configuration();
    let mut manager = tailnet_menu::Manager::new(dir.path().join("tailnet.json"));
    let mut ui = ReaderUi::default();
    manager.set_local_address(Some("192.168.44.9".parse().unwrap()));
    manager.event(TailnetEvent::Connect, &mut ui).unwrap();
    let driver = manager.driver().unwrap();
    driver
        .with(|runtime| {
            assert_eq!(
                runtime.local_endpoint(),
                Some("192.168.44.9:41641".parse().unwrap())
            );
            manager.set_local_address(Some("10.20.30.40".parse().unwrap()));
            assert_eq!(
                runtime.local_endpoint(),
                Some("10.20.30.40:41641".parse().unwrap())
            );
            manager.set_local_address(None);
            assert_eq!(runtime.local_endpoint(), None);
            Ok(())
        })
        .unwrap();
    STATE.with_borrow_mut(|s| s.joined = true);
    assert!(manager.close_and_wait(Duration::ZERO));
}

#[test]
fn transient_setup_report_keeps_runtime_available_until_explicit_stop() {
    let dir = configuration();
    let mut manager = tailnet_menu::Manager::new(dir.path().join("tailnet.json"));
    let mut ui = ReaderUi::default();
    manager.event(TailnetEvent::Connect, &mut ui).unwrap();
    STATE.with_borrow_mut(|s| s.outcome = Some(tailnet::Registration::Retrying));
    assert!(manager.poll(&mut ui));
    assert!(manager.driver().is_some());
    assert_eq!(
        STATE.with_borrow(|s| (s.starts, s.closes, s.drop_count)),
        (1, 0, 0)
    );
    STATE.with_borrow_mut(|s| s.joined = true);
    manager.event(TailnetEvent::Stop, &mut ui).unwrap();
    assert!(manager.driver().is_none());
    assert_eq!(STATE.with_borrow(|s| s.drop_count), 1);
}
