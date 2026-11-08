//! Application qualification against the private Headscale fixture.
#[allow(dead_code)]
#[path = "../src/catalog.rs"]
mod catalog;
#[path = "../../femto-microlink/examples/support/transport.rs"]
mod fixture_transport;

fn main() -> anyhow::Result<()> {
    use anyhow::ensure;
    use femto_microlink::{
        application_tcp::{Shared, ThreadShared},
        bootstrap,
        control_session::{ControlSession, Endpoint},
        control_worker::ControlledNetwork,
        coordinator::Clock,
        enrollment::Outcome,
        host_tcp_tls,
        host_tls::TlsConnector,
        network::Network,
        protocol::{
            CAPABILITY_VERSION,
            control::{HostInfo, MapRequest, RegisterRequest},
        },
        relay::Certificate,
        socket_udp::UdpTransport,
        store::HostStore,
        timestamp::TimestampClock,
    };
    use std::{
        fs,
        net::SocketAddr,
        path::Path,
        sync::{Arc, atomic::AtomicBool},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    let args: Vec<_> = std::env::args().collect();
    ensure!(
        args.len() == 8 || (args.len() == 9 && args[8] == "relay"),
        "usage: headscale-services LOOPBACK:PORT ROOT.der IDENTITY FEED CREDENTIALS OUTPUT.epub EXPECTED.epub [relay]"
    );
    let relay_only = args.get(8).is_some();
    let address: SocketAddr = args[1].parse()?;
    ensure!(
        address.ip().is_loopback(),
        "fixture coordinator must be loopback"
    );
    let url = url::Url::parse(&args[4])?;
    ensure!(
        url.scheme() == "https" && url.host_str() == Some("femto-independent-peer.femto.test"),
        "expected fixture HTTPS peer"
    );
    let mut roots = rustls::RootCertStore::empty();
    roots.add(rustls::pki_types::CertificateDer::from(fs::read(&args[2])?))?;
    let connector = TlsConnector::with_roots(roots.clone(), Duration::from_secs(60))?;
    let cancelled = Arc::new(AtomicBool::new(false));
    let connect = || {
        connector.connect(
            address,
            "localhost",
            &Certificate::Name("localhost".into()),
            cancelled.clone(),
        )
    };
    let key = bootstrap::exchange(connect()?, "localhost", address.port(), CAPABILITY_VERSION)?;
    let store = HostStore::open(&args[3])?;
    let identity = store.identity()?.clone();
    let endpoint = Endpoint {
        host: "localhost".into(),
        port: address.port(),
        control_key: key,
        version: CAPABILITY_VERSION,
    };
    let mut control = ControlSession::upgrade(
        connect()?,
        &endpoint,
        identity.machine_key(),
        Duration::from_secs(60),
        cancelled.clone(),
    )?;
    let hostinfo = HostInfo::esp32("femto-independent-probe", "");
    let registration = RegisterRequest::new(
        CAPABILITY_VERSION,
        identity.node_key().public(),
        identity.network_lock_key().public(),
        hostinfo.clone(),
    );
    ensure!(
        control.register(&registration)? == Outcome::Authorized,
        "existing fixture identity not authorized"
    );
    let map = MapRequest::new(
        CAPABILITY_VERSION,
        identity.node_key().public(),
        identity.disco_key().public(),
        hostinfo,
    )
    .streaming();
    let timestamps = TimestampClock::open(
        store,
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?;
    let network = Network::new(&identity, timestamps, 64, 4)?;
    let mut owner = ControlledNetwork::start(control, map, network)?;
    let started = Instant::now();
    let clock = move || Clock {
        unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64,
        monotonic_ms: started.elapsed().as_millis() as u64,
    };
    let mut relays = femto_microlink::host_derp::Pool::default();
    if relay_only {
        while relays.is_empty() {
            ensure!(
                started.elapsed() < Duration::from_secs(25),
                "catalog fixture relay setup timed out"
            );
            fixture_transport::pump_fixture_relay(
                &mut owner,
                &mut relays,
                &connector,
                address,
                identity.node_key(),
                clock,
            )
            .map_err(|error| anyhow::anyhow!(error.to_string()))?;
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let driver = fixture_transport::FixtureDriver {
        owner,
        udp: if relay_only {
            None
        } else {
            Some(UdpTransport::bind("0.0.0.0:0".parse()?)?)
        },
        relays,
        clock,
        direct_sent: 0,
        relay_events: 0,
    };
    let shared = ThreadShared::new(driver);
    let mut transport = femto_catalog::tailnet::Tailnet::with_tls(
        Shared::new(shared.clone()),
        host_tcp_tls::Connector::with_roots(roots)?,
    );
    let deadline = started + Duration::from_secs(60);
    let auth_bytes = zeroize::Zeroizing::new(fs::read(&args[5])?);
    let auth = femto_catalog::auth::BasicAuth::from_json(url.as_str(), &auth_bytes)?;
    ensure!(
        matches!(
            femto_catalog::http::fetch(&mut transport, url.as_str(), deadline, cancelled.clone()),
            Err(femto_catalog::http::Error::Status(401))
        ),
        "unauthenticated catalog access was not refused"
    );
    let feed = femto_catalog::http::fetch_authenticated(
        &mut transport,
        url.as_str(),
        deadline,
        cancelled.clone(),
        Some(&auth),
    )?;
    ensure!(
        feed.title == "Independent fixture" && feed.publications.len() == 1,
        "catalog metadata mismatch"
    );
    ensure!(
        feed.source == url.join("/catalog/feed.json")?.as_str(),
        "catalog redirect source mismatch"
    );
    let acquisition = catalog::acquisition_url(&feed, 1, 1)?;
    ensure!(
        acquisition == url.join("/books/book.epub")?.as_str(),
        "relative acquisition resolution mismatch"
    );
    let output = Path::new(&args[6]);
    let result = catalog::AcquisitionFile::new(output)?.receive(
        &mut transport,
        acquisition,
        deadline,
        cancelled.clone(),
        Some(&auth),
    )?;
    ensure!(
        fs::read(output)? == fs::read(&args[7])?,
        "downloaded EPUB differs from fixture"
    );
    ensure!(
        catalog::AcquisitionFile::new(output).is_err(),
        "existing download was replaceable"
    );
    qualify_progress(
        &mut transport,
        url.join("/progress/book")?.as_str(),
        &auth,
        output,
        deadline,
        cancelled.clone(),
    )?;
    qualify_reader_commit(
        &mut transport,
        url.join("/progress/book")?.as_str(),
        &auth,
        output,
        deadline,
        cancelled,
    )?;
    drop(transport);
    let (direct_sent, relay_events) =
        shared.with(|driver| Ok((driver.direct_sent, driver.relay_events)))?;
    ensure!(
        !relay_only || (direct_sent == 0 && relay_events > 0),
        "catalog relay-only evidence missing"
    );
    ensure!(
        shared.with(|driver| Ok(driver.owner.close_and_wait(Duration::from_secs(2))))?,
        "catalog control worker did not join"
    );
    println!(
        "{}",
        serde_json::json!({"mode":if relay_only {"derp-services"} else {"direct-services"},
        "derp_traffic":relay_only,"direct_udp_sent":direct_sent,"relay_events":relay_events,
        "joined_shutdown":true,"independent_opds":true,"unauthenticated_denied":true,
        "authenticated_redirect":true,"relative_acquisition":true,"epub_validated":true,
        "epub_exact":true,"epub_noclobber":true,"epub_bytes":result["bytes"],
        "progress_create":true,"progress_upload":true,"progress_conflict":true,
        "progress_download_resolution":true,"progress_precondition_race":true,
        "progress_stale_resolution":true,"progress_local_resolution":true,"progress_converged":true,
        "reader_download_journal":true,"reader_upload_journal":true,
        "reader_stale_completion_rejected":true,"reader_reopen_position":true,
        "reader_unrelated_state_preserved":true})
    );
    Ok(())
}

fn qualify_progress(
    transport: &mut impl femto_catalog::http::Transport,
    source: &str,
    auth: &femto_catalog::auth::BasicAuth,
    path: &std::path::Path,
    deadline: std::time::Instant,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<()> {
    use anyhow::{bail, ensure};
    use femto_reader::{
        document::{Block, Document},
        layout::Locator,
        progress::{Progress, SyncAction},
    };
    use femto_sync::{Choice, Outcome, Request};
    let mut document = Document::open_epub(path)?;
    let chapter = document.chapter(0)?;
    let (block, positions) = chapter
        .blocks
        .iter()
        .enumerate()
        .find_map(|(index, block)| {
            let Block::Text(paragraph) = block else {
                return None;
            };
            let positions: Vec<_> = paragraph
                .text
                .char_indices()
                .take(3)
                .map(|(byte, _)| byte)
                .collect();
            (positions.len() == 3).then_some((index, positions))
        })
        .ok_or_else(|| anyhow::anyhow!("progress fixture needs three text positions"))?;
    let records: Vec<_> = positions
        .into_iter()
        .map(|byte| Progress {
            version: 1,
            book_id: document.metadata.id.clone(),
            position: Locator {
                chapter: 0,
                block,
                byte,
            },
            recorded_at: 1,
        })
        .collect();
    let request = |local: usize, baseline: Option<usize>| Request {
        source,
        local: Some(&records[local]),
        baseline: baseline.map(|index| &records[index]),
        auth: Some(auth),
        deadline,
        cancelled: cancelled.clone(),
    };
    let check = |outcome, expected, position, tag: &str| -> anyhow::Result<()> {
        let Outcome::Synchronized {
            action,
            baseline,
            etag,
        } = outcome
        else {
            bail!("expected synchronized progress")
        };
        ensure!(
            action == expected && baseline == records[position] && etag.as_str() == tag,
            "progress action, baseline, or ETag mismatch"
        );
        Ok(())
    };
    check(
        request(0, None).execute(&mut document, transport)?,
        SyncAction::Create,
        0,
        "\"1\"",
    )?;
    check(
        request(1, Some(0)).execute(&mut document, transport)?,
        SyncAction::Upload,
        1,
        "\"2\"",
    )?;
    let Outcome::Conflict {
        remote: Some(remote),
        etag: Some(etag),
    } = request(2, Some(0)).execute(&mut document, transport)?
    else {
        bail!("diverged progress did not produce a conflict")
    };
    ensure!(
        remote == records[1] && etag.as_str() == "\"2\"",
        "conflict snapshot mismatch"
    );
    check(
        request(2, Some(0)).resolve(
            &mut document,
            transport,
            Choice::Remote,
            Some((&remote, &etag)),
        )?,
        SyncAction::Download,
        1,
        "\"2\"",
    )?;
    ensure!(
        matches!(
            request(2, Some(1)).execute(&mut document, transport),
            Err(femto_sync::Error::RemoteChanged)
        ),
        "concurrent update was not refused"
    );
    let Outcome::Conflict {
        remote: Some(fresh),
        etag: Some(fresh_tag),
    } = request(2, Some(1)).resolve(
        &mut document,
        transport,
        Choice::Local,
        Some((&remote, &etag)),
    )?
    else {
        bail!("stale resolution overwrote changed progress")
    };
    ensure!(
        fresh == records[0] && fresh_tag.as_str() == "\"3\"",
        "fresh conflict snapshot mismatch"
    );
    check(
        request(2, Some(1)).resolve(
            &mut document,
            transport,
            Choice::Local,
            Some((&fresh, &fresh_tag)),
        )?,
        SyncAction::Upload,
        2,
        "\"4\"",
    )?;
    check(
        request(2, Some(2)).execute(&mut document, transport)?,
        SyncAction::Converged,
        2,
        "\"4\"",
    )?;
    Ok(())
}

fn qualify_reader_commit(
    transport: &mut impl femto_catalog::http::Transport,
    source: &str,
    auth: &femto_catalog::auth::BasicAuth,
    path: &std::path::Path,
    deadline: std::time::Instant,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> anyhow::Result<()> {
    use anyhow::{bail, ensure};
    use femto_reader::{
        controller::{Action, Reader},
        document::Document,
        journal::StateJournal,
        layout::{Font, Fonts, Locator},
        progress::SyncAction,
        state::ReaderState,
    };
    use femto_sync::reader::{Commit, Session};
    let font = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/DejaVuSerif.ttf");
    let make_reader = |state| {
        Reader::new(
            Document::open_epub(path)?,
            Fonts::new(Font::load(&font)?),
            state,
        )
    };
    let mut reader = make_reader(ReaderState::default())?;
    let id = reader.document.metadata.id.clone();
    reader.record_position(1);
    reader.state.frontlight.brightness = 3;
    reader
        .state
        .toggle_bookmark(&id, Locator::default(), "Keep".into());
    let directory = tempfile::tempdir_in(path.parent().unwrap())?;
    let (mut journal, _) = StateJournal::open(directory.path())?;
    journal.save(&reader.state)?;
    let exchange = |session: Session, transport: &mut _| {
        session.exchange(
            &mut Document::open_epub(path)?,
            transport,
            Some(auth),
            deadline,
            cancelled.clone(),
        )
    };
    let completed = exchange(Session::prepare(&reader, source)?, transport)?;
    let Commit::Conflict { conflict } =
        completed.commit(&mut reader, 2, |_| panic!("conflict attempted persistence"))?
    else {
        bail!("fresh reader did not report differing remote progress")
    };
    let expected = conflict
        .remote()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("missing remote progress"))?;
    let completion = exchange(
        conflict.choose(&reader, femto_sync::Choice::Remote)?,
        transport,
    )?;
    ensure!(
        matches!(
            completion.commit(&mut reader, 2, |state| journal.save(state))?,
            Commit::Saved {
                action: SyncAction::Download,
                ..
            }
        ),
        "reader download was not committed"
    );
    drop(journal);
    let (mut journal, saved) = StateJournal::open(directory.path())?;
    let checkpoint = saved.books[&id].sync.as_ref().unwrap();
    ensure!(
        saved.books[&id].position == expected.position
            && checkpoint.baseline == expected
            && checkpoint.source == source
            && checkpoint.etag == "\"4\"",
        "reopened download/checkpoint mismatch"
    );
    ensure!(
        saved.books[&id].bookmarks[0].label == "Keep" && saved.frontlight.brightness == 3,
        "sync changed unrelated state"
    );
    ensure!(
        serde_json::to_vec(&saved)? == serde_json::to_vec(&reader.state)?,
        "journal and reader disagree"
    );
    reader = make_reader(saved)?;
    ensure!(
        reader.position() == expected.position,
        "reopened reader lost downloaded position"
    );
    let completed = exchange(Session::prepare(&reader, source)?, transport)?;
    reader.dispatch(Action::Bookmark(0))?;
    ensure!(
        reader.position() != expected.position,
        "stale completion fixture did not navigate"
    );
    ensure!(
        completed
            .commit(&mut reader, 3, |_| panic!(
                "stale completion attempted persistence"
            ))
            .is_err(),
        "stale reader completion was accepted"
    );
    drop(journal);
    let (reopened, saved) = StateJournal::open(directory.path())?;
    journal = reopened;
    ensure!(
        saved.books[&id].position == expected.position
            && saved.books[&id].sync.as_ref().unwrap().etag == "\"4\"",
        "stale completion changed journal"
    );
    reader = make_reader(saved)?;
    reader.dispatch(Action::Bookmark(0))?;
    reader.record_position(3);
    journal.save(&reader.state)?;
    let expected_upload = reader.progress_snapshot();
    let completed = exchange(Session::prepare(&reader, source)?, transport)?;
    ensure!(
        matches!(
            completed.commit(&mut reader, 4, |state| journal.save(state))?,
            Commit::Saved {
                action: SyncAction::Upload,
                ..
            }
        ),
        "reader upload was not committed"
    );
    drop(journal);
    let (_, saved) = StateJournal::open(directory.path())?;
    let checkpoint = saved.books[&id].sync.as_ref().unwrap();
    ensure!(
        checkpoint.baseline == expected_upload
            && checkpoint.etag == "\"5\""
            && checkpoint.source == source,
        "reopened upload checkpoint mismatch"
    );
    ensure!(
        serde_json::to_vec(&saved)? == serde_json::to_vec(&reader.state)?,
        "uploaded journal and reader disagree"
    );
    ensure!(
        make_reader(saved)?.position() == expected_upload.position,
        "reopened reader lost uploaded position"
    );
    Ok(())
}
