use super::*;
use femto_microlink::{
    host_runtime::Runtime,
    host_tcp::{Driver, Shared},
    protocol::key::{Node, PrivateKey},
};
use std::io;

pub(super) fn target(text: &str) -> std::result::Result<String, String> {
    if text.len() > 4096 {
        return Err("application URL exceeds 4096 bytes".into());
    }
    let url = url::Url::parse(text).map_err(|_| "invalid application URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || !matches!(url.host(), Some(url::Host::Domain(_)))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.port_or_known_default() == Some(0)
    {
        return Err("application URL requires HTTP(S), a tailnet peer name, and no embedded credentials or fragment".into());
    }
    Ok(url.to_string())
}
pub(super) struct Download {
    pub destination: crate::catalog::AcquisitionFile,
    pub publication: usize,
    pub acquisition: usize,
}
pub(super) struct Request {
    pub source: String,
    pub download: Option<Download>,
    pub auth: Option<femto_catalog::auth::BasicAuth>,
    pub started: Instant,
    pub deadline: Instant,
}
pub(super) enum Work {
    Kosync {
        request: Box<crate::kosync::Prepared>,
        started: Instant,
    },
    Catalog(Request),
    Browse {
        request: Request,
        download_dir: Option<PathBuf>,
    },
}
pub(super) fn run(
    owner: ControlledNetwork<HostStore>,
    udp: UdpTransport,
    reconnect: reconnect::Controller,
    node_key: PrivateKey<Node>,
    work: Work,
    config: femto_microlink::host_runtime::TransportConfig,
    tls: femto_microlink::host_tcp_tls::Connector,
) -> Result<()> {
    let started = match &work {
        Work::Catalog(request) | Work::Browse { request, .. } => request.started,
        Work::Kosync { started, .. } => *started,
    };
    let mut runtime = Shared::new(Runtime::with_transport(
        owner,
        udp,
        reconnect,
        node_key,
        move || clock(started),
        config,
    )?);
    let mut transport = femto_catalog::tailnet::Tailnet::with_tls(runtime.clone(), tls);
    let mut interrupts = crate::catalog::Interrupts::new()?;
    let operation = interrupts.begin();
    match work {
        Work::Kosync { mut request, .. } => {
            let result = request.exchange(&mut transport, operation.flag())?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Work::Browse {
            request,
            download_dir,
        } => {
            drop(operation);
            drop(interrupts);
            use std::os::fd::AsFd;
            let stdin = io::stdin();
            let input = stdin.lock();
            crate::catalog::browse_transport(
                &mut transport,
                crate::catalog::BrowseOptions {
                    source: request.source,
                    download_dir,
                    timeout: 30,
                    download_timeout: 60,
                    overall_deadline: Some(request.deadline),
                },
                request.auth.as_ref(),
                || read_command(input.as_fd(), request.deadline, || runtime.pump()),
            )?;
        }
        Work::Catalog(request) => {
            let result = receive(&mut transport, request, operation.flag())?;
            println!("{}", result);
        }
    }
    Ok(())
}
fn receive(
    transport: &mut impl femto_catalog::http::Transport,
    request: Request,
    cancelled: Arc<AtomicBool>,
) -> Result<serde_json::Value> {
    let catalog = femto_catalog::http::fetch_authenticated(
        transport,
        &request.source,
        femto_catalog::deadline::Limit::Until(request.deadline),
        cancelled.clone(),
        request.auth.as_ref(),
    )
    .context("fetch catalog through tailnet")?;
    if let Some(download) = request.download {
        let source =
            crate::catalog::acquisition_url(&catalog, download.publication, download.acquisition)?;
        let result = download.destination.receive(
            transport,
            source,
            request.deadline,
            cancelled,
            request.auth.as_ref(),
        )?;
        Ok(serde_json::json!({"download": result, "opds_source": catalog.source}))
    } else {
        Ok(serde_json::json!({"opds": catalog}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        io::{Cursor, Read, Write},
        rc::Rc,
        sync::atomic::Ordering,
    };
    struct Wire {
        response: Cursor<Vec<u8>>,
        requests: Rc<RefCell<Vec<Vec<u8>>>>,
        index: usize,
    }
    impl Read for Wire {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.response.read(bytes)
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
        calls: Vec<String>,
        requests: Rc<RefCell<Vec<Vec<u8>>>>,
        output: PathBuf,
        mode: usize,
    }
    impl femto_catalog::http::Transport for Fixture {
        fn connect(
            &mut self,
            url: &url::Url,
            _: femto_catalog::deadline::Limit,
            cancelled: Arc<AtomicBool>,
        ) -> io::Result<Box<dyn femto_catalog::http::Stream>> {
            let index = self.calls.len();
            self.calls.push(url.to_string());
            self.requests.borrow_mut().push(Vec::new());
            let (kind, body) = if index == 0 {
                let feed = serde_json::json!({
                    "metadata":{"title":"Books"}, "links":[{"rel":"self","href":"/feed","type":"application/opds+json"}],
                    "publications":[{"metadata":{"title":"First"},"links":[]}],
                    "groups":[{"metadata":{"title":"Group"},"publications":[{"metadata":{"title":"Selected"},"links":[
                        {"rel":"preview","type":"application/epub+zip","href":"/preview"},
                        {"rel":"download","type":"application/epub+zip","href":"/first"},
                        {"rel":"download","type":"application/epub+zip","href":"https://files.example/selected.epub"}
                    ]}]}]
                });
                ("application/opds+json", serde_json::to_vec(&feed).unwrap())
            } else {
                assert_eq!(index, 1);
                assert_eq!(url.as_str(), "https://files.example/selected.epub");
                if self.mode == 2 {
                    cancelled.store(true, Ordering::Release);
                }
                if self.mode == 3 {
                    fs::write(&self.output, b"created during transfer").unwrap();
                }
                let bytes = if self.mode == 1 {
                    b"not an epub".to_vec()
                } else {
                    include_bytes!("../../../tests/fixtures/sample.epub").to_vec()
                };
                ("application/epub+zip", bytes)
            };
            let mut response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .into_bytes();
            response.extend_from_slice(&body);
            Ok(Box::new(Wire {
                response: Cursor::new(response),
                requests: self.requests.clone(),
                index,
            }))
        }
    }
    #[test]
    fn selected_acquisition_reuses_transport_and_preserves_publication_guards() {
        for mode in 0..6 {
            let directory = tempfile::tempdir().unwrap();
            let output = directory.path().join("book.epub");
            let started = Instant::now();
            let request = Request {
                source: "https://books.example/feed".into(),
                auth: Some(
                    femto_catalog::auth::BasicAuth::new(
                        "https://books.example/feed",
                        "reader",
                        "password",
                        false,
                    )
                    .unwrap(),
                ),
                started,
                deadline: if mode == 5 {
                    started
                } else {
                    started + Duration::from_secs(2)
                },
                download: Some(Download {
                    destination: crate::catalog::AcquisitionFile::new(&output).unwrap(),
                    publication: if mode == 4 { 3 } else { 2 },
                    acquisition: 2,
                }),
            };
            let requests = Rc::new(RefCell::new(Vec::new()));
            let mut transport = Fixture {
                calls: Vec::new(),
                requests: requests.clone(),
                output: output.clone(),
                mode,
            };
            let result = receive(&mut transport, request, Arc::new(AtomicBool::new(false)));
            if mode == 0 {
                assert_eq!(
                    result.unwrap()["download"]["path"],
                    output.to_str().unwrap()
                );
                assert_eq!(
                    fs::read(&output).unwrap(),
                    include_bytes!("../../../tests/fixtures/sample.epub")
                );
            } else {
                assert!(result.is_err(), "mode {mode}");
                if mode == 3 {
                    assert_eq!(fs::read(&output).unwrap(), b"created during transfer");
                } else {
                    assert!(!output.exists());
                }
            }
            assert_eq!(
                fs::read_dir(directory.path()).unwrap().count(),
                usize::from(mode == 0 || mode == 3)
            );
            if mode != 5 {
                let requests = requests.borrow();
                assert!(String::from_utf8_lossy(&requests[0]).contains("Authorization: Basic "));
                if requests.len() > 1 {
                    assert!(!String::from_utf8_lossy(&requests[1]).contains("Authorization:"));
                }
                assert_eq!(transport.calls.len(), if mode == 4 { 1 } else { 2 });
            } else {
                assert!(transport.calls.is_empty());
            }
        }
    }
}

/// Reads exclusive input one byte at a time so stdio buffering cannot hide ready commands.
fn read_command(
    input: std::os::fd::BorrowedFd<'_>,
    deadline: Instant,
    mut pump: impl FnMut() -> io::Result<()>,
) -> Result<Option<String>> {
    use std::os::fd::AsRawFd;
    let mut bytes = Vec::new();
    loop {
        anyhow::ensure!(Instant::now() < deadline, "browser runtime deadline");
        pump()?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        anyhow::ensure!(!remaining.is_zero(), "browser runtime deadline");
        let mut descriptor = libc::pollfd {
            fd: input.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                remaining.as_millis().clamp(1, 50) as i32,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if result == 0 {
            continue;
        }
        anyhow::ensure!(Instant::now() < deadline, "browser runtime deadline");
        anyhow::ensure!(
            descriptor.revents & (libc::POLLNVAL | libc::POLLERR) == 0,
            "browser input descriptor failed"
        );
        let mut byte = 0u8;
        let count = unsafe { libc::read(input.as_raw_fd(), (&mut byte as *mut u8).cast(), 1) };
        if count < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.into());
        }
        if count == 0 {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Ok(Some(String::from_utf8(bytes)?))
            };
        }
        bytes.push(byte);
        anyhow::ensure!(bytes.len() <= 288, "browser command exceeds 288 bytes");
        if byte == b'\n' {
            return Ok(Some(String::from_utf8(bytes)?));
        }
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;
    use std::{
        io::Write,
        os::{fd::AsFd, unix::net::UnixStream},
    };
    #[test]
    fn partial_input_keeps_pumping_and_does_not_prefetch_following_commands() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"s1 caf\xc3").unwrap();
        let (ready, wait) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            if wait.recv().is_ok() {
                writer.write_all(b"\xa9\nq\n").unwrap();
            }
        });
        let mut pumps = 0;
        let line = read_command(
            reader.as_fd(),
            Instant::now() + Duration::from_secs(2),
            || {
                pumps += 1;
                if pumps == 9 {
                    ready.send(()).unwrap();
                }
                Ok(())
            },
        );
        drop(ready);
        worker.join().unwrap();
        let line = line.unwrap();
        assert_eq!(line.as_deref(), Some("s1 café\n"));
        assert!(pumps > "s1 café\n".len());
        assert_eq!(
            read_command(
                reader.as_fd(),
                Instant::now() + Duration::from_secs(1),
                || Ok(())
            )
            .unwrap()
            .as_deref(),
            Some("q\n")
        );
        assert!(
            read_command(
                reader.as_fd(),
                Instant::now() + Duration::from_secs(1),
                || Ok(())
            )
            .unwrap()
            .is_none()
        );
    }
    #[test]
    fn input_obeys_deadline_length_utf8_and_pump_failures() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        let mut pumps = 0;
        assert!(
            read_command(
                reader.as_fd(),
                Instant::now() + Duration::from_millis(20),
                || {
                    pumps += 1;
                    Ok(())
                }
            )
            .is_err()
        );
        assert!(pumps > 0);
        writer.write_all(b"q\n").unwrap();
        assert!(
            read_command(
                reader.as_fd(),
                Instant::now() + Duration::from_secs(1),
                || Err(io::ErrorKind::ConnectionAborted.into())
            )
            .is_err()
        );
        assert_eq!(
            read_command(
                reader.as_fd(),
                Instant::now() + Duration::from_secs(1),
                || Ok(())
            )
            .unwrap()
            .as_deref(),
            Some("q\n")
        );
        for bytes in [vec![b'x'; 289], vec![0xff, b'\n']] {
            let (reader, mut writer) = UnixStream::pair().unwrap();
            writer.write_all(&bytes).unwrap();
            assert!(
                read_command(
                    reader.as_fd(),
                    Instant::now() + Duration::from_secs(1),
                    || Ok(())
                )
                .is_err()
            );
        }
    }
}
