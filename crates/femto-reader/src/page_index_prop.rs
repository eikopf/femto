//! Property tests of the persistent page index against a map model: a
//! record is returned exactly for the book, chapter, salt, fonts and layout
//! it was stored under, and damage to the file loses records but never
//! invents them.
use super::*;
use crate::layout::{Font, Hyphenation};
use proptest::prelude::*;

const BOOKS: [&str; 2] = [
    "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
];
const SALTS: [&str; 2] = ["build-a", "build-b"];
const CHAPTERS: [usize; 5] = [0, 1, 7, u32::MAX as usize, u32::MAX as usize + 1];

fn font(serif: bool) -> Font {
    let data: &[u8] = if serif {
        include_bytes!("../../../tests/fixtures/DejaVuSerif.ttf")
    } else {
        include_bytes!("../../../tests/fixtures/DejaVuSans.ttf")
    };
    Font::from_bytes(data.to_vec()).unwrap()
}

/// Font sets that differ in content, slot, or fallback order.
fn font_sets() -> Vec<Fonts> {
    let set = |regular: bool, slots: [Option<bool>; 3], fallback: &[bool]| {
        let mut fonts = Fonts::new(font(regular));
        fonts.bold = slots[0].map(font);
        fonts.italic = slots[1].map(font);
        fonts.bold_italic = slots[2].map(font);
        fonts.fallback = fallback.iter().copied().map(font).collect();
        fonts
    };
    vec![
        set(false, [None; 3], &[]),
        set(true, [None; 3], &[]),
        set(false, [Some(true), None, None], &[]),
        set(false, [None, Some(true), None], &[]),
        set(false, [None, None, Some(true)], &[]),
        set(false, [None; 3], &[true]),
        set(false, [Some(false), None, None], &[]),
        set(false, [None; 3], &[false]),
        set(false, [None; 3], &[false, true]),
        set(false, [None; 3], &[true, false]),
        set(true, [Some(false), None, None], &[]),
    ]
}

/// The default layout and every layout that differs from it in one field,
/// plus pairs of fields exchanged.
fn configs() -> Vec<LayoutConfig> {
    let base = LayoutConfig::default();
    vec![
        base,
        LayoutConfig { width: 481, ..base },
        LayoutConfig { height: 801, ..base },
        LayoutConfig { margin: 25, ..base },
        LayoutConfig { footer: 1, ..base },
        LayoutConfig { footer: 24, ..base },
        LayoutConfig { footer: 25, ..base },
        LayoutConfig { font_size: 24.5, ..base },
        LayoutConfig { line_height: 1.4, ..base },
        LayoutConfig { hyphenation: Hyphenation::Off, ..base },
        LayoutConfig { hyphenation: Hyphenation::German, ..base },
        LayoutConfig { width: 800, height: 480, ..base },
        LayoutConfig { margin: 0, footer: 24, ..base },
    ]
}

#[test]
fn every_layout_input_changes_the_key() {
    let fonts = font_sets();
    let configs = configs();
    let mut seen = std::collections::BTreeMap::new();
    for salt in SALTS {
        let store = IndexStore::new("unused", salt);
        for (font, fonts) in fonts.iter().enumerate() {
            for (config, &layout) in configs.iter().enumerate() {
                let key = store.key(fonts, layout);
                // Equal inputs give equal keys, also from a second store.
                assert_eq!(key, IndexStore::new("elsewhere", salt).key(fonts, layout));
                if let Some(other) = seen.insert(key, (salt, font, config)) {
                    panic!("{other:?} and {:?} share key {key:x}", (salt, font, config));
                }
            }
        }
    }
    assert_eq!(seen.len(), 2 * fonts.len() * configs.len());
}

#[derive(Clone, Debug)]
enum Op {
    Store {
        book: usize,
        key: (usize, usize, usize),
        chapter: usize,
        pages: Vec<[usize; 4]>,
    },
    /// Cuts the book's file short, as an interrupted copy would.
    Truncate { book: usize, at: prop::sample::Index },
    Overwrite { book: usize, bytes: Vec<u8> },
}

fn position() -> impl Strategy<Value = usize> {
    prop_oneof![
        200 => 0usize..6,
        20 => Just(u32::MAX as usize),
        1 => Just(u32::MAX as usize + 1),
        1 => Just(usize::MAX),
        30 => any::<u32>().prop_map(|value| value as usize),
    ]
}

fn op() -> impl Strategy<Value = Op> {
    // Few distinct keys, so that stores under the same and under a different
    // key both follow each other often.
    let key = prop_oneof![
        8 => Just((0usize, 0usize, 0usize)),
        1 => Just((1, 0, 0)),
        1 => Just((0, 1, 0)),
        1 => Just((0, 0, 4)),
        1 => (0usize..2, 0usize..2, 0usize..13),
    ];
    prop_oneof![
        12 => (
            0usize..2,
            key,
            prop_oneof![20 => prop::sample::select(&CHAPTERS[..3]), 1 => prop::sample::select(&CHAPTERS[3..])],
            prop::collection::vec([position(), position(), position(), position()], 0..5),
        )
            .prop_map(|(book, key, chapter, pages)| Op::Store { book, key, chapter, pages }),
        2 => (0usize..2, any::<prop::sample::Index>()).prop_map(|(book, at)| Op::Truncate { book, at }),
        1 => (0usize..2, prop::collection::vec(any::<u8>(), 0..80))
            .prop_map(|(book, bytes)| Op::Overwrite { book, bytes }),
    ]
}

type Records = Vec<(u32, Vec<[u32; 4]>)>;

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    #[test]
    fn the_store_agrees_with_a_map_of_the_last_key_per_book(
        ops in prop::collection::vec(op(), 1..25),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let stores = SALTS.map(|salt| IndexStore::new(dir.path(), salt));
        let fonts = [font_sets().swap_remove(0), font_sets().swap_remove(1)];
        let configs = configs();
        // Per book: the key of its file and the records in file order.
        let mut model: [Option<((usize, usize, usize), Records)>; 2] = [None, None];
        let (mut stored, mut replaced, mut rekeyed, mut skipped) = (0, 0, 0, 0);
        for op in &ops {
            match op {
                Op::Store { book, key, chapter, pages } => {
                    stores[key.0]
                        .store(BOOKS[*book], &fonts[key.1], configs[key.2], *chapter, &pages.iter().map(|page| PageBounds {
                            start: Locator { chapter: *chapter, block: page[0], byte: page[1] },
                            end: Locator { chapter: *chapter, block: page[2], byte: page[3] },
                        }).collect::<Vec<_>>())
                        .unwrap();
                    let narrow: Option<Vec<[u32; 4]>> = pages
                        .iter()
                        .map(|page| {
                            let mut out = [0; 4];
                            for (out, value) in out.iter_mut().zip(page) {
                                *out = u32::try_from(*value).ok()?;
                            }
                            Some(out)
                        })
                        .collect();
                    // A chapter or position beyond the format is not recorded.
                    if let (Ok(chapter), Some(pages)) = (u32::try_from(*chapter), narrow) {
                        let entry = &mut model[*book];
                        if entry.as_ref().is_none_or(|(old, _)| old != key) {
                            rekeyed += usize::from(entry.is_some());
                            *entry = Some((*key, Vec::new()));
                        }
                        let records = &mut entry.as_mut().unwrap().1;
                        replaced += usize::from(records.iter().any(|(other, _)| *other == chapter));
                        records.retain(|(other, _)| *other != chapter);
                        records.push((chapter, pages));
                        stored += 1;
                    } else {
                        skipped += 1;
                    }
                }
                Op::Truncate { book, at } => {
                    let path = stores[0].path(BOOKS[*book]);
                    let Ok(mut bytes) = std::fs::read(&path) else { continue };
                    let at = at.index(bytes.len() + 1);
                    bytes.truncate(at);
                    std::fs::write(&path, &bytes).unwrap();
                    if at < 12 {
                        model[*book] = None;
                    } else if let Some((_, records)) = &mut model[*book] {
                        // Complete records before the cut survive.
                        let mut end = 12;
                        records.retain(|(_, pages)| {
                            end += 8 + 16 * pages.len();
                            end <= at
                        });
                    }
                }
                Op::Overwrite { book, bytes } => {
                    std::fs::create_dir_all(dir.path()).unwrap();
                    std::fs::write(stores[0].path(BOOKS[*book]), bytes).unwrap();
                    model[*book] = None;
                }
            }
            // Observe every key and chapter of both books after every step.
            for (book, expected) in model.iter().enumerate() {
                for salt in 0..2 {
                    for font in 0..2 {
                        for (config, &layout) in configs.iter().enumerate() {
                            for chapter in CHAPTERS {
                                let loaded = stores[salt].load(BOOKS[book], &fonts[font], layout, chapter);
                                let expected = expected
                                    .as_ref()
                                    .filter(|(key, _)| *key == (salt, font, config))
                                    .and_then(|(_, records)| {
                                        records.iter().find(|(other, _)| *other as usize == chapter)
                                    })
                                    .map(|(_, pages)| {
                                        pages
                                            .iter()
                                            .map(|page| PageBounds {
                                                start: Locator { chapter, block: page[0] as usize, byte: page[1] as usize },
                                                end: Locator { chapter, block: page[2] as usize, byte: page[3] as usize },
                                            })
                                            .collect::<Vec<_>>()
                                    });
                                prop_assert_eq!(loaded, expected, "book {} key {:?} chapter {}", book, (salt, font, config), chapter);
                            }
                        }
                    }
                }
            }
        }
        if std::env::var_os("PROP_STATS").is_some() {
            eprintln!("ops {} stored {stored} replaced {replaced} rekeyed {rekeyed} skipped {skipped}", ops.len());
        }
    }

    /// Arbitrary file contents never panic and never produce a record under
    /// a key the file does not name.
    #[test]
    fn arbitrary_files_load_without_panicking(
        body in prop::collection::vec(any::<u8>(), 0..200),
        header in 0usize..4,
        chapter in prop::sample::select(&CHAPTERS[..]),
    ) {
        let dir = tempfile::tempdir().unwrap();
        let store = IndexStore::new(dir.path(), SALTS[0]);
        let fonts = font_sets().swap_remove(0);
        let layout = LayoutConfig::default();
        let mut bytes = Vec::new();
        if header >= 1 {
            bytes.extend_from_slice(MAGIC);
        }
        if header >= 2 {
            let key = store.key(&fonts, layout) ^ u64::from(header == 3);
            bytes.extend_from_slice(&key.to_le_bytes());
        }
        bytes.extend_from_slice(&body);
        std::fs::write(store.path(BOOKS[0]), &bytes).unwrap();
        let loaded = store.load(BOOKS[0], &fonts, layout, chapter);
        if header != 2 && !bytes.starts_with(MAGIC) {
            prop_assert_eq!(&loaded, &None);
        }
        if header == 3 {
            prop_assert_eq!(&loaded, &None);
        }
        if let Some(pages) = loaded {
            // Whatever is read can be recorded again and read back the same.
            store.store(BOOKS[1], &fonts, layout, chapter, &pages).unwrap();
            prop_assert_eq!(store.load(BOOKS[1], &fonts, layout, chapter), Some(pages));
        }
    }
}

/// The page and file limits leave earlier records readable.
#[test]
fn oversized_indexes_are_not_recorded_and_keep_earlier_records() {
    let dir = tempfile::tempdir().unwrap();
    let store = IndexStore::new(dir.path(), SALTS[0]);
    let fonts = font_sets().swap_remove(0);
    let layout = LayoutConfig::default();
    let page = |block| PageBounds {
        start: Locator { chapter: 0, block, byte: 0 },
        end: Locator { chapter: 0, block: block + 1, byte: 0 },
    };
    let pages = |count: usize| (0..count).map(page).collect::<Vec<_>>();
    store.store(BOOKS[0], &fonts, layout, 0, &pages(3)).unwrap();
    store.store(BOOKS[0], &fonts, layout, 1, &pages(MAX_PAGES + 1)).unwrap();
    assert_eq!(store.load(BOOKS[0], &fonts, layout, 1), None);
    store.store(BOOKS[0], &fonts, layout, 1, &pages(MAX_PAGES)).unwrap();
    assert_eq!(store.load(BOOKS[0], &fonts, layout, 1).map(|p| p.len()), Some(MAX_PAGES));
    // Eight chapters of the largest index fill the two-megabyte file.
    for chapter in 2..12 {
        store.store(BOOKS[0], &fonts, layout, chapter, &pages(MAX_PAGES)).unwrap();
    }
    assert!(std::fs::metadata(store.path(BOOKS[0])).unwrap().len() <= MAX_FILE_BYTES);
    assert_eq!(store.load(BOOKS[0], &fonts, layout, 0), Some(pages(3)));
    assert_eq!(store.load(BOOKS[0], &fonts, layout, 1).map(|p| p.len()), Some(MAX_PAGES));
    assert_eq!(store.load(BOOKS[0], &fonts, layout, 11), None);
}
