//! Fixed-name font sets with a shared limit on retained file bytes.
use crate::{
    Result,
    layout::{Font, Fonts},
};
use std::path::Path;

pub struct Loaded {
    pub fonts: Fonts,
    pub warnings: Vec<String>,
    /// Retained file bytes, excluding the caller's default face and parser caches.
    pub bytes: u64,
}

pub fn load(
    directory: &Path,
    default: impl FnOnce() -> Result<Font>,
    limit: u64,
) -> Result<Loaded> {
    let mut remaining = limit;
    let mut warnings = Vec::new();
    let mut face = |name: &str| match Font::load_bounded(directory.join(name), remaining) {
        Ok(font) => {
            remaining -= font.byte_len() as u64;
            Some(font)
        }
        Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            warnings.push(format!("{name}: {error}"));
            None
        }
    };
    let regular = face("regular.ttf");
    let bold = face("bold.ttf");
    let italic = face("italic.ttf");
    let bold_italic = face("bold-italic.ttf");
    let fallback = (1..=8)
        .filter_map(|index| face(&format!("fallback{index}.ttf")))
        .collect();
    let mut fonts = Fonts::new(match regular {
        Some(font) => font,
        None => default()?,
    });
    fonts.bold = bold;
    fonts.italic = italic;
    fonts.bold_italic = bold_italic;
    fonts.fallback = fallback;
    Ok(Loaded {
        fonts,
        warnings,
        bytes: limit - remaining,
    })
}
