//! Portable document, layout, and reader state for Femto.

pub mod clipping;
pub mod controller;
pub mod dictionary;
pub mod display;
pub mod document;
pub mod download;
pub mod fontset;
mod glyph_cache;
pub mod hashing;
pub mod image;
pub mod journal;
pub mod koreader;
pub mod layout;
mod library_cache;
mod menu;
pub mod navigation;
pub mod page_index;
pub mod pagination;
pub mod remote;
mod scripts;
pub mod selection;
pub mod state;
pub mod ui;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("document: {0}")]
    Document(String),
    #[error("invalid font")]
    Font,
    #[error("resource limit: {0}")]
    Limit(&'static str),
    #[error("invalid layout: {0}")]
    Layout(&'static str),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Png(#[from] png::EncodingError),
    #[error("image: {0}")]
    Image(String),
}

pub type Result<T> = std::result::Result<T, Error>;

// Xtensa LLVM 1.97 fails to lower Serde's optimized f32 numeric visitor.
// Decode through f64, then narrow as the JSON float path does.
pub(crate) fn deserialize_f32<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<f32, D::Error> {
    <f64 as serde::Deserialize>::deserialize(deserializer).map(|value| value as f32)
}
