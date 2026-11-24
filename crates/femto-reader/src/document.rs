use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    ops::Range,
    path::Path,
};

use epub_stream::book::ChapterEventsOptions;
use epub_stream::render_prep::{ListNumberStyle, StyledEvent, StyledEventOrRun};
use epub_stream::{EpubBook, EpubBookOptions, ZipLimits};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    Error, Result,
    image::{self, Size},
    layout::Locator,
};

fn list_number(mut number: i64, style: ListNumberStyle) -> String {
    let mut label = match style {
        ListNumberStyle::LowerAlpha | ListNumberStyle::UpperAlpha if number > 0 => {
            let base = if style == ListNumberStyle::LowerAlpha {
                b'a'
            } else {
                b'A'
            };
            let mut digits = Vec::new();
            while number > 0 {
                number -= 1;
                digits.push(char::from(base + (number % 26) as u8));
                number /= 26;
            }
            digits.into_iter().rev().collect()
        }
        ListNumberStyle::LowerRoman | ListNumberStyle::UpperRoman
            if (1..=3999).contains(&number) =>
        {
            let mut roman = String::new();
            for (value, symbol) in [
                (1000, "M"),
                (900, "CM"),
                (500, "D"),
                (400, "CD"),
                (100, "C"),
                (90, "XC"),
                (50, "L"),
                (40, "XL"),
                (10, "X"),
                (9, "IX"),
                (5, "V"),
                (4, "IV"),
                (1, "I"),
            ] {
                while number >= value {
                    roman.push_str(symbol);
                    number -= value;
                }
            }
            if style == ListNumberStyle::LowerRoman {
                roman.make_ascii_lowercase();
            }
            roman
        }
        _ => number.to_string(),
    };
    label.push('.');
    label
}

/// Limits on one chapter. In 5,146 chapters of the 110 EPUBs of one
/// library the largest source file was 892 KiB (a bibliography), and the
/// most a parsed chapter held was 3,116 blocks, 448 KiB of text and 6,285
/// spans. Five chapters produced more than 16,384 parser events, which was
/// the limit on events before it had its own.
pub const MAX_CHAPTER_BYTES: usize = 1024 * 1024;
/// Blocks kept for a chapter.
pub const MAX_CHAPTER_ITEMS: usize = 16_384;
/// Styled spans kept for a chapter, across its paragraphs.
pub const MAX_CHAPTER_SPANS: usize = 32_768;
/// Events the parser may produce for a chapter. They are not kept; this
/// bounds the work, and what is kept has the limits above.
const MAX_CHAPTER_EVENTS: usize = 131_072;
/// The navigation document. The largest in the same library was 163 KiB.
const MAX_NAVIGATION_BYTES: usize = 512 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Metadata {
    pub id: String,
    pub title: String,
    pub author: String,
    pub language: String,
    pub chapters: Vec<String>,
    pub contents: Vec<ContentsEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContentsEntry {
    pub title: String,
    pub href: String,
    pub depth: usize,
    pub base: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Hyphens {
    None,
    Manual,
    #[default]
    Auto,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextAlign {
    #[default]
    Start,
    End,
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseDirection {
    #[default]
    Auto,
    Ltr,
    Rtl,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum TabSize {
    /// Computed interval relative to the reader base font size.
    Em(f32),
    Spaces(f32),
    Px(f32),
}

impl Default for TabSize {
    fn default() -> Self {
        Self::Spaces(8.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum TextIndent {
    Pixels(f32),
    Em(f32),
    Percent(f32),
}
impl Default for TextIndent {
    fn default() -> Self {
        Self::Pixels(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    #[serde(default)]
    pub indent: TextIndent,
    #[serde(default)]
    pub tab_size: TabSize,
    #[serde(default)]
    pub direction: BaseDirection,
    #[serde(default)]
    pub align: TextAlign,
    #[serde(default)]
    pub hyphens: Hyphens,
    pub scale: f32,
    pub bold: bool,
    pub italic: bool,
}

impl Default for TextStyle {
    fn default() -> Self {
        Self {
            indent: TextIndent::default(),
            tab_size: TabSize::default(),
            align: TextAlign::Start,
            direction: BaseDirection::Auto,
            hyphens: Hyphens::Auto,
            scale: 1.0,
            bold: false,
            italic: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Span {
    pub language: Option<String>,
    pub range: Range<usize>,
    pub style: TextStyle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListItem {
    pub depth: usize,
    pub marker: Option<String>,
    pub marker_label: String,
}

#[derive(Clone, Debug, Default)]
pub struct Paragraph {
    pub list: Option<ListItem>,
    pub text: String,
    pub spans: Vec<Span>,
}

impl Paragraph {
    fn push(&mut self, text: &str, style: TextStyle, language: Option<String>) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        if let Some(previous) = self.spans.last_mut()
            && previous.range.end == start
            && previous.style == style
            && previous.language == language
        {
            previous.range.end = self.text.len();
            return;
        }
        self.spans.push(Span {
            language,
            range: start..self.text.len(),
            style,
        });
    }
}

#[derive(Clone, Debug)]
pub enum Block {
    Text(Paragraph),
    Image(ImageBlock),
}

#[derive(Clone, Debug)]
pub struct ImageBlock {
    pub width: Option<epub_stream::css::ImageLimit>,
    pub height: Option<epub_stream::css::ImageLimit>,
    pub source: String,
    pub alt: String,
    pub intrinsic: Option<Size>,
    pub max_width: Option<epub_stream::css::ImageLimit>,
    pub max_height: Option<epub_stream::css::ImageLimit>,
    /// Non-inherited minimum image width; None resolves to zero.
    pub min_width: Option<epub_stream::css::ImageLimit>,
    /// Non-inherited minimum image height; None resolves to zero.
    pub min_height: Option<epub_stream::css::ImageLimit>,
    pub width_hint: Option<u16>,
    pub height_hint: Option<u16>,
}

/// Source provenance retained only when a position adapter requests it.
#[derive(Clone, Debug)]
pub struct SourceRun {
    pub start: Locator,
    pub text: String,
    pub source: epub_stream::render_prep::TextSource,
    pub white_space: epub_stream::css::WhiteSpace,
}

pub(crate) struct SourceImage {
    pub offset: usize,
    pub location: Locator,
}

type ChapterMapping = (
    Chapter,
    Vec<SourceRun>,
    Vec<epub_stream::render_prep::SourceElement>,
    Vec<SourceImage>,
);

#[derive(Clone, Debug)]
pub struct Chapter {
    pub language: String,
    pub index: usize,
    pub href: String,
    pub blocks: Vec<Block>,
    pub warnings: Vec<String>,
    pub anchors: BTreeMap<String, Locator>,
    pub notes: Vec<Range<Locator>>,
    pub links: Vec<DocumentLink>,
}

#[derive(Clone, Debug)]
pub struct DocumentLink {
    pub noteref: bool,
    pub start: Locator,
    pub end: Locator,
    pub href: String,
    pub label: String,
}

enum Source {
    Epub(Box<EpubBook<File>>),
    Text(File),
    MemoryText(String),
}

pub struct Document {
    pub metadata: Metadata,
    path: Option<std::path::PathBuf>,
    source: Source,
    supplemental: Vec<String>,
    progress_weights: Option<Vec<u64>>,
}

impl Document {
    /// Opens bounded UTF-8 text without requiring a mounted filesystem.
    pub fn from_text(title: &str, text: &str) -> Result<Self> {
        if text.len() > MAX_CHAPTER_BYTES || title.len() > 1024 {
            return Err(Error::Limit("TXT size or title"));
        }
        Ok(Self {
            metadata: Metadata {
                id: format!("{:x}", Sha256::digest(text.as_bytes())),
                title: title.into(),
                author: String::new(),
                language: "und".into(),
                chapters: vec!["text".into()],
                contents: vec![],
            },
            source: Source::MemoryText(text.into()),
            path: None,
            supplemental: vec![],
            progress_weights: Some(vec![text.len().max(1) as u64]),
        })
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_format(path.as_ref(), None)
    }

    /// Validate an EPUB in temporary storage independently of its file extension.
    pub fn open_epub(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_format(path.as_ref(), Some("epub"))
    }

    fn open_format(path: &Path, format: Option<&str>) -> Result<Self> {
        let mut hashing = FileHash::new(File::open(path)?);
        loop {
            if let Some(file) = hashing.step()? {
                return Self::from_identified_format(path, file, format);
            }
        }
    }
    /// Parse the same file handle used by a caller's document-identity calculation.
    /// The callback runs between bounded hash reads and before archive parsing.
    pub fn from_file(
        path: &Path,
        mut file: File,
        mut check: impl FnMut() -> std::io::Result<()>,
    ) -> Result<Self> {
        file.seek(SeekFrom::Start(0))?;
        let mut hashing = FileHash::new(file);
        loop {
            check()?;
            if let Some(file) = hashing.step()? {
                check()?;
                return Self::from_identified(path, file);
            }
        }
    }
    pub(crate) fn from_identified(path: &Path, file: IdentifiedFile) -> Result<Self> {
        Self::from_identified_format(path, file, None)
    }
    fn from_identified_format(
        path: &Path,
        hashed: IdentifiedFile,
        format: Option<&str>,
    ) -> Result<Self> {
        let IdentifiedFile { mut file, id } = hashed;
        file.seek(SeekFrom::Start(0))?;
        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase);
        match format.or(extension.as_deref()) {
            Some("epub") => {
                let options = EpubBookOptions {
                    zip_limits: Some(ZipLimits::new(image::MAX_ENCODED_BYTES, 64)),
                    max_nav_bytes: Some(MAX_NAVIGATION_BYTES),
                    ..Default::default()
                };
                let mut book = EpubBook::from_reader_with_options(file, options)
                    .map_err(|e| Error::Document(e.to_string()))?;
                if book.chapter_count() > 4096 {
                    return Err(Error::Limit("spine entries"));
                }
                let meta = book.metadata();
                let nav_base = book
                    .spine()
                    .toc_id()
                    .and_then(|id| meta.get_item(id))
                    .or_else(|| {
                        meta.manifest.iter().find(|item| {
                            item.properties
                                .as_deref()
                                .is_some_and(|p| p.split_whitespace().any(|p| p == "nav"))
                        })
                    })
                    .or_else(|| {
                        meta.manifest.iter().find(|item| {
                            item.media_type == "application/x-dtbncx+xml"
                                || item.href.to_ascii_lowercase().ends_with(".ncx")
                        })
                    })
                    .map(|item| item.href.clone())
                    .unwrap_or_default();
                let contents = book
                    .navigation()
                    .map(|nav| {
                        nav.toc_flat()
                            .into_iter()
                            .map(|(depth, entry)| ContentsEntry {
                                title: entry.label.clone(),
                                href: entry.href.clone(),
                                depth,
                                base: nav_base.clone(),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let chapters: Vec<String> = book.chapters().map(|chapter| chapter.href).collect();
                let spine_paths: std::collections::BTreeSet<_> = chapters
                    .iter()
                    .filter_map(|href| {
                        crate::navigation::resolve("", href)
                            .ok()
                            .map(|(path, _)| path)
                    })
                    .collect();
                let mut supplemental = BTreeMap::new();
                for item in &meta.manifest {
                    if item.media_type != "application/xhtml+xml" {
                        continue;
                    }
                    let Ok((path, _)) = crate::navigation::resolve("", &item.href) else {
                        continue;
                    };
                    if spine_paths.contains(&path) {
                        continue;
                    }
                    supplemental.insert(path, item.href.clone());
                    if supplemental.len() > 4096 {
                        return Err(Error::Limit("supplemental documents"));
                    }
                }
                let metadata = Metadata {
                    id,
                    title: meta.title.clone(),
                    author: meta.author.clone(),
                    language: meta.language.clone(),
                    chapters,
                    contents,
                };
                let progress_weights = (0..metadata.chapters.len())
                    .map(|index| {
                        book.chapter_uncompressed_size(index)
                            .ok()
                            .map(|size| size.max(1) as u64)
                    })
                    .collect::<Option<Vec<_>>>();
                Ok(Self {
                    metadata,
                    source: Source::Epub(Box::new(book)),
                    path: Some(path.to_owned()),
                    supplemental: supplemental.into_values().collect(),
                    progress_weights,
                })
            }
            Some("txt") => {
                if file.metadata()?.len() > MAX_CHAPTER_BYTES as u64 {
                    return Err(Error::Limit("TXT size"));
                }
                Ok(Self {
                    metadata: Metadata {
                        id,
                        title: path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned(),
                        author: String::new(),
                        language: "und".into(),
                        chapters: vec!["text".into()],
                        contents: vec![],
                    },
                    source: Source::Text(file),
                    path: Some(path.to_owned()),
                    supplemental: vec![],
                    progress_weights: Some(vec![1]),
                })
            }
            _ => Err(Error::Document("supported formats: EPUB, TXT".into())),
        }
    }

    pub fn document_count(&self) -> usize {
        self.metadata.chapters.len() + self.supplemental.len()
    }
    /// A reopened source must be checked against metadata.id before use.
    pub fn source_path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn document_href(&self, index: usize) -> Option<&String> {
        if index < self.metadata.chapters.len() {
            self.metadata.chapters.get(index)
        } else {
            self.supplemental.get(index - self.metadata.chapters.len())
        }
    }

    /// Estimate through the visible page, weighted by uncompressed spine sizes.
    pub(crate) fn estimated_progress(
        &self,
        chapter: usize,
        completed: usize,
        pages: usize,
    ) -> Option<u8> {
        let weights = self.progress_weights.as_ref()?;
        let weight = *weights.get(chapter)?;
        if weights.len() != self.metadata.chapters.len() || pages == 0 || completed > pages {
            return None;
        }
        let total = weights
            .iter()
            .try_fold(0u64, |sum, weight| sum.checked_add(*weight))?;
        let before = weights[..chapter]
            .iter()
            .try_fold(0u64, |sum, weight| sum.checked_add(*weight))?;
        if total == 0 {
            return None;
        }
        if chapter + 1 == weights.len() && completed == pages {
            return Some(100);
        }
        let fraction =
            (before as f64 + weight as f64 * completed as f64 / pages as f64) / total as f64;
        Some((fraction * 100.0).floor().clamp(0.0, 99.0) as u8)
    }

    /// The chapter that holds the given share of the book, and the share of
    /// that chapter that lies before it: the inverse of the estimate above.
    pub(crate) fn position_at(&self, percent: u8) -> Option<(usize, f64)> {
        let weights = self.progress_weights.as_ref()?;
        if weights.len() != self.metadata.chapters.len() {
            return None;
        }
        let total = weights
            .iter()
            .try_fold(0u64, |sum, weight| sum.checked_add(*weight))?;
        let wanted = total as f64 * f64::from(percent.min(100)) / 100.0;
        let mut before = 0.0;
        for (chapter, weight) in weights.iter().enumerate() {
            let weight = *weight as f64;
            if weight > 0.0 && wanted < before + weight {
                return Some((chapter, (wanted - before) / weight));
            }
            before += weight;
        }
        // The end of the book is the end of its last chapter with content.
        weights
            .iter()
            .rposition(|weight| *weight > 0)
            .map(|chapter| (chapter, 1.0))
    }

    pub fn chapter(&mut self, index: usize) -> Result<Chapter> {
        self.chapter_with_sources(index, false)
            .map(|(chapter, _)| chapter)
    }

    /// Retain XML token provenance for one chapter without changing its layout text.
    pub fn chapter_with_sources(
        &mut self,
        index: usize,
        track_source: bool,
    ) -> Result<(Chapter, Vec<SourceRun>)> {
        self.chapter_with_mapping(index, track_source)
            .map(|(chapter, sources, _, _)| (chapter, sources))
    }

    fn chapter_with_mapping(&mut self, index: usize, track_source: bool) -> Result<ChapterMapping> {
        let mut images = Vec::new();
        let mut elements = Vec::new();
        let mut sources = Vec::new();
        let href = self
            .document_href(index)
            .ok_or_else(|| Error::Document("chapter out of range".into()))?
            .clone();
        let mut blocks = Vec::new();
        let mut anchors = BTreeMap::new();
        let mut notes = Vec::new();
        let mut note_starts: Vec<Option<Locator>> = Vec::new();
        let mut pending = Vec::new();
        let mut links: Vec<DocumentLink> = Vec::new();
        let mut active_link = None::<(String, bool)>;
        match &mut self.source {
            Source::MemoryText(text) => text_blocks(text, &mut blocks),
            Source::Text(file) => {
                file.seek(SeekFrom::Start(0))?;
                let mut text = String::new();
                file.take((MAX_CHAPTER_BYTES + 1) as u64)
                    .read_to_string(&mut text)?;
                if text.len() > MAX_CHAPTER_BYTES {
                    return Err(Error::Limit("TXT size"));
                }
                text_blocks(&text, &mut blocks);
            }
            Source::Epub(book) => {
                let mut paragraph = Paragraph::default();
                let mut lists: Vec<(bool, i64, i64, ListNumberStyle)> = Vec::new();
                let mut items: Vec<(ListItem, ListNumberStyle)> = Vec::new();
                let mut options = ChapterEventsOptions {
                    max_items: MAX_CHAPTER_EVENTS,
                    ..Default::default()
                };
                options.render.track_source = track_source;
                options.render.memory.max_entry_bytes = MAX_CHAPTER_BYTES;
                options.render.memory.max_css_bytes = 128 * 1024;
                book.resource_events(&href, options, |event| {
                    match event {
                        StyledEventOrRun::SourceElement(element) => elements.push(element),
                        StyledEventOrRun::NoteBodyStart => note_starts.push(None),
                        StyledEventOrRun::NoteBodyEnd => {
                            if let Some(Some(start)) = note_starts.pop() {
                                let end = Locator {
                                    chapter: index,
                                    block: blocks.len(),
                                    byte: paragraph.text.len(),
                                };
                                if start < end {
                                    notes.push(start..end);
                                }
                            }
                        }
                        StyledEventOrRun::Anchor(id) => pending.push(id),
                        StyledEventOrRun::LinkStart(href) => active_link = Some((href, false)),
                        StyledEventOrRun::NoteReferenceStart(href) => {
                            active_link = Some((href, true))
                        }
                        StyledEventOrRun::LinkEnd => active_link = None,
                        StyledEventOrRun::Run(run) => {
                            if paragraph.list.is_none()
                                && !run.text.trim().is_empty()
                                && let Some((item, _)) = items.last_mut()
                            {
                                paragraph.list = Some(item.clone());
                                item.marker = None;
                            }
                            let start = Locator {
                                chapter: index,
                                block: blocks.len(),
                                byte: paragraph.text.len(),
                            };
                            if !run.text.trim().is_empty() {
                                for pending in &mut note_starts {
                                    pending.get_or_insert(start);
                                }
                                for id in pending.drain(..) {
                                    anchors.entry(id).or_insert(start);
                                }
                            }
                            if let Some((href, noteref)) = &active_link {
                                let end = Locator {
                                    byte: start.byte + run.text.len(),
                                    ..start
                                };
                                if let Some(last) = links.last_mut().filter(|last| {
                                    last.href == *href
                                        && last.noteref == *noteref
                                        && last.end == start
                                }) {
                                    last.end = end;
                                    last.label.push_str(&run.text);
                                } else {
                                    links.push(DocumentLink {
                                        noteref: *noteref,
                                        start,
                                        end,
                                        href: href.clone(),
                                        label: run.text.clone(),
                                    });
                                }
                            }
                            if let Some(source) = run.source {
                                sources.push(SourceRun {
                                    start,
                                    text: run.text.clone(),
                                    source,
                                    white_space: run.style.white_space,
                                });
                            }
                            paragraph.push(
                                &run.text,
                                TextStyle {
                                    indent: match run.style.text_indent {
                                        Some(epub_stream::css::TextIndent::Px(n)) => {
                                            TextIndent::Pixels(n)
                                        }
                                        Some(epub_stream::css::TextIndent::Em(n)) => {
                                            TextIndent::Em(n)
                                        }
                                        Some(epub_stream::css::TextIndent::Percent(n)) => {
                                            TextIndent::Percent(n)
                                        }
                                        _ => TextIndent::default(),
                                    },
                                    tab_size: match run.style.tab_size {
                                        Some(epub_stream::css::TabSize::Spaces(n)) => {
                                            TabSize::Spaces(n)
                                        }
                                        Some(epub_stream::css::TabSize::Px(n)) => TabSize::Px(n),
                                        Some(epub_stream::css::TabSize::Em(n)) => TabSize::Em(n),
                                        _ => TabSize::default(),
                                    },
                                    direction: match run.style.direction {
                                        Some(epub_stream::css::Direction::Ltr) => {
                                            BaseDirection::Ltr
                                        }
                                        Some(epub_stream::css::Direction::Rtl) => {
                                            BaseDirection::Rtl
                                        }
                                        _ => BaseDirection::Auto,
                                    },
                                    align: match run.style.text_align {
                                        Some(epub_stream::css::TextAlign::Left) => TextAlign::Left,
                                        Some(epub_stream::css::TextAlign::Right) => {
                                            TextAlign::Right
                                        }
                                        Some(epub_stream::css::TextAlign::Center) => {
                                            TextAlign::Center
                                        }
                                        Some(epub_stream::css::TextAlign::End) => TextAlign::End,
                                        Some(epub_stream::css::TextAlign::Justify) => {
                                            TextAlign::Justify
                                        }
                                        _ => TextAlign::Start,
                                    },
                                    hyphens: match run.style.hyphens {
                                        Some(epub_stream::css::Hyphens::None) => Hyphens::None,
                                        Some(epub_stream::css::Hyphens::Manual) => Hyphens::Manual,
                                        _ => Hyphens::Auto,
                                    },
                                    scale: (run.style.size_px / 16.0).clamp(0.5, 3.0),
                                    bold: run.style.weight >= 600,
                                    italic: run.style.italic,
                                },
                                run.language,
                            );
                        }
                        StyledEventOrRun::Event(event) => match event {
                            StyledEvent::ListStart(ordered) => {
                                flush_with_sources(&mut paragraph, &mut blocks, &mut sources);
                                if lists.len() >= 16 {
                                    return Err(epub_stream::error::EpubError::Parse(
                                        "list nesting limit exceeded".into(),
                                    ));
                                }
                                lists.push((ordered, 0, 1, ListNumberStyle::Decimal));
                            }
                            StyledEvent::ListEnd => {
                                flush_with_sources(&mut paragraph, &mut blocks, &mut sources);
                                lists.pop();
                            }
                            StyledEvent::ListStartAt(number) => {
                                if let Some((true, count, step, _)) = lists.last_mut() {
                                    *count = i64::from(number) - *step;
                                }
                            }
                            StyledEvent::ListReversed(number) => {
                                if let Some((true, count, step, _)) = lists.last_mut() {
                                    *step = -1;
                                    *count = i64::from(number) + 1;
                                }
                            }
                            StyledEvent::ListItemValue(number) => {
                                if let Some((true, count, _, _)) = lists.last_mut()
                                    && let Some((item, item_style)) = items.last_mut()
                                {
                                    *count = i64::from(number);
                                    item.marker_label = list_number(*count, *item_style);
                                    item.marker = Some(item.marker_label.clone());
                                }
                            }
                            StyledEvent::ListItemStyle(style) => {
                                use epub_stream::css::ListStyleType as L;
                                if let Some((item, _)) = items.last_mut() {
                                    let count = lists.last().map_or(1, |list| list.1);
                                    item.marker_label = match style {
                                        L::Disc | L::Inherit => "•".into(),
                                        L::Circle => "◦".into(),
                                        L::Square => "▪".into(),
                                        L::None => String::new(),
                                        L::Decimal => list_number(count, ListNumberStyle::Decimal),
                                        L::LowerAlpha => {
                                            list_number(count, ListNumberStyle::LowerAlpha)
                                        }
                                        L::UpperAlpha => {
                                            list_number(count, ListNumberStyle::UpperAlpha)
                                        }
                                        L::LowerRoman => {
                                            list_number(count, ListNumberStyle::LowerRoman)
                                        }
                                        L::UpperRoman => {
                                            list_number(count, ListNumberStyle::UpperRoman)
                                        }
                                    };
                                    item.marker = (!item.marker_label.is_empty())
                                        .then(|| item.marker_label.clone());
                                }
                            }
                            StyledEvent::ListNumberStyle(style) => {
                                if let Some((_, _, _, current)) = lists.last_mut() {
                                    *current = style;
                                }
                            }
                            StyledEvent::ListItemNumberStyle(style) => {
                                if let Some((true, count, _, _)) = lists.last()
                                    && let Some((item, item_style)) = items.last_mut()
                                {
                                    *item_style = style;
                                    item.marker_label = list_number(*count, style);
                                    item.marker = Some(item.marker_label.clone());
                                }
                            }
                            StyledEvent::ListItemStart => {
                                flush_with_sources(&mut paragraph, &mut blocks, &mut sources);
                                if items.len() >= 16 {
                                    return Err(epub_stream::error::EpubError::Parse(
                                        "list item nesting limit exceeded".into(),
                                    ));
                                }
                                let marker =
                                    if let Some((ordered, count, step, style)) = lists.last_mut() {
                                        *count += *step;
                                        if *ordered {
                                            list_number(*count, *style)
                                        } else {
                                            "•".into()
                                        }
                                    } else {
                                        "•".into()
                                    };
                                items.push((
                                    ListItem {
                                        depth: lists.len().max(1),
                                        marker_label: marker.clone(),
                                        marker: Some(marker),
                                    },
                                    lists.last().map(|list| list.3).unwrap_or_default(),
                                ));
                            }
                            StyledEvent::ListItemEnd => {
                                flush_with_sources(&mut paragraph, &mut blocks, &mut sources);
                                items.pop();
                            }
                            StyledEvent::LineBreak => {
                                paragraph.push("\n", TextStyle::default(), None)
                            }
                            _ => flush_with_sources(&mut paragraph, &mut blocks, &mut sources),
                        },
                        StyledEventOrRun::Image(image) => {
                            flush_with_sources(&mut paragraph, &mut blocks, &mut sources);
                            let start = Locator {
                                chapter: index,
                                block: blocks.len(),
                                byte: 0,
                            };
                            for note in &mut note_starts {
                                note.get_or_insert(start);
                            }
                            for id in pending.drain(..) {
                                anchors.entry(id).or_insert(start);
                            }
                            if let Some((href, noteref)) = &active_link {
                                links.push(DocumentLink {
                                    noteref: *noteref,
                                    start,
                                    end: Locator { byte: 1, ..start },
                                    href: href.clone(),
                                    label: image.alt.clone(),
                                });
                            }
                            if let Some(offset) = image.source_offset {
                                images.push(SourceImage {
                                    offset,
                                    location: Locator {
                                        chapter: index,
                                        block: blocks.len(),
                                        byte: 0,
                                    },
                                });
                            }
                            blocks.push(Block::Image(ImageBlock {
                                source: image.src,
                                alt: image.alt,
                                intrinsic: None,
                                width: image.width,
                                height: image.height,
                                max_width: image.max_width,
                                max_height: image.max_height,
                                min_width: image.min_width,
                                min_height: image.min_height,
                                width_hint: image.width_px,
                                height_hint: image.height_px,
                            }));
                        }
                    }
                    Ok(())
                })
                .map_err(|e| Error::Document(e.to_string()))?;
                flush_with_sources(&mut paragraph, &mut blocks, &mut sources);
                for id in pending.drain(..) {
                    anchors.entry(id).or_insert(Locator {
                        chapter: index,
                        block: blocks.len(),
                        byte: 0,
                    });
                }
            }
        }
        if blocks.len() > MAX_CHAPTER_ITEMS {
            return Err(Error::Limit("chapter blocks"));
        }
        let spans = blocks.iter().map(|block| match block {
            Block::Text(paragraph) => paragraph.spans.len(),
            Block::Image(_) => 0,
        });
        if spans.sum::<usize>() > MAX_CHAPTER_SPANS {
            return Err(Error::Limit("chapter spans"));
        }
        if blocks
            .iter()
            .filter(|block| matches!(block, Block::Image(_)))
            .count()
            > 256
        {
            return Err(Error::Limit("chapter images"));
        }
        let mut warnings = Vec::new();
        let mut sizes = std::collections::BTreeMap::new();
        for block in &mut blocks {
            if let Block::Image(image) = block {
                let size = sizes.entry(image.source.clone()).or_insert_with(|| {
                    self.resource(&image.source)
                        .and_then(|bytes| image::dimensions(&bytes))
                        .map_err(|error| error.to_string())
                });
                match size {
                    Ok(size) => image.intrinsic = Some(*size),
                    Err(error) => warnings.push(format!("{}: {error}", image.source)),
                }
            }
        }
        for block in &mut blocks {
            if let Block::Text(paragraph) = block {
                for span in &mut paragraph.spans {
                    if span.language.is_none() {
                        span.language = Some(if self.metadata.language.len() <= 63 {
                            self.metadata.language.clone()
                        } else {
                            "und".into()
                        });
                    }
                }
            }
        }
        Ok((
            Chapter {
                language: self.metadata.language.clone(),
                index,
                href,
                blocks,
                warnings,
                anchors,
                notes,
                links,
            },
            sources,
            elements,
            images,
        ))
    }

    /// Convert an EPUB source location to a normalized KOReader XPointer.
    pub fn koreader_position(&mut self, location: Locator) -> Result<String> {
        let (chapter, sources, elements, images) =
            self.chapter_with_mapping(location.chapter, true)?;
        let xml = self.koreader_xhtml(location.chapter, &chapter.href)?;
        if let Some(image) = images.iter().find(|image| image.location == location) {
            return crate::koreader::encode_image(
                &xml,
                location.chapter,
                &sources,
                &elements,
                image.offset,
            )
            .map(|pointer| pointer.to_string());
        }
        crate::koreader::encode(&xml, location.chapter, &sources, &elements, location)
            .map(|pointer| pointer.to_string())
    }

    /// Estimate a source location's fraction of the book for KOReader's display
    /// metadata. The XPointer remains the authoritative reading position.
    pub fn source_progress(&mut self, location: Locator) -> Result<f64> {
        let chapter = self.chapter(location.chapter)?;
        let length = |block: &Block| match block {
            Block::Text(paragraph) => paragraph.text.len().max(1),
            Block::Image(_) => 1,
        };
        let block = chapter
            .blocks
            .get(location.block)
            .ok_or_else(|| Error::Document("invalid source progress block".into()))?;
        let byte = match block {
            Block::Text(paragraph) if paragraph.text.is_char_boundary(location.byte) => {
                location.byte
            }
            Block::Image(_) if location.byte == 0 => 0,
            _ => return Err(Error::Document("invalid source progress offset".into())),
        };
        let total: usize = chapter.blocks.iter().map(length).sum();
        let before: usize = chapter.blocks[..location.block].iter().map(length).sum();
        let fraction = (before + byte) as f64 / total.max(1) as f64;
        let weights = self
            .progress_weights
            .as_ref()
            .ok_or(Error::Limit("book progress weights"))?;
        let weight = *weights
            .get(location.chapter)
            .ok_or(Error::Limit("book progress chapter"))?;
        let total = weights
            .iter()
            .try_fold(0u64, |sum, weight| sum.checked_add(*weight))
            .ok_or(Error::Limit("book progress weights"))?;
        let before = weights[..location.chapter]
            .iter()
            .try_fold(0u64, |sum, weight| sum.checked_add(*weight))
            .ok_or(Error::Limit("book progress weights"))?;
        Ok(((before as f64 + weight as f64 * fraction) / total.max(1) as f64).clamp(0.0, 1.0))
    }

    /// Resolve a KOReader EPUB XPointer without changing persistent reading state.
    pub fn resolve_koreader_position(&mut self, position: &str) -> Result<Locator> {
        let pointer = crate::koreader::XPointer::parse(position)?;
        let (chapter, sources, elements, images) =
            self.chapter_with_mapping(pointer.chapter(), true)?;
        let xml = self.koreader_xhtml(pointer.chapter(), &chapter.href)?;
        if pointer.is_image() {
            let offset = crate::koreader::decode_image(
                &xml,
                pointer.chapter(),
                &sources,
                &elements,
                &pointer,
            )?;
            return images
                .into_iter()
                .find(|image| image.offset == offset)
                .map(|image| image.location)
                .ok_or_else(|| Error::Document("KOReader image has no rendered source".into()));
        }
        crate::koreader::decode(&xml, pointer.chapter(), &sources, &elements, &pointer)
    }

    fn koreader_xhtml(&mut self, index: usize, href: &str) -> Result<String> {
        if index >= self.metadata.chapters.len() {
            return Err(Error::Document(
                "KOReader sync requires a spine chapter".into(),
            ));
        }
        let Source::Epub(book) = &mut self.source else {
            return Err(Error::Document(
                "KOReader positions currently require EPUB".into(),
            ));
        };
        let mut bytes = Vec::new();
        book.read_resource_into_with_hard_cap(href, &mut bytes, MAX_CHAPTER_BYTES)
            .map_err(|error| Error::Document(error.to_string()))?;
        String::from_utf8(bytes).map_err(|_| Error::Document("KOReader XHTML must be UTF-8".into()))
    }

    pub fn resolve_link(&mut self, base: &str, reference: &str) -> Result<Locator> {
        let (path, fragment) = crate::navigation::resolve(base, reference)?;
        let index = self
            .metadata
            .chapters
            .iter()
            .chain(&self.supplemental)
            .position(|href| {
                crate::navigation::resolve("", href).is_ok_and(|(candidate, _)| candidate == path)
            })
            .ok_or_else(|| {
                Error::Document("link target is not a declared XHTML document".into())
            })?;
        if let Some(fragment) = fragment.filter(|fragment| !fragment.is_empty()) {
            self.chapter(index)?
                .anchors
                .get(&fragment)
                .copied()
                .ok_or_else(|| Error::Document(format!("missing anchor: {fragment}")))
        } else {
            Ok(Locator {
                chapter: index,
                ..Default::default()
            })
        }
    }

    /// The encoded cover image, if the book names one within the size cap.
    pub fn cover(&mut self) -> Option<Vec<u8>> {
        let Source::Epub(book) = &mut self.source else {
            return None;
        };
        let mut options = epub_stream::book::CoverImageOptions::default();
        options.image.max_bytes = image::Limits::ALONE.encoded;
        let mut bytes = Vec::new();
        book.read_cover_image_into_with_options(&mut bytes, options)
            .ok()?
            .map(|_| bytes)
    }

    /// Reads an archive resource with an explicit size cap. Network URLs and
    /// host filesystem paths are never fetched through document resources.
    pub fn resource(&mut self, href: &str) -> Result<Vec<u8>> {
        if href.starts_with('/') || href.contains('\\') || href.contains(':') {
            return Err(Error::Document(
                "external image resources are unsupported".into(),
            ));
        }
        let Source::Epub(book) = &mut self.source else {
            return Err(Error::Document("TXT has no archive resources".into()));
        };
        let mut bytes = Vec::new();
        book.read_resource_into_with_hard_cap(href, &mut bytes, image::MAX_ENCODED_BYTES)
            .map_err(|e| Error::Document(e.to_string()))?;
        Ok(bytes)
    }
}

/// Owns the same file description through hashing and metadata parsing.
pub(crate) struct FileHash {
    file: Option<File>,
    digest: Option<crate::hashing::Hasher>,
    stamp: Option<crate::library_cache::FileStamp>,
}
pub(crate) struct IdentifiedFile {
    file: File,
    id: String,
}
impl IdentifiedFile {
    pub(crate) fn into_hash(self) -> FileHash {
        FileHash::new(self.file)
    }
    pub(crate) fn cached(file: File, id: String) -> Self {
        Self { file, id }
    }
    pub(crate) fn stamp(&self) -> Option<crate::library_cache::FileStamp> {
        crate::library_cache::FileStamp::read(&self.file)
    }
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
}
impl FileHash {
    pub(crate) fn new(file: File) -> Self {
        Self {
            stamp: crate::library_cache::FileStamp::read(&file),
            file: Some(file),
            digest: None,
        }
    }
    /// Batches hash reads until the budget expires. One read can exceed the budget.
    pub(crate) fn step_for(
        &mut self,
        budget: std::time::Duration,
    ) -> Result<Option<IdentifiedFile>> {
        let start = std::time::Instant::now();
        loop {
            let result = self.step()?;
            if result.is_some() || start.elapsed() >= budget {
                return Ok(result);
            }
        }
    }

    /// Hashes at most 8192 bytes; only EOF produces a parser-ready file.
    pub(crate) fn step(&mut self) -> Result<Option<IdentifiedFile>> {
        let result = self.step_inner();
        if result.is_err() {
            self.file = None;
            self.digest = None;
        }
        result
    }

    fn step_inner(&mut self) -> Result<Option<IdentifiedFile>> {
        let file = self
            .file
            .as_mut()
            .ok_or_else(|| Error::Document("file hash already completed".into()))?;
        if self.digest.is_none() {
            self.digest = Some(crate::hashing::Hasher::new()?);
        }
        let mut bytes = [0; 8192];
        let count = file.read(&mut bytes)?;
        if count == 0 {
            if let (Some(before), Some(after)) =
                (self.stamp, crate::library_cache::FileStamp::read(file))
                && before != after
            {
                return Err(Error::Document("file changed during hashing".into()));
            }
            let digest = self.digest.take().unwrap().finish()?;
            let id = crate::hashing::hex(&digest);
            return Ok(Some(IdentifiedFile {
                file: self.file.take().unwrap(),
                id,
            }));
        }
        self.digest.as_mut().unwrap().update(&bytes[..count])?;
        Ok(None)
    }
}

fn text_blocks(text: &str, blocks: &mut Vec<Block>) {
    let text = text.replace("\r\n", "\n");
    for paragraph in text.split("\n\n") {
        let mut p = Paragraph::default();
        p.push(paragraph.trim(), TextStyle::default(), None);
        if !p.text.is_empty() {
            blocks.push(Block::Text(p));
        }
    }
}

fn flush_with_sources(
    paragraph: &mut Paragraph,
    blocks: &mut Vec<Block>,
    sources: &mut Vec<SourceRun>,
) {
    if paragraph.text.trim().is_empty() {
        while sources
            .last()
            .is_some_and(|run| run.start.block >= blocks.len())
        {
            sources.pop();
        }
    }
    flush(paragraph, blocks);
}

fn flush(paragraph: &mut Paragraph, blocks: &mut Vec<Block>) {
    if !paragraph.text.trim().is_empty() {
        blocks.push(Block::Text(std::mem::take(paragraph)));
    } else {
        *paragraph = Paragraph::default();
    }
}

#[cfg(test)]
mod hashed_file_tests {
    use super::*;

    #[test]
    fn mutation_during_hashing_discards_the_digest_and_file() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.txt");
        std::fs::write(&path, vec![b'a'; 8192]).unwrap();
        let mut hashing = FileHash::new(File::open(&path).unwrap());
        assert!(hashing.step().unwrap().is_none());
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"changed")
            .unwrap();
        assert!(hashing.step().unwrap().is_none());
        let error = hashing.step().err().unwrap();
        assert!(error.to_string().contains("file changed during hashing"));
        assert!(hashing.file.is_none());
        assert!(hashing.digest.is_none());
        assert!(hashing.step().is_err());
    }

    #[test]
    fn hash_budget_batches_reads_without_changing_identity_or_consuming_parser_handle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.txt");
        let bytes = vec![b'a'; 32768];
        std::fs::write(&path, &bytes).unwrap();
        let mut hashing = FileHash::new(File::open(&path).unwrap());
        assert!(
            hashing
                .step_for(std::time::Duration::ZERO)
                .unwrap()
                .is_none()
        );
        let file = hashing
            .step_for(std::time::Duration::from_secs(5))
            .unwrap()
            .unwrap();
        assert_eq!(file.id(), format!("{:x}", Sha256::digest(&bytes)));
        assert_eq!(
            Document::from_identified(&path, file).unwrap().metadata.id,
            format!("{:x}", Sha256::digest(&bytes))
        );
    }

    #[test]
    fn hashing_requires_eof_and_bounds_each_step() {
        let dir = tempfile::tempdir().unwrap();
        for length in [0, 1, 8192, 8193, 32768] {
            let path = dir.path().join("book.txt");
            let bytes = vec![b'a'; length];
            std::fs::write(&path, &bytes).unwrap();
            let mut hashing = FileHash::new(File::open(&path).unwrap());
            for _ in 0..length.div_ceil(8192) {
                assert!(hashing.step().unwrap().is_none());
            }
            let file = hashing.step().unwrap().unwrap();
            assert_eq!(file.id(), format!("{:x}", Sha256::digest(&bytes)));
            assert!(hashing.step().is_err());
            let document = Document::from_identified(&path, file).unwrap();
            assert_eq!(
                document.metadata.id,
                format!("{:x}", Sha256::digest(&bytes))
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn epub_parser_uses_the_hashed_handle_after_path_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("book.epub");
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sample.epub");
        std::fs::copy(&fixture, &path).unwrap();
        let expected = Document::open(&fixture).unwrap();
        let mut hashing = FileHash::new(File::open(&path).unwrap());
        let file = loop {
            if let Some(file) = hashing.step().unwrap() {
                break file;
            }
        };
        std::fs::rename(&path, dir.path().join("original.epub")).unwrap();
        std::fs::write(&path, b"invalid replacement").unwrap();
        let parsed = Document::from_identified(&path, file).unwrap();
        assert_eq!(parsed.metadata.id, expected.metadata.id);
        assert_eq!(parsed.metadata.title, expected.metadata.title);
        assert_eq!(
            parsed.metadata.chapters.len(),
            expected.metadata.chapters.len()
        );
        assert!(Document::open(&path).is_err());
    }
    #[test]
    fn estimated_progress_weights_chapters_and_excludes_supplements() {
        let mut doc = Document::from_text("Progress", "text").unwrap();
        doc.metadata.chapters = vec!["short".into(), "long".into()];
        doc.progress_weights = Some(vec![1, 9]);
        assert_eq!(doc.estimated_progress(0, 1, 2), Some(5));
        assert_eq!(doc.estimated_progress(0, 2, 2), Some(10));
        assert_eq!(doc.estimated_progress(1, 1, 2), Some(55));
        assert_eq!(doc.estimated_progress(1, 2, 2), Some(100));
        assert_eq!(doc.position_at(0), Some((0, 0.0)));
        assert_eq!(doc.position_at(5), Some((0, 0.5)));
        assert_eq!(doc.position_at(10), Some((1, 0.0)));
        assert_eq!(doc.position_at(100), Some((1, 1.0)));
        assert_eq!(doc.estimated_progress(2, 1, 1), None);
        assert_eq!(doc.estimated_progress(0, 1, 0), None);
        assert_eq!(doc.estimated_progress(0, 3, 2), None);
        doc.progress_weights = Some(vec![u64::MAX, 1]);
        assert_eq!(doc.estimated_progress(0, 1, 2), None);
        doc.progress_weights = None;
        assert_eq!(doc.estimated_progress(0, 1, 2), None);
    }
}
