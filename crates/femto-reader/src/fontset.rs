//! Fixed-name font sets with a shared limit on retained file bytes.
use crate::{
    Result,
    layout::{Font, Fonts},
};
use std::path::Path;

pub struct Loaded {
    pub fonts: Fonts,
    pub warnings: Vec<String>,
    /// Retained file bytes, excluding the caller's default family and parser caches.
    pub bytes: u64,
}

/// Loads `regular.ttf` with its optional `bold.ttf`, `italic.ttf` and
/// `bold-italic.ttf` as one family, plus `fallback1.ttf` to `fallback8.ttf`.
/// Without a usable regular face the default family is used unchanged, so
/// styles of two different typefaces are never mixed.
pub fn load(
    directory: &Path,
    default: impl FnOnce() -> Result<Fonts>,
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
    let mut fonts = match face("regular.ttf") {
        Some(regular) => {
            let mut fonts = Fonts::new(regular);
            fonts.bold = face("bold.ttf");
            fonts.italic = face("italic.ttf");
            fonts.bold_italic = face("bold-italic.ttf");
            fonts
        }
        None => default()?,
    };
    fonts.fallback = (1..=8)
        .filter_map(|index| face(&format!("fallback{index}.ttf")))
        .collect();
    Ok(Loaded {
        fonts,
        warnings,
        bytes: limit - remaining,
    })
}
