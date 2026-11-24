//! The archive reader against archives written here byte by byte, so that
//! they can say what no archiver would write.
use std::io::Cursor;

use epub_stream::zip::StreamingZip;

/// One file of an archive, as the archive's headers describe it.
struct Entry<'a> {
    name: &'a str,
    /// 0 for stored, 8 for DEFLATE.
    method: u16,
    packed: Vec<u8>,
    /// The size the headers state for the file once unpacked.
    size: usize,
}

fn stored<'a>(name: &'a str, data: &[u8]) -> Entry<'a> {
    Entry {
        name,
        method: 0,
        packed: data.to_vec(),
        size: data.len(),
    }
}

/// An archive without checksums: zero is how a writer says it computed none.
fn archive(entries: &[Entry]) -> Vec<u8> {
    let (mut bytes, mut directory) = (Vec::new(), Vec::new());
    for entry in entries {
        // The fields a local header and a directory record share.
        let mut fields = Vec::new();
        fields.extend(20u16.to_le_bytes());
        fields.extend(0x0800u16.to_le_bytes());
        fields.extend(entry.method.to_le_bytes());
        fields.extend([0; 8]);
        fields.extend((entry.packed.len() as u32).to_le_bytes());
        fields.extend((entry.size as u32).to_le_bytes());
        fields.extend((entry.name.len() as u16).to_le_bytes());
        let local = bytes.len() as u32;
        bytes.extend(b"PK\x03\x04");
        bytes.extend(&fields);
        bytes.extend([0; 2]);
        bytes.extend(entry.name.bytes());
        bytes.extend(&entry.packed);
        directory.extend(b"PK\x01\x02");
        directory.extend(20u16.to_le_bytes());
        directory.extend(&fields);
        directory.extend([0; 12]);
        directory.extend(local.to_le_bytes());
        directory.extend(entry.name.bytes());
    }
    let start = bytes.len() as u32;
    bytes.extend(&directory);
    bytes.extend(b"PK\x05\x06");
    bytes.extend([0; 4]);
    bytes.extend((entries.len() as u16).to_le_bytes());
    bytes.extend((entries.len() as u16).to_le_bytes());
    bytes.extend((directory.len() as u32).to_le_bytes());
    bytes.extend(start.to_le_bytes());
    bytes.extend([0; 2]);
    bytes
}

fn open(entries: &[Entry]) -> StreamingZip<Cursor<Vec<u8>>> {
    StreamingZip::new(Cursor::new(archive(entries))).unwrap()
}

/// DEFLATE of a run of zero bytes with the fixed code (RFC 1951, section
/// 3.2.6): one literal, then matches of 258 bytes at distance one, which
/// take 13 bits each.
fn deflated_zeros(len: usize) -> Vec<u8> {
    assert_eq!(len % 258, 1);
    // A final block of the fixed code, then the literal 0.
    let mut bits = vec![1, 1, 0, 0, 0, 1, 1, 0, 0, 0, 0];
    for _ in 0..len / 258 {
        // Length code 285 and distance code 0.
        bits.extend([1, 1, 0, 0, 0, 1, 0, 1, 0, 0, 0, 0, 0]);
    }
    // The end of the block.
    bits.extend([0; 7]);
    bits.chunks(8)
        .map(|byte| byte.iter().rev().fold(0, |byte, bit| byte << 1 | bit))
        .collect()
}

fn zeros(name: &str, len: usize, stated: usize) -> Entry<'_> {
    Entry {
        name,
        method: 8,
        packed: deflated_zeros(len),
        size: stated,
    }
}

/// An entry is as large as the archive's directory states, which is the
/// size every limit is checked against. One that unpacks to more is refused
/// before more than that is written, and one that unpacks to less is refused
/// too, by both ways of reading.
#[test]
fn an_entry_that_is_not_the_size_it_states_is_refused() {
    const LARGE: usize = 1 + 258 * 4096;
    let entries = [
        zeros("true", LARGE, LARGE),
        zeros("more", LARGE, 16),
        zeros("one more", 259, 258),
        zeros("less", 259, 260),
        zeros("none", 1, 0),
        Entry {
            size: 4,
            ..stored("stored more", b"sixteen bytes...")
        },
        Entry {
            size: 16,
            ..stored("stored less", b"four")
        },
    ];
    assert!(entries[1].packed.len() < 8 * 1024);
    let mut zip = open(&entries);
    for entry in &entries {
        let listed = zip.get_entry(entry.name).unwrap().clone();
        let mut written = Vec::new();
        let streamed = zip.read_file_to_writer(&listed, &mut written);
        assert!(
            written.len() <= entry.size,
            "{}: {} bytes written of an entry that states {}",
            entry.name,
            written.len(),
            entry.size
        );
        // Room to spare, so that only the stated size can be what stops it.
        let mut buffer = vec![0xaa; entry.size + 1024];
        let read = zip.read_file(&listed, &mut buffer);
        if entry.name == "true" {
            assert_eq!(streamed, Ok(LARGE));
            assert_eq!(read, Ok(LARGE));
            assert_eq!(written, vec![0; LARGE]);
            assert_eq!(&buffer[..LARGE], &written[..]);
        } else {
            assert!(streamed.is_err(), "{}: {streamed:?}", entry.name);
            assert!(read.is_err(), "{}: {read:?}", entry.name);
            assert!(
                buffer[entry.size..].iter().all(|byte| *byte == 0xaa),
                "{}: read past the size it states",
                entry.name
            );
        }
    }
}

/// `Document::resource` reads with a cap of `image::MAX_ENCODED_BYTES`. A
/// book of a few kilobytes cannot make it return more by understating what
/// an entry unpacks to.
#[test]
fn a_resource_is_never_larger_than_the_cap() {
    const CAP: usize = femto_reader::image::MAX_ENCODED_BYTES;
    let book = |image: Entry| {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("book.epub");
        let bytes = archive(&[
            stored("mimetype", b"application/epub+zip"),
            stored(
                "META-INF/container.xml",
                br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#,
            ),
            stored(
                "OEBPS/content.opf",
                br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id"><metadata xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:identifier id="id">urn:x</dc:identifier><dc:title>Book</dc:title><dc:language>en</dc:language></metadata><manifest><item id="c" href="c.xhtml" media-type="application/xhtml+xml"/><item id="i" href="big.png" media-type="image/png"/></manifest><spine><itemref idref="c"/></spine></package>"#,
            ),
            stored(
                "OEBPS/c.xhtml",
                br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>c</title></head><body><p>Text.</p></body></html>"#,
            ),
            image,
        ]);
        assert!(bytes.len() < 64 * 1024, "the book is {} bytes", bytes.len());
        std::fs::write(&path, bytes).unwrap();
        femto_reader::document::Document::open(&path).unwrap()
    };
    const LARGE: usize = 1 + 258 * (4 * CAP / 258);
    assert!(
        book(zeros("OEBPS/big.png", LARGE, 16))
            .resource("big.png")
            .is_err()
    );
    // An entry within the cap that is the size it states is read.
    const SMALL: usize = 1 + 258 * 4096;
    assert_eq!(
        book(zeros("OEBPS/big.png", SMALL, SMALL))
            .resource("big.png")
            .unwrap(),
        vec![0; SMALL]
    );
}

/// Names that differ only in case are different files (EPUB OCF 3.3,
/// section 4.2.2), and so are a name and the same name after a slash.
#[test]
fn a_name_finds_the_entry_of_exactly_that_name() {
    let entries = [
        stored("OEBPS/Chapter.xhtml", b"upper"),
        stored("OEBPS/chapter.xhtml", b"lower"),
        stored("/a", b"rooted"),
        stored("a", b"relative"),
        stored("mimetype", b"application/epub+zip"),
    ];
    let mut zip = open(&entries);
    for entry in &entries {
        let found = zip.get_entry(entry.name).unwrap().clone();
        assert_eq!(found.filename, entry.name);
        let mut read = Vec::new();
        zip.read_file_to_writer(&found, &mut read).unwrap();
        assert_eq!(read, entry.packed, "{}", entry.name);
    }
    // A name no entry has exactly still finds one that differs in case or
    // by a leading slash: the first such in the archive.
    for (name, found) in [
        ("oebps/CHAPTER.xhtml", "OEBPS/Chapter.xhtml"),
        ("/A", "/a"),
        ("A", "/a"),
        ("/mimetype", "mimetype"),
        ("MIMETYPE", "mimetype"),
    ] {
        assert_eq!(zip.get_entry(name).unwrap().filename, found, "{name}");
    }
    assert!(zip.get_entry("b").is_none());
}
