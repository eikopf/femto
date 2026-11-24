//! Reader transitions shared by terminal, desktop, and device frontends.

use crate::{
    Error, Result,
    document::{Block, Chapter, Document, DocumentLink, Paragraph, Span, TextStyle},
    layout::{
        Fonts, LayoutConfig, Locator, Page, paginate, rasterize_display, rasterize_display_report,
    },
    page_index::IndexStore,
    pagination::{PageCache, PageIndexer, Record},
    state::{BookTypography, ReaderState},
};
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Next,
    Previous,
    Chapter(usize),
    Contents(usize),
    Link(usize),
    Bookmark(usize),
    Clipping(usize),
    /// Go to the given percentage of the book.
    Progress(u8),
    ToggleBookmark,
    Back,
    Reflow(LayoutConfig),
    Rotate(crate::state::Rotation),
    Tap {
        x: u32,
        y: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Unchanged,
    Redraw,
    Menu,
}

pub struct Reader {
    pub document: Document,
    pub state: ReaderState,
    fonts: Fonts,
    pagination: PageCache,
    position: Locator,
    history: Vec<Locator>,
    links: Vec<DocumentLink>,
    /// Rendered reading pages, so prepared and recently shown pages need no rasterization.
    frames: Vec<(FrameKey, Vec<u8>)>,
    /// Rendered menu cells by text, size and levels, most recently used last.
    /// Menus redraw mostly unchanged rows on every selection change.
    cells: std::sync::Mutex<Vec<(CellKey, Vec<u8>)>>,
    index_store: Option<IndexStore>,
    /// Index of the following chapter, built while idle: the pagination it
    /// follows and the job.
    ahead: Option<(u64, PageIndexer)>,
}

/// Indexes a chapter for display: recorded page bounds are reused, and
/// otherwise the job finishes as soon as the target page is known.
fn chapter_indexer(
    store: Option<&IndexStore>,
    fonts: &Fonts,
    book: &str,
    chapter: std::sync::Arc<Chapter>,
    config: LayoutConfig,
    target: Locator,
) -> Result<PageIndexer> {
    let recorded = store.and_then(|store| store.load(book, fonts, config, chapter.index));
    let indexer = PageIndexer::from_shared(chapter, config, target)?.partial();
    Ok(match recorded {
        Some(pages) => indexer.seeded(pages),
        None => indexer,
    })
}

type CellKey = (String, u32, u32, u16, CellStyle);

/// The block that starts nearest below the given share of a chapter. Blocks
/// stand in for length, which is only known once the chapter is laid out.
fn block_at(chapter: &Chapter, share: f64) -> usize {
    ((chapter.blocks.len() as f64 * share) as usize).min(chapter.blocks.len().saturating_sub(1))
}

/// How a menu cell's text is set.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CellStyle {
    /// Small text from the top-left corner, for dense grids and status lines.
    Compact,
    /// A label of the given pixel size, centered vertically.
    Label { size: u32, bold: bool },
}

/// Everything a rendered reading page depends on besides the fonts, which are
/// fixed for a reader. A pagination belongs to one chapter of one document.
#[derive(Clone, Copy, PartialEq)]
struct FrameKey {
    pages: u64,
    index: usize,
    raster: crate::display::TextRaster,
    levels: u16,
    highlights: u64,
}

pub struct LinkPreview {
    source_chapter: std::sync::Arc<Chapter>,
    pagination: PageCache,
    page_index: usize,
    target: Locator,
    origin: String,
    origin_position: Locator,
    origin_layout: LayoutConfig,
    config: LayoutConfig,
}

impl LinkPreview {
    pub fn page(&self) -> &Page {
        self.pagination.page()
    }

    pub fn page_index(&self) -> usize {
        self.page_index
    }

    pub fn page_count(&self) -> usize {
        self.pagination.bounds().len()
    }
}

pub struct PreviewOpening {
    source_chapter: std::sync::Arc<Chapter>,
    indexer: PageIndexer,
    target: Locator,
    origin: String,
    origin_position: Locator,
    origin_layout: LayoutConfig,
    config: LayoutConfig,
}

impl PreviewOpening {
    pub fn indexed_pages(&self) -> usize {
        self.indexer.page_count()
    }
}

pub struct Reflowing {
    indexer: PageIndexer,
    origin: String,
    position: Locator,
    previous: LayoutConfig,
    next: LayoutConfig,
    previous_rotation: crate::state::Rotation,
    next_rotation: crate::state::Rotation,
    previous_raster: crate::display::TextRaster,
    next_raster: crate::display::TextRaster,
    /// Replacement fonts and the family name saved with them.
    fonts: Option<(Fonts, Option<String>)>,
}

pub struct Navigating {
    clipping: Option<crate::clipping::Clipping>,
    indexer: PageIndexer,
    target: Locator,
    origin: String,
    position: Locator,
    layout: LayoutConfig,
    rotation: crate::state::Rotation,
    history: Vec<Locator>,
    remember: bool,
    back: bool,
    links: Vec<DocumentLink>,
}

pub struct BookOpening {
    phase: OpeningPhase,
    origin: String,
    origin_position: Locator,
    config: LayoutConfig,
}
enum OpeningPhase {
    Hashing {
        path: std::path::PathBuf,
        file: crate::document::FileHash,
        cache_root: Option<(std::path::PathBuf, usize)>,
    },
    Cached {
        path: std::path::PathBuf,
        file: Option<crate::document::IdentifiedFile>,
        stamp: Option<crate::library_cache::FileStamp>,
        root: std::path::PathBuf,
        limit: usize,
    },
    Indexing(Box<IndexedOpening>),
}
impl OpeningPhase {
    fn cached(path: &std::path::Path, root: &std::path::Path, limit: usize) -> Result<Self> {
        let root = root.canonicalize()?;
        let crate::library_cache::OpenedBook {
            path,
            file,
            identity,
        } = crate::library_cache::Cache::open_book(&root, path, limit)?;
        Ok(match identity {
            Some((id, stamp)) => Self::Cached {
                path,
                stamp: Some(stamp),
                file: Some(crate::document::IdentifiedFile::cached(file, id)),
                root,
                limit,
            },
            None => Self::Hashing {
                path,
                file: crate::document::FileHash::new(file),
                cache_root: Some((root, limit)),
            },
        })
    }
    fn read_document(&mut self, budget: std::time::Duration) -> Result<Option<Document>> {
        match self {
            Self::Cached {
                path,
                file,
                stamp,
                root,
                limit,
            } => {
                let file = file
                    .take()
                    .ok_or_else(|| Error::Document("cached opening already consumed".into()))?;
                if file.stamp() != *stamp {
                    *self = Self::Hashing {
                        path: path.clone(),
                        file: file.into_hash(),
                        cache_root: Some((root.clone(), *limit)),
                    };
                    return self.read_document(budget);
                }
                Ok(Some(Document::from_identified(path, file)?))
            }
            Self::Hashing {
                path,
                file,
                cache_root,
            } => {
                let Some(file) = file.step_for(budget)? else {
                    return Ok(None);
                };
                let stamp = file.stamp();
                let document = Document::from_identified(path, file)?;
                if let Some((root, limit)) = cache_root {
                    // This index is disposable; failure to persist it must not
                    // prevent reading the successfully opened document.
                    let _ = crate::library_cache::Cache::record_open(
                        root,
                        path,
                        &document.metadata,
                        stamp,
                        *limit,
                    );
                }
                Ok(Some(document))
            }
            Self::Indexing(_) => Ok(None),
        }
    }
}

struct IndexedOpening {
    typography: BookTypography,
    document: Document,
    indexer: PageIndexer,
    position: Locator,
    links: Vec<DocumentLink>,
}

/// Startup owns its inputs; dropping it cancels without writing reader state.
pub struct ReaderStartup {
    phase: OpeningPhase,
    fonts: Fonts,
    state: ReaderState,
    index_store: Option<IndexStore>,
}

impl ReaderStartup {
    pub fn from_file(
        path: impl AsRef<std::path::Path>,
        fonts: Fonts,
        state: ReaderState,
    ) -> Result<Self> {
        state.layout.validate()?;
        let path = path.as_ref().to_owned();
        let file = crate::document::FileHash::new(std::fs::File::open(&path)?);
        Ok(Self {
            phase: OpeningPhase::Hashing {
                path,
                file,
                cache_root: None,
            },
            fonts,
            state,
            index_store: None,
        })
    }

    /// Reuses a cached identity only when the opened file's size and mtime match.
    /// A miss hashes cooperatively and refreshes the disposable metadata cache.
    pub fn from_cached_file(
        path: impl AsRef<std::path::Path>,
        root: impl AsRef<std::path::Path>,
        limit: usize,
        fonts: Fonts,
        state: ReaderState,
    ) -> Result<Self> {
        state.layout.validate()?;
        Ok(Self {
            phase: OpeningPhase::cached(path.as_ref(), root.as_ref(), limit)?,
            fonts,
            state,
            index_store: None,
        })
    }

    pub fn open_cached_file(
        &mut self,
        path: impl AsRef<std::path::Path>,
        root: impl AsRef<std::path::Path>,
        limit: usize,
    ) -> Result<()> {
        self.phase = OpeningPhase::cached(path.as_ref(), root.as_ref(), limit)?;
        Ok(())
    }

    pub fn from_document(document: Document, fonts: Fonts, state: ReaderState) -> Result<Self> {
        Ok(Self {
            phase: OpeningPhase::Indexing(Box::new(prepare_opening(
                document, &state, &fonts, None,
            )?)),
            fonts,
            state,
            index_store: None,
        })
    }

    /// Replaces the pending book only after the new file handle opens.
    pub fn open_file(&mut self, path: impl AsRef<std::path::Path>) -> Result<()> {
        let path = path.as_ref().to_owned();
        let file = crate::document::FileHash::new(std::fs::File::open(&path)?);
        self.phase = OpeningPhase::Hashing {
            path,
            file,
            cache_root: None,
        };
        Ok(())
    }

    /// Records and reuses chapter page indexes; applies to books opened afterwards.
    pub fn set_index_store(&mut self, store: Option<IndexStore>) {
        self.index_store = store;
    }

    pub fn cancel(self) -> (Fonts, ReaderState) {
        (self.fonts, self.state)
    }

    /// A blank page with the busy mark, shown while the first book loads
    /// and no earlier view exists to keep on screen.
    pub fn loading_frame(&self, levels: u16) -> Result<Vec<u8>> {
        if !matches!(levels, 2 | 4 | 256) {
            return Err(Error::Layout("display levels must be 2, 4, or 256"));
        }
        let layout = self.state.layout;
        layout.validate()?;
        let mut pixels = vec![255; layout.width as usize * layout.height as usize];
        crate::display::busy_mark(&mut pixels, layout.width as usize);
        Ok(pixels)
    }

    pub fn indexed_pages(&self) -> usize {
        match &self.phase {
            OpeningPhase::Hashing { .. } | OpeningPhase::Cached { .. } => 0,
            OpeningPhase::Indexing(job) => job.indexer.page_count(),
        }
    }

    /// Hashes one file chunk or indexes one page. Metadata and chapter parsing
    /// occur synchronously when hashing completes.
    pub fn step(&mut self) -> Result<bool> {
        self.step_for(std::time::Duration::ZERO)
    }

    /// Batches hashing within the budget; indexing still processes one page.
    /// Metadata and chapter parsing at EOF can exceed the budget.
    pub fn step_for(&mut self, budget: std::time::Duration) -> Result<bool> {
        if let OpeningPhase::Indexing(job) = &mut self.phase {
            return job.indexer.step(&self.fonts);
        }
        if let Some(document) = self.phase.read_document(budget)? {
            self.phase = OpeningPhase::Indexing(Box::new(prepare_opening(
                document,
                &self.state,
                &self.fonts,
                self.index_store.as_ref(),
            )?));
        }
        Ok(false)
    }

    pub fn finish(mut self) -> Result<Reader> {
        let OpeningPhase::Indexing(job) = self.phase else {
            return Err(Error::Document("startup hashing not completed".into()));
        };
        let IndexedOpening {
            typography,
            document,
            indexer,
            position,
            links,
        } = *job;
        let pagination = indexer.finish()?;
        let position = normalize_position(&pagination, position);
        typography.apply(&mut self.state);
        Ok(Reader {
            document,
            fonts: self.fonts,
            state: self.state,
            pagination,
            position,
            history: Vec::new(),
            links,
            frames: Vec::new(),
            cells: Default::default(),
            index_store: self.index_store,
            ahead: None,
        })
    }
}

fn prepare_opening(
    mut document: Document,
    state: &ReaderState,
    fonts: &Fonts,
    store: Option<&IndexStore>,
) -> Result<IndexedOpening> {
    if document.metadata.chapters.is_empty() {
        return Err(Error::Document("book has no chapters".into()));
    }
    let mut position = state
        .books
        .get(&document.metadata.id)
        .map(|book| book.position)
        .unwrap_or_default();
    if position.chapter >= document.document_count() {
        position = Locator::default();
    }
    let typography = state.typography_for(&document.metadata.id);
    typography.text_raster.validate()?;
    let config = typography.layout(state.layout);
    let chapter = document.chapter(position.chapter)?;
    let links = chapter.links.clone();
    let indexer = chapter_indexer(
        store,
        fonts,
        &document.metadata.id,
        std::sync::Arc::new(chapter),
        config,
        position,
    )?;
    Ok(IndexedOpening {
        typography,
        document,
        position,
        links,
        indexer,
    })
}

impl BookOpening {
    pub fn indexed_pages(&self) -> usize {
        match &self.phase {
            OpeningPhase::Hashing { .. } | OpeningPhase::Cached { .. } => 0,
            OpeningPhase::Indexing(opening) => opening.indexer.page_count(),
        }
    }
}

impl Reader {
    pub fn new(mut document: Document, fonts: Fonts, mut state: ReaderState) -> Result<Self> {
        if document.metadata.chapters.is_empty() {
            return Err(Error::Document("book has no chapters".into()));
        }
        let mut position = state
            .books
            .get(&document.metadata.id)
            .map(|book| book.position)
            .unwrap_or_default();
        if position.chapter >= document.document_count() {
            position = Locator::default();
        }
        state
            .typography_for(&document.metadata.id)
            .apply(&mut state);
        state.text_raster.validate()?;
        let chapter = document.chapter(position.chapter)?;
        let links = chapter.links.clone();
        let pagination = PageCache::new(chapter, &fonts, state.layout, position)?;
        position = normalize_position(&pagination, position);
        Ok(Self {
            document,
            fonts,
            state,
            pagination,
            position,
            history: Vec::new(),
            links,
            frames: Vec::new(),
            cells: Default::default(),
            index_store: None,
            ahead: None,
        })
    }

    pub fn page(&self) -> &Page {
        self.pagination.page()
    }

    /// Records and reuses chapter page indexes from now on.
    pub fn set_index_store(&mut self, store: Option<IndexStore>) {
        self.index_store = store;
    }

    /// Whether every page of the visible chapter is indexed; until then
    /// [`Self::page_count`] grows.
    pub fn is_indexed(&self) -> bool {
        self.pagination.is_complete()
    }

    /// One bounded step of background indexing: the rest of the visible
    /// chapter, then a record of it, then the following chapter so that
    /// crossing into it needs no indexing. Returns whether a step ran.
    pub fn index_step(&mut self) -> Result<bool> {
        if self.pagination.extend(&self.fonts)? {
            return Ok(true);
        }
        let Some(store) = self.index_store.clone() else {
            return Ok(false);
        };
        let book = self.document.metadata.id.clone();
        let chapter = self.pagination.chapter();
        if self.pagination.record == Record::Unwritten {
            // A failed write is not retried on every idle poll.
            self.pagination.record = Record::Written;
            store.store(
                &book,
                &self.fonts,
                self.pagination.config(),
                chapter.index,
                self.pagination.bounds(),
            )?;
            return Ok(true);
        }
        let config = self.pagination.config();
        let id = self.pagination.id();
        if self.ahead.as_ref().is_some_and(|(owner, _)| *owner != id) {
            self.ahead = None;
        }
        let next = chapter.index + 1;
        let Some((_, job)) = &mut self.ahead else {
            if next >= self.document.metadata.chapters.len()
                || store.load(&book, &self.fonts, config, next).is_some()
            {
                return Ok(false);
            }
            let start = Locator {
                chapter: next,
                ..Default::default()
            };
            let job = PageIndexer::new(self.document.chapter(next)?, config, start)?;
            self.ahead = Some((id, job));
            return Ok(true);
        };
        if job.is_finished() {
            return Ok(false);
        }
        if job.step(&self.fonts)? {
            store.store(&book, &self.fonts, config, next, job.bounds())?;
        }
        Ok(true)
    }

    pub fn begin_reflow(&self, next: LayoutConfig) -> Result<Reflowing> {
        self.reflow_with(next, None)
    }
    /// Prepares replacing the fonts; the page stays in the old ones until
    /// the chapter is laid out in the new ones.
    pub fn begin_fonts(&self, fonts: Fonts, family: Option<String>) -> Result<Reflowing> {
        self.reflow_with(self.state.layout, Some((fonts, family)))
    }
    fn reflow_with(
        &self,
        next: LayoutConfig,
        fonts: Option<(Fonts, Option<String>)>,
    ) -> Result<Reflowing> {
        Ok(Reflowing {
            indexer: chapter_indexer(
                self.index_store.as_ref(),
                fonts.as_ref().map_or(&self.fonts, |(fonts, _)| fonts),
                &self.document.metadata.id,
                self.pagination.chapter(),
                next,
                self.position,
            )?,
            origin: self.document.metadata.id.clone(),
            position: self.position,
            previous: self.state.layout,
            next,
            previous_rotation: self.state.rotation,
            next_rotation: self.state.rotation,
            previous_raster: self.state.text_raster,
            next_raster: self.state.text_raster,
            fonts,
        })
    }
    /// Prepares layout and raster changes as one cancellable transition.
    pub fn begin_typography(&self, next: BookTypography) -> Result<Reflowing> {
        next.text_raster.validate()?;
        let mut job = self.begin_reflow(next.layout(self.state.layout))?;
        job.next_raster = next.text_raster;
        Ok(job)
    }

    /// Prepares cross-chapter navigation. Other actions use normal dispatch.
    pub fn begin_navigation(&mut self, action: Action) -> Result<Option<Navigating>> {
        let mut clipping = None;
        let chapter = self.position.chapter;
        let (target, remember, back) = match action {
            Action::Link(index) => (self.link_target(index)?, true, false),
            Action::Clipping(index) => {
                let record = self.verified_clipping(index)?;
                let target = record.range.start;
                clipping = Some(record);
                (target, true, false)
            }
            Action::Chapter(chapter) => (
                Locator {
                    chapter,
                    ..Default::default()
                },
                true,
                false,
            ),
            Action::Contents(index) => {
                let entry = self
                    .document
                    .metadata
                    .contents
                    .get(index)
                    .cloned()
                    .ok_or_else(|| Error::Document("contents entry out of range".into()))?;
                (
                    self.document.resolve_link(&entry.base, &entry.href)?,
                    true,
                    false,
                )
            }
            Action::Bookmark(index) => {
                let target = self
                    .state
                    .books
                    .get(&self.document.metadata.id)
                    .and_then(|book| book.bookmarks.get(index))
                    .ok_or_else(|| Error::Document("bookmark out of range".into()))?
                    .location;
                (target, true, false)
            }
            Action::Back => match self.history.last() {
                Some(target) => (*target, false, true),
                None => return Ok(None),
            },
            Action::Progress(percent) => {
                let (target, share) = self.progress_position(percent)?;
                if target == chapter {
                    return Ok(None);
                }
                let destination = self.document.chapter(target)?;
                let target = Locator {
                    chapter: target,
                    block: block_at(&destination, share),
                    byte: 0,
                };
                return self
                    .navigation_job(target, true, false, std::sync::Arc::new(destination))
                    .map(Some);
            }
            Action::Next
                if self.page_index() + 1 == self.page_count()
                    && self.pagination.is_complete()
                    && chapter + 1 < self.document.metadata.chapters.len() =>
            {
                (
                    Locator {
                        chapter: chapter + 1,
                        ..Default::default()
                    },
                    false,
                    false,
                )
            }
            Action::Previous
                if self.page_index() == 0
                    && chapter > 0
                    && chapter < self.document.metadata.chapters.len() =>
            {
                (
                    Locator {
                        chapter: chapter - 1,
                        block: usize::MAX,
                        byte: usize::MAX,
                    },
                    false,
                    false,
                )
            }
            _ => return Ok(None),
        };
        if target.chapter == chapter {
            return Ok(None);
        }
        let destination = self.document.chapter(target.chapter)?;
        let mut job =
            self.navigation_job(target, remember, back, std::sync::Arc::new(destination))?;
        job.clipping = clipping;
        Ok(Some(job))
    }
    fn progress_position(&self, percent: u8) -> Result<(usize, f64)> {
        self.document
            .position_at(percent)
            .ok_or_else(|| Error::Document("this book has no measure of progress".into()))
    }
    fn navigation_job(
        &self,
        target: Locator,
        remember: bool,
        back: bool,
        chapter: std::sync::Arc<Chapter>,
    ) -> Result<Navigating> {
        let links = chapter.links.clone();
        Ok(Navigating {
            clipping: None,
            indexer: chapter_indexer(
                self.index_store.as_ref(),
                &self.fonts,
                &self.document.metadata.id,
                chapter,
                self.state.layout,
                target,
            )?,
            target,
            origin: self.document.metadata.id.clone(),
            position: self.position,
            layout: self.state.layout,
            rotation: self.state.rotation,
            history: self.history.clone(),
            remember,
            back,
            links,
        })
    }
    /// Prepares a source position without changing visible or saved state.
    pub fn begin_location(&mut self, target: Locator) -> Result<Navigating> {
        let chapter = self.document.chapter(target.chapter)?;
        let valid = match chapter.blocks.get(target.block) {
            Some(Block::Text(text)) => text.text.is_char_boundary(target.byte),
            Some(Block::Image(_)) => target.byte == 0,
            None => false,
        };
        if !valid {
            return Err(Error::Document("invalid source position".into()));
        }
        self.navigation_job(target, true, false, std::sync::Arc::new(chapter))
    }
    fn check_navigation(&self, job: &Navigating) -> Result<()> {
        if let Some(clipping) = &job.clipping
            && !self
                .state
                .books
                .get(&clipping.book_id)
                .is_some_and(|book| book.clippings.contains(clipping))
        {
            return Err(Error::Document("clipping changed during navigation".into()));
        }
        if job.origin != self.document.metadata.id
            || job.position != self.position
            || job.layout != self.state.layout
            || job.rotation != self.state.rotation
            || job.history != self.history
        {
            return Err(Error::Document("reader changed during navigation".into()));
        }
        Ok(())
    }
    pub fn step_navigation(&self, job: &mut Navigating) -> Result<bool> {
        self.check_navigation(job)?;
        job.indexer.step(&self.fonts)
    }
    pub fn finish_navigation(&mut self, job: Navigating) -> Result<Effect> {
        self.finish_navigation_using(job, None, |_| Ok(()))
    }
    /// Publishes saved state before replacing the visible page. Failed storage
    /// leaves the position, annotations and back history unchanged.
    pub fn finish_navigation_saved(
        &mut self,
        job: Navigating,
        now: u64,
        save: impl FnOnce(&ReaderState) -> Result<()>,
    ) -> Result<Effect> {
        self.finish_navigation_using(job, Some(now), save)
    }
    fn finish_navigation_using(
        &mut self,
        job: Navigating,
        now: Option<u64>,
        save: impl FnOnce(&ReaderState) -> Result<()>,
    ) -> Result<Effect> {
        self.check_navigation(&job)?;
        let pagination = job.indexer.finish()?;
        let position = normalize_position(&pagination, job.target);
        rasterize_display(
            pagination.page(),
            &self.fonts,
            job.layout,
            self.state.text_raster,
            256,
            |source| self.document.resource(source),
        )?;
        if let Some(now) = now {
            let mut state = self.state.clone();
            state.set_position(&self.document.metadata.id, position, now);
            state
                .books
                .get_mut(&self.document.metadata.id)
                .unwrap()
                .typography = Some(BookTypography::current(&state));
            save(&state)?;
            self.state = state;
        }
        if job.remember {
            if self.history.len() == 64 {
                self.history.remove(0);
            }
            self.history.push(self.position);
        }
        if job.back {
            self.history.pop();
        }
        self.pagination = pagination;
        self.position = position;
        self.links = job.links;
        Ok(Effect::Redraw)
    }
    pub fn begin_rotation(&self, rotation: crate::state::Rotation) -> Result<Reflowing> {
        let mut next = self.state.layout;
        if rotation.landscape() != self.state.rotation.landscape() {
            core::mem::swap(&mut next.width, &mut next.height);
        }
        let mut job = self.begin_reflow(next)?;
        job.next_rotation = rotation;
        Ok(job)
    }
    fn check_reflow(&self, job: &Reflowing) -> Result<()> {
        if job.origin != self.document.metadata.id
            || job.position != self.position
            || job.previous != self.state.layout
            || job.previous_rotation != self.state.rotation
            || job.previous_raster != self.state.text_raster
        {
            return Err(Error::Document("reader changed during reflow".into()));
        }
        Ok(())
    }
    pub fn step_reflow(&self, job: &mut Reflowing) -> Result<bool> {
        self.check_reflow(job)?;
        let fonts = job.fonts.as_ref().map_or(&self.fonts, |(fonts, _)| fonts);
        job.indexer.step(fonts)
    }
    pub fn finish_reflow(&mut self, job: Reflowing) -> Result<Effect> {
        self.check_reflow(&job)?;
        let pagination = job.indexer.finish()?;
        let position = normalize_position(&pagination, job.position);
        rasterize_display(
            pagination.page(),
            job.fonts.as_ref().map_or(&self.fonts, |(fonts, _)| fonts),
            job.next,
            job.next_raster,
            256,
            |source| self.document.resource(source),
        )?;
        self.position = position;
        self.pagination = pagination;
        self.state.layout = job.next;
        self.state.text_raster = job.next_raster;
        self.state.rotation = job.next_rotation;
        if let Some((fonts, family)) = job.fonts {
            // Everything rendered or indexed ahead used the old fonts.
            self.fonts = fonts;
            self.state.font_family = family;
            self.frames.clear();
            self.cells.lock().unwrap_or_else(|e| e.into_inner()).clear();
            self.ahead = None;
        }
        Ok(Effect::Redraw)
    }

    /// Opens the file without hashing or parsing it. Dropping the job cancels.
    pub fn begin_open_file(&self, path: impl AsRef<std::path::Path>) -> Result<BookOpening> {
        let path = path.as_ref().to_owned();
        let file = crate::document::FileHash::new(std::fs::File::open(&path)?);
        Ok(BookOpening {
            phase: OpeningPhase::Hashing {
                path,
                file,
                cache_root: None,
            },
            origin: self.document.metadata.id.clone(),
            origin_position: self.position,
            config: self.state.layout,
        })
    }
    pub fn begin_open_cached_file(
        &self,
        path: impl AsRef<std::path::Path>,
        root: impl AsRef<std::path::Path>,
        limit: usize,
    ) -> Result<BookOpening> {
        Ok(BookOpening {
            phase: OpeningPhase::cached(path.as_ref(), root.as_ref(), limit)?,
            origin: self.document.metadata.id.clone(),
            origin_position: self.position,
            config: self.state.layout,
        })
    }

    /// Parses the destination before starting cooperative page indexing.
    pub fn begin_open(&self, document: Document) -> Result<BookOpening> {
        Ok(BookOpening {
            phase: OpeningPhase::Indexing(Box::new(prepare_opening(
                document,
                &self.state,
                &self.fonts,
                self.index_store.as_ref(),
            )?)),
            origin: self.document.metadata.id.clone(),
            origin_position: self.position,
            config: self.state.layout,
        })
    }

    fn check_opening(&self, opening: &BookOpening) -> Result<()> {
        if opening.origin != self.document.metadata.id
            || opening.origin_position != self.position
            || opening.config != self.state.layout
        {
            return Err(Error::Document("reader changed while opening book".into()));
        }
        if let OpeningPhase::Indexing(job) = &opening.phase
            && job.typography != self.state.typography_for(&job.document.metadata.id)
        {
            return Err(Error::Document(
                "book typography changed while opening".into(),
            ));
        }
        Ok(())
    }

    pub fn step_open(&self, opening: &mut BookOpening) -> Result<bool> {
        self.step_open_for(opening, std::time::Duration::ZERO)
    }

    pub fn step_open_for(
        &self,
        opening: &mut BookOpening,
        budget: std::time::Duration,
    ) -> Result<bool> {
        self.check_opening(opening)?;
        if let OpeningPhase::Indexing(job) = &mut opening.phase {
            return job.indexer.step(&self.fonts);
        }
        if let Some(document) = opening.phase.read_document(budget)? {
            opening.phase = self.begin_open(document)?.phase;
        }
        Ok(false)
    }

    /// Renders and validates the destination before replacing visible state.
    pub fn finish_open(&mut self, opening: BookOpening) -> Result<()> {
        self.check_opening(&opening)?;
        let OpeningPhase::Indexing(job) = opening.phase else {
            return Err(Error::Document("book hashing not completed".into()));
        };
        let IndexedOpening {
            typography,
            mut document,
            indexer,
            position,
            links,
        } = *job;
        let pagination = indexer.finish()?;
        let position = normalize_position(&pagination, position);
        rasterize_display(
            pagination.page(),
            &self.fonts,
            typography.layout(self.state.layout),
            typography.text_raster,
            256,
            |source| document.resource(source),
        )?;
        let timestamp = self
            .state
            .books
            .get(&self.document.metadata.id)
            .map_or(0, |book| book.last_opened);
        self.record_position(timestamp);
        typography.apply(&mut self.state);
        self.document = document;
        self.pagination = pagination;
        self.position = position;
        self.links = links;
        self.history.clear();
        Ok(())
    }

    /// Synchronous convenience wrapper for callers without an event loop.
    pub fn open_document(&mut self, document: Document) -> Result<()> {
        let mut opening = self.begin_open(document)?;
        while !self.step_open(&mut opening)? {}
        self.finish_open(opening)
    }
    /// Source anchor retained across reflow until an explicit navigation action.
    pub fn position(&self) -> Locator {
        self.position
    }

    /// Captures source text from the first through the last visible text line.
    /// Images and generated markers contribute no text. Reading position is unchanged.
    pub fn clip_page_text(&mut self, now: u64) -> Result<bool> {
        let page = self.page();
        let first = page
            .lines
            .first()
            .ok_or_else(|| Error::Document("page has no text to clip".into()))?;
        let last = page.lines.last().expect("first text line exists");
        let range = first.start..last.end;
        let clipping = crate::clipping::Clipping::capture(&mut self.document, range, now)?;
        self.state.add_clipping(clipping)
    }

    pub fn toggle_clipping_highlight(&mut self, index: usize) -> Result<()> {
        let clipping = self
            .state
            .books
            .get(&self.document.metadata.id)
            .and_then(|book| book.clippings.get(index))
            .cloned()
            .ok_or_else(|| Error::Document("clipping out of range".into()))?;
        if !clipping.highlighted {
            clipping.validate_document(&mut self.document)?;
        }
        self.state
            .set_clipping_highlight(&clipping, !clipping.highlighted)
    }

    fn verified_clipping(&mut self, index: usize) -> Result<crate::clipping::Clipping> {
        let clipping = self
            .state
            .books
            .get(&self.document.metadata.id)
            .and_then(|book| book.clippings.get(index))
            .cloned()
            .ok_or_else(|| Error::Document("clipping out of range".into()))?;
        clipping.validate_document(&mut self.document)?;
        Ok(clipping)
    }

    pub fn page_index(&self) -> usize {
        self.pagination.selected()
    }
    pub fn section_label(&self) -> String {
        let chapter = self.position.chapter;
        if chapter < self.document.metadata.chapters.len() {
            format!(
                "chapter {}/{}",
                chapter + 1,
                self.document.metadata.chapters.len()
            )
        } else {
            "supplemental document".into()
        }
    }
    pub fn page_count(&self) -> usize {
        self.pagination.pages.len()
    }
    /// Approximate book progress through the visible page; excludes supplements.
    pub fn book_progress_percent(&self) -> Option<u8> {
        self.document.estimated_progress(
            self.position.chapter,
            self.page_index() + 1,
            self.page_count(),
        )
    }
    pub fn warnings(&self) -> &[String] {
        &self.pagination.warnings
    }

    pub fn page_links(&self) -> Vec<&DocumentLink> {
        let page = self.page();
        self.links
            .iter()
            .filter(|link| link.start < page.end && link.end > page.start)
            .collect()
    }

    /// Returns a Unicode word's source range under a logical display coordinate.
    /// Hits use positive glyph advances and line ink height. Ligatures select the
    /// complete source word; this does not provide caret positions within a glyph.
    pub fn word_at(&self, x: u32, y: u32) -> Option<std::ops::Range<Locator>> {
        if x >= self.state.layout.width || y >= self.state.layout.height {
            return None;
        }
        let (x, y) = (x as f32, y as f32);
        let chapter = self.pagination.chapter();
        for line in &self.page().lines {
            if y < line.baseline - line.ascent || y >= line.baseline + line.descent {
                continue;
            }
            let Some(Block::Text(paragraph)) = chapter.blocks.get(line.start.block) else {
                continue;
            };
            for glyph in &line.glyphs {
                if glyph.advance <= 0.0 || x < glyph.x || x >= glyph.x + glyph.advance {
                    continue;
                }
                if let Some((start, word)) =
                    paragraph.text.unicode_word_indices().find(|(start, word)| {
                        *start <= glyph.cluster && glyph.cluster < start + word.len()
                    })
                {
                    return Some(
                        Locator {
                            byte: start,
                            ..line.start
                        }..Locator {
                            byte: start + word.len(),
                            ..line.start
                        },
                    );
                }
            }
        }
        None
    }

    /// Source-ordered words represented by positive-advance glyphs on this page.
    pub(crate) fn visible_words(&self) -> Result<Vec<(String, Locator, Locator)>> {
        let chapter = self.pagination.chapter();
        let mut words = Vec::new();
        for (block, source) in chapter.blocks.iter().enumerate() {
            let Block::Text(paragraph) = source else {
                continue;
            };
            let mut clusters: Vec<_> = self
                .page()
                .lines
                .iter()
                .filter(|line| line.start.block == block)
                .flat_map(|line| line.glyphs.iter())
                .filter(|glyph| glyph.advance > 0.0)
                .map(|glyph| glyph.cluster)
                .collect();
            if clusters.is_empty() {
                continue;
            }
            clusters.sort_unstable();
            clusters.dedup();
            for (byte, word) in paragraph.text.unicode_word_indices() {
                let end = byte + word.len();
                let index = clusters.partition_point(|cluster| *cluster < byte);
                if clusters.get(index).is_none_or(|cluster| *cluster >= end) {
                    continue;
                }
                if words.len() == 1024 {
                    return Err(Error::Limit("page word choices"));
                }
                let start = Locator {
                    chapter: chapter.index,
                    block,
                    byte,
                };
                words.push((
                    format!(
                        "{}: {}",
                        words.len() + 1,
                        word.chars().take(32).collect::<String>()
                    ),
                    start,
                    Locator { byte: end, ..start },
                ));
            }
        }
        Ok(words)
    }

    /// Returns the page-link index under a logical display coordinate.
    pub fn link_at(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.state.layout.width || y >= self.state.layout.height {
            return None;
        }
        let (x, y) = (x as f32, y as f32);
        let links = self.page_links();
        if links.is_empty() {
            return None;
        }
        for image in &self.page().images {
            if x >= image.x as f32
                && x < image.x as f32 + image.size.width as f32
                && y >= image.y as f32
                && y < image.y as f32 + image.size.height as f32
                && let Some(index) = links
                    .iter()
                    .position(|link| link.start <= image.location && link.end > image.location)
            {
                return Some(index);
            }
        }
        for line in &self.page().lines {
            if y < line.baseline - line.ascent || y >= line.baseline + line.descent {
                continue;
            }
            for glyph in &line.glyphs {
                if glyph.advance <= 0.0 || x < glyph.x || x >= glyph.x + glyph.advance {
                    continue;
                }
                let start = Locator {
                    byte: glyph.cluster,
                    ..line.start
                };
                let end = Locator {
                    byte: line
                        .glyphs
                        .iter()
                        .map(|g| g.cluster)
                        .filter(|byte| *byte > glyph.cluster)
                        .min()
                        .unwrap_or(line.end.byte),
                    ..line.start
                };
                if let Some(index) = links
                    .iter()
                    .position(|link| link.start < end && link.end > start)
                {
                    return Some(index);
                }
            }
        }
        None
    }

    fn link_target(&mut self, index: usize) -> Result<Locator> {
        let href = self
            .page_links()
            .get(index)
            .map(|link| link.href.clone())
            .ok_or_else(|| Error::Document("page link out of range".into()))?;
        let base = self
            .document
            .document_href(self.page().start.chapter)
            .ok_or_else(|| Error::Document("document out of range".into()))?
            .clone();
        self.document.resolve_link(&base, &href)
    }

    pub fn begin_preview(&mut self, index: usize, config: LayoutConfig) -> Result<PreviewOpening> {
        let target = self.link_target(index)?;
        let chapter = std::sync::Arc::new(self.document.chapter(target.chapter)?);
        Ok(PreviewOpening {
            indexer: PageIndexer::for_preview(std::sync::Arc::clone(&chapter), config, target)?,
            source_chapter: chapter,
            target,
            config,
            origin: self.document.metadata.id.clone(),
            origin_position: self.position,
            origin_layout: self.state.layout,
        })
    }

    fn validate_preview_opening(&self, job: &PreviewOpening) -> Result<()> {
        if job.origin != self.document.metadata.id
            || job.origin_position != self.position
            || job.origin_layout != self.state.layout
        {
            return Err(Error::Document("link preview opening is stale".into()));
        }
        Ok(())
    }

    pub fn step_preview(&self, job: &mut PreviewOpening) -> Result<bool> {
        self.validate_preview_opening(job)?;
        job.indexer.step(&self.fonts)
    }

    pub fn finish_preview(&self, job: PreviewOpening) -> Result<LinkPreview> {
        self.validate_preview_opening(&job)?;
        let pagination = job.indexer.finish()?;
        let page_index = pagination.page_for(job.target);
        Ok(LinkPreview {
            source_chapter: job.source_chapter,
            pagination,
            page_index,
            target: job.target,
            config: job.config,
            origin: job.origin,
            origin_position: job.origin_position,
            origin_layout: job.origin_layout,
        })
    }

    pub fn preview_link(&mut self, index: usize, config: LayoutConfig) -> Result<LinkPreview> {
        let mut job = self.begin_preview(index, config)?;
        while !self.step_preview(&mut job)? {}
        self.finish_preview(job)
    }

    pub(crate) fn validate_preview(&self, preview: &LinkPreview) -> Result<()> {
        if preview.origin != self.document.metadata.id
            || preview.origin_position != self.position
            || preview.origin_layout != self.state.layout
        {
            return Err(Error::Document("link preview is stale".into()));
        }
        Ok(())
    }

    pub fn turn_preview(&self, preview: &mut LinkPreview, next: bool) -> Result<Effect> {
        self.validate_preview(preview)?;
        let index = if next {
            (preview.page_index + 1).min(preview.page_count().saturating_sub(1))
        } else {
            preview.page_index.saturating_sub(1)
        };
        if index == preview.page_index {
            return Ok(Effect::Unchanged);
        }
        preview.pagination.select(index, &self.fonts)?;
        preview.page_index = index;
        preview.target = preview.page().start;
        Ok(Effect::Redraw)
    }

    pub fn render_preview(&mut self, preview: &LinkPreview, levels: u16) -> Result<Vec<u8>> {
        self.validate_preview(preview)?;
        rasterize_display(
            preview.page(),
            &self.fonts,
            preview.config,
            self.state.text_raster,
            levels,
            |source| self.document.resource(source),
        )
    }

    pub fn follow_preview(&mut self, preview: &LinkPreview) -> Result<Effect> {
        self.validate_preview(preview)?;
        self.move_to(preview.target, true)
    }
    pub fn begin_follow_preview(&self, preview: &LinkPreview) -> Result<Option<Navigating>> {
        self.validate_preview(preview)?;
        if preview.target.chapter == self.position.chapter {
            return Ok(None);
        }
        self.navigation_job(
            preview.target,
            true,
            false,
            std::sync::Arc::clone(&preview.source_chapter),
        )
        .map(Some)
    }

    pub fn render(&mut self, levels: u16) -> Result<Vec<u8>> {
        self.render_with_selection(None, levels)
    }

    pub(crate) fn render_selection(
        &mut self,
        range: &std::ops::Range<Locator>,
        levels: u16,
    ) -> Result<Vec<u8>> {
        self.render_with_selection(Some(range), levels)
    }

    fn frame_key(&self, levels: u16) -> FrameKey {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        if let Some(book) = self.state.books.get(&self.document.metadata.id) {
            // Highlights are drawn only while their recorded source still verifies.
            for clipping in book.clippings.iter().filter(|c| c.highlighted) {
                for end in [clipping.range.start, clipping.range.end] {
                    (end.chapter, end.block, end.byte).hash(&mut hasher);
                }
                (clipping.version, &clipping.book_id, &clipping.text).hash(&mut hasher);
            }
        }
        FrameKey {
            pages: self.pagination.id(),
            index: self.pagination.selected(),
            raster: self.state.text_raster,
            levels,
            highlights: hasher.finish(),
        }
    }

    /// Lays out and renders the page that a turn within the chapter would
    /// show, one bounded step per call. Returns whether a step ran; visible
    /// state is unchanged.
    pub fn prepare_turn(&mut self, forward: bool, levels: u16) -> Result<bool> {
        let current = self.pagination.selected();
        let index = if forward {
            current + 1
        } else {
            current.wrapping_sub(1)
        };
        if index >= self.page_count() {
            return Ok(false);
        }
        if self.pagination.prepare(index, &self.fonts)? {
            return Ok(true);
        }
        let key = FrameKey {
            index,
            ..self.frame_key(levels)
        };
        if self.frames.iter().any(|(known, _)| *known == key) {
            return Ok(false);
        }
        // Both pages are laid out, so selecting swaps them without layout.
        self.pagination.select(index, &self.fonts)?;
        let rendered = self.render(levels);
        self.pagination.select(current, &self.fonts)?;
        rendered.map(|_| true)
    }

    fn render_with_selection(
        &mut self,
        pending: Option<&std::ops::Range<Locator>>,
        levels: u16,
    ) -> Result<Vec<u8>> {
        const FRAMES: usize = 3;
        let key = self.frame_key(levels);
        if pending.is_none()
            && let Some((_, pixels)) = self.frames.iter().find(|(known, _)| *known == key)
        {
            return Ok(pixels.clone());
        }
        let pixels = self.rasterize_with_selection(pending, levels)?;
        if pending.is_none() {
            // Frames of other paginations or settings can never match again.
            self.frames.retain(|(known, _)| {
                FrameKey {
                    index: key.index,
                    ..*known
                } == key
            });
            if self.frames.len() == FRAMES {
                let farthest = (0..FRAMES)
                    .max_by_key(|&i| self.frames[i].0.index.abs_diff(self.page_index()))
                    .expect("nonempty frames");
                self.frames.swap_remove(farthest);
            }
            self.frames.push((key, pixels.clone()));
        }
        Ok(pixels)
    }

    fn rasterize_with_selection(
        &mut self,
        pending: Option<&std::ops::Range<Locator>>,
        levels: u16,
    ) -> Result<Vec<u8>> {
        let mut rendered = rasterize_display_report(
            self.pagination.page(),
            &self.fonts,
            self.state.layout,
            self.state.text_raster,
            levels,
            |source| self.document.resource(source),
        )?;
        for warning in rendered.warnings {
            if self.pagination.warnings.len() < 64 && !self.pagination.warnings.contains(&warning) {
                self.pagination.warnings.push(warning);
            }
        }
        self.draw_link_underlines(&mut rendered.pixels);
        let mut ranges: Vec<_> = pending.into_iter().cloned().collect();
        let mut omitted = false;
        let chapter = self.pagination.chapter();
        if let Some(book) = self.state.books.get(&self.document.metadata.id) {
            for clipping in &book.clippings {
                if !clipping.highlighted
                    || !self.pagination.page().lines.iter().any(|line| {
                        clipping.range.start < line.end && clipping.range.end > line.start
                    })
                {
                    continue;
                }
                let validation = if clipping.range.start.chapter == chapter.index
                    && clipping.range.end.chapter == chapter.index
                {
                    clipping.validate_chapter(&self.document.metadata.id, &chapter)
                } else {
                    clipping.validate_document(&mut self.document)
                };
                if validation.is_ok() {
                    ranges.push(clipping.range.clone());
                } else {
                    omitted = true;
                }
            }
        }
        let warning = "Saved highlight omitted: source verification failed";
        if omitted
            && self.pagination.warnings.len() < 64
            && !self.pagination.warnings.iter().any(|w| w == warning)
        {
            self.pagination.warnings.push(warning.into());
        }
        if !ranges.is_empty() {
            self.draw_selection_ranges(&mut rendered.pixels, &ranges);
        }
        Ok(rendered.pixels)
    }

    fn draw_selection_ranges(&self, pixels: &mut [u8], ranges: &[std::ops::Range<Locator>]) {
        let config = self.state.layout;
        let mut coverage = vec![0u8; pixels.len().div_ceil(8)];
        for line in &self.page().lines {
            let mut clusters: Vec<_> = line.glyphs.iter().map(|g| g.cluster).collect();
            clusters.push(line.end.byte);
            clusters.sort_unstable();
            clusters.dedup();
            for glyph in &line.glyphs {
                if glyph.advance <= 0.0 {
                    continue;
                }
                let start = Locator {
                    byte: glyph.cluster,
                    ..line.start
                };
                let next = clusters.partition_point(|byte| *byte <= glyph.cluster);
                let end = Locator {
                    byte: clusters.get(next).copied().unwrap_or(line.end.byte),
                    ..line.start
                };
                if !ranges
                    .iter()
                    .any(|range| range.start < end && range.end > start)
                {
                    continue;
                }
                let left = (glyph.x.floor().max(0.0) as u32).min(config.width);
                let right = ((glyph.x + glyph.advance).ceil().max(0.0) as u32).min(config.width);
                let top =
                    ((line.baseline - line.ascent).floor().max(0.0) as u32).min(config.height);
                let bottom =
                    ((line.baseline + line.descent).ceil().max(0.0) as u32).min(config.height);
                if left < right && top < bottom {
                    for y in top..bottom {
                        let row = y as usize * config.width as usize;
                        mark_selection_span(
                            &mut coverage,
                            row + left as usize,
                            row + right as usize,
                        );
                    }
                }
            }
        }
        for (index, pixel) in pixels.iter_mut().enumerate() {
            if coverage[index / 8] & (1 << (index % 8)) != 0 {
                *pixel = 255 - *pixel;
            }
        }
    }

    fn draw_link_underlines(&self, pixels: &mut [u8]) {
        let links = self.page_links();
        if links.is_empty() {
            return;
        }
        let config = self.state.layout;
        for line in &self.page().lines {
            let mut clusters: Vec<_> = line.glyphs.iter().map(|glyph| glyph.cluster).collect();
            clusters.push(line.end.byte);
            clusters.sort_unstable();
            clusters.dedup();
            for glyph in &line.glyphs {
                if glyph.advance <= 0.0 {
                    continue;
                }
                let start = Locator {
                    byte: glyph.cluster,
                    ..line.start
                };
                let next = clusters.partition_point(|byte| *byte <= glyph.cluster);
                let end = Locator {
                    byte: clusters.get(next).copied().unwrap_or(line.end.byte),
                    ..line.start
                };
                if !links
                    .iter()
                    .any(|link| link.start < end && link.end > start)
                {
                    continue;
                }
                let left = glyph.x.floor().max(0.0) as u32;
                let right = (glyph.x + glyph.advance).ceil().max(0.0) as u32;
                let top = (glyph.y + glyph.size * 0.12).round().max(0.0) as u32;
                let thickness = (glyph.size / 18.0).round().max(1.0) as u32;
                for y in top..top.saturating_add(thickness).min(config.height) {
                    let row = y as usize * config.width as usize;
                    let start = row + left.min(config.width) as usize;
                    let end = row + right.min(config.width) as usize;
                    pixels[start..end].fill(0);
                }
            }
        }
    }

    /// Renders a bounded menu using the same font and grayscale path as books.
    pub fn render_panel(&self, lines: &[String], levels: u16) -> Result<Vec<u8>> {
        if lines.len() > 8 || lines.iter().any(|line| line.chars().count() > 40) {
            return Err(Error::Limit("menu text"));
        }
        let chapter = Chapter {
            language: "und".into(),
            index: 0,
            href: String::new(),
            warnings: Vec::new(),
            anchors: Default::default(),
            notes: Vec::new(),
            links: Vec::new(),
            blocks: lines
                .iter()
                .map(|text| {
                    Block::Text(Paragraph {
                        list: None,
                        text: text.clone(),
                        spans: vec![Span {
                            language: None,
                            range: 0..text.len(),
                            style: TextStyle::default(),
                        }],
                    })
                })
                .collect(),
        };
        let config = LayoutConfig {
            font_size: 18.0,
            ..self.state.layout
        };
        let pages = paginate(&chapter, &self.fonts, config)?;
        if pages.pages.len() != 1 {
            return Err(Error::Layout("menu does not fit viewport"));
        }
        rasterize_display(
            &pages.pages[0],
            &self.fonts,
            config,
            self.state.text_raster,
            levels,
            |_| Err(Error::Document("menu resource".into())),
        )
    }

    pub fn record_position(&mut self, now: u64) {
        self.state
            .set_position(&self.document.metadata.id, self.position, now);
        let typography = BookTypography::current(&self.state);
        self.state
            .books
            .get_mut(&self.document.metadata.id)
            .unwrap()
            .typography = Some(typography);
    }

    /// Menu rows share their partition with pointer hit testing.
    pub fn render_rows(
        &self,
        rows: &[String],
        selected: Option<usize>,
        levels: u16,
    ) -> Result<Vec<u8>> {
        let width = self.state.layout.width;
        let height = self.state.layout.height;
        let geometry = crate::menu::Geometry::new(width, height)?;
        if rows.len() > geometry.rows {
            return Err(Error::Limit("menu rows"));
        }
        let mut pixels = vec![255; (width * height) as usize];
        for (index, text) in rows.iter().enumerate() {
            let (y, row_height) = geometry.row(index);
            let row = self.render_cell(text, width, row_height, levels)?;
            let offset = (y * width) as usize;
            for (destination, source) in pixels[offset..offset + row.len()].iter_mut().zip(row) {
                *destination = if selected == Some(index) {
                    255 - source
                } else {
                    source
                };
            }
            let line = offset + ((row_height - 1) * width) as usize;
            pixels[line..line + width as usize].fill(0);
        }
        Ok(pixels)
    }

    pub(crate) fn render_cell(
        &self,
        text: &str,
        width: u32,
        height: u32,
        levels: u16,
    ) -> Result<Vec<u8>> {
        self.render_styled_cell(text, width, height, levels, CellStyle::Compact)
    }

    pub(crate) fn render_styled_cell(
        &self,
        text: &str,
        width: u32,
        height: u32,
        levels: u16,
        style: CellStyle,
    ) -> Result<Vec<u8>> {
        // A menu page shows up to eight full-width rows; keep about three pages.
        const CELL_BYTES: usize = 1024 * 1024;
        let mut cells = self.cells.lock().unwrap_or_else(|e| e.into_inner());
        let known = cells.iter().position(|((known, w, h, l, s), _)| {
            known == text && (*w, *h, *l, *s) == (width, height, levels, style)
        });
        let entry = match known {
            Some(index) => cells.remove(index),
            None => {
                let pixels = self.rasterize_cell(text, width, height, levels, style)?;
                let mut held: usize = cells.iter().map(|(_, cell)| cell.len()).sum();
                while !cells.is_empty() && held + pixels.len() > CELL_BYTES {
                    held -= cells.remove(0).1.len();
                }
                ((text.to_owned(), width, height, levels, style), pixels)
            }
        };
        let pixels = entry.1.clone();
        cells.push(entry);
        Ok(pixels)
    }

    fn rasterize_cell(
        &self,
        text: &str,
        width: u32,
        height: u32,
        levels: u16,
        style: CellStyle,
    ) -> Result<Vec<u8>> {
        let (size, bold) = match style {
            CellStyle::Compact => (18, false),
            CellStyle::Label { size, bold } => (size, bold),
        };
        let chapter = Chapter {
            language: "und".into(),
            index: 0,
            href: String::new(),
            warnings: vec![],
            anchors: Default::default(),
            notes: Vec::new(),
            links: vec![],
            blocks: vec![Block::Text(Paragraph {
                list: None,
                text: text.to_owned(),
                spans: vec![Span {
                    language: None,
                    range: 0..text.len(),
                    style: TextStyle {
                        bold,
                        ..TextStyle::default()
                    },
                }],
            })],
        };
        let raster = |config: LayoutConfig| {
            let pages = paginate(&chapter, &self.fonts, config)?;
            let page = pages
                .pages
                .first()
                .ok_or(Error::Layout("menu cell has no page"))?;
            let pixels = rasterize_display(
                page,
                &self.fonts,
                config,
                Default::default(),
                levels,
                |_| Err(Error::Document("menu resource".into())),
            )?;
            let lines: Vec<_> = page
                .lines
                .iter()
                .map(|line| (line.baseline, line.baseline - line.ascent))
                .collect();
            Ok::<_, Error>((pixels, lines, pages.pages.len()))
        };
        let CellStyle::Label { .. } = style else {
            let (pixels, _, pages) = raster(LayoutConfig {
                width,
                height,
                // Retain the layout engine's three-line minimum for 18 px menu text.
                margin: 12.min(height.saturating_sub(65) / 2),
                font_size: 18.0,
                line_height: 1.2,
                hyphenation: crate::layout::Hyphenation::Off,
                footer: 0,
            })?;
            if pages != 1 {
                return Err(Error::Layout("menu row exceeds viewport"));
            }
            return Ok(pixels);
        };
        // Lay the label out on a taller canvas, then cut the cell out around
        // the lines that fit, so every label shares one vertical alignment.
        let line_height = 1.2;
        let margin = 20.min(width / 8);
        let canvas = height.max((4.0 * size as f32 * line_height) as u32 + 2 * margin);
        let (pixels, lines, _) = raster(LayoutConfig {
            width,
            height: canvas,
            margin,
            font_size: size as f32,
            line_height,
            hyphenation: crate::layout::Hyphenation::Off,
            footer: 0,
        })?;
        let fitting = ((height as f32 / (size as f32 * line_height)) as usize).max(1);
        let shown = &lines[..lines.len().min(fitting)];
        // Center the span from the first line's capital height to the last
        // baseline; font ascent and descent are too uneven to center on.
        let (top, bottom) = match (shown.first(), shown.last()) {
            (Some(first), Some(last)) => (first.0 - 0.68 * size as f32, last.0),
            _ => (0.0, 0.0),
        };
        // Canvas row shown at the cell's first row; negative when the text
        // has to move down.
        let start = ((top + bottom) / 2.0 - height as f32 / 2.0).round() as i64;
        // Lines that do not fit are left out instead of showing a sliver.
        let end = lines
            .get(shown.len())
            .map_or(i64::from(canvas), |next| next.1.floor() as i64);
        let mut cell = vec![255; (width * height) as usize];
        for (row, target) in cell.chunks_exact_mut(width as usize).enumerate() {
            let source = start + row as i64;
            if (0..end.min(i64::from(canvas))).contains(&source) {
                let offset = source as usize * width as usize;
                target.copy_from_slice(&pixels[offset..offset + width as usize]);
            }
        }
        Ok(cell)
    }

    fn move_to(&mut self, target: Locator, remember: bool) -> Result<Effect> {
        let previous = self.position;
        // Build the destination before changing visible state or back history.
        let mut links = None;
        let replacement = if target.chapter != previous.chapter {
            let chapter = self.document.chapter(target.chapter)?;
            links = Some(chapter.links.clone());
            let pages = PageCache::new(chapter, &self.fonts, self.state.layout, target)?;
            Some(pages)
        } else {
            None
        };
        if replacement.is_none() {
            self.pagination.select_at(target, &self.fonts)?;
        }
        let pages = replacement.as_ref().unwrap_or(&self.pagination);
        let target = normalize_position(pages, target);
        if target == previous {
            return Ok(Effect::Unchanged);
        }
        if remember {
            if self.history.len() == 64 {
                self.history.remove(0);
            }
            self.history.push(previous);
        }
        if let Some(pages) = replacement {
            self.pagination = pages;
        }
        if let Some(links) = links {
            self.links = links;
        }
        self.position = target;
        Ok(Effect::Redraw)
    }

    pub fn dispatch(&mut self, action: Action) -> Result<Effect> {
        let chapter = self.page().start.chapter;
        match action {
            Action::Contents(index) => {
                let entry = self
                    .document
                    .metadata
                    .contents
                    .get(index)
                    .cloned()
                    .ok_or_else(|| Error::Document("contents entry out of range".into()))?;
                let target = self.document.resolve_link(&entry.base, &entry.href)?;
                self.move_to(target, true)
            }
            Action::Link(index) => {
                let target = self.link_target(index)?;
                self.move_to(target, true)
            }
            // The page after the last indexed one is laid out on demand.
            Action::Next
                if self.page_index() + 1 < self.page_count()
                    || self.pagination.extend(&self.fonts)? =>
            {
                self.pagination.select(self.page_index() + 1, &self.fonts)?;
                self.position = self.page().start;
                Ok(Effect::Redraw)
            }
            Action::Next if chapter + 1 < self.document.metadata.chapters.len() => self.move_to(
                Locator {
                    chapter: chapter + 1,
                    ..Default::default()
                },
                false,
            ),
            Action::Previous if self.page_index() > 0 => {
                self.pagination.select(self.page_index() - 1, &self.fonts)?;
                self.position = self.page().start;
                Ok(Effect::Redraw)
            }
            Action::Previous if chapter > 0 && chapter < self.document.metadata.chapters.len() => {
                self.move_to(
                    Locator {
                        chapter: chapter - 1,
                        block: usize::MAX,
                        byte: usize::MAX,
                    },
                    false,
                )
            }
            Action::Chapter(chapter) => self.move_to(
                Locator {
                    chapter,
                    ..Default::default()
                },
                true,
            ),
            Action::Bookmark(index) => {
                let target = self
                    .state
                    .books
                    .get(&self.document.metadata.id)
                    .and_then(|book| book.bookmarks.get(index))
                    .ok_or_else(|| Error::Document("bookmark out of range".into()))?
                    .location;
                self.move_to(target, true)
            }
            Action::Clipping(index) => {
                let clipping = self.verified_clipping(index)?;
                self.move_to(clipping.range.start, true)
            }
            Action::Progress(percent) => {
                let (chapter, share) = self.progress_position(percent)?;
                let block = block_at(&self.pagination.chapter(), share);
                self.move_to(
                    Locator {
                        chapter,
                        block,
                        byte: 0,
                    },
                    true,
                )
            }
            Action::ToggleBookmark => {
                let page = self.page();
                let label = page
                    .lines
                    .iter()
                    .find(|line| line.start <= self.position && self.position <= line.end)
                    .or_else(|| page.lines.first())
                    .map(|line| line.text.clone())
                    .unwrap_or_else(|| format!("Chapter {} image", chapter + 1));
                self.state
                    .toggle_bookmark(&self.document.metadata.id, self.position, label);
                Ok(Effect::Redraw)
            }
            Action::Back => {
                let Some(target) = self.history.last().copied() else {
                    return Ok(Effect::Unchanged);
                };
                let effect = self.move_to(target, false)?;
                self.history.pop();
                Ok(effect)
            }
            Action::Rotate(rotation) => {
                if rotation == self.state.rotation {
                    return Ok(Effect::Unchanged);
                }
                let mut config = self.state.layout;
                if rotation.landscape() != self.state.rotation.landscape() {
                    core::mem::swap(&mut config.width, &mut config.height);
                }
                self.dispatch(Action::Reflow(config))?;
                self.state.rotation = rotation;
                Ok(Effect::Redraw)
            }
            Action::Reflow(config) => {
                let anchor = self.position;
                let updated =
                    PageCache::new(self.document.chapter(chapter)?, &self.fonts, config, anchor)?;
                let anchor = normalize_position(&updated, anchor);
                self.position = anchor;
                self.pagination = updated;
                self.state.layout = config;
                Ok(Effect::Redraw)
            }
            Action::Tap { x, y } => {
                let config = self.state.layout;
                if x >= config.width || y >= config.height {
                    return Ok(Effect::Unchanged);
                }
                if let Some(index) = self.link_at(x, y) {
                    return self.dispatch(Action::Link(index));
                }
                if x < config.width / 3 {
                    self.dispatch(Action::Previous)
                } else if x >= config.width * 2 / 3 {
                    self.dispatch(Action::Next)
                } else {
                    Ok(Effect::Menu)
                }
            }
            Action::Next | Action::Previous => Ok(Effect::Unchanged),
        }
    }
}

fn normalize_position(pages: &PageCache, requested: Locator) -> Locator {
    let page = pages.page();
    let text_position = page.lines.iter().any(|line| {
        if requested.chapter != line.start.chapter
            || requested.block != line.start.block
            || requested.byte < line.start.byte
            || requested.byte > line.end.byte
        {
            return false;
        }
        let offset = requested.byte - line.start.byte;
        // The omitted suffix consists only of ASCII layout whitespace.
        offset >= line.text.len() || line.text.is_char_boundary(offset)
    });
    if text_position || page.images.iter().any(|image| image.location == requested) {
        requested
    } else {
        page.start
    }
}

// The half-open span is within the framebuffer. Only its boundary bytes need
// bitwise updates; whole bytes can be filled without per-pixel iteration.
fn mark_selection_span(coverage: &mut [u8], start: usize, end: usize) {
    if start >= end {
        return;
    }
    let first = start / 8;
    let last = (end - 1) / 8;
    let first_mask = u8::MAX << (start % 8);
    let last_mask = u8::MAX >> (7 - (end - 1) % 8);
    if first == last {
        coverage[first] |= first_mask & last_mask;
    } else {
        coverage[first] |= first_mask;
        coverage[first + 1..last].fill(u8::MAX);
        coverage[last] |= last_mask;
    }
}

#[cfg(test)]
mod selection_coverage_tests {
    use super::mark_selection_span;

    #[test]
    fn packed_spans_match_pixel_union_at_every_byte_boundary() {
        const PIXELS: usize = 257;
        for start in 0..=PIXELS {
            for end in start..=PIXELS {
                let mut coverage = vec![0; PIXELS.div_ceil(8)];
                mark_selection_span(&mut coverage, 7, 19);
                mark_selection_span(&mut coverage, start, end);
                mark_selection_span(&mut coverage, start, end);
                for pixel in 0..coverage.len() * 8 {
                    assert_eq!(
                        coverage[pixel / 8] & (1 << (pixel % 8)) != 0,
                        (7..19).contains(&pixel) || (start..end).contains(&pixel),
                        "span {start}..{end}, pixel {pixel}"
                    );
                }
            }
        }
    }

    #[test]
    fn dense_overlapping_selection_retains_one_bit_per_screen_pixel() {
        let mut coverage = vec![0; (480usize * 800).div_ceil(8)];
        let capacity = coverage.capacity();
        for glyph in 0..131_072 {
            let row = glyph % 800;
            mark_selection_span(&mut coverage, row * 480, (row + 1) * 480);
        }
        assert_eq!(coverage.len(), 48_000);
        assert_eq!(coverage.capacity(), capacity);
        assert!(coverage.iter().all(|byte| *byte == u8::MAX));
    }
}

#[cfg(test)]
mod cached_open_tests {
    use super::*;
    use crate::{layout::Font, state::Library};
    use std::{fs, path::Path, time::Duration};

    fn start(path: &Path, root: &Path, limit: usize) -> ReaderStartup {
        let fonts = Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
                .unwrap(),
        );
        ReaderStartup::from_cached_file(path, root, limit, fonts, ReaderState::default()).unwrap()
    }
    fn parse(startup: &mut ReaderStartup) -> String {
        loop {
            if let OpeningPhase::Indexing(job) = &startup.phase {
                return job.document.metadata.id.clone();
            }
            assert!(!startup.step().unwrap());
        }
    }
    fn cache_json(root: &Path) -> serde_json::Value {
        serde_json::from_slice(&fs::read(root.join(".femto-library-cache.json")).unwrap()).unwrap()
    }
    fn set_cache(root: &Path, value: &serde_json::Value) {
        fs::write(
            root.join(".femto-library-cache.json"),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn selected_book_populates_cache_and_warm_open_skips_hashing_without_rewriting() {
        let dir = tempfile::tempdir().unwrap();
        let book = dir.path().join("book.txt");
        fs::write(&book, "chapter ".repeat(4000)).unwrap();
        fs::write(dir.path().join("unopened.epub"), "invalid unrelated book").unwrap();
        assert!(
            Library::load_cached(dir.path(), 8)
                .unwrap()
                .entries
                .is_empty()
        );
        let mut cold = start(&book, dir.path(), 8);
        assert!(matches!(cold.phase, OpeningPhase::Hashing { .. }));
        let id = parse(&mut cold);
        let snapshot = Library::load_cached(dir.path(), 8).unwrap();
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.entries[0].metadata.id, id);
        let path = dir.path().join(".femto-library-cache.json");
        let modified = fs::metadata(&path).unwrap().modified().unwrap() - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let bytes = fs::read(&path).unwrap();
        let mut warm = start(&book, dir.path(), 8);
        assert!(matches!(warm.phase, OpeningPhase::Cached { .. }));
        assert!(!warm.step().unwrap());
        assert!(matches!(warm.phase, OpeningPhase::Indexing(_)));
        assert_eq!(parse(&mut warm), id);
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), modified);
    }

    #[test]
    fn legacy_cache_displays_metadata_but_hashes_once_even_with_a_stamp_field() {
        let dir = tempfile::tempdir().unwrap();
        let book = dir.path().join("book.txt");
        fs::write(&book, "book text").unwrap();
        parse(&mut start(&book, dir.path(), 8));
        let mut legacy = cache_json(dir.path());
        legacy["version"] = 1.into();
        // A v1 cache cannot authorize cached identity even if it contains
        // an unknown stamp field copied from a newer writer.
        set_cache(dir.path(), &legacy);
        assert_eq!(
            Library::load_cached(dir.path(), 8).unwrap().entries.len(),
            1
        );
        let mut upgrading = start(&book, dir.path(), 8);
        assert!(matches!(upgrading.phase, OpeningPhase::Hashing { .. }));
        parse(&mut upgrading);
        assert_eq!(cache_json(dir.path())["version"], 2);
        assert!(matches!(
            start(&book, dir.path(), 8).phase,
            OpeningPhase::Cached { .. }
        ));
        fs::remove_file(&book).unwrap();
        // Loading the menu does not stat or open every cached document.
        assert_eq!(
            Library::load_cached(dir.path(), 8).unwrap().entries.len(),
            1
        );
    }

    #[test]
    fn size_and_mtime_changes_rehash_and_explicit_scan_verifies_preserved_stamps() {
        let dir = tempfile::tempdir().unwrap();
        let book = dir.path().join("book.txt");
        fs::write(&book, "first").unwrap();
        let first = parse(&mut start(&book, dir.path(), 8));
        fs::write(&book, "second-longer").unwrap();
        let mut resized = start(&book, dir.path(), 8);
        assert!(matches!(resized.phase, OpeningPhase::Hashing { .. }));
        let second = parse(&mut resized);
        assert_ne!(first, second);
        let file = fs::File::options().write(true).open(&book).unwrap();
        let modified = file.metadata().unwrap().modified().unwrap() + Duration::from_secs(2);
        file.set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let mut retimed = start(&book, dir.path(), 8);
        assert!(matches!(retimed.phase, OpeningPhase::Hashing { .. }));
        assert_eq!(parse(&mut retimed), second);
        fs::write(&book, "third--longer").unwrap();
        file.set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(matches!(
            start(&book, dir.path(), 8).phase,
            OpeningPhase::Cached { .. }
        ));
        let verified = Library::scan_cached(dir.path(), 8).unwrap();
        assert_ne!(verified.entries[0].metadata.id, second);
        assert_eq!(
            parse(&mut start(&book, dir.path(), 8)),
            verified.entries[0].metadata.id
        );
    }

    #[test]
    fn cached_open_keeps_its_handle_and_rechecks_pending_handle_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let book = dir.path().join("book.txt");
        fs::write(&book, "original").unwrap();
        let original = parse(&mut start(&book, dir.path(), 8));
        let mut opened = start(&book, dir.path(), 8);
        fs::rename(&book, dir.path().join("old.txt")).unwrap();
        fs::write(&book, "replacement text").unwrap();
        assert_eq!(parse(&mut opened), original);
        let replacement = parse(&mut start(&book, dir.path(), 8));
        assert_ne!(replacement, original);
        let mut pending = start(&book, dir.path(), 8);
        assert!(matches!(pending.phase, OpeningPhase::Cached { .. }));
        fs::write(&book, "changed while pending").unwrap();
        assert!(!pending.step().unwrap());
        assert!(matches!(pending.phase, OpeningPhase::Hashing { .. }));
        assert_ne!(parse(&mut pending), replacement);
    }

    #[test]
    fn cache_capacity_stays_loadable_and_failed_cache_writes_do_not_block_reading() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["one.txt", "two.txt", "three.txt"] {
            let book = dir.path().join(name);
            fs::write(&book, name).unwrap();
            parse(&mut start(&book, dir.path(), 2));
            let snapshot = Library::load_cached(dir.path(), 2).unwrap();
            assert!(snapshot.errors.is_empty());
            assert!(snapshot.entries.len() <= 2);
            assert!(snapshot.entries.iter().any(|entry| entry.path == book));
        }
        fs::remove_file(dir.path().join(".femto-library-cache.json")).unwrap();
        fs::create_dir(dir.path().join(".femto-library-cache.json")).unwrap();
        let book = dir.path().join("one.txt");
        parse(&mut start(&book, dir.path(), 2));
    }

    #[test]
    fn cached_paths_cannot_escape_the_selected_library() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        fs::create_dir(&root).unwrap();
        let book = root.join("book.txt");
        fs::write(&book, "inside").unwrap();
        parse(&mut start(&book, &root, 8));
        let outside = dir.path().join("outside.txt");
        fs::write(&outside, "outside").unwrap();
        let mut corrupt = cache_json(&root);
        corrupt["entries"][0]["path"] = serde_json::to_value(root.join("../outside.txt")).unwrap();
        set_cache(&root, &corrupt);
        let snapshot = Library::load_cached(&root, 8).unwrap();
        assert!(snapshot.entries.is_empty());
        assert!(!snapshot.errors.is_empty());
        assert!(OpeningPhase::cached(&outside, &root, 8).is_err());
        #[cfg(unix)]
        {
            fs::remove_file(&book).unwrap();
            std::os::unix::fs::symlink(&outside, &book).unwrap();
            assert!(OpeningPhase::cached(&book, &root, 8).is_err());
        }
    }
}
