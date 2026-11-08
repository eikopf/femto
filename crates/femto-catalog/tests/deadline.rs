use femto_catalog::deadline::{Bounded, Budget};
use std::{
    io::{self, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

struct Fake {
    pending: usize,
    cancel_on_read: Option<Arc<AtomicBool>>,
    calls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}
impl Read for Fake {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.pending > 0 {
            self.pending -= 1;
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if let Some(cancel) = &self.cancel_on_read {
            cancel.store(true, Ordering::Release);
        }
        bytes.fill(b'x');
        Ok(bytes.len())
    }
}
impl Write for Fake {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(bytes.len().min(2))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}
fn fake() -> Fake {
    Fake {
        pending: 0,
        cancel_on_read: None,
        calls: Arc::default(),
        drops: Arc::default(),
    }
}
fn budget(cancel: Arc<AtomicBool>) -> Budget {
    Budget::new(Instant::now() + Duration::from_secs(5), cancel)
}

#[test]
fn cancellation_during_read_withholds_bytes_and_closes_stream() {
    let cancel = Arc::new(AtomicBool::new(false));
    let mut backend = fake();
    backend.cancel_on_read = Some(cancel.clone());
    let drops = backend.drops.clone();
    let calls = backend.calls.clone();
    let mut stream = Bounded::new(backend, budget(cancel));
    let mut bytes = [42; 20];
    assert_eq!(
        stream.read(&mut bytes).unwrap_err().kind(),
        io::ErrorKind::ConnectionAborted
    );
    assert_eq!(bytes, [42; 20]);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(
        stream.write(b"no").unwrap_err().kind(),
        io::ErrorKind::NotConnected
    );
    assert_eq!(calls.load(Ordering::Relaxed), 1);
}

#[test]
fn pre_cancelled_and_expired_operations_do_not_touch_backend() {
    for budget in [
        budget(Arc::new(AtomicBool::new(true))),
        Budget::new(Instant::now(), Arc::default()),
    ] {
        let backend = fake();
        let calls = backend.calls.clone();
        let drops = backend.drops.clone();
        let mut stream = Bounded::new(backend, budget);
        assert!(stream.flush().is_err());
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn pending_io_retries_and_partial_writes_remain_partial() {
    let mut backend = fake();
    backend.pending = 2;
    let calls = backend.calls.clone();
    let mut stream = Bounded::new(backend, budget(Arc::default()));
    let mut bytes = [0; 2048];
    assert_eq!(stream.read(&mut bytes).unwrap(), 1024);
    assert!(bytes[..1024].iter().all(|b| *b == b'x'));
    assert!(bytes[1024..].iter().all(|b| *b == 0));
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    assert_eq!(stream.write(b"hello").unwrap(), 2);
    stream.write_all(b"hello").unwrap();
}

#[test]
fn stalled_operation_observes_absolute_deadline() {
    let mut backend = fake();
    backend.pending = usize::MAX;
    let drops = backend.drops.clone();
    let mut stream = Bounded::new(
        backend,
        Budget::new(Instant::now() + Duration::from_millis(30), Arc::default()),
    );
    let mut bytes = [42];
    assert_eq!(
        stream.read(&mut bytes).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(bytes, [42]);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}
