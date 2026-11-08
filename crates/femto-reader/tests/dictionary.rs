use femto_reader::dictionary::Dictionary;
use std::{fs, path::Path};

fn fixture(dir: &Path, definition: &[u8], synonyms: bool) {
    let mut idx = Vec::new();
    for word in ["Alpha", "beta", "café"] {
        idx.extend_from_slice(word.as_bytes());
        idx.push(0);
        idx.extend_from_slice(&0u32.to_be_bytes());
        idx.extend_from_slice(&(definition.len() as u32).to_be_bytes());
    }
    fs::write(dir.join("test.idx"), &idx).unwrap();
    fs::write(dir.join("test.dict"), definition).unwrap();
    fs::write(dir.join("test.ifo"), format!("StarDict's dict ifo file\nversion=2.4.2\nbookname=Example\nwordcount=3\nsynwordcount={}\nidxfilesize={}\nsametypesequence=h\n", u8::from(synonyms),idx.len())).unwrap();
    if synonyms {
        fs::write(dir.join("test.syn"), b"alias\0\0\0\0\x01").unwrap();
    }
}

fn ready(path: &Path) -> Dictionary {
    let mut dictionary = Dictionary::open(path).unwrap();
    assert!(!dictionary.is_ready());
    assert!(dictionary.lookup("alpha").is_err());
    for _ in 0..10 {
        if dictionary.prepare_step().unwrap() {
            return dictionary;
        }
    }
    panic!("fixture indexing did not finish");
}

#[test]
fn offline_lookup_prepares_and_resolves_case_synonyms_and_markup() {
    let dir = tempfile::tempdir().unwrap();
    fixture(
        dir.path(),
        b"<p>A &amp; B</p><p>Second definition.</p>",
        true,
    );
    let dictionary = ready(&dir.path().join("test.ifo"));
    assert_eq!(dictionary.name(), "Example");
    let alpha = dictionary.lookup("ALPHA").unwrap();
    assert_eq!(alpha[0].word, "Alpha");
    assert!(alpha[0].text.contains("A & B"));
    assert!(alpha[0].text.contains("Second definition."));
    assert_eq!(dictionary.lookup("alias").unwrap()[0].word, "beta");
    assert_eq!(dictionary.lookup("CAFÉ").unwrap()[0].word, "café");
    assert!(dictionary.lookup("missing").unwrap().is_empty());
    assert!(dictionary.lookup(&"a".repeat(256)).is_err());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 4);
}

#[test]
fn missing_declared_synonyms_and_oversized_definitions_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), b"definition", true);
    fs::remove_file(dir.path().join("test.syn")).unwrap();
    assert!(Dictionary::open(dir.path().join("test.ifo")).is_err());
    fixture(dir.path(), &vec![b'x'; 65537], false);
    let dictionary = ready(&dir.path().join("test.ifo"));
    assert!(
        dictionary
            .lookup("alpha")
            .unwrap_err()
            .to_string()
            .contains("definition too large")
    );
}

#[test]
fn truncated_definition_is_not_returned_as_successful_partial_text() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), b"complete definition", false);
    let dictionary = ready(&dir.path().join("test.ifo"));
    fs::write(dir.path().join("test.dict"), b"short").unwrap();
    assert!(dictionary.lookup("alpha").is_err());
}

#[test]
fn index_caches_are_private_and_rebuilt_after_same_size_replacement() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), b"old", false);
    let path = dir.path().join("test.ifo");
    let first = ready(&path);
    assert_eq!(first.lookup("alpha").unwrap()[0].text, "old");
    drop(first);
    fixture(dir.path(), b"new", false);
    let second = ready(&path);
    assert_eq!(second.lookup("alpha").unwrap()[0].text, "new");
    let mut bytes = fs::read(dir.path().join("test.idx")).unwrap();
    bytes.truncate(7);
    fs::write(dir.path().join("test.idx"), bytes).unwrap();
    assert!(Dictionary::open(path).is_err());
}

fn compressed_fixture(dir: &Path) {
    for extension in ["ifo", "idx", "dict.dz"] {
        fs::copy(
            Path::new(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/dictionary"
            ))
            .join(format!("compressed.{extension}")),
            dir.join(format!("compressed.{extension}")),
        )
        .unwrap();
    }
}

#[test]
fn dictzip_reads_across_flush_chunks_and_the_inflate_window() {
    let dir = tempfile::tempdir().unwrap();
    compressed_fixture(dir.path());
    let dictionary = ready(&dir.path().join("compressed.ifo"));
    assert_eq!(
        dictionary.lookup("alpha").unwrap()[0].text,
        "naïve ".repeat(4000).trim()
    );
    assert_eq!(
        dictionary.lookup("beta").unwrap()[0].text,
        "b".repeat(60000)
    );
    assert_eq!(dictionary.lookup("CAFÉ").unwrap()[0].text, "A small café.");
}

#[test]
fn dictzip_rejects_index_ranges_outside_uncompressed_data() {
    for (offset, size) in [(88000u32, 1000u32), (999999, 20)] {
        let dir = tempfile::tempdir().unwrap();
        compressed_fixture(dir.path());
        let path = dir.path().join("compressed.idx");
        let mut idx = fs::read(&path).unwrap();
        idx[6..10].copy_from_slice(&offset.to_be_bytes());
        idx[10..14].copy_from_slice(&size.to_be_bytes());
        fs::write(path, idx).unwrap();
        let dictionary = ready(&dir.path().join("compressed.ifo"));
        assert!(dictionary.lookup("alpha").is_err());
    }
}

#[test]
fn dictzip_rejects_bad_tables_headers_and_chunk_lengths() {
    for case in 0..9 {
        let dir = tempfile::tempdir().unwrap();
        compressed_fixture(dir.path());
        let path = dir.path().join("compressed.dict.dz");
        let mut data = fs::read(&path).unwrap();
        match case {
            0 => data[16..18].copy_from_slice(&2u16.to_le_bytes()),
            1 => data[18..20].copy_from_slice(&0u16.to_le_bytes()),
            2 => data[20..22].copy_from_slice(&0u16.to_le_bytes()),
            3 => data[22..24].copy_from_slice(&0u16.to_le_bytes()),
            4 => {
                data.truncate(data.len() - 8);
            }
            5 => data[18..20].copy_from_slice(&39999u16.to_le_bytes()),
            6 => data[18..20].copy_from_slice(&40001u16.to_le_bytes()),
            7 => data[3] |= 0x80,
            8 => {
                let name = 12 + u16::from_le_bytes([data[10], data[11]]) as usize;
                data.splice(name..name, vec![b'x'; 17000]);
            }
            _ => unreachable!(),
        }
        fs::write(path, data).unwrap();
        let result = Dictionary::open(dir.path().join("compressed.ifo")).and_then(|mut d| {
            while !d.prepare_step()? {}
            d.lookup("alpha")
        });
        assert!(result.is_err(), "mutation {case} accepted");
    }
}

#[test]
fn dictzip_rejects_overflowing_64_bit_definition_offset() {
    let dir = tempfile::tempdir().unwrap();
    compressed_fixture(dir.path());
    let mut idx = b"alpha\0".to_vec();
    idx.extend_from_slice(&u64::MAX.to_be_bytes());
    idx.extend_from_slice(&20u32.to_be_bytes());
    fs::write(dir.path().join("compressed.idx"), &idx).unwrap();
    fs::write(dir.path().join("compressed.ifo"), format!("StarDict's dict ifo file\nversion=3.0.0\nbookname=Overflow\nwordcount=1\nidxfilesize={}\nidxoffsetbits=64\nsametypesequence=m\n", idx.len())).unwrap();
    let dictionary = ready(&dir.path().join("compressed.ifo"));
    assert!(
        dictionary
            .lookup("alpha")
            .unwrap_err()
            .to_string()
            .contains("overflow")
    );
}

#[test]
fn metadata_rejects_invalid_required_fields_and_encoding() {
    let mutations = [
        ("version=2.4.2", "version=9.0.0"),
        ("version=2.4.2\n", ""),
        ("bookname=Example", "bookname="),
        ("wordcount=3", "wordcount=3\nwordcount=2"),
        ("wordcount=3", "wordcount=4294967296"),
        ("wordcount=3", "wordcount=+3"),
        ("synwordcount=0", "synwordcount=invalid"),
        ("sametypesequence=h", "sametypesequence=h1"),
        ("sametypesequence=h", "sametypesequence="),
        (
            "sametypesequence=h",
            "idxoffsetbits=128\nsametypesequence=h",
        ),
        ("sametypesequence=h", "idxoffsetbits=64\nsametypesequence=h"),
        ("StarDict's dict ifo file", "StarDict's dict ifo file extra"),
        ("bookname=Example", "broken line"),
        ("bookname=Example", "bookname=Example\nidxfilesize=1"),
    ];
    for (from, to) in mutations {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), b"definition", false);
        let path = dir.path().join("test.ifo");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(from));
        fs::write(&path, text.replace(from, to)).unwrap();
        assert!(Dictionary::open(path).is_err(), "accepted {to}");
    }
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path(), b"definition", false);
    let path = dir.path().join("test.ifo");
    let mut text = fs::read(&path).unwrap();
    text.push(0xff);
    fs::write(&path, text).unwrap();
    assert!(Dictionary::open(path).is_err());
}

#[test]
fn preparation_rejects_inexact_entry_counts_and_unannounced_synonyms() {
    for field in ["wordcount=3", "synwordcount=1"] {
        for count in ["0", "2", "4"] {
            let dir = tempfile::tempdir().unwrap();
            fixture(dir.path(), b"definition", true);
            let path = dir.path().join("test.ifo");
            let text = fs::read_to_string(&path).unwrap();
            fs::write(
                &path,
                text.replace(
                    field,
                    &format!("{}={count}", field.split('=').next().unwrap()),
                ),
            )
            .unwrap();
            let result = Dictionary::open(path).and_then(|mut dictionary| {
                for _ in 0..16 {
                    if dictionary.prepare_step()? {
                        return Ok(());
                    }
                }
                panic!("small invalid index did not terminate");
            });
            assert!(result.is_err(), "accepted {field} -> {count}");
        }
    }
}

#[test]
fn index_preparation_validates_words_order_and_synonym_targets() {
    for case in 0..7 {
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path(), b"definition", true);
        let path = dir.path().join(if matches!(case, 3 | 5 | 6) {
            "test.syn"
        } else {
            "test.idx"
        });
        let mut bytes = fs::read(&path).unwrap();
        match case {
            0 => bytes[0] = b'z', // z lpha precedes beta in the file.
            1 => bytes[0] = 0xff,
            2 => bytes[0] = 0, // Empty word.
            3 => {
                let end = bytes.len();
                bytes[end - 4..].copy_from_slice(&3u32.to_be_bytes());
            }
            4 => {
                bytes.splice(..5, vec![b'a'; 256]);
            }
            5 => bytes[0] = 0xff,
            6 => bytes = b"zlias\0\0\0\0\0alias\0\0\0\0\x01".to_vec(),
            _ => unreachable!(),
        }
        fs::write(&path, bytes).unwrap();
        if case == 6 {
            let path = dir.path().join("test.ifo");
            let text = fs::read_to_string(&path).unwrap();
            fs::write(path, text.replace("synwordcount=1", "synwordcount=2")).unwrap();
        }
        if case == 4 {
            let path = dir.path().join("test.ifo");
            let text = fs::read_to_string(&path).unwrap();
            let text = text
                .lines()
                .map(|line| {
                    if line.starts_with("idxfilesize=") {
                        format!(
                            "idxfilesize={}",
                            fs::metadata(dir.path().join("test.idx")).unwrap().len()
                        )
                    } else {
                        line.into()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            fs::write(path, text).unwrap();
        }
        let mut dictionary = Dictionary::open(dir.path().join("test.ifo")).unwrap();
        let mut rejected = false;
        for _ in 0..8 {
            if dictionary.prepare_step().is_err() {
                rejected = true;
                break;
            }
        }
        assert!(rejected, "case {case}");
        assert!(dictionary.prepare_step().is_err());
        assert!(!dictionary.is_ready());
        assert!(dictionary.lookup("alpha").is_err());
    }
}

#[test]
fn word_order_validation_crosses_preparation_batches() {
    let dir = tempfile::tempdir().unwrap();
    let mut idx = Vec::new();
    for n in 0..1000 {
        let word = if n == 584 {
            "a0000".to_owned()
        } else {
            format!("w{n:04}")
        };
        idx.extend_from_slice(word.as_bytes());
        idx.push(0);
        idx.extend_from_slice(&0u32.to_be_bytes());
        idx.extend_from_slice(&4u32.to_be_bytes());
    }
    fs::write(dir.path().join("test.idx"), &idx).unwrap();
    fs::write(dir.path().join("test.dict"), b"text").unwrap();
    fs::write(dir.path().join("test.ifo"), format!("StarDict's dict ifo file\nversion=2.4.2\nbookname=Order\nwordcount=1000\nidxfilesize={}\nsametypesequence=m\n", idx.len())).unwrap();
    let mut dictionary = Dictionary::open(dir.path().join("test.ifo")).unwrap();
    assert!(!dictionary.prepare_step().unwrap());
    let mut rejected = false;
    for _ in 0..8 {
        if dictionary.prepare_step().is_err() {
            rejected = true;
            break;
        }
    }
    assert!(rejected);
    assert!(dictionary.prepare_step().is_err());
    assert!(!dictionary.is_ready());
}

fn typed_dictionary(dir: &Path, data: &[u8], types: Option<&str>) -> Dictionary {
    fixture(dir, data, false);
    let path = dir.join("test.ifo");
    let text = fs::read_to_string(&path).unwrap().replace(
        "sametypesequence=h\n",
        &types.map_or(String::new(), |types| format!("sametypesequence={types}\n")),
    );
    fs::write(&path, text).unwrap();
    ready(&path)
}

#[test]
fn typed_definitions_preserve_text_and_skip_framed_binary_data() {
    let cases: &[(Option<&str>, &[u8], &str)] = &[
        (Some("th"), b"tone\0<p>Meaning</p>", "[tone]\nMeaning"),
        (Some("m"), b"text\0", "text"),
        (None, b"mfirst\0hsecond<br>line\0", "first\nsecond\nline"),
        (None, b"W\0\0\0\x02\xff\xfemmeaning\0", "meaning"),
        (Some("Wm"), b"\0\0\0\x02\xff\xfemeaning", "meaning"),
        (Some("mW"), b"meaning\0\xff\xfe", "meaning"),
    ];
    for &(types, data, expected) in cases {
        let dir = tempfile::tempdir().unwrap();
        let dictionary = typed_dictionary(dir.path(), data, types);
        assert_eq!(dictionary.lookup("alpha").unwrap()[0].text, expected);
    }
}

#[test]
fn malformed_typed_fields_fail_without_returning_partial_definitions() {
    let cases: &[(Option<&str>, &[u8])] = &[
        (None, b"mvalid\0mtruncated"),
        (Some("tm"), b"missing separator"),
        (Some("m"), b"first\0hidden"),
        (Some("m"), b"\xff"),
        (None, b"mvalid\0h\xff\0"),
        (None, b"mvalid\0W\0\0"),
        (None, b"W\0\0\0\x03x"),
        (Some("Wm"), b"\xff\xff\xff\xff"),
        (None, b"1invalid\0"),
    ];
    for &(types, data) in cases {
        let dir = tempfile::tempdir().unwrap();
        let dictionary = typed_dictionary(dir.path(), data, types);
        assert!(
            dictionary.lookup("alpha").is_err(),
            "accepted {types:?} {data:?}"
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let data = b"t\0".repeat(32768);
    let dictionary = typed_dictionary(dir.path(), &data, None);
    assert!(
        dictionary
            .lookup("alpha")
            .unwrap_err()
            .to_string()
            .contains("decoded definition too large")
    );
}

#[test]
fn dictionary_discovery_is_sorted_bounded_and_does_not_recurse() {
    use femto_reader::dictionary::{MAX_DICTIONARIES, discover};
    let dir = tempfile::tempdir().unwrap();
    assert!(discover(dir.path().join("missing")).unwrap().is_empty());
    std::fs::create_dir(dir.path().join("nested")).unwrap();
    fs::write(dir.path().join("nested/hidden.ifo"), b"").unwrap();
    fs::write(dir.path().join("z.ifo"), b"").unwrap();
    fs::write(dir.path().join("a.ifo"), b"").unwrap();
    fs::write(dir.path().join("ignored.txt"), b"").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(dir.path().join("a.ifo"), dir.path().join("alias.ifo")).unwrap();
    assert_eq!(
        discover(dir.path()).unwrap(),
        [dir.path().join("a.ifo"), dir.path().join("z.ifo")]
    );
    for n in 2..MAX_DICTIONARIES {
        fs::write(dir.path().join(format!("{n}.ifo")), b"").unwrap();
    }
    assert_eq!(discover(dir.path()).unwrap().len(), MAX_DICTIONARIES);
    fs::write(dir.path().join("overflow.ifo"), b"").unwrap();
    assert!(discover(dir.path()).is_err());
}

#[test]
fn dictionary_scan_counts_unrelated_entries_toward_work_limit() {
    use femto_reader::dictionary::discover;
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("dictionary.ifo"), b"").unwrap();
    for n in 0..4095 {
        fs::write(dir.path().join(format!("unrelated-{n}.txt")), b"").unwrap();
    }
    assert_eq!(
        discover(dir.path()).unwrap(),
        [dir.path().join("dictionary.ifo")]
    );
    fs::write(dir.path().join("overflow.txt"), b"").unwrap();
    assert!(matches!(
        discover(dir.path()),
        Err(femto_reader::Error::Limit("dictionary directory entries"))
    ));
}

#[test]
fn dictionary_names_use_utf8_byte_limits() {
    use femto_reader::dictionary::valid_name;
    assert!(valid_name(&format!("{}.ifo", "a".repeat(251))));
    assert!(!valid_name(&format!("{}.ifo", "a".repeat(252))));
    assert!(valid_name(&format!("{}a.ifo", "é".repeat(125))));
    assert!(!valid_name(&format!("{}.ifo", "é".repeat(126))));
    for name in [
        "",
        ".ifo",
        "a.IFO",
        "../a.ifo",
        "a/b.ifo",
        "a\\b.ifo",
        "a\0.ifo",
        "a\n.ifo",
        "a\u{85}.ifo",
    ] {
        assert!(!valid_name(name), "accepted {name:?}");
    }
}

#[cfg(unix)]
#[test]
fn dictionary_discovery_rejects_invalid_utf8_instead_of_lossy_names() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("valid.ifo"), b"").unwrap();
    fs::write(
        dir.path()
            .join(OsString::from_vec(b"invalid-\xff.ifo".to_vec())),
        b"",
    )
    .unwrap();
    assert!(matches!(
        femto_reader::dictionary::discover(dir.path()),
        Err(femto_reader::Error::Document(_))
    ));
}
