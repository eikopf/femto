use femto_catalog::{
    auth::BasicAuth, browser::Browser, deadline::Limit, search::Search, worker::Worker,
};
use femto_reader::ui::{CatalogEvent, ReaderUi};
use std::{
    fs::File,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use zeroize::Zeroizing;

pub enum Output {
    Feed(Box<femto_catalog::Catalog>),
    Book {
        ready: Box<femto_reader::download::Ready>,
        budget: femto_catalog::deadline::Budget,
    },
}
pub type Jobs = Worker<Output>;

enum Action {
    Download(String),
    Search(usize),
    Follow(String),
    Publication(usize),
    Back,
    None,
}
/// A listing the reader came through, kept so that Back is immediate.
struct Level {
    feed: femto_catalog::Catalog,
    position: usize,
}
/// Work the reader is waiting on.
enum Request {
    Enter(Search),
    Download(String),
}
enum Pending {
    Enter,
    /// The next page of the listing on screen; nobody is waiting on it.
    More,
    Download,
}
const MAX_LEVELS: usize = 8;
/// Entries in one listing.
const MAX_ENTRIES: usize = 1024;
/// Further pages are fetched while fewer entries than this lie ahead.
const AHEAD: usize = 100;
pub type TransportFactory =
    Arc<dyn Fn(&[String]) -> io::Result<Box<dyn femto_catalog::http::Transport>> + Send + Sync>;

pub struct CatalogMenu {
    origins: Vec<String>,
    transport: Option<TransportFactory>,
    source: String,
    directory: PathBuf,
    auth: Option<BasicAuth>,
    browser: Option<Browser>,
    above: Vec<Level>,
    /// The selected entry of the listing, while something else covers it.
    position: usize,
    actions: Vec<Action>,
    wanted: Option<Request>,
    pending: Option<(u64, Pending)>,
    /// The listing has no further pages worth asking for.
    complete: bool,
    detail: bool,
    search: Option<usize>,
    download_progress: Option<DownloadProgress>,
}
#[derive(Default)]
struct DownloadCount {
    bytes: AtomicU32,
    checking: AtomicBool,
}
struct Counted<'a, W> {
    output: &'a mut W,
    count: &'a DownloadCount,
}
impl<W: Write> Write for Counted<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let written = self.output.write(bytes)?;
        let amount = u32::try_from(written).unwrap_or(u32::MAX);
        let mut old = self.count.bytes.load(Ordering::Relaxed);
        loop {
            match self.count.bytes.compare_exchange_weak(
                old,
                old.saturating_add(amount),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => old = current,
            }
        }
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}
struct DownloadProgress {
    count: Arc<DownloadCount>,
    last: Instant,
    shown: (u32, bool),
}
impl DownloadProgress {
    fn title(&mut self, now: Instant) -> Option<String> {
        if now.saturating_duration_since(self.last) < Duration::from_secs(2) {
            return None;
        }
        let checking = self.count.checking.load(Ordering::Acquire);
        let kib = self.count.bytes.load(Ordering::Relaxed) / 1024;
        if self.shown == (kib, checking) {
            return None;
        }
        self.shown = (kib, checking);
        self.last = now;
        Some(if checking {
            "Checking book".into()
        } else {
            format!("Downloading {kib} KiB")
        })
    }
}
impl CatalogMenu {
    pub fn set_transport_factory(&mut self, factory: TransportFactory) {
        self.transport = Some(factory);
    }
    #[cfg(test)]
    pub fn load() -> Result<Option<Self>, String> {
        Self::load_from(Path::new("/sdcard/femto"))
    }
    /// The file-based configuration, exercised by host tests; the device
    /// reads its settings from internal storage.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn load_from(root: &Path) -> Result<Option<Self>, String> {
        let file = match File::open(root.join("catalog.url")) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("Cannot read catalog configuration".into()),
        };
        let mut bytes = Vec::new();
        file.take(4097)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read catalog configuration")?;
        if bytes.len() > 4096 {
            return Err("Catalog configuration exceeds 4096 bytes".into());
        }
        let source = std::str::from_utf8(&bytes)
            .map_err(|_| "Invalid catalog URL")?
            .trim()
            .to_owned();
        let mut origins = Vec::new();
        let mut auth = None;
        match File::open(root.join("catalog-tailnet-origins")) {
            Ok(file) => {
                if !file
                    .metadata()
                    .map_err(|_| "Cannot inspect catalog routes")?
                    .is_file()
                {
                    return Err("Catalog routes must be a regular file".into());
                }
                let mut bytes = Vec::new();
                file.take(8193)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "Cannot read catalog routes")?;
                if bytes.len() > 8192 {
                    return Err("Catalog routes exceed 8192 bytes".into());
                }
                let text = std::str::from_utf8(&bytes).map_err(|_| "Invalid catalog routes")?;
                origins = text
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(_) => return Err("Cannot open catalog routes".into()),
        }
        match File::open(root.join("catalog-auth.json")) {
            Ok(file) => {
                if !file
                    .metadata()
                    .map_err(|_| "Cannot inspect catalog credentials")?
                    .is_file()
                {
                    return Err("Catalog credentials must be a regular file".into());
                }
                let mut bytes = Zeroizing::new(Vec::with_capacity(8193));
                file.take(8193)
                    .read_to_end(&mut bytes)
                    .map_err(|_| "Cannot read catalog credentials")?;
                auth = Some(BasicAuth::from_json(&source, &bytes).map_err(|e| e.to_string())?);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Cannot open catalog credentials".into()),
        }
        Self::configured(source, origins, auth, root.to_owned()).map(Some)
    }
    /// A catalog from settings held by the caller: its address, the origins
    /// reached through the tailnet, optional credentials, and the directory
    /// that receives downloads.
    pub fn configured(
        source: String,
        origins: Vec<String>,
        auth: Option<BasicAuth>,
        directory: PathBuf,
    ) -> Result<Self, String> {
        let mut menu = Self::new(source)?;
        let refs: Vec<_> = origins.iter().map(String::as_str).collect();
        femto_catalog::routing::Routed::new((), (), &refs)
            .map_err(|_| "Invalid catalog tailnet origins")?;
        menu.origins = origins;
        menu.auth = auth;
        menu.directory = directory;
        Ok(menu)
    }
    pub fn new(source: String) -> Result<Self, String> {
        if source.len() > 4096 || source.chars().any(char::is_control) || source.contains('\\') {
            return Err("Invalid catalog URL".into());
        }
        let url = femto_catalog::http::Url::parse(&source).map_err(|_| "Invalid catalog URL")?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || url.port_or_known_default() == Some(0)
        {
            return Err("Invalid catalog URL".into());
        }
        Ok(Self {
            origins: Vec::new(),
            transport: None,
            source,
            directory: "/sdcard/femto".into(),
            auth: None,
            browser: None,
            above: Vec::new(),
            position: 0,
            actions: Vec::new(),
            wanted: None,
            pending: None,
            complete: false,
            detail: false,
            search: None,
            download_progress: None,
        })
    }
    fn fetch(&self, target: Search, worker: &mut Jobs) -> io::Result<u64> {
        let auth = self.auth.clone();
        let factory = self.transport.clone();
        let origins = self.origins.clone();
        worker.submit(Limit::SILENCE, move |_, cancel| {
            let mut transport = request_transport(factory.as_ref(), &origins)?;
            let feed = femto_catalog::http::fetch_search(
                &mut transport,
                &target,
                Limit::SILENCE,
                cancel,
                auth.as_ref(),
            )?;
            Ok(Output::Feed(Box::new(feed)))
        })
    }
    fn download(&mut self, source: String, worker: &mut Jobs) -> io::Result<u64> {
        let directory = self.directory.clone();
        let auth = self.auth.clone();
        let factory = self.transport.clone();
        let origins = self.origins.clone();
        let count = Arc::new(DownloadCount::default());
        let observed = count.clone();
        let id = worker.submit(Limit::SILENCE, move |budget, cancel| {
            let mut transport = request_transport(factory.as_ref(), &origins)?;
            let mut staged =
                femto_reader::download::Staged::new(&directory).map_err(std::io::Error::other)?;
            femto_catalog::http::download_epub(
                &mut transport,
                &source,
                Limit::SILENCE,
                cancel,
                auth.as_ref(),
                &mut Counted {
                    output: &mut staged,
                    count: &count,
                },
            )?;
            budget.check()?;
            count.checking.store(true, Ordering::Release);
            let ready = staged.validate().map_err(std::io::Error::other)?;
            budget.check()?;
            Ok(Output::Book {
                ready: Box::new(ready),
                budget,
            })
        })?;
        self.download_progress = Some(DownloadProgress {
            count: observed,
            last: Instant::now(),
            shown: (0, false),
        });
        Ok(id)
    }
    /// Asks for something the reader then waits on. The view stays in place
    /// under the busy mark; a page being fetched in the background gives way.
    fn want(
        &mut self,
        request: Request,
        worker: &mut Jobs,
        ui: &mut ReaderUi,
    ) -> Result<(), String> {
        if let Some(position) = ui.catalog_position().filter(|_| !self.detail) {
            self.position = position;
        }
        worker.cancel();
        self.pending = None;
        self.wanted = Some(request);
        if !ui.is_catalog_open() {
            ui.set_catalog_view("Catalog", &[])
                .map_err(|e| e.to_string())?;
        }
        ui.set_catalog_busy(true);
        self.dispatch(worker, ui);
        Ok(())
    }
    /// Starts wanted work once the worker is free.
    fn dispatch(&mut self, worker: &mut Jobs, ui: &mut ReaderUi) {
        if worker.is_busy() {
            return;
        }
        let started = match self.wanted.take() {
            Some(Request::Enter(target)) => {
                self.fetch(target, worker).map(|id| (id, Pending::Enter))
            }
            Some(Request::Download(source)) => self
                .download(source, worker)
                .map(|id| (id, Pending::Download)),
            None => return,
        };
        match started {
            Ok(pending) => self.pending = Some(pending),
            Err(error) => {
                ui.set_catalog_busy(false);
                ui.show_error(error.to_string());
            }
        }
    }
    /// Fetches the next page of the listing on screen before it is reached.
    fn fetch_ahead(&mut self, worker: &mut Jobs, ui: &ReaderUi) {
        if self.complete || self.detail || self.pending.is_some() || worker.is_busy() {
            return;
        }
        let (Some(position), Some(browser)) = (ui.catalog_position(), &self.browser) else {
            return;
        };
        if self.actions.len() >= MAX_ENTRIES || position + AHEAD < self.actions.len() {
            return;
        }
        let Some(source) = browser.continuation() else {
            self.complete = true;
            return;
        };
        match self.fetch(Search::Feed(source.to_owned()), worker) {
            Ok(id) => self.pending = Some((id, Pending::More)),
            Err(_) => self.complete = true,
        }
    }
    /// Search, then sections, then books: one list, as far as it is loaded.
    fn rows(&mut self) -> Vec<String> {
        let mut rows = Vec::new();
        self.actions.clear();
        let Some(browser) = &self.browser else {
            return rows;
        };
        for (index, link) in browser.searches().enumerate() {
            rows.push(
                link.title
                    .clone()
                    .unwrap_or_else(|| "Search catalog".into()),
            );
            self.actions.push(Action::Search(index));
        }
        for destination in browser.destinations().iter().filter(|d| !d.paging) {
            rows.push(destination.label.to_owned());
            self.actions
                .push(Action::Follow(destination.source.to_owned()));
        }
        for (index, publication) in browser.publications().enumerate() {
            rows.push(match publication.authors.first() {
                Some(author) => format!("{} - {author}", publication.title),
                None => publication.title.clone(),
            });
            self.actions.push(Action::Publication(index));
        }
        rows.truncate(2048);
        self.actions.truncate(2048);
        rows
    }
    fn show(&mut self, ui: &mut ReaderUi) -> Result<(), String> {
        self.detail = false;
        self.search = None;
        if self.browser.is_none() {
            ui.close_catalog();
            return Ok(());
        }
        let rows = self.rows();
        let title = &self.browser.as_ref().unwrap().current().title;
        ui.set_catalog_view(title, &rows)
            .map_err(|e| e.to_string())?;
        ui.set_catalog_position(self.position);
        Ok(())
    }
    pub fn event(
        &mut self,
        event: CatalogEvent,
        worker: &mut Jobs,
        ui: &mut ReaderUi,
    ) -> Result<(), String> {
        // The view under a download stays as it was; its entries other than
        // the way back do nothing until the book has arrived.
        let downloading = matches!(self.wanted, Some(Request::Download(_)))
            || matches!(self.pending, Some((_, Pending::Download)));
        if downloading
            && let CatalogEvent::Activate(index) = event
            && !matches!(self.actions.get(index), Some(Action::Back))
        {
            return Ok(());
        }
        // Back abandons what was asked for.
        if self.wanted.is_some()
            || matches!(self.pending, Some((_, Pending::Enter | Pending::Download)))
        {
            worker.cancel();
            self.pending = None;
            self.wanted = None;
            self.download_progress = None;
            ui.set_catalog_busy(false);
            return self.show(ui);
        }
        match event {
            CatalogEvent::Search => {
                let index = self.search.ok_or("No catalog search selected")?;
                let search = self
                    .browser
                    .as_ref()
                    .ok_or("No catalog loaded")?
                    .search(index, ui.query())
                    .map_err(|e| e.to_string())?;
                self.show(ui)?;
                self.want(Request::Enter(search), worker, ui)
            }
            CatalogEvent::Open if self.browser.is_none() => self.want(
                Request::Enter(Search::Feed(self.source.clone())),
                worker,
                ui,
            ),
            CatalogEvent::Open => self.show(ui),
            CatalogEvent::Back if self.detail || self.search.is_some() => self.show(ui),
            CatalogEvent::Back => {
                let Some(level) = self.above.pop() else {
                    ui.close_catalog();
                    return Ok(());
                };
                // A page still arriving for the listing being left is dropped.
                worker.cancel();
                self.pending = None;
                self.browser = Some(Browser::new(level.feed));
                self.position = level.position;
                self.complete = false;
                self.show(ui)
            }
            CatalogEvent::Activate(index) => match self.actions.get(index) {
                Some(Action::Download(source)) => {
                    let source = source.clone();
                    self.want(Request::Download(source), worker, ui)?;
                    ui.set_catalog_title("Downloading book");
                    Ok(())
                }
                Some(Action::Search(index)) => {
                    self.search = Some(*index);
                    self.position = ui.catalog_position().unwrap_or(self.position);
                    ui.open_catalog_search();
                    Ok(())
                }
                Some(Action::Follow(source)) => {
                    let source = source.clone();
                    self.want(Request::Enter(Search::Feed(source)), worker, ui)
                }
                Some(Action::Back) => self.show(ui),
                Some(Action::Publication(index)) => {
                    let book = self
                        .browser
                        .as_ref()
                        .unwrap()
                        .publications()
                        .nth(*index)
                        .ok_or("Publication unavailable")?;
                    let mut rows = vec![
                        book.title.clone(),
                        format!("By {}", book.authors.join(", ")),
                    ];
                    let mut actions = vec![Action::None, Action::None];
                    for link in &book.links {
                        if let Some(source) = link.direct_epub() {
                            rows.push(link.title.clone().unwrap_or_else(|| "Download EPUB".into()));
                            actions.push(Action::Download(source.to_owned()));
                        }
                    }
                    if actions.len() == 2 {
                        rows.push("Download unavailable".into());
                        actions.push(Action::None);
                    }
                    rows.push("Back to catalog".into());
                    actions.push(Action::Back);
                    self.position = ui.catalog_position().unwrap_or(self.position);
                    ui.set_catalog_view("Publication", &rows)
                        .map_err(|e| e.to_string())?;
                    self.detail = true;
                    self.actions = actions;
                    Ok(())
                }
                _ => Ok(()),
            },
        }
    }
    /// Advances catalog work; true when the screen needs drawing again.
    /// `page_items` is the number of entries on one menu page.
    pub fn poll(&mut self, worker: &mut Jobs, ui: &mut ReaderUi, page_items: usize) -> bool {
        let Some(completion) = worker.poll() else {
            self.dispatch(worker, ui);
            if matches!(self.pending, Some((_, Pending::Download)))
                && ui.is_catalog_open()
                && let Some(title) = self
                    .download_progress
                    .as_mut()
                    .and_then(|p| p.title(Instant::now()))
            {
                ui.set_catalog_title(&title);
                return true;
            }
            self.fetch_ahead(worker, ui);
            return false;
        };
        let pending = match self.pending.take() {
            Some((id, pending)) if id == completion.id => pending,
            // Abandoned work has finished; what replaced it can start.
            other => {
                self.pending = other;
                self.dispatch(worker, ui);
                return false;
            }
        };
        if let Pending::More = pending {
            return self.extend(completion.result, ui, page_items);
        }
        self.download_progress = None;
        ui.set_catalog_busy(false);
        if !ui.is_catalog_open() {
            return true;
        }
        match completion.result {
            Ok(Output::Book { ready, budget }) => {
                let result = budget.check().map_err(|e| e.to_string()).and_then(|()| {
                    ready
                        .publish(|source, destination| {
                            // The pinned ESP-IDF FAT VFS uses f_rename without replacement.
                            #[cfg(target_os = "espidf")]
                            {
                                std::fs::rename(source, destination)
                            }
                            #[cfg(not(target_os = "espidf"))]
                            {
                                std::fs::hard_link(source, destination)
                            }
                        })
                        .map_err(|error| match error {
                            // Names are content hashes: the same book is there.
                            femto_reader::Error::Io(error)
                                if error.kind() == io::ErrorKind::AlreadyExists =>
                            {
                                "This book is already in the library".into()
                            }
                            error => error.to_string(),
                        })
                });
                let _ = self.show(ui);
                match result {
                    Ok(_) => {
                        if let Err(error) = ui.rescan_library() {
                            ui.show_error(format!("Book saved; {error}"));
                        }
                    }
                    Err(error) => ui.show_error(error),
                }
            }
            Ok(Output::Feed(feed)) => {
                // Loading a listing again replaces it; anything else is below it.
                if let Some(current) = self.browser.take().map(Browser::into_current)
                    && current.source != feed.source
                {
                    if self.above.len() == MAX_LEVELS {
                        self.above.remove(0);
                    }
                    self.above.push(Level {
                        feed: current,
                        position: self.position,
                    });
                }
                self.browser = Some(Browser::new(*feed));
                self.position = 0;
                self.complete = false;
                if let Err(error) = self.show(ui) {
                    ui.show_error(error);
                }
            }
            Err(error) => {
                let _ = self.show(ui);
                ui.show_error(error.to_string());
            }
        }
        true
    }
    /// Adds a page that arrived in the background. The screen is drawn again
    /// only if the new entries land on the page being shown.
    fn extend(
        &mut self,
        result: Result<Output, femto_catalog::http::Error>,
        ui: &mut ReaderUi,
        page_items: usize,
    ) -> bool {
        let (Ok(Output::Feed(page)), Some(browser)) = (result, &mut self.browser) else {
            self.complete = true;
            return false;
        };
        if !browser.append(*page) {
            self.complete = true;
            return false;
        }
        // Another view covers the list; it is rebuilt when shown again.
        let Some(position) = ui.catalog_position().filter(|_| !self.detail) else {
            return false;
        };
        let before = self.actions.len();
        let rows = self.rows();
        if ui.set_catalog_rows(&rows).is_err() {
            return false;
        }
        let items = page_items.max(1);
        !before.is_multiple_of(items) && position / items == before / items
    }
}

fn request_transport(
    factory: Option<&TransportFactory>,
    origins: &[String],
) -> io::Result<Box<dyn femto_catalog::http::Transport>> {
    match factory {
        Some(factory) => factory(origins),
        None if origins.is_empty() => Ok(Box::new(crate::network::Transport)),
        None => Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "Tailnet catalog transport unavailable",
        )),
    }
}

#[cfg(test)]
#[path = "../../../crates/femto/tests/support/catalog_network.rs"]
pub(crate) mod test_network;

#[cfg(test)]
mod tests {
    use super::*;
    fn finish(menu: &mut CatalogMenu, worker: &mut Jobs, ui: &mut ReaderUi) {
        let deadline = Instant::now() + Duration::from_secs(5);
        // The second round lets a background page start and arrive.
        for _ in 0..2 {
            loop {
                menu.poll(worker, ui, 6);
                if !worker.is_busy() && menu.wanted.is_none() {
                    break;
                }
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    fn enter(menu: &mut CatalogMenu, source: &str, worker: &mut Jobs, ui: &mut ReaderUi) {
        menu.want(Request::Enter(Search::Feed(source.into())), worker, ui)
            .unwrap();
    }
    #[test]
    fn feed_navigation_details_and_failure_preserve_state() {
        let _loader = CatalogMenu::load;
        let mut menu = CatalogMenu::new("https://example.com/start".into()).unwrap();
        menu.set_transport_factory(Arc::new(|_| Ok(Box::new(test_network::Transport))));
        let mut worker = Worker::default();
        let mut ui = ReaderUi::default();
        menu.event(CatalogEvent::Open, &mut worker, &mut ui)
            .unwrap();
        assert!(ui.is_catalog_open());
        finish(&mut menu, &mut worker, &mut ui);
        assert_eq!(menu.browser.as_ref().unwrap().current().title, "/start");
        // The next page joined the list in the background and ended it.
        assert_eq!(menu.actions.len(), 2);
        assert!(menu.browser.as_ref().unwrap().continuation().is_none() && !ui.is_busy());
        ui.set_catalog_position(1);
        enter(&mut menu, "https://example.com/next", &mut worker, &mut ui);
        // The list stays in place while the reader waits.
        assert!(ui.is_busy() && ui.catalog_position() == Some(1));
        finish(&mut menu, &mut worker, &mut ui);
        assert_eq!(menu.browser.as_ref().unwrap().current().title, "/next");
        assert_eq!(ui.catalog_position(), Some(0));
        // Back needs no request and returns to the entry that was left.
        menu.event(CatalogEvent::Back, &mut worker, &mut ui)
            .unwrap();
        assert!(!worker.is_busy());
        assert_eq!(menu.browser.as_ref().unwrap().current().title, "/start");
        assert_eq!(ui.catalog_position(), Some(1));
        assert_eq!(menu.actions.len(), 2);
        menu.event(CatalogEvent::Activate(0), &mut worker, &mut ui)
            .unwrap();
        assert!(menu.detail);
        menu.event(CatalogEvent::Back, &mut worker, &mut ui)
            .unwrap();
        assert!(!menu.detail);
        enter(&mut menu, "https://example.com/fail", &mut worker, &mut ui);
        finish(&mut menu, &mut worker, &mut ui);
        assert_eq!(menu.browser.as_ref().unwrap().current().title, "/start");
        assert!(ui.is_catalog_open());
    }
    #[test]
    fn credentials_keep_original_origin_across_navigation_and_back() {
        let mut menu = CatalogMenu::new("https://auth.example/start".into()).unwrap();
        menu.set_transport_factory(Arc::new(|_| Ok(Box::new(test_network::Transport))));
        menu.auth = Some(
            BasicAuth::from_json(
                &menu.source,
                br#"{"username":"reader","password":"secret"}"#,
            )
            .unwrap(),
        );
        let mut worker = Worker::default();
        let mut ui = ReaderUi::default();
        menu.event(CatalogEvent::Open, &mut worker, &mut ui)
            .unwrap();
        finish(&mut menu, &mut worker, &mut ui);
        assert_eq!(menu.browser.as_ref().unwrap().current().title, "/start");
        enter(
            &mut menu,
            "https://other.example/next",
            &mut worker,
            &mut ui,
        );
        finish(&mut menu, &mut worker, &mut ui);
        assert_eq!(menu.browser.as_ref().unwrap().current().title, "/next");
        menu.event(CatalogEvent::Back, &mut worker, &mut ui)
            .unwrap();
        finish(&mut menu, &mut worker, &mut ui);
        assert_eq!(menu.browser.as_ref().unwrap().current().title, "/start");
    }
    #[test]
    fn search_expands_query_and_back_restores_the_source_feed() {
        let mut menu = CatalogMenu::new("https://example.com/searchable".into()).unwrap();
        menu.set_transport_factory(Arc::new(|_| Ok(Box::new(test_network::Transport))));
        let mut worker = Worker::default();
        let mut ui = ReaderUi::default();
        menu.event(CatalogEvent::Open, &mut worker, &mut ui)
            .unwrap();
        finish(&mut menu, &mut worker, &mut ui);
        menu.event(CatalogEvent::Activate(0), &mut worker, &mut ui)
            .unwrap();
        assert!(ui.is_editing());
        assert!(
            menu.event(CatalogEvent::Search, &mut worker, &mut ui)
                .is_err()
        );
        assert!(!worker.is_busy());
        for c in "café & snow".chars() {
            ui.type_char(c);
        }
        ui.submit_search();
        let event = ui.take_catalog_event().unwrap();
        menu.event(event, &mut worker, &mut ui).unwrap();
        finish(&mut menu, &mut worker, &mut ui);
        assert_eq!(
            menu.browser.as_ref().unwrap().current().source,
            "https://example.com/results?query=caf%C3%A9%20%26%20snow"
        );
        menu.event(CatalogEvent::Back, &mut worker, &mut ui)
            .unwrap();
        finish(&mut menu, &mut worker, &mut ui);
        assert_eq!(
            menu.browser.as_ref().unwrap().current().title,
            "/searchable"
        );
        menu.event(CatalogEvent::Activate(0), &mut worker, &mut ui)
            .unwrap();
        menu.event(CatalogEvent::Back, &mut worker, &mut ui)
            .unwrap();
        assert!(!ui.is_editing());
        assert!(!worker.is_busy());
        assert!(menu.above.is_empty());
        menu.event(CatalogEvent::Activate(0), &mut worker, &mut ui)
            .unwrap();
        ui.type_char('x');
        menu.event(CatalogEvent::Search, &mut worker, &mut ui)
            .unwrap();
        menu.event(CatalogEvent::Back, &mut worker, &mut ui)
            .unwrap();
        finish(&mut menu, &mut worker, &mut ui);
        assert_eq!(
            menu.browser.as_ref().unwrap().current().title,
            "/searchable"
        );
        assert!(menu.above.is_empty());
    }
    #[test]
    fn cancelling_initial_load_cannot_reopen_closed_menu() {
        let mut menu = CatalogMenu::new("https://example.com/start".into()).unwrap();
        menu.set_transport_factory(Arc::new(|_| Ok(Box::new(test_network::Transport))));
        let mut worker = Worker::default();
        let mut ui = ReaderUi::default();
        menu.event(CatalogEvent::Open, &mut worker, &mut ui)
            .unwrap();
        menu.event(CatalogEvent::Back, &mut worker, &mut ui)
            .unwrap();
        assert!(!ui.is_catalog_open());
        finish(&mut menu, &mut worker, &mut ui);
        assert!(menu.browser.is_none());
        assert!(!ui.is_catalog_open());
        for source in [
            "file:///tmp/feed",
            "https://user:secret@example.com",
            "https://example.com/#fragment",
            "http://example.com:0",
        ] {
            assert!(CatalogMenu::new(source.into()).is_err());
        }
    }
}

#[cfg(test)]
mod download_progress_tests {
    use super::*;

    #[test]
    fn polling_refreshes_visible_progress_and_cancel_discards_it() {
        let mut menu = CatalogMenu::new("http://books.example/feed".into()).unwrap();
        let mut worker = Jobs::default();
        let (send, receive) = std::sync::mpsc::channel::<()>();
        let (started, ready) = std::sync::mpsc::channel();
        let id = worker
            .submit(
                femto_catalog::deadline::Limit::Until(Instant::now() + Duration::from_secs(10)),
                move |_, _| {
                    started.send(()).unwrap();
                    let _ = receive.recv();
                    Err(io::Error::other("test completion").into())
                },
            )
            .unwrap();
        // Cancellation before entry correctly skips the operation. This test
        // needs an active operation whose completion remains under its control.
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        menu.pending = Some((id, Pending::Download));
        let count = Arc::new(DownloadCount::default());
        count.bytes.store(4096, Ordering::Relaxed);
        menu.download_progress = Some(DownloadProgress {
            count,
            last: Instant::now() - Duration::from_secs(2),
            shown: (0, false),
        });
        let mut ui = ReaderUi::default();
        assert!(!menu.poll(&mut worker, &mut ui, 6));
        ui.set_catalog_view("Publication", &["Download EPUB".into()])
            .unwrap();
        assert!(menu.poll(&mut worker, &mut ui, 6));
        assert!(!menu.poll(&mut worker, &mut ui, 6));
        menu.event(CatalogEvent::Activate(0), &mut worker, &mut ui)
            .unwrap();
        assert!(menu.pending.is_some());
        menu.event(CatalogEvent::Back, &mut worker, &mut ui)
            .unwrap();
        assert!(menu.pending.is_none());
        assert!(menu.download_progress.is_none());
        send.send(()).unwrap();
        assert!(worker.cancel_and_wait(Duration::from_secs(1)));
        assert!(!menu.poll(&mut worker, &mut ui, 6));
        assert!(!ui.is_catalog_open());
    }

    #[test]
    fn accepted_partial_writes_are_counted_and_errors_are_preserved() {
        struct Sink {
            bytes: Vec<u8>,
            flushed: bool,
        }
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.bytes.len() >= 4 {
                    return Err(io::ErrorKind::StorageFull.into());
                }
                let n = bytes.len().min(2);
                self.bytes.extend_from_slice(&bytes[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> io::Result<()> {
                self.flushed = true;
                Ok(())
            }
        }
        let count = DownloadCount::default();
        let mut sink = Sink {
            bytes: Vec::new(),
            flushed: false,
        };
        let mut writer = Counted {
            output: &mut sink,
            count: &count,
        };
        assert_eq!(
            writer.write_all(b"abcdef").unwrap_err().kind(),
            io::ErrorKind::StorageFull
        );
        assert_eq!(count.bytes.load(Ordering::Relaxed), 4);
        writer.flush().unwrap();
        assert_eq!(sink.bytes, b"abcd");
        assert!(sink.flushed);
    }

    #[test]
    fn progress_titles_are_throttled_and_validation_has_its_own_phase() {
        let start = Instant::now();
        let count = Arc::new(DownloadCount::default());
        let mut progress = DownloadProgress {
            count: count.clone(),
            last: start,
            shown: (0, false),
        };
        count.bytes.store(4095, Ordering::Relaxed);
        assert!(progress.title(start + Duration::from_secs(1)).is_none());
        assert_eq!(
            progress.title(start + Duration::from_secs(2)).as_deref(),
            Some("Downloading 3 KiB")
        );
        count.bytes.store(4096, Ordering::Relaxed);
        assert!(progress.title(start + Duration::from_secs(3)).is_none());
        assert_eq!(
            progress.title(start + Duration::from_secs(4)).as_deref(),
            Some("Downloading 4 KiB")
        );
        assert!(progress.title(start + Duration::from_secs(6)).is_none());
        count.checking.store(true, Ordering::Release);
        assert_eq!(
            progress.title(start + Duration::from_secs(6)).as_deref(),
            Some("Checking book")
        );
        assert!(progress.title(start + Duration::from_secs(8)).is_none());
    }
}
