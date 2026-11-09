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

#[derive(Clone, Copy)]
enum Panel {
    Root,
    Rotation,
    Catalog,
    CatalogSearch,
    Kosync,
    Tailnet,
    Contents,
    Chapters,
    Bookmarks,
    Clippings,
    Clipping(usize),
    ClippingDelete,
    SelectWord,
    WordPreview,
    DictionaryOpening,
    DictionaryResult,
    DictionaryChooser,
    WordList,
    Links,
    LinkPreview,
    PreviewOpening,
    Settings,
    Remote,
    Light,
    Power,
    Library,
    Books,
    Recent,
    Folders,
    Folder(usize),
    Search,
    Results,
    Scanning,
    Opening,
    Reflowing,
    Navigating,
    Errors,
    Diagnostic(usize),
}
#[derive(Clone, Copy)]
enum Command {
    Kosync(KosyncEvent),
    Catalog(CatalogEvent),
    Tailnet(TailnetEvent),
    Sleep,
    Rotation(crate::state::Rotation),
    IdleSleep(crate::state::IdleSleep),
    Resume,
    Rescan,
    CancelScan,
    CancelOpen,
    CancelPreview,
    CancelReflow,
    CancelNavigation,
    Noop,
    ClipPage,
    DeleteClipping(usize),
    ToggleHighlight(usize),
    ConfirmClippingDelete,
    CancelClippingDelete,
    SelectWord,
    SaveWord,
    LookupWord,
    ChooseDictionary(usize),
    CloseDictionary,
    SelectWordList,
    PickWord(crate::layout::Locator, crate::layout::Locator),
    ExtendWord,
    CancelWord,
    Character(char),
    Delete,
    Clear,
    Find,
    Open(Panel),
    Reader(Action),
    Size(f32),
    Hyphenation,
    ResetTypography,
    RemoteCycle(u32),
    RemoteDefaults,
    RemoteDisable,
    LineSpacing,
    Margin,
    Stroke,
    Gamma,
    Hinting,
    Fractional,
    Book(usize),
    Light { brightness: u8, warmth: u8 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogEvent {
    Open,
    Search,
    Activate(usize),
    Back,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KosyncEvent {
    Open,
    Push,
    Pull,
    Auth,
    Register,
    Back,
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
    kosync_available: bool,
    kosync_event: Option<KosyncEvent>,
    kosync_status: String,
    kosync_busy: bool,
    selected: usize,
    error: Option<String>,
    device_status: Option<String>,
    library: Library,
    folders: Vec<PathBuf>,
    light_request: bool,
    sleep_available: bool,
    sleep_request: bool,
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
        if matches!(
            self.panel,
            Some(Panel::DictionaryOpening | Panel::DictionaryResult)
        ) {
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
        if !matches!(self.panel, Some(Panel::DictionaryOpening)) {
            self.dictionary_job = None;
            return Effect::Unchanged;
        }
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
        match reader.begin_navigation(action)? {
            Some(job) => Ok(self.start_navigation(job)),
            None => reader.dispatch(action),
        }
    }
    fn start_navigation(&mut self, job: Navigating) -> Effect {
        self.navigation = Some(job);
        self.navigation_return = self.panel;
        self.panel = Some(Panel::Navigating);
        self.selected = 0;
        self.error = None;
        Effect::Redraw
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
        if !matches!(self.panel, Some(Panel::Navigating)) {
            if self.navigation.is_some()
                && matches!(self.navigation_return, Some(Panel::LinkPreview))
            {
                self.preview = None;
                self.preview_return = None;
            }
            self.navigation = None;
            self.navigation_return = None;
            return Effect::Unchanged;
        }
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
    fn edit_query(&mut self) -> &mut String {
        if matches!(self.panel, Some(Panel::CatalogSearch)) {
            &mut self.catalog_query
        } else {
            &mut self.query
        }
    }
    pub fn set_catalog_view(&mut self, title: &str, rows: &[String]) -> Result<()> {
        if rows.len() > 2048 {
            return Err(crate::Error::Limit("catalog menu rows"));
        }
        let clean = |text: &str| {
            text.chars()
                .filter(|c| !c.is_control())
                .take(256)
                .collect::<String>()
        };
        self.catalog_title = clean(title);
        self.catalog_rows = rows.iter().map(|row| clean(row)).collect();
        self.panel = Some(Panel::Catalog);
        self.selected = 0;
        self.error = None;
        Ok(())
    }

    pub fn set_sleep_available(&mut self, available: bool) {
        self.sleep_available = available;
    }
    pub fn take_sleep_request(&mut self) -> bool {
        std::mem::take(&mut self.sleep_request)
    }

    /// Updates a read-only root-menu status row without requesting a panel refresh.
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
        self.panel = Some(Panel::Scanning);
        self.selected = 0;
        self.error = None;
        Ok(Effect::Redraw)
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
        self.panel = Some(Panel::Reflowing);
        self.selected = 0;
        self.error = None;
        Effect::Redraw
    }
    fn cancel_reflow(&mut self) -> Effect {
        self.reflow = None;
        self.panel = self.reflow_return.take();
        self.selected = 0;
        Effect::Redraw
    }
    pub fn poll_reflow(&mut self, reader: &mut Reader) -> Effect {
        if !matches!(self.panel, Some(Panel::Reflowing)) {
            self.reflow = None;
            self.reflow_return = None;
            return Effect::Unchanged;
        }
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
                        self.panel = Some(Panel::Settings);
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
        if !matches!(self.panel, Some(Panel::Opening)) {
            self.opening = None;
            return Effect::Unchanged;
        }
        let Some(mut opening) = self.opening.take() else {
            return Effect::Unchanged;
        };
        let result = reader.step_open(&mut opening);
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
        match scan.step() {
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
        matches!(self.panel, Some(Panel::Search | Panel::CatalogSearch))
    }
    pub fn query(&self) -> &str {
        if matches!(self.panel, Some(Panel::CatalogSearch)) {
            &self.catalog_query
        } else {
            &self.query
        }
    }
    pub fn type_char(&mut self, character: char) -> Effect {
        if !self.is_editing()
            || character.is_control()
            || self.query().len() + character.len_utf8() > 64
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
        self.folders = library
            .entries
            .iter()
            .filter_map(|e| e.path.parent().map(|p| p.to_path_buf()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        self.library = library;
        self.selected = 0;
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
    pub fn back(&mut self, reader: &mut Reader) -> Result<Effect> {
        match self.panel {
            Some(Panel::DictionaryOpening | Panel::DictionaryResult) => {
                if let Some(dictionary) = self.dictionary_job.take().filter(|d| d.is_ready()) {
                    self.dictionary_cache = Some(dictionary);
                }
                self.dictionary_rows.clear();
                self.panel = Some(Panel::WordPreview);
                self.selected = 0;
                self.error = None;
                Ok(Effect::Redraw)
            }
            Some(Panel::SelectWord | Panel::WordPreview | Panel::WordList) => {
                self.word_origin = None;
                self.word_pending = None;
                self.word_choices.clear();
                self.panel = Some(Panel::Clippings);
                self.selected = 0;
                self.error = None;
                Ok(Effect::Redraw)
            }
            Some(Panel::ClippingDelete) => {
                self.clipping_delete = None;
                self.panel = Some(Panel::Clippings);
                self.selected = 0;
                self.error = None;
                Ok(Effect::Redraw)
            }
            Some(Panel::Clipping(_)) => {
                self.panel = Some(Panel::Clippings);
                self.selected = 0;
                Ok(Effect::Redraw)
            }
            Some(Panel::Navigating) => Ok(self.cancel_navigation(reader)),
            Some(Panel::PreviewOpening) => Ok(self.cancel_preview()),
            Some(Panel::LinkPreview) => {
                self.preview = None;
                self.panel = self.preview_return.take();
                self.error = None;
                Ok(Effect::Redraw)
            }
            Some(Panel::Catalog | Panel::CatalogSearch) => {
                self.catalog_event = Some(CatalogEvent::Back);
                Ok(Effect::Redraw)
            }
            Some(Panel::Kosync) => {
                self.kosync_event = Some(KosyncEvent::Back);
                Ok(Effect::Redraw)
            }
            Some(Panel::Reflowing) => Ok(self.cancel_reflow()),
            Some(Panel::Opening) => {
                self.opening = None;
                self.open_library();
                Ok(Effect::Redraw)
            }
            Some(Panel::Scanning) => {
                self.scan = None;
                self.open_library();
                Ok(Effect::Redraw)
            }
            Some(Panel::Diagnostic(_)) => {
                self.panel = Some(Panel::Errors);
                self.selected = 0;
                Ok(Effect::Redraw)
            }
            None => self.navigate(reader, Action::Back),
            Some(Panel::Root) => {
                self.panel = None;
                self.error = None;
                Ok(Effect::Redraw)
            }
            Some(Panel::Books | Panel::Recent | Panel::Folders | Panel::Search | Panel::Errors) => {
                self.open_library();
                Ok(Effect::Redraw)
            }
            Some(Panel::Results) => {
                self.panel = Some(Panel::Search);
                self.selected = 0;
                self.error = None;
                Ok(Effect::Redraw)
            }
            Some(Panel::Folder(_)) => {
                self.panel = Some(Panel::Folders);
                self.selected = 0;
                self.error = None;
                Ok(Effect::Redraw)
            }
            Some(_) => {
                self.open();
                Ok(Effect::Redraw)
            }
        }
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
        self.panel = Some(Panel::Root);
        self.selected = 0;
        self.error = None;
    }
    pub fn turn(&mut self, reader: &mut Reader, next: bool) -> Result<Effect> {
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
        let count = self.entries(reader).len();
        let previous = self.selected;
        self.selected = if next {
            (self.selected + 1).min(count.saturating_sub(1))
        } else {
            self.selected.saturating_sub(1)
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
        if !matches!(self.panel, Some(Panel::PreviewOpening)) {
            self.preview_opening = None;
            return Effect::Unchanged;
        }
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
        self.panel = Some(Panel::PreviewOpening);
        Ok(Effect::Redraw)
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
        let entries = self.entries(reader);
        if self.is_editing() {
            geometry.keyboard()?;
            let Some(index) = geometry.key_at(x, row) else {
                return Ok(Effect::Unchanged);
            };
            self.selected = index;
            return self.activate(reader, entries[index].1);
        }
        if row == geometry.rows - 1 {
            let page = self.selected / geometry.items;
            let next = if x < layout.width / 2 {
                page.saturating_sub(1)
            } else {
                (page + 1).min(entries.len().saturating_sub(1) / geometry.items)
            };
            self.selected = next * geometry.items;
            return Ok(Effect::Redraw);
        }
        let index = (self.selected / geometry.items) * geometry.items + row - 1;
        let Some((_, command)) = entries.get(index) else {
            return Ok(Effect::Unchanged);
        };
        self.selected = index;
        self.activate(reader, *command)
    }
    pub fn select(&mut self, reader: &mut Reader) -> Result<Effect> {
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
        let entries = self.entries(reader);
        match entries.get(self.selected) {
            Some((_, command)) => self.activate(reader, *command),
            None => Ok(Effect::Unchanged),
        }
    }
    fn activate(&mut self, reader: &mut Reader, command: Command) -> Result<Effect> {
        self.error = None;
        let result = match command {
            Command::Kosync(event) => {
                self.kosync_event = Some(event);
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
                self.panel = Some(Panel::DictionaryOpening);
                self.selected = 0;
                Ok(Effect::Redraw)
            })(),
            Command::CloseDictionary => self.back(reader),
            Command::CancelWord => self.back(reader),
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
            Command::CancelClippingDelete => self.back(reader),
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
            Command::CancelScan => self.back(reader),
            Command::Rescan => self.rescan_library(),
            Command::Character(character) => Ok(self.type_char(character)),
            Command::Delete => Ok(self.delete_char()),
            Command::Clear => {
                self.edit_query().clear();
                Ok(Effect::Redraw)
            }
            Command::RemoteCycle(usage) => {
                let mut bindings = reader.state.remote_bindings.clone().unwrap_or_default();
                bindings.cycle(usage).map(|()| {
                    reader.state.remote_bindings = Some(bindings);
                    self.selected = self
                        .selected
                        .min(self.entries(reader).len().saturating_sub(1));
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
            Command::IdleSleep(timeout) => {
                reader.state.idle_sleep = timeout;
                Ok(Effect::Redraw)
            }
            Command::Sleep => {
                self.sleep_request = true;
                Ok(Effect::Unchanged)
            }
            Command::Find => Ok(self.submit_search()),
            Command::Light { brightness, warmth } => {
                self.light_request = true;
                reader.state.frontlight = crate::state::FrontlightSettings { brightness, warmth };
                Ok(Effect::Redraw)
            }
            Command::Book(index) => match self.library.entries.get(index) {
                Some(entry) => reader.begin_open_file(&entry.path).map(|opening| {
                    self.opening = Some(opening);
                    self.panel = Some(Panel::Opening);
                    self.selected = 0;
                    Effect::Redraw
                }),
                None => Err(crate::Error::Document("library entry unavailable".into())),
            },
            Command::CancelReflow => Ok(self.cancel_reflow()),
            Command::CancelNavigation => Ok(self.cancel_navigation(reader)),
            Command::CancelPreview => Ok(self.cancel_preview()),
            Command::CancelOpen => {
                self.opening = None;
                self.open_library();
                Ok(Effect::Redraw)
            }
            Command::Resume => {
                self.panel = None;
                Ok(Effect::Redraw)
            }
            Command::Open(panel) => {
                self.panel = Some(panel);
                self.selected = 0;
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
            Command::Size(delta) => self.resize(reader, delta),
            Command::ResetTypography => self.typography(
                reader,
                crate::state::BookTypography::current(&crate::state::ReaderState::default()),
            ),
            Command::Hyphenation => {
                let mut next = reader.state.layout;
                next.hyphenation = next.hyphenation.next();
                self.reflow(reader, next)
            }
            Command::LineSpacing => {
                let current = reader.state.layout;
                let values = [1.0, 1.15, 1.35, 1.6, 2.0];
                let next = values
                    .into_iter()
                    .filter(|value| *value > current.line_height)
                    .chain(
                        values
                            .into_iter()
                            .filter(|value| *value <= current.line_height),
                    )
                    .map(|line_height| crate::layout::LayoutConfig {
                        line_height,
                        ..current
                    })
                    .find(|config| config.validate().is_ok())
                    .unwrap_or(current);
                self.reflow(reader, next)
            }
            Command::Margin => {
                let current = reader.state.layout;
                let values = [0, 8, 16, 24, 32, 40, 48];
                let next = values
                    .into_iter()
                    .filter(|value| *value > current.margin)
                    .chain(values.into_iter().filter(|value| *value <= current.margin))
                    .map(|margin| crate::layout::LayoutConfig { margin, ..current })
                    .find(|config| config.validate().is_ok())
                    .unwrap_or(current);
                self.reflow(reader, next)
            }
            Command::Stroke => {
                let value = reader.state.text_raster.stroke;
                reader.state.text_raster.stroke = if value < 0.1 {
                    0.2
                } else if value < 0.3 {
                    0.4
                } else {
                    0.0
                };
                Ok(Effect::Redraw)
            }
            Command::Gamma => {
                let value = reader.state.text_raster.coverage_gamma;
                reader.state.text_raster.coverage_gamma = if value > 0.9 {
                    0.85
                } else if value > 0.75 {
                    0.7
                } else {
                    1.0
                };
                Ok(Effect::Redraw)
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
                    ]);
                }
                rows.push((
                    if self.kosync_busy { "Cancel" } else { "Back" }.into(),
                    Command::Kosync(KosyncEvent::Back),
                ));
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
            Panel::Root => vec![
                ("Resume reading".into(), Command::Resume),
                ("Contents".into(), Command::Open(Panel::Contents)),
                ("Chapters".into(), Command::Open(Panel::Chapters)),
                ("Bookmarks".into(), Command::Open(Panel::Bookmarks)),
                (
                    "Toggle bookmark".into(),
                    Command::Reader(Action::ToggleBookmark),
                ),
                ("Links on this page".into(), Command::Open(Panel::Links)),
                ("Typography".into(), Command::Open(Panel::Settings)),
                ("Return from jump".into(), Command::Reader(Action::Back)),
                ("Library".into(), Command::Open(Panel::Library)),
                ("Frontlight".into(), Command::Open(Panel::Light)),
            ]
            .into_iter()
            .chain(
                self.kosync_available
                    .then(|| ("KOReader sync".into(), Command::Kosync(KosyncEvent::Open))),
            )
            .chain(
                self.catalog_available
                    .then(|| ("Catalog".into(), Command::Catalog(CatalogEvent::Open))),
            )
            .chain(
                self.device_status
                    .iter()
                    .map(|status| (status.clone(), Command::Noop)),
            )
            .chain(
                self.sleep_available
                    .then(|| ("Save and sleep".into(), Command::Sleep)),
            )
            .chain(
                self.sleep_available
                    .then(|| ("Automatic sleep".into(), Command::Open(Panel::Power))),
            )
            .chain(
                self.tailnet_available
                    .then(|| ("Tailnet".into(), Command::Tailnet(TailnetEvent::Open))),
            )
            .chain(
                self.rotation_available
                    .then(|| ("Screen orientation".into(), Command::Open(Panel::Rotation))),
            )
            .chain(std::iter::once((
                "Clippings".into(),
                Command::Open(Panel::Clippings),
            )))
            .chain(
                (!self.dictionary_choices.is_empty())
                    .then(|| ("Dictionary".into(), Command::Open(Panel::DictionaryChooser))),
            )
            .chain(std::iter::once((
                "Remote buttons".into(),
                Command::Open(Panel::Remote),
            )))
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
            Panel::DictionaryOpening => vec![("Cancel lookup".into(), Command::CloseDictionary)],
            Panel::DictionaryResult => {
                std::iter::once(("Back to selection".into(), Command::CloseDictionary))
                    .chain(
                        self.dictionary_rows
                            .iter()
                            .map(|row| (row.clone(), Command::Noop)),
                    )
                    .collect()
            }
            Panel::WordPreview => vec![
                ("Cancel".into(), Command::CancelWord),
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
                ("Keep excerpt".into(), Command::CancelClippingDelete),
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
            ],
            Panel::Reflowing => vec![("Cancel reflow".into(), Command::CancelReflow)],
            Panel::Navigating => vec![("Cancel jump".into(), Command::CancelNavigation)],
            Panel::PreviewOpening => vec![("Cancel preview".into(), Command::CancelPreview)],
            Panel::Opening => vec![("Cancel opening".into(), Command::CancelOpen)],
            Panel::Scanning => vec![("Cancel scan".into(), Command::CancelScan)],
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
            Panel::Search | Panel::CatalogSearch => "abcdefghijklmnopqrstuvwxyz0123456789"
                .chars()
                .map(|c| (c.to_string(), Command::Character(c)))
                .chain([
                    ("Delete".into(), Command::Delete),
                    ("Space".into(), Command::Character(' ')),
                    ("Clear".into(), Command::Clear),
                    ("Find".into(), Command::Find),
                ])
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
            Panel::Books | Panel::Recent | Panel::Folder(_) | Panel::Results => {
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
                        (label, Command::Book(i))
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
            Panel::Bookmarks => reader
                .state
                .books
                .get(&reader.document.metadata.id)
                .map(|book| {
                    book.bookmarks
                        .iter()
                        .enumerate()
                        .map(|(i, b)| (b.label.clone(), Command::Reader(Action::Bookmark(i))))
                        .collect()
                })
                .unwrap_or_default(),
            Panel::LinkPreview => Vec::new(),
            Panel::Links => reader
                .page_links()
                .iter()
                .enumerate()
                .map(|(i, l)| (l.label.clone(), Command::Reader(Action::Link(i))))
                .collect(),
            Panel::Light => {
                let crate::state::FrontlightSettings { brightness, warmth } =
                    reader.state.frontlight;
                vec![
                    (
                        format!("Brighter ({brightness}/10)"),
                        Command::Light {
                            brightness: brightness.saturating_add(1).min(10),
                            warmth,
                        },
                    ),
                    (
                        format!("Dimmer ({brightness}/10)"),
                        Command::Light {
                            brightness: brightness.saturating_sub(1),
                            warmth,
                        },
                    ),
                    (
                        format!("Warmer ({warmth}/9)"),
                        Command::Light {
                            brightness,
                            warmth: warmth.saturating_add(1).min(9),
                        },
                    ),
                    (
                        format!("Cooler ({warmth}/9)"),
                        Command::Light {
                            brightness,
                            warmth: warmth.saturating_sub(1),
                        },
                    ),
                    (
                        "Off".into(),
                        Command::Light {
                            brightness: 0,
                            warmth,
                        },
                    ),
                ]
            }
            Panel::Settings => vec![
                (
                    format!("Larger text ({:.0} px)", reader.state.layout.font_size),
                    Command::Size(2.0),
                ),
                (
                    format!("Smaller text ({:.0} px)", reader.state.layout.font_size),
                    Command::Size(-2.0),
                ),
                (
                    format!("Stroke: {:.1} px", reader.state.text_raster.stroke),
                    Command::Stroke,
                ),
                (
                    format!("Coverage: {:.2}", reader.state.text_raster.coverage_gamma),
                    Command::Gamma,
                ),
                (
                    format!("Hinting: {}", reader.state.text_raster.hinting),
                    Command::Hinting,
                ),
                (
                    format!("Fractional: {}", reader.state.text_raster.fractional),
                    Command::Fractional,
                ),
                (
                    format!("Auto hyphens: {}", reader.state.layout.hyphenation.label()),
                    Command::Hyphenation,
                ),
                ("Reset book typography".into(), Command::ResetTypography),
                (
                    format!("Line spacing: {:.2}", reader.state.layout.line_height),
                    Command::LineSpacing,
                ),
                (
                    format!("Margins: {} px", reader.state.layout.margin),
                    Command::Margin,
                ),
            ],
        }
    }
    fn render_keyboard(&self, reader: &Reader, levels: u16) -> Result<Vec<u8>> {
        let layout = reader.state.layout;
        let geometry = crate::menu::Geometry::new(layout.width, layout.height)?;
        geometry.keyboard()?;
        let tail: String = self
            .query()
            .graphemes(true)
            .rev()
            .take(24)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let heading = self
            .error
            .as_ref()
            .map(|error| format!("< Back | {error}"))
            .unwrap_or_else(|| format!("< Back | {tail}_"));
        let heading = heading.graphemes(true).take(32).collect::<String>();
        let mut pixels = reader.render_rows(&[heading], None, levels)?;
        for (index, (label, _)) in self.entries(reader).iter().enumerate() {
            let (row, x, width) = geometry.key(index);
            let (top, height) = geometry.row(row);
            let cell = reader.render_cell(label, width, height, levels)?;
            for y in 0..height {
                for dx in 0..width {
                    let value = cell[(y * width + dx) as usize];
                    pixels[((top + y) * layout.width + x + dx) as usize] =
                        if y == height - 1 || dx == width - 1 {
                            0
                        } else if self.selected == index {
                            255 - value
                        } else {
                            value
                        };
                }
            }
        }
        Ok(pixels)
    }
    pub fn render(&self, reader: &mut Reader, levels: u16) -> Result<Vec<u8>> {
        let Some(panel) = self.panel else {
            return reader.render(levels);
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
            Panel::Reflowing => "Reflowing pages",
            Panel::Navigating => "Opening chapter",
            Panel::Opening => "Opening book",
            Panel::PreviewOpening => "Opening note preview",
            Panel::Scanning => "Scanning library",
            Panel::Errors => "Scan errors",
            Panel::Diagnostic(_) => "Error details",
            Panel::Search | Panel::CatalogSearch => "Search",
            Panel::Results => "Search results",
            Panel::Root => &reader_title,
            Panel::Rotation => "Screen orientation",
            Panel::Tailnet => "Tailnet",
            Panel::Catalog => &self.catalog_title,
            Panel::Kosync => "KOReader sync",
            Panel::Contents => "Contents",
            Panel::Chapters => "Chapters",
            Panel::Bookmarks => "Bookmarks",
            Panel::Clippings => "Clippings",
            Panel::Clipping(_) => "Saved excerpt",
            Panel::ClippingDelete => "Delete excerpt?",
            Panel::SelectWord => "Select word",
            Panel::WordPreview => "Selected text",
            Panel::DictionaryOpening => "Preparing dictionary",
            Panel::DictionaryResult => "Dictionary",
            Panel::DictionaryChooser => "Choose dictionary",
            Panel::WordList => "Choose source word",
            Panel::Links => "Page links",
            Panel::LinkPreview => "Link preview",
            Panel::Settings => "Typography",
            Panel::Remote => "Remote buttons",
            Panel::Light => "Frontlight",
            Panel::Power => "Automatic sleep",
            Panel::Library if self.library.truncated => "Library (scan limit)",
            Panel::Library => "Library",
            Panel::Books => "All books",
            Panel::Recent => "Recent books",
            Panel::Folders | Panel::Folder(_) => "Folders",
        };
        let entries = self.entries(reader);
        let geometry =
            crate::menu::Geometry::new(reader.state.layout.width, reader.state.layout.height)?;
        let count = geometry.items;
        let start = self.selected / count * count;
        let mut rows = vec![
            self.error
                .clone()
                .unwrap_or_else(|| format!("< Back | {title}")),
        ];
        for i in start..start + count {
            rows.push(
                entries
                    .get(i)
                    .map(|(label, _)| label.clone())
                    .unwrap_or_default(),
            );
        }
        if entries.is_empty() {
            rows[1] = "No entries".into();
        }
        rows.push(format!(
            "< Previous | {}/{} | Next >",
            start / count + 1,
            entries.len().div_ceil(count).max(1)
        ));
        for row in &mut rows {
            *row = row.graphemes(true).take(32).collect();
        }
        reader.render_rows(
            &rows,
            (!entries.is_empty()).then_some(self.selected % count + 1),
            levels,
        )
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
