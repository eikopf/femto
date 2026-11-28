use std::{
    borrow::Cow,
    io::{Read, Write},
    ops::Range,
    path::Path,
};

use rustybuzz::{Direction, Face, UnicodeBuffer};
use serde::{Deserialize, Serialize};
use swash::{
    CacheKey, FontRef,
    scale::{Render, ScaleContext, Source},
    zeno::{Format, Vector},
};
use unicode_bidi::BidiInfo;
use unicode_linebreak::{BreakOpportunity, linebreaks};
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

use crate::{
    Error, Result,
    document::{Block, Chapter, Hyphens, Paragraph, Span, TextStyle},
    image::{self, Size},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
pub struct Locator {
    pub chapter: usize,
    pub block: usize,
    pub byte: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Hyphenation {
    #[default]
    Book,
    Off,
    English,
    German,
    French,
    Spanish,
    Italian,
    Dutch,
    Portuguese,
}

impl Hyphenation {
    pub fn next(self) -> Self {
        match self {
            Self::Book => Self::Off,
            Self::Off => Self::English,
            Self::English => Self::German,
            Self::German => Self::French,
            Self::French => Self::Spanish,
            Self::Spanish => Self::Italian,
            Self::Italian => Self::Dutch,
            Self::Dutch => Self::Portuguese,
            Self::Portuguese => Self::Book,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Book => "Book",
            Self::Off => "Off",
            Self::English => "English",
            Self::German => "German",
            Self::French => "French",
            Self::Spanish => "Spanish",
            Self::Italian => "Italian",
            Self::Dutch => "Dutch",
            Self::Portuguese => "Portuguese",
        }
    }
    fn language(self) -> Option<&'static str> {
        match self {
            Self::Book | Self::Off => None,
            Self::English => Some("en"),
            Self::German => Some("de"),
            Self::French => Some("fr"),
            Self::Spanish => Some("es"),
            Self::Italian => Some("it"),
            Self::Dutch => Some("nl"),
            Self::Portuguese => Some("pt"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutConfig {
    pub width: u32,
    pub height: u32,
    pub margin: u32,
    #[serde(deserialize_with = "crate::deserialize_f32")]
    pub font_size: f32,
    #[serde(deserialize_with = "crate::deserialize_f32")]
    pub line_height: f32,
    #[serde(default)]
    pub hyphenation: Hyphenation,
    /// Rows at the bottom that book content leaves free, for a status line.
    /// They overlap the bottom margin, so a margin at least as deep is
    /// unchanged by them.
    #[serde(default)]
    pub footer: u32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            width: 480,
            height: 800,
            margin: 24,
            font_size: 24.0,
            line_height: 1.35,
            hyphenation: Hyphenation::Book,
            footer: 0,
        }
    }
}

impl LayoutConfig {
    /// The rows below book content: the margin, or the footer if deeper.
    fn below(&self) -> u32 {
        self.margin.max(self.footer)
    }
    pub fn validate(&self) -> Result<()> {
        if self.width > 2048 || self.height > 2048 || self.width < 80 || self.height < 80 {
            return Err(Error::Layout("viewport must be between 80 and 2048 pixels"));
        }
        if !self.font_size.is_finite()
            || !(8.0..=96.0).contains(&self.font_size)
            || !self.line_height.is_finite()
            || !(1.0..=3.0).contains(&self.line_height)
        {
            return Err(Error::Layout("invalid font size or line height"));
        }
        let margin = self
            .margin
            .checked_mul(2)
            .ok_or(Error::Layout("margin overflow"))?;
        let vertical = self
            .margin
            .checked_add(self.below())
            .ok_or(Error::Layout("margin overflow"))?;
        if margin >= self.width
            || vertical >= self.height
            || ((self.height - vertical) as f32) < self.font_size * self.line_height * 3.0
        {
            return Err(Error::Layout("margins leave insufficient reading area"));
        }
        Ok(())
    }
}

/// The shaping face parsed once from the font data it borrows.
#[derive(yoke::Yokeable)]
struct Parsed<'a>(Face<'a>);

pub struct Font {
    /// Font bytes with their parsed face; parsing per shaping call
    /// dominated layout time. Boxed because the face is several kilobytes
    /// and fonts are moved by value on small device stacks.
    face: Box<yoke::Yoke<Parsed<'static>, Box<[u8]>>>,
    key: CacheKey,
    offset: u32,
    /// Content hash, for keys that must survive restarts.
    digest: u64,
    /// Results that depend only on the font data and are otherwise recomputed
    /// for every candidate line.
    shaping: std::sync::Mutex<ShapingCache>,
}

type PlanKey = (Direction, rustybuzz::Script, Option<rustybuzz::Language>);

#[derive(Default)]
struct ShapingCache {
    /// Vertical glyph extents in font units; `None` for glyphs without outlines.
    extents: std::collections::HashMap<u16, Option<(i16, i16)>>,
    plans: Vec<(PlanKey, rustybuzz::ShapePlan)>,
}

impl Font {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        Self::load_bounded(path, 16 * 1024 * 1024)
    }
    pub(crate) fn load_bounded(path: impl AsRef<Path>, limit: u64) -> Result<Self> {
        let path = path.as_ref();
        let metadata = std::fs::metadata(path)?;
        if !metadata.is_file() {
            return Err(Error::Font);
        }
        if metadata.len() > limit {
            return Err(Error::Limit("font size"));
        }
        let file = std::fs::File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(Error::Font);
        }
        let mut data = Vec::new();
        file.take(limit.saturating_add(1)).read_to_end(&mut data)?;
        if data.len() as u64 > limit {
            return Err(Error::Limit("font size"));
        }
        Self::from_bytes(data)
    }
    pub(crate) fn byte_len(&self) -> usize {
        self.data().len()
    }
    fn data(&self) -> &[u8] {
        self.face.backing_cart()
    }
    fn face(&self) -> &Face<'_> {
        &self.face.get().0
    }
    pub fn from_bytes(data: Vec<u8>) -> Result<Self> {
        let reference = FontRef::from_index(&data, 0).ok_or(Error::Font)?;
        let (key, offset) = (reference.key, reference.offset);
        let digest = crate::page_index::fnv(crate::page_index::FNV_START, &data);
        let face = Box::new(yoke::Yoke::try_attach_to_cart(
            data.into_boxed_slice(),
            |bytes| Face::from_slice(bytes, 0).map(Parsed).ok_or(Error::Font),
        )?);
        Ok(Self {
            digest,
            face,
            key,
            offset,
            shaping: Default::default(),
        })
    }
    /// Shapes with a plan retained per direction, script and language.
    fn shape(&self, face: &Face<'_>, buffer: UnicodeBuffer) -> rustybuzz::GlyphBuffer {
        const PLANS: usize = 16;
        let key = (buffer.direction(), buffer.script(), buffer.language());
        let mut cache = self.shaping.lock().unwrap_or_else(|e| e.into_inner());
        let index = match cache.plans.iter().position(|(known, _)| *known == key) {
            Some(index) => index,
            None => {
                if cache.plans.len() == PLANS {
                    cache.plans.clear();
                }
                let plan = rustybuzz::ShapePlan::new(face, key.0, Some(key.1), key.2.as_ref(), &[]);
                cache.plans.push((key, plan));
                cache.plans.len() - 1
            }
        };
        rustybuzz::shape_with_plan(face, &cache.plans[index].1, buffer)
    }
    fn vertical_extents(&self, face: &Face<'_>, glyph: u16) -> Option<(i16, i16)> {
        *self
            .shaping
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extents
            .entry(glyph)
            .or_insert_with(|| {
                face.glyph_bounding_box(rustybuzz::ttf_parser::GlyphId(glyph))
                    .map(|bounds| (bounds.y_min, bounds.y_max))
            })
    }
    fn swash(&self) -> FontRef<'_> {
        FontRef {
            data: self.data(),
            key: self.key,
            offset: self.offset,
        }
    }
}

pub struct Fonts {
    pub regular: Font,
    pub bold: Option<Font>,
    pub italic: Option<Font>,
    pub bold_italic: Option<Font>,
    pub fallback: Vec<Font>,
    /// Glyph masks shared by every page and menu rendered with these fonts.
    masks: std::sync::Mutex<crate::glyph_cache::GlyphCache>,
}

impl Fonts {
    pub fn new(regular: Font) -> Self {
        Self {
            regular,
            bold: None,
            italic: None,
            bold_italic: None,
            fallback: Vec::new(),
            masks: std::sync::Mutex::new(crate::glyph_cache::GlyphCache::new(512 * 1024, 1024)),
        }
    }
    /// Content hash of every font slot in selection order.
    pub(crate) fn digest(&self) -> u64 {
        let slots = [
            Some(&self.regular),
            self.bold.as_ref(),
            self.italic.as_ref(),
            self.bold_italic.as_ref(),
        ];
        slots
            .into_iter()
            .chain(self.fallback.iter().map(Some))
            .fold(crate::page_index::FNV_START, |hash, font| {
                crate::page_index::fnv(hash, &font.map_or(0, |font| font.digest).to_le_bytes())
            })
    }
    /// Identifies the font slots and raster settings that determine a mask.
    fn mask_tag(&self, text: crate::display::TextRaster) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for id in 0..4 + self.fallback.len() as u8 {
            self.get(id).key.value().hash(&mut hasher);
        }
        (text.hinting, text.stroke.to_bits()).hash(&mut hasher);
        hasher.finish()
    }
    fn select(&self, style: TextStyle) -> (u8, &Font) {
        if style.bold
            && style.italic
            && let Some(font) = &self.bold_italic
        {
            return (3, font);
        }
        if style.bold
            && let Some(font) = &self.bold
        {
            return (1, font);
        }
        if style.italic
            && let Some(font) = &self.italic
        {
            return (2, font);
        }
        (0, &self.regular)
    }
    fn get(&self, id: u8) -> &Font {
        match id {
            1 => self.bold.as_ref(),
            2 => self.italic.as_ref(),
            3 => self.bold_italic.as_ref(),
            4.. => self.fallback.get((id - 4) as usize),
            _ => None,
        }
        .unwrap_or(&self.regular)
    }

    fn segments(&self, text: &str, base: usize, style: TextStyle) -> Vec<(Range<usize>, u8)> {
        let (preferred_id, preferred) = self.select(style);
        let covers = |font: &Font, text: &str| {
            let map = font.swash().charmap();
            text.chars().all(|c| {
                c.is_control()
                    || matches!(
                        c,
                        '\u{ad}' | '\u{200c}' | '\u{200d}' | '\u{fe0e}' | '\u{fe0f}'
                    )
                    || map.map(c) != 0
            })
        };
        let mut segments: Vec<(Range<usize>, u8)> = Vec::new();
        for (offset, grapheme) in text.grapheme_indices(true) {
            let font = if grapheme == "\u{ad}" {
                segments.last().map_or(preferred_id, |(_, font)| *font)
            } else if covers(preferred, grapheme) {
                preferred_id
            } else {
                self.fallback
                    .iter()
                    .position(|font| covers(font, grapheme))
                    .map(|i| i as u8 + 4)
                    .unwrap_or(preferred_id)
            };
            let end = base + offset + grapheme.len();
            if let Some((range, previous)) = segments.last_mut()
                && *previous == font
            {
                range.end = end;
            } else {
                segments.push((base + offset..end, font));
            }
        }
        segments
    }
}

#[derive(Clone, Debug)]
pub struct Glyph {
    pub id: u16,
    pub font: u8,
    pub size: f32,
    pub x: f32,
    pub y: f32,
    pub advance: f32,
    pub cluster: usize,
}

#[derive(Clone, Debug)]
pub struct Line {
    pub start: Locator,
    pub end: Locator,
    pub text: String,
    pub hyphenated: bool,
    pub glyphs: Vec<Glyph>,
    pub marker_glyphs: Vec<Glyph>,
    pub width: f32,
    pub height: f32,
    pub ascent: f32,
    pub descent: f32,
    pub baseline: f32,
}

#[derive(Clone, Debug)]
pub struct Page {
    pub start: Locator,
    pub end: Locator,
    pub lines: Vec<Line>,
    pub images: Vec<PlacedImage>,
}

#[derive(Clone, Debug)]
pub struct PlacedImage {
    pub source: String,
    pub x: u32,
    pub y: u32,
    pub size: Size,
    pub location: Locator,
}

impl Page {
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty() && self.images.is_empty()
    }
}

#[derive(Debug)]
pub struct Pagination {
    pub config: LayoutConfig,
    pub pages: Vec<Page>,
    pub warnings: Vec<String>,
}

impl Pagination {
    pub fn page_for(&self, locator: Locator) -> usize {
        self.pages
            .partition_point(|page| page.start <= locator)
            .saturating_sub(1)
    }
}

pub fn paginate(chapter: &Chapter, fonts: &Fonts, config: LayoutConfig) -> Result<Pagination> {
    let mut pages = Vec::new();
    let mut glyphs = 0;
    let warnings = paginate_from(
        chapter,
        fonts,
        config,
        Locator {
            chapter: chapter.index,
            ..Default::default()
        },
        |page| {
            glyphs += page.lines.iter().map(|l| l.glyphs.len()).sum::<usize>();
            if glyphs > 131_072 {
                return Err(Error::Limit("chapter glyph cache"));
            }
            if pages.len() == 256 {
                return Err(Error::Limit("chapter page cache"));
            }
            pages.push(page);
            Ok(true)
        },
    )?;
    Ok(Pagination {
        config,
        pages,
        warnings,
    })
}

/// Emits pages while retaining only the current page. Resume at a previously
/// emitted page start with the same chapter, fonts and layout configuration.
/// Returning false stops before laying out another page.
pub fn paginate_from(
    chapter: &Chapter,
    fonts: &Fonts,
    config: LayoutConfig,
    start_at: Locator,
    mut sink: impl FnMut(Page) -> Result<bool>,
) -> Result<Vec<String>> {
    Ok(paginate_step(chapter, fonts, config, start_at, &mut sink)?.warnings)
}

pub(crate) struct PageStep {
    pub warnings: Vec<String>,
    pub next: Option<Locator>,
}

pub(crate) fn paginate_step(
    chapter: &Chapter,
    fonts: &Fonts,
    config: LayoutConfig,
    start_at: Locator,
    sink: impl FnMut(Page) -> Result<bool>,
) -> Result<PageStep> {
    paginate_step_until(chapter, fonts, config, start_at, None, None, sink)
}

pub(crate) fn paginate_step_until(
    chapter: &Chapter,
    fonts: &Fonts,
    config: LayoutConfig,
    start_at: Locator,
    end_at: Option<Locator>,
    begin_at: Option<Locator>,
    sink: impl FnMut(Page) -> Result<bool>,
) -> Result<PageStep> {
    paginate_step_with_context(
        chapter,
        fonts,
        config,
        start_at,
        end_at,
        begin_at,
        &mut PaginationContext::default(),
        sink,
    )
}

#[allow(clippy::too_many_arguments)] // Same pagination inputs plus retained cursor analysis.
pub(crate) fn paginate_step_with_context(
    chapter: &Chapter,
    fonts: &Fonts,
    config: LayoutConfig,
    start_at: Locator,
    end_at: Option<Locator>,
    begin_at: Option<Locator>,
    context: &mut PaginationContext,
    mut sink: impl FnMut(Page) -> Result<bool>,
) -> Result<PageStep> {
    if let Some(begin) = begin_at {
        if begin.chapter != chapter.index || begin > start_at || begin.block >= chapter.blocks.len()
        {
            return Err(Error::Layout("invalid pagination context start"));
        }
        if begin.byte != 0
            && !matches!(chapter.blocks.get(begin.block), Some(Block::Text(p)) if p.text.is_char_boundary(begin.byte))
        {
            return Err(Error::Layout("invalid pagination context byte"));
        }
    }
    if let Some(end) = end_at {
        if end.chapter != chapter.index || end <= start_at || end.block > chapter.blocks.len() {
            return Err(Error::Layout("invalid pagination end"));
        }
        if end.byte != 0
            && !matches!(chapter.blocks.get(end.block), Some(Block::Text(p)) if p.text.is_char_boundary(end.byte))
        {
            return Err(Error::Layout("invalid pagination end byte"));
        }
    }
    if start_at.chapter != chapter.index
        || (start_at.block >= chapter.blocks.len()
            && !(start_at.block == 0 && chapter.blocks.is_empty()))
    {
        return Err(Error::Layout("invalid pagination start"));
    }
    config.validate()?;
    if fonts.fallback.len() > 8 {
        return Err(Error::Limit("fallback fonts"));
    }
    let available = (config.width - 2 * config.margin) as f32;
    let bottom = (config.height - config.below()) as f32;
    let mut warnings = chapter.warnings.clone();
    let emitted = std::cell::Cell::new(0);
    let mut missing = 0;
    let mut emit = |page: Page| -> Result<bool> {
        emitted.set(emitted.get() + 1);
        if emitted.get() > 16_384 {
            return Err(Error::Limit("chapter page index"));
        }
        missing += page
            .lines
            .iter()
            .flat_map(|l| l.glyphs.iter().chain(&l.marker_glyphs))
            .filter(|g| g.id == 0)
            .count();
        sink(page)
    };
    let mut page = Page {
        start: Locator {
            chapter: chapter.index,
            ..Default::default()
        },
        end: Locator {
            chapter: chapter.index,
            ..Default::default()
        },
        lines: vec![],
        images: vec![],
    };
    let mut y = config.margin as f32;
    let mut glyph_count = 0;
    for (block_index, block) in chapter.blocks.iter().enumerate().skip(start_at.block) {
        if end_at.is_some_and(|end| {
            block_index > end.block || (block_index == end.block && end.byte == 0)
        }) {
            break;
        }
        let placeholder;
        let clipped;
        let paragraph = match block {
            Block::Text(paragraph) => {
                let begin = begin_at
                    .filter(|begin| begin.block == block_index)
                    .map_or(0, |begin| begin.byte);
                let end = end_at
                    .filter(|end| end.block == block_index)
                    .map_or(paragraph.text.len(), |end| end.byte);
                if begin != 0 || end < paragraph.text.len() {
                    // Neutral padding preserves original source-byte coordinates
                    // without admitting text outside the note to bidi or shaping.
                    let mut text = " ".repeat(begin);
                    text.push_str(&paragraph.text[begin..end]);
                    clipped = Paragraph {
                        list: if begin == 0 {
                            paragraph.list.clone()
                        } else {
                            None
                        },
                        text,
                        spans: paragraph
                            .spans
                            .iter()
                            .filter(|span| span.range.start < end && span.range.end > begin)
                            .map(|span| {
                                let mut span = span.clone();
                                span.range.start = span.range.start.max(begin);
                                span.range.end = span.range.end.min(end);
                                span
                            })
                            .collect(),
                    };
                    &clipped
                } else {
                    paragraph
                }
            }
            Block::Image(image) => {
                let Some(size) = image_size(
                    image,
                    image.intrinsic.unwrap_or(crate::image::Size {
                        width: 1,
                        height: 1,
                    }),
                    config.width - 2 * config.margin,
                    config.height - config.margin - config.below(),
                )?
                else {
                    continue;
                };
                if image.intrinsic.is_some() {
                    let location = Locator {
                        chapter: chapter.index,
                        block: block_index,
                        byte: 0,
                    };
                    if y + size.height as f32 > bottom && !page.is_empty() {
                        if !emit(page)? {
                            if missing != 0 {
                                warnings.push(format!(
                                    "{missing} glyphs unavailable in selected fonts"
                                ));
                            }
                            return Ok(PageStep {
                                warnings,
                                next: Some(location),
                            });
                        }
                        glyph_count = 0;
                        page = Page {
                            start: location,
                            end: location,
                            lines: vec![],
                            images: vec![],
                        };
                        y = config.margin as f32;
                    }
                    if page.is_empty() {
                        page.start = location;
                    }
                    page.end = Locator {
                        byte: 1,
                        ..location
                    };
                    page.images.push(PlacedImage {
                        source: image.source.clone(),
                        x: (config.width - size.width) / 2,
                        y: y.ceil() as u32,
                        size,
                        location,
                    });
                    y = y.ceil() + size.height as f32 + config.font_size * 0.4;
                    continue;
                }
                let description: String = image.alt.chars().take(160).collect();
                let text = format!(
                    "[Image unavailable{}{}]",
                    if description.is_empty() { "" } else { ": " },
                    description
                );
                placeholder = Paragraph {
                    list: None,
                    spans: vec![Span {
                        language: None,
                        range: 0..text.len(),
                        style: TextStyle::default(),
                    }],
                    text,
                };
                &placeholder
            }
        };
        let direction = paragraph
            .spans
            .first()
            .map_or(crate::document::BaseDirection::Auto, |span| {
                span.style.direction
            });
        let base = match direction {
            crate::document::BaseDirection::Auto => None,
            crate::document::BaseDirection::Ltr => Some(unicode_bidi::Level::ltr()),
            crate::document::BaseDirection::Rtl => Some(unicode_bidi::Level::rtl()),
        };
        if context
            .paragraph
            .as_ref()
            .is_none_or(|(index, _, _)| *index != block_index)
        {
            // Release the previous block before allocating its replacement.
            context.paragraph = None;
            let hyphens = if matches!(block, Block::Text(_)) {
                hyphen_points(paragraph, &chapter.language, config.hyphenation)
            } else {
                Vec::new()
            };
            context.paragraph = Some((block_index, Shaping::new(&paragraph.text, base), hyphens));
            #[cfg(test)]
            {
                context.analyses += 1;
            }
        }
        let (_, shaping, hyphens) = context.paragraph.as_mut().unwrap();
        let marker = if let Some(text) = paragraph.list.as_ref().map(|item| &item.marker_label) {
            let style = paragraph
                .spans
                .first()
                .map(|span| span.style)
                .unwrap_or_default();
            let marker = Paragraph {
                list: None,
                text: text.clone(),
                spans: vec![Span {
                    language: None,
                    range: 0..text.len(),
                    style,
                }],
            };
            Some(shape(
                &marker,
                &Shaping::new(text, Some(unicode_bidi::Level::ltr())),
                0..text.len(),
                fonts,
                config.font_size,
            )?)
        } else {
            None
        };
        let gutter = paragraph.list.as_ref().map_or(0.0, |item| {
            (item.depth as f32 * config.font_size * 1.5)
                .min(available / 2.0)
                .max(
                    marker
                        .as_ref()
                        .map_or(0.0, |line| line.width + config.font_size * 0.3),
                )
        });
        if gutter >= available {
            return Err(Error::Layout("list marker exceeds reading width"));
        }
        let column = available - gutter;
        let mut start = if block_index == start_at.block {
            start_at.byte
        } else {
            0
        };
        if start > paragraph.text.len() || !paragraph.text.is_char_boundary(start) {
            return Err(Error::Layout("invalid pagination start"));
        }
        while start < paragraph.text.len() {
            let indent = if start == 0 {
                use crate::document::TextIndent;
                let value = match paragraph
                    .spans
                    .first()
                    .map(|span| span.style.indent)
                    .unwrap_or_default()
                {
                    TextIndent::Pixels(n) => n,
                    TextIndent::Em(n) => n * config.font_size,
                    TextIndent::Percent(n) => n * column / 100.0,
                };
                if !value.is_finite() {
                    return Err(Error::Layout("invalid text indent"));
                }
                value.clamp(
                    -(config.margin as f32),
                    (column - config.font_size).max(0.0),
                )
            } else {
                0.0
            };
            let line_available = column - indent;
            shaping.scripts.prepare(&paragraph.text, start);
            let (end, mut line) = break_line(
                paragraph,
                shaping,
                start,
                line_available,
                fonts,
                config.font_size,
                hyphens,
            )?;
            line.start = Locator {
                chapter: chapter.index,
                block: block_index,
                byte: start,
            };
            line.end = Locator {
                chapter: chapter.index,
                block: block_index,
                byte: end,
            };
            if start == 0
                && paragraph
                    .list
                    .as_ref()
                    .is_some_and(|item| item.marker.is_some())
                && let Some(marker) = &marker
            {
                line.marker_glyphs = marker.glyphs.clone();
                line.ascent = line.ascent.max(marker.ascent);
                line.descent = line.descent.max(marker.descent);
                line.height = line.height.max(marker.height);
            }
            let natural_height = line.ascent + line.descent;
            line.height = (line.height * config.line_height).max(natural_height);
            if line.height > bottom - config.margin as f32 {
                return Err(Error::Layout("line metrics exceed reading area"));
            }
            if y + line.height > bottom && !page.is_empty() {
                if !emit(page)? {
                    if missing != 0 {
                        warnings.push(format!("{missing} glyphs unavailable in selected fonts"));
                    }
                    return Ok(PageStep {
                        warnings,
                        next: Some(line.start),
                    });
                }
                glyph_count = 0;
                page = Page {
                    start: line.start,
                    end: line.end,
                    lines: vec![],
                    images: vec![],
                };
                y = config.margin as f32;
            }
            if page.is_empty() {
                page.start = line.start;
            }
            let rtl = shaping
                .bidi
                .paragraphs
                .iter()
                .find(|p| p.range.contains(&start))
                .is_some_and(|p| p.level.is_rtl());
            let align = paragraph
                .spans
                .first()
                .map_or(crate::document::TextAlign::Start, |span| span.style.align);
            use crate::document::TextAlign;
            if align == TextAlign::Justify {
                justify(&mut line, paragraph, line_available);
            }
            let space = (line_available - line.width).max(0.0);
            let x = config.margin as f32
                + if rtl { 0.0 } else { gutter }
                + if rtl { 0.0 } else { indent }
                + match align {
                    TextAlign::Center => space / 2.0,
                    TextAlign::Right => space,
                    TextAlign::Left => 0.0,
                    TextAlign::Start | TextAlign::Justify if rtl => space,
                    TextAlign::End if !rtl => space,
                    _ => 0.0,
                };
            let baseline = y + line.ascent + (line.height - natural_height) / 2.0;
            line.baseline = baseline;
            for glyph in &mut line.glyphs {
                glyph.x += x;
                glyph.y += baseline;
            }
            let marker_x = if rtl {
                (config.width - config.margin) as f32 - gutter + config.font_size * 0.3
                    - indent.min(0.0)
            } else {
                config.margin as f32 + gutter
                    - marker.as_ref().map_or(0.0, |m| m.width)
                    - config.font_size * 0.3
                    + indent.min(0.0)
            };
            for glyph in &mut line.marker_glyphs {
                glyph.x += marker_x;
                glyph.y += baseline;
            }
            glyph_count += line.glyphs.len() + line.marker_glyphs.len();
            if glyph_count > 131_072 {
                return Err(Error::Limit("page glyph cache"));
            }
            y += line.height;
            page.end = line.end;
            page.lines.push(line);
            start = end;
        }
        y += config.font_size * 0.4;
    }
    if !page.is_empty() || emitted.get() == 0 {
        emit(page)?;
    }
    if missing != 0 {
        warnings.push(format!("{missing} glyphs unavailable in selected fonts"));
    }
    Ok(PageStep {
        warnings,
        next: None,
    })
}

fn justify(line: &mut Line, paragraph: &Paragraph, width: f32) {
    let source = &paragraph.text[line.start.byte..line.end.byte];
    if source.contains('\t')
        || paragraph.text[line.end.byte..].trim().is_empty()
        || source.chars().any(|c| {
            matches!(
                c,
                '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}' | '\u{b}' | '\u{c}'
            )
        })
        || line.width >= width
    {
        return;
    }
    let first = line.start.byte + source.len() - source.trim_start().len();
    let last = line.start.byte + source.trim_end().len();
    // Glyphs are in visual order. Expand after a complete space cluster so
    // combining glyphs and reordered bidi runs retain their relative positions.
    let gaps: Vec<_> = line
        .glyphs
        .iter()
        .enumerate()
        .filter_map(|(index, glyph)| {
            (glyph.cluster > first
                && glyph.cluster < last
                && paragraph.text.as_bytes().get(glyph.cluster) == Some(&b' ')
                && line
                    .glyphs
                    .get(index + 1)
                    .is_none_or(|next| next.cluster != glyph.cluster))
            .then_some(index)
        })
        .collect();
    if gaps.is_empty() {
        return;
    }
    let extra = (width - line.width) / gaps.len() as f32;
    let mut passed = 0;
    for (index, glyph) in line.glyphs.iter_mut().enumerate() {
        glyph.x += extra * passed as f32;
        if gaps.get(passed) == Some(&index) {
            glyph.advance += extra;
            passed += 1;
        }
    }
    line.width = width;
}

fn hyphen_points(paragraph: &Paragraph, fallback: &str, mode: Hyphenation) -> Vec<usize> {
    let mut points = Vec::new();
    if mode == Hyphenation::Off {
        return points;
    }
    for (offset, word) in paragraph.text.unicode_word_indices() {
        if word.len() > 128 {
            continue;
        }
        let first_span = paragraph
            .spans
            .partition_point(|span| span.range.end <= offset);
        if paragraph.spans[first_span..]
            .iter()
            .take_while(|span| span.range.start < offset + word.len())
            .any(|span| span.style.hyphens != Hyphens::Auto)
        {
            continue;
        }
        let mut languages = paragraph.spans[first_span..]
            .iter()
            .take_while(|span| span.range.start < offset + word.len())
            .map(|span| {
                mode.language()
                    .unwrap_or_else(|| span.language.as_deref().unwrap_or(fallback))
            });
        let language = languages
            .next()
            .unwrap_or_else(|| mode.language().unwrap_or(fallback));
        if languages.any(|other| !language.eq_ignore_ascii_case(other)) {
            continue;
        }
        let primary = language.split('-').next().unwrap_or("");
        let [first, second] = primary.as_bytes() else {
            continue;
        };
        let Some(lang) =
            hypher::Lang::from_iso([first.to_ascii_lowercase(), second.to_ascii_lowercase()])
        else {
            continue;
        };
        // Dictionary offsets refer to NFC text; locators retain source bytes.
        let mut boundaries = Vec::new();
        let lookup = if word.is_ascii() {
            Cow::Borrowed(word)
        } else {
            let mut normalized = String::with_capacity(word.len());
            for (byte, grapheme) in word.grapheme_indices(true) {
                normalized.extend(grapheme.nfc());
                boundaries.push((normalized.len(), byte + grapheme.len()));
            }
            Cow::Owned(normalized)
        };
        if !lookup.chars().all(char::is_alphabetic) {
            continue;
        }
        let mut consumed = 0;
        for syllable in hypher::hyphenate(&lookup, lang) {
            consumed += syllable.len();
            if consumed < lookup.len() {
                let original = if word.is_ascii() {
                    consumed
                } else if let Ok(index) =
                    boundaries.binary_search_by_key(&consumed, |(byte, _)| *byte)
                {
                    boundaries[index].1
                } else {
                    continue;
                };
                points.push(offset + original);
                if points.len() == 32_768 {
                    return points;
                }
            }
        }
    }
    points
}

fn image_dimension(
    value: Option<epub_stream::css::ImageLimit>,
    available: u32,
) -> Result<Option<f64>> {
    use epub_stream::css::ImageLimit;
    let (number, percent) = match value {
        None | Some(ImageLimit::None) => return Ok(None),
        Some(ImageLimit::Px(number)) => (number, false),
        Some(ImageLimit::Percent(number)) => (number, true),
    };
    if !number.is_finite() || number < 0.0 {
        return Err(Error::Layout("invalid image size limit"));
    }
    Ok(Some(if percent {
        f64::from(number) * f64::from(available) / 100.0
    } else {
        f64::from(number)
    }))
}

fn image_size(
    image: &crate::document::ImageBlock,
    intrinsic: crate::image::Size,
    available_width: u32,
    available_height: u32,
) -> Result<Option<crate::image::Size>> {
    let min_width = image_dimension(image.min_width, available_width)?.unwrap_or(0.0);
    let min_height = image_dimension(image.min_height, available_height)?.unwrap_or(0.0);
    let max_width = image_dimension(image.max_width, available_width)?
        .unwrap_or(f64::MAX)
        .max(min_width);
    let max_height = image_dimension(image.max_height, available_height)?
        .unwrap_or(f64::MAX)
        .max(min_height);
    if max_width == 0.0 || max_height == 0.0 {
        return Ok(None);
    }
    if image.width.is_none() && image.height.is_none() && min_width == 0.0 && min_height == 0.0 {
        if image_limit(image.max_width, available_width)? == 0
            || image_limit(image.max_height, available_height)? == 0
        {
            return Ok(None);
        }
        return intrinsic
            .fit(
                image
                    .width_hint
                    .map(u32::from)
                    .filter(|v| *v != 0)
                    .unwrap_or(intrinsic.width)
                    .min(image_limit(image.max_width, available_width)?),
                image
                    .height_hint
                    .map(u32::from)
                    .filter(|v| *v != 0)
                    .unwrap_or(intrinsic.height)
                    .min(image_limit(image.max_height, available_height)?),
            )
            .map(Some);
    }
    intrinsic.fit(available_width, available_height)?;
    let width = image_dimension(image.width, available_width)?.or_else(|| {
        image
            .width
            .is_none()
            .then_some(image.width_hint)
            .flatten()
            .filter(|v| *v != 0)
            .map(f64::from)
    });
    let height = image_dimension(image.height, available_height)?.or_else(|| {
        image
            .height
            .is_none()
            .then_some(image.height_hint)
            .flatten()
            .filter(|v| *v != 0)
            .map(f64::from)
    });
    let ratio = f64::from(intrinsic.width) / f64::from(intrinsic.height);
    let (mut width, mut height) = match (width, height) {
        (Some(width), Some(height)) => (
            width.clamp(min_width, max_width),
            height.clamp(min_height, max_height),
        ),
        (width, height) => {
            let width = width.map(|w| w.max(min_width));
            let height = height.map(|h| h.max(min_height));
            let mut width =
                width.unwrap_or_else(|| height.map_or(f64::from(intrinsic.width), |h| h * ratio));
            let mut height = height.unwrap_or(width / ratio);
            if width == 0.0 || height == 0.0 {
                width = min_width.max(min_height * ratio);
                height = width / ratio;
            }
            if width == 0.0 || height == 0.0 {
                return Ok(None);
            }
            // Preserve the ratio when feasible; conflicting author constraints can change it.
            let lower = (min_width / width).max(min_height / height);
            let upper = (max_width / width).min(max_height / height);
            let scale = 1.0_f64.max(lower).min(upper);
            (
                (width * scale).clamp(min_width, max_width),
                (height * scale).clamp(min_height, max_height),
            )
        }
    };
    if width == 0.0 || height == 0.0 {
        return Ok(None);
    }
    let scale = (f64::from(available_width) / width)
        .min(f64::from(available_height) / height)
        .min((crate::image::MAX_IMAGE_PIXELS as f64 / (width * height)).sqrt())
        .min(1.0);
    width *= scale;
    height *= scale;
    Ok(Some(crate::image::Size {
        width: width.floor().max(1.0) as u32,
        height: height.floor().max(1.0) as u32,
    }))
}

fn image_limit(limit: Option<epub_stream::css::ImageLimit>, available: u32) -> Result<u32> {
    use epub_stream::css::ImageLimit;
    let (number, percent) = match limit {
        None | Some(ImageLimit::None) => return Ok(available),
        Some(ImageLimit::Px(number)) => (number, false),
        Some(ImageLimit::Percent(number)) => (number, true),
    };
    if !number.is_finite() || number < 0.0 {
        return Err(Error::Layout("invalid image size limit"));
    }
    let resolved = if percent {
        f64::from(number) * f64::from(available) / 100.0
    } else {
        f64::from(number)
    };
    Ok(resolved.min(f64::from(available)).floor() as u32)
}

struct Shaping {
    bidi: BidiAnalysis,
    scripts: crate::scripts::Window,
}

struct BidiAnalysis {
    original_classes: Vec<unicode_bidi::BidiClass>,
    levels: Vec<unicode_bidi::Level>,
    paragraphs: Vec<unicode_bidi::ParagraphInfo>,
}

impl BidiAnalysis {
    fn visual_runs(
        &self,
        text: &str,
        range: Range<usize>,
    ) -> Result<(Vec<unicode_bidi::Level>, Vec<Range<usize>>)> {
        let para = self
            .paragraphs
            .iter()
            .find(|para| para.range.contains(&range.start))
            .ok_or(Error::Layout("missing bidi paragraph"))?;
        // Preserve resolved paragraph context while applying UAX L1/L2 to
        // line-sized scratch buffers. The crate otherwise clones every level
        // in the paragraph on each candidate line fit.
        let line_bidi = BidiInfo {
            text: &text[range.clone()],
            original_classes: self.original_classes[range.clone()].to_vec(),
            levels: self.levels[range.clone()].to_vec(),
            paragraphs: vec![],
        };
        let line_para = unicode_bidi::ParagraphInfo {
            range: 0..range.len(),
            level: para.level,
        };
        let (levels, local_runs) = line_bidi.visual_runs(&line_para, 0..range.len());
        let runs = local_runs
            .into_iter()
            .map(|run| run.start + range.start..run.end + range.start)
            .collect();
        Ok((levels, runs))
    }
}

impl Shaping {
    fn new(text: &str, base: Option<unicode_bidi::Level>) -> Self {
        let mut scripts = crate::scripts::Window::new(text);
        scripts.prepare(text, 0);
        let bidi = BidiInfo::new(text, base);
        Self {
            bidi: BidiAnalysis {
                original_classes: bidi.original_classes,
                levels: bidi.levels,
                paragraphs: bidi.paragraphs,
            },
            scripts,
        }
    }
}

/// Retains only the paragraph at the cooperative pagination cursor. Its owner
/// must keep the chapter, layout configuration and preview range unchanged.
#[derive(Default)]
pub(crate) struct PaginationContext {
    paragraph: Option<(usize, Shaping, Vec<usize>)>,
    #[cfg(test)]
    pub(crate) analyses: usize,
}

fn shape_hyphen(
    p: &Paragraph,
    shaping: &Shaping,
    range: Range<usize>,
    fonts: &Fonts,
    size: f32,
) -> Result<Line> {
    let cluster = p.text[..range.end]
        .char_indices()
        .next_back()
        .map(|(i, _)| i)
        .unwrap_or(range.start);
    let style = p
        .spans
        .iter()
        .find(|span| span.range.contains(&cluster))
        .map(|span| span.style)
        .unwrap_or_default();
    let mut line = shape(p, shaping, range, fonts, size)?;
    let mark = Paragraph {
        list: None,
        text: "-".into(),
        spans: vec![Span {
            language: None,
            range: 0..1,
            style,
        }],
    };
    let mut suffix = shape(&mark, &Shaping::new("-", None), 0..1, fonts, size)?;
    for glyph in &mut suffix.glyphs {
        glyph.x += line.width;
        glyph.cluster = cluster;
    }
    line.width += suffix.width;
    line.height = line.height.max(suffix.height);
    line.ascent = line.ascent.max(suffix.ascent);
    line.descent = line.descent.max(suffix.descent);
    line.glyphs.extend(suffix.glyphs);
    line.hyphenated = true;
    Ok(line)
}

fn soft_hyphen_enabled(p: &Paragraph, end: usize) -> bool {
    let mark = end.saturating_sub('\u{ad}'.len_utf8());
    p.spans
        .iter()
        .find(|span| span.range.contains(&mark))
        .is_none_or(|span| span.style.hyphens != Hyphens::None)
}

/// Where a line that starts with `text` may end within the line buffer.
fn break_opportunities(text: &str) -> impl Iterator<Item = (usize, BreakOpportunity)> + '_ {
    use crate::scripts::LINE_BYTES;
    // Whether a line may end before a character is decided by the text up to
    // and including that character, so one character past the buffer settles
    // every end within it. Looking further would read the rest of a paragraph
    // that has no opportunity, once for each of its lines.
    let seen = (LINE_BYTES + 1..text.len())
        .find(|&byte| text.is_char_boundary(byte))
        .unwrap_or(text.len());
    let text = &text[..seen];
    let mut ordinary = linebreaks(text).peekable();
    let mut soft = text
        .char_indices()
        .filter_map(|(byte, c)| (c == '\u{ad}').then_some(byte + c.len_utf8()))
        .peekable();
    std::iter::from_fn(
        move || match (ordinary.peek().copied(), soft.peek().copied()) {
            (Some((end, kind)), Some(hyphen)) if hyphen == end => {
                ordinary.next();
                soft.next();
                Some((end, kind))
            }
            (Some((end, _)), Some(hyphen)) if hyphen < end => {
                soft.next();
                Some((hyphen, BreakOpportunity::Allowed))
            }
            (None, Some(hyphen)) => {
                soft.next();
                Some((hyphen, BreakOpportunity::Allowed))
            }
            _ => ordinary.next(),
        },
    )
    // Where the text seen was cut short, its end lies past the buffer as well.
    .take_while(|(end, _)| *end <= LINE_BYTES)
}

/// Byte offset at which unshaped advances of the regular face exceed `width`.
/// Only a starting point for the line fit; shaping decides.
fn estimated_fit(p: &Paragraph, start: usize, width: f32, fonts: &Fonts, size: f32) -> usize {
    let face = fonts.regular.face();
    let scale = size / face.units_per_em() as f32;
    let mut total = 0.0;
    for (offset, c) in p.text[start..].char_indices() {
        total += face
            .glyph_index(c)
            .and_then(|glyph| face.glyph_hor_advance(glyph))
            .map_or(size * 0.5, |advance| f32::from(advance) * scale);
        if total > width {
            return start + offset;
        }
    }
    p.text.len()
}

/// Selects the last break opportunity that fits, as a scan over every
/// opportunity in order would. The scan starts near an estimate instead of at
/// the first opportunity, which assumes that a prefix of a fitting line without
/// a discretionary hyphen also fits. Only shaping says what fits: joined
/// letters and kerned pairs are narrower than their unshaped advances by any
/// amount, so a line is shaped once more to see that the next word overflows.
fn break_line(
    p: &Paragraph,
    shaping: &Shaping,
    start: usize,
    width: f32,
    fonts: &Fonts,
    size: f32,
    hyphens: &[usize],
) -> Result<(usize, Line)> {
    let mut candidates = Vec::new();
    for (relative, kind) in break_opportunities(&p.text[start..]) {
        let end = start + relative;
        if kind == BreakOpportunity::Allowed && p.text[start..end].chars().all(|c| c == '\u{ad}') {
            continue;
        }
        candidates.push(end);
        if kind == BreakOpportunity::Mandatory {
            break;
        }
    }
    let mut best = None;
    if !candidates.is_empty() {
        let estimate = estimated_fit(p, start, width, fonts, size);
        let first = candidates
            .partition_point(|&end| end <= estimate)
            .clamp(1, candidates.len())
            - 1;
        // A discretionary hyphen can be wider than the following complete
        // word with its invisible soft hyphen, so widths are not monotonic:
        // an overflowing hyphenated candidate ends the line only if it
        // overflows without its hyphen as well, any other always does.
        let mut overflow = None;
        let mut index = first;
        loop {
            let line = shape(p, shaping, start..candidates[index], fonts, size)?;
            if line.width <= width {
                best = Some((candidates[index], line));
                break;
            }
            if !line.hyphenated {
                overflow = Some(candidates[index]);
            }
            if index == 0 {
                break;
            }
            index -= 1;
        }
        if overflow.is_none() {
            for &end in &candidates[first + 1..] {
                let line = shape(p, shaping, start..end, fonts, size)?;
                if line.width <= width {
                    best = Some((end, line));
                    continue;
                }
                // Only the hyphen may be what overflows. Without it the text
                // is a prefix of every longer line, and ends the scan too.
                let text = p.text[start..end].trim_end_matches('\u{ad}');
                if !line.hyphenated
                    || shape(p, shaping, start..start + text.len(), fonts, size)?.width > width
                {
                    overflow = Some(end);
                    break;
                }
            }
        }
        if let Some(end) = overflow {
            let after = best.as_ref().map_or(start, |(end, _)| *end);
            let splits = &hyphens[hyphens.partition_point(|&n| n <= after)..];
            let splits = &splits[..splits.partition_point(|&n| n < end)];
            // The longest hyphenated prefix that fits wins, so try long ones first.
            for &split in splits.iter().rev() {
                // The initial implementation adds hyphens only to LTR lines.
                if shaping.bidi.levels[start..split]
                    .iter()
                    .any(|level| level.is_rtl())
                {
                    continue;
                }
                let candidate = shape_hyphen(p, shaping, start..split, fonts, size)?;
                if candidate.width <= width {
                    best = Some((split, candidate));
                    break;
                }
            }
        }
    }
    if let Some(best) = best {
        return Ok(best);
    }
    // Emergency breaks preserve grapheme clusters when an unbreakable token exceeds the viewport.
    for (offset, grapheme) in p.text[start..].grapheme_indices(true) {
        let end = start + offset + grapheme.len();
        if end - start > crate::scripts::LINE_BYTES {
            break;
        }
        let line = shape(p, shaping, start..end, fonts, size)?;
        if line.width > width && best.is_some() {
            break;
        }
        best = Some((end, line));
        if grapheme.contains('\n') {
            break;
        }
    }
    best.ok_or(Error::Limit("single grapheme exceeds line buffer"))
}

fn shape(
    p: &Paragraph,
    shaping: &Shaping,
    range: Range<usize>,
    fonts: &Fonts,
    size: f32,
) -> Result<Line> {
    if range.len() > crate::scripts::LINE_BYTES {
        return Err(Error::Limit("line buffer"));
    }
    let trimmed_end = range.start
        + p.text[range.clone()]
            .trim_end_matches([' ', '\t', '\r', '\n'])
            .len();
    let show_soft_hyphen = range.end == trimmed_end
        && range.end < p.text.len()
        && p.text[range.clone()].ends_with('\u{ad}')
        && soft_hyphen_enabled(p, range.end)
        && p.text[range.start..range.end - 2]
            .chars()
            .any(|c| c != '\u{ad}');
    let range = range.start..trimmed_end;
    let base_face = fonts.regular.face();
    let base_factor = size / base_face.units_per_em() as f32;
    let mut line = Line {
        start: Locator::default(),
        end: Locator::default(),
        text: p.text[range.clone()].into(),
        hyphenated: show_soft_hyphen,
        glyphs: vec![],
        marker_glyphs: vec![],
        width: 0.0,
        height: size,
        ascent: (base_face.ascender() as f32 * base_factor).max(0.0),
        descent: (-f32::from(base_face.descender()) * base_factor).max(0.0),
        baseline: 0.0,
    };
    if range.is_empty() {
        return Ok(line);
    }
    let para = shaping
        .bidi
        .paragraphs
        .iter()
        .find(|para| para.range.contains(&range.start))
        .ok_or(Error::Layout("missing bidi paragraph"))?;
    let (mut levels, mut runs) = shaping.bidi.visual_runs(&p.text, range.clone())?;
    if show_soft_hyphen {
        let mark = range.end - '\u{ad}'.len_utf8();
        if let Some((previous, _)) = p.text[range.start..mark].char_indices().next_back() {
            // UAX L1 resets the invisible trailing BN character to paragraph
            // level. A displayed hyphen belongs to the preceding word instead.
            let level = shaping.bidi.levels[range.start + previous];
            levels[mark - range.start..range.len()].fill(level);
            let mut logical: Vec<Range<usize>> = Vec::new();
            for (offset, c) in p.text[range.clone()].char_indices() {
                let byte = range.start + offset;
                if let Some(last) = logical.last_mut()
                    && levels[last.start - range.start] == levels[byte - range.start]
                {
                    last.end = byte + c.len_utf8();
                } else {
                    logical.push(byte..byte + c.len_utf8());
                }
            }
            let run_levels: Vec<_> = logical
                .iter()
                .map(|run| levels[run.start - range.start])
                .collect();
            runs = BidiInfo::reorder_visual(&run_levels)
                .into_iter()
                .map(|index| logical[index].clone())
                .collect();
        }
    }
    let mut tabs = Vec::new();
    for run in runs {
        let rtl = levels[run.start - range.start].is_rtl();
        let mut spans: Vec<_> = p
            .spans
            .iter()
            .filter_map(|span| {
                let start = run.start.max(span.range.start);
                let end = run.end.min(span.range.end);
                (start < end).then_some((start..end, span.style, span.language.as_deref()))
            })
            .collect();
        if rtl {
            spans.reverse();
        }
        for (span, style, language) in spans {
            let mut segments = Vec::new();
            for (part, script) in crate::scripts::within(shaping.scripts.ranges(), span) {
                segments.extend(
                    fonts
                        .segments(&p.text[part.clone()], part.start, style)
                        .into_iter()
                        .map(|(range, font)| (range, font, script)),
                );
            }
            if rtl {
                segments.reverse();
            }
            for (span, font_id, script) in segments {
                let font = fonts.get(font_id);
                let face = font.face();
                let px = size * style.scale;
                let factor = px / face.units_per_em() as f32;
                line.ascent = line.ascent.max(face.ascender() as f32 * factor);
                line.descent = line.descent.max(-f32::from(face.descender()) * factor);
                let mut buffer = UnicodeBuffer::new();
                for (offset, character) in p.text[span.clone()].char_indices() {
                    let character = if character == '\u{ad}' {
                        if show_soft_hyphen
                            && span.start + offset + character.len_utf8() == range.end
                        {
                            '-'
                        } else {
                            continue;
                        }
                    } else {
                        character
                    };
                    buffer.add(character, offset as u32);
                }
                buffer.set_direction(if rtl {
                    Direction::RightToLeft
                } else {
                    Direction::LeftToRight
                });
                if let Some(language) = language.and_then(|tag| tag.parse().ok()) {
                    buffer.set_language(language);
                }
                if let Some(script) = rustybuzz::Script::from_iso15924_tag(
                    rustybuzz::ttf_parser::Tag(script.as_iso15924_tag()),
                ) {
                    buffer.set_script(script);
                }
                buffer.guess_segment_properties();
                let shaped = font.shape(face, buffer);
                for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
                    let cluster = span.start + info.cluster as usize;
                    if p.text.as_bytes().get(cluster) == Some(&b'\t') {
                        tabs.push((line.glyphs.len(), line.width, 0.0, style.tab_size));
                        continue;
                    }
                    if let Some((y_min, y_max)) = font.vertical_extents(face, info.glyph_id as u16)
                    {
                        let offset = pos.y_offset as f32;
                        line.ascent = line.ascent.max((f32::from(y_max) + offset) * factor);
                        line.descent = line.descent.max(-(f32::from(y_min) + offset) * factor);
                    }
                    line.glyphs.push(Glyph {
                        id: info.glyph_id as u16,
                        font: font_id,
                        size: px,
                        x: line.width + pos.x_offset as f32 * factor,
                        y: -(pos.y_offset as f32 * factor),
                        advance: pos.x_advance as f32 * factor,
                        cluster: span.start + info.cluster as usize,
                    });
                    line.width += pos.x_advance as f32 * factor;
                }
                line.height = line.height.max(px);
            }
        }
    }
    if !tabs.is_empty() {
        let style = p.spans.first().map_or(TextStyle::default(), |s| s.style);
        let (_, font) = fonts.select(style);
        let face = font.face();
        let px = size * style.scale;
        let advance = |ch| {
            face.glyph_index(ch)
                .and_then(|id| face.glyph_hor_advance(id))
                .map(|width| width as f32 * px / face.units_per_em() as f32)
                .filter(|width| *width > 0.0)
                .unwrap_or(px * 0.5)
        };
        let space = advance(' ');
        let minimum = 0.5 * advance('0');
        let mut added = 0.0;
        let rtl = para.level.is_rtl();
        // Resolve stops from the paragraph's start edge, then apply physical
        // shifts in visual order. Tabs retain source bytes but have no glyph.
        for offset in 0..tabs.len() {
            let i = if rtl { tabs.len() - 1 - offset } else { offset };
            let (_, position, gap, tab_size) = &mut tabs[i];
            let interval = match *tab_size {
                crate::document::TabSize::Spaces(n) => n * space,
                crate::document::TabSize::Px(n) => n,
                crate::document::TabSize::Em(n) => n * size,
            };
            if !interval.is_finite() || interval < 0.0 {
                return Err(Error::Layout("invalid tab interval"));
            }
            if interval == 0.0 {
                continue;
            }
            let distance = if rtl {
                line.width - *position
            } else {
                *position
            } + added;
            let mut next = (distance / interval).floor().mul_add(interval, interval);
            if next - distance < minimum {
                next += interval;
            }
            if !next.is_finite() {
                return Err(Error::Layout("tab position overflow"));
            }
            *gap = next - distance;
            added += *gap;
        }
        if !(line.width + added).is_finite() {
            return Err(Error::Layout("tab position overflow"));
        }
        let mut gaps = tabs.iter().peekable();
        let mut shift = 0.0;
        for (index, glyph) in line.glyphs.iter_mut().enumerate() {
            while let Some((_, _, gap, _)) = gaps.next_if(|(at, _, _, _)| *at <= index) {
                shift += gap;
            }
            glyph.x += shift;
        }
        line.width += added;
    }
    Ok(line)
}

pub fn rasterize(page: &Page, fonts: &Fonts, config: LayoutConfig) -> Result<Vec<u8>> {
    rasterize_with_assets(page, fonts, config, |_| {
        Err(Error::Image("image resource resolver required".into()))
    })
}

pub fn rasterize_with_assets(
    page: &Page,
    fonts: &Fonts,
    config: LayoutConfig,
    resource: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    rasterize_display(page, fonts, config, Default::default(), 256, resource)
}

/// A rendered page and bounded diagnostics for unavailable image assets.
pub struct RenderedPage {
    pub pixels: Vec<u8>,
    pub warnings: Vec<String>,
}

pub fn rasterize_display(
    page: &Page,
    fonts: &Fonts,
    config: LayoutConfig,
    text: crate::display::TextRaster,
    levels: u16,
    resource: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<Vec<u8>> {
    Ok(rasterize_display_report(page, fonts, config, text, levels, resource)?.pixels)
}

pub fn rasterize_display_report(
    page: &Page,
    fonts: &Fonts,
    config: LayoutConfig,
    text: crate::display::TextRaster,
    levels: u16,
    resource: impl FnMut(&str) -> Result<Vec<u8>>,
) -> Result<RenderedPage> {
    let mut masks = fonts.masks.lock().unwrap_or_else(|e| e.into_inner());
    masks.retag(fonts.mask_tag(text));
    rasterize_cached(page, fonts, config, text, levels, resource, &mut masks)
}

fn rasterize_cached(
    page: &Page,
    fonts: &Fonts,
    config: LayoutConfig,
    text: crate::display::TextRaster,
    levels: u16,
    mut resource: impl FnMut(&str) -> Result<Vec<u8>>,
    cache: &mut crate::glyph_cache::GlyphCache,
) -> Result<RenderedPage> {
    config.validate()?;
    let coverage = text.coverage_table()?;
    if !matches!(levels, 2 | 4 | 256) {
        return Err(Error::Layout("display levels must be 2, 4, or 256"));
    }
    let mut pixels = vec![255; config.width as usize * config.height as usize];
    let mut warnings = Vec::new();
    for placement in &page.images {
        if placement.size.width == 0
            || placement.size.height == 0
            || placement
                .x
                .checked_add(placement.size.width)
                .is_none_or(|x| x > config.width)
            || placement
                .y
                .checked_add(placement.size.height)
                .is_none_or(|y| y > config.height)
        {
            return Err(Error::Layout("image outside viewport"));
        }
        let decoded = resource(&placement.source).and_then(|bytes| image::decode(&bytes));
        let image = match decoded.and_then(|image| image.resize(placement.size)) {
            Ok(image) => image,
            Err(error) => {
                if warnings.len() < 64 {
                    warnings.push(format!(
                        "Image unavailable: {}: {}",
                        placement.source.chars().take(160).collect::<String>(),
                        error.to_string().chars().take(256).collect::<String>()
                    ));
                }
                image_placeholder(&mut pixels, config.width, placement);
                continue;
            }
        };
        for row in 0..image.size.height {
            let start = ((placement.y + row) * config.width + placement.x) as usize;
            let source = (row * image.size.width) as usize;
            pixels[start..start + image.size.width as usize]
                .copy_from_slice(&image.pixels[source..source + image.size.width as usize]);
        }
    }
    // A blank frame is already quantized; dithering it costs a full-frame pass.
    if !page.images.is_empty() {
        image::quantize(&mut pixels, config.width, levels)?;
    }
    let mut context = ScaleContext::new();
    for line in &page.lines {
        for glyph in line.glyphs.iter().chain(&line.marker_glyphs) {
            let origin = if text.fractional {
                glyph.x.floor()
            } else {
                glyph.x.round()
            };
            let offset = if text.fractional {
                glyph.x - origin
            } else {
                0.0
            };
            let key = [
                u32::from(glyph.font),
                u32::from(glyph.id),
                glyph.size.to_bits(),
                offset.to_bits(),
            ];
            let Some(image) = cache.get_or_render(key, || {
                let mut scaler = context
                    .builder(fonts.get(glyph.font).swash())
                    .size(glyph.size)
                    .hint(text.hinting)
                    .build();
                Render::new(&[Source::Outline])
                    .format(Format::Alpha)
                    .offset(Vector::new(offset, 0.0))
                    .embolden(text.stroke)
                    .render(&mut scaler, glyph.id)
            }) else {
                continue;
            };
            let left = origin as i32 + image.placement.left;
            let top = glyph.y.round() as i32 - image.placement.top;
            for row in 0..image.placement.height {
                for col in 0..image.placement.width {
                    let x = left + col as i32;
                    let y = top + row as i32;
                    if x < 0 || y < 0 || x >= config.width as i32 || y >= config.height as i32 {
                        continue;
                    }
                    let alpha = coverage
                        [image.data[(row * image.placement.width + col) as usize] as usize]
                        as u16;
                    let dst = &mut pixels[y as usize * config.width as usize + x as usize];
                    *dst = (*dst as u16 * (255 - alpha) / 255) as u8;
                }
            }
        }
    }
    let level: [u8; 256] = std::array::from_fn(|v| crate::display::text_level(v as u8, levels));
    for pixel in &mut pixels {
        *pixel = level[*pixel as usize];
    }
    Ok(RenderedPage { pixels, warnings })
}

// Preserve the image's allocated space and source anchors after a decode failure.
fn image_placeholder(pixels: &mut [u8], stride: u32, placement: &PlacedImage) {
    let width = placement.size.width;
    let height = placement.size.height;
    for y in 0..height {
        let diagonal = u64::from(y) * u64::from(width - 1) / u64::from((height - 1).max(1));
        for x in 0..width {
            if y == 0
                || y == height - 1
                || x == 0
                || x == width - 1
                || u64::from(x) == diagonal
                || u64::from(width - 1 - x) == diagonal
            {
                pixels[((placement.y + y) * stride + placement.x + x) as usize] = 0;
            }
        }
    }
}

pub fn write_png(output: impl Write, pixels: &[u8], config: LayoutConfig) -> Result<()> {
    config.validate()?;
    if pixels.len() != config.width as usize * config.height as usize {
        return Err(Error::Layout("framebuffer size"));
    }
    let mut encoder = png::Encoder::new(output, config.width, config.height);
    encoder.set_color(png::ColorType::Grayscale);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(pixels)?;
    Ok(())
}

#[cfg(test)]
mod raster_cache_tests {
    use super::*;
    use crate::{display::TextRaster, document::Document, glyph_cache::GlyphCache};

    #[test]
    fn cached_masks_match_uncached_pixels_for_every_raster_control() {
        let root =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
        let mut document = Document::open(root.join("typography.txt")).unwrap();
        let mut fonts = Fonts::new(Font::load(root.join("DejaVuSerif.ttf")).unwrap());
        fonts
            .fallback
            .push(Font::load(root.join("DejaVuSans.ttf")).unwrap());
        let config = LayoutConfig::default();
        let pages = paginate(&document.chapter(0).unwrap(), &fonts, config).unwrap();
        for page in &pages.pages {
            for fractional in [false, true] {
                for hinting in [false, true] {
                    for (stroke, coverage_gamma) in [(0.0, 1.0), (0.2, 0.7)] {
                        for levels in [2, 4, 256] {
                            let text = TextRaster {
                                fractional,
                                hinting,
                                stroke,
                                coverage_gamma,
                            };
                            let mut cached = GlyphCache::default();
                            let mut uncached = GlyphCache::new(0, 0);
                            let actual = rasterize_cached(
                                page,
                                &fonts,
                                config,
                                text,
                                levels,
                                |_| panic!("text only"),
                                &mut cached,
                            )
                            .unwrap();
                            let expected = rasterize_cached(
                                page,
                                &fonts,
                                config,
                                text,
                                levels,
                                |_| panic!("text only"),
                                &mut uncached,
                            )
                            .unwrap();
                            assert!(
                                actual.pixels == expected.pixels,
                                "cache changed pixels: {text:?}, levels={levels}"
                            );
                            if !fractional {
                                assert!(
                                    cached.renders * 2 < uncached.renders,
                                    "expected repeated glyph reuse: {} / {}",
                                    cached.renders,
                                    uncached.renders
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod bidi_scratch_tests {
    use super::*;

    #[test]
    fn line_sized_reordering_matches_full_paragraph_oracle() {
        for text in [
            "abc אבג 123 (דה) xyz  ",
            "אבג abc \t  ",
            "a \u{2067}אבג 12\u{2069} z \u{202b}rtl\u{202c} ",
            "a\u{ad}bc אב\u{ad}גד ",
            "abc אבג\nדהו def\u{2029}ג abc ",
            "a\u{202a}b\u{202c}\u{200f}  ",
        ] {
            for base in [
                None,
                Some(unicode_bidi::Level::ltr()),
                Some(unicode_bidi::Level::rtl()),
            ] {
                let oracle = BidiInfo::new(text, base);
                let shaping = Shaping::new(text, base);
                for para in &oracle.paragraphs {
                    let boundaries: Vec<_> = text
                        .char_indices()
                        .map(|(i, _)| i)
                        .chain(std::iter::once(text.len()))
                        .filter(|i| para.range.start <= *i && *i <= para.range.end)
                        .collect();
                    for (i, &start) in boundaries.iter().enumerate() {
                        for &end in &boundaries[i + 1..] {
                            let range = start..end;
                            let (expected, expected_runs) = oracle.visual_runs(para, range.clone());
                            let (actual, actual_runs) =
                                shaping.bidi.visual_runs(text, range.clone()).unwrap();
                            assert_eq!(actual.len(), range.len());
                            assert_eq!(actual, expected[range.clone()], "{text:?} {range:?}");
                            assert_eq!(actual_runs, expected_runs, "{text:?} {range:?}");
                        }
                    }
                }
            }
        }
    }
}
