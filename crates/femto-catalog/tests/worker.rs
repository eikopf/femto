use femto_catalog::{
    http::{Stream, Transport, Url},
    worker::{Completion, Worker},
};
use std::{
    io::{self, Cursor, Read, Write},
    sync::{Arc, atomic::AtomicBool, mpsc},
    time::{Duration, Instant},
};

struct Reply(Cursor<Vec<u8>>);
impl Read for Reply {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.read(b)
    }
}
impl Write for Reply {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Mock;
impl Transport for Mock {
    fn connect(&mut self, _: &Url, _: Instant, _: Arc<AtomicBool>) -> io::Result<Box<dyn Stream>> {
        let feed = include_bytes!("../../../tests/fixtures/catalog.json");
        let mut reply = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/opds+json\r\nContent-Length: {}\r\n\r\n",
            feed.len()
        )
        .into_bytes();
        reply.extend(feed);
        Ok(Box::new(Reply(Cursor::new(reply))))
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(5)
}
fn completion(worker: &mut Worker) -> Completion {
    let until = deadline();
    loop {
        if let Some(result) = worker.poll() {
            return result;
        }
        assert!(Instant::now() < until, "worker failed to complete");
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn feeds_complete_and_identifiers_are_not_reused() {
    let mut worker = Worker::default();
    let first = worker
        .fetch("https://example.com/feed", None, deadline(), || Ok(Mock))
        .unwrap();
    let reply = completion(&mut worker);
    assert_eq!(reply.id, first);
    assert!(reply.result.is_ok());
    assert!(!worker.is_busy());
    let second = worker
        .fetch("https://example.com/feed", None, deadline(), || Ok(Mock))
        .unwrap();
    assert!(second > first);
    assert!(completion(&mut worker).result.is_ok());
}
#[test]
fn blocked_platform_operation_retains_ownership_and_rejects_new_work() {
    let mut worker = Worker::default();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    worker
        .fetch("https://example.com/feed", None, deadline(), move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(Mock)
        })
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(worker.poll().is_none());
    assert_eq!(
        worker
            .fetch("https://example.com/feed", None, deadline(), || Ok(Mock))
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(!worker.cancel_and_wait(Duration::ZERO));
    assert!(worker.is_busy());
    release_tx.send(()).unwrap();
    let result = completion(&mut worker).result.unwrap_err();
    assert!(
        matches!(result, femto_catalog::http::Error::Io(e) if e.kind() == io::ErrorKind::ConnectionAborted)
    );
    assert!(worker.cancel_and_wait(Duration::ZERO));
}
#[test]
fn expired_and_invalid_requests_never_start_factory() {
    let mut worker = Worker::default();
    for (url, end) in [
        ("file:///book", deadline()),
        ("https://example.com", Instant::now()),
    ] {
        assert!(
            worker
                .fetch::<Mock>(url, None, end, || panic!("factory must not run"))
                .is_err()
        );
        assert!(!worker.is_busy());
    }
}
#[test]
fn worker_panic_becomes_an_error_and_slot_is_reusable() {
    let mut worker = Worker::default();
    worker
        .fetch::<Mock>("https://example.com", None, deadline(), || {
            panic!("injected failure")
        })
        .unwrap();
    assert!(completion(&mut worker).result.is_err());
    worker
        .fetch("https://example.com", None, deadline(), || Ok(Mock))
        .unwrap();
    assert!(completion(&mut worker).result.is_ok());
}

struct NotifyDrop(mpsc::Sender<()>);
impl Drop for NotifyDrop {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}
struct Notifying(Mock, NotifyDrop);
impl Transport for Notifying {
    fn connect(
        &mut self,
        url: &Url,
        end: Instant,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        let _keep_alive = &self.1;
        self.0.connect(url, end, cancel)
    }
}
#[test]
fn cancellation_discards_a_completed_feed_before_delivery() {
    let (done_tx, done_rx) = mpsc::channel();
    let mut worker = Worker::default();
    worker
        .fetch("https://example.com", None, deadline(), move || {
            Ok(Notifying(Mock, NotifyDrop(done_tx)))
        })
        .unwrap();
    // Transport destruction follows the successful parse and task budget check.
    done_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.cancel();
    let result = completion(&mut worker).result.unwrap_err();
    assert!(
        matches!(result, femto_catalog::http::Error::Io(e) if e.kind() == io::ErrorKind::ConnectionAborted)
    );
}
#[test]
fn dropping_owner_joins_platform_work_before_returning() {
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (destroyed_tx, destroyed_rx) = mpsc::channel();
    let mut worker = Worker::default();
    worker
        .fetch("https://example.com", None, deadline(), move || {
            let _resource = NotifyDrop(destroyed_tx);
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(Mock)
        })
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (dropping_tx, dropping_rx) = mpsc::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    let owner = std::thread::spawn(move || {
        dropping_tx.send(()).unwrap();
        drop(worker);
        // Joining the owner must also prove that the platform resource was dropped.
        destroyed_rx.try_recv().unwrap();
        dropped_tx.send(()).unwrap();
    });
    dropping_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(
        dropped_rx.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    release_tx.send(()).unwrap();
    dropped_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    owner.join().unwrap();
}

#[test]
fn cancelled_typed_results_are_dropped_without_delivery() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Prepared(Arc<AtomicUsize>);
    impl Drop for Prepared {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    let drops = Arc::new(AtomicUsize::new(0));
    let result_drops = drops.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    let mut worker = Worker::default();
    worker
        .submit(deadline(), move |_, _| {
            let result = Prepared(result_drops);
            ready_tx.send(()).unwrap();
            Ok(result)
        })
        .unwrap();
    ready_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    worker.cancel();
    let end = deadline();
    loop {
        if let Some(done) = worker.poll() {
            assert!(done.result.is_err());
            break;
        }
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(drops.load(Ordering::Relaxed), 1);
}
