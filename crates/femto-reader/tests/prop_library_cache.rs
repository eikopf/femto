//! Property tests of the library metadata cache: after any sequence of file
//! changes, a cached scan lists exactly what a scan without the cache lists
//! and what the files themselves say, reuses exactly the entries that are
//! still true, and an opened book has the identity of its bytes.
use femto_reader::{
    controller::ReaderStartup,
    layout::{Font, Fonts},
    state::{Library, LibraryScan, ReaderState},
};
use proptest::prelude::*;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const NAMES: [&str; 8] = [
    "a.txt",
    "b.txt",
    "B.TXT",
    "\u{e9}t\u{e9}.txt",
    "sub/a.txt",
    "sub/deep/c.txt",
    "sub/notes.md",
    "z.epub",
];
const LIMIT: usize = 64;

/// Small contents with repeats of length, so that a change can keep the size.
fn content(name: &str, index: usize) -> Vec<u8> {
    let fixture = |file: &str| {
        std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures").join(file))
            .unwrap()
    };
    if name.ends_with(".epub") {
        return match index {
            0 | 3 => fixture("sample.epub"),
            1 | 4 => fixture("navigation.epub"),
            _ => b"not an archive".to_vec(),
        };
    }
    match index {
        0 => b"one".to_vec(),
        1 => b"two".to_vec(),
        2 => b"three\n\nparagraphs\n\nhere".to_vec(),
        3 => Vec::new(),
        4 => vec![b'x'; 20_000],
        _ => vec![b'y'; 20_000],
    }
}

#[derive(Clone, Debug)]
enum Op {
    Write { name: usize, content: usize, keep_time: bool },
    Remove { name: usize },
    Rename { from: usize, to: usize },
    /// A complete cached scan.
    Scan,
    /// A cached scan that stops at the book limit.
    Truncated { limit: usize },
    /// A cached scan dropped after some steps.
    Abandoned { steps: usize },
    /// Opens a book through the cached opening path.
    Open { name: usize },
    Corrupt { bytes: Vec<u8> },
}

fn op() -> impl Strategy<Value = Op> {
    let name = || 0..NAMES.len();
    prop_oneof![
        8 => (name(), 0usize..6, any::<bool>())
            .prop_map(|(name, content, keep_time)| Op::Write { name, content, keep_time }),
        2 => name().prop_map(|name| Op::Remove { name }),
        3 => (name(), name()).prop_map(|(from, to)| Op::Rename { from, to }),
        6 => Just(Op::Scan),
        1 => (1usize..4).prop_map(|limit| Op::Truncated { limit }),
        1 => (0usize..12).prop_map(|steps| Op::Abandoned { steps }),
        8 => name().prop_map(|name| Op::Open { name }),
        1 => prop_oneof![
            Just(b"{".to_vec()),
            Just(br#"{"version":2,"root":"/","entries":[]}"#.to_vec()),
            prop::collection::vec(any::<u8>(), 0..40),
        ]
        .prop_map(|bytes| Op::Corrupt { bytes }),
    ]
}

fn fonts() -> Fonts {
    Fonts::new(
        Font::from_bytes(include_bytes!("../../../tests/fixtures/DejaVuSans.ttf").to_vec())
            .unwrap(),
    )
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

type Stamp = (u64, std::time::SystemTime);
fn stamp(path: &Path) -> Stamp {
    let metadata = std::fs::metadata(path).unwrap();
    (metadata.len(), metadata.modified().unwrap())
}

fn run_cached(root: &Path, limit: usize) -> (Library, usize) {
    let mut scan = LibraryScan::cached(root, limit).unwrap();
    loop {
        if let Some(library) = scan.step().unwrap() {
            return (library, scan.cache_hits());
        }
    }
}

fn listed(library: &Library) -> Vec<(PathBuf, String, String, String, String)> {
    library
        .entries
        .iter()
        .map(|entry| {
            let meta = &entry.metadata;
            (
                entry.path.clone(),
                meta.id.clone(),
                meta.title.clone(),
                meta.author.clone(),
                meta.language.clone(),
            )
        })
        .collect()
}

#[derive(Default)]
struct Stats {
    scans: usize,
    hits: usize,
    misses: usize,
    opens: usize,
    trusted: usize,
    stale_candidates: usize,
}

fn run(ops: &[Op], stats: &mut Stats) -> Result<(), TestCaseError> {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let cache_file = root.join(".femto-library-cache.json");
    // What the cache file is expected to say: path to content identity and
    // the file stamp it was recorded with. None while it is unreadable.
    let mut cache: Option<BTreeMap<PathBuf, (String, Stamp)>> = Some(BTreeMap::new());
    for (step, op) in ops.iter().enumerate() {
        match op {
            Op::Write { name, content: index, keep_time } => {
                let path = root.join(NAMES[*name]);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                let before = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                std::fs::write(&path, content(NAMES[*name], *index)).unwrap();
                if let (true, Some(before)) = (keep_time, before) {
                    std::fs::File::options()
                        .write(true)
                        .open(&path)
                        .unwrap()
                        .set_modified(before)
                        .unwrap();
                }
            }
            Op::Remove { name } => drop(std::fs::remove_file(root.join(NAMES[*name]))),
            Op::Rename { from, to } => {
                let target = root.join(NAMES[*to]);
                std::fs::create_dir_all(target.parent().unwrap()).unwrap();
                drop(std::fs::rename(root.join(NAMES[*from]), target));
            }
            Op::Corrupt { bytes } => {
                std::fs::write(&cache_file, bytes).unwrap();
                cache = None;
            }
            Op::Abandoned { steps } => {
                let before = std::fs::read(&cache_file).ok();
                let mut scan = LibraryScan::cached(&root, LIMIT).unwrap();
                let mut completed = None;
                for _ in 0..*steps {
                    completed = scan.step().unwrap();
                    if completed.is_some() {
                        break;
                    }
                }
                if let Some(library) = completed {
                    // Few enough books that it finished, and so saved.
                    prop_assert!(!library.truncated);
                    cache = Some(
                        library
                            .entries
                            .iter()
                            .map(|entry| (entry.path.clone(), (entry.metadata.id.clone(), stamp(&entry.path))))
                            .collect(),
                    );
                    continue;
                }
                let partial = scan.cancel();
                prop_assert!(partial.truncated);
                prop_assert_eq!(std::fs::read(&cache_file).ok(), before, "step {}", step);
                for entry in &partial.entries {
                    prop_assert_eq!(&entry.metadata.id, &hex(&std::fs::read(&entry.path).unwrap()));
                }
            }
            Op::Scan | Op::Truncated { .. } => {
                let limit = match op {
                    Op::Truncated { limit } => *limit,
                    _ => LIMIT,
                };
                let before = std::fs::read(&cache_file).ok();
                let plain = Library::scan_with_limit(&root, limit).unwrap();
                let (cached, hits) = run_cached(&root, limit);
                prop_assert_eq!(listed(&cached), listed(&plain), "step {}", step);
                prop_assert_eq!(cached.truncated, plain.truncated);
                // Every listed book is what its bytes and name say.
                for entry in &cached.entries {
                    let bytes = std::fs::read(&entry.path).unwrap();
                    prop_assert_eq!(&entry.metadata.id, &hex(&bytes), "step {}", step);
                    if entry.path.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt")) {
                        prop_assert_eq!(
                            entry.metadata.title.as_str(),
                            entry.path.file_stem().unwrap().to_str().unwrap()
                        );
                    }
                }
                // Reuse: exactly the listed books the cache already described.
                let usable = cache.as_ref().filter(|cache| cache.len() <= limit);
                let expected_hits = cached
                    .entries
                    .iter()
                    .filter(|entry| {
                        usable.is_some_and(|cache| {
                            cache.get(&entry.path).is_some_and(|(id, _)| *id == entry.metadata.id)
                        })
                    })
                    .count();
                prop_assert_eq!(hits, expected_hits, "step {}", step);
                stats.scans += 1;
                stats.hits += hits;
                stats.misses += cached.entries.len() - hits;
                if cached.truncated {
                    prop_assert_eq!(std::fs::read(&cache_file).ok(), before, "step {}", step);
                    continue;
                }
                // Without truncation every book file is listed or reported.
                let books = NAMES
                    .iter()
                    .filter(|name| !name.ends_with(".md") && root.join(name).is_file())
                    .count();
                prop_assert_eq!(cached.entries.len() + plain.errors.len(), books, "{:?}", plain.errors);
                prop_assert_eq!(
                    cached.errors.iter().filter(|e| !e.contains("library cache ignored")).count(),
                    plain.errors.len(),
                    "{:?}", cached.errors
                );
                cache = Some(
                    cached
                        .entries
                        .iter()
                        .map(|entry| (entry.path.clone(), (entry.metadata.id.clone(), stamp(&entry.path))))
                        .collect(),
                );
                // The saved cache lists the same library without a scan, and
                // a second scan reuses all of it and rewrites nothing.
                let snapshot = Library::load_cached(&root, LIMIT).unwrap();
                prop_assert_eq!(listed(&snapshot), listed(&cached), "step {}", step);
                prop_assert!(snapshot.errors.is_empty(), "{:?}", snapshot.errors);
                let saved = std::fs::read(&cache_file).unwrap();
                let (again, hits) = run_cached(&root, LIMIT);
                prop_assert_eq!(listed(&again), listed(&cached));
                prop_assert_eq!(hits, cached.entries.len());
                prop_assert_eq!(std::fs::read(&cache_file).unwrap(), saved);
            }
            Op::Open { name } => {
                let path = root.join(NAMES[*name]);
                let Ok(bytes) = std::fs::read(&path) else { continue };
                let Ok(mut startup) =
                    ReaderStartup::from_cached_file(&path, &root, LIMIT, fonts(), ReaderState::default())
                else {
                    continue;
                };
                let now = stamp(&path);
                let trusted = cache
                    .as_ref()
                    .and_then(|cache| cache.get(&path))
                    .filter(|(_, recorded)| *recorded == now)
                    .map(|(id, _)| id.clone());
                let opened = (|| {
                    while !startup.step()? {}
                    startup.finish()
                })();
                let Ok(reader) = opened else {
                    // Only a file that is no book fails to open; renames
                    // put archives under text names and text under .epub.
                    let archive = bytes.starts_with(b"PK");
                    let book = match NAMES[*name].rsplit('.').next() {
                        Some("epub") => archive,
                        Some("md") => false,
                        _ => !archive && !bytes.is_empty(),
                    };
                    prop_assert!(!book, "step {}: {:?} did not open", step, NAMES[*name]);
                    continue;
                };
                stats.opens += 1;
                let id = reader.document.metadata.id.clone();
                let actual = hex(&bytes);
                match trusted {
                    // Size and modification time match the cache: its
                    // identity is reused, as documented.
                    Some(trusted) => {
                        stats.trusted += 1;
                        stats.stale_candidates += usize::from(trusted != actual);
                        prop_assert_eq!(&id, &trusted, "step {}", step);
                    }
                    None => {
                        prop_assert_eq!(&id, &actual, "step {}", step);
                        // The opening recorded what it found.
                        match &mut cache {
                            Some(cache) => drop(cache.insert(path.clone(), (id, now))),
                            None => cache = Some([(path.clone(), (id, now))].into()),
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 150, max_shrink_iters: 300, ..ProptestConfig::default() })]

    #[test]
    fn cached_scans_and_openings_agree_with_the_files(ops in prop::collection::vec(op(), 1..40)) {
        let mut stats = Stats::default();
        run(&ops, &mut stats)?;
        if std::env::var_os("PROP_STATS").is_some() {
            eprintln!(
                "stats scans {} hits {} misses {} opens {} trusted {} stale {}",
                stats.scans, stats.hits, stats.misses, stats.opens, stats.trusted, stats.stale_candidates
            );
        }
    }
}
