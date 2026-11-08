use anyhow::Context;
use femto_reader::{
    document::Document,
    progress::{MAX_PROGRESS_BYTES, Outcome, Progress},
    state::ReaderState,
};
use std::{
    fs::File,
    io::Read,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

fn read(path: &Path) -> anyhow::Result<Progress> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_PROGRESS_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(Progress::decode(&bytes)?)
}
pub fn export(book: &Path, path: &Path) -> anyhow::Result<()> {
    #[cfg(all(unix, not(target_os = "espidf")))]
    let state_file = crate::state_file::StateFile::acquire(path)?;
    #[cfg(all(unix, not(target_os = "espidf")))]
    let path = state_file.path();
    let mut document = Document::open(book)?;
    let state = ReaderState::load(path)?;
    let progress = Progress::from_state(&state, &document.metadata.id)
        .context("book has no saved progress")?;
    progress.validate_document(&mut document)?;
    println!("{}", serde_json::to_string_pretty(&progress)?);
    Ok(())
}
pub fn import(
    book: &Path,
    record: &Path,
    path: &Path,
    expected: Option<&Path>,
) -> anyhow::Result<()> {
    let progress = read(record)?;
    let expected = expected.map(read).transpose()?;
    report(
        apply(book, path, &progress, expected.as_ref(), || Ok(()))?,
        None,
    )
}
fn apply(
    book: &Path,
    path: &Path,
    progress: &Progress,
    expected: Option<&Progress>,
    check: impl Fn() -> anyhow::Result<()>,
) -> anyhow::Result<Outcome> {
    check()?;
    #[cfg(all(unix, not(target_os = "espidf")))]
    let state_file = crate::state_file::StateFile::acquire(path)?;
    #[cfg(all(unix, not(target_os = "espidf")))]
    let path = state_file.path();
    let mut document = Document::open(book)?;
    let mut state = ReaderState::load(path)?;
    let outcome = progress.apply(
        expected,
        &mut state,
        &mut document,
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs(),
    )?;
    check()?;
    if outcome == Outcome::Applied {
        state.save(path)?;
    }
    Ok(outcome)
}
fn report(outcome: Outcome, remote: Option<(&str, Option<&str>)>) -> anyhow::Result<()> {
    let mut value = serde_json::to_value(&outcome)?;
    if let Some((source, etag)) = remote {
        value["source"] = serde_json::json!(source);
        value["etag"] = serde_json::json!(etag);
    }
    println!("{}", serde_json::to_string_pretty(&value)?);
    anyhow::ensure!(
        !matches!(outcome, Outcome::Conflict { .. }),
        "progress conflict; local state unchanged"
    );
    Ok(())
}

#[cfg(all(unix, not(target_os = "espidf")))]
#[derive(clap::Args)]
pub struct PullArgs {
    book: std::path::PathBuf,
    source: String,
    #[arg(long)]
    state: std::path::PathBuf,
    #[arg(long)]
    expected: Option<std::path::PathBuf>,
    #[arg(long)]
    credentials: Option<std::path::PathBuf>,
    #[arg(long, requires = "credentials")]
    allow_http_auth: bool,
    #[arg(long)]
    endpoint: Option<std::net::SocketAddr>,
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=60))]
    timeout: u64,
}
#[cfg(all(unix, not(target_os = "espidf")))]
pub fn pull(args: PullArgs) -> anyhow::Result<()> {
    use crate::catalog::{HostTransport, Interrupts, credentials};
    use std::{
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };
    let expected = args.expected.as_deref().map(read).transpose()?;
    let auth = args
        .credentials
        .as_deref()
        .map(|path| credentials(path, &args.source, args.allow_http_auth))
        .transpose()?;
    let origin = url::Url::parse(&args.source)?.origin();
    let mut transport = HostTransport {
        endpoint: args.endpoint.map(|address| (origin, address)),
    };
    let mut interrupts = Interrupts::new()?;
    let operation = interrupts.begin();
    let cancelled = operation.flag();
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    let response = femto_catalog::http::fetch_progress_versioned(
        &mut transport,
        &args.source,
        deadline,
        cancelled.clone(),
        auth.as_ref(),
    )?;
    let progress = Progress::decode(&response.body)?;
    let outcome = apply(
        &args.book,
        &args.state,
        &progress,
        expected.as_ref(),
        || {
            anyhow::ensure!(
                !cancelled.load(Ordering::Acquire),
                "progress request cancelled"
            );
            anyhow::ensure!(Instant::now() < deadline, "progress request deadline");
            Ok(())
        },
    )?;
    report(
        outcome,
        Some((
            &response.source,
            response.etag.as_ref().map(|tag| tag.as_str()),
        )),
    )
}

#[cfg(all(unix, not(target_os = "espidf")))]
#[derive(clap::Args)]
#[command(group(clap::ArgGroup::new("condition").required(true).args(["create", "if_match"])))]
pub struct PushArgs {
    book: std::path::PathBuf,
    source: String,
    #[arg(long)]
    state: std::path::PathBuf,
    /// Create only if the endpoint has no current representation.
    #[arg(long)]
    create: bool,
    /// Exact quoted strong ETag from the remote record being replaced.
    #[arg(long)]
    if_match: Option<String>,
    #[arg(long)]
    credentials: Option<std::path::PathBuf>,
    #[arg(long, requires = "credentials")]
    allow_http_auth: bool,
    #[arg(long)]
    endpoint: Option<std::net::SocketAddr>,
    #[arg(long, default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..=60))]
    timeout: u64,
}
#[cfg(all(unix, not(target_os = "espidf")))]
pub fn push(args: PushArgs) -> anyhow::Result<()> {
    use crate::catalog::{HostTransport, Interrupts, credentials};
    use femto_catalog::http::{Precondition, StrongEtag};
    use std::time::{Duration, Instant};
    let etag = args
        .if_match
        .as_deref()
        .map(StrongEtag::parse)
        .transpose()?;
    let condition = match etag.as_ref() {
        Some(tag) => Precondition::Match(tag),
        None => {
            anyhow::ensure!(args.create, "upload requires a precondition");
            Precondition::Create
        }
    };
    let auth = args
        .credentials
        .as_deref()
        .map(|path| credentials(path, &args.source, args.allow_http_auth))
        .transpose()?;
    let state_file = crate::state_file::StateFile::acquire(&args.state)?;
    let mut document = Document::open(&args.book)?;
    let state = ReaderState::load(state_file.path())?;
    let progress = Progress::from_state(&state, &document.metadata.id)
        .context("book has no saved progress")?;
    progress.validate_document(&mut document)?;
    let bytes = serde_json::to_vec(&progress)?;
    let origin = url::Url::parse(&args.source)?.origin();
    let mut transport = HostTransport {
        endpoint: args.endpoint.map(|address| (origin, address)),
    };
    let mut interrupts = Interrupts::new()?;
    let operation = interrupts.begin();
    let result = femto_catalog::http::put_progress(
        &mut transport,
        &args.source,
        Instant::now() + Duration::from_secs(args.timeout),
        operation.flag(),
        auth.as_ref(),
        &bytes,
        condition,
    );
    let etag = match result {
        Err(femto_catalog::http::Error::Status(412)) => anyhow::bail!("remote progress conflict (HTTP 412); fetch the current record before retrying"),
        result => result.context("progress upload failed; remote commit may be unknown, inspect the remote record before retrying")?,
    };
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"status":"uploaded", "source":args.source, "etag":etag.as_str(), "book_id":progress.book_id})
        )?
    );
    Ok(())
}
