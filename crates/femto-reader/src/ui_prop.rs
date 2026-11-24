//! Discovery harness: random input and service-event histories replayed
//! against the real `ReaderUi` and `Reader` over fixture books, checked
//! against small independent models after every step.
//!
//! Run with
//! `cargo test -p femto-reader --release --lib ui::machine -- --nocapture`.
use super::*;
use crate::{
    document::Document,
    layout::{Font, Fonts, Locator},
    state::{LibraryEntry, LibraryMetadata, ReaderState},
};
use proptest::prelude::*;
use std::{collections::HashMap, path::Path, sync::Mutex};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}
fn fonts() -> Fonts {
    Fonts::new(Font::load(fixture("DejaVuSerif.ttf")).unwrap())
}
const BOOKS: [&str; 3] = ["navigation.epub", "supplemental-notes.epub", "cover.epub"];
fn library(count: usize) -> Library {
    static ENTRIES: std::sync::OnceLock<Vec<LibraryEntry>> = std::sync::OnceLock::new();
    let entries = ENTRIES.get_or_init(|| {
        BOOKS
            .iter()
            .map(|name| {
                let path = fixture(name);
                let document = Document::open(&path).unwrap();
                LibraryEntry {
                    path,
                    metadata: LibraryMetadata {
                        id: document.metadata.id,
                        title: document.metadata.title,
                        author: document.metadata.author,
                        language: document.metadata.language,
                    },
                }
            })
            .collect()
    });
    Library {
        entries: entries.iter().take(count).cloned().collect(),
        errors: (0..count).map(|i| format!("scan error {i}")).collect(),
        truncated: false,
    }
}

#[derive(Clone, Debug)]
pub(super) enum Op {
    /// A page button, by the direction the frontend resolved.
    Turn(bool),
    Swipe(bool),
    /// A page button whose pending work is left to later polls.
    TurnLazy(bool),
    Select,
    Back,
    Power,
    /// Per-mille of the viewport.
    Tap(u16, u16),
    Gesture(u8),
    /// Moves the selection to an entry with the buttons and selects it.
    Choose(u8),
    Type(char),
    Submit,
    /// Polls every pending job this many times.
    Poll(u8),
    Drain,
    /// Presses Back until the page shows.
    Home,
    SaveReload,
    // Service events, arriving whenever.
    AskConflict(u8, Option<u8>),
    KosyncDone,
    CatalogDone(u8),
    CatalogRows(u8),
    TailnetStatus(u8),
    SetUpdate(u8),
    DeviceStatus,
    SetLibrary(u8),
    DeviceRefresh(u8),
}

#[derive(Clone, Debug)]
pub(super) struct Config {
    pub sleep: bool,
    pub services: bool,
    pub landscape: bool,
    pub status_line: bool,
    /// What the three reading gestures do, when not the defaults.
    pub gestures: Option<[usize; 3]>,
}

/// Which invariants to enforce; known findings are switched off to search
/// past them.
#[derive(Clone, Copy, Debug)]
pub(super) struct Checks {
    pub honest_effect: bool,
    pub depth: bool,
    pub conflict_answered: bool,
    pub pages: bool,
    /// Let service lists shrink under the selection.
    pub shrinking_rows: bool,
    /// Hold taps on the keyboard to the honest-effect check.
    pub keyboard_taps: bool,
}
impl Checks {
    #[allow(dead_code)]
    pub const ALL: Self = Self {
        honest_effect: true,
        depth: true,
        conflict_answered: true,
        pages: true,
        shrinking_rows: true,
        keyboard_taps: true,
    };
}

#[derive(Default, Debug)]
pub(super) struct Stats {
    pub panels: std::collections::BTreeSet<String>,
    pub max_depth: usize,
    pub page_turns: usize,
    pub page_checks: usize,
    pub chapter_crossings: usize,
    pub reflows: usize,
    pub opens: usize,
    pub navigations: usize,
    pub previews: usize,
    pub sleeps: usize,
    pub reloads: usize,
    pub conflicts: usize,
    pub slider_steps: usize,
    pub bookmarks: usize,
    pub busy_inputs: usize,
    pub errors_shown: usize,
}

/// Menu depth written out from the documented menu tree, not from
/// `Panel::parent`.
fn depth(panel: Option<Panel>) -> usize {
    use Panel::*;
    match panel {
        None => 0,
        Some(Root | SyncConflict) => 1,
        Some(Book | Contents | Bookmarks | Library | Catalog | Light | Typography | Settings) => 2,
        Some(
            Chapters | GoTo | Links | Clippings | Kosync | Device | Tailnet | Update | Power
            | Rotation | Refresh | DictionaryChooser | Remote | Gestures | Fonts | CatalogSearch
            | Books | Recent | Folders | Search | Errors | Delete,
        ) => 3,
        Some(
            DeviceText(_) | DeleteConfirm(_) | Results | Folder(_) | Diagnostic(_) | Clipping(_)
            | ClippingDelete | SelectWord | WordPreview | WordList,
        ) => 4,
        Some(DictionaryResult) => 5,
        Some(LinkPreview) => usize::MAX,
    }
}
fn name(panel: Option<Panel>) -> String {
    use Panel::*;
    match panel {
        None => "page",
        Some(p) => match p {
            Root => "Root",
            Book => "Book",
            Fonts => "Fonts",
            Gestures => "Gestures",
            Rotation => "Rotation",
            Catalog => "Catalog",
            CatalogSearch => "CatalogSearch",
            Device => "Device",
            DeviceText(_) => "DeviceText",
            Kosync => "Kosync",
            Tailnet => "Tailnet",
            Update => "Update",
            Contents => "Contents",
            Chapters => "Chapters",
            GoTo => "GoTo",
            SyncConflict => "SyncConflict",
            Bookmarks => "Bookmarks",
            Clippings => "Clippings",
            Clipping(_) => "Clipping",
            ClippingDelete => "ClippingDelete",
            SelectWord => "SelectWord",
            WordPreview => "WordPreview",
            DictionaryResult => "DictionaryResult",
            DictionaryChooser => "DictionaryChooser",
            WordList => "WordList",
            Links => "Links",
            LinkPreview => "LinkPreview",
            Typography => "Typography",
            Settings => "Settings",
            Remote => "Remote",
            Light => "Light",
            Power => "Power",
            Refresh => "Refresh",
            Library => "Library",
            Books => "Books",
            Delete => "Delete",
            DeleteConfirm(_) => "DeleteConfirm",
            Recent => "Recent",
            Folders => "Folders",
            Folder(_) => "Folder",
            Search => "Search",
            Results => "Results",
            Errors => "Errors",
            Diagnostic(_) => "Diagnostic",
        },
    }
    .into()
}

/// Page starts of a whole book in reading order, from a clean walk.
fn walk(book: &Path, state: &ReaderState) -> Option<std::sync::Arc<Vec<Locator>>> {
    static CACHE: Mutex<Option<HashMap<String, std::sync::Arc<Vec<Locator>>>>> = Mutex::new(None);
    let key = format!("{book:?} {:?}", state.layout);
    if let Some(found) = CACHE.lock().unwrap().get_or_insert_default().get(&key) {
        return Some(found.clone());
    }
    let fresh = ReaderState {
        layout: state.layout,
        text_raster: state.text_raster,
        rotation: state.rotation,
        ..ReaderState::default()
    };
    let mut reader = Reader::new(Document::open(book).ok()?, fonts(), fresh).ok()?;
    if reader.state.layout != state.layout {
        return None;
    }
    let mut starts = vec![reader.page().start];
    while reader.dispatch(Action::Next).ok()? == Effect::Redraw {
        starts.push(reader.page().start);
        if starts.len() > 5000 {
            return None;
        }
    }
    let starts = std::sync::Arc::new(starts);
    CACHE
        .lock()
        .unwrap()
        .get_or_insert_default()
        .insert(key, starts.clone());
    Some(starts)
}

struct Harness {
    reader: Reader,
    ui: ReaderUi,
    config: Config,
    checks: Checks,
    stats: Stats,
    book: PathBuf,
    frame: Vec<u8>,
    /// The Light panel is showing because a gesture opened it over the page.
    shortcut: bool,
    /// A sync question is on screen and unanswered.
    asked: bool,
    answers: usize,
    kosync_pending: bool,
    catalog_pending: bool,
    device_toggle: bool,
    library_size: usize,
    dir: tempfile::TempDir,
    clock: u64,
}

fn device_rows(toggle: bool, count: usize) -> Vec<DeviceRow> {
    [
        DeviceRow {
            label: "Wi-Fi".into(),
            kind: DeviceRowKind::Toggle(toggle),
        },
        DeviceRow {
            label: "Network name".into(),
            kind: DeviceRowKind::Text {
                value: "home".into(),
                secret: false,
                limit: 8,
            },
        },
        DeviceRow {
            label: "Password".into(),
            kind: DeviceRowKind::Text {
                value: "secret".into(),
                secret: true,
                limit: 4,
            },
        },
        DeviceRow {
            label: "Address".into(),
            kind: DeviceRowKind::Info,
        },
        DeviceRow {
            label: "Forget".into(),
            kind: DeviceRowKind::Action,
        },
        DeviceRow {
            label: "More".into(),
            kind: DeviceRowKind::Submenu,
        },
    ]
    .into_iter()
    .cycle()
    .take(count)
    .collect()
}

type Check = std::result::Result<(), String>;
macro_rules! ensure {
    ($cond:expr, $($arg:tt)*) => {
        if !$cond {
            return Err(format!($($arg)*));
        }
    };
}

impl Harness {
    fn new(config: Config, checks: Checks) -> Self {
        let mut state = ReaderState::default();
        if config.landscape {
            state.layout.width = 800;
            state.layout.height = 480;
            state.rotation = crate::state::Rotation::Clockwise;
        }
        if config.status_line {
            state.layout.footer = STATUS_ROWS;
        }
        if let Some([up, down, hold]) = config.gestures {
            use crate::state::GestureAction;
            state.gestures.swipe_up = GestureAction::ALL[up];
            state.gestures.swipe_down = GestureAction::ALL[down];
            state.gestures.hold = GestureAction::ALL[hold];
        }
        let book = fixture(BOOKS[0]);
        let mut reader = Reader::new(Document::open(&book).unwrap(), fonts(), state).unwrap();
        let mut ui = ReaderUi::default();
        ui.set_sleep_available(config.sleep);
        ui.set_library(library(3));
        ui.set_battery(Some(71));
        ui.set_refresh_available(true);
        ui.set_gestures_available(true);
        ui.set_page_buttons_available(true);
        ui.set_rotation_available(true);
        ui.set_firmware("1.2.3");
        if config.services {
            ui.set_kosync_available(true);
            ui.set_catalog_available(true);
            ui.set_device_available(true);
            ui.set_tailnet_available(true);
            ui.set_update(Some(UpdateView {
                status: "Version 2 is available".into(),
                step: UpdateStep::Action("Download".into()),
            }));
            ui.set_font_families(vec!["Serif".into(), "Sans".into()]);
        }
        let frame = ui.render_view(&mut reader, 2).unwrap();
        Self {
            reader,
            ui,
            config,
            checks,
            stats: Stats::default(),
            book,
            frame,
            shortcut: false,
            asked: false,
            answers: 0,
            kosync_pending: false,
            catalog_pending: false,
            device_toggle: false,
            library_size: 3,
            dir: tempfile::tempdir().unwrap(),
            clock: 1,
        }
    }

    fn poll(&mut self) -> Check {
        for kind in 0..4 {
            let before = (self.ui.panel, self.reader.position());
            let (was, effect) = match kind {
                0 => (self.ui.is_navigating(), self.ui.poll_navigation(&mut self.reader)),
                1 => (self.ui.is_reflowing(), self.ui.poll_reflow(&mut self.reader)),
                2 => (self.ui.is_opening(), self.ui.poll_open(&mut self.reader)),
                _ => (
                    self.ui.is_preview_opening(),
                    self.ui.poll_preview(&mut self.reader),
                ),
            };
            let still = match kind {
                0 => self.ui.is_navigating(),
                1 => self.ui.is_reflowing(),
                2 => self.ui.is_opening(),
                _ => self.ui.is_preview_opening(),
            };
            if was && !still {
                match kind {
                    0 => self.stats.navigations += 1,
                    1 => self.stats.reflows += 1,
                    2 => self.stats.opens += 1,
                    _ => self.stats.previews += 1,
                }
            }
            if !was {
                ensure!(
                    effect == Effect::Unchanged
                        && before == (self.ui.panel, self.reader.position()),
                    "idle poll {kind} changed the view"
                );
            }
            if effect == Effect::Unchanged {
                ensure!(
                    before == (self.ui.panel, self.reader.position()),
                    "poll {kind} reported no change but moved"
                );
            }
        }
        Ok(())
    }
    fn drain(&mut self) -> Check {
        for _ in 0..20_000 {
            if !(self.ui.is_navigating()
                || self.ui.is_reflowing()
                || self.ui.is_opening()
                || self.ui.is_preview_opening())
            {
                return Ok(());
            }
            self.poll()?;
        }
        Err("pending work never finished".into())
    }

    /// What the device loop does after an input event.
    fn frontend(&mut self) -> Check {
        if let Some(rotation) = self.ui.take_rotation_request() {
            if let Err(error) = self.ui.rotate(&self.reader, rotation) {
                self.ui.show_error(error.to_string());
            }
        }
        if let Some(event) = self.ui.take_kosync_event() {
            match event {
                KosyncEvent::Open => self.ui.set_kosync_view("Ready", false),
                KosyncEvent::Back => {
                    self.kosync_pending = false;
                    self.ui.close_kosync();
                }
                KosyncEvent::TakeRemote | KosyncEvent::KeepLocal => {
                    ensure!(self.asked, "answer to a question that was not asked");
                    self.asked = false;
                    self.answers += 1;
                }
                _ => {
                    self.kosync_pending = true;
                    self.ui.set_kosync_view("Connecting", true);
                }
            }
        }
        if let Some(event) = self.ui.take_catalog_event() {
            match event {
                CatalogEvent::Open => {
                    let rows: Vec<String> = (0..9).map(|i| format!("Feed {i}")).collect();
                    self.ui.set_catalog_view("Catalog", &rows).unwrap();
                }
                CatalogEvent::Back => {
                    if self.catalog_pending {
                        self.catalog_pending = false;
                        self.ui.set_catalog_busy(false);
                    } else {
                        self.ui.close_catalog();
                    }
                }
                CatalogEvent::Activate(_) | CatalogEvent::Search => {
                    self.catalog_pending = true;
                    self.ui.set_catalog_busy(true);
                }
            }
        }
        if let Some(event) = self.ui.take_device_event() {
            match event {
                DeviceEvent::Open => self
                    .ui
                    .set_device_view("Network", device_rows(self.device_toggle, 6))
                    .unwrap(),
                DeviceEvent::Back => self.ui.close_device(),
                DeviceEvent::Activate(_) | DeviceEvent::Text(..) => {
                    self.device_toggle = !self.device_toggle;
                    self.ui
                        .set_device_view("Network", device_rows(self.device_toggle, 6))
                        .unwrap();
                }
            }
        }
        if let Some(TailnetEvent::Open) = self.ui.take_tailnet_event() {
            self.ui.open_tailnet();
        }
        self.ui.take_font_request();
        self.ui.take_sleep_request();
        self.ui.take_light_request();
        self.ui.take_update_request();
        Ok(())
    }

    fn busy(&self) -> bool {
        self.ui.is_busy()
    }

    /// The selection the interface acts on and draws: the stored one, kept
    /// within the entries there are now.
    fn selected(&self) -> usize {
        self.ui.menu(&self.reader).1
    }

    fn step(&mut self, op: &Op) -> Check {
        let panel = self.ui.panel;
        let busy = self.busy();
        let editing = self.ui.is_editing();
        let position = self.reader.position();
        let page = self.reader.page_index();
        let book_id = self.reader.document.metadata.id.clone();
        let (entries, selected) = self.ui.menu(&self.reader);
        let count = entries.len();
        let bookmarks = |reader: &Reader| -> Vec<Locator> {
            reader
                .state
                .books
                .get(&reader.document.metadata.id)
                .map(|book| book.bookmarks.iter().map(|b| b.location).collect())
                .unwrap_or_default()
        };
        let marks = bookmarks(&self.reader);
        let light = self.reader.state.frontlight;
        let mut effect = None;
        let mut input = true;
        match op {
            Op::Turn(next) => {
                let pages = self.ui.turns_pages();
                effect = Some(self.ui.turn(&mut self.reader, *next));
                if busy {
                    self.stats.busy_inputs += 1;
                } else if panel.is_none() {
                    self.stats.page_turns += 1;
                    if self.busy() {
                        self.frame.clear();
                    }
                    self.drain()?;
                    self.check_turn(position, *next)?;
                } else if !pages {
                    let expected = if *next {
                        (selected + 1).min(count.saturating_sub(1))
                    } else {
                        selected.saturating_sub(1)
                    };
                    ensure!(
                        self.selected() == expected && self.ui.panel == panel,
                        "turn in {} moved {selected} to {} of {count}",
                        name(panel),
                        self.selected()
                    );
                    ensure!(
                        self.reader.position() == position,
                        "a menu turn moved the book"
                    );
                }
            }
            Op::Swipe(next) => {
                let pages = self.ui.turns_pages();
                effect = Some(self.ui.swipe(&mut self.reader, *next));
                if busy {
                    self.stats.busy_inputs += 1;
                } else if panel.is_none() {
                    self.stats.page_turns += 1;
                    if self.busy() {
                        self.frame.clear();
                    }
                    self.drain()?;
                    self.check_turn(position, *next)?;
                } else if !pages && !self.ui.is_editing() {
                    let items = self.ui.menu_items(&self.reader);
                    let (at, last) = (selected / items, count.saturating_sub(1) / items);
                    let target = if *next {
                        (at + 1).min(last)
                    } else {
                        at.saturating_sub(1)
                    };
                    let expected = if target == at { selected } else { target * items };
                    ensure!(
                        self.selected() == expected && self.ui.panel == panel,
                        "swipe in {} moved {selected} to {} of {count}",
                        name(panel),
                        self.selected()
                    );
                    ensure!(
                        self.reader.position() == position,
                        "a menu swipe moved the book"
                    );
                }
            }
            Op::Select => effect = Some(self.ui.select(&mut self.reader)),
            Op::TurnLazy(next) => effect = Some(self.ui.turn(&mut self.reader, *next)),
            Op::Choose(index) => {
                if busy || panel.is_none() || self.ui.turns_pages() || count == 0 {
                    effect = Some(self.ui.select(&mut self.reader));
                } else {
                    let target = usize::from(*index) % count;
                    for _ in 0..count {
                        if self.selected() == target {
                            break;
                        }
                        let next = self.selected() < target;
                        self.ui.turn(&mut self.reader, next).map_err(|e| e.to_string())?;
                    }
                    let label = entries[target].0.clone();
                    let slider = match entries[target].1 {
                        Command::Step(setting) => Some((setting, setting.position(&self.reader.state))),
                        _ => None,
                    };
                    effect = Some(self.ui.select(&mut self.reader));
                    if let (Some((setting, before)), true) = (slider, self.selected() == target) {
                        self.stats.slider_steps += 1;
                        if matches!(
                            setting,
                            Setting::Brightness | Setting::Warmth | Setting::Stroke | Setting::Coverage
                        ) {
                            let after = setting.position(&self.reader.state);
                            ensure!(
                                after == (before + 1) % setting.positions(),
                                "{label}: stepped from {before} to {after}"
                            );
                        }
                    }
                    // The frame before no longer matches the moved selection.
                    self.frame.clear();
                }
            }
            Op::Back => {
                effect = Some(self.ui.back(&mut self.reader));
                self.frontend()?;
                if !busy && panel.is_some() {
                    ensure!(
                        self.reader.position() == position,
                        "Back in a menu moved the book"
                    );
                    if self.checks.depth && !matches!(panel, Some(Panel::LinkPreview)) {
                        let expected = if self.shortcut && matches!(panel, Some(Panel::Light)) {
                            0
                        } else {
                            depth(panel) - 1
                        };
                        ensure!(
                            depth(self.ui.panel) == expected,
                            "Back from {} (depth {}) gave {}",
                            name(panel),
                            depth(panel),
                            name(self.ui.panel)
                        );
                    }
                }
            }
            Op::Power => {
                let label = entries.get(selected).map(|(label, _)| label.clone());
                effect = Some(self.ui.power(&mut self.reader));
                let requested = self.ui.take_sleep_request();
                let expected = if panel.is_none() {
                    self.config.sleep
                } else {
                    !busy && !self.ui.turns_pages() && label.as_deref() == Some("Save and sleep")
                        && panel == Some(Panel::Root)
                };
                ensure!(
                    requested == expected,
                    "power in {}: sleep requested {requested}, expected {expected}",
                    name(panel)
                );
                if requested {
                    self.stats.sleeps += 1;
                    ensure!(
                        self.reader.position() == position,
                        "sleeping moved the book"
                    );
                    // The sleep page is the same whatever menu was open.
                    let shown = self
                        .ui
                        .render_sleep(&mut self.reader, 2)
                        .map_err(|e| format!("sleep page: {e}"))?;
                    let plain = ReaderUi::default()
                        .render_sleep(&mut self.reader, 2)
                        .map_err(|e| format!("sleep page: {e}"))?;
                    ensure!(shown == plain, "sleep page depends on the menu");
                    if !busy {
                        self.reload()?;
                    }
                }
            }
            Op::Tap(x, y) => {
                let layout = self.reader.state.layout;
                let x = u32::from(*x) * layout.width / 1000;
                let y = u32::from(*y) * layout.height / 1000;
                effect = Some(self.ui.tap(&mut self.reader, x, y));
            }
            Op::Gesture(which) => {
                let gesture = [Gesture::SwipeUp, Gesture::SwipeDown, Gesture::Hold]
                    [usize::from(*which) % 3];
                // Repeated, so that stepped levels reach their ends.
                for _ in 0..usize::from(*which) / 3 {
                    let _ = self.ui.gesture(&mut self.reader, gesture);
                }
                self.frame.clear();
                effect = Some(self.ui.gesture(&mut self.reader, gesture));
                if panel.is_none() && matches!(self.ui.panel, Some(Panel::Light)) {
                    self.shortcut = true;
                }
                if panel.is_some() {
                    ensure!(
                        self.ui.panel == panel
                            && self.selected() == selected
                            && self.reader.position() == position
                            && self.reader.state.frontlight == light,
                        "a gesture acted inside a menu"
                    );
                }
            }
            Op::Type(character) => {
                let before = self.ui.query().to_owned();
                let limit = self.ui.text_limit();
                effect = Some(Ok(self.ui.type_char(*character)));
                ensure!(
                    self.ui.query().len() <= limit.max(before.len()),
                    "typed past the limit"
                );
            }
            Op::Submit => effect = Some(Ok(self.ui.submit_search())),
            Op::Poll(times) => {
                input = false;
                for _ in 0..*times {
                    self.poll()?;
                }
            }
            Op::Drain => {
                input = false;
                self.drain()?;
            }
            Op::Home => {
                for _ in 0..12 {
                    if !self.ui.is_open() && !self.busy() {
                        break;
                    }
                    self.ui.back(&mut self.reader).map_err(|e| e.to_string())?;
                    self.frontend()?;
                }
                ensure!(
                    !self.ui.is_open(),
                    "twelve Backs from {} left {}",
                    name(panel),
                    name(self.ui.panel)
                );
                self.frame.clear();
            }
            Op::SaveReload => {
                input = false;
                if !busy {
                    self.reload()?;
                }
            }
            Op::AskConflict(remote, local) => {
                input = false;
                let asks = !self.ui.is_open() && !busy;
                ensure!(
                    self.ui.ask_sync_conflict(*remote, *local) == asks,
                    "the sync question was {} in {}",
                    if asks { "refused" } else { "asked" },
                    name(panel)
                );
                if asks {
                    self.asked = true;
                    self.stats.conflicts += 1;
                }
            }
            Op::KosyncDone => {
                input = false;
                if std::mem::take(&mut self.kosync_pending) && self.ui.is_kosync_open() {
                    self.ui.set_kosync_view("Ready", false);
                }
            }
            Op::CatalogDone(rows) => {
                input = false;
                if std::mem::take(&mut self.catalog_pending) {
                    self.ui.set_catalog_busy(false);
                    if self.ui.is_catalog_open() {
                        let rows: Vec<String> = (0..*rows).map(|i| format!("Entry {i}")).collect();
                        self.ui.set_catalog_view("Results", &rows).unwrap();
                    }
                }
            }
            Op::CatalogRows(rows) => {
                input = false;
                if self.ui.catalog_position().is_some() {
                    // The list only grows while it is read.
                    let rows = if self.checks.shrinking_rows {
                        usize::from(*rows)
                    } else {
                        usize::from(*rows).max(self.ui.catalog_rows.len())
                    };
                    let rows: Vec<String> = (0..rows).map(|i| format!("Entry {i}")).collect();
                    self.ui.set_catalog_rows(&rows).unwrap();
                }
            }
            Op::TailnetStatus(rows) => {
                input = false;
                let rows = if self.checks.shrinking_rows {
                    usize::from(*rows)
                } else {
                    usize::from(*rows).max(self.ui.tailnet_rows.len())
                };
                let rows: Vec<String> = (0..rows).map(|i| format!("peer {i}")).collect();
                self.ui.set_tailnet_status(&rows);
            }
            Op::SetUpdate(kind) => {
                input = false;
                if self.config.services {
                    self.ui.set_update(Some(UpdateView {
                        status: format!("Status {kind}"),
                        step: if kind % 2 == 0 {
                            UpdateStep::Action("Install".into())
                        } else {
                            UpdateStep::Working {
                                label: "Downloading".into(),
                                percent: Some(*kind),
                            }
                        },
                    }));
                }
            }
            Op::DeviceStatus => {
                input = false;
                self.ui.set_device_status("82% | home");
            }
            Op::SetLibrary(size) => {
                input = false;
                // The frontend publishes a library only when no scan runs.
                self.library_size = usize::from(*size) % 4;
                self.ui.set_library(library(self.library_size));
            }
            Op::DeviceRefresh(rows) => {
                input = false;
                if matches!(self.ui.panel, Some(Panel::Device)) {
                    self.ui
                        .set_device_view("Network", device_rows(self.device_toggle, usize::from(*rows) % 8))
                        .unwrap();
                }
            }
        }
        let effect = match effect {
            Some(Err(error)) => {
                // The device loop shows input errors in the menu.
                self.ui.open();
                self.ui.show_error(error.to_string());
                Some(Effect::Redraw)
            }
            Some(Ok(effect)) => Some(effect),
            None => None,
        };
        if input && !matches!(op, Op::Back | Op::Home) {
            self.frontend()?;
        }
        if input && busy && !matches!(op, Op::Back | Op::Home | Op::Power | Op::Type(_) | Op::Submit)
        {
            ensure!(
                effect == Some(Effect::Unchanged)
                    && self.ui.panel == panel
                    && self.selected() == selected
                    && self.reader.position() == position,
                "input acted while work was pending"
            );
        }
        if !matches!(self.ui.panel, Some(Panel::Light)) {
            self.shortcut = false;
        }
        if self.asked && !matches!(self.ui.panel, Some(Panel::SyncConflict)) {
            // Back declines to answer; anything else lost the question.
            let declined = matches!(op, Op::Back | Op::Home)
                || matches!(op, Op::Tap(..)) && !busy;
            ensure!(
                !self.checks.conflict_answered || declined,
                "the sync question vanished from {} without an answer",
                name(self.ui.panel)
            );
            self.asked = false;
        }

        // Bookmarks change only by toggling the place that was showing.
        if self.reader.document.metadata.id == book_id {
            let now = bookmarks(&self.reader);
            if now != marks {
                self.stats.bookmarks += 1;
                let mut toggled = marks.clone();
                match toggled.iter().position(|mark| *mark == position) {
                    Some(index) => {
                        toggled.remove(index);
                    }
                    None => toggled.push(position),
                }
                let (mut a, mut b) = (now.clone(), toggled);
                a.sort();
                b.sort();
                ensure!(a == b, "bookmarks {marks:?} became {now:?} at {position:?}");
            }
        } else {
            self.book = self
                .ui
                .book_path(&self.reader.document.metadata.id)
                .or_else(|| {
                    library(3)
                        .entries
                        .into_iter()
                        .find(|e| e.metadata.id == self.reader.document.metadata.id)
                        .map(|e| e.path)
                })
                .ok_or("opened a book that is not in the library")?;
        }

        // Settings stay on their steps and in range.
        let state = &self.reader.state;
        ensure!(
            state.frontlight.brightness <= 10 && state.frontlight.warmth <= 9,
            "frontlight out of range: {:?}",
            state.frontlight
        );
        ensure!(
            TEXT_SIZES.contains(&state.layout.font_size)
                && LINE_SPACINGS.contains(&state.layout.line_height)
                && MARGINS.contains(&(state.layout.margin as f32))
                && STROKES.contains(&state.text_raster.stroke)
                && COVERAGES.contains(&state.text_raster.coverage_gamma)
                && crate::state::FULL_REFRESH_INTERVALS.contains(&state.full_refresh_interval)
                && [0, STATUS_ROWS].contains(&state.layout.footer),
            "a setting left its steps: {:?} {:?}",
            state.layout,
            state.text_raster
        );
        ensure!(state.layout.validate().is_ok(), "layout became invalid");

        // The view.
        self.stats.panels.insert(name(self.ui.panel));
        if self.ui.error.is_some() {
            self.stats.errors_shown += 1;
        }
        if depth(self.ui.panel) != usize::MAX {
            self.stats.max_depth = self.stats.max_depth.max(depth(self.ui.panel));
        }
        if matches!(self.ui.panel, Some(Panel::LinkPreview)) {
            ensure!(self.ui.preview.is_some(), "preview panel without a preview");
        }
        let frame = self
            .ui
            .render_view(&mut self.reader, 2)
            .map_err(|e| format!("render in {}: {e}", name(self.ui.panel)))?;
        ensure!(
            frame.len() == (self.reader.state.layout.width * self.reader.state.layout.height) as usize,
            "frame size"
        );
        if self.checks.honest_effect
            && effect == Some(Effect::Unchanged)
            && !self.frame.is_empty()
            && !matches!(op, Op::Power)
            && (self.checks.keyboard_taps || !(matches!(op, Op::Tap(..)) && editing))
        {
            ensure!(
                frame == self.frame,
                "{op:?} in {} reported no change but the view differs (now {})",
                name(panel),
                name(self.ui.panel)
            );
        }
        self.frame = frame;
        let _ = page;
        Ok(())
    }

    /// A turn on the page lands where a clean walk of the book says.
    fn check_turn(&mut self, before: Locator, next: bool) -> Check {
        if !self.checks.pages || self.ui.is_open() || self.busy() {
            return Ok(());
        }
        let Some(starts) = walk(&self.book, &self.reader.state) else {
            return Ok(());
        };
        // The page that was showing is the last one starting at or before
        // the anchor.
        let Some(index) = starts.iter().rposition(|start| *start <= before) else {
            return Ok(());
        };
        if before.chapter != starts[index].chapter {
            return Ok(());
        }
        let expected = if next {
            (index + 1).min(starts.len() - 1)
        } else {
            index.saturating_sub(1)
        };
        self.stats.page_checks += 1;
        if starts[expected].chapter != starts[index].chapter {
            self.stats.chapter_crossings += 1;
        }
        let shown = self.reader.page().start;
        ensure!(
            shown == starts[expected],
            "turn {} from {before:?} (page {index} of {}) showed {shown:?}, a clean walk gives {:?}",
            if next { "forward" } else { "back" },
            starts.len(),
            starts[expected]
        );
        if expected != index {
            ensure!(
                self.reader.position() == shown,
                "a turn left the anchor at {:?} on the page starting {shown:?}",
                self.reader.position()
            );
        }
        Ok(())
    }

    /// Saved state decodes and reopens at the same place.
    fn reload(&mut self) -> Check {
        self.stats.reloads += 1;
        if let Some(entry) = library(3)
            .entries
            .into_iter()
            .find(|e| e.metadata.id == self.reader.document.metadata.id)
        {
            self.book = entry.path;
        }
        self.clock += 1;
        self.reader.record_position(self.clock);
        let path = self.dir.path().join("reader.json");
        self.reader.state.save(&path).map_err(|e| format!("save: {e}"))?;
        let state = ReaderState::load(&path).map_err(|e| format!("load: {e}"))?;
        let reopened = Reader::new(
            Document::open(&self.book).map_err(|e| e.to_string())?,
            fonts(),
            state,
        )
        .map_err(|e| format!("reopen: {e}"))?;
        ensure!(
            reopened.position() == self.reader.position()
                && reopened.page().start == self.reader.page().start
                && reopened.page_index() == self.reader.page_index(),
            "reopened at {:?} page {} instead of {:?} page {}",
            reopened.position(),
            reopened.page_index(),
            self.reader.position(),
            self.reader.page_index()
        );
        ensure!(
            reopened.state.layout == self.reader.state.layout,
            "reopened with another layout"
        );
        Ok(())
    }
}

pub(super) fn run(config: Config, checks: Checks, ops: &[Op]) -> std::result::Result<Stats, String> {
    let mut harness = Harness::new(config, checks);
    for (index, op) in ops.iter().enumerate() {
        harness
            .step(op)
            .map_err(|error| format!("step {index} {op:?}: {error}"))?;
    }
    harness.drain().map_err(|e| format!("final drain: {e}"))?;
    harness.reload().map_err(|e| format!("final reload: {e}"))?;
    Ok(harness.stats)
}

fn op(weights: [u32; 6]) -> impl Strategy<Value = Op> {
    let [page, menu, deep, text, service, work] = weights;
    let choices: Vec<(u32, BoxedStrategy<Op>)> = vec![
        (page * 4, any::<bool>().prop_map(Op::Turn).boxed()),
        (page * 4, prop::bool::weighted(0.7).prop_map(Op::Turn).boxed()),
        (page, any::<bool>().prop_map(Op::Swipe).boxed()),
        (page * 2, prop::bool::weighted(0.8).prop_map(Op::TurnLazy).boxed()),
        (page * 3, (0..36u8).prop_map(Op::Gesture).boxed()),
        (page, Just(Op::Home).boxed()),
        (menu * 2, any::<bool>().prop_map(Op::Turn).boxed()),
        (menu * 2, any::<bool>().prop_map(Op::Swipe).boxed()),
        (menu * 2, Just(Op::Select).boxed()),
        (menu * 2, Just(Op::Back).boxed()),
        (menu + page, Just(Op::Power).boxed()),
        (menu * 2, (0..1000u16, 0..1000u16).prop_map(|(x, y)| Op::Tap(x, y)).boxed()),
        (deep * 6, (0..24u8).prop_map(Op::Choose).boxed()),
        (deep, (0..100u8).prop_map(Op::Choose).boxed()),
        (
            text,
            prop::sample::select(vec!['a', 'Z', '9', ' ', 'é', '\u{301}', '語', '\n'])
                .prop_map(Op::Type)
                .boxed(),
        ),
        (text, Just(Op::Submit).boxed()),
        (text * 2, (36..40u8).prop_map(Op::Choose).boxed()),
        (
            service,
            (0..=120u8, prop::option::of(0..=100u8))
                .prop_map(|(r, l)| Op::AskConflict(r, l))
                .boxed(),
        ),
        (service, Just(Op::KosyncDone).boxed()),
        (service, (0..20u8).prop_map(Op::CatalogDone).boxed()),
        (service, (0..20u8).prop_map(Op::CatalogRows).boxed()),
        (service, (0..12u8).prop_map(Op::TailnetStatus).boxed()),
        (service, (0..100u8).prop_map(Op::SetUpdate).boxed()),
        (service, Just(Op::DeviceStatus).boxed()),
        (service, (0..4u8).prop_map(Op::SetLibrary).boxed()),
        (service, (0..8u8).prop_map(Op::DeviceRefresh).boxed()),
        (work * 2, (0..4u8).prop_map(Op::Poll).boxed()),
        (work * 3 + 1, Just(Op::Drain).boxed()),
        (1, Just(Op::SaveReload).boxed()),
    ];
    proptest::strategy::Union::new_weighted(
        choices.into_iter().filter(|(weight, _)| *weight > 0).collect(),
    )
}

fn config() -> impl Strategy<Value = Config> {
    (
        any::<bool>(),
        prop::bool::weighted(0.8),
        prop::bool::weighted(0.25),
        any::<bool>(),
        prop::option::weighted(0.5, [0..8usize, 0..8usize, 0..8usize]),
    )
        .prop_map(|(sleep, services, landscape, status_line, gestures)| Config {
            sleep,
            services,
            landscape,
            status_line,
            gestures,
        })
}

/// Swarm testing: each case draws its own mix of operation families.
pub(super) fn case(length: usize) -> impl Strategy<Value = (Config, Vec<Op>)> {
    let weight = || prop::sample::select(vec![0u32, 0, 1, 1, 3, 8]);
    (
        config(),
        [weight(), weight(), weight(), weight(), weight(), weight()],
    )
        .prop_flat_map(move |(config, mut weights)| {
            if weights[..3].iter().all(|w| *w == 0) {
                weights[2] = 3;
            }
            (Just(config), prop::collection::vec(op(weights), 1..length))
        })
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(48)
}

/// Checks that are expected to hold on unmodified code. The ones named in
/// `FEMTO_PROP_STRICT` (comma separated) are switched on besides.
fn checks() -> Checks {
    let strict = std::env::var("FEMTO_PROP_STRICT").unwrap_or_default();
    let on = |name: &str| strict.split(',').any(|s| s == name || s == "all");
    Checks {
        honest_effect: on("effect"),
        depth: on("depth"),
        conflict_answered: on("conflict"),
        pages: true,
        shrinking_rows: true,
        keyboard_taps: true,
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), max_shrink_iters: 400, ..ProptestConfig::default() })]
    #[test]
    fn random_histories_keep_the_interface_consistent((config, ops) in case(60)) {
        if let Err(error) = run(config, checks(), &ops) {
            prop_assert!(false, "{error}");
        }
    }
}

/// Prints what the generated histories reach.
#[test]
#[ignore = "coverage report, run by hand"]
fn coverage_of_generated_histories() {
    use proptest::{strategy::ValueTree, test_runner::TestRunner};
    let mut runner = TestRunner::deterministic();
    let mut total = Stats::default();
    let mut failures = std::collections::BTreeMap::<String, usize>::new();
    let relaxed = Checks {
        honest_effect: false,
        depth: false,
        conflict_answered: false,
        pages: true,
        shrinking_rows: false,
        keyboard_taps: false,
    };
    for _ in 0..cases() {
        let (config, ops) = case(60).new_tree(&mut runner).unwrap().current();
        let shown = format!("{config:?}\n  {ops:?}");
        match run(config, relaxed, &ops) {
            Ok(stats) => {
                total.panels.extend(stats.panels);
                total.max_depth = total.max_depth.max(stats.max_depth);
                total.page_turns += stats.page_turns;
                total.page_checks += stats.page_checks;
                total.chapter_crossings += stats.chapter_crossings;
                total.reflows += stats.reflows;
                total.opens += stats.opens;
                total.navigations += stats.navigations;
                total.previews += stats.previews;
                total.sleeps += stats.sleeps;
                total.reloads += stats.reloads;
                total.conflicts += stats.conflicts;
                total.slider_steps += stats.slider_steps;
                total.bookmarks += stats.bookmarks;
                total.busy_inputs += stats.busy_inputs;
                total.errors_shown += stats.errors_shown;
            }
            Err(error) => {
                if std::env::var("FEMTO_PROP_VERBOSE").is_ok() {
                    println!("FAIL {error}\n  {shown}");
                }
                let key = error.split(": ").nth(1).unwrap_or(&error).chars().take(60).collect();
                *failures.entry(key).or_default() += 1;
            }
        }
    }
    println!("{total:#?}\nfailures: {failures:#?}");
}
