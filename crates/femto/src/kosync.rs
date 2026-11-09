use anyhow::{Context, Result, ensure};
use femto_catalog::http::Transport;
use femto_kosync::{
    book::{Book, boundary},
    config::{Route, Settings},
};
use femto_reader::state::ReaderState;
use std::{
    fs::OpenOptions,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Operation {
    Register,
    Auth,
    Push,
    Pull,
    Status,
}
#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    config: PathBuf,
    #[arg(long, global = true, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=300))]
    timeout: u64,
    /// Numeric LAN endpoint override; HTTPS still verifies the configured hostname.
    #[arg(long, global = true)]
    endpoint: Option<std::net::SocketAddr>,
    #[command(subcommand)]
    action: Action,
}
#[derive(clap::Subcommand)]
enum Action {
    /// Create an account on the configured KOReader server.
    Register,
    /// Verify the configured account credentials.
    Auth,
    /// Upload this book's saved reading position.
    Push(BookArgs),
    /// Download and save this book's remote reading position.
    Pull(BookArgs),
    /// Inspect remote progress without changing local reading state.
    Status(BookArgs),
}
#[derive(clap::Args)]
struct BookArgs {
    book: PathBuf,
    #[arg(long)]
    state: PathBuf,
}

pub fn settings(path: &Path) -> Result<Settings> {
    Ok(Settings::load(path)?)
}

pub fn run(args: Args) -> Result<()> {
    let settings = settings(&args.config)?;
    ensure!(
        settings.route == Route::Lan,
        "tailnet configuration requires tailnet-connect --kosync-config"
    );
    let (operation, book) = match args.action {
        Action::Register => (Operation::Register, None),
        Action::Auth => (Operation::Auth, None),
        Action::Push(book) => (Operation::Push, Some(book)),
        Action::Pull(book) => (Operation::Pull, Some(book)),
        Action::Status(book) => (Operation::Status, Some(book)),
    };
    let mut interrupts = crate::catalog::Interrupts::new()?;
    let interrupt = interrupts.begin();
    let cancelled = interrupt.flag();
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    let origin = url::Url::parse(settings.client.server())?.origin();
    let mut transport = crate::catalog::HostTransport {
        endpoint: args.endpoint.map(|endpoint| (origin, endpoint)),
    };
    let mut request = Prepared::new(
        settings,
        operation,
        book.as_ref().map(|book| book.book.as_path()),
        book.as_ref().map(|book| book.state.as_path()),
        deadline,
        &cancelled,
    )?;
    let result = request.exchange(&mut transport, cancelled)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

pub struct Prepared {
    pub settings: Settings,
    operation: Operation,
    book: Option<(Book, crate::state_file::StateFile, ReaderState)>,
    pub deadline: Instant,
}
impl Prepared {
    pub fn new(
        settings: Settings,
        operation: Operation,
        book: Option<&Path>,
        state: Option<&Path>,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Self> {
        boundary(deadline, cancelled)?;
        let requires_book = matches!(
            operation,
            Operation::Push | Operation::Pull | Operation::Status
        );
        ensure!(
            requires_book == (book.is_some() && state.is_some())
                && book.is_some() == state.is_some(),
            "select book and state only for push, pull, or status"
        );
        let book = if let (Some(path), Some(state)) = (book, state) {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)?;
            let book = Book::from_file(path, file, settings.checksum, deadline, cancelled)?;
            let lock = crate::state_file::StateFile::acquire(state)?;
            let state = ReaderState::load(lock.path())?;
            Some((book, lock, state))
        } else {
            None
        };
        Ok(Self {
            settings,
            operation,
            book,
            deadline,
        })
    }
    pub fn exchange(
        &mut self,
        transport: &mut impl Transport,
        cancelled: Arc<AtomicBool>,
    ) -> Result<serde_json::Value> {
        let client = &self.settings.client;
        boundary(self.deadline, &cancelled)?;
        match self.operation {
            Operation::Register => {
                client.register(transport, self.deadline, cancelled)?;
                Ok(serde_json::json!({"status":"registered"}))
            }
            Operation::Auth => {
                client.authorize(transport, self.deadline, cancelled)?;
                Ok(serde_json::json!({"status":"authenticated"}))
            }
            Operation::Push => {
                let (book, _, state) = self.book.as_mut().context("book required")?;
                let progress = book.prepare_push(state, &self.settings)?;
                boundary(self.deadline, &cancelled)?;
                let receipt = client.push(transport, &progress, self.deadline, cancelled)?;
                Ok(
                    serde_json::json!({"status":"uploaded","document":receipt.document,"timestamp":receipt.timestamp,"percentage":progress.percentage}),
                )
            }
            Operation::Pull | Operation::Status => {
                let (book, lock, state) = self.book.as_mut().context("book required")?;
                let Some(remote) =
                    client.pull(transport, &book.id, self.deadline, cancelled.clone())?
                else {
                    return Ok(
                        serde_json::json!({"status":"no_remote_progress","document":book.id}),
                    );
                };
                let location = book.resolve_remote(&remote)?;
                boundary(self.deadline, &cancelled)?;
                if matches!(self.operation, Operation::Pull) {
                    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                    state.set_position(&book.document.metadata.id, location, now);
                    state.save(lock.path())?;
                }
                Ok(
                    serde_json::json!({"status":if matches!(self.operation,Operation::Pull) {"downloaded"} else {"remote_progress"},"document":remote.position.document,"timestamp":remote.timestamp,"percentage":remote.position.percentage,"device":remote.position.device,"position":location}),
                )
            }
        }
    }
}
