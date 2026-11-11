use std::{
    cell::Cell,
    io::{Cursor, Read, Seek, SeekFrom},
    rc::Rc,
};

use epub_stream::{EpubBook, zip::StreamingZip};

struct MeasuredReader {
    source: Cursor<Vec<u8>>,
    bytes: Rc<Cell<usize>>,
}

impl Read for MeasuredReader {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        let count = self.source.read(output)?;
        self.bytes.set(self.bytes.get() + count);
        Ok(count)
    }
}

impl Seek for MeasuredReader {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.source.seek(position)
    }
}

fn archive(corrupt_second_chapter: bool) -> Vec<u8> {
    let mut bytes = include_bytes!("../../../tests/fixtures/sample.epub").to_vec();
    if corrupt_second_chapter {
        let zip = StreamingZip::new(Cursor::new(&bytes)).unwrap();
        let entry = zip.get_entry("EPUB/two.xhtml").unwrap();
        assert_eq!(entry.method, 8);
        let offset = entry.local_header_offset as usize;
        let name = u16::from_le_bytes(bytes[offset + 26..offset + 28].try_into().unwrap());
        let extra = u16::from_le_bytes(bytes[offset + 28..offset + 30].try_into().unwrap());
        let payload = offset + 30 + usize::from(name) + usize::from(extra);
        // DEFLATE block type 3 is invalid. No other resource uses this payload.
        bytes[payload] = 7;
    }
    let end = bytes.len() - 22;
    assert_eq!(&bytes[end..end + 4], b"PK\x05\x06");
    let directory = u32::from_le_bytes(bytes[end + 16..end + 20].try_into().unwrap());
    // ZIP permits a gap before its central directory. The local entry offsets
    // remain unchanged; only the directory offset moves past these unused bytes.
    let padding = 8 * 1024 * 1024;
    bytes.splice(
        directory as usize..directory as usize,
        std::iter::repeat_n(0, padding),
    );
    bytes[end + padding + 16..end + padding + 20]
        .copy_from_slice(&(directory + padding as u32).to_le_bytes());
    bytes
}

#[test]
fn opening_and_chapter_reads_seek_past_unrelated_archive_bytes() {
    for corrupt in [false, true] {
        let bytes = Rc::new(Cell::new(0));
        let source = MeasuredReader {
            source: Cursor::new(archive(corrupt)),
            bytes: bytes.clone(),
        };
        let mut book = EpubBook::from_reader(source).unwrap();
        assert_eq!(book.chapter_count(), 2);
        assert!(book.chapter_uncompressed_size(1).unwrap() > 0);
        let first = book.read_spine_item_bytes(0).unwrap();
        assert!(!first.is_empty());
        assert!(
            bytes.get() < 64 * 1024,
            "read {} bytes before chapter two",
            bytes.get()
        );
        let second = book.read_spine_item_bytes(1);
        if corrupt {
            assert!(second.is_err());
        } else {
            assert!(!second.unwrap().is_empty());
        }
        assert_eq!(book.read_spine_item_bytes(0).unwrap(), first);
        assert!(
            bytes.get() < 64 * 1024,
            "read {} bytes across chapter seeks",
            bytes.get()
        );
    }
}
