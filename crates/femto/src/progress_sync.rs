//! One bounded exchange using a durable per-book synchronization checkpoint.
use crate::{
    catalog::{HostTransport, Interrupts, credentials},
    state_file::StateFile,
};
use anyhow::{Context, Result};
use femto_catalog::http::{self, StrongEtag};
use femto_reader::{
    document::Document,
    progress::{Outcome, Progress, SyncAction},
    state::{ReaderState, atomic_write},
};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{self, Read},
    path::PathBuf,
    sync::atomic::Ordering,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(clap::Args)]
pub struct SyncArgs {
    book: PathBuf,
    source: String,
    #[arg(long)]
    state: PathBuf,
    /// Per-book checkpoint, distinct from the reader state and service store.
    #[arg(long)]
    sync_state: PathBuf,
    #[arg(long)]
    credentials: Option<PathBuf>,
    #[arg(long, requires = "credentials")]
    allow_http_auth: bool,
    #[arg(long)]
    endpoint: Option<std::net::SocketAddr>,
    #[arg(long, default_value_t = 15, value_parser = clap::value_parser!(u64).range(1..=60))]
    timeout: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    version: u32,
    source: String,
    state: PathBuf,
    baseline: Progress,
    etag: String,
}
fn load(path: &std::path::Path) -> Result<Option<Checkpoint>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take(8193).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 8192, "sync checkpoint size limit");
    Ok(Some(serde_json::from_slice(&bytes)?))
}
pub fn sync(args: SyncArgs) -> Result<()> {
    let url = url::Url::parse(&args.source)?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
            && url.as_str().len() <= 4096,
        "invalid sync URL"
    );
    let source = url.to_string();
    let auth = args
        .credentials
        .as_deref()
        .map(|p| credentials(p, &source, args.allow_http_auth))
        .transpose()?;
    let prepared = Prepared::new(&args.book, &source, &args.state, &args.sync_state, auth)?;
    let mut transport = HostTransport {
        endpoint: args.endpoint.map(|address| (url.origin(), address)),
    };
    let mut interrupts = Interrupts::new()?;
    let operation = interrupts.begin();
    let cancelled = operation.flag();
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    prepared.execute(&mut transport, deadline, cancelled)
}

pub(crate) struct Prepared {
    source: String,
    auth: Option<femto_catalog::auth::BasicAuth>,
    state_file: StateFile,
    checkpoint_file: StateFile,
    document: Document,
    state: ReaderState,
    local: Option<Progress>,
    checkpoint: Option<Checkpoint>,
}
impl Prepared {
    pub(crate) fn new(
        book: &std::path::Path,
        source: &str,
        state_path: &std::path::Path,
        checkpoint_path: &std::path::Path,
        auth: Option<femto_catalog::auth::BasicAuth>,
    ) -> Result<Self> {
        let source = source.to_owned();
        let state_file = StateFile::acquire(state_path)?;
        let checkpoint_file = StateFile::acquire(checkpoint_path)?;
        let mut document = Document::open(book)?;
        let state = ReaderState::load(state_file.path())?;
        let local = Progress::from_state(&state, &document.metadata.id);
        if let Some(local) = &local {
            local.validate_document(&mut document)?;
        }
        let checkpoint = load(checkpoint_file.path())?;
        if let Some(checkpoint) = &checkpoint {
            anyhow::ensure!(
                checkpoint.version == 1
                    && checkpoint.source == source
                    && checkpoint.state == state_file.path(),
                "sync checkpoint belongs to another source or local state"
            );
            checkpoint.baseline.validate_document(&mut document)?;
            StrongEtag::parse(&checkpoint.etag)?;
        }
        Ok(Self {
            source,
            auth,
            state_file,
            checkpoint_file,
            document,
            state,
            local,
            checkpoint,
        })
    }
    pub(crate) fn execute(
        self,
        transport: &mut impl http::Transport,
        deadline: Instant,
        cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<()> {
        let Self {
            source,
            auth,
            state_file,
            checkpoint_file,
            mut document,
            mut state,
            local,
            checkpoint,
        } = self;
        let check = || -> Result<()> {
            anyhow::ensure!(
                !cancelled.load(Ordering::Acquire),
                "progress sync cancelled"
            );
            anyhow::ensure!(Instant::now() < deadline, "progress sync deadline");
            Ok(())
        };
        let exchange = femto_sync::Request {
            source: &source,
            local: local.as_ref(),
            baseline: checkpoint.as_ref().map(|c| &c.baseline),
            auth: auth.as_ref(),
            deadline,
            cancelled: cancelled.clone(),
        }
        .execute(&mut document, transport);
        // A cancelled stream may report disconnection after a retried Interrupted read.
        check()?;
        let exchange = exchange?;
        let (action, baseline, tag) = match exchange {
            femto_sync::Outcome::Empty => {
                println!("{{\"status\":\"empty\"}}");
                return Ok(());
            }
            femto_sync::Outcome::Conflict { remote, .. } => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({"status":"conflict", "local":local, "remote":remote, "baseline":checkpoint.as_ref().map(|c| &c.baseline)})
                    )?
                );
                anyhow::bail!("progress sync conflict; neither side nor checkpoint changed");
            }
            femto_sync::Outcome::Synchronized {
                action,
                baseline,
                etag,
            } => (action, baseline, etag),
        };
        if action == SyncAction::Download {
            let outcome = baseline.apply(
                local.as_ref(),
                &mut state,
                &mut document,
                SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
            )?;
            anyhow::ensure!(
                !matches!(outcome, Outcome::Conflict { .. }),
                "local progress changed during sync"
            );
            check()?;
            state.save(state_file.path())?;
        }
        check()?;
        let checkpoint = Checkpoint {
            version: 1,
            source,
            state: state_file.path().to_owned(),
            baseline,
            etag: tag.as_str().into(),
        };
        let bytes = serde_json::to_vec(&checkpoint)?;
        anyhow::ensure!(bytes.len() <= 8192, "sync checkpoint size limit");
        atomic_write(checkpoint_file.path(), &bytes).context(
            "progress transferred but checkpoint save failed; next sync will compare again",
        )?;
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"status":"synchronized", "action":action, "etag":tag.as_str()})
            )?
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        collections::VecDeque,
        io::{Cursor, Write},
        rc::Rc,
        sync::{Arc, atomic::AtomicBool},
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
    #[test]
    fn cancelled_transport_failure_preserves_state_and_reports_cancellation() {
        struct Cancel;
        impl http::Transport for Cancel {
            fn connect(
                &mut self,
                _: &url::Url,
                _: Instant,
                cancelled: Arc<AtomicBool>,
            ) -> io::Result<Box<dyn http::Stream>> {
                cancelled.store(true, Ordering::Release);
                Err(io::ErrorKind::NotConnected.into())
            }
        }
        let directory = tempfile::tempdir().unwrap();
        let book =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sample.txt");
        let state_path = directory.path().join("reader.json");
        let checkpoint_path = directory.path().join("checkpoint.json");
        ReaderState::default().save(&state_path).unwrap();
        let original = std::fs::read(&state_path).unwrap();
        let prepared = Prepared::new(
            &book,
            "https://peer.example/progress/book",
            &state_path,
            &checkpoint_path,
            None,
        )
        .unwrap();
        let error = prepared
            .execute(
                &mut Cancel,
                Instant::now() + Duration::from_secs(2),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "progress sync cancelled");
        assert_eq!(std::fs::read(&state_path).unwrap(), original);
        assert!(!checkpoint_path.exists());
        assert!(StateFile::acquire(&state_path).is_ok());
        assert!(StateFile::acquire(&checkpoint_path).is_ok());
    }

    #[test]
    fn prepared_sync_uses_injected_transport_and_keeps_conflicts_unchanged() {
        let book =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sample.txt");
        let document = Document::open(&book).unwrap();
        for mode in 0..3 {
            let directory = tempfile::tempdir().unwrap();
            let state_path = directory.path().join("reader.json");
            let checkpoint_path = directory.path().join("checkpoint.json");
            let mut state = ReaderState::default();
            state.set_position(
                &document.metadata.id,
                femto_reader::layout::Locator {
                    chapter: 0,
                    block: 0,
                    byte: 0,
                },
                1,
            );
            state.save(&state_path).unwrap();
            let original = std::fs::read(&state_path).unwrap();
            let prepared = Prepared::new(
                &book,
                "https://peer.example/progress/book",
                &state_path,
                &checkpoint_path,
                None,
            )
            .unwrap();
            assert!(StateFile::acquire(&state_path).is_err());
            assert!(StateFile::acquire(&checkpoint_path).is_err());
            let replies = if mode == 2 {
                let mut remote = Progress::from_state(&state, &document.metadata.id).unwrap();
                remote.position.byte = 1;
                let body = serde_json::to_vec(&remote).unwrap();
                let mut response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nETag: \"remote\"\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                )
                .into_bytes();
                response.extend_from_slice(&body);
                VecDeque::from([response])
            } else {
                VecDeque::from([
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
                    if mode == 0 {
                        b"HTTP/1.1 201 Created\r\nETag: \"created\"\r\nContent-Length: 0\r\n\r\n"
                            .to_vec()
                    } else {
                        b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\n\r\n".to_vec()
                    },
                ])
            };
            let requests = Rc::new(RefCell::new(Vec::new()));
            let mut transport = Fixture {
                replies,
                requests: requests.clone(),
            };
            let result = prepared.execute(
                &mut transport,
                Instant::now() + Duration::from_secs(2),
                Arc::new(AtomicBool::new(false)),
            );
            assert_eq!(result.is_ok(), mode == 0);
            if mode == 2 {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("progress sync conflict")
                );
            }
            assert_eq!(std::fs::read(&state_path).unwrap(), original);
            assert_eq!(checkpoint_path.exists(), mode == 0);
            assert!(StateFile::acquire(&state_path).is_ok());
            assert!(StateFile::acquire(&checkpoint_path).is_ok());
            if mode == 0 {
                let checkpoint = load(&checkpoint_path).unwrap().unwrap();
                assert_eq!(checkpoint.etag, "\"created\"");
                assert_eq!(checkpoint.baseline.position.byte, 0);
            }
            let requests = requests.borrow();
            assert_eq!(requests.len(), if mode == 2 { 1 } else { 2 });
            if mode != 2 {
                let put = String::from_utf8_lossy(&requests[1]);
                assert!(put.starts_with("PUT /progress/book HTTP/1.1\r\n"));
                assert!(put.contains("If-None-Match: *\r\n"));
            }
        }
    }
}
