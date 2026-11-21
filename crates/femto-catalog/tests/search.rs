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
