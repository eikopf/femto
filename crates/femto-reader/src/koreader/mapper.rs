use super::{Step, TextNode, XPointer, text_nodes};
use crate::{
    Error, Result,
    document::{MAX_CHAPTER_BYTES, MAX_CHAPTER_ITEMS, SourceRun},
    layout::Locator,
};
use epub_stream::{css::Display, render_prep::SourceElement};
use quick_xml::{Reader, events::Event};
use std::{collections::BTreeMap, ops::Range};

fn unsupported() -> Error {
    Error::Document("KOReader position cannot be mapped to this chapter".into())
}

struct Frame {
    step: Step,
    elements: BTreeMap<String, usize>,
    children: usize,
    texts: usize,
    preserve: bool,
    block: bool,
}
impl Frame {
    fn new(name: String, index: usize, inherited_pre: bool) -> Self {
        let preserve = inherited_pre || matches!(name.as_str(), "pre" | "textarea");
        let block = matches!(
            name.as_str(),
            "address"
                | "article"
                | "aside"
                | "blockquote"
                | "body"
                | "caption"
                | "center"
                | "dd"
                | "details"
                | "dialog"
                | "div"
                | "dl"
                | "dt"
                | "fieldset"
                | "figcaption"
                | "figure"
                | "footer"
                | "form"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "header"
                | "hgroup"
                | "hr"
                | "legend"
                | "li"
                | "listing"
                | "main"
                | "menu"
                | "nav"
                | "ol"
                | "p"
                | "plaintext"
                | "pre"
                | "section"
                | "summary"
                | "table"
                | "tbody"
                | "td"
                | "textarea"
                | "tfoot"
                | "th"
                | "thead"
                | "tr"
                | "ul"
                | "xmp"
        );
        Self {
            step: Step { name, index },
            elements: BTreeMap::new(),
            children: 0,
            texts: 0,
            preserve,
            block,
        }
    }
}

/// Visits normalized text paths for a well-formed XHTML chapter. The caller's
/// styled runs supply computed whitespace modes for the visible text.
fn visit(
    xml: &str,
    chapter: usize,
    sources: &[SourceRun],
    elements: &[SourceElement],
    mut callback: impl FnMut(&XPointer, &TextNode) -> Result<()>,
    mut element_callback: impl FnMut(&XPointer, usize) -> Result<()>,
) -> Result<()> {
    if xml.len() > MAX_CHAPTER_BYTES {
        return Err(Error::Limit("KOReader XHTML"));
    }
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<Frame> = Vec::new();
    let mut pending: Option<Range<usize>> = None;
    let mut body_seen = false;
    let mut count = 0usize;
    loop {
        let start = reader.buffer_position() as usize;
        let event = reader.read_event().map_err(|_| unsupported())?;
        let end = reader.buffer_position() as usize;
        if matches!(event, Event::Text(_) | Event::GeneralRef(_)) {
            if !stack.is_empty() {
                if let Some(range) = &mut pending {
                    range.end = end;
                } else {
                    pending = Some(start..end);
                }
            }
            continue;
        }
        if let Some(range) = pending.take() {
            flush(
                xml,
                range,
                false,
                chapter,
                &mut stack,
                sources,
                &mut callback,
            )?;
        }
        match event {
            Event::Start(ref element) | Event::Empty(ref element) => {
                count += 1;
                if count > MAX_CHAPTER_ITEMS || stack.len() >= 128 {
                    return Err(Error::Limit("KOReader XML nodes"));
                }
                let name = std::str::from_utf8(element.local_name().as_ref())
                    .map_err(|_| unsupported())?
                    .to_owned();
                if name.len() > 256 {
                    return Err(unsupported());
                }
                if stack.is_empty() {
                    if name != "body" {
                        continue;
                    }
                    if body_seen {
                        return Err(unsupported());
                    }
                    body_seen = true;
                    stack.push(Frame::new(name, 1, false));
                } else {
                    let parent = stack.last_mut().unwrap();
                    parent.children += 1;
                    let index = parent.elements.entry(name.clone()).or_default();
                    *index += 1;
                    let frame = Frame::new(name, *index, parent.preserve);
                    stack.push(frame);
                }
                if let Ok(index) = elements.binary_search_by_key(&start, |element| element.offset) {
                    let style = &elements[index];
                    let inherited_block =
                        stack.iter().rev().nth(1).is_some_and(|parent| parent.block);
                    let frame = stack.last_mut().unwrap();
                    frame.preserve = matches!(
                        style.white_space,
                        epub_stream::css::WhiteSpace::Pre
                            | epub_stream::css::WhiteSpace::PreWrap
                            | epub_stream::css::WhiteSpace::PreLine
                    );
                    frame.block = match style.display {
                        Some(Display::Inline | Display::None) => false,
                        Some(Display::Block) => true,
                        Some(Display::Inherit) => inherited_block,
                        None => frame.block,
                    };
                }
                if stack
                    .last()
                    .is_some_and(|frame| matches!(frame.step.name.as_str(), "img" | "image"))
                {
                    let mut steps = vec![
                        Step {
                            name: "body".into(),
                            index: 1,
                        },
                        Step {
                            name: "DocFragment".into(),
                            index: chapter + 1,
                        },
                    ];
                    steps.extend(stack.iter().map(|frame| frame.step.clone()));
                    element_callback(
                        &XPointer {
                            steps,
                            offset: None,
                        },
                        start,
                    )?;
                }
                if matches!(event, Event::Empty(_)) {
                    stack.pop();
                }
            }
            Event::End(element) => {
                if let Some(frame) = stack.last() {
                    if element.local_name().as_ref() != frame.step.name.as_bytes() {
                        return Err(unsupported());
                    }
                    stack.pop();
                }
            }
            Event::CData(_) if !stack.is_empty() => {
                flush(
                    xml,
                    start + 9..end - 3,
                    true,
                    chapter,
                    &mut stack,
                    sources,
                    &mut callback,
                )?;
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !body_seen || !stack.is_empty() {
        return Err(unsupported());
    }
    Ok(())
}
fn flush(
    xml: &str,
    range: Range<usize>,
    cdata: bool,
    chapter: usize,
    stack: &mut [Frame],
    sources: &[SourceRun],
    callback: &mut impl FnMut(&XPointer, &TextNode) -> Result<()>,
) -> Result<()> {
    let Some(frame) = stack.last() else {
        return Ok(());
    };
    let first = sources.partition_point(|run| run.source.token.end <= range.start);
    let source = sources
        .get(first)
        .filter(|run| run.source.token.start < range.end);
    let preserve = source.map_or(frame.preserve, |run| {
        matches!(
            run.white_space,
            epub_stream::css::WhiteSpace::Pre
                | epub_stream::css::WhiteSpace::PreWrap
                | epub_stream::css::WhiteSpace::PreLine
        )
    });
    let strip = frame.children == 0 && matches!(frame.step.name.as_str(), "pre" | "textarea");
    text_nodes(
        &xml[range.clone()],
        range.start,
        preserve,
        cdata,
        strip,
        |node| {
            let frame = stack.last_mut().unwrap();
            if frame.block
                && frame.children == 0
                && !preserve
                && node
                    .text
                    .chars()
                    .all(|ch| matches!(ch, ' ' | '\r' | '\n' | '\t'))
            {
                return Ok(());
            }
            frame.children += 1;
            frame.texts += 1;
            let text_index = frame.texts;
            let mut steps = vec![
                Step {
                    name: "body".into(),
                    index: 1,
                },
                Step {
                    name: "DocFragment".into(),
                    index: chapter + 1,
                },
            ];
            steps.extend(stack.iter().map(|frame| frame.step.clone()));
            steps.push(Step {
                name: "text()".into(),
                index: text_index,
            });
            callback(
                &XPointer {
                    steps,
                    offset: Some(0),
                },
                &node,
            )
        },
    )
}

fn raw_position(xml: &str, run: &SourceRun, character: usize) -> Result<usize> {
    let token = xml.get(run.source.token.clone()).ok_or_else(unsupported)?;
    if token.starts_with('&') {
        return Ok(if character == 0 {
            run.source.token.start
        } else {
            run.source.token.end
        });
    }
    let (text, base) = if token.starts_with("<![CDATA[") {
        (
            token
                .strip_prefix("<![CDATA[")
                .and_then(|s| s.strip_suffix("]]>"))
                .ok_or_else(unsupported)?,
            run.source.token.start + 9,
        )
    } else {
        (token, run.source.token.start)
    };
    if character == text.chars().count() {
        return Ok(base + text.len());
    }
    text.char_indices()
        .nth(character)
        .map(|(byte, _)| base + byte)
        .ok_or_else(unsupported)
}

pub fn encode(
    xml: &str,
    chapter: usize,
    sources: &[SourceRun],
    elements: &[SourceElement],
    location: Locator,
) -> Result<XPointer> {
    if location.chapter != chapter {
        return Err(unsupported());
    }
    let run = sources
        .iter()
        .find(|run| {
            run.start.block == location.block
                && run.start.byte <= location.byte
                && location.byte < run.start.byte + run.text.len()
        })
        .or_else(|| {
            sources.iter().rev().find(|run| {
                run.start.block == location.block
                    && location.byte == run.start.byte + run.text.len()
            })
        })
        .ok_or_else(unsupported)?;
    let byte = location.byte - run.start.byte;
    if !run.text.is_char_boundary(byte) {
        return Err(unsupported());
    }
    let character = run.text[..byte].chars().count();
    let raw = raw_position(xml, run, run.source.characters[character])?;
    let mut result = None;
    visit(
        xml,
        chapter,
        sources,
        elements,
        |path, node| {
            if node.raw.contains(&raw) || (result.is_none() && raw == node.raw.end) {
                let mut path = path.clone();
                path.offset = Some(node.origins.partition_point(|&origin| origin < raw));
                result = Some(path);
            }
            Ok(())
        },
        |_, _| Ok(()),
    )?;
    result.ok_or_else(unsupported)
}

pub fn decode(
    xml: &str,
    chapter: usize,
    sources: &[SourceRun],
    elements: &[SourceElement],
    pointer: &XPointer,
) -> Result<Locator> {
    if pointer.chapter() != chapter {
        return Err(unsupported());
    }
    let mut raw = None;
    visit(
        xml,
        chapter,
        sources,
        elements,
        |path, node| {
            if path.steps == pointer.steps {
                let offset = pointer.offset.unwrap_or(0);
                raw = Some(if offset == node.origins.len() {
                    node.raw.end
                } else {
                    *node.origins.get(offset).ok_or_else(unsupported)?
                });
            }
            Ok(())
        },
        |_, _| Ok(()),
    )?;
    let raw = raw.ok_or_else(unsupported)?;
    for run in sources {
        if run.source.token.start <= raw && raw <= run.source.token.end {
            let token = xml.get(run.source.token.clone()).ok_or_else(unsupported)?;
            let decoded = if token.starts_with('&') {
                usize::from(raw == run.source.token.end)
            } else {
                let base =
                    run.source.token.start + if token.starts_with("<![CDATA[") { 9 } else { 0 };
                let end = raw
                    .max(base)
                    .min(run.source.token.end - if token.starts_with("<![CDATA[") { 3 } else { 0 });
                xml.get(base..end).ok_or_else(unsupported)?.chars().count()
            };
            let character = run
                .source
                .characters
                .partition_point(|&source| source < decoded);
            if character < run.source.characters.len() {
                let byte = run
                    .text
                    .char_indices()
                    .nth(character)
                    .map_or(run.text.len(), |(byte, _)| byte);
                return Ok(Locator {
                    byte: run.start.byte + byte,
                    ..run.start
                });
            }
        }
    }
    Err(unsupported())
}

/// Image positions identify an exact source element, including repeated images.
pub fn encode_image(
    xml: &str,
    chapter: usize,
    sources: &[SourceRun],
    elements: &[SourceElement],
    offset: usize,
) -> Result<XPointer> {
    let mut result = None;
    visit(
        xml,
        chapter,
        sources,
        elements,
        |_, _| Ok(()),
        |path, start| {
            if start == offset {
                result = Some(path.clone());
            }
            Ok(())
        },
    )?;
    result.ok_or_else(unsupported)
}

pub fn decode_image(
    xml: &str,
    chapter: usize,
    sources: &[SourceRun],
    elements: &[SourceElement],
    pointer: &XPointer,
) -> Result<usize> {
    if pointer.offset.is_some_and(|offset| offset != 0) {
        return Err(unsupported());
    }
    let mut result = None;
    visit(
        xml,
        chapter,
        sources,
        elements,
        |_, _| Ok(()),
        |path, start| {
            if path.steps == pointer.steps {
                result = Some(start);
            }
            Ok(())
        },
    )?;
    result.ok_or_else(unsupported)
}
