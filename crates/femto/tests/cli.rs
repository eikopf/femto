use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn read(dir: &Path, input: &[u8]) -> String {
    read_options(dir, input, &[])
}

fn read_options(dir: &Path, input: &[u8], options: &[&str]) -> String {
    read_book(dir, input, options, "sample.epub")
}

fn read_book(dir: &Path, input: &[u8], options: &[&str], book: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_femto"))
        .arg("read")
        .arg(fixture(book))
        .arg("--font")
        .arg(fixture("DejaVuSerif.ttf"))
        .arg("--fallback-font")
        .arg(fixture("DejaVuSans.ttf"))
        .arg("--state")
        .arg(dir.join("state.json"))
        .arg("--display")
        .arg(dir.join("display.png"))
        .args(options)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn contents_and_footnote_commands_follow_fragments_and_return() {
    let dir = tempfile::tempdir().unwrap();
    let output = read_book(
        dir.path(),
        b"t\nl\nl 1\nback\nt 2\nback\nq\n",
        &[],
        "navigation.epub",
    );
    assert!(output.contains("2: Footnote"));
    assert!(output.contains("the first footnote -> two.xhtml#note"));
    assert!(output.contains("chapter 2/2"));
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join("state.json")).unwrap()).unwrap();
    let book = state["books"].as_object().unwrap().values().next().unwrap();
    assert_eq!(book["position"]["chapter"], 0);
    assert_eq!(book["position"]["block"], 0);
}

#[test]
fn chapter_bookmark_commands_and_raster_settings_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let output = read_options(
        dir.path(),
        b"b\nc 2\nm 1\nc 999\nq\n",
        &[
            "--text-stroke",
            "0.2",
            "--text-gamma",
            "0.7",
            "--text-hinting",
            "false",
            "--fractional-text",
            "true",
            "--levels",
            "4",
        ],
    );
    assert!(output.contains("chapter 2/2"));
    let before: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join("state.json")).unwrap()).unwrap();
    assert_eq!(before["text_raster"]["stroke"], 0.2);
    assert_eq!(before["text_raster"]["coverage_gamma"], 0.7);
    assert_eq!(before["text_raster"]["hinting"], false);
    assert_eq!(before["text_raster"]["fractional"], true);
    let book = before["books"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert_eq!(book["position"]["chapter"], 0);
    read(dir.path(), b"q\n");
    let after: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join("state.json")).unwrap()).unwrap();
    assert_eq!(after["text_raster"], before["text_raster"]);
}

#[test]
fn reading_commands_save_bookmarks_reflow_and_resume() {
    let dir = tempfile::tempdir().unwrap();
    let output = read(dir.path(), b"n\nb\n+\nq\n");
    assert!(output.contains("page 2/"));
    let before: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join("state.json")).unwrap()).unwrap();
    assert_eq!(before["layout"]["font_size"], 26.0);
    let books = before["books"].as_object().unwrap();
    assert_eq!(books.len(), 1);
    let book = books.values().next().unwrap();
    assert_eq!(book["bookmarks"].as_array().unwrap().len(), 1);
    assert!(book["position"]["block"].as_u64().unwrap() > 0);
    let png = fs::read(dir.path().join("display.png")).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    read(dir.path(), b"q\n");
    let after: serde_json::Value =
        serde_json::from_slice(&fs::read(dir.path().join("state.json")).unwrap()).unwrap();
    let resumed = after["books"].as_object().unwrap().values().next().unwrap();
    assert_eq!(resumed["position"], book["position"]);
    assert_eq!(resumed["bookmarks"], book["bookmarks"]);
}

#[cfg(all(unix, not(target_os = "espidf")))]
#[test]
fn tailnet_identity_command_retains_public_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("identity");
    let invoke = || {
        let result = Command::new(env!("CARGO_BIN_EXE_femto"))
            .arg("tailnet-identity")
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap()
    };
    let first = invoke();
    assert_eq!(first, invoke());
    assert_eq!(first.as_object().unwrap().len(), 3);
    assert!(first["node_key"].as_str().unwrap().starts_with("nodekey:"));
    assert!(first["machine_key"].as_str().unwrap().starts_with("mkey:"));
    assert!(
        first["disco_key"]
            .as_str()
            .unwrap()
            .starts_with("discokey:")
    );
}

#[test]
fn opds_inspection_resolves_saved_feed_links_and_rejects_oversized_input() {
    let output = Command::new(env!("CARGO_BIN_EXE_femto"))
        .args([
            "opds-inspect",
            fixture("catalog.json").to_str().unwrap(),
            "--source",
            "https://catalog.example/opds/catalog.json",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let catalog: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        catalog["publications"][0]["links"][0]["resolved"],
        "https://catalog.example/books/sample.epub"
    );
    assert_eq!(catalog["groups"][0]["title"], "Collections");
    let dir = tempfile::tempdir().unwrap();
    let large = dir.path().join("large.json");
    fs::write(&large, vec![b' '; femto_catalog::MAX_FEED_BYTES + 1]).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_femto"))
        .arg("opds-inspect")
        .arg(large)
        .args(["--source", "https://catalog.example/"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("feed bytes"));
}

#[cfg(unix)]
#[test]
fn opds_fetch_authenticates_local_http_redirect_and_reads_chunked_catalog() {
    use std::os::unix::fs::PermissionsExt;
    use std::{
        io::Read,
        net::TcpListener,
        time::{Duration, Instant},
    };
    let dir = tempfile::tempdir().unwrap();
    let credentials = dir.path().join("credentials.json");
    fs::write(
        &credentials,
        br#"{"username":"Aladdin","password":"open sesame"}"#,
    )
    .unwrap();
    fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600)).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        for expected in ["/start", "/feeds/catalog.json"] {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "client did not connect");
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                assert!(request.len() < 8192);
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                request.extend(byte);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with(&format!("GET {expected} HTTP/1.1\r\n")));
            assert!(request.contains("Host: catalog.example\r\n"));
            assert!(request.contains("Authorization: Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ==\r\n"));
            if expected == "/start" {
                stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: /feeds/catalog.json\r\nContent-Length: 0\r\n\r\n").unwrap();
            } else {
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/opds+json\r\nTransfer-Encoding: chunked\r\n\r\n").unwrap();
                let body = fs::read(fixture("catalog.json")).unwrap();
                for chunk in body.chunks(97) {
                    write!(stream, "{:x}\r\n", chunk.len()).unwrap();
                    stream.write_all(chunk).unwrap();
                    stream.write_all(b"\r\n").unwrap();
                }
                stream.write_all(b"0\r\n\r\n").unwrap();
            }
        }
    });
    let output = Command::new(env!("CARGO_BIN_EXE_femto"))
        .args([
            "opds-fetch",
            "http://catalog.example/start",
            "--endpoint",
            &address.to_string(),
            "--timeout",
            "3",
        ])
        .arg("--credentials")
        .arg(&credentials)
        .arg("--allow-http-auth")
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let catalog: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        catalog["source"],
        "http://catalog.example/feeds/catalog.json"
    );
    assert_eq!(
        catalog["navigation"][0]["resolved"],
        "http://catalog.example/feeds/recent.json"
    );
}

#[cfg(unix)]
#[test]
fn opds_acquisition_publishes_readable_epub_and_cleans_failed_downloads() {
    use std::{
        io::Read,
        net::TcpListener,
        time::{Duration, Instant},
    };
    for scenario in 0..3 {
        let valid = scenario != 1;
        let collision = scenario == 2;
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("downloaded.epub");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let racing_destination = destination.clone();
        let server = std::thread::spawn(move || {
            for catalog in [true, false] {
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline);
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    assert!(request.len() < 8192);
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.extend(byte);
                }
                if !catalog && collision {
                    fs::write(&racing_destination, b"existing book").unwrap();
                }
                let (kind, bytes) = if catalog {
                    ("application/opds+json", serde_json::to_vec(&serde_json::json!({"metadata":{"title":"Downloads"},"links":[{"rel":"self","href":"/catalog"}],"publications":[{"metadata":{"title":"Specimen"},"links":[{"rel":"download","type":"application/epub+zip","href":"/book.epub"}]}]})).unwrap())
                } else {
                    (
                        "application/epub+zip",
                        if valid {
                            fs::read(fixture("sample.epub")).unwrap()
                        } else {
                            b"invalid EPUB".to_vec()
                        },
                    )
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n",
                    bytes.len()
                )
                .unwrap();
                stream.write_all(&bytes).unwrap();
            }
        });
        let run = || {
            Command::new(env!("CARGO_BIN_EXE_femto"))
                .args([
                    "opds-download",
                    "http://catalog.example/catalog",
                    "--publication",
                    "1",
                    "--endpoint",
                    &address.to_string(),
                    "--timeout",
                    "4",
                    "--output",
                ])
                .arg(&destination)
                .output()
                .unwrap()
        };
        let output = run();
        server.join().unwrap();
        assert_eq!(
            output.status.success(),
            valid && !collision,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if collision {
            assert_eq!(fs::read(&destination).unwrap(), b"existing book");
            assert!(output.stdout.is_empty());
        } else if valid {
            assert_eq!(
                fs::read(&destination).unwrap(),
                fs::read(fixture("sample.epub")).unwrap()
            );
            let document = femto_reader::document::Document::open(&destination).unwrap();
            assert!(!document.metadata.chapters.is_empty());
            let library = femto_reader::state::Library::scan(dir.path()).unwrap();
            assert_eq!(library.entries.len(), 1);
            let refused = run();
            assert!(!refused.status.success());
            assert!(String::from_utf8_lossy(&refused.stderr).contains("already exists"));
            assert_eq!(
                fs::read(&destination).unwrap(),
                fs::read(fixture("sample.epub")).unwrap()
            );
        } else {
            assert!(!destination.exists());
            assert!(output.stdout.is_empty());
        }
        assert_eq!(
            fs::read_dir(dir.path()).unwrap().count(),
            usize::from(valid)
        );
    }
}

#[test]
fn opds_browser_follows_pages_and_retains_feed_after_failure() {
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        time::{Duration, Instant},
    };
    let directory = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let source = format!("http://{}/", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        for expected in [
            "/",
            "/next",
            "/fail",
            "/",
            "/search?query=caf%C3%A9%20%26%20books",
            "/bad.epub",
            "/book.epub",
            "/",
        ] {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut socket = loop {
                match listener.accept() {
                    Ok((s, _)) => break s,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            };
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(socket.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line, format!("GET {expected} HTTP/1.1\r\n"));
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
            }
            if expected.ends_with(".epub") {
                let bytes = if expected == "/book.epub" {
                    fs::read(fixture("sample.epub")).unwrap()
                } else {
                    b"not an EPUB".to_vec()
                };
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nContent-Length: {}\r\n\r\n", bytes.len()).unwrap();
                socket.write_all(&bytes).unwrap();
            } else if expected == "/fail" {
                socket
                    .write_all(b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\n\r\n")
                    .unwrap();
            } else {
                let body = serde_json::json!({"metadata":{"title":if expected == "/" {"Root"} else {"Second\u{1b}[2J"}},"links":[{"rel":"search","href":"/search{?query,title}","type":"application/opds+json","templated":true},{"rel":"self","href":expected},{"rel":"next","href":if expected == "/" {"/next"} else {"/fail"},"type":"application/opds+json"}],"publications":[{"metadata":{"title":"Book"},"links":[{"rel":"download","href":"/book.epub","type":"application/epub+zip"}]},{"metadata":{"title":"Broken"},"links":[{"rel":"download","href":"/bad.epub","type":"application/epub+zip"}]}]}).to_string();
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/opds+json\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
            }
        }
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_femto"))
        .args(["opds-browse", &source, "--download-dir"])
        .arg(directory.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all("1\np1\n1\nb\ns1 café & books\nd2 broken.epub\nd1 saved book.epub\nd1 saved book.epub\nd1 ../escape.epub\nb\nq\n".as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(!stdout.contains('\u{1b}'));
    assert_eq!(stdout.matches("Catalog: \"Root\"").count(), 3);
    assert_eq!(stdout.matches("Catalog: \"Second").count(), 8);
    assert!(stdout.contains("\"title\": \"Book\""));
    let errors = String::from_utf8_lossy(&output.stderr);
    assert!(errors.contains("Catalog request failed"));
    assert!(errors.contains("downloaded EPUB validation failed"));
    assert!(errors.contains("already exists"));
    assert!(errors.contains("single name"));
    let saved = directory.path().join("saved book.epub");
    assert_eq!(
        fs::read(&saved).unwrap(),
        fs::read(fixture("sample.epub")).unwrap()
    );
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    let mut document = femto_reader::document::Document::open(&saved).unwrap();
    assert!(document.chapter(0).is_ok());
}

#[test]
fn catalog_sigint_cleans_partial_download_and_browser_can_retry() {
    use std::{
        io::{BufRead, BufReader, Read},
        net::TcpListener,
        sync::mpsc,
        time::{Duration, Instant},
    };
    for browser in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("book.epub");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let source = format!("http://{}/", listener.local_addr().unwrap());
        let (ready_tx, ready_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            for request in 0..if browser { 3 } else { 2 } {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut socket = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(e)
                            if e.kind() == std::io::ErrorKind::WouldBlock
                                && Instant::now() < deadline =>
                        {
                            std::thread::sleep(Duration::from_millis(5))
                        }
                        Err(e) => panic!("accept: {e}"),
                    }
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let mut reader = BufReader::new(socket.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert_eq!(
                    line,
                    format!(
                        "GET {} HTTP/1.1\r\n",
                        if request == 0 { "/" } else { "/book" }
                    )
                );
                loop {
                    line.clear();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                }
                if request == 0 {
                    let body = r#"{"metadata":{"title":"Catalog"},"links":[{"rel":"self","href":"/"}],"publications":[{"metadata":{"title":"Book"},"links":[{"rel":"download","href":"/book","type":"application/epub+zip"}]}]}"#;
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/opds+json\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
                } else if request == 1 {
                    socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nContent-Length: 10000\r\n\r\npartial data").unwrap();
                    ready_tx.send(()).unwrap();
                    // The cancelled client must close this stalled transfer.
                    assert_eq!(socket.read(&mut [0]).unwrap(), 0);
                } else {
                    let bytes = fs::read(fixture("sample.epub")).unwrap();
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/epub+zip\r\nContent-Length: {}\r\n\r\n", bytes.len()).unwrap();
                    socket.write_all(&bytes).unwrap();
                }
            }
        });
        let mut command = Command::new(env!("CARGO_BIN_EXE_femto"));
        if browser {
            command
                .args(["opds-browse", &source, "--download-dir"])
                .arg(directory.path());
        } else {
            command
                .args(["opds-download", &source, "--publication", "1", "--output"])
                .arg(&destination);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        if browser {
            input.write_all(b"d1 book.epub\n").unwrap();
        }
        ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        // Only the spawned test process receives SIGINT, never the test runner.
        assert_eq!(
            unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
            0
        );
        if browser {
            input.write_all(b"d1 book.epub\nq\n").unwrap();
        }
        drop(input);
        let deadline = Instant::now() + Duration::from_secs(8);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                child.kill().unwrap();
                panic!("catalog command did not finish after SIGINT");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        server.join().unwrap();
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(errors.contains("cancelled"), "{errors}");
        assert_eq!(output.status.success(), browser, "{errors}");
        assert_eq!(
            fs::read_dir(directory.path()).unwrap().count(),
            usize::from(browser)
        );
        if browser {
            assert_eq!(
                fs::read(destination).unwrap(),
                fs::read(fixture("sample.epub")).unwrap()
            );
        }
    }
}
