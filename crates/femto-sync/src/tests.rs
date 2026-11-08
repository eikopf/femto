use super::*;
use femto_reader::{layout::Locator, state::ReaderState};
use std::{
    cell::RefCell,
    collections::VecDeque,
    io::{self, Cursor, Read, Write},
    path::PathBuf,
    rc::Rc,
    time::Duration,
};
struct Wire {
    reply: Cursor<Vec<u8>>,
    requests: Rc<RefCell<Vec<Vec<u8>>>>,
    index: usize,
}
impl Read for Wire {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.reply.read(bytes)
    }
}
impl Write for Wire {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.requests.borrow_mut()[self.index].extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Fixture {
    replies: VecDeque<Vec<u8>>,
    requests: Rc<RefCell<Vec<Vec<u8>>>>,
}
impl http::Transport for Fixture {
    fn connect(
        &mut self,
        _: &url::Url,
        _: Instant,
        _: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn http::Stream>> {
        let index = self.requests.borrow().len();
        self.requests.borrow_mut().push(Vec::new());
        Ok(Box::new(Wire {
            reply: Cursor::new(self.replies.pop_front().expect("unexpected request")),
            requests: self.requests.clone(),
            index,
        }))
    }
}

fn record(document: &Document, byte: usize) -> Progress {
    let mut state = ReaderState::default();
    state.set_position(
        &document.metadata.id,
        Locator {
            chapter: 0,
            block: 0,
            byte,
        },
        1,
    );
    Progress::from_state(&state, &document.metadata.id).unwrap()
}
fn reply(remote: Option<&Progress>, etag: bool) -> Vec<u8> {
    let Some(remote) = remote else {
        return b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec();
    };
    let body = serde_json::to_vec(remote).unwrap();
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{}Content-Length: {}\r\n\r\n",
        if etag { "ETag: \"fresh\"\r\n" } else { "" },
        body.len()
    )
    .into_bytes();
    response.extend(body);
    response
}
fn document() -> Document {
    Document::open(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sample.txt"),
    )
    .unwrap()
}
fn fixture(replies: Vec<Vec<u8>>) -> Fixture {
    Fixture {
        replies: replies.into(),
        requests: Rc::default(),
    }
}
fn request<'a>(local: Option<&'a Progress>, baseline: Option<&'a Progress>) -> Request<'a> {
    Request {
        source: "https://peer.example/progress/book",
        local,
        baseline,
        auth: None,
        deadline: Instant::now() + Duration::from_secs(5),
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}
#[test]
fn all_actions_use_fresh_comparison_and_conditional_writes() {
    use SyncAction::*;
    let mut doc = document();
    for (base, local, remote, expected) in [
        (None, None, None, Empty),
        (None, Some(0), None, Create),
        (Some(0), Some(1), Some(0), Upload),
        (Some(0), Some(0), Some(1), Download),
        (None, None, Some(1), Download),
        (Some(0), Some(1), Some(1), Converged),
        (Some(0), Some(1), Some(2), Conflict),
        (Some(0), Some(0), None, Conflict),
    ] {
        let base = base.map(|b| record(&doc, b));
        let local = local.map(|b| record(&doc, b));
        let remote = remote.map(|b| record(&doc, b));
        let mut transport = fixture(vec![
            reply(remote.as_ref(), true),
            b"HTTP/1.1 200 OK\r\nETag: \"written\"\r\nContent-Length: 0\r\n\r\n".to_vec(),
        ]);
        let result = request(local.as_ref(), base.as_ref())
            .execute(&mut doc, &mut transport)
            .unwrap();
        match result {
            Outcome::Empty => assert_eq!(expected, Empty),
            Outcome::Conflict { remote: actual, .. } => {
                assert_eq!(expected, Conflict);
                assert_eq!(actual, remote);
            }
            Outcome::Synchronized {
                action,
                baseline,
                etag,
            } => {
                assert_eq!(action, expected);
                let written = matches!(action, Create | Upload);
                assert_eq!(
                    baseline,
                    if written {
                        local.unwrap()
                    } else {
                        remote.unwrap()
                    }
                );
                assert_eq!(
                    etag.as_str(),
                    if written { "\"written\"" } else { "\"fresh\"" }
                );
            }
        }
        let requests = transport.requests.borrow();
        assert!(requests[0].starts_with(b"GET "));
        if matches!(expected, Create | Upload) {
            assert_eq!(requests.len(), 2);
            let put = String::from_utf8_lossy(&requests[1]);
            assert!(put.starts_with("PUT "));
            assert!(put.contains(if expected == Create {
                "If-None-Match: *\r\n"
            } else {
                "If-Match: \"fresh\"\r\n"
            }));
        } else {
            assert_eq!(requests.len(), 1);
        }
    }
}
#[test]
fn invalid_snapshots_and_cancelled_requests_never_access_transport() {
    let mut doc = document();
    let mut invalid = record(&doc, 0);
    invalid.book_id = "f".repeat(64);
    for baseline in [false, true] {
        let mut transport = fixture(vec![]);
        let req = if baseline {
            request(None, Some(&invalid))
        } else {
            request(Some(&invalid), None)
        };
        assert!(req.execute(&mut doc, &mut transport).is_err());
        assert!(transport.requests.borrow().is_empty());
    }
    let mut transport = fixture(vec![]);
    let req = request(None, None);
    req.cancelled
        .store(true, std::sync::atomic::Ordering::Release);
    assert!(req.execute(&mut doc, &mut transport).is_err());
    assert!(transport.requests.borrow().is_empty());
}
#[test]
fn failed_remote_validation_and_uploads_do_not_return_a_baseline() {
    let mut doc = document();
    let local = record(&doc, 1);
    let base = record(&doc, 0);
    let mut wrong = base.clone();
    wrong.book_id = "f".repeat(64);
    for response in [reply(Some(&wrong), true), reply(Some(&base), false)] {
        let mut transport = fixture(vec![response]);
        assert!(
            request(Some(&local), Some(&base))
                .execute(&mut doc, &mut transport)
                .is_err()
        );
        assert_eq!(transport.requests.borrow().len(), 1);
    }
    for (response, changed) in [
        (
            b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\n\r\n".to_vec(),
            true,
        ),
        (vec![], false),
    ] {
        let mut transport = fixture(vec![reply(Some(&base), true), response]);
        let result = request(Some(&local), Some(&base)).execute(&mut doc, &mut transport);
        if changed {
            assert!(matches!(result, Err(Error::RemoteChanged)));
        } else {
            assert!(matches!(result, Err(Error::Upload(_))));
        }
        assert_eq!(transport.requests.borrow().len(), 2);
    }
}

fn live_reader() -> femto_reader::controller::Reader {
    use femto_reader::{
        controller::Reader,
        layout::{Font, Fonts},
    };
    let fonts = Fonts::new(
        Font::load(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/DejaVuSerif.ttf"),
        )
        .unwrap(),
    );
    let mut reader = Reader::new(document(), fonts, ReaderState::default()).unwrap();
    reader.record_position(1);
    reader.state.toggle_bookmark(
        &reader.document.metadata.id,
        Locator::default(),
        "Keep".into(),
    );
    reader.clip_page_text(7).unwrap();
    reader.toggle_clipping_highlight(0).unwrap();
    reader
}

#[test]
fn reader_snapshot_defers_document_validation_without_accessing_transport() {
    let mut reader = live_reader();
    let mut baseline = reader.progress_snapshot();
    baseline.position.chapter = usize::MAX;
    let id = baseline.book_id.clone();
    reader.state.books.get_mut(&id).unwrap().sync = Some(femto_reader::progress::SyncCheckpoint {
        source: "https://peer.example/progress/book".into(),
        baseline,
        etag: "\"saved\"".into(),
    });
    let before = serde_json::to_vec(&reader.state).unwrap();
    let session = crate::reader::Session::prepare(&reader, "https://peer.example/progress/book")
        .expect("snapshot does not parse baseline chapter");
    let mut transport = fixture(vec![]);
    assert!(matches!(
        session.exchange(
            &mut document(),
            &mut transport,
            None,
            Instant::now() + Duration::from_secs(30),
            Arc::new(AtomicBool::new(false))
        ),
        Err(Error::Reader(_))
    ));
    assert!(transport.requests.borrow().is_empty());
    assert_eq!(serde_json::to_vec(&reader.state).unwrap(), before);

    let reader = live_reader();
    let session =
        crate::reader::Session::prepare(&reader, "https://peer.example/progress/book").unwrap();
    let mut replaced = Document::open(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sample.epub"),
    )
    .unwrap();
    assert!(matches!(
        session.exchange(
            &mut replaced,
            &mut transport,
            None,
            Instant::now() + Duration::from_secs(30),
            Arc::new(AtomicBool::new(false))
        ),
        Err(Error::Reader(_))
    ));
    assert!(transport.requests.borrow().is_empty());
}

#[test]
fn cancelled_or_expired_resolution_stops_before_document_validation() {
    let mut doc = document();
    let mut observed = record(&doc, 0);
    observed.position.chapter = usize::MAX;
    let tag = StrongEtag::parse("\"observed\"").unwrap();
    for cancelled in [true, false] {
        let mut req = request(None, None);
        req.cancelled = Arc::new(AtomicBool::new(cancelled));
        if !cancelled {
            req.deadline = Instant::now();
        }
        let mut transport = fixture(vec![]);
        let error = req
            .resolve(
                &mut doc,
                &mut transport,
                Choice::Remote,
                Some((&observed, &tag)),
            )
            .unwrap_err();
        assert!(matches!(error, Error::Io(ref error) if error.kind() ==
            if cancelled { io::ErrorKind::ConnectionAborted } else { io::ErrorKind::TimedOut }));
        assert!(transport.requests.borrow().is_empty());
    }
}

fn completion(
    reader: &mut femto_reader::controller::Reader,
    remote: Option<&Progress>,
    cancelled: Arc<AtomicBool>,
) -> crate::reader::Completion {
    let session =
        crate::reader::Session::prepare(reader, "https://peer.example/progress/book").unwrap();
    let mut transport = fixture(vec![
        reply(remote, true),
        b"HTTP/1.1 201 Created\r\nETag: \"written\"\r\nContent-Length: 0\r\n\r\n".to_vec(),
    ]);
    session
        .exchange(
            &mut document(),
            &mut transport,
            None,
            Instant::now() + Duration::from_secs(30),
            cancelled,
        )
        .unwrap()
}
#[test]
fn reader_commits_position_and_checkpoint_in_one_recoverable_journal_slot() {
    use femto_reader::journal::StateJournal;
    let mut reader = live_reader();
    let id = reader.document.metadata.id.clone();
    let directory = tempfile::tempdir().unwrap();
    let (mut journal, _) = StateJournal::open(directory.path()).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let create = completion(&mut reader, None, cancel.clone());
    assert!(matches!(
        create
            .commit(&mut reader, 2, |state| journal.save(state))
            .unwrap(),
        crate::reader::Commit::Saved {
            action: SyncAction::Create,
            ..
        }
    ));
    let (_, first) = StateJournal::open(directory.path()).unwrap();
    assert_eq!(
        first.books[&id].sync.as_ref().unwrap().baseline.position,
        first.books[&id].position
    );
    let mut remote = reader.progress_snapshot();
    remote.position.byte = 1;
    let download = completion(&mut reader, Some(&remote), cancel.clone());
    reader.state.frontlight.brightness = 3;
    download
        .commit(&mut reader, 3, |state| {
            journal.save(state)?;
            cancel.store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        })
        .unwrap();
    assert_eq!(reader.position(), remote.position);
    let (_, saved) = StateJournal::open(directory.path()).unwrap();
    assert_eq!(saved.books[&id].position, remote.position);
    assert_eq!(saved.books[&id].sync.as_ref().unwrap().baseline, remote);
    assert_eq!(saved.books[&id].bookmarks[0].label, "Keep");
    assert_eq!(saved.books[&id].clippings, first.books[&id].clippings);
    assert!(saved.books[&id].clippings[0].highlighted);
    assert_eq!(saved.frontlight.brightness, 3);
    assert_eq!(
        serde_json::to_vec(&reader.state).unwrap(),
        serde_json::to_vec(&saved).unwrap()
    );
    std::fs::write(directory.path().join("state1.bin"), b"torn write").unwrap();
    let (_, recovered) = StateJournal::open(directory.path()).unwrap();
    assert_eq!(
        serde_json::to_vec(&recovered).unwrap(),
        serde_json::to_vec(&first).unwrap()
    );
    assert!(matches!(
        crate::reader::Session::prepare(&reader, "https://other.example/progress/book"),
        Err(Error::CheckpointSource)
    ));
}
#[test]
fn reader_rejects_stale_and_cancelled_completions_without_persisting() {
    use femto_reader::controller::Action;
    for mode in 0..3 {
        let mut reader = live_reader();
        let cancel = Arc::new(AtomicBool::new(false));
        let completed = completion(&mut reader, None, cancel.clone());
        match mode {
            0 => {
                reader.dispatch(Action::Chapter(0)).unwrap();
                reader.dispatch(Action::Next).unwrap();
            }
            1 => {
                cancel.store(true, std::sync::atomic::Ordering::Release);
            }
            _ => {
                let baseline = reader.progress_snapshot();
                let id = baseline.book_id.clone();
                reader.state.books.get_mut(&id).unwrap().sync =
                    Some(femto_reader::progress::SyncCheckpoint {
                        source: "https://peer.example/progress/book".into(),
                        baseline,
                        etag: "\"other\"".into(),
                    });
            }
        }
        let before = serde_json::to_vec(&reader.state).unwrap();
        let position = reader.position();
        assert!(
            completed
                .commit(&mut reader, 2, |_| panic!("stale result persisted"))
                .is_err()
        );
        assert_eq!(reader.position(), position);
        assert_eq!(serde_json::to_vec(&reader.state).unwrap(), before);
    }
}
#[test]
fn persistence_failure_preserves_visible_position_state_and_checkpoint() {
    let mut reader = live_reader();
    let cancel = Arc::new(AtomicBool::new(false));
    completion(&mut reader, None, cancel.clone())
        .commit(&mut reader, 2, |_| Ok(()))
        .unwrap();
    let mut remote = reader.progress_snapshot();
    remote.position.byte = 1;
    let completed = completion(&mut reader, Some(&remote), cancel);
    let state = serde_json::to_vec(&reader.state).unwrap();
    let position = reader.position();
    let frame = reader.render(2).unwrap();
    assert!(
        completed
            .commit(&mut reader, 3, |_| Err(
                io::Error::other("write failed").into()
            ))
            .is_err()
    );
    assert_eq!(reader.position(), position);
    assert_eq!(serde_json::to_vec(&reader.state).unwrap(), state);
    assert_eq!(reader.render(2).unwrap(), frame);
}

#[test]
fn resolution_requires_the_observed_remote_record_and_etag() {
    let mut doc = document();
    let local = record(&doc, 1);
    let remote = record(&doc, 2);
    let tag = StrongEtag::parse("\"fresh\"").unwrap();
    for choice in [Choice::Local, Choice::Remote] {
        for changed in 0..3 {
            let mut fresh = remote.clone();
            if changed == 1 {
                fresh.position.byte = 3;
            }
            let mut response = reply(Some(&fresh), true);
            if changed == 2 {
                response = String::from_utf8(response)
                    .unwrap()
                    .replace("\"fresh\"", "\"newer\"")
                    .into_bytes();
            }
            let mut transport = fixture(vec![
                response,
                b"HTTP/1.1 200 OK\r\nETag: \"saved\"\r\nContent-Length: 0\r\n\r\n".to_vec(),
            ]);
            let result = request(Some(&local), None)
                .resolve(&mut doc, &mut transport, choice, Some((&remote, &tag)))
                .unwrap();
            let requests = transport.requests.borrow();
            if changed != 0 {
                assert!(matches!(result, Outcome::Conflict { .. }));
                assert_eq!(requests.len(), 1);
            } else {
                let Outcome::Synchronized {
                    action, baseline, ..
                } = result
                else {
                    panic!("resolution not applied")
                };
                match choice {
                    Choice::Local => {
                        assert_eq!(action, SyncAction::Upload);
                        assert_eq!(baseline, local);
                        assert_eq!(requests.len(), 2);
                        assert!(
                            String::from_utf8_lossy(&requests[1]).contains("If-Match: \"fresh\"")
                        );
                    }
                    Choice::Remote => {
                        assert_eq!(action, SyncAction::Download);
                        assert_eq!(baseline, remote);
                        assert_eq!(requests.len(), 1);
                    }
                }
            }
        }
    }
    let mut transport = fixture(vec![
        reply(Some(&remote), true),
        b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\n\r\n".to_vec(),
    ]);
    assert!(matches!(
        request(Some(&local), None).resolve(
            &mut doc,
            &mut transport,
            Choice::Local,
            Some((&remote, &tag))
        ),
        Err(Error::RemoteChanged)
    ));
}
#[test]
fn resolution_of_absence_creates_only_after_a_fresh_absent_response() {
    let mut doc = document();
    let local = record(&doc, 1);
    let mut transport = fixture(vec![
        reply(None, true),
        b"HTTP/1.1 201 Created\r\nETag: \"created\"\r\nContent-Length: 0\r\n\r\n".to_vec(),
    ]);
    assert!(matches!(
        request(Some(&local), None)
            .resolve(&mut doc, &mut transport, Choice::Local, None)
            .unwrap(),
        Outcome::Synchronized {
            action: SyncAction::Create,
            ..
        }
    ));
    assert!(String::from_utf8_lossy(&transport.requests.borrow()[1]).contains("If-None-Match: *"));
    let mut transport = fixture(vec![reply(Some(&local), true)]);
    assert!(matches!(
        request(Some(&local), None)
            .resolve(&mut doc, &mut transport, Choice::Local, None)
            .unwrap(),
        Outcome::Conflict { .. }
    ));
    assert_eq!(transport.requests.borrow().len(), 1);
}
#[test]
fn conflict_choice_rejects_local_navigation_before_starting_another_exchange() {
    let mut reader = live_reader();
    let mut remote = reader.progress_snapshot();
    remote.position.byte = 1;
    let completed = completion(&mut reader, Some(&remote), Arc::new(AtomicBool::new(false)));
    let crate::reader::Commit::Conflict { conflict } = completed
        .commit(&mut reader, 2, |_| panic!("conflict persisted"))
        .unwrap()
    else {
        panic!("expected conflict")
    };
    assert_eq!(conflict.remote(), Some(&remote));
    reader
        .dispatch(femto_reader::controller::Action::Next)
        .unwrap();
    assert!(conflict.choose(&reader, Choice::Local).is_err());
}

#[test]
fn reset_persists_only_the_checkpoint_removal_and_allows_a_new_source() {
    let mut reader = live_reader();
    assert!(crate::reader::Reset::prepare(&reader).is_none());
    completion(&mut reader, None, Arc::new(AtomicBool::new(false)))
        .commit(&mut reader, 2, |_| Ok(()))
        .unwrap();
    let reset = crate::reader::Reset::prepare(&reader).unwrap();
    reader.state.frontlight.brightness = 3;
    let id = reader.document.metadata.id.clone();
    let position = reader.position();
    let frame = reader.render(2).unwrap();
    let mut expected = reader.state.clone();
    expected.books.get_mut(&id).unwrap().sync = None;
    let directory = tempfile::tempdir().unwrap();
    let (mut journal, _) = femto_reader::journal::StateJournal::open(directory.path()).unwrap();
    journal.save(&reader.state).unwrap();
    reset
        .commit(&mut reader, |state| journal.save(state))
        .unwrap();
    assert_eq!(reader.position(), position);
    assert_eq!(reader.render(2).unwrap(), frame);
    let saved = femto_reader::journal::StateJournal::open(directory.path())
        .unwrap()
        .1;
    assert_eq!(
        serde_json::to_vec(&reader.state).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
    assert_eq!(
        serde_json::to_vec(&saved).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
    assert!(crate::reader::Session::prepare(&reader, "https://new.example/progress/book").is_ok());
}
#[test]
fn reset_rejects_changed_snapshots_and_preserves_state_on_write_failure() {
    for mode in 0..3 {
        let mut reader = live_reader();
        completion(&mut reader, None, Arc::new(AtomicBool::new(false)))
            .commit(&mut reader, 2, |_| Ok(()))
            .unwrap();
        let reset = crate::reader::Reset::prepare(&reader).unwrap();
        match mode {
            0 => reader.record_position(3),
            1 => {
                reader
                    .state
                    .books
                    .get_mut(&reader.document.metadata.id)
                    .unwrap()
                    .sync
                    .as_mut()
                    .unwrap()
                    .etag = "\"changed\"".into()
            }
            _ => (),
        }
        let before = serde_json::to_vec(&reader.state).unwrap();
        let position = reader.position();
        let result = reset.commit(&mut reader, |_| {
            assert_eq!(mode, 2, "stale reset reached storage");
            Err(io::Error::other("reset write failure").into())
        });
        assert!(result.is_err());
        assert_eq!(reader.position(), position);
        assert_eq!(serde_json::to_vec(&reader.state).unwrap(), before);
    }
}

#[test]
fn sync_commit_preserves_clipping_edits_made_after_exchange() {
    use femto_reader::{clipping::Clipping, journal::StateJournal};
    for remove in [false, true] {
        let mut reader = live_reader();
        let id = reader.document.metadata.id.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        completion(&mut reader, None, cancel.clone())
            .commit(&mut reader, 2, |_| Ok(()))
            .unwrap();
        let mut remote = reader.progress_snapshot();
        remote.position.byte = 1;
        let completed = completion(&mut reader, Some(&remote), cancel);
        let old = reader.state.books[&id].clippings[0].clone();
        if remove {
            reader.state.remove_clipping(&old).unwrap();
        } else {
            reader.toggle_clipping_highlight(0).unwrap();
        }
        let mut added = Clipping::capture(
            &mut reader.document,
            Locator::default()..Locator {
                byte: 1,
                ..Locator::default()
            },
            19,
        )
        .unwrap();
        added.highlighted = true;
        reader.state.add_clipping(added.clone()).unwrap();
        let expected = reader.state.books[&id].clippings.clone();
        let directory = tempfile::tempdir().unwrap();
        let (mut journal, _) = StateJournal::open(directory.path()).unwrap();
        completed
            .commit(&mut reader, 3, |state| journal.save(state))
            .unwrap();
        assert_eq!(reader.position(), remote.position);
        assert_eq!(reader.state.books[&id].clippings, expected);
        let (_, restored) = StateJournal::open(directory.path()).unwrap();
        assert_eq!(restored.books[&id].clippings, expected);
        assert_eq!(restored.books[&id].position, remote.position);
        assert_eq!(restored.books[&id].sync.as_ref().unwrap().baseline, remote);
        assert_eq!(restored.books[&id].clippings.last(), Some(&added));
        assert_eq!(restored.books[&id].bookmarks[0].label, "Keep");
    }
}
