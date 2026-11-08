#[path = "../../../firmware/picco/src/progress_sync.rs"]
mod progress_sync;
use femto_catalog::http;
use femto_reader::{
    controller::Reader,
    document::Document,
    journal::StateJournal,
    layout::{Font, Fonts},
    state::ReaderState,
    ui::ReaderUi,
};
use std::{
    collections::VecDeque,
    io::{self, Cursor, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
fn book() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sample.txt")
}
fn reader() -> Reader {
    let font = Font::load(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/DejaVuSerif.ttf"),
    )
    .unwrap();
    let mut reader = Reader::new(
        Document::open(book()).unwrap(),
        Fonts::new(font),
        ReaderState::default(),
    )
    .unwrap();
    reader.record_position(1);
    reader
}
fn service(root: &Path) -> progress_sync::Service {
    std::fs::write(
        root.join("progress.url"),
        "https://peer.example/progress/\n",
    )
    .unwrap();
    progress_sync::Service::load(root).unwrap().unwrap()
}
struct Wire {
    input: Cursor<Vec<u8>>,
    request: Arc<Mutex<Vec<u8>>>,
}
impl Read for Wire {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.input.read(b)
    }
}
impl Write for Wire {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.request.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
type Requests = Arc<Mutex<Vec<Arc<Mutex<Vec<u8>>>>>>;
struct Transport {
    replies: VecDeque<Vec<u8>>,
    requests: Requests,
}
impl http::Transport for Transport {
    fn connect(
        &mut self,
        _: &http::Url,
        _: Instant,
        _: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn http::Stream>> {
        let request = Arc::new(Mutex::new(Vec::new()));
        self.requests.lock().unwrap().push(request.clone());
        Ok(Box::new(Wire {
            input: Cursor::new(self.replies.pop_front().expect("unexpected request")),
            request,
        }))
    }
}
fn poll(
    jobs: &mut progress_sync::Jobs,
    reader: &mut Reader,
    journal: &mut Option<StateJournal>,
    ui: &mut ReaderUi,
) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !progress_sync::poll(jobs, reader, journal, ui, &mut None) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn device_worker_creates_and_downloads_through_journal() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("progress-auth.json"),
        br#"{"username":"reader","password":"fixture"}"#,
    )
    .unwrap();
    let service = service(root.path());
    assert!(!service.tailnet);
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.open_sync();
    let mut journal = Some(StateJournal::open(root.path()).unwrap().0);
    let mut jobs = progress_sync::Jobs::default();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let capture = requests.clone();
    service
        .start(&mut reader, book(), &mut jobs, None, move || {
            Ok(Box::new(Transport {
                replies: VecDeque::from([
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
                    b"HTTP/1.1 201 Created\r\nETag: \"created\"\r\nContent-Length: 0\r\n\r\n"
                        .to_vec(),
                ]),
                requests: capture,
            }))
        })
        .unwrap();
    poll(&mut jobs, &mut reader, &mut journal, &mut ui);
    let id = reader.document.metadata.id.clone();
    assert!(reader.state.books[&id].sync.is_some());
    let put = requests.lock().unwrap()[1].lock().unwrap().clone();
    assert!(String::from_utf8_lossy(&put).contains(&format!("PUT /progress/{id} HTTP/1.1")));
    assert!(String::from_utf8_lossy(&put).contains("If-None-Match: *"));
    assert!(String::from_utf8_lossy(&put).contains("Authorization: Basic cmVhZGVyOmZpeHR1cmU="));
    let mut remote = reader.progress_snapshot();
    remote.position.byte = 1;
    let body = serde_json::to_vec(&remote).unwrap();
    let mut reply=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: \"next\"\r\nContent-Length: {}\r\n\r\n",body.len()).into_bytes();
    reply.extend(body);
    service
        .start(&mut reader, book(), &mut jobs, None, move || {
            Ok(Box::new(Transport {
                replies: VecDeque::from([reply]),
                requests,
            }))
        })
        .unwrap();
    poll(&mut jobs, &mut reader, &mut journal, &mut ui);
    assert_eq!(reader.position(), remote.position);
    assert_eq!(
        StateJournal::open(root.path()).unwrap().1.books[&id].position,
        remote.position
    );
}
#[test]
fn cancelled_worker_retains_ownership_until_factory_returns() {
    let root = tempfile::tempdir().unwrap();
    let service = service(root.path());
    let mut reader = reader();
    let mut jobs = progress_sync::Jobs::default();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    service
        .start(&mut reader, book(), &mut jobs, None, move || {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Err(io::Error::other("released"))
        })
        .unwrap();
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        service
            .start(&mut reader, book(), &mut jobs, None, || panic!(
                "second job"
            ))
            .is_err()
    );
    assert!(!jobs.cancel_and_wait(Duration::ZERO));
    assert!(jobs.is_busy());
    release_tx.send(()).unwrap();
    assert!(jobs.cancel_and_wait(Duration::from_secs(5)));
    assert!(!jobs.is_busy());
    assert!(
        reader.state.books[&reader.document.metadata.id]
            .sync
            .is_none()
    );
}
#[test]
fn configuration_bounds_routes_and_credentials() {
    let root = tempfile::tempdir().unwrap();
    assert!(progress_sync::Service::load(root.path()).unwrap().is_none());
    service(root.path());
    std::fs::write(root.path().join("progress-route"), "tailnet").unwrap();
    assert!(
        progress_sync::Service::load(root.path())
            .unwrap()
            .unwrap()
            .tailnet
    );
    std::fs::write(root.path().join("progress-route"), "fallback").unwrap();
    assert!(progress_sync::Service::load(root.path()).is_err());
    std::fs::remove_file(root.path().join("progress-route")).unwrap();
    for url in [
        "https://peer.example/progress",
        "https://user:secret@peer.example/",
        "https://peer.example/?query",
        "https://peer.example/#fragment",
        "https://peer.example:0/",
    ] {
        std::fs::write(root.path().join("progress.url"), url).unwrap();
        assert!(progress_sync::Service::load(root.path()).is_err());
    }
    service(root.path());
    std::fs::write(root.path().join("progress-auth.json"), vec![b'x'; 8193]).unwrap();
    assert!(progress_sync::Service::load(root.path()).is_err());
}

#[test]
fn journal_failure_disables_further_device_saves_without_applying_result() {
    let root = tempfile::tempdir().unwrap();
    let service = service(root.path());
    let mut reader = reader();
    let mut ui = ReaderUi::default();
    ui.open_sync();
    let (mut store, _) = StateJournal::open(root.path()).unwrap();
    store.save(&reader.state).unwrap();
    std::fs::create_dir(root.path().join("state1.bin")).unwrap();
    let before = serde_json::to_vec(&reader.state).unwrap();
    let mut journal = Some(store);
    let mut jobs = progress_sync::Jobs::default();
    service
        .start(&mut reader, book(), &mut jobs, None, || {
            Ok(Box::new(Transport {
                replies: VecDeque::from([
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
                    b"HTTP/1.1 201 Created\r\nETag: \"created\"\r\nContent-Length: 0\r\n\r\n"
                        .to_vec(),
                ]),
                requests: Arc::default(),
            }))
        })
        .unwrap();
    poll(&mut jobs, &mut reader, &mut journal, &mut ui);
    assert!(journal.is_none());
    assert_eq!(serde_json::to_vec(&reader.state).unwrap(), before);
    std::fs::remove_dir(root.path().join("state1.bin")).unwrap();
    assert_eq!(
        serde_json::to_vec(&StateJournal::open(root.path()).unwrap().1).unwrap(),
        before
    );
}

#[test]
fn device_conflict_choices_recheck_and_commit_the_selected_position() {
    for choice in [femto_sync::Choice::Local, femto_sync::Choice::Remote] {
        let root = tempfile::tempdir().unwrap();
        let service = service(root.path());
        let mut reader = reader();
        let original = reader.progress_snapshot();
        let mut remote = original.clone();
        remote.position.byte = 1;
        let body = serde_json::to_vec(&remote).unwrap();
        let mut response=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: \"remote\"\r\nContent-Length: {}\r\n\r\n",body.len()).into_bytes();
        response.extend(body);
        let mut ui = ReaderUi::default();
        ui.open_sync();
        let mut journal = Some(StateJournal::open(root.path()).unwrap().0);
        let mut jobs = progress_sync::Jobs::default();
        let first = response.clone();
        service
            .start(&mut reader, book(), &mut jobs, None, move || {
                Ok(Box::new(Transport {
                    replies: VecDeque::from([first]),
                    requests: Arc::default(),
                }))
            })
            .unwrap();
        let mut conflict = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while !progress_sync::poll(&mut jobs, &mut reader, &mut journal, &mut ui, &mut conflict) {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        let conflict = conflict.expect("conflict must be retained for a choice");
        assert_eq!(conflict.local(), &original);
        assert_eq!(conflict.remote(), Some(&remote));
        assert!(reader.state.books[&original.book_id].sync.is_none());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let capture = requests.clone();
        service
            .start(
                &mut reader,
                book(),
                &mut jobs,
                Some((conflict, choice)),
                move || {
                    Ok(Box::new(Transport {
                        replies: VecDeque::from([
                            response,
                            b"HTTP/1.1 200 OK\r\nETag: \"resolved\"\r\nContent-Length: 0\r\n\r\n"
                                .to_vec(),
                        ]),
                        requests: capture,
                    }))
                },
            )
            .unwrap();
        poll(&mut jobs, &mut reader, &mut journal, &mut ui);
        let expected = if choice == femto_sync::Choice::Local {
            original.position
        } else {
            remote.position
        };
        assert_eq!(reader.position(), expected);
        let saved = StateJournal::open(root.path()).unwrap().1;
        assert_eq!(saved.books[&original.book_id].position, expected);
        assert_eq!(
            saved.books[&original.book_id]
                .sync
                .as_ref()
                .unwrap()
                .baseline
                .position,
            expected
        );
        assert_eq!(
            requests.lock().unwrap().len(),
            if choice == femto_sync::Choice::Local {
                2
            } else {
                1
            }
        );
    }
}

#[test]
fn reset_failure_disables_device_journal_and_preserves_the_checkpoint() {
    let mut reader = reader();
    let id = reader.document.metadata.id.clone();
    let baseline = reader.progress_snapshot();
    reader.state.books.get_mut(&id).unwrap().sync = Some(femto_reader::progress::SyncCheckpoint {
        source: format!("https://peer.example/progress/{id}"),
        baseline,
        etag: "\"saved\"".into(),
    });
    let reset = femto_sync::reader::Reset::prepare(&reader).unwrap();
    let root = tempfile::tempdir().unwrap();
    let (mut store, _) = StateJournal::open(root.path()).unwrap();
    store.save(&reader.state).unwrap();
    let mut journal = Some(store);
    std::fs::create_dir(root.path().join("state1.bin")).unwrap();
    let before = serde_json::to_vec(&reader.state).unwrap();
    assert!(
        progress_sync::reset(
            reset,
            &progress_sync::Jobs::default(),
            &mut reader,
            &mut journal
        )
        .is_err()
    );
    assert!(journal.is_none());
    assert_eq!(serde_json::to_vec(&reader.state).unwrap(), before);
}

#[test]
fn reset_refuses_an_active_worker_without_touching_the_journal() {
    let mut reader = reader();
    let id = reader.document.metadata.id.clone();
    let baseline = reader.progress_snapshot();
    reader.state.books.get_mut(&id).unwrap().sync = Some(femto_reader::progress::SyncCheckpoint {
        source: format!("https://peer.example/progress/{id}"),
        baseline,
        etag: "\"saved\"".into(),
    });
    let reset = femto_sync::reader::Reset::prepare(&reader).unwrap();
    let root = tempfile::tempdir().unwrap();
    let (mut store, _) = StateJournal::open(root.path()).unwrap();
    store.save(&reader.state).unwrap();
    let mut journal = Some(store);
    let before = std::fs::read(root.path().join("state0.bin")).unwrap();
    let mut jobs = progress_sync::Jobs::default();
    let (release, wait) = std::sync::mpsc::channel();
    jobs.submit(Instant::now() + Duration::from_secs(10), move |_, _| {
        wait.recv().unwrap();
        Err(io::Error::other("released").into())
    })
    .unwrap();
    assert!(progress_sync::reset(reset, &jobs, &mut reader, &mut journal).is_err());
    assert!(journal.is_some());
    assert!(reader.state.books[&id].sync.is_some());
    assert_eq!(
        std::fs::read(root.path().join("state0.bin")).unwrap(),
        before
    );
    assert!(!root.path().join("state1.bin").exists());
    release.send(()).unwrap();
    assert!(jobs.cancel_and_wait(Duration::from_secs(5)));
}
