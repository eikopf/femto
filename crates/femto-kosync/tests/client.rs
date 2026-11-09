use femto_catalog::http::{Stream, Transport, Url};
use femto_kosync::{Client, Credentials, DocumentId, Error, Progress};
use std::{
    io::{self, Cursor, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Default)]
struct Fixture {
    reply: Vec<u8>,
    writes: Arc<Mutex<Vec<u8>>>,
    urls: Vec<String>,
}
struct Connection {
    input: Cursor<Vec<u8>>,
    output: Arc<Mutex<Vec<u8>>>,
}
impl Read for Connection {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.input.read(out)
    }
}
impl Write for Connection {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.output.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Transport for Fixture {
    fn connect(
        &mut self,
        url: &Url,
        _: Instant,
        _: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        self.urls.push(url.to_string());
        Ok(Box::new(Connection {
            input: Cursor::new(self.reply.clone()),
            output: self.writes.clone(),
        }))
    }
}
fn fixture(status: u16, json: &str) -> Fixture {
    Fixture { reply: format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{json}", json.len()).into_bytes(), ..Default::default() }
}
fn client() -> Client {
    Client::new(
        "https://peer.test/kosync",
        Credentials::from_password("alice", "password").unwrap(),
        false,
    )
    .unwrap()
}
fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(10)
}
fn cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
fn id() -> DocumentId {
    DocumentId::filename("Café.epub").unwrap()
}
fn position() -> Progress {
    Progress {
        document: id(),
        progress: "/body/DocFragment[1]/body/p/text().5".into(),
        percentage: 0.25,
        device: "Femto".into(),
        device_id: "fixture-device".into(),
    }
}
#[test]
fn registration_and_authorization_use_koreader_credentials() {
    let mut transport = fixture(201, r#"{"username":"alice"}"#);
    client()
        .register(&mut transport, deadline(), cancel())
        .unwrap();
    assert_eq!(transport.urls, ["https://peer.test/kosync/users/create"]);
    let bytes = transport.writes.lock().unwrap();
    let request = std::str::from_utf8(&bytes).unwrap();
    let (headers, body) = request.split_once("\r\n\r\n").unwrap();
    assert!(headers.contains("x-auth-user: alice\r\n"));
    assert!(headers.contains("x-auth-key: 5f4dcc3b5aa765d61d8327deb882cf99\r\n"));
    assert!(headers.contains("Accept: application/vnd.koreader.v1+json\r\n"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(body).unwrap(),
        serde_json::json!({"username":"alice","password":"5f4dcc3b5aa765d61d8327deb882cf99"})
    );
    let mut transport = fixture(200, r#"{"authorized":"OK"}"#);
    client()
        .authorize(&mut transport, deadline(), cancel())
        .unwrap();
    assert!(
        transport
            .writes
            .lock()
            .unwrap()
            .starts_with(b"GET /kosync/users/auth HTTP/1.1\r\n")
    );
}
#[test]
fn progress_matches_wire_contract_and_missing_is_empty_object() {
    let mut transport = fixture(200, "{}");
    assert_eq!(
        client()
            .pull(&mut transport, &id(), deadline(), cancel())
            .unwrap(),
        None
    );
    let mut value = serde_json::to_value(position()).unwrap();
    value["timestamp"] = 1730000000u64.into();
    let mut transport = fixture(200, &value.to_string());
    let remote = client()
        .pull(&mut transport, &id(), deadline(), cancel())
        .unwrap()
        .unwrap();
    assert_eq!(remote.position, position());
    assert_eq!(remote.timestamp, 1730000000);
    assert!(transport.urls[0].ends_with(id().as_str()));
    let mut transport = fixture(
        200,
        &serde_json::json!({"document": id(), "timestamp":1730000001}).to_string(),
    );
    let receipt = client()
        .push(&mut transport, &position(), deadline(), cancel())
        .unwrap();
    assert_eq!(receipt.timestamp, 1730000001);
    let request = String::from_utf8(transport.writes.lock().unwrap().clone()).unwrap();
    assert!(request.starts_with("PUT /kosync/syncs/progress HTTP/1.1\r\n"));
    assert!(!request.contains("If-Match"));
    assert_eq!(
        serde_json::from_str::<Progress>(request.split_once("\r\n\r\n").unwrap().1).unwrap(),
        position()
    );
}
#[test]
fn server_cannot_redirect_credentials_or_substitute_a_document() {
    let mut transport = fixture(302, "{}");
    transport.reply =
        b"HTTP/1.1 302 Found\r\nLocation: https://elsewhere.test/\r\nContent-Length: 0\r\n\r\n"
            .to_vec();
    assert!(matches!(
        client().pull(&mut transport, &id(), deadline(), cancel()),
        Err(Error::Status(302))
    ));
    assert_eq!(transport.urls.len(), 1);
    for invalid in [
        "[]",
        "null",
        r#"{"progress":"1"}"#,
        r#"{"document":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","progress":"1","percentage":0.1,"device":"Other","timestamp":1}"#,
    ] {
        let mut transport = fixture(200, invalid);
        assert!(
            client()
                .pull(&mut transport, &id(), deadline(), cancel())
                .is_err()
        );
    }
    let mut transport = fixture(401, "do not expose this response");
    assert_eq!(
        client()
            .authorize(&mut transport, deadline(), cancel())
            .unwrap_err()
            .to_string(),
        "KOReader authentication failed"
    );
}
#[test]
fn bounds_and_framing_prevent_partial_results() {
    for response in [
        "HTTP/1.1 200 OK\r\nContent-Length: 999999\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 20\r\n\r\n{}",
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nTransfer-Encoding: chunked\r\n\r\n{}",
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 2\r\n\r\n{}",
    ] {
        let mut transport = Fixture {
            reply: response.into(),
            ..Default::default()
        };
        assert!(
            client()
                .pull(&mut transport, &id(), deadline(), cancel())
                .is_err()
        );
    }
    let mut transport = Fixture { reply: b"HTTP/1.1 200 OK\r\nContent-Type: application/vnd.koreader.v1+json\r\nTransfer-Encoding: chunked\r\n\r\n1\r\n{\r\n1\r\n}\r\n0\r\n\r\n".to_vec(), ..Default::default() };
    assert_eq!(
        client()
            .pull(&mut transport, &id(), deadline(), cancel())
            .unwrap(),
        None
    );
}
#[test]
fn validation_and_cancellation_precede_connection() {
    let credentials = Credentials::from_password("alice", "secret").unwrap();
    assert_eq!(format!("{credentials:?}"), "Credentials([redacted])");
    for url in [
        "http://peer.test",
        "https://alice:secret@peer.test",
        "https://peer.test/?secret",
        "https://peer.test/#secret",
        "file:///tmp/service",
    ] {
        assert!(Client::new(url, credentials.clone(), false).is_err());
    }
    assert!(Client::new("http://127.0.0.1:7200", credentials, true).is_ok());
    assert!(Credentials::from_password("alice\r\nx-evil: yes", "secret").is_err());
    let mut transport = fixture(200, "{}");
    let cancelled = cancel();
    cancelled.store(true, Ordering::Release);
    assert!(
        client()
            .pull(&mut transport, &id(), deadline(), cancelled)
            .is_err()
    );
    assert!(
        client()
            .pull(&mut transport, &id(), Instant::now(), cancel())
            .is_err()
    );
    let mut invalid = position();
    invalid.percentage = f64::NAN;
    assert!(
        client()
            .push(&mut transport, &invalid, deadline(), cancel())
            .is_err()
    );
    assert!(transport.urls.is_empty());
}
