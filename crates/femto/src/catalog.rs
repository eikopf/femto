#[path = "host_http.rs"]
mod host_http;
use femto_catalog::http::{Transport, Url};
pub use host_http::HostTransport;
use host_http::remaining;
use std::{
    io::{self, Read, Write},
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub fn credentials(
    path: &std::path::Path,
    source: &str,
    allow_http: bool,
) -> anyhow::Result<femto_catalog::auth::BasicAuth> {
    use std::{
        fs::OpenOptions,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };
    use zeroize::Zeroizing;
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Input {
        username: Zeroizing<String>,
        password: Zeroizing<String>,
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "catalog credentials must be a private regular file"
    );
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(8193).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= 8192,
        "catalog credential file exceeds 8192 bytes"
    );
    let input: Input = serde_json::from_slice(&bytes)
        .map_err(|_| anyhow::anyhow!("invalid catalog credential JSON"))?;
    Ok(femto_catalog::auth::BasicAuth::new(
        source,
        &input.username,
        &input.password,
        allow_http,
    )?)
}

#[derive(clap::Args)]
pub struct DownloadArgs {
    source: String,
    /// One-based publication number: top-level publications, then each group in order.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=256))]
    publication: u32,
    /// One-based direct EPUB link among the selected publication's eligible links.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=1024))]
    acquisition: u32,
    #[arg(long)]
    output: std::path::PathBuf,
    #[arg(long)]
    credentials: Option<std::path::PathBuf>,
    #[arg(long, requires = "credentials")]
    allow_http_auth: bool,
    #[arg(long)]
    endpoint: Option<SocketAddr>,
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=300))]
    timeout: u64,
}

pub fn download(args: DownloadArgs) -> anyhow::Result<()> {
    let mut interrupts = Interrupts::new()?;
    let operation = interrupts.begin();
    let destination = AcquisitionFile::new(&args.output)?;
    let auth = args
        .credentials
        .as_deref()
        .map(|path| credentials(path, &args.source, args.allow_http_auth))
        .transpose()?;
    let origin = Url::parse(&args.source)?.origin();
    let mut transport = HostTransport {
        endpoint: args.endpoint.map(|address| (origin, address)),
    };
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    let cancelled = operation.flag();
    let catalog = femto_catalog::http::fetch_authenticated(
        &mut transport,
        &args.source,
        deadline,
        cancelled.clone(),
        auth.as_ref(),
    )?;
    let source = acquisition_url(
        &catalog,
        args.publication as usize,
        args.acquisition as usize,
    )?;
    let result = destination.receive(&mut transport, source, deadline, cancelled, auth.as_ref())?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

pub(crate) fn acquisition_url(
    catalog: &femto_catalog::Catalog,
    publication: usize,
    acquisition: usize,
) -> anyhow::Result<&str> {
    use anyhow::Context;
    let publication = publication
        .checked_sub(1)
        .context("publication number must be positive")?;
    let acquisition = acquisition
        .checked_sub(1)
        .context("acquisition number must be positive")?;
    catalog
        .publications
        .iter()
        .chain(catalog.groups.iter().flat_map(|group| &group.publications))
        .nth(publication)
        .context("publication number out of range")?
        .links
        .iter()
        .filter_map(|link| link.direct_epub())
        .nth(acquisition)
        .context("no direct unrestricted EPUB at the selected acquisition number")
}

pub(crate) struct AcquisitionFile {
    temporary: tempfile::NamedTempFile,
    parent: std::path::PathBuf,
    output: std::path::PathBuf,
}
impl AcquisitionFile {
    pub(crate) fn new(output: &std::path::Path) -> anyhow::Result<Self> {
        anyhow::ensure!(
            output
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("epub")),
            "download destination must have an .epub extension"
        );
        match std::fs::symlink_metadata(output) {
            Ok(_) => anyhow::bail!("download destination already exists"),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        let parent = output
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new("."));
        let temporary = tempfile::Builder::new()
            .prefix(".femto-download-")
            .suffix(".part")
            .tempfile_in(parent)?;
        Ok(Self {
            temporary,
            parent: parent.to_owned(),
            output: output.to_owned(),
        })
    }
    pub(crate) fn receive(
        mut self,
        transport: &mut impl Transport,
        source: &str,
        deadline: Instant,
        cancelled: Arc<AtomicBool>,
        auth: Option<&femto_catalog::auth::BasicAuth>,
    ) -> anyhow::Result<serde_json::Value> {
        use anyhow::Context;
        use femto_reader::document::Document;
        let result = femto_catalog::http::download_epub(
            transport,
            source,
            deadline,
            cancelled.clone(),
            auth,
            self.temporary.as_file_mut(),
        )?;
        self.temporary.as_file_mut().flush()?;
        self.temporary.as_file().sync_all()?;
        remaining(deadline, &cancelled)?;
        let mut document = Document::open_epub(self.temporary.path())
            .context("downloaded EPUB validation failed")?;
        document
            .chapter(0)
            .context("downloaded EPUB first chapter is unreadable")?;
        let metadata = document.metadata.clone();
        drop(document);
        remaining(deadline, &cancelled)?;
        self.temporary
            .persist_noclobber(&self.output)
            .map_err(|error| error.error)
            .context("could not publish downloaded book without replacement")?;
        std::fs::File::open(&self.parent)?
            .sync_all()
            .context("book published but directory sync failed")?;
        Ok(serde_json::json!({"path":self.output,"bytes":result.bytes,"metadata":metadata}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credential_files_are_private_bounded_and_do_not_echo_secrets() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(&path, br#"{"username":"reader","password":"secret value"}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(credentials(&path, "https://catalog.example/", false).is_ok());
        assert!(credentials(&path, "http://catalog.example/", false).is_err());
        assert!(credentials(&path, "http://catalog.example/", true).is_ok());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(credentials(&path, "https://catalog.example/", false).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("link");
        symlink(&path, &link).unwrap();
        assert!(credentials(&link, "https://catalog.example/", false).is_err());
        assert!(credentials(dir.path(), "https://catalog.example/", false).is_err());
        std::fs::write(
            &path,
            br#"{"username":"reader","password":"secret value","secret value":1}"#,
        )
        .unwrap();
        let error = credentials(&path, "https://catalog.example/", false).unwrap_err();
        assert!(!error.to_string().contains("secret value"));
        std::fs::write(&path, vec![b' '; 8193]).unwrap();
        assert!(credentials(&path, "https://catalog.example/", false).is_err());
    }
}

#[derive(clap::Args)]
pub struct BrowseArgs {
    source: String,
    /// Existing directory for browser downloads; commands accept a single filename.
    #[arg(long)]
    download_dir: Option<std::path::PathBuf>,
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=300))]
    download_timeout: u64,
    #[arg(long)]
    credentials: Option<std::path::PathBuf>,
    #[arg(long, requires = "credentials")]
    allow_http_auth: bool,
    #[arg(long)]
    endpoint: Option<SocketAddr>,
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=60))]
    timeout: u64,
}

pub fn browse(args: BrowseArgs) -> anyhow::Result<()> {
    use std::io::BufRead;
    let auth = args
        .credentials
        .as_deref()
        .map(|path| credentials(path, &args.source, args.allow_http_auth))
        .transpose()?;
    let origin = Url::parse(&args.source)?.origin();
    let mut transport = HostTransport {
        endpoint: args.endpoint.map(|address| (origin, address)),
    };
    let stdin = io::stdin();
    let mut input = stdin.lock();
    browse_transport(
        &mut transport,
        BrowseOptions {
            source: args.source,
            download_dir: args.download_dir,
            timeout: args.timeout,
            download_timeout: args.download_timeout,
            overall_deadline: None,
        },
        auth.as_ref(),
        || {
            let mut line = String::new();
            if input.by_ref().take(290).read_line(&mut line)? == 0 {
                Ok(None)
            } else {
                Ok(Some(line))
            }
        },
    )
}

pub(crate) struct BrowseOptions {
    pub source: String,
    pub download_dir: Option<std::path::PathBuf>,
    pub timeout: u64,
    pub download_timeout: u64,
    pub overall_deadline: Option<Instant>,
}
impl BrowseOptions {
    fn deadline(&self, seconds: u64) -> Instant {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        self.overall_deadline
            .map_or(deadline, |limit| limit.min(deadline))
    }
}
pub(crate) fn browse_transport<T: Transport>(
    transport: &mut T,
    args: BrowseOptions,
    auth: Option<&femto_catalog::auth::BasicAuth>,
    mut read_line: impl FnMut() -> anyhow::Result<Option<String>>,
) -> anyhow::Result<()> {
    use femto_catalog::browser::Browser;
    let mut interrupts = Interrupts::new()?;
    let load = |transport: &mut T, interrupts: &mut Interrupts, source: &str| {
        let operation = interrupts.begin();
        femto_catalog::http::fetch_authenticated(
            transport,
            source,
            args.deadline(args.timeout),
            operation.flag(),
            auth,
        )
    };
    let mut browser = Browser::new(load(transport, &mut interrupts, &args.source)?);
    loop {
        anyhow::ensure!(
            args.overall_deadline
                .is_none_or(|deadline| Instant::now() < deadline),
            "browser runtime deadline"
        );
        // JSON quoting prevents feed titles and URLs from injecting terminal controls.
        println!(
            "Catalog: {}",
            serde_json::to_string(&browser.current().title)?
        );
        println!(
            "Source: {}",
            serde_json::to_string(&browser.current().source)?
        );
        for (index, destination) in browser.destinations().iter().enumerate() {
            println!(
                "{}: {}",
                index + 1,
                serde_json::to_string(destination.label)?
            );
        }
        for (index, publication) in browser.publications().enumerate() {
            println!(
                "p{}: {}",
                index + 1,
                serde_json::to_string(&publication.title)?
            );
        }
        for (index, search) in browser.searches().enumerate() {
            println!(
                "s{}: {}",
                index + 1,
                serde_json::to_string(search.title.as_deref().unwrap_or("Search"))?
            );
        }
        print!(
            "Enter catalog number, pN for book details, sN keywords to search, dN filename.epub to download, b for back, r to refresh, q to quit: "
        );
        io::stdout().flush()?;
        let Some(line) = read_line()? else {
            return Ok(());
        };
        anyhow::ensure!(line.len() <= 288, "browser command exceeds 288 bytes");
        let command = line.trim();
        if command == "q" {
            return Ok(());
        }
        if let Some(number) = command
            .strip_prefix('p')
            .and_then(|n| n.parse::<usize>().ok())
            .and_then(|n| n.checked_sub(1))
        {
            if let Some(publication) = browser.publications().nth(number) {
                println!("{}", serde_json::to_string_pretty(publication)?);
            } else {
                eprintln!("Publication number out of range");
            }
            continue;
        }
        if let Some((selector, filename)) = command.split_once(' ')
            && let Some(number) = selector
                .strip_prefix('d')
                .and_then(|n| n.parse::<usize>().ok())
                .and_then(|n| n.checked_sub(1))
        {
            let result = (|| -> anyhow::Result<serde_json::Value> {
                use anyhow::Context;
                let directory = args
                    .download_dir
                    .as_deref()
                    .context("downloads require --download-dir")?;
                let output = browser_destination(directory, filename)?;
                let publication = browser
                    .publications()
                    .nth(number)
                    .context("publication number out of range")?;
                let source = publication
                    .links
                    .iter()
                    .find_map(|link| link.direct_epub())
                    .context("publication has no direct unrestricted EPUB")?;
                let operation = interrupts.begin();
                AcquisitionFile::new(&output)?.receive(
                    transport,
                    source,
                    args.deadline(args.download_timeout),
                    operation.flag(),
                    auth,
                )
            })();
            match result {
                Ok(result) => println!("{}", serde_json::to_string_pretty(&result)?),
                Err(error) => eprintln!(
                    "Download failed: {}",
                    serde_json::to_string(&format!("{error:#}"))?
                ),
            }
            continue;
        }
        if let Some((selector, query)) = command.split_once(' ')
            && let Some(number) = selector
                .strip_prefix('s')
                .and_then(|n| n.parse::<usize>().ok())
                .and_then(|n| n.checked_sub(1))
        {
            match browser.search(number, query) {
                Ok(search) => {
                    let found = browser.follow("", |_| {
                        let operation = interrupts.begin();
                        femto_catalog::http::fetch_search(
                            transport,
                            &search,
                            args.deadline(args.timeout),
                            operation.flag(),
                            auth,
                        )
                    });
                    if let Err(error) = found {
                        eprintln!(
                            "Catalog request failed: {}",
                            serde_json::to_string(&error.to_string())?
                        );
                    }
                }
                Err(error) => eprintln!(
                    "Search unavailable: {}",
                    serde_json::to_string(&error.to_string())?
                ),
            }
            continue;
        }
        let result = match command {
            "b" => browser
                .back(|source| load(transport, &mut interrupts, source))
                .map(|_| ()),
            "r" => {
                let source = browser.current().source.clone();
                browser.follow(&source, |source| load(transport, &mut interrupts, source))
            }
            _ => {
                let source = command
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| n.checked_sub(1))
                    .and_then(|n| browser.destinations().get(n).map(|d| d.source.to_owned()));
                let Some(source) = source else {
                    eprintln!("Unknown command or catalog number");
                    continue;
                };
                browser.follow(&source, |source| load(transport, &mut interrupts, source))
            }
        };
        if let Err(error) = result {
            eprintln!(
                "Catalog request failed: {}",
                serde_json::to_string(&error.to_string())?
            );
        }
    }
}

fn browser_destination(
    directory: &std::path::Path,
    filename: &str,
) -> anyhow::Result<std::path::PathBuf> {
    use std::path::Component;
    let mut components = std::path::Path::new(filename).components();
    anyhow::ensure!(
        !filename.is_empty()
            && filename.len() <= 240
            && !filename.chars().any(char::is_control)
            && !filename.contains(['/', '\\'])
            && matches!(components.next(), Some(Component::Normal(_)))
            && components.next().is_none(),
        "download filename must be a single name of at most 240 bytes"
    );
    Ok(directory.join(filename))
}

/// Command-scoped SIGINT handling; an idle browser retains default Ctrl-C behavior.
pub struct Interrupts {
    idle: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    hooks: Vec<signal_hook::SigId>,
}
impl Interrupts {
    pub fn new() -> io::Result<Self> {
        let mut signals = Self {
            idle: Arc::new(AtomicBool::new(true)),
            cancelled: Arc::new(AtomicBool::new(false)),
            hooks: Vec::new(),
        };
        signals
            .hooks
            .push(signal_hook::flag::register_conditional_default(
                signal_hook::consts::SIGINT,
                signals.idle.clone(),
            )?);
        signals.hooks.push(signal_hook::flag::register(
            signal_hook::consts::SIGINT,
            signals.cancelled.clone(),
        )?);
        Ok(signals)
    }
    pub fn begin(&mut self) -> Operation<'_> {
        self.cancelled.store(false, Ordering::SeqCst);
        self.idle.store(false, Ordering::SeqCst);
        Operation(self)
    }
}
impl Drop for Interrupts {
    fn drop(&mut self) {
        for hook in self.hooks.drain(..) {
            signal_hook::low_level::unregister(hook);
        }
    }
}
pub struct Operation<'a>(&'a mut Interrupts);
impl Operation<'_> {
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.0.cancelled.clone()
    }
}
impl Drop for Operation<'_> {
    fn drop(&mut self) {
        self.0.idle.store(true, Ordering::SeqCst);
    }
}
