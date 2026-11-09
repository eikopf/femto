use femto_catalog::http::{self, Stream, Transport, Url};
use std::{
    collections::VecDeque,
    io::{self, Cursor, Read, Write},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
const SOURCE: &str = "https://catalog.example/start";
const FEED: &[u8] = include_bytes!("../../../tests/fixtures/catalog.json");
struct Fragmented {
    input: Cursor<Vec<u8>>,
    output: Arc<Mutex<Vec<u8>>>,
}
impl Read for Fragmented {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        let n = b.len().min(3);
        self.input.read(&mut b[..n])
    }
}
impl Write for Fragmented {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.output.lock().unwrap().extend(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[derive(Default)]
struct Mock {
    responses: VecDeque<Vec<u8>>,
    requests: Vec<Arc<Mutex<Vec<u8>>>>,
    urls: Vec<String>,
}
impl Transport for Mock {
    fn connect(
        &mut self,
        url: &Url,
        _: Instant,
        _: Arc<AtomicBool>,
    ) -> io::Result<Box<dyn Stream>> {
        self.urls.push(url.to_string());
        let output = Arc::new(Mutex::new(Vec::new()));
        self.requests.push(output.clone());
        Ok(Box::new(Fragmented {
            input: Cursor::new(self.responses.pop_front().expect("unexpected request")),
            output,
        }))
    }
}
fn fetch(mock: &mut Mock) -> Result<femto_catalog::Catalog, http::Error> {
    http::fetch(
        mock,
        SOURCE,
        Instant::now() + Duration::from_secs(5),
        Arc::new(AtomicBool::new(false)),
    )
}
fn reply(headers: &str, body: &[u8]) -> Vec<u8> {
    [
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/opds+json; charset=utf-8\r\n{headers}\r\n"
        )
        .as_bytes(),
        body,
    ]
    .concat()
}
#[test]
fn fragmented_length_chunked_and_close_delimited_feeds_work() {
    let chunks = [
        format!("{:x};extension=x\r\n", FEED.len()).as_bytes(),
        FEED,
        b"\r\n0\r\nX-Trailer: ignored\r\n\r\n",
    ]
    .concat();
    for response in [
        reply(&format!("Content-Length: {}\r\n", FEED.len()), FEED),
        reply("Transfer-Encoding: chunked\r\n", &chunks),
        reply("", FEED),
    ] {
        let mut mock = Mock {
            responses: [response].into(),
            ..Default::default()
        };
        assert_eq!(fetch(&mut mock).unwrap().title, "Femto catalog specimen");
        let requests = mock.requests[0].lock().unwrap();
        let request = std::str::from_utf8(&requests).unwrap();
        assert!(request.starts_with("GET /start HTTP/1.1\r\nHost: catalog.example\r\n"));
        assert!(request.contains("Accept-Encoding: identity\r\n"));
        assert!(!request.contains("Authorization:"));
    }
}
#[test]
fn redirects_change_relative_link_base_and_never_downgrade_https() {
    let mut mock = Mock {
        responses: [
            b"HTTP/1.1 302 Found\r\nLocation: /feeds/catalog.json\r\n\r\n".to_vec(),
            reply("", FEED),
        ]
        .into(),
        ..Default::default()
    };
    let catalog = fetch(&mut mock).unwrap();
    assert_eq!(catalog.source, "https://catalog.example/feeds/catalog.json");
    assert_eq!(
        catalog.navigation[0].resolved.as_deref(),
        Some("https://catalog.example/feeds/recent.json")
    );
    for location in [
        "http://catalog.example/",
        "https://user:secret@catalog.example/",
        "file:///etc/passwd",
        "/start",
    ] {
        let mut mock = Mock {
            responses: [format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\n\r\n").into_bytes()]
                .into(),
            ..Default::default()
        };
        assert!(fetch(&mut mock).is_err());
        assert_eq!(mock.urls.len(), 1);
    }
    let mut mock = Mock {
        responses: (0..6)
            .map(|i| format!("HTTP/1.1 302 Found\r\nLocation: /redirect/{i}\r\n\r\n").into_bytes())
            .collect(),
        ..Default::default()
    };
    assert!(matches!(fetch(&mut mock), Err(http::Error::Redirect)));
    assert_eq!(mock.urls.len(), 6);
}
#[test]
fn malformed_framing_status_and_content_type_do_not_return_partial_catalogs() {
    for response in [
        reply("Content-Length: 1\r\nContent-Length: 1\r\n", b"x"),
        reply("Content-Length: 1\r\nTransfer-Encoding: chunked\r\n", b"x"),
        reply("Content-Length: 524289\r\n", b""),
        reply("Content-Length: 20\r\n", b"{}"),
        reply("Content-Encoding: gzip\r\n", FEED),
        reply("Transfer-Encoding: chunked\r\n", b"100000\r\n"),
        reply("Transfer-Encoding: chunked\r\n", b"2\r\n{}xx0\r\n\r\n"),
        b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=books\r\n\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n".to_vec(),
        format!("HTTP/1.1 200 OK\r\nX-Long: {}\r\n\r\n", "a".repeat(16384)).into_bytes(),
    ] {
        let mut mock = Mock {
            responses: [response].into(),
            ..Default::default()
        };
        assert!(fetch(&mut mock).is_err());
    }
}
#[test]
fn cancellation_and_deadline_prevent_connection() {
    let mut mock = Mock::default();
    assert!(
        http::fetch(
            &mut mock,
            SOURCE,
            Instant::now() + Duration::from_secs(1),
            Arc::new(AtomicBool::new(true))
        )
        .is_err()
    );
    assert!(
        http::fetch(
            &mut mock,
            SOURCE,
            Instant::now(),
            Arc::new(AtomicBool::new(false))
        )
        .is_err()
    );
    assert!(mock.urls.is_empty());
}

#[test]
fn basic_credentials_follow_only_the_configured_origin() {
    use femto_catalog::auth::BasicAuth;
    let auth = BasicAuth::from_json(
        SOURCE,
        br#"{"username":"Aladdin","password":"open sesame"}"#,
    )
    .unwrap()
    .clone();
    assert!(!format!("{auth:?}").contains("Aladdin"));
    for (location, forwarded) in [
        ("/next", true),
        ("https://other.example/next", false),
        ("https://catalog.example:444/next", false),
    ] {
        let mut mock = Mock {
            responses: [
                format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\n\r\n").into_bytes(),
                reply("", FEED),
            ]
            .into(),
            ..Default::default()
        };
        http::fetch_authenticated(
            &mut mock,
            SOURCE,
            Instant::now() + Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
            Some(&auth),
        )
        .unwrap();
        let expected = "Authorization: Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==\r\n";
        assert!(
            std::str::from_utf8(&mock.requests[0].lock().unwrap())
                .unwrap()
                .contains(expected)
        );
        assert_eq!(
            std::str::from_utf8(&mock.requests[1].lock().unwrap())
                .unwrap()
                .contains(expected),
            forwarded
        );
    }
    assert!(BasicAuth::new("http://catalog.example/", "user", "password", false).is_err());
    assert!(BasicAuth::new("http://catalog.example/", "user", "password", true).is_ok());
    for (user, password) in [
        ("user:name", "ok"),
        ("user\n", "ok"),
        ("user", "bad\r\nheader"),
        ("user", "é"),
    ] {
        assert!(BasicAuth::new(SOURCE, user, password, false).is_err());
    }
    assert!(BasicAuth::new(SOURCE, "user", &"a".repeat(1025), false).is_err());
}

#[test]
fn authentication_rejection_does_not_retry_or_emit_a_partial_catalog() {
    let auth = femto_catalog::auth::BasicAuth::new(SOURCE, "user", "wrong", false).unwrap();
    let mut mock = Mock {
        responses: [
            b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=books\r\n\r\n".to_vec(),
        ]
        .into(),
        ..Default::default()
    };
    assert!(matches!(
        http::fetch_authenticated(
            &mut mock,
            SOURCE,
            Instant::now() + Duration::from_secs(5),
            Arc::new(AtomicBool::new(false)),
            Some(&auth)
        ),
        Err(http::Error::Status(401))
    ));
    assert_eq!(mock.requests.len(), 1);
}

#[test]
fn epub_download_streams_bounded_writes_and_honors_mid_body_cancellation() {
    use std::sync::atomic::Ordering;
    struct Sink {
        bytes: Vec<u8>,
        largest: usize,
        cancel: Option<Arc<AtomicBool>>,
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.largest = self.largest.max(bytes.len());
            self.bytes.extend(bytes);
            if let Some(flag) = &self.cancel {
                flag.store(true, Ordering::Release);
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let payload = vec![42; 24_000];
    for cancel in [false, true] {
        let flag = Arc::new(AtomicBool::new(false));
        let mut sink = Sink {
            bytes: vec![],
            largest: 0,
            cancel: cancel.then(|| flag.clone()),
        };
        let response = [format!("HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nContent-Length: {}\r\n\r\n", payload.len()).as_bytes(), &payload].concat();
        let mut mock = Mock {
            responses: [response].into(),
            ..Default::default()
        };
        let result = http::download_epub(
            &mut mock,
            SOURCE,
            Instant::now() + Duration::from_secs(5),
            flag,
            None,
            &mut sink,
        );
        if cancel {
            assert!(
                matches!(result, Err(http::Error::Io(error)) if error.kind() == io::ErrorKind::ConnectionAborted)
            );
            assert!(sink.bytes.len() < payload.len());
        } else {
            assert_eq!(result.unwrap().bytes, payload.len());
            assert_eq!(sink.bytes, payload);
        }
        assert!(sink.largest <= 8192);
    }
}

#[test]
fn oversized_or_truncated_downloads_and_storage_failures_are_errors() {
    for response in [
        format!("HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nContent-Length: {}\r\n\r\n", http::MAX_EPUB_BYTES + 1).into_bytes(),
        b"HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nTransfer-Encoding: chunked\r\n\r\n4000001\r\n".to_vec(),
        b"HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nContent-Length: 100\r\n\r\nshort".to_vec(),
    ] {
        let mut mock = Mock { responses: [response].into(), ..Default::default() };
        assert!(http::download_epub(&mut mock, SOURCE, Instant::now() + Duration::from_secs(5), Arc::new(AtomicBool::new(false)), None, &mut Vec::new()).is_err());
    }
    struct FailedDisk;
    impl Write for FailedDisk {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("disk failed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut mock = Mock { responses: [b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: 2\r\n\r\nPK".to_vec()].into(), ..Default::default() };
    assert!(
        matches!(http::download_epub(&mut mock, SOURCE, Instant::now() + Duration::from_secs(5), Arc::new(AtomicBool::new(false)), None, &mut FailedDisk), Err(http::Error::Io(error)) if error.to_string() == "disk failed")
    );
}
