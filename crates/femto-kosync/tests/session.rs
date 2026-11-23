#![cfg(feature = "reader")]
use femto_catalog::http::{Stream, Transport, Url};
use femto_kosync::{
    DocumentId,
    config::Settings,
    session::{Session, TransportFactory},
};
use femto_reader::{
    controller::{Action, Reader},
    document::Document,
    layout::{Font, Fonts},
    state::ReaderState,
    ui::{KosyncEvent, ReaderUi},
};
use std::{
    io::{self, Cursor, Read, Write},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}
struct Reply(Cursor<Vec<u8>>);
impl Read for Reply {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.0.read(out)
    }
}
impl Write for Reply {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Mock {
    body: String,
    ready: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
}
impl Transport for Mock {
    fn connect(
        &mut self,
        _: &Url,
        deadline: femto_catalog::deadline::Limit,
        cancel: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        while !self.ready.load(Ordering::SeqCst) {
            femto_kosync::book::boundary(deadline, &cancel)?;
            std::thread::sleep(Duration::from_millis(1));
        }
        Ok(Box::new(Reply(Cursor::new(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                self.body.len(),
                self.body
            )
            .into_bytes(),
        ))))
    }
}
fn setup(
    path: &std::path::Path,
) -> (
    Reader,
    Session,
    ReaderUi,
    TransportFactory,
    Arc<AtomicBool>,
    Arc<AtomicUsize>,
) {
    let reader = Reader::new(
        Document::open(path).unwrap(),
        Fonts::new(Font::load(fixture("DejaVuSerif.ttf")).unwrap()),
        ReaderState::default(),
    )
    .unwrap();
    let id = DocumentId::sampled(&mut std::fs::File::open(path).unwrap()).unwrap();
    let body = serde_json::json!({"document":id,"progress":"/body/DocFragment[1]/body/h1/text().4","percentage":0.1,"device":"KOReader","device_id":"peer","timestamp":123}).to_string();
    let ready = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let factory: TransportFactory = {
        let ready = ready.clone();
        let calls = calls.clone();
        Arc::new(move |origins| {
            assert_eq!(origins, &["https://peer.test"]);
            Ok(Box::new(Mock {
                body: body.clone(),
                ready: ready.clone(),
                calls: calls.clone(),
            }))
        })
    };
    let settings = Settings::from_json(br#"{"server":"https://peer.test/prefix/","username":"alice","password":"secret","device_id":"test","route":"tailnet"}"#).unwrap();
    (
        reader,
        Session::new(settings),
        ReaderUi::default(),
        factory,
        ready,
        calls,
    )
}
fn until(mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !check() {
        assert!(Instant::now() < deadline, "fixture timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn download_commits_current_annotations_and_rejects_stale_failed_and_cancelled_results() {
    for case in ["success", "stale", "failed", "cancelled"] {
        let (mut reader, mut session, mut ui, factory, ready, calls) =
            setup(&fixture("sample.epub"));
        reader.record_position(10);
        let original = reader.position();
        session
            .event(KosyncEvent::Pull, &reader, &mut ui, factory.clone())
            .unwrap();
        until(|| calls.load(Ordering::SeqCst) == 1);
        reader.dispatch(Action::ToggleBookmark).unwrap();
        if case == "stale" {
            reader.dispatch(Action::Chapter(1)).unwrap();
        }
        if case == "cancelled" {
            session
                .event(KosyncEvent::Back, &reader, &mut ui, factory.clone())
                .unwrap();
            session
                .event(KosyncEvent::Open, &reader, &mut ui, factory)
                .unwrap();
        }
        let before = serde_json::to_vec(&reader.state).unwrap();
        let before_position = reader.position();
        ready.store(true, Ordering::SeqCst);
        let mut saves = 0;
        until(|| {
            session.poll(&mut reader, &mut ui, 20, |state| {
                saves += 1;
                assert_eq!(state.books.values().next().unwrap().bookmarks.len(), 1);
                if case == "failed" {
                    Err(io::Error::other("disk full").into())
                } else {
                    Ok(())
                }
            });
            !session.is_busy()
        });
        if case == "success" {
            assert_eq!(saves, 1);
            assert_eq!(
                reader.position(),
                femto_reader::layout::Locator {
                    byte: 4,
                    ..original
                }
            );
            assert_eq!(
                reader.state.books.values().next().unwrap().position,
                reader.position()
            );
        } else {
            assert_eq!(saves, usize::from(case == "failed"));
            assert_eq!(reader.position(), before_position);
            assert_eq!(serde_json::to_vec(&reader.state).unwrap(), before);
        }
    }
}
#[test]
fn changed_source_file_is_rejected_before_network_access() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("book.epub");
    std::fs::copy(fixture("sample.epub"), &path).unwrap();
    let (mut reader, mut session, mut ui, factory, _, calls) = setup(&path);
    std::fs::rename(&path, dir.path().join("original.epub")).unwrap();
    std::fs::copy(fixture("navigation.epub"), &path).unwrap();
    session
        .event(KosyncEvent::Pull, &reader, &mut ui, factory)
        .unwrap();
    until(|| {
        session.poll(&mut reader, &mut ui, 20, |_| {
            panic!("changed file must not save")
        });
        !session.is_busy()
    });
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
