use femto_catalog::{
    Catalog,
    browser::Browser,
    search::{MAX_QUERY_BYTES, Search},
};
/// OPDS 2 templates resolve without the network.
trait Direct {
    fn search_url(&self, index: usize, query: &str) -> Result<String, femto_catalog::Error>;
}
impl Direct for Browser {
    fn search_url(&self, index: usize, query: &str) -> Result<String, femto_catalog::Error> {
        match self.search(index, query)? {
            Search::Feed(source) => Ok(source),
            Search::Described { .. } => panic!("unexpected description"),
        }
    }
}
fn browser(template: &str) -> Browser {
    let value = serde_json::json!({"metadata":{"title":"Search"},"links":[{"rel":"self","href":"/"},{"rel":"search","type":"application/opds+json; charset=utf-8","templated":true,"href":template}],"publications":[]});
    Browser::new(
        Catalog::parse(
            &serde_json::to_vec(&value).unwrap(),
            "https://example.org/opds/feed",
        )
        .unwrap(),
    )
}
#[test]
fn keyword_expansion_encodes_unicode_reserved_characters_and_omits_undefined_fields() {
    let browser = browser("search{?query,title,author}");
    let url = browser.search_url(0, "café &?#%+/ 雪").unwrap();
    assert_eq!(
        url,
        "https://example.org/opds/search?query=caf%C3%A9%20%26%3F%23%25%2B%2F%20%E9%9B%AA"
    );
    assert_eq!(browser.history_len(), 0);
    assert!(browser.search_url(1, "word").is_err());
    for template in [
        "search?query={query}",
        "search{?query*}",
        "search{?query:256}",
    ] {
        assert_eq!(
            self::browser(template).search_url(0, "snow 雪").unwrap(),
            "https://example.org/opds/search?query=snow%20%E9%9B%AA"
        );
    }
    assert_eq!(
        self::browser("?language=de{&query,author}")
            .search_url(0, "book")
            .unwrap(),
        "https://example.org/opds/feed?language=de&query=book"
    );
}
#[test]
fn invalid_ambiguous_and_oversized_searches_fail_before_loading() {
    for template in [
        "search{?title}",
        "search{?query",
        "search{?query:0}",
        "search{?query:2}",
        "search?query=old{&query}",
        "search#{query}",
        "ftp://example.org/{?query}",
        "https://user:secret@example.org/{?query}",
        "search{?query}#anchor",
    ] {
        assert!(
            browser(template).search_url(0, "books").is_err(),
            "{template}"
        );
    }
    for query in ["", "   ", "abc\n", "\u{7f}"] {
        assert!(browser("search{?query}").search_url(0, query).is_err());
    }
    assert!(
        browser("search{?query}")
            .search_url(0, &"a".repeat(MAX_QUERY_BYTES))
            .is_ok()
    );
    assert!(
        browser("search{?query}")
            .search_url(0, &"a".repeat(MAX_QUERY_BYTES + 1))
            .is_err()
    );
    assert!(
        browser(&format!("search{}", "{?query}".repeat(400)))
            .search_url(0, &"雪".repeat(80))
            .is_err()
    );
    // Reserved expansion cannot smuggle a second query value or truncate the keyword.
    assert!(
        browser("search?query={+query}")
            .search_url(0, "a&query=b")
            .is_err()
    );
}
#[test]
fn search_requires_a_templated_opds_search_link() {
    for change in 0..4 {
        let mut link = femto_catalog::Link {
            href: "search{?query}".into(),
            resolved: None,
            title: None,
            media_type: Some("application/opds+json".into()),
            relations: vec!["search".into()],
            templated: true,
            indirect: false,
        };
        match change {
            0 => link.templated = false,
            1 => link.indirect = true,
            2 => link.media_type = Some("text/html".into()),
            _ => link.relations.clear(),
        }
        assert!(link.search_url("https://example.org/", "books").is_err());
    }
}
#[test]
fn a_template_with_a_stray_percent_sign_is_refused() {
    for template in [
        "%/é",
        "/search?discount=5%,é{&query}",
        "/search{?query}#100%aé",
        "/caf%é{?query}",
    ] {
        assert!(
            browser(template).search_url(0, "term").is_err(),
            "{template}"
        );
    }
}
#[test]
fn reserved_expansion_encodes_a_term_outside_ascii() {
    for (term, encoded) in [
        ("plain", "plain"),
        ("100%", "100%25"),
        ("é", "%C3%A9"),
        ("書", "%E6%9B%B8"),
        ("a😀b", "a%F0%9F%98%80b"),
    ] {
        assert_eq!(
            browser("/search?query={+query}")
                .search_url(0, term)
                .unwrap(),
            format!("https://example.org/search?query={encoded}")
        );
        // A fragment is never sent, and a term in the path is not the query.
        for template in ["/search{#query}", "/s/{+query}?query=x"] {
            assert!(
                browser(template).search_url(0, term).is_err(),
                "{template} {term}"
            );
        }
    }
    // Reserved expansion passes an encoded triplet through, so a term that
    // looks encoded would arrive decoded: refused, whatever the bytes are.
    for term in ["%41", "%C2%00%8A", "%CA%C0é"] {
        assert!(
            browser("/search?query={+query}")
                .search_url(0, term)
                .is_err(),
            "{term}"
        );
    }
}
#[test]
fn a_prefix_modifier_counts_characters_of_a_multi_byte_term() {
    let browser = browser("/search{?query:3}");
    for term in ["書書", "書書書"] {
        assert_eq!(
            browser.search_url(0, term).unwrap(),
            format!(
                "https://example.org/search?query={}",
                "%E6%9B%B8".repeat(term.chars().count())
            )
        );
    }
    assert!(browser.search_url(0, "書書書書").is_err());
    assert!(browser.search_url(0, "abcd").is_err());
}
#[test]
fn reserved_expansion_passes_every_encoded_byte_of_a_term() {
    // Triplets that are not UTF-8 pass as they are; none is dropped, and
    // the one after an invalid triplet is not taken for part of it.
    for term in ["%C2%00%8A", "%CA%C0é", "%C3%C3x", "%E3%81x%F0%90%41"] {
        let path = term.replace('é', "%C3%A9");
        assert_eq!(
            browser("/s/{+query}?query={query}")
                .search_url(0, term)
                .unwrap(),
            format!(
                "https://example.org/s/{path}?query={}",
                path.replace('%', "%25").replace("%25C3%25A9", "%C3%A9")
            )
        );
    }
}
