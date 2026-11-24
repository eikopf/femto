//! Discovery properties for the vendored archive reader, through the same
//! public API the reader uses: generated archives read back what was put in,
//! and archives that lie about themselves are refused within the limits.
use epub_stream::zip::{StreamingZip, ZipLimits};
use femto_reader::document::Document;
use proptest::prelude::*;
use std::io::Cursor;

// ------------------------------------------------------------ archive writer

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}
/// DEFLATE with the fixed Huffman code (RFC 1951 section 3.2.6): literals,
/// and for a run of one byte, matches of length 258 at distance 1.
struct Bits {
    out: Vec<u8>,
    bit: u8,
}
impl Bits {
    fn low_first(&mut self, value: u32, count: u8) {
        for i in 0..count {
            if self.bit == 0 {
                self.out.push(0);
            }
            *self.out.last_mut().unwrap() |= (((value >> i) & 1) as u8) << self.bit;
            self.bit = (self.bit + 1) % 8;
        }
    }
    /// Huffman codes are packed starting from their most significant bit.
    fn code(&mut self, value: u32, count: u8) {
        for i in (0..count).rev() {
            self.low_first((value >> i) & 1, 1);
        }
    }
    fn symbol(&mut self, symbol: u32) {
        match symbol {
            0..=143 => self.code(0x30 + symbol, 8),
            144..=255 => self.code(0x190 + symbol - 144, 9),
            256..=279 => self.code(symbol - 256, 7),
            _ => self.code(0xc0 + symbol - 280, 8),
        }
    }
}
fn deflate(data: &[u8]) -> Vec<u8> {
    let mut bits = Bits {
        out: Vec::new(),
        bit: 0,
    };
    bits.low_first(1, 1);
    bits.low_first(1, 2);
    let mut at = 0;
    while at < data.len() {
        bits.symbol(u32::from(data[at]));
        at += 1;
        while data.len() - at >= 258 && data[at..at + 258].iter().all(|b| *b == data[at - 1]) {
            bits.symbol(285);
            bits.code(0, 5);
            at += 258;
        }
    }
    bits.symbol(256);
    bits.out
}
/// DEFLATE without compression (RFC 1951 section 3.2.4), in blocks of a given size.
fn deflate_stored(data: &[u8], block: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let blocks: Vec<&[u8]> = data.chunks(block.clamp(1, 65_535)).collect();
    if blocks.is_empty() {
        return vec![1, 0, 0, 0xff, 0xff];
    }
    for (index, part) in blocks.iter().enumerate() {
        out.push(u8::from(index + 1 == blocks.len()));
        out.extend((part.len() as u16).to_le_bytes());
        out.extend((!(part.len() as u16)).to_le_bytes());
        out.extend(*part);
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Method {
    Stored,
    Deflated,
    DeflatedBlocks(usize),
}
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    data: Vec<u8>,
    method: Method,
    local_extra: usize,
    central_extra: usize,
    comment: usize,
    /// A zero checksum is how a writer says it did not compute one.
    checksum: bool,
}
/// Where the fields of one entry lie in the archive, for targeted damage.
#[derive(Debug, Clone, Copy, Default)]
struct Offsets {
    local: usize,
    central: usize,
}
struct Archive {
    bytes: Vec<u8>,
    entries: Vec<Offsets>,
    end: usize,
}
fn archive(entries: &[Entry], comment: usize, prefix: usize) -> Archive {
    let mut bytes = vec![0x5a; prefix];
    let mut offsets = Vec::new();
    let mut central = Vec::new();
    for entry in entries {
        let packed = match entry.method {
            Method::Stored => entry.data.clone(),
            Method::Deflated => deflate(&entry.data),
            Method::DeflatedBlocks(block) => deflate_stored(&entry.data, block),
        };
        let method: u16 = if entry.method == Method::Stored { 0 } else { 8 };
        let crc = if entry.checksum {
            crc32(&entry.data)
        } else {
            0
        };
        let local = bytes.len();
        let mut fixed = Vec::new();
        fixed.extend(20u16.to_le_bytes());
        fixed.extend(0x0800u16.to_le_bytes());
        fixed.extend(method.to_le_bytes());
        fixed.extend([0; 4]);
        fixed.extend(crc.to_le_bytes());
        fixed.extend((packed.len() as u32).to_le_bytes());
        fixed.extend((entry.data.len() as u32).to_le_bytes());
        fixed.extend((entry.name.len() as u16).to_le_bytes());
        bytes.extend(0x0403_4b50u32.to_le_bytes());
        bytes.extend(&fixed);
        bytes.extend(extra_len(entry.local_extra).to_le_bytes());
        bytes.extend(entry.name.bytes());
        bytes.extend(extra(entry.local_extra));
        bytes.extend(&packed);
        offsets.push(Offsets {
            local,
            central: central.len(),
        });
        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend(0x031eu16.to_le_bytes());
        central.extend(&fixed);
        central.extend(extra_len(entry.central_extra).to_le_bytes());
        central.extend((entry.comment as u16).to_le_bytes());
        central.extend([0; 8]);
        central.extend((local as u32).to_le_bytes());
        central.extend(entry.name.bytes());
        central.extend(extra(entry.central_extra));
        central.extend(std::iter::repeat_n(b'c', entry.comment));
    }
    let start = bytes.len();
    for offset in &mut offsets {
        offset.central += start;
    }
    bytes.extend(&central);
    let end = bytes.len();
    bytes.extend(0x0605_4b50u32.to_le_bytes());
    bytes.extend([0; 4]);
    bytes.extend((entries.len() as u16).to_le_bytes());
    bytes.extend((entries.len() as u16).to_le_bytes());
    bytes.extend((central.len() as u32).to_le_bytes());
    bytes.extend((start as u32).to_le_bytes());
    bytes.extend((comment as u16).to_le_bytes());
    bytes.extend(std::iter::repeat_n(b'#', comment));
    Archive {
        bytes,
        entries: offsets,
        end,
    }
}
/// An extra field of an unknown kind, as archivers add for timestamps.
fn extra(len: usize) -> Vec<u8> {
    if len == 0 {
        return Vec::new();
    }
    let mut out = vec![0x55, 0x54];
    out.extend((len as u16).to_le_bytes());
    out.extend(std::iter::repeat_n(7, len));
    out
}
fn extra_len(len: usize) -> u16 {
    if len == 0 { 0 } else { len as u16 + 4 }
}

fn data() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        3 => prop::collection::vec(any::<u8>(), 0..200),
        1 => (0usize..3000, any::<u8>()).prop_map(|(len, byte)| vec![byte; len]),
        1 => (8180usize..8200, any::<u8>()).prop_map(|(len, seed)| (0..len).map(|i| (i as u8).wrapping_mul(seed | 1)).collect()),
    ]
}
fn entry() -> impl Strategy<Value = Entry> {
    (
        // A small alphabet, so that names differing only in case occur.
        prop_oneof![3 => "[abAB]{1,2}(/[abAB])?(\\.[xX])?", 1 => "[a-z]{1,8}/[a-zé書]{1,8}\\.xhtml", 1 => "/?[ab]"],
        data(),
        prop_oneof![
            Just(Method::Stored),
            Just(Method::Deflated),
            (1usize..9000).prop_map(Method::DeflatedBlocks)
        ],
        prop_oneof![Just(0usize), 1usize..30],
        prop_oneof![Just(0usize), 1usize..30],
        prop_oneof![Just(0usize), 1usize..30],
        prop::bool::weighted(0.8),
    )
        .prop_map(|(name, data, method, local_extra, central_extra, comment, checksum)| Entry {
            name,
            data,
            method,
            local_extra,
            central_extra,
            comment,
            checksum,
        })
}
fn entries() -> impl Strategy<Value = Vec<Entry>> {
    prop::collection::vec(entry(), 0..8).prop_map(|mut entries| {
        let mut seen = std::collections::HashSet::new();
        entries.retain(|entry| seen.insert(entry.name.clone()));
        entries
    })
}
const LIMIT: usize = 16 * 1024;
fn open(bytes: Vec<u8>) -> Result<StreamingZip<Cursor<Vec<u8>>>, epub_stream::zip::ZipError> {
    StreamingZip::new_with_limits(Cursor::new(bytes), Some(ZipLimits::new(LIMIT, 64)))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(800))]

    /// Every entry of a generated archive is listed and reads back whole, by
    /// both read paths, whatever surrounds it.
    #[test]
    fn archives_read_back(entries in entries(), comment in prop_oneof![Just(0usize), 1usize..5000], prefix in prop_oneof![Just(0usize), 1usize..3000]) {
        let mut zip = open(archive(&entries, comment, prefix).bytes).unwrap();
        prop_assert_eq!(zip.num_entries(), entries.len());
        let listed: Vec<_> = zip.entries().cloned().collect();
        for (listed, entry) in listed.iter().zip(&entries) {
            prop_assert_eq!(&listed.filename, &entry.name);
            prop_assert_eq!(listed.uncompressed_size, entry.data.len() as u64);
            // A byte to spare: see `a_buffer_of_the_exact_size_is_enough`.
            let mut buffer = vec![0xaa; entry.data.len() + 1];
            let count = zip.read_file(listed, &mut buffer).unwrap();
            prop_assert_eq!(&buffer[..count], &entry.data[..]);
            let mut written = Vec::new();
            prop_assert_eq!(zip.read_file_to_writer(listed, &mut written).unwrap(), entry.data.len());
            prop_assert_eq!(&written, &entry.data);
            // One byte too few is refused and not truncated.
            if !entry.data.is_empty() {
                let mut short = vec![0; entry.data.len() - 1];
                prop_assert!(zip.read_file(listed, &mut short).is_err());
            }
        }
    }

    /// A name finds the entry of that name when there is one. Names that
    /// differ only in case are different files (EPUB OCF 3.3 section 4.2.2).
    #[test]
    #[ignore = "bug: get_entry returns an earlier entry whose name differs only in case or by a leading slash"]
    fn a_name_finds_its_own_entry(entries in entries()) {
        let zip = open(archive(&entries, 0, 0).bytes).unwrap();
        for entry in &entries {
            let found = zip.get_entry(&entry.name).unwrap();
            prop_assert_eq!(&found.filename, &entry.name);
        }
    }

    /// An archive whose directory lies is refused or read within the limits:
    /// nothing read is larger than the limit or than the entry said it was.
    #[test]
    fn lying_archives_are_refused_or_read_within_limits(
        entries in entries().prop_filter("entries", |e| !e.is_empty()),
        which in any::<prop::sample::Index>(),
        // Field offsets within the central record, the local header and the
        // end record, with their widths.
        field in prop::sample::select(vec![
            ('c', 10usize, 2usize), ('c', 16, 4), ('c', 20, 4), ('c', 24, 4), ('c', 28, 2), ('c', 30, 2), ('c', 32, 2), ('c', 42, 4), ('c', 0, 4),
            ('l', 0, 4), ('l', 8, 2), ('l', 18, 4), ('l', 22, 4), ('l', 26, 2), ('l', 28, 2),
            ('e', 8, 2), ('e', 10, 2), ('e', 12, 4), ('e', 16, 4), ('e', 20, 2),
        ]),
        value in prop_oneof![Just(0u32), Just(1), Just(u32::MAX), Just(u32::MAX - 1), Just(0xffff), Just(0x7fff_ffff), 0u32..40_000, any::<u32>()],
        relative in any::<bool>(),
    ) {
        let built = archive(&entries, 0, 0);
        let target = built.entries[which.index(entries.len())];
        let at = match field.0 { 'c' => target.central, 'l' => target.local, _ => built.end } + field.1;
        let mut bytes = built.bytes;
        let mut old = [0; 4];
        old[..field.2].copy_from_slice(&bytes[at..at + field.2]);
        let value = if relative { u32::from_le_bytes(old).wrapping_add(value % 9).wrapping_sub(4) } else { value };
        bytes[at..at + field.2].copy_from_slice(&value.to_le_bytes()[..field.2]);
        let Ok(mut zip) = open(bytes) else { return Ok(()) };
        for listed in zip.entries().cloned().collect::<Vec<_>>() {
            let mut buffer = vec![0; (listed.uncompressed_size as usize).min(LIMIT)];
            if let Ok(count) = zip.read_file(&listed, &mut buffer) {
                prop_assert!(count <= buffer.len());
            }
            let mut written = Vec::new();
            if let Ok(count) = zip.read_file_to_writer(&listed, &mut written) {
                prop_assert_eq!(count, written.len());
                // Known: a deflated entry may inflate past its stated size;
                // see `an_entry_that_inflates_past_its_stated_size_is_refused`.
                if listed.method != 8 {
                    prop_assert!(written.len() <= LIMIT, "{} bytes read with a limit of {}", written.len(), LIMIT);
                }
            }
        }
    }

    /// Truncation and stray bytes never panic.
    #[test]
    fn damaged_archives_never_panic(
        entries in entries(), comment in 0usize..40,
        cut in any::<prop::sample::Index>(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..4),
        tail in any::<bool>(),
    ) {
        let mut bytes = archive(&entries, comment, 0).bytes;
        for (index, byte) in edits {
            let at = index.index(bytes.len());
            bytes[at] = byte;
        }
        if tail {
            // Keep the end record so that parsing reaches the damaged part.
            let keep = bytes.split_off(bytes.len() - 22 - comment);
            bytes.truncate(cut.index(bytes.len() + 1));
            bytes.extend(keep);
        } else {
            bytes.truncate(cut.index(bytes.len() + 1));
        }
        let Ok(mut zip) = open(bytes) else { return Ok(()) };
        for listed in zip.entries().cloned().collect::<Vec<_>>() {
            let mut buffer = vec![0; (listed.uncompressed_size as usize).min(LIMIT)];
            let _ = zip.read_file(&listed, &mut buffer);
            let _ = zip.read_file_to_writer(&listed, &mut std::io::sink());
        }
        let _ = zip.validate_mimetype();
    }

    #[test]
    fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..300), record in prop::collection::vec(any::<u8>(), 18)) {
        let _ = open(bytes.clone()).map(|zip| zip.num_entries());
        // With an end record, so the directory it points at is parsed.
        let mut ended = bytes;
        ended.extend(0x0605_4b50u32.to_le_bytes());
        ended.extend(&record[..16]);
        ended.extend([0, 0]);
        if let Ok(mut zip) = open(ended) {
            for listed in zip.entries().cloned().collect::<Vec<_>>() {
                let _ = zip.read_file_to_writer(&listed, &mut std::io::sink());
            }
        }
    }
}

// ------------------------------------------------------------------- findings

fn stored(name: &str, data: &[u8]) -> Entry {
    Entry {
        name: name.into(),
        data: data.to_vec(),
        method: Method::Stored,
        local_extra: 0,
        central_extra: 0,
        comment: 0,
        checksum: true,
    }
}

#[test]
#[ignore = "bug: get_entry returns an earlier entry whose name differs only in case"]
fn names_that_differ_in_case_are_different_files() {
    let entries = [
        stored("OEBPS/Chapter.xhtml", b"upper"),
        stored("OEBPS/chapter.xhtml", b"lower"),
    ];
    let mut zip = open(archive(&entries, 0, 0).bytes).unwrap();
    let found = zip.get_entry("OEBPS/chapter.xhtml").unwrap().clone();
    let mut read = Vec::new();
    zip.read_file_to_writer(&found, &mut read).unwrap();
    assert_eq!(read, b"lower");
}

/// An archive whose one resource says it is 16 bytes and inflates to 8 MiB.
fn inflating(stated: u32, actual: usize, checksum: bool) -> (Vec<Entry>, Vec<u8>) {
    let mut entries = vec![
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
            br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml"><head><title>c</title></head><body><p>Text.</p><img src="big.png" alt="big"/></body></html>"#,
        ),
    ];
    entries.push(Entry {
        method: Method::Deflated,
        checksum,
        ..stored("OEBPS/big.png", &vec![0; actual])
    });
    let built = archive(&entries, 0, 0);
    let mut bytes = built.bytes;
    let (central, local) = (built.entries[4].central, built.entries[4].local);
    bytes[central + 24..central + 28].copy_from_slice(&stated.to_le_bytes());
    bytes[local + 22..local + 26].copy_from_slice(&stated.to_le_bytes());
    (entries, bytes)
}

/// `ZipLimits::max_file_read_size` is the "maximum compressed or uncompressed
/// file size allowed for reads". The size checked is the one the directory
/// states; what the entry inflates to is not counted.
#[test]
#[ignore = "bug: a deflated entry is inflated past its stated size and past the read limit"]
fn an_entry_that_inflates_past_its_stated_size_is_refused() {
    for checksum in [true, false] {
        let (_, bytes) = inflating(16, 8 * LIMIT, checksum);
        assert!(bytes.len() < LIMIT);
        let mut zip = open(bytes).unwrap();
        let entry = zip.get_entry("OEBPS/big.png").unwrap().clone();
        assert_eq!(entry.uncompressed_size, 16);
        let mut written = Vec::new();
        let result = zip.read_file_to_writer(&entry, &mut written);
        assert!(
            written.len() <= LIMIT,
            "{} bytes written with a limit of {LIMIT}: {result:?}",
            written.len()
        );
        assert!(result.is_err());
    }
}

/// `Document::resource` "reads an archive resource with an explicit size cap"
/// of `image::MAX_ENCODED_BYTES`.
#[test]
#[ignore = "bug: a resource larger than the cap is returned whole when the archive understates its size"]
fn a_resource_is_never_larger_than_the_cap() {
    let cap = femto_reader::image::MAX_ENCODED_BYTES;
    let (_, bytes) = inflating(16, 4 * cap, false);
    assert!(
        bytes.len() < 64 * 1024,
        "the archive itself is {} bytes",
        bytes.len()
    );
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("book.epub");
    std::fs::write(&path, bytes).unwrap();
    let mut document = Document::open(&path).unwrap();
    assert_eq!(document.document_count(), 1);
    match document.resource("big.png") {
        Ok(resource) => assert!(
            resource.len() <= cap,
            "{} bytes returned with a cap of {cap}",
            resource.len()
        ),
        Err(_) => {}
    }
}

/// The honest form of the same book opens and reads, so the tests above fail
/// for the size and for nothing else.
#[test]
fn the_same_book_with_true_sizes_is_read() {
    let (entries, _) = inflating(16, 4096, true);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("book.epub");
    std::fs::write(&path, archive(&entries, 0, 0).bytes).unwrap();
    let mut document = Document::open(&path).unwrap();
    assert_eq!(document.document_count(), 1);
    assert_eq!(document.resource("big.png").unwrap(), vec![0; 4096]);
    assert!(document.chapter(0).is_ok());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(800))]

    /// A buffer of exactly the entry's size is large enough.
    #[test]
    #[ignore = "bug: read_file refuses a buffer of exactly the entry's size when compressed bytes remain once it is full"]
    fn a_buffer_of_the_exact_size_is_enough(entries in entries()) {
        let mut zip = open(archive(&entries, 0, 0).bytes).unwrap();
        for (listed, entry) in zip.entries().cloned().collect::<Vec<_>>().iter().zip(&entries) {
            let mut buffer = vec![0xaa; entry.data.len()];
            prop_assert_eq!(zip.read_file(listed, &mut buffer).map_err(|e| format!("{e:?}")), Ok(entry.data.len()));
            prop_assert_eq!(&buffer, &entry.data);
        }
    }
}

/// An empty file written with DEFLATE, as Python's `zipfile` and others
/// write it, and a file whose end-of-stream code falls in the read after
/// the one that produced its last byte.
#[test]
#[ignore = "bug: read_file refuses a buffer of exactly the entry's size when compressed bytes remain once it is full"]
fn deflated_entries_read_into_a_buffer_of_their_size() {
    // With the fixed code, 8191 literals end inside the 8192nd compressed
    // byte, the last of the first read; the end code spills into the next.
    let cases = [
        vec![b'a'; 100],
        Vec::new(),
        (0..8191).map(|i| b'a' + (i % 26) as u8).collect(),
    ];
    let mut failed = Vec::new();
    for data in cases {
        let entry = Entry {
            method: Method::Deflated,
            ..stored("OEBPS/c.xhtml", &data)
        };
        let mut zip = open(archive(&[entry], 0, 0).bytes).unwrap();
        let listed = zip.get_entry("OEBPS/c.xhtml").unwrap().clone();
        let mut written = Vec::new();
        assert_eq!(
            zip.read_file_to_writer(&listed, &mut written),
            Ok(data.len())
        );
        assert_eq!(written, data);
        let mut buffer = vec![0; data.len()];
        match zip.read_file(&listed, &mut buffer) {
            Ok(count) => assert_eq!(&buffer[..count], &data[..]),
            Err(error) => failed.push(format!("{} bytes: {error:?}", data.len())),
        }
    }
    assert!(failed.is_empty(), "{failed:?}");
}
