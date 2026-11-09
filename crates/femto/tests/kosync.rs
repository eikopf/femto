#![cfg(unix)]
use femto_reader::{document::Document, layout::Locator, state::ReaderState};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::mpsc,
    thread,
};
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sample.epub")
}
fn config(directory: &Path, origin: &str) -> PathBuf {
    let path = directory.join("kosync.json");
    fs::write(&path,serde_json::to_vec(&serde_json::json!({"server":origin,"username":"alice","password":"secret-test-password","allow_http":true,"device_id":"fixture-one"})).unwrap()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}
fn serve(status: u16, body: String) -> (String, mpsc::Receiver<Vec<u8>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (send, recv) = mpsc::channel();
    let thread = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let count = stream.read(&mut buffer).unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&buffer[..count]);
            if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let header = std::str::from_utf8(&request[..end]).unwrap();
                let len = header
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
                    .map_or(0, |value| value.parse::<usize>().unwrap());
                if request.len() >= end + 4 + len {
                    break;
                }
            }
            assert!(request.len() < 65536);
        }
        send.send(request).unwrap();
        write!(stream,"HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
    });
    (origin, recv, thread)
}
fn run(config: &Path, action: &str, state: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_femto"));
    command
        .arg("kosync")
        .arg("--config")
        .arg(config)
        .arg(action);
    if let Some(state) = state {
        command.arg(fixture()).arg("--state").arg(state);
    }
    command.output().unwrap()
}
fn seed(path: &Path) -> ReaderState {
    let doc = Document::open(fixture()).unwrap();
    let mut state = ReaderState::default();
    let position = Locator {
        chapter: 0,
        block: 1,
        byte: 3,
    };
    state.set_position(&doc.metadata.id, position, 123);
    state.toggle_bookmark(&doc.metadata.id, position, "Keep bookmark".into());
    state.save(path).unwrap();
    state
}
fn document_id() -> femto_kosync::DocumentId {
    femto_kosync::DocumentId::sampled(&mut fs::File::open(fixture()).unwrap()).unwrap()
}
#[test]
fn cli_upload_uses_koreader_identity_and_xpointer_without_changing_state() {
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state.json");
    seed(&state);
    let before = fs::read(&state).unwrap();
    let (origin, request, server) = serve(
        200,
        serde_json::json!({"document":document_id(),"timestamp":1700000000}).to_string(),
    );
    let output = run(&config(directory.path(), &origin), "push", Some(&state));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = request.recv().unwrap();
    server.join().unwrap();
    assert!(request.starts_with(b"PUT /syncs/progress HTTP/1.1\r\n"));
    let body = &request[request.windows(4).position(|s| s == b"\r\n\r\n").unwrap() + 4..];
    let progress: femto_kosync::Progress = serde_json::from_slice(body).unwrap();
    assert_eq!(progress.document, document_id());
    assert!(
        progress
            .progress
            .starts_with("/body[1]/DocFragment[1]/body[1]/p[1]/text()[1].")
    );
    assert!(progress.percentage > 0.0 && progress.percentage < 1.0);
    assert_eq!(fs::read(state).unwrap(), before);
}
#[test]
fn cli_pull_preserves_bookmarks_and_errors_preserve_saved_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let state_path = directory.path().join("state.json");
    let original = seed(&state_path);
    let mut remote = serde_json::json!({"document":document_id(),"progress":"/body/DocFragment[1]/body/h1/text().3","percentage":0.1,"device":"KOReader","device_id":"other","timestamp":1700000000});
    let (origin, request, server) = serve(200, remote.to_string());
    let output = run(
        &config(directory.path(), &origin),
        "pull",
        Some(&state_path),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    request.recv().unwrap();
    server.join().unwrap();
    let saved = ReaderState::load(&state_path).unwrap();
    let id = Document::open(fixture()).unwrap().metadata.id;
    assert_eq!(
        saved.books[&id].position,
        Locator {
            chapter: 0,
            block: 0,
            byte: 3
        }
    );
    assert_eq!(
        serde_json::to_value(&saved.books[&id].bookmarks).unwrap(),
        serde_json::to_value(&original.books[&id].bookmarks).unwrap()
    );
    let before = fs::read(&state_path).unwrap();
    remote["progress"] = "/body/DocFragment[1]/body/p[999]/text().0".into();
    for (status, body, success) in [
        (200, remote.to_string(), false),
        (401, "{}".into(), false),
        (200, "{}".into(), true),
    ] {
        let (origin, request, server) = serve(status, body);
        let output = run(
            &config(directory.path(), &origin),
            "pull",
            Some(&state_path),
        );
        assert_eq!(output.status.success(), success);
        request.recv().unwrap();
        server.join().unwrap();
        assert_eq!(fs::read(&state_path).unwrap(), before);
        assert!(!String::from_utf8_lossy(&output.stderr).contains("secret-test-password"));
    }
}
#[test]
fn cli_rejects_unsafe_configuration_and_existing_state_owner() {
    let directory = tempfile::tempdir().unwrap();
    let config = config(directory.path(), "http://127.0.0.1:1");
    fs::set_permissions(&config, fs::Permissions::from_mode(0o644)).unwrap();
    let output = run(&config, "auth", None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("private regular file"));
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
    let state = directory.path().join("state.json");
    seed(&state);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.path().join("state.json.femto-lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let output = run(&config, "pull", Some(&state));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("in use"));
}

#[test]
fn cancelling_a_held_pull_preserves_saved_state_and_releases_ownership() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    let directory = tempfile::tempdir().unwrap();
    let state = directory.path().join("state.json");
    seed(&state);
    let before = fs::read(&state).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let config = config(
        directory.path(),
        &format!("http://{}", listener.local_addr().unwrap()),
    );
    let (sender, received) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut byte = [0];
        while !request.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() < 8192);
        }
        assert!(request.starts_with(b"GET /syncs/progress/"));
        sender.send(stream).unwrap();
    });
    let mut child = Command::new(env!("CARGO_BIN_EXE_femto"))
        .args(["kosync", "--config"])
        .arg(config)
        .arg("pull")
        .arg(fixture())
        .arg("--state")
        .arg(&state)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let held = received.recv_timeout(Duration::from_secs(5)).unwrap();
    server.join().unwrap();
    // The server remains connected and sends no response during cancellation.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("pull ignored cancellation");
        }
        thread::sleep(Duration::from_millis(20));
    }
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("cancelled"));
    assert_eq!(fs::read(&state).unwrap(), before);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.path().join("state.json.femto-lock"))
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).unwrap();
    drop(held);
}
