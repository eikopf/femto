use femto_reader::{
    document::Document,
    fontset,
    layout::{Font, Fonts, LayoutConfig, paginate, rasterize},
};
use std::fs;
const SERIF: &[u8] = include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf");
const SANS: &[u8] = include_bytes!("../../../tests/fixtures/DejaVuSans.ttf");
fn default_font() -> femto_reader::Result<Fonts> {
    let mut fonts = Fonts::new(Font::from_bytes(SERIF.to_vec())?);
    fonts.bold = Some(Font::from_bytes(SANS.to_vec())?);
    Ok(fonts)
}

#[test]
fn default_family_is_used_whole_unless_the_directory_has_a_regular_face() {
    let root = tempfile::tempdir().unwrap();
    let absent = fontset::load(&root.path().join("missing"), None, default_font, 0).unwrap();
    assert_eq!(absent.bytes, 0);
    assert!(absent.warnings.is_empty());
    assert!(absent.fonts.bold.is_some());
    // An unusable regular face keeps the default family; other styles in the
    // directory belong to a different typeface and are not read.
    fs::write(root.path().join("regular.ttf"), b"invalid").unwrap();
    fs::write(root.path().join("italic.ttf"), SERIF).unwrap();
    fs::write(root.path().join("fallback1.ttf"), SANS).unwrap();
    let loaded = fontset::load(root.path(), None, default_font, SANS.len() as u64).unwrap();
    assert_eq!(loaded.warnings.len(), 1);
    assert!(loaded.fonts.bold.is_some());
    assert!(loaded.fonts.italic.is_none());
    assert_eq!(loaded.fonts.fallback.len(), 1);
    assert_eq!(loaded.bytes, SANS.len() as u64);
    // A usable regular face replaces the whole family; invalid styles are isolated.
    fs::write(root.path().join("regular.ttf"), SERIF).unwrap();
    fs::create_dir(root.path().join("bold.ttf")).unwrap();
    let loaded = fontset::load(root.path(), None, default_font, 2 * SERIF.len() as u64).unwrap();
    assert_eq!(loaded.warnings.len(), 2);
    assert!(loaded.fonts.bold.is_none());
    assert!(loaded.fonts.italic.is_some());
    assert_eq!(loaded.bytes, 2 * SERIF.len() as u64);
}

#[test]
fn aggregate_limit_is_enforced_and_failed_face_does_not_consume_budget() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("regular.ttf"), SERIF).unwrap();
    fs::write(root.path().join("bold.ttf"), SANS).unwrap();
    fs::write(root.path().join("italic.ttf"), SERIF).unwrap();
    fs::write(root.path().join("fallback1.ttf"), SERIF).unwrap();
    let loaded = fontset::load(root.path(), None, default_font, 2 * SERIF.len() as u64).unwrap();
    assert!(SANS.len() > SERIF.len());
    // The oversized bold face is skipped and leaves its budget to the italic.
    assert_eq!(loaded.bytes, 2 * SERIF.len() as u64);
    assert_eq!(loaded.warnings.len(), 2);
    assert!(loaded.fonts.bold.is_none());
    assert!(loaded.fonts.italic.is_some());
    assert!(loaded.fonts.fallback.is_empty());
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
        None,
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

#[test]
fn named_families_load_from_subdirectories_and_share_the_fallbacks() {
    let root = tempfile::tempdir().unwrap();
    for family in ["Sans", "Serif", "Empty"] {
        fs::create_dir(root.path().join(family)).unwrap();
    }
    fs::write(root.path().join("Sans/regular.ttf"), SANS).unwrap();
    fs::write(root.path().join("Serif/regular.ttf"), SERIF).unwrap();
    fs::write(root.path().join("Serif/italic.ttf"), SERIF).unwrap();
    fs::write(root.path().join("fallback1.ttf"), SANS).unwrap();
    assert_eq!(fontset::families(root.path()), ["Sans", "Serif"]);
    assert!(fontset::families(&root.path().join("missing")).is_empty());
    let limit = 4 * SANS.len() as u64;
    let serif = fontset::load(root.path(), Some("Serif"), default_font, limit).unwrap();
    assert!(serif.warnings.is_empty());
    assert!(serif.fonts.italic.is_some() && serif.fonts.bold.is_none());
    assert_eq!(serif.fonts.fallback.len(), 1);
    let sans = fontset::load(root.path(), Some("Sans"), default_font, limit).unwrap();
    assert_eq!(serif.bytes, (2 * SERIF.len() + SANS.len()) as u64);
    assert_eq!(sans.bytes, 2 * SANS.len() as u64);
    // A family without a regular face leaves the default in place.
    let empty = fontset::load(root.path(), Some("Empty"), default_font, limit).unwrap();
    assert_eq!(empty.bytes, SANS.len() as u64);
    assert!(empty.fonts.bold.is_some());
}
