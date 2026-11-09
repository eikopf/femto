use femto_reader::document::{Block, Document};
use serde::Deserialize;
#[derive(Deserialize)]
struct Corpus {
    engine_revision: String,
    books: Vec<Book>,
}
#[derive(Deserialize)]
struct Book {
    file: String,
    sha256: String,
    nodes: Vec<Node>,
}
#[derive(Deserialize)]
struct Node {
    path: String,
    text: String,
}
#[test]
fn engine_paths_resolve_to_the_same_visible_characters() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let path = std::env::var_os("FEMTO_KOREADER_POSITION_FIXTURE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("koreader-position-vectors.json"));
    let corpus: Corpus = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(
        corpus.engine_revision,
        "fb4d562a2765c52bff052759112a94a0b05239db"
    );
    for book in corpus.books {
        use sha2::{Digest, Sha256};
        assert_eq!(
            format!(
                "{:x}",
                Sha256::digest(std::fs::read(root.join(&book.file)).unwrap())
            ),
            book.sha256,
            "fixture changed without regenerating engine vectors"
        );
        let mut document = Document::open(root.join(&book.file)).unwrap();
        for node in book.nodes {
            let characters: Vec<_> = node
                .text
                .chars()
                .enumerate()
                .filter(|(_, ch)| !ch.is_whitespace())
                .collect();
            if characters.is_empty() {
                continue;
            }
            for &(offset, expected) in [
                characters[0],
                characters[characters.len() / 2],
                *characters.last().unwrap(),
            ]
            .iter()
            {
                let path = format!("{}.{}", node.path.strip_suffix(".0").unwrap(), offset);
                let location = document
                    .resolve_koreader_position(&path)
                    .unwrap_or_else(|error| panic!("{} {path}: {error}", book.file));
                let chapter = document.chapter(location.chapter).unwrap();
                let Block::Text(paragraph) = &chapter.blocks[location.block] else {
                    panic!("text position resolved to image");
                };
                assert_eq!(
                    paragraph.text[location.byte..].chars().next(),
                    Some(expected),
                    "{} {path}",
                    book.file
                );
                let exported = document.koreader_position(location).unwrap();
                assert_eq!(
                    document.resolve_koreader_position(&exported).unwrap(),
                    location
                );
            }
        }
    }
}

#[test]
#[ignore = "requires the pinned C++ engine through scripts/check-koreader-positions.py"]
fn exported_positions_resolve_in_the_official_engine() {
    use std::io::Write;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in [
        "sample.epub",
        "whitespace.epub",
        "navigation.epub",
        "lists.epub",
        "tab-size.epub",
        "kosync-css.epub",
    ] {
        let path = root.join("tests/fixtures").join(name);
        let mut document = Document::open(&path).unwrap();
        let mut expected = Vec::new();
        let mut queries = tempfile::tempfile().unwrap();
        for index in 0..document.metadata.chapters.len() {
            let chapter = document.chapter(index).unwrap();
            for (block, content) in chapter.blocks.iter().enumerate() {
                let Block::Text(paragraph) = content else {
                    continue;
                };
                let chars: Vec<_> = paragraph
                    .text
                    .char_indices()
                    .filter(|(_, ch)| !ch.is_whitespace())
                    .collect();
                if chars.is_empty() {
                    continue;
                }
                for &(byte, ch) in [chars[0], chars[chars.len() / 2], *chars.last().unwrap()].iter()
                {
                    let locator = femto_reader::layout::Locator {
                        chapter: index,
                        block,
                        byte,
                    };
                    let pointer = document.koreader_position(locator).unwrap();
                    writeln!(queries, "{pointer}").unwrap();
                    expected.push((pointer, ch));
                }
            }
        }
        use std::io::{Seek, SeekFrom};
        queries.seek(SeekFrom::Start(0)).unwrap();
        let output = std::process::Command::new(root.join("scripts/koreader-position-oracle"))
            .arg(path)
            .arg("--resolve")
            .current_dir(&root)
            .stdin(queries)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let lines: Vec<_> = stdout.lines().collect();
        assert_eq!(lines.len(), expected.len());
        for (line, (sent, ch)) in lines.iter().zip(expected) {
            let (pointer, hex) = line.split_once('\t').unwrap();
            assert_ne!(pointer, "INVALID", "{name}: {sent}");
            let offset: usize = pointer.rsplit_once('.').unwrap().1.parse().unwrap();
            let bytes: Vec<_> = hex
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
                .collect();
            let text = String::from_utf8(bytes).unwrap();
            assert_eq!(text.chars().nth(offset), Some(ch), "{name}: {sent}");
        }
    }
}
