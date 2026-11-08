use femto_catalog::{auth::BasicAuth, browser::Browser, worker::Worker};
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
    Refresh,
    Publication(usize),
    Back,
    None,
}
pub type TransportFactory =
    Arc<dyn Fn(&[String]) -> io::Result<Box<dyn femto_catalog::http::Transport>> + Send + Sync>;

pub struct CatalogMenu {
    origins: Vec<String>,
    transport: Option<TransportFactory>,
    source: String,
    directory: PathBuf,
    auth: Option<BasicAuth>,
    browser: Option<Browser>,
    actions: Vec<Action>,
    pending: Option<(u64, bool)>,
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
    #[cfg(any(test, target_os = "espidf"))]
    pub fn load() -> Result<Option<Self>, String> {
        Self::load_from(Path::new("/sdcard/femto"))
    }
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
        let mut menu = Self::new(source)?;
        menu.directory = root.to_owned();
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
                menu.origins = text
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned)
                    .collect();
                let refs: Vec<_> = menu.origins.iter().map(String::as_str).collect();
                femto_catalog::routing::Routed::new((), (), &refs)
                    .map_err(|_| "Invalid catalog tailnet origins")?;
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
                menu.auth =
                    Some(BasicAuth::from_json(&menu.source, &bytes).map_err(|e| e.to_string())?);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("Cannot open catalog credentials".into()),
        }
        Ok(Some(menu))
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
            actions: Vec::new(),
            pending: None,
            detail: false,
            search: None,
            download_progress: None,
        })
    }
    fn start(
        &mut self,
        source: String,
        back: bool,
        worker: &mut Jobs,
        ui: &mut ReaderUi,
    ) -> Result<(), String> {
        let auth = self.auth.clone();
        let factory = self.transport.clone();
        let origins = self.origins.clone();
        let deadline = Instant::now() + Duration::from_secs(30);
        let id = worker
            .submit(deadline, move |_, cancel| {
                let mut transport = request_transport(factory.as_ref(), &origins)?;
                let feed = femto_catalog::http::fetch_authenticated(
                    &mut transport,
                    &source,
                    deadline,
                    cancel,
                    auth.as_ref(),
                )?;
                Ok(Output::Feed(Box::new(feed)))
            })
            .map_err(|e| e.to_string())?;
        self.pending = Some((id, back));
        ui.set_catalog_view("Loading catalog", &["Cancel".into()])
            .map_err(|e| e.to_string())
    }
    fn download(
        &mut self,
        source: String,
        worker: &mut Jobs,
        ui: &mut ReaderUi,
    ) -> Result<(), String> {
        let directory = self.directory.clone();
        let auth = self.auth.clone();
        let factory = self.transport.clone();
        let origins = self.origins.clone();
        let deadline = Instant::now() + Duration::from_secs(120);
        let count = Arc::new(DownloadCount::default());
        let observed = count.clone();
        let id = worker
            .submit(deadline, move |budget, cancel| {
                let mut transport = request_transport(factory.as_ref(), &origins)?;
                let mut staged = femto_reader::download::Staged::new(&directory)
                    .map_err(std::io::Error::other)?;
                femto_catalog::http::download_epub(
                    &mut transport,
                    &source,
                    deadline,
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
            })
            .map_err(|e| e.to_string())?;
        self.pending = Some((id, false));
        self.download_progress = Some(DownloadProgress {
            count: observed,
            last: Instant::now(),
            shown: (0, false),
        });
        ui.set_catalog_view("Downloading book", &["Cancel".into()])
            .map_err(|e| e.to_string())
    }
    fn show(&mut self, ui: &mut ReaderUi) -> Result<(), String> {
        self.detail = false;
        self.search = None;
        let Some(browser) = &self.browser else {
            ui.open();
            return Ok(());
        };
        let mut rows = vec!["Refresh".into()];
        let mut actions = vec![Action::Refresh];
        for (index, link) in browser.searches().enumerate() {
            rows.push(
                link.title
                    .clone()
                    .unwrap_or_else(|| "Search catalog".into()),
            );
            actions.push(Action::Search(index));
        }
        for destination in browser.destinations() {
            rows.push(destination.label.to_owned());
            actions.push(Action::Follow(destination.source.to_owned()));
        }
        for (index, publication) in browser.publications().enumerate() {
            rows.push(format!("Book: {}", publication.title));
            actions.push(Action::Publication(index));
        }
        ui.set_catalog_view(&browser.current().title, &rows)
            .map_err(|e| e.to_string())?;
        self.actions = actions;
        Ok(())
    }
    pub fn event(
        &mut self,
        event: CatalogEvent,
        worker: &mut Jobs,
        ui: &mut ReaderUi,
    ) -> Result<(), String> {
        if self.pending.is_some() {
            worker.cancel();
            self.pending = None;
            self.download_progress = None;
            return self.show(ui);
        }
        match event {
            CatalogEvent::Search => {
                let index = self.search.ok_or("No catalog search selected")?;
                let source = self
                    .browser
                    .as_ref()
                    .ok_or("No catalog loaded")?
                    .search_url(index, ui.query())
                    .map_err(|e| e.to_string())?;
                self.start(source, false, worker, ui)
            }
            CatalogEvent::Open if self.browser.is_none() => {
                self.start(self.source.clone(), false, worker, ui)
            }
            CatalogEvent::Open => self.show(ui),
            CatalogEvent::Back if self.detail || self.search.is_some() => self.show(ui),
            CatalogEvent::Back => {
                if let Some(source) = self.browser.as_ref().and_then(|b| b.back_source()) {
                    self.start(source.to_owned(), true, worker, ui)
                } else {
                    ui.open();
                    Ok(())
                }
            }
            CatalogEvent::Activate(index) => match self.actions.get(index) {
                Some(Action::Download(source)) => self.download(source.clone(), worker, ui),
                Some(Action::Search(index)) => {
                    self.search = Some(*index);
                    ui.open_catalog_search();
                    Ok(())
                }
                Some(Action::Follow(source)) => self.start(source.clone(), false, worker, ui),
                Some(Action::Refresh) => self.start(
                    self.browser.as_ref().unwrap().current().source.clone(),
                    false,
                    worker,
                    ui,
                ),
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
    pub fn poll(&mut self, worker: &mut Jobs, ui: &mut ReaderUi) -> bool {
        let Some(completion) = worker.poll() else {
            if self.pending.is_some()
                && ui.is_catalog_open()
                && let Some(title) = self
                    .download_progress
                    .as_mut()
                    .and_then(|p| p.title(Instant::now()))
            {
                return ui.set_catalog_view(&title, &["Cancel".into()]).is_ok();
            }
            return false;
        };
        let Some((id, back)) = self.pending else {
            return false;
        };
        if id != completion.id {
            return false;
        }
        self.pending = None;
        self.download_progress = None;
        if !ui.is_catalog_open() {
            return false;
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
                        .map_err(|e| e.to_string())
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
                let candidate = Browser::new(*feed);
                if 1 + candidate.searches().count()
                    + candidate.destinations().len()
                    + candidate.publications().count()
                    > 2048
                {
                    let _ = self.show(ui);
                    ui.show_error("Catalog exceeds menu row limit".into());
                    return true;
                }
                let feed = candidate.into_current();
                if let Some(browser) = &mut self.browser {
                    if back {
                        let _ = browser.back(|_| Ok::<_, ()>(feed));
                    } else {
                        let source = feed.source.clone();
                        let _ = browser.follow(&source, |_| Ok::<_, ()>(feed));
                    }
                } else {
                    self.browser = Some(Browser::new(feed));
                }
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
        while worker.is_busy() {
            menu.poll(worker, ui);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
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
        menu.event(CatalogEvent::Activate(1), &mut worker, &mut ui)
            .unwrap();
        finish(&mut menu, &mut worker, &mut ui);
        assert_eq!(menu.browser.as_ref().unwrap().current().title, "/next");
        menu.event(CatalogEvent::Back, &mut worker, &mut ui)
            .unwrap();
        finish(&mut menu, &mut worker, &mut ui);
        assert_eq!(menu.browser.as_ref().unwrap().current().title, "/start");
        menu.event(CatalogEvent::Activate(2), &mut worker, &mut ui)
            .unwrap();
        assert!(menu.detail);
        menu.event(CatalogEvent::Back, &mut worker, &mut ui)
            .unwrap();
        assert!(!menu.detail);
        menu.start(
            "https://example.com/fail".into(),
            false,
            &mut worker,
            &mut ui,
        )
        .unwrap();
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
        menu.start(
            "https://other.example/next".into(),
            false,
            &mut worker,
            &mut ui,
        )
        .unwrap();
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
        menu.event(CatalogEvent::Activate(1), &mut worker, &mut ui)
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
        menu.event(CatalogEvent::Activate(1), &mut worker, &mut ui)
            .unwrap();
        menu.event(CatalogEvent::Back, &mut worker, &mut ui)
            .unwrap();
        assert!(!ui.is_editing());
        assert!(!worker.is_busy());
        assert_eq!(menu.browser.as_ref().unwrap().history_len(), 0);
        menu.event(CatalogEvent::Activate(1), &mut worker, &mut ui)
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
        assert_eq!(menu.browser.as_ref().unwrap().history_len(), 0);
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
            .submit(Instant::now() + Duration::from_secs(10), move |_, _| {
                started.send(()).unwrap();
                let _ = receive.recv();
                Err(io::Error::other("test completion").into())
            })
            .unwrap();
        // Cancellation before entry correctly skips the operation. This test
        // needs an active operation whose completion remains under its control.
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        menu.pending = Some((id, false));
        let count = Arc::new(DownloadCount::default());
        count.bytes.store(4096, Ordering::Relaxed);
        menu.download_progress = Some(DownloadProgress {
            count,
            last: Instant::now() - Duration::from_secs(2),
            shown: (0, false),
        });
        let mut ui = ReaderUi::default();
        assert!(!menu.poll(&mut worker, &mut ui));
        ui.set_catalog_view("Downloading book", &["Cancel".into()])
            .unwrap();
        assert!(menu.poll(&mut worker, &mut ui));
        assert!(!menu.poll(&mut worker, &mut ui));
        menu.event(CatalogEvent::Back, &mut worker, &mut ui)
            .unwrap();
        assert!(menu.pending.is_none());
        assert!(menu.download_progress.is_none());
        send.send(()).unwrap();
        assert!(worker.cancel_and_wait(Duration::from_secs(1)));
        assert!(!menu.poll(&mut worker, &mut ui));
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
