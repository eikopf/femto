use femto_reader::{
    document::{Block, Document},
    layout::Locator,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
#[derive(Deserialize)]
struct Corpus {
    engine_revision: String,
    sha256: String,
    images: Vec<Image>,
}
#[derive(Deserialize)]
struct Image {
    pointer: String,
    source: String,
}
fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn corpus() -> Corpus {
    let path = std::env::var_os("FEMTO_KOREADER_IMAGE_FIXTURE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root().join("tests/fixtures/koreader-image-vectors.json"));
    let corpus: Corpus = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        corpus.engine_revision,
        "fb4d562a2765c52bff052759112a94a0b05239db"
    );
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(
                std::fs::read(root().join("tests/fixtures/kosync-images.epub")).unwrap()
            )
        ),
        corpus.sha256
    );
    corpus
}
fn positions(document: &mut Document) -> Vec<(Locator, String)> {
    let mut result = Vec::new();
    for chapter in 0..document.metadata.chapters.len() {
        let parsed = document.chapter(chapter).unwrap();
        for (block, content) in parsed.blocks.iter().enumerate() {
            if let Block::Image(image) = content {
                result.push((
                    Locator {
                        chapter,
                        block,
                        byte: 0,
                    },
                    // Femto resources are OPF-relative; the fixture OPF is in EPUB/.
                    format!(
                        "EPUB/{}",
                        femto_reader::navigation::resolve(&parsed.href, &image.source)
                            .unwrap()
                            .0
                    ),
                ));
            }
        }
    }
    result
}
#[test]
fn engine_image_pointers_preserve_repeated_elements_and_image_only_chapters() {
    let corpus = corpus();
    let mut document = Document::open(root().join("tests/fixtures/kosync-images.epub")).unwrap();
    let locations = positions(&mut document);
    assert_eq!(locations.len(), corpus.images.len());
    for ((location, source), image) in locations.into_iter().zip(corpus.images) {
        assert_eq!(source, image.source);
        assert_eq!(document.koreader_position(location).unwrap(), image.pointer);
        assert_eq!(
            document.resolve_koreader_position(&image.pointer).unwrap(),
            location
        );
        assert_eq!(
            document
                .resolve_koreader_position(&(image.pointer.clone() + ".0"))
                .unwrap(),
            location
        );
        assert!(
            document
                .resolve_koreader_position(&(image.pointer + ".1"))
                .is_err()
        );
        assert!(
            document
                .koreader_position(Locator {
                    byte: 1,
                    ..location
                })
                .is_err()
        );
    }
}
#[test]
#[ignore = "requires the pinned C++ engine through scripts/check-koreader-images.py"]
fn exported_images_resolve_in_the_official_engine() {
    use std::io::{Seek, SeekFrom, Write};
    let mut document = Document::open(root().join("tests/fixtures/kosync-images.epub")).unwrap();
    let mut queries = tempfile::tempfile().unwrap();
    let mut expected = Vec::new();
    for (location, source) in positions(&mut document) {
        let pointer = document.koreader_position(location).unwrap();
        writeln!(queries, "{pointer}").unwrap();
        expected.push((pointer, source));
    }
    queries.seek(SeekFrom::Start(0)).unwrap();
    let output = std::process::Command::new(root().join("scripts/koreader-position-oracle"))
        .args(["tests/fixtures/kosync-images.epub", "--resolve"])
        .current_dir(root())
        .stdin(queries)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert_eq!(stdout.lines().count(), expected.len());
    for (line, (pointer, source)) in stdout.lines().zip(expected) {
        let encoded: String = source
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(line, format!("IMAGE\t{pointer}\t{encoded}"));
    }
}
