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

/// Most family directories offered for selection.
const FAMILIES: usize = 24;

/// The subdirectories holding a `regular.ttf`, by name.
pub fn families(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().join("regular.ttf").is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.chars().count() <= 32)
        .collect();
    names.sort();
    names.truncate(FAMILIES);
    names
}

/// Loads `regular.ttf` with its optional `bold.ttf`, `italic.ttf` and
/// `bold-italic.ttf` as one family, plus `fallback1.ttf` to `fallback8.ttf`.
/// The family comes from the named subdirectory, or from the directory
/// itself; fallbacks always come from the directory.
/// Without a usable regular face the default family is used unchanged, so
/// styles of two different typefaces are never mixed.
pub fn load(
    directory: &Path,
    family: Option<&str>,
    default: impl FnOnce() -> Result<Fonts>,
    limit: u64,
) -> Result<Loaded> {
    let mut remaining = limit;
    let mut warnings = Vec::new();
    let styles = family.map_or_else(|| directory.to_owned(), |name| directory.join(name));
    let mut face = |path: std::path::PathBuf| match Font::load_bounded(&path, remaining) {
        Ok(font) => {
            remaining -= font.byte_len() as u64;
            Some(font)
        }
        Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            warnings.push(format!("{name}: {error}"));
            None
        }
    };
    let mut fonts = match face(styles.join("regular.ttf")) {
        Some(regular) => {
            let mut fonts = Fonts::new(regular);
            fonts.bold = face(styles.join("bold.ttf"));
            fonts.italic = face(styles.join("italic.ttf"));
            fonts.bold_italic = face(styles.join("bold-italic.ttf"));
            fonts
        }
        None => default()?,
    };
    fonts.fallback = (1..=8)
        .filter_map(|index| face(directory.join(format!("fallback{index}.ttf"))))
        .collect();
    Ok(Loaded {
        fonts,
        warnings,
        bytes: limit - remaining,
    })
}
