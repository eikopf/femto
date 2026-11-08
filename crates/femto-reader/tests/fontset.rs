use femto_reader::{
    document::Document,
    fontset,
    layout::{Font, LayoutConfig, paginate, rasterize},
};
use std::fs;
const SERIF: &[u8] = include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf");
const SANS: &[u8] = include_bytes!("../../../tests/fixtures/DejaVuSans.ttf");
fn default_font() -> femto_reader::Result<Font> {
    Font::from_bytes(SERIF.to_vec())
}

#[test]
fn missing_directory_uses_embedded_face_and_invalid_faces_are_isolated() {
    let root = tempfile::tempdir().unwrap();
    let absent = fontset::load(&root.path().join("missing"), default_font, 0).unwrap();
    assert_eq!(absent.bytes, 0);
    assert!(absent.warnings.is_empty());
    fs::write(root.path().join("regular.ttf"), b"invalid").unwrap();
    fs::create_dir(root.path().join("bold.ttf")).unwrap();
    fs::write(root.path().join("italic.ttf"), SERIF).unwrap();
    let loaded = fontset::load(root.path(), default_font, SERIF.len() as u64).unwrap();
    assert_eq!(loaded.warnings.len(), 2);
    assert!(loaded.fonts.bold.is_none());
    assert!(loaded.fonts.italic.is_some());
    assert_eq!(loaded.bytes, SERIF.len() as u64);
}

#[test]
fn aggregate_limit_is_enforced_and_failed_face_does_not_consume_budget() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("regular.ttf"), SANS).unwrap();
    fs::write(root.path().join("bold.ttf"), SERIF).unwrap();
    fs::write(root.path().join("italic.ttf"), SERIF).unwrap();
    let loaded = fontset::load(root.path(), default_font, SERIF.len() as u64).unwrap();
    assert!(SANS.len() > SERIF.len());
    assert_eq!(loaded.bytes, SERIF.len() as u64);
    assert_eq!(loaded.warnings.len(), 2);
    assert!(loaded.fonts.bold.is_some());
    assert!(loaded.fonts.italic.is_none());
}

#[test]
fn loaded_style_and_fallback_faces_reach_shaping_and_rasterization() {
    let root = tempfile::tempdir().unwrap();
    for name in ["regular.ttf", "bold.ttf", "italic.ttf", "bold-italic.ttf"] {
        fs::write(root.path().join(name), SERIF).unwrap();
    }
    fs::write(root.path().join("fallback1.ttf"), SANS).unwrap();
    let expected = (SERIF.len() * 4 + SANS.len()) as u64;
    let loaded = fontset::load(
        root.path(),
        || panic!("valid regular face must skip default"),
        expected,
    )
    .unwrap();
    assert!(loaded.warnings.is_empty());
    assert_eq!(loaded.bytes, expected);
    assert!(loaded.fonts.bold_italic.is_some());
    let mut doc = Document::open(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/sample.epub"),
    )
    .unwrap();
    let config = LayoutConfig::default();
    let pages = paginate(&doc.chapter(0).unwrap(), &loaded.fonts, config).unwrap();
    for face in [1, 2] {
        assert!(
            pages
                .pages
                .iter()
                .flat_map(|page| &page.lines)
                .flat_map(|line| &line.glyphs)
                .any(|glyph| glyph.font == face)
        );
    }
    let mut arabic = Document::from_text("Fallback", "مرحبا").unwrap();
    let pages = paginate(&arabic.chapter(0).unwrap(), &loaded.fonts, config).unwrap();
    assert!(
        pages.pages[0]
            .lines
            .iter()
            .flat_map(|line| &line.glyphs)
            .any(|glyph| glyph.font == 4 && glyph.id != 0)
    );
    assert!(
        rasterize(&pages.pages[0], &loaded.fonts, config)
            .unwrap()
            .iter()
            .any(|pixel| *pixel < 128)
    );
}
