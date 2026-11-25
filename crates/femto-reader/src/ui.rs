//! Fixed-row reader menus shared with device frontends.
use crate::{
    Result,
    controller::{
        Action, BookOpening, Effect, LinkPreview, Navigating, PreviewOpening, Reader, Reflowing,
    },
    state::{Library, LibraryScan},
};
use std::{
    collections::{BTreeSet, HashSet},
    path::PathBuf,
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Panel {
    Root,
    /// Tools for the open book that are used less often than the root rows.
    Book,
    Fonts,
    Gestures,
    Rotation,
    Catalog,
    CatalogSearch,
    /// A list of rows supplied by the device frontend.
    Device,
    /// Editing the text of one device row.
    DeviceText(usize),
    Kosync,
    Tailnet,
    Update,
    Contents,
    Chapters,
    GoTo,
    /// Asks which position to keep when two devices have both moved.
    SyncConflict,
    Bookmarks,
    Clippings,
    Clipping(usize),
    ClippingDelete,
    SelectWord,
    WordPreview,
    DictionaryResult,
    DictionaryChooser,
    WordList,
    Links,
    LinkPreview,
    Typography,
    /// Device preferences and connections.
    Settings,
    Remote,
    Light,
    Power,
    Refresh,
    Library,
    Books,
    /// The books, listed to choose one for deletion.
    Delete,
    /// Confirming deletion of the library entry at this index.
    DeleteConfirm(usize),
    Recent,
    Folders,
    Folder(usize),
    Search,
    Results,
    Errors,
    Diagnostic(usize),
}
impl Panel {
    /// The panel Back returns to; `None` leaves the menu.
    fn parent(self) -> Option<Self> {
        Some(match self {
            Self::Root | Self::SyncConflict => return None,
            Self::Book
            | Self::Contents
            | Self::Bookmarks
            | Self::Library
            | Self::Catalog
            | Self::Light
            | Self::Typography
            | Self::Settings
            | Self::LinkPreview => Self::Root,
            Self::Chapters | Self::GoTo | Self::Links | Self::Clippings | Self::Kosync => {
                Self::Book
            }
            Self::Device
            | Self::Tailnet
            | Self::Update
            | Self::Power
            | Self::Rotation
            | Self::Refresh
            | Self::DictionaryChooser
            | Self::Remote => Self::Settings,
            Self::DeviceText(_) => Self::Device,
            Self::Fonts => Self::Typography,
            Self::Gestures => Self::Settings,
            Self::CatalogSearch => Self::Catalog,
            Self::Books | Self::Recent | Self::Folders | Self::Search | Self::Errors => {
                Self::Library
            }
            Self::Delete => Self::Library,
            Self::DeleteConfirm(_) => Self::Delete,
            Self::Results => Self::Search,
            Self::Folder(_) => Self::Folders,
            Self::Diagnostic(_) => Self::Errors,
            Self::Clipping(_)
            | Self::ClippingDelete
            | Self::SelectWord
            | Self::WordPreview
            | Self::WordList => Self::Clippings,
            Self::DictionaryResult => Self::WordPreview,
        })
    }
}
#[derive(Clone, Copy)]
enum Command {
    Kosync(KosyncEvent),
    Catalog(CatalogEvent),
    DeviceOpen,
    DeviceRow(usize),
    /// Switch the keyboard between lower case, upper case and symbols.
    Layer,
    Tailnet(TailnetEvent),
    Sleep,
    Update,
    AutoUpdate,
    Font(Option<usize>),
    CycleGesture(Gesture),
    Rotation(crate::state::Rotation),
    IdleSleep(crate::state::IdleSleep),
    FullRefresh(u8),
    Resume,
    Rescan,
    Noop,
    ClipPage,
    DeleteClipping(usize),
    ToggleHighlight(usize),
    ConfirmClippingDelete,
    SelectWord,
    SaveWord,
    LookupWord,
    ChooseDictionary(usize),
    SelectWordList,
    PickWord(crate::layout::Locator, crate::layout::Locator),
    ExtendWord,
    Character(char),
    Delete,
    Find,
    Open(Panel),
    Reader(Action),
    Hyphenation,
    ResetTypography,
    RemoteCycle(u32),
    RemoteDefaults,
    RemoteDisable,
    Hinting,
    Fractional,
    Book(usize),
    DeleteBook(usize),
    ConfirmDeleteBook(usize),
    /// Return to the enclosing menu.
    Leave,
    /// Advance a stepped setting, wrapping to its first value.
    Step(Setting),
    /// Choose a value of a stepped setting by position.
    Set(Setting, usize),
    SwapPageButtons,
    StatusLine,
    SyncAutomatically,
    SyncConflict,
    SwapMenuButtons,
}

/// A touch gesture on the reading page that is not a page turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    SwipeUp,
    SwipeDown,
    Hold,
}
impl Gesture {
    fn action(self, gestures: &mut crate::state::Gestures) -> &mut crate::state::GestureAction {
        match self {
            Self::SwipeUp => &mut gestures.swipe_up,
            Self::SwipeDown => &mut gestures.swipe_down,
            Self::Hold => &mut gestures.hold,
        }
    }
}

/// Text broken at spaces into at most `limit` lines that a panel holds,
/// ending in an ellipsis when it does not all fit.
fn wrap(text: &str, limit: usize) -> Vec<String> {
    const WIDTH: usize = 38;
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        let word: String = word.graphemes(true).take(WIDTH).collect();
        let full = lines.len() == limit;
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= WIDTH => {
                line.push(' ');
                line.push_str(&word);
            }
            Some(last) if full => {
                while last.chars().count() + 2 > WIDTH {
                    last.pop();
                }
                last.push_str(" \u{2026}");
                break;
            }
            _ if full => break,
            _ => lines.push(word),
        }
    }
    lines
}

/// The book's cover reduced to fit the screen and centered on white. A cover
/// smaller than the screen keeps its size.
fn sleep_cover(reader: &mut Reader, levels: u16) -> Option<Vec<u8>> {
    let (width, height) = (reader.state.layout.width, reader.state.layout.height);
    let cover =
        crate::image::decode_within(&reader.document.cover()?, crate::image::Limits::ALONE).ok()?;
    let size = cover.size.reduced(width, height).ok()?;
    // The full-size cover is released before the page is made.
    let cover = {
        let reduced = cover.resize(size).ok()?;
        drop(cover);
        reduced
    };
    let mut pixels = vec![255; (width * height) as usize];
    let (left, top) = ((width - size.width) / 2, (height - size.height) / 2);
    for (row, source) in cover.pixels.chunks_exact(size.width as usize).enumerate() {
        let start = ((top + row as u32) * width + left) as usize;
        pixels[start..start + source.len()].copy_from_slice(source);
    }
    crate::image::quantize(&mut pixels, width, levels).ok()?;
    Some(pixels)
}

/// The places the Go to list offers are this many percent of the book apart.
const GO_TO_STEP: usize = 5;

/// Rows at the bottom of the reading page that the text leaves to the status
/// line, and the lowest of them that hold its ink. The rest keep it apart
/// from the last line of text.
const STATUS_ROWS: u32 = 32;
const STATUS_INK_ROWS: u32 = 22;

/// A setting with a short ordered list of values, drawn as a stepped slider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Setting {
    Brightness,
    Warmth,
    TextSize,
    LineSpacing,
    Margin,
    Stroke,
    Coverage,
}

const TEXT_SIZES: [f32; 12] = [
    14.0, 16.0, 18.0, 20.0, 22.0, 24.0, 26.0, 28.0, 30.0, 32.0, 36.0, 40.0,
];
const LINE_SPACINGS: [f32; 5] = [1.0, 1.15, 1.35, 1.6, 2.0];
const MARGINS: [f32; 7] = [0.0, 8.0, 16.0, 24.0, 32.0, 40.0, 48.0];
const STROKES: [f32; 3] = [0.0, 0.2, 0.4];
const COVERAGES: [f32; 3] = [1.0, 0.85, 0.7];

impl Setting {
    /// Values in slider order; frontlight levels are their own positions.
    fn values(self) -> &'static [f32] {
        match self {
            Self::Brightness => &[0.0; 11],
            Self::Warmth => &[0.0; 10],
            Self::TextSize => &TEXT_SIZES,
            Self::LineSpacing => &LINE_SPACINGS,
            Self::Margin => &MARGINS,
            Self::Stroke => &STROKES,
            Self::Coverage => &COVERAGES,
        }
    }
    fn positions(self) -> usize {
        self.values().len()
    }
    /// Position of the listed value nearest the current one.
    fn position(self, state: &crate::state::ReaderState) -> usize {
        let current = match self {
            Self::Brightness => return usize::from(state.frontlight.brightness).min(10),
            Self::Warmth => return usize::from(state.frontlight.warmth).min(9),
            Self::TextSize => state.layout.font_size,
            Self::LineSpacing => state.layout.line_height,
            Self::Margin => state.layout.margin as f32,
            Self::Stroke => state.text_raster.stroke,
            Self::Coverage => state.text_raster.coverage_gamma,
        };
        (0..self.positions())
            .min_by(|a, b| {
                let distance = |i: &usize| (self.values()[*i] - current).abs();
                distance(a).total_cmp(&distance(b))
            })
            .unwrap_or(0)
    }
    /// The layout with this setting at `position`; unchanged for settings
    /// that do not affect layout.
    fn layout(
        self,
        mut layout: crate::layout::LayoutConfig,
        position: usize,
    ) -> crate::layout::LayoutConfig {
        if let Some(&value) = self.values().get(position) {
            match self {
                Self::TextSize => layout.font_size = value,
                Self::LineSpacing => layout.line_height = value,
                Self::Margin => layout.margin = value as u32,
                _ => {}
            }
        }
        layout
    }
    fn label(self, state: &crate::state::ReaderState) -> String {
        match self {
            Self::Brightness => format!("Brightness {}", state.frontlight.brightness),
            Self::Warmth => format!("Warmth {}", state.frontlight.warmth),
            Self::TextSize => format!("Text size {:.0}", state.layout.font_size),
            Self::LineSpacing => format!("Line spacing {:.2}", state.layout.line_height),
            Self::Margin => format!("Margins {}", state.layout.margin),
            Self::Stroke => format!("Stroke weight +{:.1}", state.text_raster.stroke),
            Self::Coverage => format!("Edge darkness {}", self.position(state)),
        }
    }
}

/// One row of the device settings panel, defined by the frontend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRow {
    pub label: String,
    pub kind: DeviceRowKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceRowKind {
    /// Shown only; activating it does nothing.
    Info,
    Action,
    Submenu,
    Toggle(bool),
    /// Opens the keyboard. `value` is shown beside the label unless secret,
    /// and a secret starts from an empty field.
    Text {
        value: String,
        secret: bool,
        limit: usize,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceEvent {
    Open,
    Back,
    Activate(usize),
    /// The text entered for a [`DeviceRowKind::Text`] row.
    Text(usize, String),
}

const KEY_LAYERS: [&str; 3] = [
    "abcdefghijklmnopqrstuvwxyz0123456789",
    "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
    ".,-_@:/?!#$%&*+=()'\";<>[]{}\\|~^`äöüé",
];
const LAYER_LABELS: [&str; 3] = ["ABC", "#+=", "abc"];

/// What a menu row shows beside its label.
enum Control {
    None,
    Submenu,
    Toggle(bool),
    Slider {
        position: usize,
        positions: usize,
    },
    /// A bar filled to this percentage.
    Progress(u8),
}

/// The firmware update panel, described by the frontend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdateView {
    /// What is known, for example "Version 1.2.0 is available".
    pub status: String,
    pub step: UpdateStep,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateStep {
    /// What activating the row does next, for example "Download 7.1 MB".
    Action(String),
    /// Work in progress, which activating the row does not disturb. A known
    /// percentage is drawn as a bar.
    Working { label: String, percent: Option<u8> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogEvent {
    Open,
    Search,
    Activate(usize),
    Back,
}

fn rows_clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(256).collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KosyncEvent {
    Open,
    Push,
    Pull,
    Auth,
    Register,
    Back,
    /// Answers to the question of which position to keep.
    TakeRemote,
    KeepLocal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TailnetEvent {
    Open,
    Connect,
    Provision,
    Stop,
    Renew,
}

#[derive(Default)]
pub struct ReaderUi {
    word_origin: Option<(
        String,
        crate::layout::Locator,
        crate::layout::LayoutConfig,
        crate::state::Rotation,
    )>,
    word_pending: Option<crate::clipping::Clipping>,
    dictionary_path: Option<PathBuf>,
    dictionary_choices: Vec<PathBuf>,
    dictionary_job: Option<crate::dictionary::Dictionary>,
    dictionary_cache: Option<crate::dictionary::Dictionary>,
    dictionary_rows: Vec<String>,
    word_pan: bool,
    word_by_buttons: bool,
    word_choices: Vec<(String, crate::layout::Locator, crate::layout::Locator)>,
    clipping_delete: Option<crate::clipping::Clipping>,
    navigation: Option<Navigating>,
    navigation_return: Option<Panel>,
    preview: Option<LinkPreview>,
    preview_opening: Option<PreviewOpening>,
    preview_return: Option<Panel>,
    reflow: Option<Reflowing>,
    reflow_return: Option<Panel>,
    opening: Option<BookOpening>,
    panel: Option<Panel>,
    /// The row last activated in each panel, restored when Back returns there.
    remembered: Vec<(Panel, usize)>,
    rotation_available: bool,
    rotation_request: Option<crate::state::Rotation>,
    tailnet_available: bool,
    tailnet_event: Option<TailnetEvent>,
    tailnet_rows: Vec<String>,
    catalog_available: bool,
    catalog_event: Option<CatalogEvent>,
    catalog_title: String,
    catalog_rows: Vec<String>,
    catalog_query: String,
    catalog_busy: bool,
    device_available: bool,
    device_event: Option<DeviceEvent>,
    device_title: String,
    device_rows: Vec<DeviceRow>,
    device_text: String,
    keyboard_layer: usize,
    kosync_available: bool,
    kosync_event: Option<KosyncEvent>,
    kosync_status: String,
    kosync_busy: bool,
    /// The other device's progress and this reader's, while it is asked
    /// which to keep.
    sync_conflict: (u8, Option<u8>),
    selected: usize,
    error: Option<String>,
    device_status: Option<String>,
    battery: Option<u8>,
    library: Library,
    folders: Vec<PathBuf>,
    light_request: bool,
    sleep_available: bool,
    sleep_request: bool,
    firmware: Option<String>,
    update: Option<UpdateView>,
    update_request: bool,
    font_families: Vec<String>,
    font_request: Option<Option<String>>,
    /// The panel was opened from the page by a gesture; Back returns there.
    shortcut: bool,
    page_buttons_available: bool,
    gestures_available: bool,
    refresh_available: bool,
    query: String,
    scan_root: Option<(PathBuf, usize)>,
    scan: Option<LibraryScan>,
}

impl ReaderUi {
    pub fn set_kosync_available(&mut self, available: bool) {
        self.kosync_available = available;
    }
    pub fn take_kosync_event(&mut self) -> Option<KosyncEvent> {
        self.kosync_event.take()
    }
    pub fn is_kosync_open(&self) -> bool {
        matches!(self.panel, Some(Panel::Kosync))
    }
    pub fn kosync_status(&self) -> Option<&str> {
        self.is_kosync_open().then_some(self.kosync_status.as_str())
    }
    pub fn set_kosync_view(&mut self, status: &str, busy: bool) {
        self.kosync_status = status
            .chars()
            .filter(|c| !c.is_control())
            .take(40)
            .collect();
        self.kosync_busy = busy;
        self.panel = Some(Panel::Kosync);
        self.selected = 0;
        self.error = None;
    }
    /// Asks which of two positions to keep: the other device's, at
    /// `remote` percent of the book, or this reader's. False, and nothing is
    /// asked, over a menu the user is in or while pending work, such as a
    /// page turn into another chapter, would replace the question when it
    /// finishes.
    #[must_use]
    pub fn ask_sync_conflict(&mut self, remote: u8, local: Option<u8>) -> bool {
        if self.is_open() || self.is_busy() {
            return false;
        }
        self.sync_conflict = (remote.min(100), local);
        self.panel = Some(Panel::SyncConflict);
        self.selected = 0;
        self.error = None;
        true
    }
    pub fn set_dictionary_path(&mut self, path: Option<PathBuf>) {
        self.dictionary_choices = path
            .iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(crate::dictionary::valid_name)
            })
            .cloned()
            .collect();
        self.dictionary_path = path;
        self.release_dictionary();
    }

    pub fn configure_dictionaries(
        &mut self,
        mut paths: Vec<PathBuf>,
        preferred: Option<&str>,
    ) -> Result<()> {
        if paths.len() > crate::dictionary::MAX_DICTIONARIES {
            return Err(crate::Error::Limit("dictionary choices"));
        }
        paths.sort();
        let mut names = std::collections::BTreeSet::new();
        for path in &paths {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .filter(|name| crate::dictionary::valid_name(name))
                .ok_or_else(|| crate::Error::Document("invalid dictionary filename".into()))?;
            if !names.insert(name.to_owned()) {
                return Err(crate::Error::Document(
                    "duplicate dictionary filename".into(),
                ));
            }
        }
        let selected = paths
            .iter()
            .find(|path| path.file_name().and_then(|s| s.to_str()) == preferred)
            .or_else(|| paths.first())
            .cloned();
        self.release_dictionary();
        self.dictionary_choices = paths;
        self.dictionary_path = selected;
        Ok(())
    }

    /// Releases dictionary files and caches before storage shutdown or reconfiguration.
    pub fn release_dictionary(&mut self) {
        self.dictionary_cache = None;
        self.dictionary_job = None;
        self.dictionary_rows.clear();
        if matches!(self.panel, Some(Panel::DictionaryResult)) {
            self.panel = Some(Panel::WordPreview);
            self.selected = 0;
        }
    }

    pub fn is_dictionary_result(&self) -> bool {
        matches!(self.panel, Some(Panel::DictionaryResult))
    }

    pub fn is_dictionary_opening(&self) -> bool {
        self.dictionary_job.is_some()
    }

    pub fn poll_dictionary(&mut self, reader: &mut Reader) -> Effect {
        let Some(mut dictionary) = self.dictionary_job.take() else {
            return Effect::Unchanged;
        };
        let result = (|| -> Result<bool> {
            if !self.word_context_matches(reader) {
                return Err(crate::Error::Document("word selection expired".into()));
            }
            if !dictionary.prepare_step()? {
                return Ok(false);
            }
            let pending = self
                .word_pending
                .as_ref()
                .ok_or_else(|| crate::Error::Document("word selection expired".into()))?;
            pending.validate_document(&mut reader.document)?;
            let definitions = dictionary.lookup(&pending.text)?;
            let mut bytes = 0usize;
            let mut rows = vec![dictionary.name().chars().take(64).collect()];
            for definition in definitions {
                bytes = bytes
                    .saturating_add(definition.word.len())
                    .saturating_add(definition.text.len());
                if bytes > 16 * 1024 {
                    return Err(crate::Error::Limit("dictionary display text"));
                }
                rows.extend(excerpt_rows(&definition.word));
                rows.extend(excerpt_rows(&definition.text));
            }
            if rows.len() == 1 {
                rows.push("No definition found".into());
            }
            self.dictionary_rows = rows;
            Ok(true)
        })();
        match result {
            Ok(false) => {
                self.dictionary_job = Some(dictionary);
                Effect::Unchanged
            }
            Ok(true) => {
                self.dictionary_cache = Some(dictionary);
                self.panel = Some(Panel::DictionaryResult);
                self.selected = 0;
                Effect::Redraw
            }
            Err(error) => {
                self.panel = Some(Panel::WordPreview);
                self.selected = 0;
                self.show_error(error.to_string());
                Effect::Redraw
            }
        }
    }

    fn word_context_matches(&self, reader: &Reader) -> bool {
        self.word_origin
            .as_ref()
            .is_some_and(|(id, position, layout, rotation)| {
                id == &reader.document.metadata.id
                    && *position == reader.position()
                    && *layout == reader.state.layout
                    && *rotation == reader.state.rotation
            })
    }
    pub fn is_navigating(&self) -> bool {
        self.navigation.is_some()
    }
    pub fn navigate(&mut self, reader: &mut Reader, action: Action) -> Result<Effect> {
        if self.is_navigating()
            || self.is_opening()
            || self.is_reflowing()
            || self.is_preview_opening()
            || matches!(self.panel, Some(Panel::LinkPreview))
        {
            return Ok(Effect::Unchanged);
        }
        let result = reader.begin_navigation(action).and_then(|job| match job {
            Some(job) => Ok(self.start_navigation(job)),
            None => reader.dispatch(action),
        });
        match result {
            Ok(effect) => Ok(effect),
            Err(error) => {
                if self.panel.is_some() {
                    return Err(error);
                }
                self.open();
                self.show_error(error.to_string());
                Ok(Effect::Redraw)
            }
        }
    }
    /// The visible page or menu stays in place until the destination is
    /// indexed; Back abandons the move.
    fn start_navigation(&mut self, job: Navigating) -> Effect {
        self.navigation = Some(job);
        self.navigation_return = self.panel;
        self.error = None;
        Effect::Unchanged
    }
    fn cancel_navigation(&mut self, reader: &Reader) -> Effect {
        self.navigation = None;
        self.panel = self.navigation_return.take();
        self.selected = 0;
        if matches!(self.panel, Some(Panel::LinkPreview))
            && self
                .preview
                .as_ref()
                .is_none_or(|preview| reader.validate_preview(preview).is_err())
        {
            self.preview = None;
            self.preview_return = None;
            self.panel = Some(Panel::Root);
            self.show_error("Link preview is stale".into());
        }
        Effect::Redraw
    }
    pub fn poll_navigation(&mut self, reader: &mut Reader) -> Effect {
        let Some(mut job) = self.navigation.take() else {
            return Effect::Unchanged;
        };
        match reader.step_navigation(&mut job) {
            Ok(false) => {
                self.navigation = Some(job);
                Effect::Unchanged
            }
            result => {
                let result = result.and_then(|_| reader.finish_navigation(job));
                self.cancel_navigation(reader);
                match result {
                    Ok(_) => {
                        self.preview = None;
                        self.preview_return = None;
                        self.panel = None;
                        self.error = None;
                    }
                    Err(error) => {
                        if self.panel.is_none() {
                            self.panel = Some(Panel::Root);
                        }
                        self.show_error(error.to_string());
                    }
                }
                Effect::Redraw
            }
        }
    }
    pub fn set_rotation_available(&mut self, available: bool) {
        self.rotation_available = available;
    }
    pub fn take_rotation_request(&mut self) -> Option<crate::state::Rotation> {
        self.rotation_request.take()
    }

    pub fn book_path(&self, id: &str) -> Option<PathBuf> {
        self.library
            .entries
            .iter()
            .find(|entry| entry.metadata.id == id)
            .map(|entry| entry.path.clone())
    }

    pub fn set_tailnet_available(&mut self, available: bool) {
        self.tailnet_available = available;
    }
    pub fn take_tailnet_event(&mut self) -> Option<TailnetEvent> {
        self.tailnet_event.take()
    }
    pub fn is_tailnet_open(&self) -> bool {
        matches!(self.panel, Some(Panel::Tailnet))
    }
    pub fn open_tailnet(&mut self) {
        self.preview = None;
        self.panel = Some(Panel::Tailnet);
        self.selected = 0;
        self.error = None;
    }
    pub fn set_tailnet_status(&mut self, rows: &[String]) {
        self.tailnet_rows = rows
            .iter()
            .take(96)
            .map(|row| row.chars().filter(|c| !c.is_control()).take(32).collect())
            .collect();
    }

    pub fn set_catalog_available(&mut self, available: bool) {
        self.catalog_available = available;
    }
    pub fn take_catalog_event(&mut self) -> Option<CatalogEvent> {
        self.catalog_event.take()
    }
    pub fn is_catalog_open(&self) -> bool {
        matches!(self.panel, Some(Panel::Catalog | Panel::CatalogSearch))
    }
    pub fn open_catalog_search(&mut self) {
        self.preview = None;
        self.panel = Some(Panel::CatalogSearch);
        self.catalog_query.clear();
        self.selected = 0;
        self.error = None;
    }
    /// Offers the device settings panel in the root menu.
    pub fn set_device_available(&mut self, available: bool) {
        self.device_available = available;
    }
    pub fn take_device_event(&mut self) -> Option<DeviceEvent> {
        self.device_event.take()
    }
    pub fn is_device_open(&self) -> bool {
        matches!(self.panel, Some(Panel::Device | Panel::DeviceText(_)))
    }
    /// Shows the device settings panel with these rows. The selection is
    /// kept when the rows are refreshed in place.
    pub fn set_device_view(&mut self, title: &str, rows: Vec<DeviceRow>) -> Result<()> {
        if rows.len() > 64 {
            return Err(crate::Error::Limit("device menu rows"));
        }
        let clean = |text: &str| {
            text.chars()
                .filter(|c| !c.is_control())
                .take(256)
                .collect::<String>()
        };
        let title = clean(title);
        if !matches!(self.panel, Some(Panel::Device)) || self.device_title != title {
            self.selected = 0;
        }
        self.device_title = title;
        self.device_rows = rows
            .into_iter()
            .map(|row| DeviceRow {
                label: clean(&row.label),
                kind: row.kind,
            })
            .collect();
        self.panel = Some(Panel::Device);
        self.error = None;
        Ok(())
    }
    /// Leaves the device settings panel for the menu it was opened from.
    pub fn close_device(&mut self) {
        if self.is_device_open() {
            self.device_rows.clear();
            self.device_text.clear();
            self.panel = Some(Panel::Device);
            self.leave();
        }
    }
    /// Leaves the catalog for the menu it was opened from.
    pub fn close_catalog(&mut self) {
        if self.is_catalog_open() {
            self.panel = Some(Panel::Catalog);
            self.leave();
        }
    }
    /// Leaves the sync panel for the menu it was opened from.
    pub fn close_kosync(&mut self) {
        if self.is_kosync_open() {
            self.leave();
        }
    }
    fn remember(&mut self) {
        if let Some(panel) = self.panel {
            self.remembered.retain(|(other, _)| *other != panel);
            self.remembered.push((panel, self.selected));
        }
    }
    /// Shows the current panel's parent with the row that led here selected.
    fn leave(&mut self) {
        if std::mem::take(&mut self.shortcut) {
            self.panel = None;
            self.error = None;
            return;
        }
        self.panel = self.panel.and_then(Panel::parent);
        self.selected = self
            .remembered
            .iter()
            .find(|(panel, _)| Some(*panel) == self.panel)
            .map_or(0, |(_, selected)| *selected);
        self.error = None;
    }
    /// Activates the first entry of the current menu whose label starts
    /// with `label`, for tests and the diagnostic console.
    pub fn choose(&mut self, reader: &mut Reader, label: &str) -> Result<Effect> {
        if self.is_busy() {
            return Ok(Effect::Unchanged);
        }
        if self.panel.is_none() {
            self.open();
        }
        let entries = self.entries(reader);
        let Some(index) = entries.iter().position(|(text, _)| text.starts_with(label)) else {
            return Err(crate::Error::Document(format!("no menu entry {label:?}")));
        };
        self.selected = index;
        self.activate(reader, entries[index].1)
    }
    /// Bytes the field being edited accepts.
    fn text_limit(&self) -> usize {
        match self.panel {
            Some(Panel::DeviceText(index)) => match self.device_rows.get(index) {
                Some(DeviceRow {
                    kind: DeviceRowKind::Text { limit, .. },
                    ..
                }) => (*limit).min(256),
                _ => 0,
            },
            _ => 64,
        }
    }
    fn edit_query(&mut self) -> &mut String {
        match self.panel {
            Some(Panel::CatalogSearch) => &mut self.catalog_query,
            Some(Panel::DeviceText(_)) => &mut self.device_text,
            _ => &mut self.query,
        }
    }
    pub fn set_catalog_view(&mut self, title: &str, rows: &[String]) -> Result<()> {
        self.set_catalog_rows(rows)?;
        self.catalog_title = rows_clean(title);
        self.panel = Some(Panel::Catalog);
        self.selected = 0;
        self.error = None;
        Ok(())
    }
    /// Retitles the catalog view in place, for progress on what it waits for.
    pub fn set_catalog_title(&mut self, title: &str) {
        self.catalog_title = rows_clean(title);
    }
    /// Replaces the rows of the catalog list without moving the selection,
    /// for a list that grows while it is being read.
    pub fn set_catalog_rows(&mut self, rows: &[String]) -> Result<()> {
        if rows.len() > 2048 {
            return Err(crate::Error::Limit("catalog menu rows"));
        }
        self.catalog_rows = rows.iter().map(|row| rows_clean(row)).collect();
        Ok(())
    }
    /// The selected row of the catalog list, while that list is showing.
    pub fn catalog_position(&self) -> Option<usize> {
        matches!(self.panel, Some(Panel::Catalog))
            .then(|| self.selected.min(self.catalog_rows.len().saturating_sub(1)))
    }
    pub fn set_catalog_position(&mut self, position: usize) {
        if matches!(self.panel, Some(Panel::Catalog)) {
            self.selected = position;
        }
    }
    /// Marks a catalog request the reader is waiting on: the view stays in
    /// place under the busy mark and Back abandons the request.
    pub fn set_catalog_busy(&mut self, busy: bool) {
        self.catalog_busy = busy;
    }
    /// Entries on one page of a menu.
    pub fn menu_items(&self, reader: &Reader) -> usize {
        crate::menu::Geometry::new(reader.state.layout.width, reader.state.layout.height)
            .map_or(1, |geometry| geometry.items)
    }

    /// Offers the e-paper full-refresh interval.
    pub fn set_refresh_available(&mut self, available: bool) {
        self.refresh_available = available;
    }
    /// Offers the settings for reading gestures on a touch screen.
    pub fn set_gestures_available(&mut self, available: bool) {
        self.gestures_available = available;
    }
    /// Offers the setting that exchanges the two page buttons.
    pub fn set_page_buttons_available(&mut self, available: bool) {
        self.page_buttons_available = available;
    }
    /// The power button selects inside menus and requests sleep while
    /// reading. Without a way to sleep it opens the menu instead.
    pub fn power(&mut self, reader: &mut Reader) -> Result<Effect> {
        if self.panel.is_none() && self.sleep_available {
            self.sleep_request = true;
            return Ok(Effect::Unchanged);
        }
        self.select(reader)
    }
    /// Whether a turn moves through pages rather than a menu selection.
    pub fn turns_pages(&self) -> bool {
        matches!(
            self.panel,
            None | Some(Panel::SelectWord | Panel::LinkPreview)
        )
    }
    /// A swipe turns a reading page and moves a menu by one page of entries.
    pub fn swipe(&mut self, reader: &mut Reader, next: bool) -> Result<Effect> {
        // Input waits for pending work; Back abandons it.
        if self.is_busy() {
            return Ok(Effect::Unchanged);
        }
        if matches!(
            self.panel,
            None | Some(Panel::SelectWord | Panel::LinkPreview)
        ) || self.is_editing()
        {
            return self.turn(reader, next);
        }
        let items =
            crate::menu::Geometry::new(reader.state.layout.width, reader.state.layout.height)?
                .items;
        let (entries, selected) = self.menu(reader);
        let page = selected / items;
        let last = entries.len().saturating_sub(1) / items;
        let target = if next {
            (page + 1).min(last)
        } else {
            page.saturating_sub(1)
        };
        if target == page {
            return Ok(Effect::Unchanged);
        }
        self.selected = target * items;
        Ok(Effect::Redraw)
    }
    /// Performs the configured action of a reading gesture. Menus ignore it.
    /// Frontlight steps change no pixels, so they request the light only.
    pub fn gesture(&mut self, reader: &mut Reader, gesture: Gesture) -> Result<Effect> {
        use crate::state::GestureAction::*;
        if self.is_busy() || self.panel.is_some() {
            return Ok(Effect::Unchanged);
        }
        let action = *gesture.action(&mut reader.state.gestures);
        let light = &mut reader.state.frontlight;
        let (level, limit, up) = match action {
            Nothing => return Ok(Effect::Unchanged),
            Brighter => (&mut light.brightness, 10, true),
            Dimmer => (&mut light.brightness, 10, false),
            Warmer => (&mut light.warmth, 9, true),
            Cooler => (&mut light.warmth, 9, false),
            Frontlight => {
                self.open();
                self.panel = Some(Panel::Light);
                self.shortcut = true;
                return Ok(Effect::Redraw);
            }
            Bookmark => return self.navigate(reader, Action::ToggleBookmark),
            Menu => {
                self.open();
                return Ok(Effect::Redraw);
            }
        };
        let next = if up {
            (*level + 1).min(limit)
        } else {
            level.saturating_sub(1)
        };
        self.light_request |= next != *level;
        *level = next;
        Ok(Effect::Unchanged)
    }
    /// The image left on an e-paper panel while the device sleeps.
    /// The page shown while asleep: the book's cover, or its title, author
    /// and progress when it has no cover that can be shown.
    pub fn render_sleep(&self, reader: &mut Reader, levels: u16) -> Result<Vec<u8>> {
        if let Some(pixels) = sleep_cover(reader, levels) {
            return Ok(pixels);
        }
        let metadata = &reader.document.metadata;
        // The panel holds eight lines of forty characters: three for the
        // title, two for the author, and the three below.
        let mut lines = wrap(&metadata.title, 3);
        lines.extend(wrap(&metadata.author, 2));
        if let Some(percent) = reader.book_progress_percent() {
            lines.push(format!("~{percent}% read"));
        }
        lines.extend([
            String::new(),
            "Sleeping. Hold the power button to wake.".into(),
        ]);
        if lines.len() < 8 {
            lines.insert(0, String::new());
        }
        reader.render_panel(&lines, levels)
    }
    fn control(&self, reader: &Reader, command: &Command) -> Control {
        let state = &reader.state;
        match *command {
            Command::Step(setting) => Control::Slider {
                position: setting.position(state),
                positions: setting.positions(),
            },
            Command::SwapPageButtons => Control::Toggle(state.swap_page_buttons),
            Command::StatusLine => Control::Toggle(state.layout.footer != 0),
            Command::SyncAutomatically => Control::Toggle(state.sync_automatically),
            Command::SwapMenuButtons => Control::Toggle(state.swap_menu_buttons),
            Command::AutoUpdate => Control::Toggle(state.check_updates),
            Command::Update => match self.update.as_ref().map(|view| &view.step) {
                Some(UpdateStep::Working {
                    percent: Some(percent),
                    ..
                }) => Control::Progress(*percent),
                _ => Control::None,
            },
            Command::Hinting => Control::Toggle(state.text_raster.hinting),
            Command::Fractional => Control::Toggle(state.text_raster.fractional),
            Command::DeviceRow(index) => match self.device_rows.get(index).map(|row| &row.kind) {
                Some(DeviceRowKind::Toggle(on)) => Control::Toggle(*on),
                Some(DeviceRowKind::Submenu | DeviceRowKind::Text { .. }) => Control::Submenu,
                _ => Control::None,
            },
            Command::Open(_)
            | Command::DeviceOpen
            | Command::Kosync(KosyncEvent::Open)
            | Command::Catalog(CatalogEvent::Open)
            | Command::Tailnet(TailnetEvent::Open) => Control::Submenu,
            _ => Control::None,
        }
    }
    fn set(&mut self, reader: &mut Reader, setting: Setting, position: usize) -> Result<Effect> {
        let Some(&value) = setting.values().get(position) else {
            return Ok(Effect::Unchanged);
        };
        let layout = setting.layout(reader.state.layout, position);
        match setting {
            Setting::Brightness | Setting::Warmth => {
                let light = &mut reader.state.frontlight;
                if setting == Setting::Brightness {
                    light.brightness = position as u8;
                } else {
                    light.warmth = position as u8;
                }
                self.light_request = true;
                return Ok(Effect::Redraw);
            }
            Setting::Stroke => reader.state.text_raster.stroke = value,
            Setting::Coverage => reader.state.text_raster.coverage_gamma = value,
            Setting::TextSize | Setting::LineSpacing | Setting::Margin => {}
        }
        if layout == reader.state.layout {
            return Ok(Effect::Redraw);
        }
        layout.validate()?;
        // Show the chosen value at once; the pages follow when reflow finishes.
        self.reflow(reader, layout).map(|_| Effect::Redraw)
    }
    pub fn set_sleep_available(&mut self, available: bool) {
        self.sleep_available = available;
    }
    pub fn take_sleep_request(&mut self) -> bool {
        std::mem::take(&mut self.sleep_request)
    }
    /// Shows the running firmware version in the settings.
    pub fn set_firmware(&mut self, version: &str) {
        self.firmware = Some(version.into());
    }
    /// Offers font families to choose besides the frontend's default.
    pub fn set_font_families(&mut self, families: Vec<String>) {
        self.font_families = families;
    }
    /// The family chosen in the menu, for the frontend to load and pass to
    /// [`Self::set_fonts`]; none is the default family.
    pub fn take_font_request(&mut self) -> Option<Option<String>> {
        self.font_request.take()
    }
    /// Lays the book out in other fonts and adopts them when that finishes.
    pub fn set_fonts(
        &mut self,
        reader: &Reader,
        fonts: crate::layout::Fonts,
        family: Option<String>,
    ) -> Result<Effect> {
        if self.is_navigating()
            || self.is_reflowing()
            || self.is_opening()
            || self.is_preview_opening()
        {
            return Ok(Effect::Unchanged);
        }
        let reflow = reader.begin_fonts(fonts, family)?;
        Ok(self.start_reflow(reflow))
    }
    /// Offers the firmware update panel with this content and the setting
    /// for automatic checks. Activating its action raises the update request.
    /// Returns whether the content changed.
    pub fn set_update(&mut self, view: Option<UpdateView>) -> bool {
        let changed = self.update != view;
        self.update = view;
        changed
    }
    pub fn is_update_open(&self) -> bool {
        matches!(self.panel, Some(Panel::Update))
    }
    pub fn take_update_request(&mut self) -> bool {
        std::mem::take(&mut self.update_request)
    }

    /// Updates a read-only root-menu status row without requesting a panel refresh.
    /// The battery charge shown in the reading page's status line.
    pub fn set_battery(&mut self, percent: Option<u8>) {
        self.battery = percent.map(|percent| percent.min(100));
    }
    pub fn set_device_status(&mut self, status: &str) {
        self.device_status = Some(
            status
                .chars()
                .filter(|c| !c.is_control())
                .take(64)
                .collect(),
        );
    }

    pub fn configure_library_scan(&mut self, root: PathBuf, limit: usize) -> Result<()> {
        if !(1..=4096).contains(&limit) {
            return Err(crate::Error::Limit("library limit"));
        }
        if self.is_scanning() {
            self.scan = None;
            self.open_library();
        }
        self.scan_root = Some((root, limit));
        Ok(())
    }
    pub fn rescan_library(&mut self) -> Result<Effect> {
        let (root, limit) = self
            .scan_root
            .as_ref()
            .ok_or_else(|| crate::Error::Document("No library folder configured".into()))?;
        let scan = LibraryScan::cached(root, *limit)?;
        self.scan = Some(scan);
        self.error = None;
        Ok(Effect::Unchanged)
    }
    pub fn is_reflowing(&self) -> bool {
        self.reflow.is_some()
    }
    pub fn rotate(&mut self, reader: &Reader, rotation: crate::state::Rotation) -> Result<Effect> {
        if self.is_navigating()
            || self.is_reflowing()
            || self.is_opening()
            || self.is_preview_opening()
            || matches!(self.panel, Some(Panel::LinkPreview))
            || rotation == reader.state.rotation
        {
            return Ok(Effect::Unchanged);
        }
        let reflow = reader.begin_rotation(rotation)?;
        Ok(self.start_reflow(reflow))
    }
    pub fn resize(&mut self, reader: &Reader, delta: f32) -> Result<Effect> {
        let mut next = reader.state.layout;
        next.font_size = (next.font_size + delta).clamp(8.0, 96.0);
        self.reflow(reader, next)
    }
    fn reflow(&mut self, reader: &Reader, next: crate::layout::LayoutConfig) -> Result<Effect> {
        let next = crate::state::BookTypography {
            margin: next.margin,
            font_size: next.font_size,
            line_height: next.line_height,
            hyphenation: next.hyphenation,
            text_raster: reader.state.text_raster,
        };
        self.typography(reader, next)
    }
    fn typography(
        &mut self,
        reader: &Reader,
        next: crate::state::BookTypography,
    ) -> Result<Effect> {
        if self.is_navigating()
            || self.is_reflowing()
            || self.is_opening()
            || self.is_preview_opening()
            || matches!(self.panel, Some(Panel::LinkPreview))
            || next == crate::state::BookTypography::current(&reader.state)
        {
            return Ok(Effect::Unchanged);
        }
        let reflow = reader.begin_typography(next)?;
        Ok(self.start_reflow(reflow))
    }
    fn start_reflow(&mut self, reflow: Reflowing) -> Effect {
        self.reflow_return = self.panel;
        self.reflow = Some(reflow);
        self.error = None;
        Effect::Unchanged
    }
    fn cancel_reflow(&mut self) -> Effect {
        self.reflow = None;
        // The panel never changed, so the selection stays on its row.
        self.panel = self.reflow_return.take();
        Effect::Redraw
    }
    pub fn poll_reflow(&mut self, reader: &mut Reader) -> Effect {
        let Some(mut job) = self.reflow.take() else {
            return Effect::Unchanged;
        };
        let result = reader.step_reflow(&mut job);
        match result {
            Ok(false) => {
                self.reflow = Some(job);
                Effect::Unchanged
            }
            result => {
                let result = result.and_then(|_| reader.finish_reflow(job));
                self.cancel_reflow();
                if let Err(error) = result {
                    if self.panel.is_none() {
                        self.panel = Some(Panel::Typography);
                    }
                    self.show_error(error.to_string());
                }
                Effect::Redraw
            }
        }
    }
    pub fn is_opening(&self) -> bool {
        self.opening.is_some()
    }
    pub fn poll_open(&mut self, reader: &mut Reader) -> Effect {
        let Some(mut opening) = self.opening.take() else {
            return Effect::Unchanged;
        };
        let result = reader.step_open_for(&mut opening, std::time::Duration::from_millis(5));
        match result {
            Ok(false) => {
                self.opening = Some(opening);
                Effect::Unchanged
            }
            Ok(true) => match reader.finish_open(opening) {
                Ok(()) => {
                    self.panel = None;
                    self.error = None;
                    Effect::Redraw
                }
                Err(error) => {
                    self.open_library();
                    self.show_error(error.to_string());
                    Effect::Redraw
                }
            },
            Err(error) => {
                self.open_library();
                self.show_error(error.to_string());
                Effect::Redraw
            }
        }
    }
    pub fn is_scanning(&self) -> bool {
        self.scan.is_some()
    }
    pub fn poll_scan(&mut self) -> Effect {
        let Some(scan) = &mut self.scan else {
            return Effect::Unchanged;
        };
        match scan.step_for(std::time::Duration::from_millis(5)) {
            Ok(None) => Effect::Unchanged,
            Ok(Some(library)) => {
                self.scan = None;
                self.set_library(library);
                self.open_library();
                Effect::Redraw
            }
            Err(error) => {
                self.scan = None;
                self.open_library();
                self.show_error(error.to_string());
                Effect::Redraw
            }
        }
    }
    pub fn is_editing(&self) -> bool {
        matches!(
            self.panel,
            Some(Panel::Search | Panel::CatalogSearch | Panel::DeviceText(_))
        )
    }
    pub fn query(&self) -> &str {
        match self.panel {
            Some(Panel::CatalogSearch) => &self.catalog_query,
            Some(Panel::DeviceText(_)) => &self.device_text,
            _ => &self.query,
        }
    }
    pub fn type_char(&mut self, character: char) -> Effect {
        if !self.is_editing()
            || character.is_control()
            || self.query().len() + character.len_utf8() > self.text_limit()
        {
            return Effect::Unchanged;
        }
        self.edit_query().push(character);
        self.error = None;
        Effect::Redraw
    }
    pub fn delete_char(&mut self) -> Effect {
        if !self.is_editing() {
            return Effect::Unchanged;
        }
        let Some((offset, _)) = self.query().grapheme_indices(true).next_back() else {
            return Effect::Unchanged;
        };
        self.edit_query().truncate(offset);
        self.error = None;
        Effect::Redraw
    }
    pub fn submit_search(&mut self) -> Effect {
        if !self.is_editing() {
            return Effect::Unchanged;
        }
        if matches!(self.panel, Some(Panel::CatalogSearch)) {
            self.catalog_event = Some(CatalogEvent::Search);
            return Effect::Redraw;
        }
        if let Some(Panel::DeviceText(index)) = self.panel {
            self.device_event = Some(DeviceEvent::Text(
                index,
                std::mem::take(&mut self.device_text),
            ));
            self.panel = Some(Panel::Device);
            self.selected = index;
            return Effect::Redraw;
        }
        self.panel = Some(Panel::Results);
        self.selected = 0;
        self.error = None;
        Effect::Redraw
    }
    pub fn take_light_request(&mut self) -> bool {
        std::mem::take(&mut self.light_request)
    }
    pub fn error_message(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn show_error(&mut self, error: String) {
        self.error = Some(error);
    }
    pub fn set_library(&mut self, library: Library) {
        self.scan = None;
        self.library = library;
        self.list_folders();
        self.selected = 0;
    }
    fn list_folders(&mut self) {
        self.folders = self
            .library
            .entries
            .iter()
            .filter_map(|e| e.path.parent().map(|p| p.to_path_buf()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }
    /// Deletes the file of a library entry other than the open book.
    fn delete_book(&mut self, reader: &Reader, index: usize) -> Result<()> {
        let entry = self
            .library
            .entries
            .get(index)
            .ok_or_else(|| crate::Error::Document("library entry unavailable".into()))?;
        if entry.metadata.id == reader.document.metadata.id {
            return Err(crate::Error::Document("this book is open".into()));
        }
        let (root, limit) = self
            .scan_root
            .as_ref()
            .ok_or_else(|| crate::Error::Document("library folder unavailable".into()))?;
        crate::library_cache::Cache::delete_book(root, &entry.path, *limit)?;
        self.library.entries.remove(index);
        self.list_folders();
        Ok(())
    }
    pub fn open_library(&mut self) {
        self.preview = None;
        self.panel = Some(Panel::Library);
        self.selected = 0;
        self.error = None;
    }
    pub fn is_open(&self) -> bool {
        self.panel.is_some()
    }
    /// Whether background work is pending. The view stays as it was when
    /// the work began; frontends show [`Self::render`]'s busy mark for it.
    pub fn is_busy(&self) -> bool {
        self.is_navigating()
            || self.is_reflowing()
            || self.is_opening()
            || self.is_preview_opening()
            || self.is_scanning()
            || self.is_dictionary_opening()
            || self.catalog_busy
    }
    pub fn back(&mut self, reader: &mut Reader) -> Result<Effect> {
        // Back abandons pending work and leaves the view it started from.
        if self.is_busy() {
            if self.catalog_busy {
                self.catalog_event = Some(CatalogEvent::Back);
            }
            if self.is_navigating() {
                self.cancel_navigation(reader);
            }
            if self.is_reflowing() {
                self.cancel_reflow();
            }
            if self.is_preview_opening() {
                self.cancel_preview();
            }
            self.opening = None;
            self.scan = None;
            // Prepared indexes stay available for the next lookup.
            if let Some(dictionary) = self.dictionary_job.take().filter(|d| d.is_ready()) {
                self.dictionary_cache = Some(dictionary);
            }
            return Ok(Effect::Redraw);
        }
        let Some(panel) = self.panel else {
            return self.navigate(reader, Action::Back);
        };
        match panel {
            Panel::Catalog | Panel::CatalogSearch => {
                self.catalog_event = Some(CatalogEvent::Back);
                return Ok(Effect::Redraw);
            }
            Panel::Device => {
                self.device_event = Some(DeviceEvent::Back);
                return Ok(Effect::Redraw);
            }
            Panel::Kosync => {
                self.kosync_event = Some(KosyncEvent::Back);
                return Ok(Effect::Redraw);
            }
            Panel::DeviceText(index) => {
                // Leaving the keyboard discards the entry.
                self.device_text.clear();
                self.panel = Some(Panel::Device);
                self.selected = index;
                return Ok(Effect::Redraw);
            }
            Panel::LinkPreview => {
                self.preview = None;
                self.panel = self.preview_return.take();
                self.error = None;
                return Ok(Effect::Redraw);
            }
            Panel::DictionaryResult => {
                if let Some(dictionary) = self.dictionary_job.take().filter(|d| d.is_ready()) {
                    self.dictionary_cache = Some(dictionary);
                }
                self.dictionary_rows.clear();
            }
            Panel::SelectWord | Panel::WordPreview | Panel::WordList => {
                self.word_origin = None;
                self.word_pending = None;
                self.word_choices.clear();
            }
            Panel::ClippingDelete => self.clipping_delete = None,
            _ => {}
        }
        self.leave();
        Ok(Effect::Redraw)
    }
    pub fn open(&mut self) {
        if let Some(dictionary) = self.dictionary_job.take().filter(|d| d.is_ready()) {
            self.dictionary_cache = Some(dictionary);
        }
        self.dictionary_rows.clear();
        self.word_choices.clear();
        self.word_origin = None;
        self.word_pending = None;
        self.clipping_delete = None;
        self.preview = None;
        self.remembered.clear();
        self.shortcut = false;
        self.panel = Some(Panel::Root);
        self.selected = 0;
        self.error = None;
    }
    pub fn turn(&mut self, reader: &mut Reader, next: bool) -> Result<Effect> {
        // Input waits for pending work; Back abandons it.
        if self.is_busy() {
            return Ok(Effect::Unchanged);
        }
        if matches!(self.panel, Some(Panel::SelectWord)) {
            let changed = self.word_pan != next;
            self.word_pan = next;
            return Ok(if changed {
                Effect::Redraw
            } else {
                Effect::Unchanged
            });
        }
        if matches!(self.panel, Some(Panel::LinkPreview)) {
            return reader.turn_preview(self.preview.as_mut().expect("preview panel"), next);
        }
        if self.panel.is_none() {
            return self.navigate(reader, if next { Action::Next } else { Action::Previous });
        }
        let (entries, previous) = self.menu(reader);
        self.selected = if next {
            (previous + 1).min(entries.len().saturating_sub(1))
        } else {
            previous.saturating_sub(1)
        };
        Ok(if self.selected == previous {
            Effect::Unchanged
        } else {
            Effect::Redraw
        })
    }
    pub fn preview_page(&self) -> Option<(usize, usize)> {
        if !matches!(self.panel, Some(Panel::LinkPreview)) {
            return None;
        }
        self.preview
            .as_ref()
            .map(|preview| (preview.page_index() + 1, preview.page_count()))
    }

    pub fn is_preview_opening(&self) -> bool {
        self.preview_opening.is_some()
    }

    fn cancel_preview(&mut self) -> Effect {
        self.preview_opening = None;
        self.preview = None;
        self.panel = self.preview_return.take();
        self.error = None;
        Effect::Redraw
    }

    pub fn poll_preview(&mut self, reader: &mut Reader) -> Effect {
        let Some(mut job) = self.preview_opening.take() else {
            return Effect::Unchanged;
        };
        match reader.step_preview(&mut job) {
            Ok(false) => {
                self.preview_opening = Some(job);
                Effect::Unchanged
            }
            result => {
                let result = result
                    .and_then(|_| reader.finish_preview(job))
                    .and_then(|preview| {
                        reader.render_preview(&preview, 4)?;
                        Ok(preview)
                    });
                match result {
                    Ok(preview) => {
                        self.preview = Some(preview);
                        self.panel = Some(Panel::LinkPreview);
                    }
                    Err(error) => {
                        self.cancel_preview();
                        if self.panel.is_none() {
                            self.open();
                        }
                        self.show_error(error.to_string());
                    }
                }
                Effect::Redraw
            }
        }
    }

    fn preview_link(
        &mut self,
        reader: &mut Reader,
        index: usize,
        return_to: Option<Panel>,
    ) -> Result<Effect> {
        let mut config = reader.state.layout;
        let geometry = crate::menu::Geometry::new(config.width, config.height)?;
        config.height = geometry.row(geometry.rows - 1).0 - geometry.row(1).0;
        let preview = reader.begin_preview(index, config)?;
        self.preview_opening = Some(preview);
        self.selected = 0;
        self.error = None;
        self.preview = None;
        self.preview_return = return_to;
        Ok(Effect::Unchanged)
    }

    fn capture_word_range(
        &self,
        reader: &mut Reader,
        mut range: std::ops::Range<crate::layout::Locator>,
    ) -> Result<crate::clipping::Clipping> {
        if !self.word_context_matches(reader) {
            return Err(crate::Error::Document("word selection expired".into()));
        }
        if let Some(pending) = &self.word_pending {
            pending.validate_document(&mut reader.document)?;
            range.start = range.start.min(pending.range.start);
            range.end = range.end.max(pending.range.end);
        }
        let now = reader
            .state
            .books
            .values()
            .map(|book| book.last_opened)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        crate::clipping::Clipping::capture(&mut reader.document, range, now)
    }

    pub fn tap(&mut self, reader: &mut Reader, x: u32, y: u32) -> Result<Effect> {
        // Input waits for pending work; Back abandons it.
        if self.is_busy() {
            return Ok(Effect::Unchanged);
        }
        let layout = reader.state.layout;
        if x >= layout.width || y >= layout.height {
            return Ok(Effect::Unchanged);
        }
        if matches!(self.panel, Some(Panel::SelectWord)) {
            if y >= layout.height.saturating_sub(32) {
                return self.back(reader);
            }
            let result = (|| -> Result<Option<crate::clipping::Clipping>> {
                if !self.word_context_matches(reader) {
                    return Err(crate::Error::Document("word selection expired".into()));
                }
                let Some(range) = reader.word_at(x, y + if self.word_pan { 32 } else { 0 }) else {
                    return Ok(None);
                };
                self.capture_word_range(reader, range).map(Some)
            })();
            return match result {
                Ok(Some(clipping)) => {
                    self.word_pending = Some(clipping);
                    self.panel = Some(Panel::WordPreview);
                    self.selected = 0;
                    Ok(Effect::Redraw)
                }
                Ok(None) => Ok(Effect::Unchanged),
                Err(error) => {
                    self.back(reader)?;
                    self.show_error(error.to_string());
                    Ok(Effect::Redraw)
                }
            };
        }
        if self.panel.is_none() {
            if let Some(index) = reader.link_at(x, y) {
                if reader.page_links()[index].noteref {
                    return self.preview_link(reader, index, None);
                }
                return self.navigate(reader, Action::Link(index));
            }
            if x < layout.width / 3 {
                return self.navigate(reader, Action::Previous);
            }
            if x >= layout.width * 2 / 3 {
                return self.navigate(reader, Action::Next);
            }
            let effect = reader.dispatch(Action::Tap { x, y })?;
            if effect == Effect::Menu {
                self.open();
                return Ok(Effect::Redraw);
            }
            return Ok(effect);
        }
        let geometry = crate::menu::Geometry::new(layout.width, layout.height)?;
        let row = geometry.row_at(y).unwrap();
        if matches!(self.panel, Some(Panel::LinkPreview)) {
            if row == 0 || (row == geometry.rows - 1 && x < layout.width / 2) {
                return self.back(reader);
            }
            if row == geometry.rows - 1 {
                return self.select(reader);
            }
            return self.turn(reader, x >= layout.width / 2);
        }
        if row == 0 {
            return self.back(reader);
        }
        let (entries, selected) = self.menu(reader);
        if self.is_editing() {
            geometry.keyboard()?;
            let Some(index) = geometry.key_at(x, row) else {
                return Ok(Effect::Unchanged);
            };
            self.selected = index;
            return self.activate(reader, entries[index].1);
        }
        if row == geometry.rows - 1 {
            let page = selected / geometry.items;
            let next = if x < layout.width / 2 {
                page.saturating_sub(1)
            } else {
                (page + 1).min(entries.len().saturating_sub(1) / geometry.items)
            };
            self.selected = next * geometry.items;
            return Ok(Effect::Redraw);
        }
        let index = (selected / geometry.items) * geometry.items + row - 1;
        let Some((_, command)) = entries.get(index) else {
            return Ok(Effect::Unchanged);
        };
        let moved = selected != index;
        self.selected = index;
        if let Command::Step(setting) = *command {
            // One tap target per value across the row.
            let position =
                (u64::from(x) * setting.positions() as u64 / u64::from(layout.width)) as usize;
            return self.activate(reader, Command::Set(setting, position));
        }
        // Show the tapped row even when its action only starts pending work.
        Ok(match self.activate(reader, *command)? {
            Effect::Unchanged if moved => Effect::Redraw,
            effect => effect,
        })
    }
    pub fn select(&mut self, reader: &mut Reader) -> Result<Effect> {
        // Input waits for pending work; Back abandons it.
        if self.is_busy() {
            return Ok(Effect::Unchanged);
        }
        if matches!(self.panel, Some(Panel::SelectWord)) {
            return self.back(reader);
        }
        if matches!(self.panel, Some(Panel::LinkPreview)) {
            if let Some(job) =
                reader.begin_follow_preview(self.preview.as_ref().expect("preview panel"))?
            {
                return Ok(self.start_navigation(job));
            }
            reader.follow_preview(self.preview.as_ref().expect("preview panel"))?;
            self.preview = None;
            self.panel = None;
            return Ok(Effect::Redraw);
        }
        if self.panel.is_none() {
            self.open();
            return Ok(Effect::Redraw);
        }
        let (entries, selected) = self.menu(reader);
        match entries.get(selected) {
            Some((_, command)) => {
                self.selected = selected;
                self.activate(reader, *command)
            }
            None => Ok(Effect::Unchanged),
        }
    }
    fn activate(&mut self, reader: &mut Reader, command: Command) -> Result<Effect> {
        self.error = None;
        self.remember();
        let result = match command {
            Command::Kosync(event) => {
                self.kosync_event = Some(event);
                if matches!(event, KosyncEvent::TakeRemote | KosyncEvent::KeepLocal) {
                    self.panel = None;
                }
                Ok(Effect::Redraw)
            }
            Command::SyncAutomatically => {
                reader.state.sync_automatically = !reader.state.sync_automatically;
                Ok(Effect::Redraw)
            }
            Command::SyncConflict => {
                reader.state.sync_conflict = reader.state.sync_conflict.next();
                Ok(Effect::Redraw)
            }
            Command::Rotation(rotation) => {
                self.rotation_request = Some(rotation);
                Ok(Effect::Redraw)
            }
            Command::Tailnet(event) => {
                self.tailnet_event = Some(event);
                Ok(Effect::Redraw)
            }
            Command::Catalog(event) => {
                self.catalog_event = Some(event);
                Ok(Effect::Redraw)
            }
            Command::ChooseDictionary(index) => match self.dictionary_choices.get(index).cloned() {
                Some(path) => {
                    reader.state.dictionary_name =
                        path.file_name().and_then(|s| s.to_str()).map(str::to_owned);
                    self.release_dictionary();
                    self.dictionary_path = Some(path);
                    Ok(Effect::Redraw)
                }
                None => Err(crate::Error::Document(
                    "dictionary choice no longer exists".into(),
                )),
            },
            Command::Noop => Ok(Effect::Unchanged),
            Command::SelectWord | Command::SelectWordList => (|| {
                let by_buttons = matches!(command, Command::SelectWordList);
                let choices = if by_buttons {
                    reader.visible_words()?
                } else {
                    Vec::new()
                };
                self.word_by_buttons = by_buttons;
                self.word_choices = choices;
                self.word_pan = false;
                self.word_origin = Some((
                    reader.document.metadata.id.clone(),
                    reader.position(),
                    reader.state.layout,
                    reader.state.rotation,
                ));
                self.word_pending = None;
                self.panel = Some(if self.word_by_buttons {
                    Panel::WordList
                } else {
                    Panel::SelectWord
                });
                self.selected = 0;
                Ok(Effect::Redraw)
            })(),
            Command::PickWord(start, end) => (|| {
                let clipping = self.capture_word_range(reader, start..end)?;
                self.word_pending = Some(clipping);
                self.panel = Some(Panel::WordPreview);
                self.selected = 0;
                Ok(Effect::Redraw)
            })(),
            Command::LookupWord => (|| {
                if !self.word_context_matches(reader) {
                    return Err(crate::Error::Document("word selection expired".into()));
                }
                let path = self
                    .dictionary_path
                    .as_ref()
                    .ok_or_else(|| crate::Error::Document("no dictionary configured".into()))?;
                let pending = self
                    .word_pending
                    .as_ref()
                    .ok_or_else(|| crate::Error::Document("word selection expired".into()))?;
                if pending.text.len() > 255 {
                    return Err(crate::Error::Limit("dictionary query bytes"));
                }
                pending.validate_document(&mut reader.document)?;
                self.dictionary_job = Some(match self.dictionary_cache.take() {
                    Some(dictionary) => dictionary,
                    None => crate::dictionary::Dictionary::open(path)?,
                });
                self.dictionary_rows.clear();
                Ok(Effect::Unchanged)
            })(),

            Command::ExtendWord => {
                if self.word_context_matches(reader) && self.word_pending.is_some() {
                    self.word_pan = false;
                    self.panel = Some(if self.word_by_buttons {
                        Panel::WordList
                    } else {
                        Panel::SelectWord
                    });
                    self.selected = 0;
                    Ok(Effect::Redraw)
                } else {
                    Err(crate::Error::Document("word selection expired".into()))
                }
            }
            Command::SaveWord => {
                let matches = self.word_context_matches(reader);
                let pending = self.word_pending.take();
                self.word_choices.clear();
                self.word_origin = None;
                self.panel = Some(Panel::Clippings);
                self.selected = 0;
                (|| {
                    let clipping = pending
                        .filter(|_| matches)
                        .ok_or_else(|| crate::Error::Document("word selection expired".into()))?;
                    clipping.validate_document(&mut reader.document)?;
                    reader.state.add_clipping(clipping)?;
                    Ok(Effect::Redraw)
                })()
            }
            Command::ToggleHighlight(index) => reader
                .toggle_clipping_highlight(index)
                .map(|_| Effect::Redraw),
            Command::DeleteClipping(index) => {
                self.clipping_delete = reader
                    .state
                    .books
                    .get(&reader.document.metadata.id)
                    .and_then(|book| book.clippings.get(index))
                    .cloned();
                if self.clipping_delete.is_some() {
                    self.panel = Some(Panel::ClippingDelete);
                    self.selected = 0;
                    Ok(Effect::Redraw)
                } else {
                    Err(crate::Error::Document("clipping no longer exists".into()))
                }
            }
            Command::ConfirmClippingDelete => {
                let pending = self.clipping_delete.take();
                self.panel = Some(Panel::Clippings);
                self.selected = 0;
                match pending {
                    Some(clipping) if clipping.book_id == reader.document.metadata.id => reader
                        .state
                        .remove_clipping(&clipping)
                        .map(|_| Effect::Redraw),
                    _ => Err(crate::Error::Document(
                        "clipping confirmation expired".into(),
                    )),
                }
            }
            Command::ClipPage => {
                let now = reader
                    .state
                    .books
                    .values()
                    .map(|book| book.last_opened)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                reader.clip_page_text(now).map(|_| {
                    self.panel = Some(Panel::Clippings);
                    self.selected = 0;
                    Effect::Redraw
                })
            }
            Command::Rescan => self.rescan_library(),
            Command::Character(character) => Ok(self.type_char(character)),
            Command::Delete => Ok(self.delete_char()),
            Command::Layer => {
                self.keyboard_layer = (self.keyboard_layer + 1) % KEY_LAYERS.len();
                Ok(Effect::Redraw)
            }
            Command::DeviceOpen => {
                self.device_event = Some(DeviceEvent::Open);
                Ok(Effect::Redraw)
            }
            Command::DeviceRow(index) => match self.device_rows.get(index).map(|row| &row.kind) {
                None | Some(DeviceRowKind::Info) => Ok(Effect::Unchanged),
                Some(DeviceRowKind::Text { value, secret, .. }) => {
                    self.device_text = if *secret {
                        String::new()
                    } else {
                        value.clone()
                    };
                    self.keyboard_layer = 0;
                    self.panel = Some(Panel::DeviceText(index));
                    self.selected = 0;
                    Ok(Effect::Redraw)
                }
                Some(_) => {
                    self.device_event = Some(DeviceEvent::Activate(index));
                    Ok(Effect::Redraw)
                }
            },
            Command::RemoteCycle(usage) => {
                let mut bindings = reader.state.remote_bindings.clone().unwrap_or_default();
                bindings.cycle(usage).map(|()| {
                    reader.state.remote_bindings = Some(bindings);
                    Effect::Redraw
                })
            }
            Command::RemoteDefaults => {
                reader.state.remote_bindings = None;
                self.selected = 0;
                Ok(Effect::Redraw)
            }
            Command::RemoteDisable => {
                reader.state.remote_bindings = Some(crate::remote::SavedBindings::new(&[])?);
                self.selected = 0;
                Ok(Effect::Redraw)
            }
            Command::FullRefresh(interval) => {
                reader.state.full_refresh_interval = interval;
                Ok(Effect::Redraw)
            }
            Command::IdleSleep(timeout) => {
                reader.state.idle_sleep = timeout;
                Ok(Effect::Redraw)
            }
            Command::Sleep => {
                self.sleep_request = true;
                Ok(Effect::Unchanged)
            }
            Command::Update => {
                self.update_request = matches!(
                    self.update.as_ref().map(|view| &view.step),
                    Some(UpdateStep::Action(_))
                );
                Ok(Effect::Unchanged)
            }
            Command::AutoUpdate => {
                reader.state.check_updates = !reader.state.check_updates;
                Ok(Effect::Redraw)
            }
            Command::CycleGesture(gesture) => {
                use crate::state::GestureAction;
                let action = gesture.action(&mut reader.state.gestures);
                let index = GestureAction::ALL.iter().position(|known| known == action);
                *action =
                    GestureAction::ALL[index.map_or(0, |i| (i + 1) % GestureAction::ALL.len())];
                Ok(Effect::Redraw)
            }
            Command::Font(choice) => {
                let family = choice.and_then(|index| self.font_families.get(index).cloned());
                if family != reader.state.font_family {
                    self.font_request = Some(family);
                }
                Ok(Effect::Unchanged)
            }
            Command::SwapMenuButtons => {
                reader.state.swap_menu_buttons = !reader.state.swap_menu_buttons;
                Ok(Effect::Redraw)
            }
            Command::SwapPageButtons => {
                reader.state.swap_page_buttons = !reader.state.swap_page_buttons;
                Ok(Effect::Redraw)
            }
            Command::StatusLine => {
                if self.is_navigating() || self.is_reflowing() || self.is_opening() {
                    return Ok(Effect::Unchanged);
                }
                let mut next = reader.state.layout;
                next.footer = if next.footer == 0 { STATUS_ROWS } else { 0 };
                Ok(self.start_reflow(reader.begin_reflow(next)?))
            }
            Command::Find => Ok(self.submit_search()),
            Command::Step(setting) => {
                // Advance to the next value that the layout accepts, wrapping.
                let current = setting.position(&reader.state);
                let next = (1..=setting.positions())
                    .map(|step| (current + step) % setting.positions())
                    .find(|position| {
                        setting
                            .layout(reader.state.layout, *position)
                            .validate()
                            .is_ok()
                    })
                    .unwrap_or(current);
                self.set(reader, setting, next)
            }
            Command::Set(setting, position) => self.set(reader, setting, position),
            Command::Book(index) => match self.library.entries.get(index) {
                Some(entry) => {
                    let opening = if let Some((root, limit)) = &self.scan_root {
                        reader.begin_open_cached_file(&entry.path, root, *limit)
                    } else {
                        reader.begin_open_file(&entry.path)
                    };
                    opening.map(|opening| {
                        self.opening = Some(opening);
                        Effect::Unchanged
                    })
                }
                None => Err(crate::Error::Document("library entry unavailable".into())),
            },
            Command::DeleteBook(index) => {
                self.panel = Some(Panel::DeleteConfirm(index));
                self.selected = 0;
                Ok(Effect::Redraw)
            }
            Command::ConfirmDeleteBook(index) => {
                self.panel = Some(Panel::Delete);
                self.selected = index;
                self.delete_book(reader, index).map(|()| Effect::Redraw)
            }
            Command::Leave => self.back(reader),
            Command::Resume => {
                self.panel = None;
                Ok(Effect::Redraw)
            }
            Command::Open(panel) => {
                self.panel = Some(panel);
                self.selected = match panel {
                    // The update panel opens on its action, below the status.
                    Panel::Update => 1,
                    // The list of places opens on the one being read.
                    Panel::GoTo => reader
                        .book_progress_percent()
                        .map_or(0, |percent| usize::from(percent.min(99)) / GO_TO_STEP),
                    _ => 0,
                };
                Ok(Effect::Redraw)
            }
            Command::Reader(Action::Link(index)) => {
                self.preview_link(reader, index, Some(Panel::Links))
            }
            Command::Reader(action) => self.navigate(reader, action).map(|_| {
                if !self.is_navigating() {
                    self.panel = None;
                }
                Effect::Redraw
            }),
            Command::ResetTypography => self.typography(
                reader,
                crate::state::BookTypography::current(&crate::state::ReaderState::default()),
            ),
            Command::Hyphenation => {
                let mut next = reader.state.layout;
                next.hyphenation = next.hyphenation.next();
                self.reflow(reader, next)
            }
            Command::Hinting => {
                reader.state.text_raster.hinting = !reader.state.text_raster.hinting;
                Ok(Effect::Redraw)
            }
            Command::Fractional => {
                reader.state.text_raster.fractional = !reader.state.text_raster.fractional;
                Ok(Effect::Redraw)
            }
        };
        match result {
            Ok(effect) => Ok(effect),
            Err(error) => {
                self.error = Some(error.to_string());
                Ok(Effect::Redraw)
            }
        }
    }
    /// The entries of the current menu and the index of the selected one.
    /// The frontend and the reader can leave fewer entries than there were
    /// when the selection was made, so it is read only here, where it is
    /// kept within them.
    fn menu(&self, reader: &Reader) -> (Vec<(String, Command)>, usize) {
        let entries = self.entries(reader);
        let selected = self.selected.min(entries.len().saturating_sub(1));
        (entries, selected)
    }
    fn entries(&self, reader: &Reader) -> Vec<(String, Command)> {
        match self.panel.unwrap_or(Panel::Root) {
            Panel::Kosync => {
                let mut rows = vec![(self.kosync_status.clone(), Command::Noop)];
                if !self.kosync_busy {
                    rows.extend([
                        (
                            "Upload reading position".into(),
                            Command::Kosync(KosyncEvent::Push),
                        ),
                        (
                            "Download reading position".into(),
                            Command::Kosync(KosyncEvent::Pull),
                        ),
                        ("Check account".into(), Command::Kosync(KosyncEvent::Auth)),
                        (
                            "Create account".into(),
                            Command::Kosync(KosyncEvent::Register),
                        ),
                        ("Sync automatically".into(), Command::SyncAutomatically),
                        (
                            format!("If both moved: {}", reader.state.sync_conflict.label()),
                            Command::SyncConflict,
                        ),
                    ]);
                }
                rows
            }
            Panel::Catalog => self
                .catalog_rows
                .iter()
                .enumerate()
                .map(|(index, label)| {
                    (
                        label.clone(),
                        Command::Catalog(CatalogEvent::Activate(index)),
                    )
                })
                .collect(),
            Panel::Root => [
                ("Resume reading".into(), Command::Resume),
                ("Contents".into(), Command::Open(Panel::Contents)),
                ("Bookmarks".into(), Command::Open(Panel::Bookmarks)),
                ("Library".into(), Command::Open(Panel::Library)),
            ]
            .into_iter()
            .chain(
                self.catalog_available
                    .then(|| ("Catalog".into(), Command::Catalog(CatalogEvent::Open))),
            )
            .chain([
                ("Frontlight".into(), Command::Open(Panel::Light)),
                ("Typography".into(), Command::Open(Panel::Typography)),
                ("This book".into(), Command::Open(Panel::Book)),
                ("Settings".into(), Command::Open(Panel::Settings)),
            ])
            .chain(
                self.sleep_available
                    .then(|| ("Save and sleep".into(), Command::Sleep)),
            )
            .chain(
                self.device_status
                    .iter()
                    .map(|status| (status.clone(), Command::Noop)),
            )
            .collect(),
            Panel::Book => [
                ("Chapters".into(), Command::Open(Panel::Chapters)),
                ("Go to".into(), Command::Open(Panel::GoTo)),
                ("Links on this page".into(), Command::Open(Panel::Links)),
                ("Return from jump".into(), Command::Reader(Action::Back)),
                ("Clippings".into(), Command::Open(Panel::Clippings)),
            ]
            .into_iter()
            .chain(
                self.kosync_available
                    .then(|| ("KOReader sync".into(), Command::Kosync(KosyncEvent::Open))),
            )
            .collect(),
            Panel::Settings => std::iter::empty()
                .chain(
                    self.update
                        .is_some()
                        .then(|| ("Firmware update".into(), Command::Open(Panel::Update))),
                )
                .chain(
                    self.device_available
                        .then(|| ("Network and services".into(), Command::DeviceOpen)),
                )
                .chain(
                    self.tailnet_available
                        .then(|| ("Tailnet".into(), Command::Tailnet(TailnetEvent::Open))),
                )
                .chain(
                    self.sleep_available
                        .then(|| ("Automatic sleep".into(), Command::Open(Panel::Power))),
                )
                .chain(
                    self.rotation_available
                        .then(|| ("Screen orientation".into(), Command::Open(Panel::Rotation))),
                )
                .chain(
                    self.refresh_available
                        .then(|| ("Full refresh".into(), Command::Open(Panel::Refresh))),
                )
                .chain(
                    (!self.dictionary_choices.is_empty())
                        .then(|| ("Dictionary".into(), Command::Open(Panel::DictionaryChooser))),
                )
                .chain(
                    self.gestures_available
                        .then(|| ("Gestures".into(), Command::Open(Panel::Gestures))),
                )
                .chain(std::iter::once(("Status line".into(), Command::StatusLine)))
                .chain(std::iter::once((
                    "Remote buttons".into(),
                    Command::Open(Panel::Remote),
                )))
                .chain(
                    self.page_buttons_available
                        .then(|| {
                            [
                                ("Swap page-turn buttons".into(), Command::SwapPageButtons),
                                ("Swap menu buttons".into(), Command::SwapMenuButtons),
                            ]
                        })
                        .into_iter()
                        .flatten(),
                )
                .chain(
                    self.firmware
                        .iter()
                        .map(|version| (format!("Version {version}"), Command::Noop)),
                )
                .collect(),
            Panel::Remote => {
                use crate::remote::RemoteAction;
                let bindings = reader.state.remote_bindings.clone().unwrap_or_default();
                let common = [
                    (0x7004b, "Page Up"),
                    (0x7004e, "Page Down"),
                    (0x70050, "Left arrow"),
                    (0x7004f, "Right arrow"),
                    (0xc00e9, "Volume up"),
                    (0xc00ea, "Volume down"),
                ];
                let mut rows = vec![
                    ("Restore defaults".into(), Command::RemoteDefaults),
                    ("Disable all buttons".into(), Command::RemoteDisable),
                ];
                let usages = common
                    .iter()
                    .map(|&(usage, label)| (usage, label.to_owned()))
                    .chain(
                        bindings
                            .entries()
                            .iter()
                            .filter(|entry| !common.iter().any(|&(usage, _)| usage == entry.usage))
                            .map(|entry| (entry.usage, format!("HID {:08X}", entry.usage))),
                    );
                for (usage, label) in usages {
                    let action = match bindings
                        .entries()
                        .iter()
                        .find(|entry| entry.usage == usage)
                        .map(|entry| entry.action)
                    {
                        Some(RemoteAction::Previous) => "previous",
                        Some(RemoteAction::Next) => "next",
                        None => "off",
                    };
                    rows.push((format!("{label}: {action}"), Command::RemoteCycle(usage)));
                }
                rows
            }
            Panel::DictionaryChooser => self
                .dictionary_choices
                .iter()
                .enumerate()
                .map(|(index, path)| {
                    let marker = if self.dictionary_path.as_ref() == Some(path) {
                        "* "
                    } else {
                        ""
                    };
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    (format!("{marker}{name}"), Command::ChooseDictionary(index))
                })
                .collect(),
            Panel::Clippings => std::iter::once(("Save page text".into(), Command::ClipPage))
                .chain(
                    reader
                        .state
                        .books
                        .get(&reader.document.metadata.id)
                        .into_iter()
                        .flat_map(|book| book.clippings.iter().enumerate())
                        .map(|(index, clipping)| {
                            (
                                format!(
                                    "{}: {}",
                                    index + 1,
                                    clipping
                                        .text
                                        .chars()
                                        .take(48)
                                        .map(|c| if c.is_whitespace() { ' ' } else { c })
                                        .collect::<String>()
                                ),
                                Command::Open(Panel::Clipping(index)),
                            )
                        }),
                )
                .chain(std::iter::once(("Select text".into(), Command::SelectWord)))
                .chain(std::iter::once((
                    "Select text with buttons".into(),
                    Command::SelectWordList,
                )))
                .collect(),
            Panel::SelectWord => Vec::new(),
            Panel::WordList => self
                .word_choices
                .iter()
                .map(|(label, start, end)| (label.clone(), Command::PickWord(*start, *end)))
                .collect(),
            Panel::DictionaryResult => {
                std::iter::once(("Back to selection".into(), Command::Leave))
                    .chain(
                        self.dictionary_rows
                            .iter()
                            .map(|row| (row.clone(), Command::Noop)),
                    )
                    .collect()
            }
            Panel::WordPreview => vec![
                ("Cancel".into(), Command::Leave),
                ("Save selection".into(), Command::SaveWord),
                ("Extend selection".into(), Command::ExtendWord),
            ]
            .into_iter()
            .chain(
                self.dictionary_path
                    .iter()
                    .map(|_| ("Look up selection".into(), Command::LookupWord)),
            )
            .chain(self.word_pending.iter().flat_map(|c| {
                excerpt_rows(&c.text)
                    .into_iter()
                    .map(|row| (row, Command::Noop))
            }))
            .collect(),
            Panel::Clipping(index) => reader
                .state
                .books
                .get(&reader.document.metadata.id)
                .and_then(|book| book.clippings.get(index))
                .map(|clipping| {
                    std::iter::once(("Delete excerpt".into(), Command::DeleteClipping(index)))
                        .chain(std::iter::once((
                            "Go to source".into(),
                            Command::Reader(Action::Clipping(index)),
                        )))
                        .chain(std::iter::once((
                            if clipping.highlighted {
                                "Remove highlight"
                            } else {
                                "Highlight excerpt"
                            }
                            .into(),
                            Command::ToggleHighlight(index),
                        )))
                        .chain(
                            excerpt_rows(&clipping.text)
                                .into_iter()
                                .map(|row| (row, Command::Noop)),
                        )
                        .collect()
                })
                .unwrap_or_default(),
            Panel::ClippingDelete => vec![
                ("Keep excerpt".into(), Command::Leave),
                ("Delete this excerpt".into(), Command::ConfirmClippingDelete),
            ]
            .into_iter()
            .chain(self.clipping_delete.iter().flat_map(|clipping| {
                excerpt_rows(&clipping.text)
                    .into_iter()
                    .map(|row| (row, Command::Noop))
            }))
            .collect(),
            Panel::Rotation => {
                use crate::state::Rotation::*;
                [
                    ("Portrait", Portrait),
                    ("Clockwise", Clockwise),
                    ("Upside down", Inverted),
                    ("Counterclockwise", Counterclockwise),
                ]
                .into_iter()
                .map(|(label, value)| {
                    (
                        format!(
                            "{}{}",
                            if reader.state.rotation == value {
                                "> "
                            } else {
                                ""
                            },
                            label
                        ),
                        Command::Rotation(value),
                    )
                })
                .collect()
            }
            Panel::Tailnet => vec![
                (
                    "Connect existing identity".into(),
                    Command::Tailnet(TailnetEvent::Connect),
                ),
                (
                    "Create new identity + connect".into(),
                    Command::Tailnet(TailnetEvent::Provision),
                ),
                ("Disconnect".into(), Command::Tailnet(TailnetEvent::Stop)),
                (
                    "Renew node key / retry".into(),
                    Command::Tailnet(TailnetEvent::Renew),
                ),
            ]
            .into_iter()
            .chain(
                self.tailnet_rows
                    .iter()
                    .map(|row| (row.clone(), Command::Noop)),
            )
            .collect(),
            Panel::Update => self
                .update
                .iter()
                .flat_map(|view| {
                    let step = match &view.step {
                        UpdateStep::Action(label) | UpdateStep::Working { label, .. } => label,
                    };
                    [
                        (view.status.clone(), Command::Noop),
                        (step.clone(), Command::Update),
                        ("Check automatically".into(), Command::AutoUpdate),
                    ]
                })
                .collect(),
            Panel::Refresh => crate::state::FULL_REFRESH_INTERVALS
                .into_iter()
                .map(|interval| {
                    (
                        format!(
                            "{}Every {interval} pages",
                            if reader.state.full_refresh_interval == interval {
                                "* "
                            } else {
                                ""
                            }
                        ),
                        Command::FullRefresh(interval),
                    )
                })
                .collect(),
            Panel::Power => {
                use crate::state::IdleSleep::*;
                [Off, FiveMinutes, TenMinutes, ThirtyMinutes, SixtyMinutes]
                    .into_iter()
                    .map(|timeout| {
                        let label = if timeout == Off {
                            "Off".into()
                        } else {
                            format!("{} minutes", timeout.minutes())
                        };
                        (
                            format!(
                                "{}{}",
                                if reader.state.idle_sleep == timeout {
                                    "* "
                                } else {
                                    ""
                                },
                                label
                            ),
                            Command::IdleSleep(timeout),
                        )
                    })
                    .collect()
            }
            Panel::Library => vec![
                (
                    format!("All books ({})", self.library.entries.len()),
                    Command::Open(Panel::Books),
                ),
                ("Recent books".into(), Command::Open(Panel::Recent)),
                ("Folders".into(), Command::Open(Panel::Folders)),
                ("Search".into(), Command::Open(Panel::Search)),
                ("Rescan folder".into(), Command::Rescan),
                (
                    format!("Scan errors ({})", self.library.errors.len()),
                    Command::Open(Panel::Errors),
                ),
            ]
            .into_iter()
            .chain(
                self.scan_root
                    .is_some()
                    .then(|| ("Delete a book".into(), Command::Open(Panel::Delete))),
            )
            .collect(),
            Panel::DeleteConfirm(index) => [
                ("Keep book".into(), Command::Leave),
                ("Delete this book".into(), Command::ConfirmDeleteBook(index)),
            ]
            .into_iter()
            .chain(
                self.library
                    .entries
                    .get(index)
                    .into_iter()
                    .flat_map(|entry| {
                        [
                            (entry.metadata.title.clone(), Command::Noop),
                            (
                                entry
                                    .path
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned(),
                                Command::Noop,
                            ),
                        ]
                    }),
            )
            .collect(),
            Panel::Errors => self
                .library
                .errors
                .iter()
                .enumerate()
                .map(|(i, message)| (message.clone(), Command::Open(Panel::Diagnostic(i))))
                .collect(),
            Panel::Diagnostic(index) => self
                .library
                .errors
                .get(index)
                .map(|message| {
                    message
                        .graphemes(true)
                        .collect::<Vec<_>>()
                        .chunks(32)
                        .map(|chunk| (chunk.concat(), Command::Noop))
                        .collect()
                })
                .unwrap_or_default(),
            Panel::Search | Panel::CatalogSearch | Panel::DeviceText(_) => KEY_LAYERS
                [self.keyboard_layer]
                .chars()
                .map(|c| (c.to_string(), Command::Character(c)))
                .chain([
                    (LAYER_LABELS[self.keyboard_layer].into(), Command::Layer),
                    ("Space".into(), Command::Character(' ')),
                    ("Delete".into(), Command::Delete),
                    ("Done".into(), Command::Find),
                ])
                .collect(),
            Panel::Device => self
                .device_rows
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let label = match &row.kind {
                        DeviceRowKind::Text {
                            secret: true,
                            value,
                            ..
                        } => format!(
                            "{}: {}",
                            row.label,
                            if value.is_empty() { "not set" } else { "set" }
                        ),
                        DeviceRowKind::Text { value, .. } if value.is_empty() => {
                            format!("{}: not set", row.label)
                        }
                        DeviceRowKind::Text { value, .. } => format!("{}: {value}", row.label),
                        _ => row.label.clone(),
                    };
                    (label, Command::DeviceRow(index))
                })
                .collect(),
            Panel::Folders => self
                .folders
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    (
                        p.to_string_lossy().into_owned(),
                        Command::Open(Panel::Folder(i)),
                    )
                })
                .collect(),
            Panel::Books | Panel::Delete | Panel::Recent | Panel::Folder(_) | Panel::Results => {
                let matches = if matches!(self.panel, Some(Panel::Results)) {
                    self.library
                        .search(&self.query)
                        .into_iter()
                        .map(|e| e.path.as_path())
                        .collect::<HashSet<_>>()
                } else {
                    HashSet::new()
                };
                let mut indices: Vec<_> = self
                    .library
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| match self.panel {
                        Some(Panel::Results) => matches.contains(e.path.as_path()),
                        Some(Panel::Recent) => reader.state.books.contains_key(&e.metadata.id),
                        Some(Panel::Folder(index)) => self
                            .folders
                            .get(index)
                            .is_some_and(|folder| e.path.parent() == Some(folder.as_path())),
                        _ => true,
                    })
                    .map(|(i, _)| i)
                    .collect();
                if matches!(self.panel, Some(Panel::Recent)) {
                    indices.sort_by_key(|i| {
                        std::cmp::Reverse(
                            reader.state.books[&self.library.entries[*i].metadata.id].last_opened,
                        )
                    });
                }
                indices
                    .into_iter()
                    .map(|i| {
                        let entry = &self.library.entries[i];
                        let label = if entry.metadata.author.is_empty() {
                            entry.metadata.title.clone()
                        } else {
                            format!("{} - {}", entry.metadata.title, entry.metadata.author)
                        };
                        if matches!(self.panel, Some(Panel::Delete)) {
                            (label, Command::DeleteBook(i))
                        } else {
                            (label, Command::Book(i))
                        }
                    })
                    .collect()
            }
            Panel::Contents => reader
                .document
                .metadata
                .contents
                .iter()
                .enumerate()
                .map(|(i, c)| (c.title.clone(), Command::Reader(Action::Contents(i))))
                .collect(),
            Panel::SyncConflict => {
                let (remote, local) = self.sync_conflict;
                vec![
                    (
                        format!("Go to {remote}%"),
                        Command::Kosync(KosyncEvent::TakeRemote),
                    ),
                    (
                        local.map_or_else(
                            || "Stay here".into(),
                            |local| format!("Stay at {local}%"),
                        ),
                        Command::Kosync(KosyncEvent::KeepLocal),
                    ),
                ]
            }
            Panel::GoTo => (0..100)
                .step_by(GO_TO_STEP)
                .map(|percent| {
                    (
                        format!("{percent}%"),
                        Command::Reader(Action::Progress(percent as u8)),
                    )
                })
                .collect(),
            Panel::Chapters => reader
                .document
                .metadata
                .chapters
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    (
                        format!("{}: {c}", i + 1),
                        Command::Reader(Action::Chapter(i)),
                    )
                })
                .collect(),
            Panel::Bookmarks => std::iter::once((
                "Toggle bookmark".into(),
                Command::Reader(Action::ToggleBookmark),
            ))
            .chain(
                reader
                    .state
                    .books
                    .get(&reader.document.metadata.id)
                    .into_iter()
                    .flat_map(|book| book.bookmarks.iter().enumerate())
                    .map(|(i, b)| (b.label.clone(), Command::Reader(Action::Bookmark(i)))),
            )
            .collect(),
            Panel::LinkPreview => Vec::new(),
            Panel::Links => reader
                .page_links()
                .iter()
                .enumerate()
                .map(|(i, l)| (l.label.clone(), Command::Reader(Action::Link(i))))
                .collect(),
            Panel::Light => [Setting::Brightness, Setting::Warmth]
                .into_iter()
                .map(|setting| (setting.label(&reader.state), Command::Step(setting)))
                .chain(std::iter::once((
                    "Turn off".into(),
                    Command::Set(Setting::Brightness, 0),
                )))
                .collect(),
            Panel::Gestures => [
                ("Swipe up", Gesture::SwipeUp),
                ("Swipe down", Gesture::SwipeDown),
                ("Touch and hold", Gesture::Hold),
            ]
            .into_iter()
            .map(|(label, gesture)| {
                let mut gestures = reader.state.gestures;
                let action = gesture.action(&mut gestures).label();
                (format!("{label}: {action}"), Command::CycleGesture(gesture))
            })
            .collect(),
            Panel::Fonts => {
                let current = reader.state.font_family.as_deref();
                let row = |name: &str, chosen: bool, choice| {
                    let marker = if chosen { "> " } else { "" };
                    (format!("{marker}{name}"), Command::Font(choice))
                };
                std::iter::once(row("Default", current.is_none(), None))
                    .chain(self.font_families.iter().enumerate().map(|(index, name)| {
                        row(name, current == Some(name.as_str()), Some(index))
                    }))
                    .collect()
            }
            Panel::Typography => (!self.font_families.is_empty())
                .then(|| {
                    let name = reader.state.font_family.as_deref().unwrap_or("Default");
                    (format!("Font: {name}"), Command::Open(Panel::Fonts))
                })
                .into_iter()
                .chain(
                    [
                        Setting::TextSize,
                        Setting::LineSpacing,
                        Setting::Margin,
                        Setting::Stroke,
                        Setting::Coverage,
                    ]
                    .into_iter()
                    .map(|setting| (setting.label(&reader.state), Command::Step(setting))),
                )
                .chain([
                    (
                        format!("Hyphenation: {}", reader.state.layout.hyphenation.label()),
                        Command::Hyphenation,
                    ),
                    ("Font hinting".into(), Command::Hinting),
                    ("Fractional positions".into(), Command::Fractional),
                    ("Reset book typography".into(), Command::ResetTypography),
                ])
                .collect(),
        }
    }
    fn render_keyboard(&self, reader: &Reader, levels: u16) -> Result<Vec<u8>> {
        let layout = reader.state.layout;
        let geometry = crate::menu::Geometry::new(layout.width, layout.height)?;
        geometry.keyboard()?;
        let width = layout.width as usize;
        let mut canvas = Canvas {
            pixels: vec![255; width * layout.height as usize],
            width,
        };
        let label = |text: &str, cell_width: usize, height: usize, size, bold| {
            reader.render_styled_cell(
                text,
                cell_width as u32,
                height as u32,
                levels,
                crate::controller::CellStyle::Label { size, bold },
            )
        };
        // The field shows the end of the text with a cursor, or the error.
        let tail: String = self
            .query()
            .graphemes(true)
            .rev()
            .take(26)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let field = match &self.error {
            Some(error) => error.graphemes(true).take(64).collect(),
            None => format!("{tail}_"),
        };
        let (top, height) = geometry.row(0);
        let (top, height) = (top as usize, height as usize);
        canvas.chevron(26, top + height / 2, false, 0);
        canvas.blit(
            44,
            top,
            &label(&field, width - 44, height, 24, false)?,
            width - 44,
            false,
        );
        canvas.rule(top + height - 1);
        let (entries, selected) = self.menu(reader);
        for (index, (text, _)) in entries.iter().enumerate() {
            let (row, x, key_width) = geometry.key(index);
            let (top, height) = geometry.row(row);
            let (x, key_width, top, height) = (
                x as usize,
                key_width as usize,
                top as usize,
                height as usize,
            );
            let selected = selected == index;
            if selected {
                canvas.fill(x, top, key_width, height, 0);
            }
            // Keys are set from the left margin; copy their ink centered.
            let size = if text.chars().count() == 1 { 28 } else { 22 };
            let cell = label(text, key_width.max(160), height, size, false)?;
            let cell_width = key_width.max(160);
            let columns =
                (0..cell_width).filter(|x| (0..height).any(|y| cell[y * cell_width + x] < 255));
            let (left, right) = (
                columns.clone().min().unwrap_or(0),
                columns.max().unwrap_or(0),
            );
            let span = (right + 1 - left).min(key_width);
            let offset = x + (key_width - span) / 2;
            for y in 0..height {
                for dx in 0..span {
                    let value = cell[y * cell_width + left + dx];
                    canvas.pixels[(top + y) * width + offset + dx] =
                        if selected { 255 - value } else { value };
                }
            }
            canvas.fill(x + key_width - 1, top, 1, height, 0);
            canvas.fill(x, top + height - 1, key_width, 1, 0);
        }
        Ok(canvas.pixels)
    }
    /// Renders the current view, with an hourglass in the top-right corner
    /// while background work is pending.
    pub fn render(&self, reader: &mut Reader, levels: u16) -> Result<Vec<u8>> {
        let mut pixels = self.render_view(reader, levels)?;
        if self.is_busy() {
            crate::display::busy_mark(&mut pixels, reader.state.layout.width as usize);
        }
        Ok(pixels)
    }
    /// Draws book progress and battery charge in the rows the layout keeps
    /// free below the text.
    fn status_line(&self, reader: &Reader, pixels: &mut Vec<u8>, levels: u16) -> Result<()> {
        let layout = reader.state.layout;
        if layout.footer == 0 {
            return Ok(());
        }
        let (width, rows) = (layout.width as usize, STATUS_INK_ROWS as usize);
        let top = (layout.height as usize - rows) * width;
        let inset = layout.margin.max(12) as usize;
        // A label is set from its cell's left margin; copy its ink to where
        // it belongs, ending at `end` or else starting at the inset.
        let mut label = |text: &str, end: Option<usize>| -> Result<()> {
            let style = crate::controller::CellStyle::Label {
                size: 14,
                bold: false,
            };
            let cell =
                reader.render_styled_cell(text, layout.width, STATUS_INK_ROWS, levels, style)?;
            let columns = (0..width).filter(|x| (0..rows).any(|y| cell[y * width + x] < 255));
            let (Some(left), Some(right)) = (columns.clone().min(), columns.max()) else {
                return Ok(());
            };
            let span = right + 1 - left;
            let start = end.map_or(inset, |end| end.saturating_sub(span));
            if start + span > width {
                return Ok(());
            }
            for y in 0..rows {
                pixels[top + y * width + start..][..span]
                    .copy_from_slice(&cell[y * width + left..][..span]);
            }
            Ok(())
        };
        if let Some(percent) = reader.book_progress_percent() {
            label(&format!("{percent}%"), None)?;
        }
        if let Some(percent) = self.battery {
            // A cell outline with its terminal, filled to the charge.
            const BODY: (usize, usize) = (22, 11);
            let x = width.saturating_sub(inset + BODY.0 + 2);
            label(&format!("{percent}%"), Some(x.saturating_sub(6)))?;
            let mut canvas = Canvas {
                pixels: std::mem::take(pixels),
                width,
            };
            let y = top / width + (rows - BODY.1) / 2;
            canvas.fill(x, y, BODY.0, BODY.1, 0);
            canvas.fill(x + 1, y + 1, BODY.0 - 2, BODY.1 - 2, 255);
            canvas.fill(x + BODY.0, y + 3, 2, BODY.1 - 6, 0);
            let charge = (BODY.0 - 4) * usize::from(percent) / 100;
            canvas.fill(x + 2, y + 2, charge, BODY.1 - 4, 0);
            *pixels = canvas.pixels;
        }
        Ok(())
    }
    fn render_view(&self, reader: &mut Reader, levels: u16) -> Result<Vec<u8>> {
        let Some(panel) = self.panel else {
            let mut pixels = reader.render(levels)?;
            self.status_line(reader, &mut pixels, levels)?;
            return Ok(pixels);
        };
        if matches!(panel, Panel::SelectWord) {
            let width = reader.state.layout.width as usize;
            let height = reader.state.layout.height as usize;
            let mut pixels = match &self.word_pending {
                Some(pending) if self.word_context_matches(reader) => {
                    reader.render_selection(&pending.range, levels)?
                }
                _ => reader.render(levels)?,
            };
            let instruction = if self.word_pending.is_some() {
                "Tap end word; arrows pan; Back"
            } else {
                "Tap word; arrows pan; Back exits"
            };
            let footer = reader.render_cell(instruction, width as u32, 80, levels)?;
            let offset = height
                .checked_sub(32)
                .ok_or(crate::Error::Layout("word selection viewport too small"))?
                * width;
            if self.word_pan {
                pixels.copy_within(32 * width.., 0);
            }
            pixels[offset..].copy_from_slice(&footer[..32 * width]);
            pixels[offset..offset + width].fill(0);
            return Ok(pixels);
        }
        if self.is_editing() {
            return self.render_keyboard(reader, levels);
        }
        if matches!(panel, Panel::LinkPreview) {
            let preview = self.preview.as_ref().expect("preview panel");
            let geometry =
                crate::menu::Geometry::new(reader.state.layout.width, reader.state.layout.height)?;
            let mut rows = vec![String::new(); geometry.rows];
            rows[0] = format!(
                "< Back | Preview {}/{}",
                preview.page_index() + 1,
                preview.page_count()
            );
            rows[geometry.rows - 1] = "< Close | Follow link >".into();
            let mut pixels = reader.render_rows(&rows, None, levels)?;
            let body = reader.render_preview(preview, levels)?;
            let width = reader.state.layout.width as usize;
            let top = geometry.row(1).0 as usize;
            pixels[top * width..top * width + body.len()].copy_from_slice(&body);
            return Ok(pixels);
        }
        let reader_title = reader.book_progress_percent().map_or_else(
            || "Reader".to_string(),
            |percent| format!("Reader ~{percent}%"),
        );
        let title = match panel {
            Panel::Errors => "Scan errors",
            Panel::Diagnostic(_) => "Error details",
            Panel::Search | Panel::CatalogSearch | Panel::DeviceText(_) => "Search",
            Panel::Device => &self.device_title,
            Panel::Results => "Search results",
            Panel::Root => &reader_title,
            Panel::Book => "This book",
            Panel::Settings => "Settings",
            Panel::Rotation => "Screen orientation",
            Panel::Tailnet => "Tailnet",
            Panel::Catalog => &self.catalog_title,
            Panel::Kosync => "KOReader sync",
            Panel::Contents => "Contents",
            Panel::Chapters => "Chapters",
            Panel::GoTo => "Go to",
            Panel::SyncConflict => "Keep which position?",
            Panel::Bookmarks => "Bookmarks",
            Panel::Clippings => "Clippings",
            Panel::Clipping(_) => "Saved excerpt",
            Panel::ClippingDelete => "Delete excerpt?",
            Panel::SelectWord => "Select word",
            Panel::WordPreview => "Selected text",
            Panel::DictionaryResult => "Dictionary",
            Panel::DictionaryChooser => "Choose dictionary",
            Panel::WordList => "Choose source word",
            Panel::Links => "Page links",
            Panel::LinkPreview => "Link preview",
            Panel::Typography => "Typography",
            Panel::Fonts => "Font",
            Panel::Gestures => "Gestures while reading",
            Panel::Remote => "Remote buttons",
            Panel::Light => "Frontlight",
            Panel::Power => "Automatic sleep",
            Panel::Refresh => "Full refresh",
            Panel::Update => "Firmware update",
            Panel::Library if self.library.truncated => "Library (incomplete)",
            Panel::Library => "Library",
            Panel::Books => "All books",
            Panel::Delete => "Delete a book",
            Panel::DeleteConfirm(_) => "Delete this book?",
            Panel::Recent => "Recent books",
            Panel::Folders | Panel::Folder(_) => "Folders",
        };
        let (entries, selection) = self.menu(reader);
        let layout = reader.state.layout;
        let geometry = crate::menu::Geometry::new(layout.width, layout.height)?;
        let count = geometry.items;
        let start = selection / count * count;
        let width = layout.width as usize;
        let mut canvas = Canvas {
            pixels: vec![255; width * layout.height as usize],
            width,
        };
        let cell = |text: &str, cell_width: usize, height: u32, size, bold| {
            let text: String = text.graphemes(true).take(96).collect();
            reader.render_styled_cell(
                &text,
                cell_width as u32,
                height,
                levels,
                crate::controller::CellStyle::Label { size, bold },
            )
        };

        // Heading: a back chevron and the panel title or the current error.
        let (top, height) = geometry.row(0);
        let (top, height) = (top as usize, height as usize);
        let heading = self.error.as_deref().unwrap_or(title);
        canvas.chevron(26, top + height / 2, false, 0);
        canvas.blit(
            44,
            top,
            &cell(heading, width - 44, height as u32, 24, true)?,
            width - 44,
            false,
        );

        for row in 1..=count {
            let (top, height) = geometry.row(row);
            let (top, height) = (top as usize, height as usize);
            let Some((label, command)) = entries.get(start + row - 1) else {
                if entries.is_empty() && row == 1 {
                    canvas.blit(
                        0,
                        top,
                        &cell("No entries", width, height as u32, 26, false)?,
                        width,
                        false,
                    );
                }
                continue;
            };
            let selected = selection == start + row - 1;
            let ink = if selected { 255 } else { 0 };
            if selected {
                canvas.fill(0, top, width, height - 1, 0);
            }
            let control = self.control(reader, command);
            let (text_width, text_height) = match control {
                Control::None => (width, height),
                Control::Submenu => (width - 48, height),
                Control::Toggle(_) => (width - 96, height),
                // The label sits above the slider track or the bar.
                Control::Slider { .. } | Control::Progress(_) => (width, height * 11 / 20),
            };
            canvas.blit(
                0,
                top,
                &cell(label, text_width, text_height as u32, 26, false)?,
                text_width,
                selected,
            );
            let middle = top + height / 2;
            match control {
                Control::None => {}
                Control::Submenu => canvas.chevron(width - 28, middle, true, ink),
                Control::Toggle(on) => canvas.toggle(width - 20 - 56, middle - 14, on, ink),
                Control::Progress(percent) => {
                    let (y0, y1) = (top + height * 12 / 20, top + height * 16 / 20);
                    let span = width - 40;
                    canvas.frame(20, y0, span, y1 - y0, ink);
                    canvas.fill(
                        20,
                        y0,
                        span * usize::from(percent.min(100)) / 100,
                        y1 - y0,
                        ink,
                    );
                }
                Control::Slider {
                    position,
                    positions,
                } => {
                    // One cell per value, matching the tap targets; the
                    // first cell stands for the lowest value and stays hollow.
                    let (y0, y1) = (top + height * 12 / 20, top + height * 16 / 20);
                    for step in 0..positions {
                        let (x0, x1) = (
                            step * width / positions + 3,
                            (step + 1) * width / positions - 3,
                        );
                        canvas.frame(x0, y0, x1 - x0, y1 - y0, ink);
                        if (1..=position).contains(&step) {
                            canvas.fill(x0, y0, x1 - x0, y1 - y0, ink);
                        }
                    }
                }
            }
        }

        // Footer: position among the pages of entries, with chevrons toward
        // the pages that exist. The left and right halves are the tap targets.
        let pages = entries.len().div_ceil(count).max(1);
        let page = start / count;
        if pages > 1 {
            let (top, height) = geometry.row(count + 1);
            let (top, height) = (top as usize, height as usize);
            let text = cell(
                &format!("{} / {pages}", page + 1),
                width,
                height as u32,
                22,
                false,
            )?;
            let columns = (0..width).filter(|x| (0..height).any(|y| text[y * width + x] < 255));
            let (left, right) = (
                columns.clone().min().unwrap_or(0),
                columns.max().unwrap_or(0),
            );
            // The cell is set from its left margin; copy its ink centered.
            let span = right + 1 - left;
            let offset = (width - span) / 2;
            for y in 0..height {
                let source = &text[y * width + left..][..span];
                canvas.pixels[(top + y) * width + offset..][..span].copy_from_slice(source);
            }
            if page > 0 {
                canvas.chevron(28, top + height / 2, false, 0);
            }
            if page + 1 < pages {
                canvas.chevron(width - 28, top + height / 2, true, 0);
            }
        }
        for row in 0..=count {
            let (top, height) = geometry.row(row);
            canvas.rule((top + height - 1) as usize);
        }
        Ok(canvas.pixels)
    }
}

/// Pixel drawing for menu rows.
struct Canvas {
    pixels: Vec<u8>,
    width: usize,
}

impl Canvas {
    fn fill(&mut self, x: usize, y: usize, width: usize, height: usize, value: u8) {
        for row in self
            .pixels
            .chunks_exact_mut(self.width)
            .skip(y)
            .take(height)
        {
            row[x..x + width].fill(value);
        }
    }
    fn frame(&mut self, x: usize, y: usize, width: usize, height: usize, value: u8) {
        self.fill(x, y, width, 2, value);
        self.fill(x, y + height - 2, width, 2, value);
        self.fill(x, y, 2, height, value);
        self.fill(x + width - 2, y, 2, height, value);
    }
    fn rule(&mut self, y: usize) {
        self.fill(0, y, self.width, 1, 0);
    }
    /// Copies a cell whose rows are `cell_width` wide, optionally inverted.
    fn blit(&mut self, x: usize, y: usize, cell: &[u8], cell_width: usize, invert: bool) {
        for (row, source) in cell.chunks_exact(cell_width).enumerate() {
            let start = (y + row) * self.width + x;
            for (destination, &value) in self.pixels[start..start + cell_width]
                .iter_mut()
                .zip(source)
            {
                *destination = if invert { 255 - value } else { value };
            }
        }
    }
    /// A chevron about 12 pixels tall centered at the point.
    fn chevron(&mut self, x: usize, y: usize, right: bool, value: u8) {
        for offset in 0..=6isize {
            // Distance from the tip grows toward both ends of the stroke.
            let reach = 6 - offset;
            let tip = if right {
                x as isize + 3 - reach
            } else {
                x as isize - 3 + reach
            };
            for dy in [-(6 - offset), 6 - offset] {
                let row = (y as isize + dy) as usize;
                self.fill((tip - 1).max(0) as usize, row, 3, 1, value);
            }
        }
    }
    /// A 56 by 28 switch: a hollow track with the knob left when off, a
    /// solid track with the knob right when on.
    fn toggle(&mut self, x: usize, y: usize, on: bool, ink: u8) {
        let paper = 255 - ink;
        self.fill(x, y, 56, 28, ink);
        if !on {
            self.fill(x + 2, y + 2, 52, 24, paper);
        }
        let knob = if on { x + 32 } else { x + 4 };
        self.fill(knob, y + 4, 20, 20, if on { paper } else { ink });
    }
}

fn excerpt_rows(text: &str) -> Vec<String> {
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            rows.push(String::new());
            continue;
        }
        let mut breaks = unicode_linebreak::linebreaks(line).peekable();
        let mut start = 0;
        while start < line.len() {
            let boundaries: Vec<_> = line[start..]
                .grapheme_indices(true)
                .take(33)
                .map(|(at, _)| start + at)
                .collect();
            let limit = boundaries.get(32).copied().unwrap_or(line.len());
            let mut end = None;
            while let Some(&(at, opportunity)) = breaks.peek() {
                if at > limit {
                    break;
                }
                breaks.next();
                if at > start && (at == line.len() || boundaries.contains(&at)) {
                    end = Some(at);
                    if opportunity == unicode_linebreak::BreakOpportunity::Mandatory {
                        break;
                    }
                }
            }
            let end = end.unwrap_or(limit);
            rows.push(line[start..end].to_owned());
            start = end;
        }
    }
    rows
}

#[cfg(test)]
mod excerpt_tests {
    use super::excerpt_rows;
    use unicode_segmentation::UnicodeSegmentation;

    #[test]
    fn prefers_line_breaks_and_retains_paragraph_spacing() {
        let text =
            "when the font changes. Words flow across without losing their source positions.";
        let rows = excerpt_rows(text);
        assert_eq!(rows.concat(), text);
        assert_eq!(rows[0], "when the font changes. Words ");
        assert!(rows[..rows.len() - 1].iter().all(|row| row.ends_with(' ')));
        assert_eq!(excerpt_rows("First.\n\nSecond."), ["First.", "", "Second."]);
        assert_eq!(
            excerpt_rows("First.\r\n\r\nSecond."),
            ["First.", "", "Second."]
        );
        assert_eq!(
            excerpt_rows("First.\u{2028}Second."),
            ["First.\u{2028}", "Second."]
        );
    }

    #[test]
    fn long_runs_fall_back_without_splitting_graphemes() {
        for cluster in ["x", "e\u{301}", "👩‍💻", "🇨🇭", "ש\u{5b8}"] {
            let text = cluster.repeat(65);
            let rows = excerpt_rows(&text);
            assert_eq!(rows.concat(), text);
            assert_eq!(
                rows.iter()
                    .map(|row| row.graphemes(true).count())
                    .collect::<Vec<_>>(),
                [32, 32, 1]
            );
            assert!(
                rows.iter()
                    .all(|row| row.graphemes(true).all(|g| g == cluster))
            );
        }
    }

    #[test]
    fn nonbreaking_space_is_kept_with_the_following_word() {
        let text = format!("{} word\u{a0}pair tail", "a".repeat(23));
        let rows = excerpt_rows(&text);
        assert_eq!(rows[0], format!("{} ", "a".repeat(23)));
        assert!(rows[1].starts_with("word\u{a0}pair"));
        assert_eq!(rows.concat(), text);
        assert!(rows.iter().all(|row| row.graphemes(true).count() <= 32));
    }
}
