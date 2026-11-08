//! Host ownership behind the shared reader Tailnet menu.
use crate::host_trust::{load_service_tls, load_tls};
use femto_microlink::{
    application_tcp::ThreadShared,
    device_config::Config,
    enrollment::Outcome,
    host_registration::{self, ApplicationRuntime, Connection, Options},
    host_tcp::Driver,
    protocol::control::HostInfo,
    store::HostStore,
};
use femto_reader::ui::{ReaderUi, TailnetEvent};
use std::{
    fs::{self, File},
    io::{self, Read},
    net::SocketAddr,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};
use zeroize::Zeroizing;

type Runtime = ThreadShared<ApplicationRuntime>;
enum Report {
    Connection(Connection, femto_microlink::host_tcp_tls::Connector),
    Stopped,
}
struct Job {
    cancel: Arc<AtomicBool>,
    thread: JoinHandle<io::Result<Report>>,
}
pub struct Manager {
    directory: PathBuf,
    endpoint: SocketAddr,
    coordinator_ca: Option<PathBuf>,
    service_ca: Option<PathBuf>,
    service_tls: Option<femto_microlink::host_tcp_tls::Connector>,
    relay_ca: Option<PathBuf>,
    relay_only: bool,
    runtime: Option<Runtime>,
    job: Option<Job>,
    stopping: bool,
    rows: Vec<String>,
}
impl Manager {
    pub fn new(directory: PathBuf, endpoint: SocketAddr, coordinator_ca: Option<PathBuf>) -> Self {
        Self {
            directory,
            endpoint,
            coordinator_ca,
            service_ca: None,
            service_tls: None,
            relay_ca: None,
            relay_only: false,
            runtime: None,
            job: None,
            stopping: false,
            rows: vec!["Disconnected".into()],
        }
    }
    pub fn factory(&self) -> femto_catalog::tailnet::Factory {
        if self.stopping {
            return Default::default();
        }
        self.runtime
            .as_ref()
            .zip(self.service_tls.as_ref())
            .map(|(runtime, tls)| {
                femto_catalog::tailnet::Factory::with_tls(runtime.downgrade(), tls.clone())
            })
            .unwrap_or_default()
    }
    pub fn with_relay_options(mut self, ca: Option<PathBuf>, relay_only: bool) -> Self {
        self.relay_ca = ca;
        self.relay_only = relay_only;
        self
    }
    pub fn with_service_ca(mut self, ca: Option<PathBuf>) -> Self {
        self.service_ca = ca;
        self
    }
    fn spawn(
        &mut self,
        task: impl FnOnce(Arc<AtomicBool>) -> io::Result<Report> + Send + 'static,
    ) -> io::Result<()> {
        if self.job.is_some() {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let thread = thread::Builder::new()
            .name("femto-enrollment".into())
            .stack_size(512 * 1024)
            .spawn(move || task(worker_cancel))?;
        self.job = Some(Job { cancel, thread });
        Ok(())
    }
    fn start(&mut self, event: TailnetEvent) -> io::Result<()> {
        if self.runtime.is_some() || self.job.is_some() || self.stopping {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Disconnect and wait before connecting",
            ));
        }
        let directory = self.directory.clone();
        let endpoint = self.endpoint;
        let coordinator_ca = self.coordinator_ca.clone();
        let service_ca = self.service_ca.clone();
        let relay_ca = self.relay_ca.clone();
        let direct_udp = !self.relay_only;
        self.spawn(move |cancel| {
            let config = load(&directory.join("tailnet.json"))?;
            if config.port != endpoint.port() {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            if cancel.load(Ordering::Acquire) {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let tls = load_tls(coordinator_ca.as_deref())?;
            let service_tls = load_service_tls(service_ca.as_deref())?;
            let transport = femto_microlink::host_runtime::TransportConfig {
                relay_tls: load_tls(relay_ca.as_deref())?,
                direct_udp,
            };
            let store = open_store(
                &directory.join("identity"),
                event == TailnetEvent::Provision,
            )?;
            let options = Options {
                host: config.host.clone(),
                address: endpoint,
                pinned_key: None,
                host_info: HostInfo {
                    ipn_version: format!("femto/{}", env!("CARGO_PKG_VERSION")),
                    backend_log_id: String::new(),
                    os: std::env::consts::OS.into(),
                    os_version: String::new(),
                    device_model: "femto-simulator".into(),
                    hostname: config.hostname.clone(),
                    machine: std::env::consts::ARCH.into(),
                    architecture: std::env::consts::ARCH.into(),
                    no_logs_no_support: true,
                    userspace: true,
                    net_info: None,
                },
                auth_key: (event != TailnetEvent::Connect)
                    .then(|| config.auth_key().map(str::to_owned))
                    .flatten(),
                followup: None,
                rotate_node_key: event == TailnetEvent::Renew,
            };
            host_registration::connect_with_transport(
                store,
                options,
                "0.0.0.0:0".parse().unwrap(),
                cancel,
                tls,
                transport,
            )
            .map(|connection| Report::Connection(connection, service_tls))
        })?;
        self.rows = vec![
            if event == TailnetEvent::Renew {
                "Renewing node key"
            } else {
                "Connecting"
            }
            .into(),
        ];
        Ok(())
    }
    pub fn event(&mut self, event: TailnetEvent, ui: &mut ReaderUi) {
        ui.open_tailnet();
        match event {
            TailnetEvent::Open => (),
            TailnetEvent::Stop => {
                self.stopping = true;
                if let Some(job) = &self.job {
                    job.cancel.store(true, Ordering::Release);
                }
                self.rows = vec!["Disconnecting".into()];
            }
            event => {
                if let Err(error) = self.start(event) {
                    ui.show_error(error.to_string());
                }
            }
        }
        ui.set_tailnet_status(&self.rows);
    }
    pub fn poll(&mut self, ui: &mut ReaderUi) -> bool {
        let previous = self.rows.clone();
        if self
            .job
            .as_ref()
            .is_some_and(|job| job.thread.is_finished())
        {
            let job = self.job.take().unwrap();
            let result = job
                .thread
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("enrollment worker panicked")));
            match result {
                Ok(Report::Connection(Connection::Connected(runtime), tls)) => {
                    self.service_tls = Some(tls);
                    self.runtime = Some(Runtime::new(*runtime));
                    if !self.stopping {
                        self.rows = vec!["Registered; waiting for map".into()];
                    }
                }
                Ok(Report::Connection(Connection::NotAuthorized(outcome), _)) if !self.stopping => {
                    self.rows = match outcome {
                        Outcome::AwaitingAuthorization(url) => {
                            let mut rows = vec!["Approval required".into()];
                            if let Some(url) = url {
                                rows.push("Open on another device:".into());
                                rows.extend(
                                    url.as_str()
                                        .as_bytes()
                                        .chunks(32)
                                        .map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
                                );
                            }
                            rows.push("Connect / renew after approval".into());
                            rows
                        }
                        Outcome::Expired => vec!["Node key expired; renew".into()],
                        _ => vec!["Registration rejected".into()],
                    };
                }
                Err(_) if !self.stopping => {
                    self.rows = vec![
                        "Tailnet setup failed".into(),
                        "Check config and identity".into(),
                    ]
                }
                _ => (),
            }
        }
        if self.stopping {
            if let Some(runtime) = self.runtime.as_ref() {
                let closed = runtime.with_cleanup(|runtime| {
                    runtime.close();
                    Ok(())
                });
                if closed.is_ok() {
                    let runtime = self.runtime.take().unwrap();
                    self.service_tls = None;
                    // Joining control/relay workers happens off the UI thread.
                    if self
                        .spawn(move |_| {
                            drop(runtime);
                            Ok(Report::Stopped)
                        })
                        .is_err()
                    {
                        self.rows = vec!["Tailnet cleanup failed".into()];
                    }
                }
            }
            if self.runtime.is_none() && self.job.is_none() {
                self.stopping = false;
                self.rows = vec!["Disconnected".into()];
            }
        } else if let Some(runtime) = &mut self.runtime {
            match runtime.pump() {
                Ok(()) => match runtime.run(|network, now| network.local_address(now)) {
                    Ok(Some(address)) => {
                        self.rows = vec![
                            format!("Address: {address}"),
                            "Peer connectivity unverified".into(),
                        ]
                    }
                    Ok(None) => self.rows = vec!["Waiting for authorized map".into()],
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                    Err(_) => {
                        if runtime
                            .with(|runtime| {
                                Ok(runtime
                                    .stop_reason()
                                    .is_some_and(|reason| !reason.retryable()))
                            })
                            .unwrap_or(false)
                        {
                            self.rows = vec!["Tailnet authorization ended".into()];
                            self.stopping = true;
                        } else {
                            self.rows = vec!["Waiting for control reconnect".into()];
                        }
                    }
                },
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                Err(_) => {
                    self.rows = vec!["Tailnet connection closed".into()];
                    self.stopping = true;
                }
            }
        }
        if previous != self.rows {
            ui.set_tailnet_status(&self.rows);
            return ui.is_tailnet_open();
        }
        false
    }
}
impl Drop for Manager {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Release);
            let _ = job.thread.join();
        }
        if let Some(runtime) = self.runtime.take() {
            loop {
                match runtime.with_cleanup(|runtime| {
                    runtime.close();
                    Ok(())
                }) {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(1))
                    }
                    _ => break,
                }
            }
        }
    }
}
fn load(path: &Path) -> io::Result<Config> {
    fn check(metadata: &fs::Metadata) -> io::Result<()> {
        if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Tailnet config must be a private regular file",
            ));
        }
        Ok(())
    }
    check(&fs::symlink_metadata(path)?)?;
    let file = File::open(path)?;
    check(&file.metadata()?)?;
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(8193).read_to_end(&mut bytes)?;
    Config::parse(&bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}
fn open_store(path: &Path, provision: bool) -> io::Result<HostStore> {
    if provision {
        fs::DirBuilder::new().mode(0o700).create(path)?;
    } else {
        fs::symlink_metadata(path.join("identity.bin"))?;
    }
    HostStore::open(path).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn config() -> &'static [u8] {
        br#"{"version":1,"host":"control.example","port":443,"hostname":"femto-test","auth_key":"test-secret"}"#
    }
    #[test]
    fn coordinator_ca_is_bounded_regular_der_or_default_trust() {
        assert!(load_tls(None).is_ok());
        assert!(load_service_tls(None).is_ok());
        let root = tempfile::tempdir().unwrap();
        assert!(load_tls(Some(root.path())).is_err());
        let path = root.path().join("ca.der");
        for bytes in [
            Vec::new(),
            b"invalid certificate".to_vec(),
            vec![0; 16 * 1024 + 1],
        ] {
            fs::write(&path, bytes).unwrap();
            assert!(load_tls(Some(&path)).is_err());
            assert!(load_service_tls(Some(&path)).is_err());
        }
        let link = root.path().join("ca-link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(load_tls(Some(&link)).is_err());
    }
    #[test]
    fn private_configuration_and_explicit_identity_creation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("tailnet.json");
        fs::write(&path, config()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            load(&path).err().unwrap().kind(),
            io::ErrorKind::PermissionDenied
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(load(&path).unwrap().hostname, "femto-test");
        let link = root.path().join("config-link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(load(&link).is_err());
        fs::write(&path, vec![b' '; 8193]).unwrap();
        assert_eq!(
            load(&path).err().unwrap().kind(),
            io::ErrorKind::InvalidInput
        );
        let identity = root.path().join("identity");
        assert!(open_store(&identity, false).is_err());
        assert!(!identity.exists());
        let store = open_store(&identity, true).unwrap();
        let node = store.identity().unwrap().node_key().public();
        drop(store);
        assert!(open_store(&identity, true).is_err());
        assert_eq!(
            open_store(&identity, false)
                .unwrap()
                .identity()
                .unwrap()
                .node_key()
                .public(),
            node
        );
    }
    #[test]
    fn pending_cancel_is_nonblocking_discards_approval_and_allows_retry() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::new(root.path().into(), "127.0.0.1:443".parse().unwrap(), None);
        let mut ui = ReaderUi::default();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        manager
            .spawn(move |cancel| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                assert!(cancel.load(Ordering::Acquire));
                Ok(Report::Connection(
                    Connection::NotAuthorized(Outcome::AwaitingAuthorization(Some(
                        femto_microlink::enrollment::ApprovalUrl::parse(
                            "https://control.example/token",
                        )
                        .unwrap(),
                    ))),
                    femto_microlink::host_tcp_tls::Connector::mozilla().unwrap(),
                ))
            })
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            manager.start(TailnetEvent::Connect).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        manager.event(TailnetEvent::Stop, &mut ui);
        assert!(!manager.poll(&mut ui));
        assert!(manager.stopping);
        assert!(manager.job.is_some());
        assert!(manager.factory().transport().is_err());
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while manager.job.is_some() {
            assert!(Instant::now() < deadline);
            manager.poll(&mut ui);
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(manager.rows, ["Disconnected"]);
        assert!(!manager.stopping);
        manager.spawn(|_| Ok(Report::Stopped)).unwrap();
    }
    #[test]
    fn dropping_manager_cancels_and_joins_setup_worker() {
        let root = tempfile::tempdir().unwrap();
        let mut manager = Manager::new(root.path().into(), "127.0.0.1:443".parse().unwrap(), None);
        let finished = Arc::new(AtomicBool::new(false));
        let worker_finished = finished.clone();
        manager
            .spawn(move |cancel| {
                while !cancel.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(1));
                }
                worker_finished.store(true, Ordering::Release);
                Ok(Report::Stopped)
            })
            .unwrap();
        drop(manager);
        assert!(finished.load(Ordering::Acquire));
    }
}
