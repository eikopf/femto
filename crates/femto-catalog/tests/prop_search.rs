//! Discovery properties for search URLs: whatever is typed arrives at the
//! server as typed, in one query parameter or one path segment.
use femto_catalog::{
    Link,
    search::{MAX_QUERY_BYTES, Search, opensearch_url},
};
use proptest::prelude::*;

const SOURCE: &str = "https://catalog.example/opds/root?page=2";

/// RFC 3986 section 2.1, applied to one component.
fn percent_decode(text: &str) -> Option<String> {
    let (bytes, mut out, mut at) = (text.as_bytes(), Vec::new(), 0);
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = std::str::from_utf8(bytes.get(at + 1..at + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            out.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}
/// The raw value of a query parameter, which must occur exactly once.
fn parameter<'a>(url: &'a str, name: &str) -> Option<&'a str> {
    let query = url.split_once('?')?.1;
    let mut values = query
        .split('&')
        .filter_map(|pair| pair.strip_prefix(name)?.strip_prefix('='));
    let value = values.next()?;
    values.next().is_none().then_some(value)
}
/// Terms a reader might type: any script, punctuation that means something
/// in a URL or a template, and surrounding or repeated spaces.
fn term() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "\\PC{1,24}",
        3 => prop::collection::vec(prop::sample::select(vec![
            "a", "Z", "9", " ", "  ", "+", "%", "%2F", "%00", "&", "=", "?", "#", "/", "//", "\\", ".", "..", "{", "}", "{searchTerms}", "{query}",
            ":", "@", ";", "'", "\"", "<", ">", "é", "書", "😀", "\u{301}", "\u{200d}", "\u{a0}", "~", "-", "_", "*", "|", "^", "`", "[", "]",
        ]), 1..8).prop_map(|parts| parts.concat()),
        1 => (1usize..=MAX_QUERY_BYTES).prop_map(|len| "x".repeat(len)),
        1 => (1usize..=MAX_QUERY_BYTES / 4).prop_map(|len| "😀".repeat(len)),
    ]
    .prop_filter("a term the client accepts", |term: &String| {
        !term.trim().is_empty() && !term.chars().any(char::is_control) && term.len() <= MAX_QUERY_BYTES
    })
}
fn unreserved(raw: &str) -> bool {
    raw.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-._~%".contains(&b))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(3000))]

    /// OpenSearch: the term arrives percent-encoded in its parameter.
    #[test]
    fn opensearch_terms_arrive_as_typed(
        term in term(),
        (template, expected) in prop::sample::select(vec![
            ("/search?q={searchTerms}", "https://catalog.example/search?q="),
            ("search?q={searchTerms}&page={startPage?}&n={count?}", "https://catalog.example/opds/search?q="),
            ("?q={searchTerms}", "https://catalog.example/opds/root?q="),
            ("https://other.example/find?lang={language}&enc={inputEncoding}&q={searchTerms}", "https://other.example/find?lang=*&enc=UTF-8&q="),
            ("//other.example/find?start={startIndex}&q={searchTerms}", "https://other.example/find?start=1&q="),
            ("../s?q={searchTerms}#results", "https://catalog.example/s?q="),
        ]),
    ) {
        let url = opensearch_url(template, SOURCE, &term).map_err(|e| TestCaseError::fail(e.to_string()))?;
        prop_assert!(url.starts_with(expected), "{}", url);
        prop_assert!(!url.contains('#'));
        let raw = parameter(&url, "q").ok_or_else(|| TestCaseError::fail(format!("no single q in {url}")))?;
        prop_assert!(unreserved(raw), "{}", raw);
        prop_assert_eq!(percent_decode(raw), Some(term));
    }

    /// OpenSearch with the term in the path: it is one segment, as typed.
    #[test]
    fn opensearch_terms_in_a_path_are_one_segment(term in term().prop_filter("refused: the_terms_dot_and_dot_dot_are_refused_in_a_path_segment", |t| t != "." && t != "..")) {
        let url = opensearch_url("/search/{searchTerms}/results?n=1", SOURCE, &term).map_err(|e| TestCaseError::fail(e.to_string()))?;
        let raw = url.strip_prefix("https://catalog.example/search/").and_then(|rest| rest.strip_suffix("/results?n=1"))
            .ok_or_else(|| TestCaseError::fail(url.clone()))?;
        prop_assert!(unreserved(raw), "{}", raw);
        prop_assert_eq!(percent_decode(raw), Some(term));
    }

    /// OPDS 2: an RFC 6570 template with a `query` variable.
    #[test]
    fn opds2_terms_arrive_as_typed(
        term in term(),
        (template, expected) in prop::sample::select(vec![
            ("/search{?query}", "https://catalog.example/search?query="),
            ("search{?query,title,author}", "https://catalog.example/opds/search?query="),
            ("/search?lang=en{&query}", "https://catalog.example/search?lang=en&query="),
            ("https://other.example/s?query={query}", "https://other.example/s?query="),
        ]),
    ) {
        let link = Link {
            href: template.into(), resolved: None, title: None,
            media_type: Some("application/opds+json".into()),
            relations: vec!["search".into()], templated: true, indirect: false,
        };
        let url = link.search_url(SOURCE, &term).map_err(|e| TestCaseError::fail(format!("{e}: {term:?}")))?;
        prop_assert!(url.starts_with(expected), "{}", url);
        let raw = parameter(&url, "query").ok_or_else(|| TestCaseError::fail(format!("no single query in {url}")))?;
        prop_assert_eq!(percent_decode(raw), Some(term.clone()));
        prop_assert_eq!(link.search(SOURCE, &term).unwrap(), Search::Feed(url));
    }

    /// Any template and any term: an error or an HTTP(S) URL within the
    /// length limit, never a panic. The `+` and `#` operators and the prefix
    /// modifier are left out, and a stray percent sign: see the ignored tests.
    #[test]
    fn arbitrary_templates_never_panic(
        template in prop::collection::vec(prop::sample::select(vec![
            "{searchTerms}", "{searchTerms?}", "{query}", "{?query}", "{&query}", "{/query}", "{.query}", "{;query}", "{query*}", "{","}", "{}", "{startPage?}", "{count}",
            "/", "//", "?", "#", "&q=", "a", "..", ":", "@", "%41", "%zz", "http://", "https://h.example", "ftp://x/", "user:pw@", " ", "\\", "é",
        ]), 0..8).prop_map(|parts| parts.concat()),
        term in term(),
        opds2 in any::<bool>(),
    ) {
        let link = Link {
            href: template.clone(), resolved: None, title: None,
            media_type: Some(if opds2 { "application/opds+json" } else { "application/atom+xml" }.into()),
            relations: vec!["search".into()], templated: true, indirect: false,
        };
        for result in [link.search_url(SOURCE, &term).ok(), opensearch_url(&template, SOURCE, &term).ok(),
            link.search(SOURCE, &term).ok().and_then(|search| match search { Search::Feed(url) => Some(url), Search::Described { .. } => None })].into_iter().flatten() {
            prop_assert!(result.starts_with("http://") || result.starts_with("https://"), "{}", result);
            prop_assert!(result.len() <= 4096 && !result.contains('#'), "{}", result);
            let authority = result.split('/').nth(2).unwrap();
            prop_assert!(!authority.contains('@'), "{}", result);
        }
    }
}

/// The template comes from the catalog server and the term from the reader;
/// neither may stop the device. RFC 6570 reserved (`+`) and fragment (`#`)
/// expansion of a term outside ASCII panics inside `iri-string` 0.7.14.
#[test]
fn reserved_expansion_of_a_non_ascii_term_does_not_panic() {
    let mut panicked = Vec::new();
    for template in [
        "/search?query={+query}",
        "/search{#query}",
        "/s/{+query}?query=x",
    ] {
        for term in ["plain", "100%", "é", "書", "😀", "a😀b"] {
            let link = Link {
                href: template.into(),
                resolved: None,
                title: None,
                media_type: Some("application/opds+json".into()),
                relations: vec!["search".into()],
                templated: true,
                indirect: false,
            };
            if std::panic::catch_unwind(|| link.search_url(SOURCE, term).is_ok()).is_err() {
                panicked.push((template, term));
            }
        }
    }
    assert!(panicked.is_empty(), "panicked: {panicked:?}");
}

/// The RFC 6570 prefix modifier (`{query:3}`) on a term of multi-byte
/// characters overflows a subtraction inside `iri-string` 0.7.14. That is a
/// panic where overflow checks are on, as in this test build; the release
/// profiles leave them off, and there the count wraps.
#[test]
fn a_prefix_modifier_on_a_multi_byte_term_does_not_panic() {
    let link = Link {
        href: "/search{?query:3}".into(),
        resolved: None,
        title: None,
        media_type: Some("application/opds+json".into()),
        relations: vec!["search".into()],
        templated: true,
        indirect: false,
    };
    assert!(
        link.search_url(SOURCE, "abcd").is_err(),
        "a cut term is refused"
    );
    let outcome =
        std::panic::catch_unwind(|| link.search_url(SOURCE, "書書").map_err(|e| e.to_string()));
    assert!(outcome.is_ok(), "panicked");
}

/// A template in which `%` is followed by one ASCII character and then a
/// multi-byte one is not a template; it must be refused, not sliced.
/// Nothing here depends on what the reader types.
#[test]
fn a_template_with_a_stray_percent_sign_is_refused() {
    let mut panicked = Vec::new();
    for template in [
        "%/é",
        "/search?discount=5%,é{&query}",
        "/search{?query}#100%aé",
        "/caf%é{?query}",
    ] {
        let link = Link {
            href: template.into(),
            resolved: None,
            title: None,
            media_type: Some("application/opds+json".into()),
            relations: vec!["search".into()],
            templated: true,
            indirect: false,
        };
        match std::panic::catch_unwind(|| link.search_url(SOURCE, "term").is_ok()) {
            Ok(accepted) => assert!(!accepted, "{template}"),
            Err(_) => panicked.push(template),
        }
    }
    assert!(panicked.is_empty(), "panicked: {panicked:?}");
}

/// A term of dots in a path segment is a term, not a path instruction.
/// The two terms that would name another resource, encoded or not, are
/// refused.
#[test]
fn the_terms_dot_and_dot_dot_are_refused_in_a_path_segment() {
    const TEMPLATE: &str = "/opds/search/{searchTerms}/results";
    assert_eq!(
        opensearch_url(TEMPLATE, SOURCE, "...").unwrap(),
        "https://catalog.example/opds/search/.../results"
    );
    for term in [".", ".."] {
        assert!(
            matches!(
                opensearch_url(TEMPLATE, SOURCE, term),
                Err(femto_catalog::Error::Field("search query"))
            ),
            "{term}"
        );
    }
}
