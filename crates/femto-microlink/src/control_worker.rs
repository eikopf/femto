//! Portable control reader with bounded handoff and serialized authorization closure.
use crate::{
    control_session::ControlSession,
    coordinator::Clock,
    derp::DeadlineIo,
    map_session::{MapTarget, Progress},
    network::Network,
    protocol::control::MapRequest,
    timestamp::EpochStore,
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::JoinHandle,
};
const MAP_QUEUE: usize = 8;
const MAP_QUEUE_BYTES: usize = 2 * crate::control_stream::MAX_MAP_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    StreamClosed,
    SetupFailed(std::io::ErrorKind),
    StreamFailed(std::io::ErrorKind),
    Overloaded,
    Cancelled,
    InvalidMap,
    Poisoned,
}
impl StopReason {
    /// A transport retry candidate, not permission to bypass registration or
    /// backoff. Inspect only after the worker finishes and teardown completes.
    pub fn retryable(self) -> bool {
        use std::io::ErrorKind;
        matches!(
            self,
            Self::StreamClosed
                | Self::SetupFailed(
                    ErrorKind::TimedOut
                        | ErrorKind::UnexpectedEof
                        | ErrorKind::ConnectionReset
                        | ErrorKind::ConnectionRefused
                        | ErrorKind::BrokenPipe
                        | ErrorKind::NotConnected
                        | ErrorKind::NetworkUnreachable
                        | ErrorKind::HostUnreachable
                )
                | Self::StreamFailed(
                    ErrorKind::TimedOut
                        | ErrorKind::UnexpectedEof
                        | ErrorKind::ConnectionReset
                        | ErrorKind::ConnectionRefused
                        | ErrorKind::BrokenPipe
                        | ErrorKind::NotConnected
                        | ErrorKind::NetworkUnreachable
                        | ErrorKind::HostUnreachable
                )
        )
    }
}
/// Caller-owned retry pacing for one control owner. A new value starts a new
/// retry budget; retain it across attempts and ordinary transport failures.
#[derive(Default, Debug)]
pub struct Reconnect {
    attempts: u8,
    ready_at: Option<u64>,
    healthy_since: Option<u64>,
    last_ms: Option<u64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconnectProgress {
    Active,
    Draining,
    Waiting { retry_at_ms: u64 },
    Restarted { attempt: u8 },
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("control reconnect attempts exhausted")]
    ReconnectExhausted,
    #[error("control reconnect clock moved backwards")]
    ClockRollback,
    #[error("control reconnect deadline overflow")]
    ClockOverflow,
    #[error("control worker stopped: {0:?}")]
    Stopped(StopReason),
    #[error(transparent)]
    Network(#[from] crate::network::Error),
    #[error(transparent)]
    Spawn(#[from] std::io::Error),
}
type State = Arc<Mutex<Option<StopReason>>>;
type ReaderWorker = (Receiver<QueuedMap>, State, JoinHandle<()>);
struct QueuedMap {
    bytes: Vec<u8>,
    retained: Arc<AtomicUsize>,
}
impl Drop for QueuedMap {
    fn drop(&mut self) {
        self.retained.fetch_sub(self.bytes.len(), Ordering::AcqRel);
    }
}
fn stop(state: &State, reason: StopReason) {
    if let Ok(mut state) = state.lock() {
        state.get_or_insert(reason);
    }
}
fn failure(state: &State, reason: StopReason) {
    if let Ok(mut state) = state.lock()
        && (state.is_none() || *state == Some(StopReason::StreamClosed))
    {
        *state = Some(reason);
    }
}
struct Sink {
    sender: SyncSender<QueuedMap>,
    retained: Arc<AtomicUsize>,
    state: State,
    initial_reset: bool,
}
impl MapTarget for Sink {
    type Error = Error;
    fn apply(&mut self, bytes: &[u8], _: Clock) -> Result<(), Error> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| Error::Stopped(StopReason::Poisoned))?;
        if let Some(reason) = *state {
            return Err(Error::Stopped(reason));
        }
        if bytes.len() > crate::control_stream::MAX_MAP_BYTES {
            *state = Some(StopReason::InvalidMap);
            return Err(Error::Stopped(StopReason::InvalidMap));
        }
        // This sink is the sole producer. The consumer can only reduce the
        // retained count; failed sends and drained frames release it on drop.
        if self.retained.load(Ordering::Acquire) + bytes.len() > MAP_QUEUE_BYTES {
            *state = Some(StopReason::Overloaded);
            return Err(Error::Stopped(StopReason::Overloaded));
        }
        let bytes = bytes.to_vec();
        self.retained.fetch_add(bytes.len(), Ordering::AcqRel);
        match self.sender.try_send(QueuedMap {
            bytes,
            retained: self.retained.clone(),
        }) {
            Ok(()) => Ok(()),
            Err(error) => {
                let reason = match error {
                    TrySendError::Full(_) => StopReason::Overloaded,
                    TrySendError::Disconnected(_) => StopReason::Cancelled,
                };
                *state = Some(reason);
                Err(Error::Stopped(reason))
            }
        }
    }
    fn disconnect(&mut self) {
        // MapSession resets its target once on construction. This sink starts
        // without authorization; later disconnects always terminate the lease.
        if self.initial_reset {
            self.initial_reset = false;
        } else {
            stop(&self.state, StopReason::StreamClosed);
        }
    }
}
impl Drop for Sink {
    fn drop(&mut self) {
        if std::thread::panicking() {
            failure(&self.state, StopReason::Poisoned);
        } else {
            stop(&self.state, StopReason::StreamClosed);
        }
    }
}

/// Owns the only network access path while a dedicated thread reads control.
/// All operations run inside `run`, after queued maps and closure checks.
pub struct ControlledNetwork<S: EpochStore> {
    network: Network<S>,
    receiver: Receiver<QueuedMap>,
    state: State,
    cancelled: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    explicitly_closed: bool,
    /// Why the last map or tick was rejected, for diagnostics only.
    rejection: Option<String>,
}
impl<S: EpochStore> ControlledNetwork<S> {
    /// The supplied control connection must already be registered. Failure to
    /// start/read the stream is reported by the next `run` boundary.
    pub fn start<T: DeadlineIo + Send + 'static>(
        control: ControlSession<T>,
        request: MapRequest,
        network: Network<S>,
    ) -> Result<Self, Error> {
        let cancelled = control.cancellation();
        Self::spawn(network, cancelled, None, move |_| Ok((control, request)))
    }
    /// Creates and registers the connection inside the worker. The stream need
    /// not be Send; the setup closure must observe the supplied cancellation.
    /// Stack size is provisional until measured on the target.
    pub fn start_with<T: DeadlineIo + 'static>(
        network: Network<S>,
        cancelled: Arc<AtomicBool>,
        stack_bytes: usize,
        setup: impl FnOnce(Arc<AtomicBool>) -> std::io::Result<(ControlSession<T>, MapRequest)>
        + Send
        + 'static,
    ) -> Result<Self, Error> {
        if !(64 * 1024..=2 * 1024 * 1024).contains(&stack_bytes) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid control worker stack size",
            )
            .into());
        }
        Self::spawn(network, cancelled, Some(stack_bytes), setup)
    }
    fn spawn<T: DeadlineIo + 'static>(
        mut network: Network<S>,
        cancelled: Arc<AtomicBool>,
        stack_bytes: Option<usize>,
        setup: impl FnOnce(Arc<AtomicBool>) -> std::io::Result<(ControlSession<T>, MapRequest)>
        + Send
        + 'static,
    ) -> Result<Self, Error> {
        network.disconnect();
        let (receiver, state, worker) = Self::spawn_reader(cancelled.clone(), stack_bytes, setup)?;
        Ok(Self {
            network,
            receiver,
            state,
            cancelled,
            worker: Some(worker),
            explicitly_closed: false,
            rejection: None,
        })
    }
    fn spawn_reader<T: DeadlineIo + 'static>(
        cancelled: Arc<AtomicBool>,
        stack_bytes: Option<usize>,
        setup: impl FnOnce(Arc<AtomicBool>) -> std::io::Result<(ControlSession<T>, MapRequest)>
        + Send
        + 'static,
    ) -> Result<ReaderWorker, Error> {
        let (sender, receiver) = mpsc::sync_channel(MAP_QUEUE);
        let state = Arc::new(Mutex::new(None));
        let worker_cancel = cancelled.clone();
        let worker_state = state.clone();
        let mut sink = Sink {
            sender,
            retained: Arc::new(AtomicUsize::new(0)),
            state: state.clone(),
            initial_reset: true,
        };
        let mut builder = std::thread::Builder::new().name("femto-control".into());
        if let Some(bytes) = stack_bytes {
            builder = builder.stack_size(bytes);
        }
        let worker = builder.spawn(move || {
            if worker_cancel.load(Ordering::Acquire) {
                stop(&worker_state, StopReason::Cancelled);
                return;
            }
            let (mut control, request) = match setup(worker_cancel.clone()) {
                Ok(session) => session,
                Err(error) => {
                    failure(&worker_state, StopReason::SetupFailed(error.kind()));
                    return;
                }
            };
            if !Arc::ptr_eq(&worker_cancel, &control.cancellation())
                || worker_cancel.load(Ordering::Acquire)
            {
                stop(&worker_state, StopReason::Cancelled);
                control.close();
                return;
            }
            let error = match control.start_map(&request, &mut sink) {
                Ok(mut stream) => loop {
                    // The sink only frames and transfers authenticated bytes.
                    // The network owner samples real clocks when applying them.
                    match stream.next(|| Clock {
                        unix_ms: 0,
                        monotonic_ms: 0,
                    }) {
                        Ok(Progress::Ended) => break None,
                        Err(error) => break Some(error),
                        Ok(_) => (),
                    }
                },
                Err(error) => Some(error),
            };
            if let Some(error) = error {
                failure(&worker_state, StopReason::StreamFailed(error.kind()));
            }
        })?;
        Ok((receiver, state, worker))
    }
    /// Restarts control setup on the same disconnected network owner. The old
    /// reader must have finished; otherwise cancellation is requested and a
    /// WouldBlock error retains it for a later retry. Use a fresh cancellation
    /// flag. Registration and a new full map are required before authorization.
    pub fn restart_with<T: DeadlineIo + 'static>(
        &mut self,
        cancelled: Arc<AtomicBool>,
        stack_bytes: usize,
        setup: impl FnOnce(Arc<AtomicBool>) -> std::io::Result<(ControlSession<T>, MapRequest)>
        + Send
        + 'static,
    ) -> Result<(), Error> {
        if !(64 * 1024..=2 * 1024 * 1024).contains(&stack_bytes)
            || Arc::ptr_eq(&cancelled, &self.cancelled)
            || cancelled.load(Ordering::Acquire)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "restart requires a valid stack and fresh uncancelled flag",
            )
            .into());
        }
        self.close();
        if !self.worker_finished() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "previous control worker is still running",
            )
            .into());
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        let (receiver, state, worker) =
            Self::spawn_reader(cancelled.clone(), Some(stack_bytes), setup)?;
        self.receiver = receiver;
        self.state = state;
        self.cancelled = cancelled;
        self.worker = Some(worker);
        self.explicitly_closed = false;
        Ok(())
    }
    /// Pumps maps and starts paced reconnect attempts after transient failures.
    /// The caller must cancel and join old relay/publication workers before
    /// setting `teardown_complete`. The lazy factory runs only for an attempt;
    /// its setup closure performs fresh authenticated setup in the new worker.
    pub fn reconnect_with<T, F>(
        &mut self,
        retry: &mut Reconnect,
        clock: Clock,
        teardown_complete: bool,
        stack_bytes: usize,
        setup: impl FnOnce() -> F,
    ) -> Result<ReconnectProgress, Error>
    where
        T: DeadlineIo + 'static,
        F: FnOnce(Arc<AtomicBool>) -> std::io::Result<(ControlSession<T>, MapRequest)>
            + Send
            + 'static,
    {
        if self.explicitly_closed {
            return Err(Error::Stopped(StopReason::Cancelled));
        }
        let now = clock.monotonic_ms;
        if retry.last_ms.is_some_and(|last| now < last) {
            self.close();
            return Err(Error::ClockRollback);
        }
        retry.last_ms = Some(now);
        if self.stop_reason().is_none() {
            match self.run(|| clock, |network| network.local_address(now)) {
                Ok(address) => {
                    retry.ready_at = None;
                    if address.is_some() {
                        let since = retry.healthy_since.get_or_insert(now);
                        if now - *since >= 30_000 {
                            retry.attempts = 0;
                        }
                    } else {
                        retry.healthy_since = None;
                    }
                    return Ok(ReconnectProgress::Active);
                }
                Err(error) if self.stop_reason().is_none() => return Err(error),
                Err(_) => {}
            }
        }
        self.network.disconnect();
        retry.healthy_since = None;
        if !self.worker_finished() {
            return Ok(ReconnectProgress::Draining);
        }
        let reason = self.stop_reason().unwrap_or(StopReason::StreamClosed);
        if !reason.retryable() {
            return Err(Error::Stopped(reason));
        }
        if retry.attempts >= 8 {
            return Err(Error::ReconnectExhausted);
        }
        let delay = (1000u64 << retry.attempts).min(60_000);
        let deadline = if let Some(deadline) = retry.ready_at {
            deadline
        } else {
            let Some(deadline) = now.checked_add(delay) else {
                self.close();
                return Err(Error::ClockOverflow);
            };
            retry.ready_at = Some(deadline);
            deadline
        };
        if now < deadline {
            return Ok(ReconnectProgress::Waiting {
                retry_at_ms: deadline,
            });
        }
        if !teardown_complete {
            return Ok(ReconnectProgress::Draining);
        }
        retry.attempts += 1;
        retry.ready_at = None;
        self.restart_with(Arc::new(AtomicBool::new(false)), stack_bytes, setup())?;
        Ok(ReconnectProgress::Restarted {
            attempt: retry.attempts,
        })
    }

    /// The operation must be bounded and nonblocking. Holding the shared lock
    /// serializes sending/releasing packets with stream closure and map arrival.
    /// No borrowed network reference can escape this callback.
    pub fn run<R>(
        &mut self,
        mut clock: impl FnMut() -> Clock,
        operation: impl FnOnce(&mut Network<S>) -> Result<R, crate::network::Error>,
    ) -> Result<R, Error> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(_) => {
                self.cancelled.store(true, Ordering::Release);
                self.network.disconnect();
                return Err(Error::Stopped(StopReason::Poisoned));
            }
        };
        if self.cancelled.load(Ordering::Acquire) {
            state.get_or_insert(StopReason::Cancelled);
        }
        if let Some(reason) = *state {
            self.network.disconnect();
            return Err(Error::Stopped(reason));
        }
        for _ in 0..MAP_QUEUE {
            match self.receiver.try_recv() {
                Ok(bytes) => {
                    if let Err(error) = self.network.apply_map(&bytes.bytes, clock()) {
                        self.rejection = Some(format!("{} byte map: {error}", bytes.bytes.len()));
                        *state = Some(StopReason::InvalidMap);
                        self.cancelled.store(true, Ordering::Release);
                        self.network.disconnect();
                        return Err(error.into());
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    *state = Some(StopReason::StreamClosed);
                    self.network.disconnect();
                    return Err(Error::Stopped(StopReason::StreamClosed));
                }
            }
        }
        if let Err(error) = self.network.tick(clock().monotonic_ms) {
            self.rejection = Some(format!("tick: {error}"));
            *state = Some(StopReason::InvalidMap);
            self.cancelled.store(true, Ordering::Release);
            self.network.disconnect();
            return Err(error.into());
        }
        Ok(operation(&mut self.network)?)
    }
    /// Cancellation is irreversible for this worker. TLS reads observe the flag on their
    /// bounded polling interval; this call does not wait for the reader thread.
    pub fn close(&mut self) {
        self.explicitly_closed = true;
        stop(&self.state, StopReason::Cancelled);
        self.cancelled.store(true, Ordering::Release);
        self.network.disconnect();
    }
    /// Retains a blocked worker on timeout. Call before releasing device drivers.
    pub fn close_and_wait(&mut self, timeout: std::time::Duration) -> bool {
        self.close();
        let started = std::time::Instant::now();
        while !self.worker_finished() {
            if started.elapsed() >= timeout {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        true
    }
    /// The final stop reason is stable once the reader has finished. A pending
    /// cancellation may be observed before the reader reports its I/O result.
    /// The detail behind [`StopReason::InvalidMap`]; never map contents.
    pub fn rejection(&self) -> Option<&str> {
        self.rejection.as_deref()
    }
    pub fn stop_reason(&self) -> Option<StopReason> {
        match self.state.lock() {
            Ok(state) => (*state).or_else(|| {
                self.cancelled
                    .load(Ordering::Acquire)
                    .then_some(StopReason::Cancelled)
            }),
            Err(_) => Some(StopReason::Poisoned),
        }
    }
    pub fn worker_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }
}
impl<S: EpochStore> Drop for ControlledNetwork<S> {
    fn drop(&mut self) {
        self.close();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        protocol::{
            identity::DeviceIdentity,
            key::{Node, PublicKey},
        },
        timestamp::TimestampClock,
    };
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
    struct LocalIo(std::rc::Rc<()>);
    impl std::io::Read for LocalIo {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Ok(0)
        }
    }
    impl std::io::Write for LocalIo {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl DeadlineIo for LocalIo {
        fn set_deadline(&mut self, _: std::time::Instant) {
            let _ = std::rc::Rc::strong_count(&self.0);
        }
    }
    fn empty_network() -> Network<Store> {
        Network::new(
            &DeviceIdentity::generate().unwrap(),
            TimestampClock::open(Store, 100).unwrap(),
            4,
            2,
        )
        .unwrap()
    }
    fn clock_at(monotonic_ms: u64) -> Clock {
        Clock {
            monotonic_ms,
            ..clock()
        }
    }
    fn finish_worker(owner: &ControlledNetwork<Store>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !owner.worker_finished() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn reconnect_attempts_are_paced_bounded_and_wait_for_teardown() {
        let (mut owner, sink, _, _) = pair();
        drop(sink);
        let mut retry = Reconnect::default();
        let mut now = 100;
        for attempt in 1..=8 {
            let delay = (1000u64 << (attempt - 1)).min(60_000);
            let deadline = now + delay;
            for time in [now, deadline - 1] {
                assert_eq!(
                    owner
                        .reconnect_with::<LocalIo, _>(
                            &mut retry,
                            clock_at(time),
                            true,
                            128 * 1024,
                            || |_| panic!("setup ran before deadline")
                        )
                        .unwrap(),
                    ReconnectProgress::Waiting {
                        retry_at_ms: deadline
                    }
                );
            }
            assert_eq!(
                owner
                    .reconnect_with::<LocalIo, _>(
                        &mut retry,
                        clock_at(deadline),
                        false,
                        128 * 1024,
                        || |_| panic!("setup ran before teardown")
                    )
                    .unwrap(),
                ReconnectProgress::Draining
            );
            let (tx, rx) = mpsc::channel();
            assert_eq!(
                owner
                    .reconnect_with::<LocalIo, _>(
                        &mut retry,
                        clock_at(deadline),
                        true,
                        128 * 1024,
                        || move |_| {
                            tx.send(()).unwrap();
                            Err(std::io::ErrorKind::TimedOut.into())
                        }
                    )
                    .unwrap(),
                ReconnectProgress::Restarted { attempt }
            );
            rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
            finish_worker(&owner);
            assert_eq!(
                owner.stop_reason(),
                Some(StopReason::SetupFailed(std::io::ErrorKind::TimedOut))
            );
            assert_eq!(owner.network.local_address(deadline).unwrap(), None);
            now = deadline + 1;
        }
        assert!(matches!(
            owner.reconnect_with::<LocalIo, _>(&mut retry, clock_at(now), true, 128 * 1024, || {
                |_| panic!("retry budget exceeded")
            }),
            Err(Error::ReconnectExhausted)
        ));
    }

    #[test]
    fn reconnect_requires_sustained_authorization_before_resetting_budget() {
        let (mut owner, mut sink, map, _) = pair();
        let mut retry = Reconnect {
            attempts: 7,
            ready_at: Some(0),
            ..Default::default()
        };
        assert_eq!(
            owner
                .reconnect_with::<LocalIo, _>(
                    &mut retry,
                    clock_at(0),
                    true,
                    128 * 1024,
                    || |_| panic!("active setup replaced")
                )
                .unwrap(),
            ReconnectProgress::Active
        );
        assert!(retry.healthy_since.is_none());
        assert!(retry.ready_at.is_none());
        sink.apply(&map, clock()).unwrap();
        for time in [1, 30_000, 30_001] {
            assert_eq!(
                owner
                    .reconnect_with::<LocalIo, _>(
                        &mut retry,
                        clock_at(time),
                        true,
                        128 * 1024,
                        || |_| panic!("authorized setup replaced")
                    )
                    .unwrap(),
                ReconnectProgress::Active
            );
            assert_eq!(retry.attempts, if time == 30_001 { 0 } else { 7 });
        }
        drop(sink);
        assert_eq!(
            owner
                .reconnect_with::<LocalIo, _>(
                    &mut retry,
                    clock_at(30_002),
                    true,
                    128 * 1024,
                    || |_| panic!("retry was not paced")
                )
                .unwrap(),
            ReconnectProgress::Waiting {
                retry_at_ms: 31_002
            }
        );
    }

    #[test]
    fn reconnect_cancellation_terminal_failure_and_bad_clocks_never_start_setup() {
        let (mut owner, sink, _, _) = pair();
        drop(sink);
        let mut retry = Reconnect::default();
        owner
            .reconnect_with::<LocalIo, _>(&mut retry, clock_at(100), true, 128 * 1024, || {
                |_| panic!("early setup")
            })
            .unwrap();
        owner.close();
        // Explicit close must stop retries even when an earlier stream failure exists.
        assert!(matches!(
            owner.reconnect_with::<LocalIo, _>(
                &mut retry,
                clock_at(1100),
                true,
                128 * 1024,
                || |_| panic!("cancelled retry")
            ),
            Err(Error::Stopped(StopReason::Cancelled))
        ));
        let (mut owner, mut sink, _, _) = pair();
        sink.apply(b"invalid", clock()).unwrap();
        assert!(matches!(
            owner.reconnect_with::<LocalIo, _>(
                &mut Reconnect::default(),
                clock_at(0),
                true,
                128 * 1024,
                || |_| panic!("invalid map retried")
            ),
            Err(Error::Stopped(StopReason::InvalidMap))
        ));
        let (mut owner, sink, _, _) = pair();
        drop(sink);
        let mut retry = Reconnect::default();
        owner
            .reconnect_with::<LocalIo, _>(&mut retry, clock_at(100), true, 128 * 1024, || {
                |_| panic!("early setup")
            })
            .unwrap();
        assert!(matches!(
            owner.reconnect_with::<LocalIo, _>(&mut retry, clock_at(99), true, 128 * 1024, || {
                |_| panic!("clock rollback retried")
            }),
            Err(Error::ClockRollback)
        ));
        let (mut owner, sink, _, _) = pair();
        drop(sink);
        assert!(matches!(
            owner.reconnect_with::<LocalIo, _>(
                &mut Reconnect::default(),
                clock_at(u64::MAX),
                true,
                128 * 1024,
                || |_| panic!("overflow retried")
            ),
            Err(Error::ClockOverflow)
        ));
    }

    #[test]
    fn setup_stop_reasons_preserve_transport_and_terminal_failures() {
        use std::{
            io::ErrorKind,
            time::{Duration, Instant},
        };
        for kind in [
            ErrorKind::TimedOut,
            ErrorKind::UnexpectedEof,
            ErrorKind::ConnectionReset,
            ErrorKind::PermissionDenied,
            ErrorKind::InvalidData,
            ErrorKind::InvalidInput,
            ErrorKind::Other,
        ] {
            let mut owner = ControlledNetwork::start_with::<LocalIo>(
                empty_network(),
                Arc::new(AtomicBool::new(false)),
                128 * 1024,
                move |_| Err(std::io::Error::new(kind, "injected setup failure")),
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !owner.worker_finished() {
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(owner.stop_reason(), Some(StopReason::SetupFailed(kind)));
            assert!(
                matches!(owner.run::<()>(clock, |_| panic!("failed setup accessed network")),
                Err(Error::Stopped(StopReason::SetupFailed(actual))) if actual == kind)
            );
            assert_eq!(
                owner.stop_reason().unwrap().retryable(),
                matches!(
                    kind,
                    ErrorKind::TimedOut | ErrorKind::UnexpectedEof | ErrorKind::ConnectionReset
                )
            );
        }
    }

    #[test]
    fn stream_failure_refines_closure_but_not_cancellation_or_invalid_maps() {
        use std::io::ErrorKind;
        for prior in [
            None,
            Some(StopReason::StreamClosed),
            Some(StopReason::Cancelled),
            Some(StopReason::InvalidMap),
            Some(StopReason::Overloaded),
            Some(StopReason::Poisoned),
        ] {
            let state = Arc::new(Mutex::new(prior));
            failure(&state, StopReason::StreamFailed(ErrorKind::TimedOut));
            let expected = match prior {
                None | Some(StopReason::StreamClosed) => {
                    Some(StopReason::StreamFailed(ErrorKind::TimedOut))
                }
                value => value,
            };
            assert_eq!(*state.lock().unwrap(), expected);
        }
        for kind in [
            ErrorKind::PermissionDenied,
            ErrorKind::InvalidData,
            ErrorKind::InvalidInput,
            ErrorKind::Other,
            ErrorKind::Interrupted,
            ErrorKind::ConnectionAborted,
        ] {
            assert!(!StopReason::StreamFailed(kind).retryable());
        }
        for kind in [
            ErrorKind::TimedOut,
            ErrorKind::UnexpectedEof,
            ErrorKind::ConnectionReset,
            ErrorKind::ConnectionRefused,
            ErrorKind::BrokenPipe,
            ErrorKind::NotConnected,
            ErrorKind::NetworkUnreachable,
            ErrorKind::HostUnreachable,
        ] {
            assert!(StopReason::StreamFailed(kind).retryable());
        }
    }

    #[test]
    fn setup_panic_is_terminal_instead_of_a_reconnect_candidate() {
        use std::time::{Duration, Instant};
        let owner = ControlledNetwork::start_with::<LocalIo>(
            empty_network(),
            Arc::new(AtomicBool::new(false)),
            128 * 1024,
            |_| panic!("injected setup panic"),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !owner.worker_finished() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(owner.stop_reason(), Some(StopReason::Poisoned));
        assert!(!owner.stop_reason().unwrap().retryable());
    }

    #[test]
    fn external_cancellation_closes_before_queued_map_or_application_access() {
        let (mut owner, mut sink, map, peer) = pair();
        sink.apply(&map, clock()).unwrap();
        owner
            .run(clock, |network| {
                network.probe(peer, 0)?;
                Ok(())
            })
            .unwrap();
        sink.apply(&map, clock()).unwrap();
        owner.cancelled.store(true, Ordering::Release);
        assert!(matches!(
            owner.run::<()>(clock, |_| panic!("cancelled operation ran")),
            Err(Error::Stopped(StopReason::Cancelled))
        ));
        assert_eq!(owner.network.local_address(0).unwrap(), None);
        assert!(
            !owner
                .network
                .flush_one(0, |_, _, _| panic!("cancelled output escaped"))
                .unwrap()
        );
    }

    #[test]
    fn restart_discards_old_handoff_and_requires_fresh_authorization() {
        use std::time::Duration;
        let (mut owner, mut sink, map, peer) = pair();
        sink.apply(&map, clock()).unwrap();
        owner
            .run(clock, |network| {
                network.probe(peer, 0)?;
                Ok(())
            })
            .unwrap();
        sink.apply(&map, clock()).unwrap();
        let old_cancel = owner.cancelled.clone();
        let old_state = owner.state.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        owner
            .restart_with::<LocalIo>(cancel.clone(), 128 * 1024, move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Err(std::io::Error::other("injected registration failure"))
            })
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(old_cancel.load(Ordering::Acquire));
        assert!(!Arc::ptr_eq(&old_state, &owner.state));
        assert!(Arc::ptr_eq(&cancel, &owner.cancelled));
        assert!(sink.apply(&map, clock()).is_err());
        drop(sink);
        owner
            .run(clock, |network| {
                assert_eq!(network.local_address(0)?, None);
                assert!(!network.flush_one(0, |_, _, _| panic!("old ciphertext escaped"))?);
                Ok(())
            })
            .unwrap();
        release_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !owner.worker_finished() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(matches!(
            owner.run::<()>(clock, |_| panic!("failed restart authorized access")),
            Err(Error::Stopped(StopReason::SetupFailed(
                std::io::ErrorKind::Other
            )))
        ));
    }

    #[test]
    fn restart_never_overlaps_workers_or_reuses_cancellation_flags() {
        use std::time::Duration;
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut owner = ControlledNetwork::start_with::<LocalIo>(
            empty_network(),
            Arc::new(AtomicBool::new(false)),
            128 * 1024,
            move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Err(std::io::Error::other("injected failure"))
            },
        )
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        for cancel in [owner.cancelled.clone(), Arc::new(AtomicBool::new(true))] {
            assert!(
                matches!(owner.restart_with::<LocalIo>(cancel, 128 * 1024, |_| panic!("invalid restart")),
                Err(Error::Spawn(e)) if e.kind() == std::io::ErrorKind::InvalidInput)
            );
        }
        assert!(
            matches!(owner.restart_with::<LocalIo>(Arc::new(AtomicBool::new(false)), 1, |_| panic!("invalid stack")),
            Err(Error::Spawn(e)) if e.kind() == std::io::ErrorKind::InvalidInput)
        );
        assert!(!owner.cancelled.load(Ordering::Acquire));
        assert!(
            matches!(owner.restart_with::<LocalIo>(Arc::new(AtomicBool::new(false)), 128 * 1024, |_| panic!("overlapping worker")),
            Err(Error::Spawn(e)) if e.kind() == std::io::ErrorKind::WouldBlock)
        );
        assert!(owner.cancelled.load(Ordering::Acquire));
        assert!(!owner.worker_finished());
        release_tx.send(()).unwrap();
        assert!(owner.close_and_wait(Duration::from_secs(2)));
        let (retry_tx, retry_rx) = mpsc::channel();
        owner
            .restart_with::<LocalIo>(Arc::new(AtomicBool::new(false)), 128 * 1024, move |_| {
                retry_tx.send(()).unwrap();
                Err(std::io::Error::other("new setup attempted"))
            })
            .unwrap();
        retry_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(owner.close_and_wait(Duration::from_secs(2)));
    }

    #[test]
    fn thread_local_setup_and_timed_shutdown_retain_blocked_worker() {
        use std::time::Duration;
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let mut owner = ControlledNetwork::start_with::<LocalIo>(
            empty_network(),
            Arc::new(AtomicBool::new(false)),
            128 * 1024,
            move |_| {
                let _local = LocalIo(std::rc::Rc::new(()));
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Err(std::io::Error::other("injected setup failure"))
            },
        )
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!owner.close_and_wait(Duration::ZERO));
        assert!(!owner.worker_finished());
        assert!(matches!(
            owner.run::<()>(clock, |_| panic!("cancelled owner released network")),
            Err(Error::Stopped(StopReason::Cancelled))
        ));
        release_tx.send(()).unwrap();
        assert!(owner.close_and_wait(Duration::from_secs(2)));
        assert!(owner.worker.is_none());
    }
    #[test]
    fn dropping_owner_cancels_and_joins_setup() {
        use std::time::Duration;
        let (entered_tx, entered_rx) = mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let done = finished.clone();
        let owner = ControlledNetwork::start_with::<LocalIo>(
            empty_network(),
            Arc::new(AtomicBool::new(false)),
            128 * 1024,
            move |cancel| {
                entered_tx.send(()).unwrap();
                while !cancel.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
                done.store(true, Ordering::Release);
                Err(std::io::Error::other("cancelled setup"))
            },
        )
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(owner);
        assert!(finished.load(Ordering::Acquire));
    }
    fn clock() -> Clock {
        Clock {
            unix_ms: 1_735_689_600_000,
            monotonic_ms: 0,
        }
    }
    fn pair() -> (ControlledNetwork<Store>, Sink, Vec<u8>, PublicKey<Node>) {
        let local = DeviceIdentity::generate().unwrap();
        let remote = DeviceIdentity::generate().unwrap();
        let peer = remote.node_key().public();
        let network =
            Network::new(&local, TimestampClock::open(Store, 100).unwrap(), 4, 2).unwrap();
        let map=serde_json::to_vec(&serde_json::json!({"Node":{"ID":1,"Key":local.node_key().public().to_string(),"Addresses":["100.64.0.1/32"],"MachineAuthorized":true},"Peers":[{"ID":2,"Key":peer.to_string(),"DiscoKey":remote.disco_key().public().to_string(),"Addresses":["100.64.0.2/32"],"Endpoints":["192.0.2.2:1234"],"MachineAuthorized":true}],"PacketFilter":[]})).unwrap();
        let (sender, receiver) = mpsc::sync_channel(MAP_QUEUE);
        let state = Arc::new(Mutex::new(None));
        let sink = Sink {
            sender,
            retained: Arc::new(AtomicUsize::new(0)),
            state: state.clone(),
            initial_reset: false,
        };
        let owner = ControlledNetwork {
            network,
            receiver,
            state,
            cancelled: Arc::new(AtomicBool::new(false)),
            worker: None,
            explicitly_closed: false,
            rejection: None,
        };
        (owner, sink, map, peer)
    }
    #[test]
    fn map_then_closure_cannot_release_stale_authorization_or_output() {
        let (mut owner, mut sink, map, peer) = pair();
        sink.apply(&map, clock()).unwrap();
        assert!(
            owner
                .run(clock, |n| {
                    assert!(n.local_address(0)?.is_some());
                    n.probe(peer, 0)
                })
                .unwrap()
        );
        sink.apply(&map, clock()).unwrap();
        sink.disconnect();
        assert!(matches!(
            owner.run::<()>(clock, |_| panic!("closed lease entered operation")),
            Err(Error::Stopped(StopReason::StreamClosed))
        ));
        assert_eq!(owner.network.local_address(0).unwrap(), None);
        assert!(
            !owner
                .network
                .flush_one(0, |_, _, _| panic!("stale output sent"))
                .unwrap()
        );
    }
    #[test]
    fn small_map_burst_drains_before_application_access_and_releases_budget() {
        let (mut owner, mut sink, map, _) = pair();
        sink.apply(&map, clock()).unwrap();
        for _ in 1..MAP_QUEUE - 1 {
            sink.apply(br#"{"KeepAlive":true}"#, clock()).unwrap();
        }
        let mut revoked: serde_json::Value = serde_json::from_slice(&map).unwrap();
        revoked["Node"]["Expired"] = true.into();
        sink.apply(&serde_json::to_vec(&revoked).unwrap(), clock())
            .unwrap();
        assert!(sink.retained.load(Ordering::Acquire) > 0);
        assert!(
            owner
                .run::<()>(clock, |_| panic!("revocation not applied before access"))
                .is_err()
        );
        assert_eq!(sink.retained.load(Ordering::Acquire), 0);
    }
    #[test]
    fn map_byte_budget_remains_two_maximum_frames() {
        let (mut owner, mut sink, mut map, _) = pair();
        map.resize(crate::control_stream::MAX_MAP_BYTES, b' ');
        sink.apply(&map, clock()).unwrap();
        sink.apply(&map, clock()).unwrap();
        assert_eq!(sink.retained.load(Ordering::Acquire), MAP_QUEUE_BYTES);
        owner.run(clock, |_| Ok(())).unwrap();
        assert_eq!(sink.retained.load(Ordering::Acquire), 0);
        sink.apply(&map, clock()).unwrap();
        sink.apply(&map, clock()).unwrap();
        assert!(matches!(
            sink.apply(b"{}", clock()),
            Err(Error::Stopped(StopReason::Overloaded))
        ));
        assert_eq!(sink.retained.load(Ordering::Acquire), MAP_QUEUE_BYTES);
        drop(owner);
        assert_eq!(sink.retained.load(Ordering::Acquire), 0);
    }
    #[test]
    fn overload_and_invalid_map_close_instead_of_dropping_updates() {
        let (mut owner, mut sink, map, _) = pair();
        for _ in 0..MAP_QUEUE {
            sink.apply(&map, clock()).unwrap();
        }
        assert!(matches!(
            sink.apply(&map, clock()),
            Err(Error::Stopped(StopReason::Overloaded))
        ));
        assert_eq!(sink.retained.load(Ordering::Acquire), MAP_QUEUE * map.len());
        assert!(matches!(
            owner.run(clock, |_| Ok(())),
            Err(Error::Stopped(StopReason::Overloaded))
        ));
        let (mut owner, mut sink, _, _) = pair();
        sink.apply(b"invalid", clock()).unwrap();
        assert!(
            owner
                .run::<()>(clock, |_| panic!("invalid map authorized operation"))
                .is_err()
        );
        assert!(owner.cancelled.load(Ordering::Acquire));
        assert_eq!(*owner.state.lock().unwrap(), Some(StopReason::InvalidMap));
    }
    #[test]
    fn disconnect_waits_for_current_nonblocking_operation_boundary() {
        let (mut owner, mut sink, map, _) = pair();
        sink.apply(&map, clock()).unwrap();
        let (start_tx, start_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let state = owner.state.clone();
        let worker = std::thread::spawn(move || {
            start_rx.recv().unwrap();
            stop(&state, StopReason::StreamClosed);
            done_tx.send(()).unwrap();
        });
        owner
            .run(clock, |n| {
                assert!(n.local_address(0)?.is_some());
                start_tx.send(()).unwrap();
                assert!(matches!(done_rx.try_recv(), Err(TryRecvError::Empty)));
                Ok(())
            })
            .unwrap();
        done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        worker.join().unwrap();
        assert!(
            owner
                .run::<()>(clock, |_| panic!("operation after close"))
                .is_err()
        );
    }
    #[test]
    fn cancellation_and_producer_drop_are_terminal() {
        let (mut owner, mut sink, map, _) = pair();
        sink.apply(&map, clock()).unwrap();
        owner.close();
        assert!(owner.cancelled.load(Ordering::Acquire));
        assert!(owner.run(clock, |_| Ok(())).is_err());
        assert!(sink.apply(&map, clock()).is_err());
        let (mut owner, sink, _, _) = pair();
        drop(sink);
        assert!(owner.run(clock, |_| Ok(())).is_err());
    }
}
