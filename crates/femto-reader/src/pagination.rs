//! Compact chapter page index with one retained page of positioned glyphs.

use std::sync::Arc;

use crate::{
    Error, Result,
    document::Chapter,
    layout::{
        Fonts, LayoutConfig, Locator, Page, PaginationContext, paginate_from, paginate_step_until,
        paginate_step_with_context,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageBounds {
    pub start: Locator,
    pub end: Locator,
}

pub struct PageCache {
    pub(crate) pages: Vec<PageBounds>,
    pub(crate) warnings: Vec<String>,
    chapter: Arc<Chapter>,
    config: LayoutConfig,
    end: Option<Locator>,
    begin: Option<Locator>,
    selected: usize,
    page: Page,
    /// Laid-out pages other than the selected one, kept so that turning to a
    /// prepared or recently shown page needs no layout.
    spares: Vec<(usize, Page)>,
    id: u64,
    /// Pagination of the chapter's remaining pages, absent once the index is complete.
    rest: Option<Box<Rest>>,
    /// How the page bounds relate to the persistent store.
    pub(crate) record: Record,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Record {
    /// Laid out here and not yet written.
    Unwritten,
    /// Laid out here and written, or failed to write once.
    Written,
    /// Read from a record made under the same layout key. Nothing proves that
    /// layout still produces these bounds, so each page is checked when it is
    /// laid out.
    Read,
}

/// Where indexing continues after the pages known so far.
struct Rest {
    next: Locator,
    context: PaginationContext,
    missing: usize,
}

fn missing_glyphs(page: &Page) -> usize {
    page.lines
        .iter()
        .flat_map(|line| &line.glyphs)
        .filter(|glyph| glyph.id == 0)
        .count()
}

fn missing_warning(missing: usize) -> Option<String> {
    (missing != 0).then(|| format!("{missing} glyphs unavailable in selected fonts"))
}

/// Distinguishes caches so results derived from one are never used with another.
fn next_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as u64
}

/// Lays out the page with the given bounds, which fails unless layout from
/// their start produces exactly them.
fn reproduce(
    chapter: &Chapter,
    fonts: &Fonts,
    config: LayoutConfig,
    expected: PageBounds,
    end: Option<Locator>,
    begin: Option<Locator>,
) -> Result<Page> {
    let mut produced = None;
    paginate_step_until(chapter, fonts, config, expected.start, end, begin, |page| {
        produced = Some(page);
        Ok(false)
    })?;
    let page = produced.ok_or(Error::Layout("missing indexed page"))?;
    if page.start != expected.start || page.end != expected.end {
        return Err(Error::Layout("page index no longer matches layout"));
    }
    Ok(page)
}

impl PageCache {
    const SPARES: usize = 2;

    pub(crate) fn id(&self) -> u64 {
        self.id
    }
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }
    pub(crate) fn config(&self) -> LayoutConfig {
        self.config
    }

    /// Whether every page of the chapter is indexed. An incomplete index only
    /// grows, and known page indexes change only when an index read from a
    /// record turns out not to match layout and is replaced.
    pub fn is_complete(&self) -> bool {
        self.rest.is_none()
    }

    /// Indexes one more page. Returns whether a page was added.
    pub fn extend(&mut self, fonts: &Fonts) -> Result<bool> {
        let Some(mut rest) = self.rest.take() else {
            return Ok(false);
        };
        if self.pages.len() == 16_384 {
            return Err(Error::Limit("chapter page index"));
        }
        let mut produced = None;
        let step = paginate_step_with_context(
            &self.chapter,
            fonts,
            self.config,
            rest.next,
            self.end,
            self.begin,
            &mut rest.context,
            |page| {
                produced = Some(page);
                Ok(false)
            },
        )?;
        let page = produced.ok_or(Error::Layout("missing indexed page"))?;
        rest.missing += missing_glyphs(&page);
        let index = self.pages.len();
        self.pages.push(PageBounds {
            start: page.start,
            end: page.end,
        });
        // The new page is usually the next one to be shown.
        if self.spares.len() < Self::SPARES {
            self.spares.push((index, page));
        }
        match step.next {
            Some(next) => {
                rest.next = next;
                self.rest = Some(rest);
            }
            None => self.warnings.extend(missing_warning(rest.missing)),
        }
        Ok(true)
    }

    /// Indexes until the page containing `locator` is known.
    pub fn index_until(&mut self, locator: Locator, fonts: &Fonts) -> Result<()> {
        while self.pages.last().is_some_and(|page| page.start <= locator) && self.extend(fonts)? {}
        Ok(())
    }

    /// Lays out an indexed page. `None` means that the index was read from a
    /// record and this page of it does not reproduce.
    fn layout(&self, index: usize, fonts: &Fonts) -> Result<Option<Page>> {
        let expected = *self
            .pages
            .get(index)
            .ok_or(Error::Layout("page index out of range"))?;
        let page = reproduce(
            &self.chapter,
            fonts,
            self.config,
            expected,
            self.end,
            self.begin,
        );
        match page {
            Ok(page) => Ok(Some(page)),
            // A failure that is not the record's shows again while re-indexing.
            Err(_) if self.record == Record::Read => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Replaces the index by one laid out from the chapter's start through the
    /// page containing `target`, and selects that page.
    fn reindex(&mut self, target: Locator, fonts: &Fonts) -> Result<()> {
        let mut job =
            PageIndexer::from_shared(Arc::clone(&self.chapter), self.config, target)?.partial();
        if let Some(begin) = self.begin {
            job.next = Some(begin);
        }
        job.begin = self.begin;
        job.end = self.end;
        while !job.step(fonts)? {}
        *self = job.finish()?;
        Ok(())
    }

    fn keep_spare(&mut self, index: usize, page: Page) {
        if self.spares.len() == Self::SPARES {
            let farthest = (0..self.spares.len())
                .max_by_key(|&i| self.spares[i].0.abs_diff(self.selected))
                .expect("nonempty spares");
            self.spares.swap_remove(farthest);
        }
        self.spares.push((index, page));
    }

    /// Lays out a page without selecting it. Returns whether layout ran. An
    /// index read from a record is replaced when the page does not reproduce;
    /// the selected page is then the one containing the start of the page
    /// selected before.
    pub fn prepare(&mut self, index: usize, fonts: &Fonts) -> Result<bool> {
        if index == self.selected || self.spares.iter().any(|(spare, _)| *spare == index) {
            return Ok(false);
        }
        let Some(page) = self.layout(index, fonts)? else {
            self.reindex(self.page.start, fonts)?;
            return Ok(true);
        };
        self.keep_spare(index, page);
        Ok(true)
    }

    pub fn new(
        chapter: Chapter,
        fonts: &Fonts,
        config: LayoutConfig,
        target: Locator,
    ) -> Result<Self> {
        let mut pages = Vec::new();
        let mut selected = 0;
        let mut retained = None;
        let warnings = paginate_from(
            &chapter,
            fonts,
            config,
            Locator {
                chapter: chapter.index,
                ..Default::default()
            },
            |page| {
                let bounds = PageBounds {
                    start: page.start,
                    end: page.end,
                };
                if retained.is_none() || page.start <= target {
                    selected = pages.len();
                    retained = Some(page);
                }
                pages.push(bounds);
                Ok(true)
            },
        )?;
        Ok(Self {
            chapter: Arc::new(chapter),
            end: None,
            begin: None,
            config,
            pages,
            warnings,
            selected,
            page: retained.ok_or(Error::Layout("no chapter page"))?,
            spares: Vec::new(),
            id: next_id(),
            rest: None,
            record: Record::Unwritten,
        })
    }

    pub(crate) fn chapter(&self) -> Arc<Chapter> {
        Arc::clone(&self.chapter)
    }

    pub fn page(&self) -> &Page {
        &self.page
    }
    pub fn bounds(&self) -> &[PageBounds] {
        &self.pages
    }
    pub fn page_for(&self, locator: Locator) -> usize {
        self.pages
            .partition_point(|page| page.start <= locator)
            .saturating_sub(1)
    }

    /// Builds the requested page before replacing the retained page. An index
    /// read from a record is replaced when the page does not reproduce, and
    /// the page containing the recorded start is selected.
    pub fn select(&mut self, index: usize, fonts: &Fonts) -> Result<()> {
        if index == self.selected {
            return Ok(());
        }
        let start = self
            .pages
            .get(index)
            .ok_or(Error::Layout("page index out of range"))?
            .start;
        self.turn(index, start, fonts)
    }

    /// Selects the page containing `locator`, indexing up to it first.
    pub fn select_at(&mut self, locator: Locator, fonts: &Fonts) -> Result<()> {
        self.index_until(locator, fonts)?;
        self.turn(self.page_for(locator), locator, fonts)
    }

    /// Selects the page `index`, or the page containing `target` when the
    /// index has to be replaced.
    fn turn(&mut self, index: usize, target: Locator, fonts: &Fonts) -> Result<()> {
        if index == self.selected {
            return Ok(());
        }
        let page = match self.spares.iter().position(|(spare, _)| *spare == index) {
            Some(spare) => self.spares.swap_remove(spare).1,
            None => match self.layout(index, fonts)? {
                Some(page) => page,
                None => return self.reindex(target, fonts),
            },
        };
        let previous = (self.selected, std::mem::replace(&mut self.page, page));
        self.selected = index;
        self.keep_spare(previous.0, previous.1);
        Ok(())
    }
}

/// Cooperative index construction. A step emits one page and inspects the
/// candidate line or image that starts the next page. Dropping cancels the job.
pub struct PageIndexer {
    chapter: Arc<Chapter>,
    config: LayoutConfig,
    target: Locator,
    next: Option<Locator>,
    end: Option<Locator>,
    begin: Option<Locator>,
    pages: Vec<PageBounds>,
    selected: usize,
    retained: Option<Page>,
    missing: usize,
    context: PaginationContext,
    /// Finish as soon as the target page is known instead of at the chapter end.
    partial: bool,
    /// Page bounds came from a persistent store and are checked on use.
    seeded: bool,
}

impl PageIndexer {
    pub fn new(chapter: Chapter, config: LayoutConfig, target: Locator) -> Result<Self> {
        Self::from_shared(Arc::new(chapter), config, target)
    }
    pub(crate) fn for_preview(
        chapter: Arc<Chapter>,
        config: LayoutConfig,
        target: Locator,
    ) -> Result<Self> {
        // Nested note bodies select the innermost complete containing range.
        let range = chapter
            .notes
            .iter()
            .filter(|range| range.contains(&target))
            .max_by_key(|range| (range.start, std::cmp::Reverse(range.end)))
            .cloned();
        let Some(range) = range else {
            return Self::from_shared(chapter, config, target);
        };
        let mut job = Self::from_shared(chapter, config, target)?;
        job.next = Some(range.start);
        job.end = Some(range.end);
        job.begin = Some(range.start);
        Ok(job)
    }

    pub(crate) fn from_shared(
        chapter: Arc<Chapter>,
        config: LayoutConfig,
        target: Locator,
    ) -> Result<Self> {
        config.validate()?;
        let next = Some(Locator {
            chapter: chapter.index,
            ..Default::default()
        });
        Ok(Self {
            chapter,
            config,
            target,
            next,
            end: None,
            begin: None,
            pages: Vec::new(),
            selected: 0,
            retained: None,
            missing: 0,
            context: PaginationContext::default(),
            partial: false,
            seeded: false,
        })
    }
    /// Lets the job finish once the page containing the target is laid out;
    /// the resulting cache indexes the remaining pages on demand.
    pub fn partial(mut self) -> Self {
        self.partial = true;
        self
    }
    /// Starts from page bounds recorded for the same chapter, fonts and
    /// layout, so only the target page needs layout. Bounds that do not
    /// reproduce are discarded and the chapter is indexed afresh.
    pub(crate) fn seeded(mut self, pages: Vec<PageBounds>) -> Self {
        let ordered = pages.windows(2).all(|pair| pair[0].start < pair[1].start);
        if !pages.is_empty() && pages.len() <= 16_384 && ordered && self.pages.is_empty() {
            self.selected = pages
                .partition_point(|page| page.start <= self.target)
                .saturating_sub(1);
            self.pages = pages;
            self.next = None;
            self.seeded = true;
        }
        self
    }
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
    pub(crate) fn bounds(&self) -> &[PageBounds] {
        &self.pages
    }
    pub fn is_finished(&self) -> bool {
        self.next.is_none()
    }
    /// Whether [`Self::finish`] can produce a cache showing the target page.
    fn is_ready(&self) -> bool {
        self.retained.is_some()
            && (self.is_finished()
                || self.partial
                    && self
                        .pages
                        .last()
                        .is_some_and(|page| page.start > self.target))
    }
    /// Lays out the target page of a seeded index.
    fn retain_seeded(&mut self, fonts: &Fonts) -> Result<()> {
        let page = reproduce(
            &self.chapter,
            fonts,
            self.config,
            self.pages[self.selected],
            self.end,
            self.begin,
        );
        match page {
            Ok(page) => self.retained = Some(page),
            Err(_) => {
                // The recorded index belongs to another layout; start over. A
                // failure that is not the record's shows again on the way.
                self.pages.clear();
                self.selected = 0;
                self.seeded = false;
                self.next = Some(self.begin.unwrap_or(Locator {
                    chapter: self.chapter.index,
                    ..Default::default()
                }));
            }
        }
        Ok(())
    }
    pub fn step(&mut self, fonts: &Fonts) -> Result<bool> {
        if self.seeded && self.retained.is_none() {
            self.retain_seeded(fonts)?;
            return Ok(self.is_ready());
        }
        let Some(start) = self.next else {
            return Ok(true);
        };
        if self.pages.len() == 16_384 {
            return Err(Error::Limit("chapter page index"));
        }
        let mut produced = None;
        let result = paginate_step_with_context(
            &self.chapter,
            fonts,
            self.config,
            start,
            self.end,
            self.begin,
            &mut self.context,
            |page| {
                produced = Some(page);
                Ok(false)
            },
        )?;
        let page = produced.ok_or(Error::Layout("missing indexed page"))?;
        self.missing += missing_glyphs(&page);
        let bounds = PageBounds {
            start: page.start,
            end: page.end,
        };
        if self.retained.is_none() || page.start <= self.target {
            self.selected = self.pages.len();
            self.retained = Some(page);
        }
        self.pages.push(bounds);
        self.next = result.next;
        Ok(self.is_ready())
    }
    pub fn finish(self) -> Result<PageCache> {
        if !self.is_ready() {
            return Err(Error::Layout("chapter indexing incomplete"));
        }
        let mut warnings = self.chapter.warnings.clone();
        let rest = self.next.map(|next| {
            Box::new(Rest {
                next,
                context: self.context,
                missing: self.missing,
            })
        });
        if rest.is_none() {
            warnings.extend(missing_warning(self.missing));
        }
        Ok(PageCache {
            chapter: self.chapter,
            end: self.end,
            begin: self.begin,
            config: self.config,
            pages: self.pages,
            warnings,
            selected: self.selected,
            page: self.retained.ok_or(Error::Layout("no chapter page"))?,
            spares: Vec::new(),
            id: next_id(),
            rest,
            record: if self.seeded {
                Record::Read
            } else {
                Record::Unwritten
            },
        })
    }
}

#[cfg(test)]
mod note_range_tests {
    use super::*;
    use crate::{
        document::{Block, Document},
        layout::{Font, rasterize_display},
    };

    #[test]
    fn note_indexes_share_source_and_match_extracted_copy_on_every_page() {
        let root =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        let fonts = Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap());
        let mut document = Document::open(root.join("semantic-notes.epub")).unwrap();
        for anchor in ["long", "inline"] {
            let target = document
                .resolve_link("text/one.xhtml", &format!("two.xhtml#{anchor}"))
                .unwrap();
            let chapter = Arc::new(document.chapter(target.chapter).unwrap());
            let range = chapter
                .notes
                .iter()
                .find(|r| r.contains(&target))
                .unwrap()
                .clone();
            let config = LayoutConfig::default();
            let mut actual =
                PageIndexer::for_preview(Arc::clone(&chapter), config, target).unwrap();
            assert!(Arc::ptr_eq(&actual.chapter, &chapter));
            let mut extracted = (*chapter).clone();
            if range.end.byte == 0 {
                extracted.blocks.truncate(range.end.block);
            } else {
                extracted.blocks.truncate(range.end.block + 1);
                let Block::Text(paragraph) = extracted.blocks.last_mut().unwrap() else {
                    panic!("text end")
                };
                paragraph.text.truncate(range.end.byte);
                paragraph.spans.retain(|s| s.range.start < range.end.byte);
                for span in &mut paragraph.spans {
                    span.range.end = span.range.end.min(range.end.byte);
                }
            }
            let mut expected = PageIndexer::new(extracted, config, target).unwrap();
            expected.next = Some(range.start);
            while !actual.step(&fonts).unwrap() {}
            while !expected.step(&fonts).unwrap() {}
            let mut actual = actual.finish().unwrap();
            let mut expected = expected.finish().unwrap();
            assert!(Arc::ptr_eq(&actual.chapter, &chapter));
            assert_eq!(actual.bounds(), expected.bounds());
            // Reverse traversal forces reconstruction of both first and last pages.
            for index in (0..actual.bounds().len()).rev() {
                actual.select(index, &fonts).unwrap();
                expected.select(index, &fonts).unwrap();
                let pixels = |cache: &PageCache| {
                    rasterize_display(cache.page(), &fonts, config, Default::default(), 4, |_| {
                        Err(Error::Document("no image".into()))
                    })
                    .unwrap()
                };
                assert_eq!(pixels(&actual), pixels(&expected));
            }
        }
    }

    #[test]
    fn invalid_range_ends_fail_before_emitting_pages() {
        let root =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        let fonts = Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap());
        let mut document = Document::from_text("Range", "café").unwrap();
        let chapter = document.chapter(0).unwrap();
        for end in [
            Locator {
                chapter: 1,
                block: 0,
                byte: 0,
            },
            Locator::default(),
            Locator {
                chapter: 0,
                block: 2,
                byte: 0,
            },
            Locator {
                chapter: 0,
                block: 1,
                byte: 1,
            },
            Locator {
                chapter: 0,
                block: 0,
                byte: 4,
            },
        ] {
            assert!(
                paginate_step_until(
                    &chapter,
                    &fonts,
                    LayoutConfig::default(),
                    Locator::default(),
                    Some(end),
                    None,
                    |_| panic!("invalid range emitted a page")
                )
                .is_err()
            );
        }
    }
}

#[cfg(test)]
mod analysis_cache_tests {
    use super::*;
    use crate::{document::Document, layout::Font};

    #[test]
    #[ignore = "manual release-mode scaling measurement"]
    fn long_paragraph_indexing_scaling() {
        let fonts = Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
                .unwrap(),
        );
        for words in [16_000, 32_000, 64_000] {
            let text = "word ".repeat(words);
            let mut document = Document::from_text("Scaling", &text).unwrap();
            let mut job = PageIndexer::new(
                document.chapter(0).unwrap(),
                LayoutConfig::default(),
                Locator::default(),
            )
            .unwrap();
            let started = std::time::Instant::now();
            while !job.step(&fonts).unwrap() {}
            eprintln!(
                "bytes={} pages={} elapsed={:?}",
                text.len(),
                job.page_count(),
                started.elapsed()
            );
            assert_eq!(job.context.analyses, 1);
        }
    }

    #[test]
    fn incremental_pages_reuse_analysis_and_match_fresh_reconstruction() {
        let fonts = Fonts::new(
            Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
                .unwrap(),
        );
        let text = format!(
            "{}\n\n{}",
            "Latin (אבג) soft\u{ad}hyphen αβγ ".repeat(180),
            "אבג Latin xyz ".repeat(180)
        );
        let mut document = Document::from_text("Cache", &text).unwrap();
        let chapter = document.chapter(0).unwrap();
        let config = LayoutConfig {
            width: 200,
            height: 240,
            margin: 12,
            ..Default::default()
        };
        let mut expected = Vec::new();
        paginate_from(&chapter, &fonts, config, Locator::default(), |page| {
            expected.push(page);
            Ok(true)
        })
        .unwrap();
        // A target after the chapter retains the page produced at each step.
        let target = Locator {
            chapter: usize::MAX,
            ..Default::default()
        };
        let mut job = PageIndexer::new(chapter.clone(), config, target).unwrap();
        loop {
            let finished = job.step(&fonts).unwrap();
            let actual = job.retained.as_ref().unwrap();
            let page = &expected[job.page_count() - 1];
            assert_eq!(actual.start, page.start);
            assert_eq!(actual.end, page.end);
            assert_eq!(format!("{:?}", actual.lines), format!("{:?}", page.lines));
            if finished {
                break;
            }
        }
        assert!(job.page_count() > 10);
        assert_eq!(job.context.analyses, chapter.blocks.len());
        let mut cache = job.finish().unwrap();
        assert_eq!(cache.pages.len(), expected.len());
        for (index, page) in expected.iter().enumerate() {
            cache.select(index, &fonts).unwrap();
            assert_eq!(cache.page.start, page.start);
            assert_eq!(cache.page.end, page.end);
            assert_eq!(
                format!("{:?}", cache.page.lines),
                format!("{:?}", page.lines)
            );
        }
    }
}
