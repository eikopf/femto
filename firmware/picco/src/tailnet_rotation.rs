//! Background key renewal with thread-local NVS ownership and joined teardown.
use femto_microlink::{
    control_session::Endpoint,
    device_config::Config,
    device_enrollment::{Context, Followup},
    enrollment::Outcome,
    protocol::{CAPABILITY_VERSION, control::HostInfo},
};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub struct Report {
    pub outcome: Outcome,
    pub followup: Option<Followup>,
}
pub struct Job {
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    result: Receiver<Result<Report, ()>>,
}
impl Job {
    pub fn start(config: Config, followup: Option<Followup>) -> Result<Self, String> {
        config.validate()?;
        crate::clock::require_ready().map_err(|e| e.to_string())?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel = cancelled.clone();
        let (sender, result) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("femto-renew".into())
            .stack_size(128 * 1024)
            .spawn(move || {
                let report = renew(config, followup, cancel).map_err(|_| ());
                let _ = sender.try_send(report);
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            cancelled,
            worker: Some(worker),
            result,
        })
    }
    pub fn poll(&mut self) -> Option<Result<Report, ()>> {
        match self.result.try_recv() {
            Ok(report) => Some(report),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(())),
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }
    pub fn close_and_wait(&mut self, timeout: Duration) -> bool {
        self.cancelled.store(true, Ordering::Release);
        let started = Instant::now();
        while self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            if started.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(
                Duration::from_millis(5).min(timeout.saturating_sub(started.elapsed())),
            );
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        true
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        while !self.close_and_wait(Duration::from_secs(1)) {}
    }
}

fn renew(
    config: Config,
    followup: Option<Followup>,
    cancelled: Arc<AtomicBool>,
) -> io::Result<Report> {
    if cancelled.load(Ordering::Acquire) {
        return Err(io::ErrorKind::Interrupted.into());
    }
    crate::clock::require_ready()?;
    let mut store = crate::identity::open().map_err(io::Error::other)?;
    let pending = store
        .begin_node_key_rotation(&config.host, config.port)
        .map_err(io::Error::other)?
        .clone();
    let context = Context::for_rotation(&config, &pending).map_err(io::Error::other)?;
    if let Some(followup) = &followup {
        context.validate(followup).map_err(io::Error::other)?;
    }
    let identity = pending.candidate();
    let control_key = crate::control::fetch_control_key(
        &config.host,
        config.port,
        CAPABILITY_VERSION,
        Instant::now() + Duration::from_secs(15),
        cancelled.clone(),
    )?;
    let endpoint = Endpoint {
        host: config.host.clone(),
        port: config.port,
        control_key,
        version: CAPABILITY_VERSION,
    };
    let mut control = crate::control::connect_session(
        &endpoint,
        identity.machine_key(),
        Duration::from_secs(60),
        cancelled,
    )?;
    let mut host = HostInfo::esp32(&config.hostname, "");
    host.ipn_version = format!("femto/{}", env!("CARGO_PKG_VERSION"));
    host.device_model = "BOOX Picco".into();
    let request = context
        .registration(
            identity.network_lock_key().public(),
            host,
            config.auth_key(),
            followup.as_ref(),
        )
        .map_err(io::Error::other)?;
    let outcome = control.register_rotation(&pending, &request, |authorized| {
        crate::clock::require_ready()?;
        store
            .complete_node_key_rotation(&config.host, config.port, authorized)
            .map_err(io::Error::other)
    })?;
    control.close();
    let followup = match &outcome {
        Outcome::AwaitingAuthorization(Some(url)) => Some(context.followup(url.clone())),
        _ => None,
    };
    Ok(Report { outcome, followup })
}
