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
}

impl PageCache {
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

    /// Builds the requested page before replacing the retained page.
    pub fn select(&mut self, index: usize, fonts: &Fonts) -> Result<()> {
        if index == self.selected {
            return Ok(());
        }
        let expected = *self
            .pages
            .get(index)
            .ok_or(Error::Layout("page index out of range"))?;
        let mut replacement = None;
        paginate_step_until(
            &self.chapter,
            fonts,
            self.config,
            expected.start,
            self.end,
            self.begin,
            |page| {
                replacement = Some(page);
                Ok(false)
            },
        )?;
        let page = replacement.ok_or(Error::Layout("missing indexed page"))?;
        if page.start != expected.start || page.end != expected.end {
            return Err(Error::Layout("page index no longer matches layout"));
        }
        self.page = page;
        self.selected = index;
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
        })
    }
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
    pub fn is_finished(&self) -> bool {
        self.next.is_none()
    }
    pub fn step(&mut self, fonts: &Fonts) -> Result<bool> {
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
        self.missing += page
            .lines
            .iter()
            .flat_map(|l| &l.glyphs)
            .filter(|g| g.id == 0)
            .count();
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
        Ok(self.is_finished())
    }
    pub fn finish(self) -> Result<PageCache> {
        if !self.is_finished() {
            return Err(Error::Layout("chapter indexing incomplete"));
        }
        let mut warnings = self.chapter.warnings.clone();
        if self.missing != 0 {
            warnings.push(format!(
                "{} glyphs unavailable in selected fonts",
                self.missing
            ));
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
