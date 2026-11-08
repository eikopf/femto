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

#[test]
fn progress_commands_exchange_positions_and_preserve_conflicts() {
    use femto_reader::{document::Document, layout::Locator, state::ReaderState};
    let dir = tempfile::tempdir().unwrap();
    let book = dir.path().join("book.txt");
    fs::write(&book, "café reading progress").unwrap();
    let doc = Document::open(&book).unwrap();
    let state_path = dir.path().join("state.json");
    let mut state = ReaderState::default();
    state.set_position(&doc.metadata.id, Locator::default(), 10);
    state.save(&state_path).unwrap();
    let exported = Command::new(env!("CARGO_BIN_EXE_femto"))
        .arg("progress-export")
        .arg(&book)
        .arg("--state")
        .arg(&state_path)
        .output()
        .unwrap();
    assert!(exported.status.success());
    let baseline = dir.path().join("baseline.json");
    fs::write(&baseline, &exported.stdout).unwrap();
    let mut remote: serde_json::Value = serde_json::from_slice(&exported.stdout).unwrap();
    remote["position"]["byte"] = serde_json::json!(6);
    remote["recorded_at"] = serde_json::json!(1);
    let record = dir.path().join("remote.json");
    fs::write(&record, serde_json::to_vec(&remote).unwrap()).unwrap();
    let before = fs::read(&state_path).unwrap();
    let run = |expected: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_femto"));
        command
            .arg("progress-import")
            .arg(&book)
            .arg(&record)
            .arg("--state")
            .arg(&state_path);
        if expected {
            command.arg("--expected").arg(&baseline);
        }
        command.output().unwrap()
    };
    let conflict = run(false);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stdout).contains("conflict"));
    assert_eq!(fs::read(&state_path).unwrap(), before);
    let applied = run(true);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert_eq!(
        ReaderState::load(&state_path).unwrap().books[&doc.metadata.id]
            .position
            .byte,
        6
    );
    let saved = fs::read(&state_path).unwrap();
    let unchanged = run(false);
    assert!(unchanged.status.success());
    assert!(String::from_utf8_lossy(&unchanged.stdout).contains("unchanged"));
    assert_eq!(fs::read(&state_path).unwrap(), saved);
}

#[test]
fn active_reader_excludes_progress_import_and_releases_lock_on_exit() {
    use femto_reader::{document::Document, layout::Locator, progress::Progress};
    use std::time::{Duration, Instant};
    for crash in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let book = directory.path().join("book.txt");
        fs::write(&book, "Reading position for locking test.").unwrap();
        let state = directory.path().join("state.json");
        let record = directory.path().join("progress.json");
        fs::write(
            &record,
            serde_json::to_vec(&Progress {
                version: 1,
                book_id: Document::open(&book).unwrap().metadata.id,
                position: Locator::default(),
                recorded_at: 0,
            })
            .unwrap(),
        )
        .unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_femto"))
            .arg("read")
            .arg(&book)
            .arg("--font")
            .arg(fixture("DejaVuSerif.ttf"))
            .arg("--state")
            .arg(&state)
            .arg("--display")
            .arg(directory.path().join("display.png"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !state.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "reader exited before saving"
            );
            if Instant::now() >= deadline {
                child.kill().unwrap();
                panic!("reader startup deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let before = fs::read(&state).unwrap();
        let import = || {
            Command::new(env!("CARGO_BIN_EXE_femto"))
                .arg("progress-import")
                .arg(&book)
                .arg(&record)
                .arg("--state")
                .arg(&state)
                .output()
                .unwrap()
        };
        let denied = import();
        assert!(!denied.status.success());
        assert!(String::from_utf8_lossy(&denied.stderr).contains("state file is in use"));
        assert_eq!(fs::read(&state).unwrap(), before);
        if crash {
            child.kill().unwrap();
        } else {
            child.stdin.take().unwrap().write_all(b"q\n").unwrap();
        }
        let status = child.wait().unwrap();
        assert_eq!(status.success(), !crash);
        let result = import();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("unchanged"));
    }
}

#[test]
fn progress_pull_authenticates_validates_and_preserves_conflicting_state() {
    use femto_reader::{
        document::Document, layout::Locator, progress::Progress, state::ReaderState,
    };
    use std::{
        io::{BufRead, BufReader},
        net::TcpListener,
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };
    for scenario in [
        "apply",
        "conflict",
        "wrong_book",
        "oversize",
        "unauthorized",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let book = directory.path().join("book.txt");
        fs::write(&book, "café reading progress").unwrap();
        let id = Document::open(&book).unwrap().metadata.id;
        let state_path = directory.path().join("state.json");
        let mut state = ReaderState::default();
        state.set_position(&id, Locator::default(), 100);
        state.save(&state_path).unwrap();
        let before = fs::read(&state_path).unwrap();
        let baseline = Progress::from_state(&state, &id).unwrap();
        let expected = directory.path().join("expected.json");
        fs::write(&expected, serde_json::to_vec(&baseline).unwrap()).unwrap();
        let mut remote = Progress {
            position: Locator {
                byte: 6,
                ..Default::default()
            },
            ..baseline
        };
        if scenario == "wrong_book" {
            remote.book_id = "0".repeat(64);
        }
        let bytes = serde_json::to_vec(&remote).unwrap();
        let credentials = directory.path().join("credentials.json");
        fs::write(
            &credentials,
            br#"{"username":"reader","password":"secret"}"#,
        )
        .unwrap();
        fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600)).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
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
            let mut input = BufReader::new(socket.try_clone().unwrap());
            let mut request = String::new();
            loop {
                let mut line = String::new();
                assert!(input.read_line(&mut line).unwrap() > 0);
                request.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            assert!(request.starts_with("GET /progress HTTP/1.1\r\n"));
            assert!(request.contains("Authorization: Basic cmVhZGVyOnNlY3JldA==\r\n"));
            if scenario == "unauthorized" {
                socket
                    .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                    .unwrap();
            } else if scenario == "oversize" {
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 4097\r\n\r\n").unwrap();
            } else {
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", bytes.len()).unwrap();
                socket.write_all(&bytes).unwrap();
            }
        });
        let mut command = Command::new(env!("CARGO_BIN_EXE_femto"));
        command
            .arg("progress-pull")
            .arg(&book)
            .arg("http://progress.example/progress")
            .arg("--endpoint")
            .arg(address.to_string())
            .arg("--state")
            .arg(&state_path)
            .arg("--credentials")
            .arg(&credentials)
            .arg("--allow-http-auth");
        if scenario != "conflict" {
            command.arg("--expected").arg(&expected);
        }
        let output = command.output().unwrap();
        server.join().unwrap();
        assert_eq!(
            output.status.success(),
            scenario == "apply",
            "{scenario}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if scenario == "apply" {
            assert_eq!(
                ReaderState::load(&state_path).unwrap().books[&id]
                    .position
                    .byte,
                6
            );
        } else {
            assert_eq!(fs::read(&state_path).unwrap(), before);
        }
    }
}

#[test]
fn progress_push_creates_rejects_stale_etag_and_retrieves_new_version() {
    use femto_reader::{
        document::Document, layout::Locator, progress::Progress, state::ReaderState,
    };
    use std::{
        io::{BufRead, BufReader, Read},
        net::TcpListener,
        time::{Duration, Instant},
    };
    let directory = tempfile::tempdir().unwrap();
    let book = directory.path().join("book.txt");
    fs::write(&book, "Progress upload specimen").unwrap();
    let id = Document::open(&book).unwrap().metadata.id;
    let state_path = directory.path().join("state.json");
    let mut state = ReaderState::default();
    state.set_position(&id, Locator::default(), 100);
    state.save(&state_path).unwrap();
    let before = fs::read(&state_path).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let source = format!("http://{}/progress", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut stored = Vec::new();
        for attempt in 0..4 {
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
            let mut request = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                if let Some(value) = line.strip_prefix("Content-Length: ") {
                    length = value.trim().parse::<usize>().unwrap();
                }
                request.push_str(&line);
                if line == "\r\n" {
                    break;
                }
            }
            if attempt == 3 {
                assert!(request.starts_with("GET /progress HTTP/1.1\r\n"));
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: \"v2\"\r\nContent-Length: {}\r\n\r\n", stored.len()).unwrap();
                socket.write_all(&stored).unwrap();
                continue;
            }
            assert!(request.starts_with("PUT /progress HTTP/1.1\r\n"));
            assert!(length <= 4096);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            assert_eq!(Progress::decode(&body).unwrap().book_id, id);
            if attempt == 0 {
                assert!(request.contains("If-None-Match: *\r\n"));
                stored = body;
                socket
                    .write_all(b"HTTP/1.1 201 Created\r\nETag: \"v1\"\r\nContent-Length: 0\r\n\r\n")
                    .unwrap();
            } else if attempt == 1 {
                assert!(request.contains("If-Match: \"stale\"\r\n"));
                socket
                    .write_all(b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\n\r\n")
                    .unwrap();
            } else {
                assert!(request.contains("If-Match: \"v1\"\r\n"));
                stored = body;
                socket
                    .write_all(b"HTTP/1.1 204 No Content\r\nETag: \"v2\"\r\n\r\n")
                    .unwrap();
            }
        }
    });
    for condition in [
        vec!["--create"],
        vec!["--if-match", "\"stale\""],
        vec!["--if-match", "\"v1\""],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_femto"))
            .arg("progress-push")
            .arg(&book)
            .arg(&source)
            .arg("--state")
            .arg(&state_path)
            .args(&condition)
            .output()
            .unwrap();
        let conflict = condition.contains(&"\"stale\"");
        assert_eq!(
            output.status.success(),
            !conflict,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        if conflict {
            assert!(String::from_utf8_lossy(&output.stderr).contains("remote progress conflict"));
        } else {
            assert!(String::from_utf8_lossy(&output.stdout).contains("uploaded"));
        }
        assert_eq!(fs::read(&state_path).unwrap(), before);
    }
    let pulled = Command::new(env!("CARGO_BIN_EXE_femto"))
        .arg("progress-pull")
        .arg(&book)
        .arg(&source)
        .arg("--state")
        .arg(&state_path)
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(pulled.status.success());
    let result: serde_json::Value = serde_json::from_slice(&pulled.stdout).unwrap();
    assert_eq!(result["etag"], "\"v2\"");
    assert_eq!(result["status"], "unchanged");
    assert_eq!(fs::read(&state_path).unwrap(), before);
    let missing = Command::new(env!("CARGO_BIN_EXE_femto"))
        .arg("progress-push")
        .arg(&book)
        .arg(&source)
        .arg("--state")
        .arg(&state_path)
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("--create"));
}

#[test]
fn persistent_progress_service_handles_clients_and_recovers_after_process_exit() {
    use femto_reader::{document::Document, layout::Locator, state::ReaderState};
    use std::io::{BufRead, BufReader};
    struct Service(std::process::Child);
    impl Drop for Service {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let store = directory.path().join("remote-store.json");
    let book = directory.path().join("book.txt");
    fs::write(&book, "Progress service specimen").unwrap();
    let id = Document::open(&book).unwrap().metadata.id;
    let state_path = directory.path().join("state.json");
    let mut state = ReaderState::default();
    state.set_position(&id, Locator::default(), 1);
    state.save(&state_path).unwrap();
    let start = || {
        let mut child = Command::new(env!("CARGO_BIN_EXE_femto"))
            .args(["progress-serve", "--bind", "127.0.0.1:0", "--store"])
            .arg(&store)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
        let url = format!(
            "http://{}/progress/{id}",
            ready["listening"].as_str().unwrap()
        );
        (Service(child), url)
    };
    let push = |url: &str, condition: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_femto"))
            .arg("progress-push")
            .arg(&book)
            .arg(url)
            .arg("--state")
            .arg(&state_path)
            .args(condition)
            .output()
            .unwrap()
    };
    let (service, url) = start();
    let created = push(&url, &["--create"]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let old_tag = metadata["etag"].as_str().unwrap();
    assert!(!push(&url, &["--create"]).status.success());
    state.set_position(
        &id,
        Locator {
            byte: 3,
            ..Default::default()
        },
        2,
    );
    state.save(&state_path).unwrap();
    let updated = push(&url, &["--if-match", old_tag]);
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&updated.stdout).unwrap();
    let new_tag = metadata["etag"].as_str().unwrap().to_owned();
    assert_ne!(new_tag, old_tag);
    assert!(!push(&url, &["--if-match", old_tag]).status.success());
    drop(service);
    let (mut service, url) = start();
    let pulled = Command::new(env!("CARGO_BIN_EXE_femto"))
        .arg("progress-pull")
        .arg(&book)
        .arg(&url)
        .arg("--state")
        .arg(&state_path)
        .output()
        .unwrap();
    assert!(
        pulled.status.success(),
        "{}",
        String::from_utf8_lossy(&pulled.stderr)
    );
    let metadata: serde_json::Value = serde_json::from_slice(&pulled.stdout).unwrap();
    assert_eq!(metadata["etag"], new_tag);
    assert_eq!(metadata["status"], "unchanged");
    assert_eq!(
        unsafe { libc::kill(service.0.id() as libc::pid_t, libc::SIGINT) },
        0
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if let Some(status) = service.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "service cancellation deadline"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn progress_sync_exchanges_two_readers_and_preserves_divergent_edits() {
    use femto_reader::{document::Document, layout::Locator, state::ReaderState};
    use std::io::{BufRead, BufReader};
    struct Service(std::process::Child);
    impl Drop for Service {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let book = dir.path().join("book.txt");
    fs::write(&book, "Two readers exchanging progress.").unwrap();
    let id = Document::open(&book).unwrap().metadata.id;
    let state_a = dir.path().join("a.json");
    let state_b = dir.path().join("b.json");
    let checkpoint_a = dir.path().join("a.sync.json");
    let checkpoint_b = dir.path().join("b.sync.json");
    let store = dir.path().join("store.json");
    let change = |path: &Path, byte: usize| {
        let mut state = ReaderState::load(path).unwrap();
        state.set_position(
            &id,
            Locator {
                byte,
                ..Default::default()
            },
            1,
        );
        state.save(path).unwrap();
    };
    change(&state_a, 0);
    let mut child = Command::new(env!("CARGO_BIN_EXE_femto"))
        .args(["progress-serve", "--bind", "127.0.0.1:0", "--store"])
        .arg(&store)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let _service = Service(child);
    let ready: serde_json::Value = serde_json::from_str(&line).unwrap();
    let source = format!(
        "http://{}/progress/{id}",
        ready["listening"].as_str().unwrap()
    );
    let sync = |state: &Path, checkpoint: &Path| {
        Command::new(env!("CARGO_BIN_EXE_femto"))
            .arg("progress-sync")
            .arg(&book)
            .arg(&source)
            .arg("--state")
            .arg(state)
            .arg("--sync-state")
            .arg(checkpoint)
            .output()
            .unwrap()
    };
    let assert_action = |output: std::process::Output, action: &str| {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["action"], action);
    };
    assert_action(sync(&state_a, &checkpoint_a), "create");
    assert_action(sync(&state_b, &checkpoint_b), "download");
    change(&state_a, 3);
    assert_action(sync(&state_a, &checkpoint_a), "upload");
    assert_action(sync(&state_b, &checkpoint_b), "download");
    assert_eq!(
        ReaderState::load(&state_b).unwrap().books[&id]
            .position
            .byte,
        3
    );
    change(&state_a, 5);
    change(&state_b, 7);
    assert_action(sync(&state_a, &checkpoint_a), "upload");
    let before_state = fs::read(&state_b).unwrap();
    let before_checkpoint = fs::read(&checkpoint_b).unwrap();
    let before_store = fs::read(&store).unwrap();
    let conflict = sync(&state_b, &checkpoint_b);
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stdout).contains("conflict"));
    assert_eq!(fs::read(&state_b).unwrap(), before_state);
    assert_eq!(fs::read(&checkpoint_b).unwrap(), before_checkpoint);
    assert_eq!(fs::read(&store).unwrap(), before_store);
    // Model a lost checkpoint after a successful remote commit.
    fs::remove_file(&checkpoint_a).unwrap();
    assert_action(sync(&state_a, &checkpoint_a), "converged");
    assert_eq!(fs::read(&store).unwrap(), before_store);
    let mismatch = sync(&state_b, &checkpoint_a);
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("another source or local state"));
    assert_eq!(fs::read(&state_b).unwrap(), before_state);
}
