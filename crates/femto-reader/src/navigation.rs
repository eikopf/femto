//! Internal EPUB URI resolution. No target is opened as a host or network URL.
use crate::{Error, Result};

pub fn resolve(base: &str, reference: &str) -> Result<(String, Option<String>)> {
    if reference.len() > 4096 || base.len() > 4096 {
        return Err(Error::Limit("navigation URI"));
    }
    let (path, fragment) = reference
        .split_once('#')
        .map_or((reference, None), |(p, f)| (p, Some(f)));
    let path = decode(path)?;
    let base = decode(base.split('#').next().unwrap_or_default())?;
    let invalid = |s: &str| {
        s.starts_with('/') || s.contains([':', '\\', '?']) || s.chars().any(char::is_control)
    };
    if invalid(&path) || invalid(&base) {
        return Err(Error::Document(
            "external or unsupported navigation URI".into(),
        ));
    }
    let combined = if path.is_empty() {
        base
    } else {
        match base.rsplit_once('/') {
            Some((dir, _)) => format!("{dir}/{path}"),
            None => path,
        }
    };
    let mut parts = Vec::new();
    for part in combined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(Error::Document("navigation escapes archive root".into()));
                }
            }
            part => parts.push(part),
        }
    }
    Ok((parts.join("/"), fragment.map(decode).transpose()?))
}

fn decode(value: &str) -> Result<String> {
    let mut result = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes.next().and_then(|b| (b as char).to_digit(16));
            let low = bytes.next().and_then(|b| (b as char).to_digit(16));
            let (Some(high), Some(low)) = (high, low) else {
                return Err(Error::Document("invalid URI escape".into()));
            };
            result.push((high * 16 + low) as u8);
        } else {
            result.push(byte);
        }
    }
    String::from_utf8(result).map_err(|_| Error::Document("navigation URI is not UTF-8".into()))
}
