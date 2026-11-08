//! Turning StarDict definition data into plain text.

use alloc::string::String;
use alloc::vec::Vec;

/// The text of a definition's fields, typed by `types` (the dictionary's
/// `sametypesequence`) or by a type byte before each field.
pub(crate) fn decode(data: &[u8], types: Option<&str>) -> Result<String, crate::Error> {
    let mut rest = data;
    let mut text = String::new();
    match types {
        Some(types) => {
            if types.is_empty() {
                return Err(crate::malformed("empty definition type sequence"));
            }
            for (i, kind) in types.bytes().enumerate() {
                let (field, after) = split_field(rest, kind, i + 1 == types.len())?;
                append_field(&mut text, kind, field)?;
                rest = after;
            }
        }
        None => {
            while let Some((&kind, after)) = rest.split_first() {
                let (field, after) = split_field(after, kind, false)?;
                append_field(&mut text, kind, field)?;
                rest = after;
            }
        }
    }
    Ok(text)
}

fn append_field(text: &mut String, kind: u8, field: &[u8]) -> Result<(), crate::Error> {
    if kind.is_ascii_uppercase() {
        return Ok(());
    }
    let field =
        core::str::from_utf8(field).map_err(|_| crate::malformed("definition is not UTF-8"))?;
    let part = match kind {
        b'h' | b'g' | b'x' | b'k' | b'w' | b'r' => to_plain_text(field),
        b't' => alloc::format!("[{}]", field.trim()),
        b'm' | b'l' | b'y' => field.trim().into(),
        _ => return Ok(()),
    };
    if part.is_empty() {
        return Ok(());
    }
    let separator = usize::from(!text.is_empty());
    if part.len() > (64usize * 1024).saturating_sub(text.len() + separator) {
        return Err(crate::malformed("decoded definition too large"));
    }
    if separator != 0 {
        text.push('\n');
    }
    text.push_str(&part);
    Ok(())
}

/// Every non-final text field is NUL-terminated; binary fields have a big-endian size.
fn split_field(data: &[u8], kind: u8, last: bool) -> Result<(&[u8], &[u8]), crate::Error> {
    if !kind.is_ascii_alphabetic() {
        return Err(crate::malformed("invalid definition field type"));
    }
    if kind.is_ascii_uppercase() {
        if last {
            return Ok((data, &[]));
        }
        let size = data
            .get(..4)
            .ok_or_else(|| crate::malformed("binary field size missing"))?;
        let size = u32::from_be_bytes([size[0], size[1], size[2], size[3]]) as usize;
        let end = size
            .checked_add(4)
            .filter(|end| *end <= data.len())
            .ok_or_else(|| crate::malformed("binary field cut short"))?;
        return Ok((&data[4..end], &data[end..]));
    }
    if last {
        let field = data.strip_suffix(&[0]).unwrap_or(data);
        if field.contains(&0) {
            return Err(crate::malformed("data after final text field"));
        }
        return Ok((field, &[]));
    }
    let end = data
        .iter()
        .position(|b| *b == 0)
        .ok_or_else(|| crate::malformed("text field terminator missing"))?;
    Ok((&data[..end], &data[end + 1..]))
}

/// Plain text from simple markup (HTML, XDXF, Pango): tags are dropped,
/// block elements start new lines, list items get a bullet, entities are
/// decoded and white space is collapsed.
pub fn to_plain_text(markup: &str) -> String {
    let mut out = String::new();
    let mut chars = markup.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => {
                let mut tag = String::new();
                for c in chars.by_ref() {
                    if c == '>' {
                        break;
                    }
                    tag.push(c);
                }
                let name: String = tag
                    .trim_start_matches('/')
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric())
                    .map(|c| c.to_ascii_lowercase())
                    .collect();
                match name.as_str() {
                    "br" | "p" | "div" | "tr" | "ol" | "ul" | "dd" | "dt" | "blockquote" | "h1"
                    | "h2" | "h3" | "h4" | "h5" | "h6" | "def" | "ex" => out.push('\n'),
                    "li" if !tag.starts_with('/') => out.push_str("\n• "),
                    _ => {}
                }
            }
            '&' => {
                let mut entity = String::new();
                while let Some(&next) = chars.peek() {
                    if next == ';' || entity.len() > 8 {
                        break;
                    }
                    entity.push(next);
                    chars.next();
                }
                if chars.peek() == Some(&';') {
                    chars.next();
                    out.push_str(entity_text(&entity).as_str());
                } else {
                    out.push('&');
                    out.push_str(&entity);
                }
            }
            '\r' | '\n' | '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    // Collapse spaces, trim lines, drop empty ones.
    let mut text = String::new();
    for line in out.split('\n') {
        let line: Vec<&str> = line.split(' ').filter(|w| !w.is_empty()).collect();
        if line.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&line.join(" "));
    }
    text
}

fn entity_text(entity: &str) -> String {
    let c = match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => ' ',
        "laquo" => '«',
        "raquo" => '»',
        "ndash" => '–',
        "mdash" => '—',
        "hellip" => '…',
        "middot" => '·',
        _ => {
            let code = if let Some(hex) = entity.strip_prefix("#x").or(entity.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                entity.strip_prefix('#').and_then(|d| d.parse().ok())
            };
            match code.and_then(char::from_u32) {
                Some(c) => c,
                None => return alloc::format!("&{entity};"),
            }
        }
    };
    String::from(c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_becomes_plain_text() {
        let html = "<p><b>hus</b> <i>Substantiv</i></p><ol><li>das Haus</li><li>das &laquo;Heim&raquo; &amp; Co</li></ol>";
        assert_eq!(
            to_plain_text(html),
            "hus Substantiv\n• das Haus\n• das «Heim» & Co"
        );
        assert_eq!(to_plain_text("a&#228;b&#xF6;c"), "aäböc");
    }

    #[test]
    fn fields_by_type() {
        assert_eq!(
            decode(b"tone\0<b>Ton</b>", Some("th")).unwrap(),
            "[tone]\nTon"
        );
        assert_eq!(
            decode(b"mfirst\0hsecond<br>line\0", None).unwrap(),
            "first\nsecond\nline"
        );
    }
}
