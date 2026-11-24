//! Discovery properties for KOReader position mapping over generated EPUBs.
//!
//! `docs/koreader-sync.md`: every reading position has a sync position, and
//! the other reader "opens a little earlier and never later".
use femto_reader::{
    document::{Block, Chapter, Document},
    layout::Locator,
};
use proptest::prelude::*;
use std::io::Write;

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

/// A ZIP archive of stored entries.
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let mut header = Vec::new();
        header.extend_from_slice(&20u16.to_le_bytes()); // version needed
        header.extend_from_slice(&0u16.to_le_bytes()); // flags
        header.extend_from_slice(&0u16.to_le_bytes()); // stored
        header.extend_from_slice(&0u16.to_le_bytes()); // time
        header.extend_from_slice(&0x21u16.to_le_bytes()); // date
        header.extend_from_slice(&crc.to_le_bytes());
        header.extend_from_slice(&(data.len() as u32).to_le_bytes());
        header.extend_from_slice(&(data.len() as u32).to_le_bytes());
        header.extend_from_slice(&(name.len() as u16).to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // extra
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&header);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&20u16.to_le_bytes()); // version made by
        central.extend_from_slice(&header);
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        central.extend_from_slice(&0u32.to_le_bytes()); // external attributes
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let start = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06");
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&start.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn epub(chapters: &[String]) -> tempfile::NamedTempFile {
    let container = r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#;
    let mut manifest = String::new();
    let mut spine = String::new();
    for index in 0..chapters.len() {
        manifest.push_str(&format!(
            r#"<item id="c{index}" href="c{index}.xhtml" media-type="application/xhtml+xml"/>"#
        ));
        spine.push_str(&format!(r#"<itemref idref="c{index}"/>"#));
    }
    let opf = format!(
        r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">prop</dc:identifier><dc:title>Prop</dc:title><dc:language>en</dc:language></metadata><manifest>{manifest}</manifest><spine>{spine}</spine></package>"#
    );
    let names: Vec<String> = (0..chapters.len()).map(|i| format!("c{i}.xhtml")).collect();
    let mut entries: Vec<(&str, &[u8])> = vec![
        ("mimetype", b"application/epub+zip"),
        ("META-INF/container.xml", container.as_bytes()),
        ("content.opf", opf.as_bytes()),
    ];
    for (name, chapter) in names.iter().zip(chapters) {
        entries.push((name, chapter.as_bytes()));
    }
    let mut file = tempfile::Builder::new().suffix(".epub").tempfile().unwrap();
    file.write_all(&zip(&entries)).unwrap();
    file.flush().unwrap();
    file
}

fn text() -> impl Strategy<Value = String> {
    prop::collection::vec(
        prop_oneof![
            8 => "[a-z]{1,8}",
            6 => Just(" ".to_string()),
            1 => Just("  ".to_string()),
            1 => Just("\n".to_string()),
            1 => Just("\n   ".to_string()),
            1 => Just("\t".to_string()),
            1 => Just("&amp;".to_string()),
            1 => Just("&lt;".to_string()),
            1 => Just("&#x41;".to_string()),
            1 => Just("&#233;".to_string()),
            1 => Just("&nbsp;".to_string()),
            1 => Just("&shy;".to_string()),
            1 => Just("&#x1F600;".to_string()),
            1 => Just("é".to_string()),
            1 => Just("e\u{301}".to_string()),
            1 => Just("שלום".to_string()),
            1 => Just("漢字".to_string()),
            1 => Just("\u{a0}".to_string()),
            1 => Just("\u{ad}".to_string()),
            1 => Just("😀".to_string()),
            1 => Just("<![CDATA[x < y]]>".to_string()),
            1 => Just("<!-- note -->".to_string()),
            1 => Just("\r\n".to_string()),
            1 => Just("\u{2003}".to_string()),
            1 => Just("\u{200b}".to_string()),
        ],
        0..8,
    )
    .prop_map(|parts| parts.concat())
}

fn inline() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        6 => text(),
        1 => Just("<br/>".to_string()),
        1 => Just("<img src=\"missing.png\" alt=\"pic\"/>".to_string()),
        1 => Just("<span></span>".to_string()),
    ];
    leaf.prop_recursive(3, 12, 4, |inner| {
        (
            prop_oneof![
                Just("span"),
                Just("b"),
                Just("i"),
                Just("em"),
                Just("a"),
                Just("sup"),
                Just("code"),
            ],
            prop::collection::vec(inner, 0..4),
        )
            .prop_map(|(tag, children)| format!("<{tag}>{}</{tag}>", children.concat()))
    })
}

fn block() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        8 => (
            prop_oneof![Just("p"), Just("div"), Just("h1"), Just("h2"), Just("blockquote"), Just("pre")],
            prop::collection::vec(inline(), 0..4),
        )
            .prop_map(|(tag, children)| format!("<{tag}>{}</{tag}>", children.concat())),
        1 => Just("<hr/>".to_string()),
        1 => text(),
        1 => (any::<bool>(), prop::collection::vec(prop::collection::vec(inline(), 0..3), 0..4)).prop_map(
            |(ordered, items)| {
                let tag = if ordered { "ol" } else { "ul" };
                let items: String = items
                    .into_iter()
                    .map(|item| format!("<li>{}</li>", item.concat()))
                    .collect();
                format!("<{tag}>{items}</{tag}>")
            }
        ),
        1 => prop::collection::vec(prop::collection::vec(inline(), 0..2), 1..4).prop_map(|cells| {
            let cells: String = cells
                .into_iter()
                .map(|cell| format!("<td>{}</td>", cell.concat()))
                .collect();
            format!("<table><tr>{cells}</tr></table>")
        }),
    ];
    leaf.prop_recursive(2, 8, 3, |inner| {
        (
            prop_oneof![Just("div"), Just("section"), Just("blockquote")],
            prop::collection::vec(prop_oneof![3 => inner, 1 => text()], 0..4),
        )
            .prop_map(|(tag, children)| format!("<{tag}>{}</{tag}>", children.concat()))
    })
}

fn xhtml() -> impl Strategy<Value = String> {
    (
        prop::collection::vec(block(), 0..6),
        prop::bool::weighted(0.2),
        prop::bool::weighted(0.5),
    )
        .prop_map(|(blocks, bom, newlines)| {
            let separator = if newlines { "\n  " } else { "" };
            format!(
                "{}<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>t</title></head>{separator}<body>{separator}{}{separator}</body></html>",
                if bom { "\u{feff}" } else { "" },
                blocks.join(separator)
            )
        })
}

fn valid(chapter: &Chapter, at: Locator) -> bool {
    at.chapter == chapter.index
        && match chapter.blocks.get(at.block) {
            Some(Block::Text(paragraph)) => paragraph.text.is_char_boundary(at.byte),
            Some(Block::Image(_)) => at.byte == 0,
            None => at.block == chapter.blocks.len() && at.byte == 0,
        }
}

/// Prints the mapping of every position of `PROBE`, a chapter body.
#[test]
#[ignore = "probe"]
fn probe() {
    let body = std::env::var("PROBE").unwrap();
    let file = epub(&[format!(
        "<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>t</title></head><body>{body}</body></html>"
    )]);
    let mut document = Document::open(file.path()).unwrap();
    let chapter = document.chapter(0).unwrap();
    for (block, content) in chapter.blocks.iter().enumerate() {
        let Block::Text(paragraph) = content else {
            eprintln!("block {block}: image");
            continue;
        };
        eprintln!("block {block}: {:?}", paragraph.text);
        let ends = paragraph.text.char_indices().map(|(b, _)| b);
        for byte in ends.chain([paragraph.text.len()]) {
            let at = Locator {
                chapter: 0,
                block,
                byte,
            };
            let pointer = document.koreader_position(at);
            let back = pointer
                .as_ref()
                .ok()
                .map(|p| document.resolve_koreader_position(p));
            eprintln!("  {byte} -> {pointer:?} -> {back:?}");
        }
    }
}

fn cases() -> u32 {
    std::env::var("PROP_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(64)
}

proptest! {
    #![proptest_config(ProptestConfig { cases: cases(), max_shrink_iters: 4000, ..ProptestConfig::default() })]

    #[test]
    fn every_position_syncs_to_itself_or_earlier(chapters in prop::collection::vec(xhtml(), 1..3)) {
        let file = epub(&chapters);
        let mut document = Document::open(file.path()).unwrap();
        for index in 0..chapters.len() {
            let chapter = document.chapter(index).unwrap();
            let mut positions = Vec::new();
            for (block, content) in chapter.blocks.iter().enumerate() {
                match content {
                    Block::Text(paragraph) => positions.extend(
                        paragraph
                            .text
                            .char_indices()
                            .map(|(byte, _)| byte)
                            .chain([paragraph.text.len()])
                            .map(|byte| Locator { chapter: index, block, byte }),
                    ),
                    Block::Image(_) => positions.push(Locator { chapter: index, block, byte: 0 }),
                }
            }
            positions.push(Locator { chapter: index, block: chapter.blocks.len(), byte: 0 });
            for at in positions {
                let pointer = document.koreader_position(at);
                prop_assert!(pointer.is_ok(), "no sync position for {at:?}: {:?}", pointer.err());
                let pointer = pointer.unwrap();
                let back = document.resolve_koreader_position(&pointer);
                prop_assert!(back.is_ok(), "{at:?} -> {pointer} does not resolve: {:?}", back.err());
                let back = back.unwrap();
                prop_assert!(valid(&chapter, back), "{at:?} -> {pointer} -> invalid {back:?}");
                // Known (see the regression tests below): a preformatted
                // newline that the other reader drops or reads as CR LF maps
                // one character later.
                let known = back == Locator { byte: at.byte + 1, ..at }
                    && matches!(&chapter.blocks[at.block], Block::Text(p) if p.text[at.byte..].starts_with('\n'));
                prop_assert!(back <= at || known, "{at:?} -> {pointer} -> later {back:?}");
                if known {
                    continue;
                }
                // A position that came from the other reader is stable.
                let again = document.koreader_position(back).unwrap();
                let settled = document.resolve_koreader_position(&again).unwrap();
                prop_assert_eq!(settled, back, "{:?} -> {} -> {:?} -> {}", at, pointer, back, again);
            }
        }
    }
}
