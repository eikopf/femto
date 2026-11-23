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
            femto_catalog::http::fetch(
                &mut transport,
                url.as_str(),
                femto_catalog::deadline::Limit::Until(deadline),
                cancelled.clone()
            ),
            Err(femto_catalog::http::Error::Status(401))
        ),
        "unauthenticated catalog access was not refused"
    );
    let feed = femto_catalog::http::fetch_authenticated(
        &mut transport,
        url.as_str(),
        femto_catalog::deadline::Limit::Until(deadline),
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
                "epub_exact":true,"epub_noclobber":true,"epub_bytes":result["bytes"]
        })
    );
    Ok(())
}
