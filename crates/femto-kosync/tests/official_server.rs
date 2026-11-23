//! Opt-in test against an isolated official KOReader server, started by scripts/check-kosync.
use femto_catalog::http::{Stream, Transport, Url};
use femto_kosync::{Client, Credentials, DocumentId, Error, Progress};
use std::{
    io,
    net::{SocketAddr, TcpStream},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

struct Loopback;
impl Transport for Loopback {
    fn connect(
        &mut self,
        url: &Url,
        _: femto_catalog::deadline::Limit,
        _: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        let address = SocketAddr::from(([127, 0, 0, 1], url.port().unwrap()));
        let stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(Box::new(stream))
    }
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(15)
}
fn cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
#[test]
#[ignore = "requires the isolated official server from scripts/check-kosync"]
fn official_server_accepts_auth_and_progress_and_returns_missing_documents() {
    let server = std::env::var("FEMTO_KOSYNC_TEST_URL").unwrap();
    let user = format!("femto-fixture-{}", std::process::id());
    let credentials = Credentials::from_password(&user, "private-fixture-password").unwrap();
    let client = Client::new(&server, credentials, true).unwrap();
    let mut transport = Loopback;
    assert!(matches!(
        client.authorize(
            &mut transport,
            femto_catalog::deadline::Limit::Until(deadline()),
            cancel()
        ),
        Err(Error::Unauthorized)
    ));
    client
        .register(
            &mut transport,
            femto_catalog::deadline::Limit::Until(deadline()),
            cancel(),
        )
        .unwrap();
    client
        .authorize(
            &mut transport,
            femto_catalog::deadline::Limit::Until(deadline()),
            cancel(),
        )
        .unwrap();
    assert!(matches!(
        client.register(
            &mut transport,
            femto_catalog::deadline::Limit::Until(deadline()),
            cancel()
        ),
        Err(Error::Status(402))
    ));
    let wrong = Client::new(
        &server,
        Credentials::from_password(&user, "wrong").unwrap(),
        true,
    )
    .unwrap();
    assert!(matches!(
        wrong.authorize(
            &mut transport,
            femto_catalog::deadline::Limit::Until(deadline()),
            cancel()
        ),
        Err(Error::Unauthorized)
    ));
    let id = DocumentId::filename("fixture.epub").unwrap();
    assert!(
        client
            .pull(
                &mut transport,
                &id,
                femto_catalog::deadline::Limit::Until(deadline()),
                cancel()
            )
            .unwrap()
            .is_none()
    );
    let mut progress = Progress {
        document: id.clone(),
        progress: "/body/DocFragment[1]/body/p/text().5".into(),
        percentage: 0.25,
        device: "Femto fixture".into(),
        device_id: "device-1".into(),
    };
    let first = client
        .push(
            &mut transport,
            &progress,
            femto_catalog::deadline::Limit::Until(deadline()),
            cancel(),
        )
        .unwrap();
    let remote = client
        .pull(
            &mut transport,
            &id,
            femto_catalog::deadline::Limit::Until(deadline()),
            cancel(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(remote.position, progress);
    assert_eq!(remote.timestamp, first.timestamp);
    assert!(matches!(
        wrong.push(
            &mut transport,
            &progress,
            femto_catalog::deadline::Limit::Until(deadline()),
            cancel()
        ),
        Err(Error::Unauthorized)
    ));
    assert_eq!(
        client
            .pull(
                &mut transport,
                &id,
                femto_catalog::deadline::Limit::Until(deadline()),
                cancel()
            )
            .unwrap()
            .unwrap(),
        remote
    );
    progress.progress = "/body/DocFragment[2]/body/p/text().7".into();
    progress.percentage = 0.75;
    progress.device_id = "device-2".into();
    client
        .push(
            &mut transport,
            &progress,
            femto_catalog::deadline::Limit::Until(deadline()),
            cancel(),
        )
        .unwrap();
    assert_eq!(
        client
            .pull(
                &mut transport,
                &id,
                femto_catalog::deadline::Limit::Until(deadline()),
                cancel()
            )
            .unwrap()
            .unwrap()
            .position,
        progress
    );
}
