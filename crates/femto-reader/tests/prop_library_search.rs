//! Property tests of library search and ordering keys: a piece of a title
//! finds the title, in either normalization form and either letter case.
use femto_reader::state::{Library, LibraryEntry, LibraryMetadata, ReaderState};
use proptest::prelude::*;
use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

fn library(titles: &[String]) -> Library {
    Library {
        entries: titles
            .iter()
            .enumerate()
            .map(|(index, title)| LibraryEntry {
                path: format!("/books/{index}.epub").into(),
                metadata: LibraryMetadata {
                    id: format!("{index:064x}"),
                    title: title.clone(),
                    author: String::new(),
                    language: "und".into(),
                },
            })
            .collect(),
        ..Library::default()
    }
}

fn found(library: &Library, query: &str) -> Vec<String> {
    library
        .search(query)
        .into_iter()
        .map(|entry| entry.metadata.id.clone())
        .collect()
}

/// Text over letters whose case or normalization is not one-to-one.
fn title(sigma: bool) -> impl Strategy<Value = String> {
    let mut alphabet = vec![
        'a', 'B', 'e', 'E', ' ', '-', '\u{e9}', '\u{c9}', '\u{301}', '\u{30a}', '\u{323}',
        '\u{c5}', '\u{df}', '\u{130}', 'I', '\u{131}', '\u{1c5}', '\u{fb03}', '\u{2460}',
        '\u{d55c}', '\u{1112}', '\u{1161}', '\u{11ab}', '\u{2126}', '\u{212a}', 'J', '\u{30c}',
        '\u{39f}', '\u{394}', '\u{3bf}', '\u{3b4}', '\u{3c3}', '\u{3c2}',
    ];
    if sigma {
        alphabet.push('\u{3a3}');
    }
    prop::collection::vec(prop::sample::select(alphabet), 1..12).prop_map(String::from_iter)
}

/// A run of whole grapheme clusters of the normalized title, so that the
/// query never separates a letter from its accents.
fn piece(title: &str, from: prop::sample::Index, to: prop::sample::Index) -> String {
    let normal: String = title.nfc().collect();
    let clusters: Vec<&str> = normal.graphemes(true).collect();
    let (a, b) = (from.index(clusters.len() + 1), to.index(clusters.len() + 1));
    clusters[a.min(b)..a.max(b)].concat()
}

fn pieces_find_their_title(
    titles: Vec<String>,
    which: prop::sample::Index,
    from: prop::sample::Index,
    to: prop::sample::Index,
) -> Result<(), TestCaseError> {
    let library = library(&titles);
    let target = which.index(titles.len());
    let query = piece(&titles[target], from, to);
    let hits = found(&library, &query);
    prop_assert!(
        hits.contains(&format!("{target:064x}")),
        "{:?} is part of {:?} but does not find it",
        query,
        titles[target]
    );
    // Either normalization form of the query finds the same entries.
    prop_assert_eq!(&found(&library, &query.nfd().collect::<String>()), &hits);
    prop_assert_eq!(&found(&library, &query.nfc().collect::<String>()), &hits);
    // Searching neither reorders nor invents entries.
    let all = found(&library, "");
    prop_assert_eq!(all.len(), titles.len());
    let mut order = all.iter().filter(|id| hits.contains(id));
    prop_assert!(hits.iter().all(|id| order.next() == Some(id)));
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 2000, max_shrink_iters: 300, ..ProptestConfig::default() })]

    #[test]
    fn a_piece_of_a_title_finds_the_title(
        titles in prop::collection::vec(title(false), 1..4),
        which in any::<prop::sample::Index>(),
        from in any::<prop::sample::Index>(),
        to in any::<prop::sample::Index>(),
    ) {
        pieces_find_their_title(titles, which, from, to)?;
    }

    /// The same with capital sigma in the alphabet.
    #[test]
    #[ignore = "bug: a query ending in capital sigma does not match the same letters inside a title"]
    fn a_piece_of_a_greek_title_finds_the_title(
        titles in prop::collection::vec(title(true), 1..4),
        which in any::<prop::sample::Index>(),
        from in any::<prop::sample::Index>(),
        to in any::<prop::sample::Index>(),
    ) {
        pieces_find_their_title(titles, which, from, to)?;
    }

    /// Recent books are exactly the listed books with saved state, most
    /// recently opened first, ties in listing order.
    #[test]
    fn recent_books_are_the_opened_ones_newest_first(
        opened in prop::collection::vec(prop::option::of(0u64..4), 0..8),
    ) {
        let titles: Vec<String> = (0..opened.len()).map(|index| format!("t{index}")).collect();
        let library = library(&titles);
        let mut state = ReaderState::default();
        for (index, time) in opened.iter().enumerate() {
            if let Some(time) = time {
                state.set_position(&format!("{index:064x}"), Default::default(), *time);
            }
        }
        let mut expected: Vec<(u64, usize)> = opened
            .iter()
            .enumerate()
            .filter_map(|(index, time)| Some(((*time)?, index)))
            .collect();
        expected.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let recent: Vec<String> =
            library.recent(&state).into_iter().map(|entry| entry.metadata.id.clone()).collect();
        prop_assert_eq!(
            recent,
            expected.into_iter().map(|(_, index)| format!("{index:064x}")).collect::<Vec<_>>()
        );
    }
}

/// The title of the Odyssey in capitals is not found by its first four
/// letters in capitals: lowercasing the query alone turns its last sigma
/// into the word-final form, which the title's lowercase does not contain.
/// docs/navigation.md states that lowercase conversion is not full case
/// folding (STRASSE does not match Straße); this case differs in that the
/// query is a literal piece of the title.
#[test]
#[ignore = "bug: a query ending in capital sigma does not match the same letters inside a title"]
fn capital_sigma_at_the_end_of_a_query_matches_inside_a_title() {
    let library = library(&["\u{39f}\u{394}\u{3a5}\u{3a3}\u{3a3}\u{395}\u{399}\u{391}".into()]);
    assert_eq!(found(&library, "\u{39f}\u{394}\u{3a5}").len(), 1);
    assert_eq!(found(&library, "\u{39f}\u{394}\u{3a5}\u{3a3}").len(), 1);
}
