use crate::{Error, Result};
use std::ops::Range;

/// One KOReader text node before layout, with an XML byte origin for every character.
#[derive(Clone, Debug)]
pub struct TextNode {
    pub raw: Range<usize>,
    pub text: String,
    pub origins: Vec<usize>,
}

/// Visits the engine's text chunks. Only one chunk's decoded text and origins are
/// retained at a time. `raw` excludes CDATA delimiters; `base` is its XML byte offset.
pub fn text_nodes(
    raw: &str,
    base: usize,
    preserve: bool,
    cdata: bool,
    strip_first_newline: bool,
    mut visit: impl FnMut(TextNode) -> Result<()>,
) -> Result<()> {
    if raw.len() > crate::document::MAX_CHAPTER_BYTES || base.checked_add(raw.len()).is_none() {
        return Err(Error::Limit("KOReader text source"));
    }
    let mut consumed = 0;
    let mut first = true;
    while consumed < raw.len() {
        let remaining = &raw[consumed..];
        let characters: Vec<_> = remaining.char_indices().take(8192).collect();
        let mut count = characters.len();
        if count == 8192 {
            for index in (0..count).rev() {
                let ch = characters[index].1;
                let next = characters.get(index + 1).map(|(_, ch)| *ch);
                if ch == ' '
                    || (ch == '\r' && next != Some('\n'))
                    || (ch == '\n' && next != Some('\r'))
                {
                    count = index + 1;
                    break;
                }
            }
        }
        let end = characters[count - 1].0 + characters[count - 1].1.len_utf8();
        let chunk = &remaining[..end];
        let mut node = decode(
            chunk,
            base + consumed,
            preserve,
            cdata,
            end < remaining.len(),
        )?;
        if first && strip_first_newline && node.text.starts_with('\n') {
            node.text.remove(0);
            node.origins.remove(0);
        }
        first = false;
        visit(node)?;
        consumed += end;
    }
    Ok(())
}
fn decode(
    raw: &str,
    base: usize,
    preserve: bool,
    cdata: bool,
    continues: bool,
) -> Result<TextNode> {
    let mut text = String::new();
    let mut origins = Vec::new();
    let mut chars = raw.char_indices().peekable();
    let mut space = false;
    let mut previous = '\0';
    let mut column = 0usize;
    while let Some((byte, ch)) = chars.next() {
        if ch == '&' && !cdata {
            let Some(length) = raw[byte..].find(';').map(|end| end + 1) else {
                if !continues {
                    return Err(Error::Document("unterminated KOReader entity".into()));
                }
                // The engine decodes each raw chunk separately. It drops a trailing
                // ampersand or incomplete numeric reference, but retains named fragments.
                let fragment = &raw[byte..];
                if fragment != "&" && !fragment.starts_with("&#") {
                    for (offset, ch) in fragment.char_indices() {
                        emit(
                            ch,
                            base + byte + offset,
                            preserve,
                            &mut text,
                            &mut origins,
                            &mut column,
                        );
                    }
                }
                break;
            };
            let entity = &raw[byte..byte + length];
            let decoded = quick_xml::escape::unescape(entity)
                .map_err(|_| Error::Document("unsupported KOReader entity".into()))?;
            while chars.peek().is_some_and(|(next, _)| *next < byte + length) {
                chars.next();
            }
            for decoded in decoded.chars() {
                emit(
                    decoded,
                    base + byte,
                    preserve,
                    &mut text,
                    &mut origins,
                    &mut column,
                );
            }
            space = false;
            previous = ';';
            continue;
        }
        if preserve {
            if ch == '\r' {
                if previous != '\n' && chars.peek().map(|(_, ch)| *ch) != Some('\n') {
                    emit(
                        '\n',
                        base + byte,
                        true,
                        &mut text,
                        &mut origins,
                        &mut column,
                    );
                    previous = '\n';
                }
                continue;
            }
            emit(ch, base + byte, true, &mut text, &mut origins, &mut column);
        } else if matches!(ch, ' ' | '\r' | '\n' | '\t') {
            if !space {
                emit(
                    ' ',
                    base + byte,
                    false,
                    &mut text,
                    &mut origins,
                    &mut column,
                );
            }
            space = true;
        } else {
            emit(ch, base + byte, false, &mut text, &mut origins, &mut column);
            space = false;
        }
        previous = ch;
    }
    Ok(TextNode {
        raw: base..base + raw.len(),
        text,
        origins,
    })
}
fn emit(
    ch: char,
    origin: usize,
    preserve: bool,
    text: &mut String,
    origins: &mut Vec<usize>,
    column: &mut usize,
) {
    if preserve && ch == '\t' {
        let spaces = 8 - *column % 8;
        for _ in 0..spaces {
            text.push(' ');
            origins.push(origin);
        }
        *column += spaces;
    } else {
        text.push(ch);
        origins.push(origin);
        if matches!(ch, '\r' | '\n') {
            *column = 1;
        } else {
            *column += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn nodes(raw: &str, preserve: bool, cdata: bool, strip: bool) -> Vec<TextNode> {
        let mut nodes = Vec::new();
        text_nodes(raw, 0, preserve, cdata, strip, |node| {
            nodes.push(node);
            Ok(())
        })
        .unwrap();
        nodes
    }
    #[test]
    fn entities_and_unicode_match_the_engine_probe() {
        let raw = "A  B&#32;  C &amp; 😀é";
        let node = nodes(raw, false, false, false).remove(0);
        assert_eq!(node.text, "A B  C & 😀é");
        assert_eq!(node.origins.len(), node.text.chars().count());
        let emoji = node.text.chars().position(|ch| ch == '😀').unwrap();
        assert_eq!(node.origins[emoji], raw.find('😀').unwrap());
        let amp = node.text.chars().position(|ch| ch == '&').unwrap();
        assert_eq!(node.origins[amp], raw.find("&amp;").unwrap());
    }
    #[test]
    fn long_nodes_split_at_engine_boundaries() {
        let raw = "a".repeat(20000);
        let chunks = nodes(&raw, false, false, false);
        assert_eq!(
            chunks
                .iter()
                .map(|node| node.text.len())
                .collect::<Vec<_>>(),
            [8192, 8192, 3616]
        );
        let raw = format!("{} {}", "😀".repeat(8000), "x".repeat(500));
        let chunks = nodes(&raw, false, false, false);
        assert_eq!(chunks[0].text.chars().count(), 8001);
        assert_eq!(chunks[1].text.len(), 500);
        assert_eq!(chunks[1].raw.start, 32001);
    }
    #[test]
    fn entities_crossing_a_chunk_boundary_match_the_engine_probe() {
        for entity in ["&amp;", "&#38;", "&#x26;"] {
            for cut in 1..entity.len() {
                let prefix = "a".repeat(8192 - cut);
                let raw = format!("{prefix}{entity}Z");
                let chunks = nodes(&raw, false, false, false);
                let fragment = if cut == 1 || entity.starts_with("&#") {
                    ""
                } else {
                    &entity[..cut]
                };
                assert_eq!(chunks.len(), 2, "{entity} cut {cut}");
                assert_eq!(chunks[0].text, format!("{prefix}{fragment}"));
                assert_eq!(chunks[1].text, format!("{}Z", &entity[cut..]));
                assert_eq!(chunks[1].origins[0], 8192);
            }
        }
    }
    #[test]
    fn preformatted_text_strips_initial_newline_and_expands_tabs() {
        assert_eq!(nodes("\nA\r\n B", true, false, true)[0].text, "A\n B");
        let node = nodes("a\t😀\n\tb", true, false, false).remove(0);
        assert_eq!(node.text, "a       😀\n       b");
        assert!(node.origins[1..8].iter().all(|&origin| origin == 1));
        assert_eq!(nodes("a &amp;  b", false, true, false)[0].text, "a &amp; b");
    }
}
