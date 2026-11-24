use femto_reader::{
    document::{Block, Document},
    layout::Locator,
};
fn document(name: &str) -> Document {
    Document::open(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name),
    )
    .unwrap()
}
#[test]
fn sample_positions_use_engine_paths_and_roundtrip_unicode_boundaries() {
    let mut document = document("sample.epub");
    let chapter = document.chapter(0).unwrap();
    let first = Locator {
        chapter: 0,
        block: 0,
        byte: 0,
    };
    assert_eq!(
        document.koreader_position(first).unwrap(),
        "/body[1]/DocFragment[1]/body[1]/h1[1]/text()[1].0"
    );
    assert_eq!(
        document
            .resolve_koreader_position("/body/DocFragment[1]/body/h1/text().0")
            .unwrap(),
        first
    );
    for (index, block) in chapter.blocks.iter().enumerate().take(5) {
        let Block::Text(paragraph) = block else {
            continue;
        };
        for (byte, ch) in paragraph.text.char_indices() {
            if ch.is_whitespace() {
                continue;
            }
            let location = Locator {
                chapter: 0,
                block: index,
                byte,
            };
            let pointer = document.koreader_position(location).unwrap();
            assert_eq!(
                document.resolve_koreader_position(&pointer).unwrap(),
                location,
                "{pointer}"
            );
        }
    }
}
#[test]
fn invalid_and_non_epub_positions_fail_without_state_mutation() {
    let mut document = document("sample.epub");
    for position in [
        "7",
        "/body/DocFragment[999]/body/p/text().0",
        "/body/DocFragment[1]/body/p[999]/text().0",
        "/body/DocFragment[1]/body/h1/text().99999",
    ] {
        assert!(document.resolve_koreader_position(position).is_err());
    }
    let mut text = Document::from_text("Text", "Sample").unwrap();
    assert!(text.koreader_position(Locator::default()).is_err());
}

#[test]
fn case_normalization_preserves_sibling_indices_and_inline_paths() {
    let mut document = document("kosync-case.epub");
    for (path, block, byte) in [
        ("p[1]/text()[1].0", 0, 0),
        ("p[1]/span[1]/text()[1].0", 0, 6),
        ("p[2]/text()[1].0", 1, 0),
        ("p[3]/text()[1].0", 2, 0),
    ] {
        let path = format!("/body[1]/DocFragment[1]/body[1]/{path}");
        let location = Locator {
            chapter: 0,
            block,
            byte,
        };
        assert_eq!(document.resolve_koreader_position(&path).unwrap(), location);
        assert_eq!(document.koreader_position(location).unwrap(), path);
    }
}

#[test]
fn split_entities_leave_other_positions_in_the_chapter_available() {
    let mut document = document("kosync-entity-boundary.epub");
    for (block, suffix) in ["mp;Z", "38;Z", "x26;Z"].iter().enumerate() {
        for (byte, node, offset) in [(0, 1, 0), (8190, 1, 8190), (8191, 2, suffix.len() - 1)] {
            let location = Locator {
                chapter: 0,
                block,
                byte,
            };
            let path = format!(
                "/body[1]/DocFragment[1]/body[1]/p[{}]/text()[{node}].{offset}",
                block + 1
            );
            assert_eq!(document.koreader_position(location).unwrap(), path);
            assert_eq!(document.resolve_koreader_position(&path).unwrap(), location);
        }
    }
}

#[test]
fn preformatted_line_endings_resolve_to_themselves_or_earlier() {
    // The source is "a\r\nb\n\rc\rd\ne". The engine reads "a\nb\nc\nd\ne":
    // it takes LF CR for one line break, where this reader shows two.
    let mut document = document("kosync-line-endings.epub");
    let chapter = document.chapter(0).unwrap();
    let Block::Text(paragraph) = &chapter.blocks[0] else {
        panic!("text expected");
    };
    assert_eq!(paragraph.text, "a\nb\n\nc\nd\ne");
    // Reading position, the engine's character, and the position it gives back.
    let positions = [
        (0, 0, 0),
        (1, 1, 1),
        (2, 2, 2),
        (3, 3, 3),
        (4, 3, 3),
        (5, 4, 5),
        (6, 5, 6),
        (7, 6, 7),
        (8, 7, 8),
        (9, 8, 9),
        (10, 9, 10),
    ];
    for (byte, offset, back) in positions {
        let location = Locator {
            chapter: 0,
            block: 0,
            byte,
        };
        let path = format!("/body[1]/DocFragment[1]/body[1]/pre[1]/text()[1].{offset}");
        assert_eq!(document.koreader_position(location).unwrap(), path);
        assert_eq!(
            document.resolve_koreader_position(&path).unwrap(),
            Locator {
                byte: back,
                ..location
            }
        );
    }
}
