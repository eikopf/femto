use femto_catalog::{auth::BasicAuth, http, worker::Worker};
use femto_reader::{controller::Reader, document::Document};
use femto_sync::{
    Choice,
    reader::{Completion, Conflict, Session},
};
use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

pub type Jobs = Worker<Result<Completion, String>>;
pub struct Service {
    base: http::Url,
    auth: Option<BasicAuth>,
    pub tailnet: bool,
}
fn read(root: &Path, name: &str, limit: usize) -> io::Result<Option<Zeroizing<Vec<u8>>>> {
    let file = match File::open(root.join(name)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other(
            "Sync configuration must be a regular file",
        ));
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::other("Sync configuration exceeds size limit"));
    }
    Ok(Some(bytes))
}
impl Service {
    pub fn load(root: &Path) -> io::Result<Option<Self>> {
        let Some(bytes) = read(root, "progress.url", 4096)? else {
            return Ok(None);
        };
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| io::Error::other("Invalid sync URL"))?
            .trim();
        let base = http::Url::parse(text).map_err(|_| io::Error::other("Invalid sync URL"))?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || !base.path().ends_with('/')
            || base.port_or_known_default() == Some(0)
            || base.as_str().len() + 64 > 4096
        {
            return Err(io::Error::other(
                "Sync URL must be an HTTP(S) directory URL",
            ));
        }
        let auth = read(root, "progress-auth.json", 8192)?
            .map(|bytes| BasicAuth::from_json(base.as_str(), &bytes).map_err(io::Error::other))
            .transpose()?;
        let tailnet = match read(root, "progress-route", 32)? {
            None => false,
            Some(bytes) => match std::str::from_utf8(&bytes).map(str::trim) {
                Ok("lan") => false,
                Ok("tailnet") => true,
                _ => return Err(io::Error::other("Sync route must be lan or tailnet")),
            },
        };
        Ok(Some(Self {
            base,
            auth,
            tailnet,
        }))
    }
    pub fn start(
        &self,
        reader: &mut Reader,
        path: PathBuf,
        jobs: &mut Jobs,
        resolution: Option<(Conflict, Choice)>,
        factory: impl FnOnce() -> io::Result<Box<dyn http::Transport>> + Send + 'static,
    ) -> io::Result<u64> {
        if jobs.is_busy() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Sync already running",
            ));
        }
        let source = self
            .base
            .join(&reader.document.metadata.id)
            .map_err(io::Error::other)?;
        let session = match resolution {
            None => Session::prepare(reader, source.as_str()),
            Some((conflict, choice)) => {
                if conflict.source() != source.as_str() {
                    return Err(io::Error::other("Conflict belongs to another sync source"));
                }
                conflict.choose(reader, choice)
            }
        }
        .map_err(io::Error::other)?;
        let auth = self.auth.clone();
        let deadline = Instant::now() + Duration::from_secs(30);
        jobs.submit(deadline, move |budget, cancelled| {
            let result = (|| -> Result<Completion, String> {
                budget.check().map_err(|e| e.to_string())?;
                let mut document = Document::open(path).map_err(|e| e.to_string())?;
                budget.check().map_err(|e| e.to_string())?;
                let mut transport = factory().map_err(|e| e.to_string())?;
                session
                    .exchange(
                        &mut document,
                        &mut transport,
                        auth.as_ref(),
                        deadline,
                        cancelled,
                    )
                    .map_err(|e| e.to_string())
            })();
            Ok(result)
        })
    }
}

pub fn preview(document: &mut Document, record: &femto_reader::progress::Progress) -> String {
    let location = record.position;
    let text = document
        .chapter(location.chapter)
        .ok()
        .and_then(|chapter| match chapter.blocks.get(location.block) {
            Some(femto_reader::document::Block::Text(p)) => p
                .text
                .get(location.byte..)
                .map(|s| s.chars().take(20).collect::<String>()),
            Some(femto_reader::document::Block::Image(_)) => Some("image".into()),
            None => None,
        })
        .unwrap_or_default();
    format!("Ch {}: {text}", location.chapter + 1)
}
