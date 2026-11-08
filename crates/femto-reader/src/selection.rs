//! Bounded source-text extraction for selection and clipping frontends.
use crate::{
    Error, Result,
    document::{Block, Chapter, Document},
    layout::Locator,
};
use std::{borrow::Borrow, ops::Range};
use unicode_segmentation::UnicodeSegmentation;

pub const MAX_SELECTION_BYTES: usize = 16 * 1024;
pub const MAX_SELECTION_CHAPTERS: usize = 8;

/// Extracts a half-open source range with text endpoints on grapheme boundaries.
/// Paragraphs are separated by two newlines. Images and generated layout marks
/// contribute no text. The caller must retain the exact document content identity
/// with any stored range. Chapter parsing remains synchronous.
pub fn extract(document: &mut Document, range: Range<Locator>) -> Result<String> {
    extract_with(range, |index| document.chapter(index))
}

/// Extracts only from the supplied parsed chapter, without reparsing or cloning it.
#[cfg(test)]
fn extract_from_chapter(chapter: &Chapter, range: Range<Locator>) -> Result<String> {
    extract_borrowed_with(range, |index| {
        if index == chapter.index {
            Ok(chapter)
        } else {
            Err(invalid())
        }
    })
}

pub(crate) fn matches_document(
    document: &mut Document,
    range: Range<Locator>,
    expected: &str,
) -> Result<bool> {
    matches_with(range, |index| document.chapter(index), expected)
}

pub(crate) fn matches_chapter(
    chapter: &Chapter,
    range: Range<Locator>,
    expected: &str,
) -> Result<bool> {
    matches_with(
        range,
        |index| {
            if index == chapter.index {
                Ok(chapter)
            } else {
                Err(invalid())
            }
        },
        expected,
    )
}

fn matches_with<C: Borrow<Chapter>>(
    range: Range<Locator>,
    chapter: impl FnMut(usize) -> Result<C>,
    expected: &str,
) -> Result<bool> {
    let mut offset = 0;
    let mut matches = true;
    visit_parts(range, chapter, |part| {
        let end = offset + part.len();
        matches &= expected.get(offset..end) == Some(part);
        offset = end;
    })?;
    Ok(matches && offset == expected.len())
}

fn invalid() -> Error {
    Error::Document("invalid text selection".into())
}

fn endpoint(chapter: &Chapter, at: Locator) -> Result<()> {
    let Some(Block::Text(paragraph)) = chapter.blocks.get(at.block) else {
        return Err(invalid());
    };
    if at.byte != paragraph.text.len()
        && !paragraph
            .text
            .grapheme_indices(true)
            .any(|(i, _)| i == at.byte)
    {
        return Err(invalid());
    }
    Ok(())
}

fn extract_with(
    range: Range<Locator>,
    chapter: impl FnMut(usize) -> Result<Chapter>,
) -> Result<String> {
    extract_borrowed_with(range, chapter)
}

fn extract_borrowed_with<C: Borrow<Chapter>>(
    range: Range<Locator>,
    chapter: impl FnMut(usize) -> Result<C>,
) -> Result<String> {
    let mut text = String::new();
    visit_parts(range, chapter, |part| text.push_str(part))?;
    Ok(text)
}

fn visit_parts<C: Borrow<Chapter>>(
    range: Range<Locator>,
    mut chapter: impl FnMut(usize) -> Result<C>,
    mut emit: impl FnMut(&str),
) -> Result<()> {
    if range.start >= range.end {
        return Err(invalid());
    }
    if range.end.chapter - range.start.chapter >= MAX_SELECTION_CHAPTERS {
        return Err(Error::Limit("selection chapters"));
    }
    let mut text_len = 0;
    for index in range.start.chapter..=range.end.chapter {
        let source = chapter(index)?;
        let chapter = source.borrow();
        if index == range.start.chapter {
            endpoint(chapter, range.start)?;
        }
        if index == range.end.chapter {
            endpoint(chapter, range.end)?;
        }
        for (block_index, block) in chapter.blocks.iter().enumerate() {
            let at = Locator {
                chapter: index,
                block: block_index,
                byte: 0,
            };
            if (index == range.start.chapter && block_index < range.start.block) || at >= range.end
            {
                continue;
            }
            let Block::Text(paragraph) = block else {
                continue;
            };
            let start = if index == range.start.chapter && block_index == range.start.block {
                range.start.byte
            } else {
                0
            };
            let end = if index == range.end.chapter && block_index == range.end.block {
                range.end.byte
            } else {
                paragraph.text.len()
            };
            let part = paragraph.text.get(start..end).ok_or_else(invalid)?;
            if part.is_empty() {
                continue;
            }
            let separator = if text_len == 0 { "" } else { "\n\n" };
            if part.len() > MAX_SELECTION_BYTES - text_len
                || separator.len() > MAX_SELECTION_BYTES - text_len - part.len()
            {
                return Err(Error::Limit("selection text"));
            }
            emit(separator);
            emit(part);
            text_len += separator.len() + part.len();
        }
    }
    if text_len == 0 {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Paragraph;

    fn at(chapter: usize, block: usize, byte: usize) -> Locator {
        Locator {
            chapter,
            block,
            byte,
        }
    }
    fn chapter(parts: &[&str]) -> Chapter {
        Chapter {
            language: "und".into(),
            index: 0,
            href: String::new(),
            blocks: parts
                .iter()
                .map(|text| {
                    Block::Text(Paragraph {
                        text: (*text).into(),
                        ..Default::default()
                    })
                })
                .collect(),
            warnings: vec![],
            anchors: Default::default(),
            notes: vec![],
            links: vec![],
        }
    }

    #[test]
    fn borrowed_chapter_preserves_extraction_and_endpoint_validation() {
        let mut document = Document::from_text("Borrowed", "Aé 👩‍💻 שלום\n\nSecond.").unwrap();
        let source = document.chapter(0).unwrap();
        let mut points = Vec::new();
        for (block, value) in source.blocks.iter().enumerate() {
            if let Block::Text(paragraph) = value {
                for byte in 0..=paragraph.text.len() {
                    points.push(at(0, block, byte));
                }
            }
        }
        for start in &points {
            for end in &points {
                let range = *start..*end;
                assert_eq!(
                    extract_from_chapter(&source, range.clone()).map_err(|e| e.to_string()),
                    extract(&mut document, range).map_err(|e| e.to_string()),
                );
            }
        }
        assert!(extract_from_chapter(&source, at(1, 0, 0)..at(1, 0, 1)).is_err());
        assert!(extract_from_chapter(&source, at(0, 0, 0)..at(1, 0, 1)).is_err());
        let oversized = chapter(&[&"x".repeat(MAX_SELECTION_BYTES + 1)]);
        assert!(matches!(
            extract_from_chapter(&oversized, at(0, 0, 0)..at(0, 0, MAX_SELECTION_BYTES + 1)),
            Err(Error::Limit("selection text"))
        ));
    }

    #[test]
    fn fragment_comparison_checks_unicode_separators_and_exact_length() {
        let source = chapter(&["Aé 👩‍💻 שלום", "second"]);
        let range = at(0, 0, 1)..at(0, 1, 6);
        let expected = "é 👩‍💻 שלום\n\nsecond";
        assert!(matches_chapter(&source, range.clone(), expected).unwrap());
        for forged in [
            "",
            "é 👩‍💻 שלוםsecond",
            "é 👩‍💻 שלום\nsecond",
            "é 👩‍💻 שלום\n\nsecond",
            "é 👩‍💻 שלום\n\nsecond!",
            "é 👩‍💻 שלום\n\nsecon",
        ] {
            assert!(!matches_chapter(&source, range.clone(), forged).unwrap());
        }
        assert!(matches_chapter(&source, at(0, 0, 2)..at(0, 1, 6), expected).is_err());
        assert!(matches_chapter(&source, at(0, 0, 0)..at(1, 0, 1), expected).is_err());
        let full = "x".repeat(MAX_SELECTION_BYTES);
        let source = chapter(&[&full, "x"]);
        assert!(matches_chapter(&source, at(0, 0, 0)..at(0, 0, full.len()), &full).unwrap());
        assert!(matches!(
            matches_chapter(&source, at(0, 0, 0)..at(0, 1, 1), "wrong"),
            Err(Error::Limit("selection text"))
        ));
    }

    #[test]
    fn mismatched_fragments_still_validate_later_chapters() {
        let mut reads = 0;
        let result = matches_with(
            at(0, 0, 0)..at(1, 0, 1),
            |index| {
                reads += 1;
                if index == 0 {
                    Ok(chapter(&["first"]))
                } else {
                    Err(Error::Document("later chapter failure".into()))
                }
            },
            "mismatched",
        );
        assert_eq!(reads, 2);
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("later chapter failure")
        );
        assert!(matches_with(at(0, 0, 0)..at(1, 0, 1), |_| Ok(chapter(&["x"])), "x\n\nx").unwrap());
    }

    #[test]
    fn preserves_unicode_source_and_paragraph_boundaries() {
        let source = "A e\u{301} 👩‍💻 שלום Z";
        let expected = "e\u{301} 👩‍💻 שלום";
        let range = at(0, 0, 2)..at(0, 0, source.len() - 2);
        assert_eq!(
            extract_with(range, |_| Ok(chapter(&[source]))).unwrap(),
            expected
        );
        assert_eq!(
            extract_with(at(0, 0, 1)..at(1, 1, 2), |index| {
                Ok(if index == 0 {
                    chapter(&["abc", "def"])
                } else {
                    chapter(&["ghi", "jkl"])
                })
            })
            .unwrap(),
            "bc\n\ndef\n\nghi\n\njk"
        );
    }

    #[test]
    fn rejects_split_graphemes_missing_blocks_and_empty_ranges() {
        for byte in [2, 3, 5, 8, 9, usize::MAX] {
            // The combining sequence occupies 1..4; the ZWJ emoji occupies 4..15.
            assert!(
                extract_with(at(0, 0, 0)..at(0, 0, byte), |_| {
                    Ok(chapter(&["Ae\u{301}👩‍💻Z"]))
                })
                .is_err(),
                "accepted byte {byte}"
            );
        }
        for range in [at(0, 0, 1)..at(0, 0, 1), at(1, 0, 0)..at(0, 0, 1)] {
            assert!(extract_with(range, |_| panic!("invalid order parsed a chapter")).is_err());
        }
        assert!(extract_with(at(0, 0, 0)..at(0, 1, 0), |_| Ok(chapter(&["a"]))).is_err());
        assert!(extract_with(at(0, 0, 1)..at(0, 1, 0), |_| Ok(chapter(&["a", "b"]))).is_err());
    }

    #[test]
    fn limits_output_including_separators_and_chapter_work() {
        let text = "x".repeat(MAX_SELECTION_BYTES);
        assert_eq!(
            extract_with(at(0, 0, 0)..at(0, 0, text.len()), |_| Ok(chapter(&[&text]))).unwrap(),
            text
        );
        assert!(matches!(
            extract_with(at(0, 0, 0)..at(0, 1, 1), |_| Ok(chapter(&[&text, "x"]))),
            Err(Error::Limit("selection text"))
        ));
        assert!(matches!(
            extract_with(at(0, 0, 0)..at(MAX_SELECTION_CHAPTERS, 0, 1), |_| panic!(
                "oversized range parsed"
            )),
            Err(Error::Limit("selection chapters"))
        ));
        let mut reads = 0;
        let result = extract_with(at(0, 0, 0)..at(MAX_SELECTION_CHAPTERS - 1, 0, 1), |_| {
            reads += 1;
            Ok(chapter(&["x"]))
        })
        .unwrap();
        assert_eq!(reads, MAX_SELECTION_CHAPTERS);
        assert_eq!(result, ["x"; MAX_SELECTION_CHAPTERS].join("\n\n"));
    }

    #[test]
    fn extracts_from_the_actual_document_parser() {
        let mut document =
            Document::from_text("Selection", "First paragraph.\n\nSecond paragraph.").unwrap();
        let chapter = document.chapter(0).unwrap();
        assert_eq!(chapter.blocks.len(), 2);
        assert_eq!(
            extract(&mut document, at(0, 0, 6)..at(0, 1, 6)).unwrap(),
            "paragraph.\n\nSecond"
        );
    }

    #[test]
    fn images_are_omitted_and_cannot_be_text_endpoints() {
        let mut source = chapter(&["before", "after"]);
        source.blocks.insert(
            1,
            Block::Image(crate::document::ImageBlock {
                width: None,
                height: None,
                source: "picture.png".into(),
                alt: "not selected text".into(),
                intrinsic: None,
                max_width: None,
                max_height: None,
                min_width: None,
                min_height: None,
                width_hint: None,
                height_hint: None,
            }),
        );
        assert_eq!(
            extract_with(at(0, 0, 0)..at(0, 2, 5), |_| Ok(source.clone())).unwrap(),
            "before\n\nafter"
        );
        for range in [at(0, 1, 0)..at(0, 2, 5), at(0, 0, 0)..at(0, 1, 0)] {
            assert!(extract_with(range, |_| Ok(source.clone())).is_err());
        }
        assert!(
            extract_with(at(0, 0, 0)..at(1, 0, 1), |index| {
                if index == 0 {
                    Ok(source.clone())
                } else {
                    Err(Error::Document("read failed".into()))
                }
            })
            .is_err()
        );
    }
}
