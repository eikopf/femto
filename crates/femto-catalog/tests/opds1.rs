use femto_catalog::{Catalog, browser::Browser, search::Search};

const NAVIGATION: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<feed xmlns="http://www.w3.org/2005/Atom" xmlns:opds="http://opds-spec.org/2010/catalog">
  <id>urn:test:root</id>
  <title>Test &amp; Catalog</title>
  <updated>2026-11-21T20:52:44Z</updated>
  <link rel="self" href="/api/v1/opds" type="application/atom+xml;profile=opds-catalog;kind=navigation"/>
  <link rel="start" href="/api/v1/opds" type="application/atom+xml;profile=opds-catalog;kind=navigation"/>
  <link rel="search" type="application/opensearchdescription+xml" title="Search" href="/api/v1/opds/search.opds"/>
  <entry>
    <title>All Books</title>
    <id>urn:test:all</id>
    <link rel="subsection" href="/api/v1/opds/catalog?page=1&amp;size=50" type="application/atom+xml;profile=opds-catalog;kind=acquisition"/>
    <content type="text">Browse all <b>available</b> books</content>
  </entry>
  <entry>
    <title><![CDATA[Shelves <new>]]></title>
    <link rel="subsection" href="shelves" type="application/atom+xml;profile=opds-catalog;kind=navigation"/>
  </entry>
  <entry>
    <title>Not a feed</title>
    <link rel="alternate" href="/page.html" type="text/html"/>
  </entry>
</feed>"#;

const ACQUISITION: &str = r#"<feed xmlns="http://www.w3.org/2005/Atom" xmlns:dc="http://purl.org/dc/terms/">
  <title>All Books</title>
  <link rel="self" href="/api/v1/opds/catalog?page=1&amp;size=2" type="application/atom+xml;profile=opds-catalog;kind=acquisition"/>
  <link rel="next" href="/api/v1/opds/catalog?page=2&amp;size=2" type="application/atom+xml;profile=opds-catalog;kind=acquisition"/>
  <entry>
    <title>The First   Book</title>
    <id>urn:test:book:1</id>
    <author><name>Ada Author</name><uri>https://example.test/ada</uri></author>
    <author><name>Ben &#66;ook</name></author>
    <dc:language>en</dc:language>
    <link href="/api/v1/opds/1/download?fileId=1" rel="http://opds-spec.org/acquisition" type="application/epub+zip" title="EPUB"/>
    <link rel="http://opds-spec.org/image" href="/api/v1/opds/1/cover" type="image/jpeg"/>
    <link rel="http://opds-spec.org/image/thumbnail" href="/api/v1/opds/1/cover" type="image/jpeg"/>
  </entry>
  <entry>
    <title>Paid Book</title>
    <link href="/buy/2" rel="http://opds-spec.org/acquisition/buy" type="application/epub+zip"/>
    <link href="/pdf/2" rel="http://opds-spec.org/acquisition/open-access" type="application/pdf"/>
  </entry>
</feed>"#;

#[test]
fn navigation_feeds_list_subsections_and_follow_relative_links() {
    let source = "http://library.test:6060/api/v1/opds";
    let catalog = Catalog::parse(NAVIGATION.as_bytes(), source).unwrap();
    assert_eq!(catalog.title, "Test & Catalog");
    assert!(catalog.publications.is_empty());
    let entries: Vec<_> = catalog
        .navigation
        .iter()
        .map(|link| {
            (
                link.title.as_deref().unwrap(),
                link.resolved.as_deref().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        entries,
        [
            (
                "All Books",
                "http://library.test:6060/api/v1/opds/catalog?page=1&size=50"
            ),
            ("Shelves <new>", "http://library.test:6060/api/v1/shelves"),
        ]
    );
    let browser = Browser::new(catalog);
    let labels: Vec<_> = browser.destinations().iter().map(|d| d.label).collect();
    // The start link points at this feed, so it is not offered.
    assert_eq!(labels, ["All Books", "Shelves <new>"]);
    assert_eq!(browser.searches().count(), 1);
    // The search link names a description that must be fetched first.
    assert_eq!(
        browser.search(0, "crow road").unwrap(),
        Search::Described {
            description: "http://library.test:6060/api/v1/opds/search.opds".into(),
            query: "crow road".into()
        }
    );
}

#[test]
fn opensearch_templates_expand_against_their_document() {
    use femto_catalog::search::{described_url, opensearch_url};
    let description = br#"<?xml version="1.0" encoding="UTF-8"?>
<OpenSearchDescription xmlns="http://a9.com/-/spec/opensearch/1.1/">
  <ShortName>Test</ShortName>
  <Url type="text/html" template="/web?q={searchTerms}"/>
  <Url type="application/atom+xml;profile=opds-catalog;kind=acquisition"
       template="/api/v1/opds/catalog?q={searchTerms}&amp;page={startPage?}"/>
</OpenSearchDescription>"#;
    let source = "http://library.test:6060/api/v1/opds/search.opds";
    assert_eq!(
        described_url(description, source, "caf\u{e9} & snow").unwrap(),
        "http://library.test:6060/api/v1/opds/catalog?q=caf%C3%A9%20%26%20snow&page="
    );
    assert_eq!(
        opensearch_url("find/{searchTerms}/{startPage}", source, "a/b").unwrap(),
        "http://library.test:6060/api/v1/opds/find/a%2Fb/1"
    );
    for bad in [
        "/s?q={searchTerm}",
        "/s?q={searchTerms",
        "/s?q={searchTerms}&k={apiKey}",
    ] {
        assert!(opensearch_url(bad, source, "x").is_err(), "{bad}");
    }
    assert!(opensearch_url("/s?q={searchTerms}", source, " ").is_err());
    assert!(described_url(b"<OpenSearchDescription/>", source, "x").is_err());
    // A feed may also carry the template itself.
    let feed = r#"<feed><title>T</title><link rel="search" type="application/atom+xml" href="/s?q={searchTerms}"/></feed>"#;
    let browser =
        Browser::new(Catalog::parse(feed.as_bytes(), "https://library.test/opds").unwrap());
    assert_eq!(
        browser.search(0, "x y").unwrap(),
        Search::Feed("https://library.test/s?q=x%20y".into())
    );
}

#[test]
fn acquisition_feeds_offer_direct_epubs_and_paging_only() {
    let source = "https://library.test/api/v1/opds/catalog?page=1&size=2";
    let catalog = Catalog::parse(ACQUISITION.as_bytes(), source).unwrap();
    assert_eq!(catalog.publications.len(), 2);
    let first = &catalog.publications[0];
    assert_eq!(first.title, "The First Book");
    assert_eq!(first.identifier.as_deref(), Some("urn:test:book:1"));
    assert_eq!(first.authors, ["Ada Author", "Ben Book"]);
    assert_eq!(first.images.len(), 2);
    assert_eq!(
        first.links.iter().find_map(|link| link.direct_epub()),
        Some("https://library.test/api/v1/opds/1/download?fileId=1")
    );
    // Purchases and other formats are not downloadable books.
    let second = &catalog.publications[1];
    assert_eq!(second.title, "Paid Book");
    assert!(second.links.iter().all(|link| link.direct_epub().is_none()));
    let browser = Browser::new(catalog);
    let pages: Vec<_> = browser
        .destinations()
        .iter()
        .map(|d| (d.label, d.source, d.paging))
        .collect();
    assert_eq!(
        pages,
        [(
            "Next page",
            "https://library.test/api/v1/opds/catalog?page=2&size=2",
            true
        )]
    );
}

#[test]
fn malformed_and_oversized_feeds_are_rejected() {
    let source = "https://library.test/opds";
    for bad in [
        "<html><body>Sign in</body></html>",
        "<feed><title>Open",
        "<feed><entry><title>x</title></feed>",
        "<feed><link href=\"javascript:alert(1)\" rel=\"self\"/></feed>",
    ] {
        assert!(Catalog::parse(bad.as_bytes(), source).is_err(), "{bad}");
    }
    let deep = format!("<feed>{}{}</feed>", "<a>".repeat(40), "</a>".repeat(40));
    assert!(Catalog::parse(deep.as_bytes(), source).is_err());
    let many: String = (0..300)
        .map(|n| {
            format!(
                "<entry><title>{n}</title><link rel=\"http://opds-spec.org/acquisition\" \
                 href=\"/b/{n}\" type=\"application/epub+zip\"/></entry>"
            )
        })
        .collect();
    assert!(Catalog::parse(format!("<feed>{many}</feed>").as_bytes(), source).is_err());
    // An empty feed is a valid, empty catalog.
    let empty = Catalog::parse(b"<feed><title/></feed>", source).unwrap();
    assert_eq!(empty.title, "Catalog");
    assert!(empty.navigation.is_empty() && empty.publications.is_empty());
}

#[test]
fn later_pages_join_the_listing_until_one_is_empty() {
    let page = |number: u32, next: Option<u32>, books: u32| {
        let next = next.map_or(String::new(), |next| {
            format!(r#"<link rel="next" href="/list?page={next}" type="application/atom+xml"/>"#)
        });
        let entries: String = (0..books)
            .map(|book| {
                format!(
                    "<entry><title>{number}.{book}</title><link rel=\"http://opds-spec.org/acquisition\" \
                     href=\"/b/{number}/{book}\" type=\"application/epub+zip\"/></entry>"
                )
            })
            .collect();
        let source = format!("https://library.test/list?page={number}");
        Catalog::parse(
            format!("<feed><title>List</title>{next}{entries}</feed>").as_bytes(),
            &source,
        )
        .unwrap()
    };
    let mut browser = Browser::new(page(1, Some(2), 2));
    assert_eq!(
        browser.continuation(),
        Some("https://library.test/list?page=2")
    );
    assert!(browser.append(page(2, Some(3), 2)));
    assert_eq!(
        browser.continuation(),
        Some("https://library.test/list?page=3")
    );
    let titles: Vec<_> = browser.publications().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, ["1.0", "1.1", "2.0", "2.1"]);
    // A page that points at itself, or back at the start, ends the listing.
    assert!(browser.append(page(3, Some(3), 1)));
    assert_eq!(browser.continuation(), None);
    let mut looped = Browser::new(page(1, Some(2), 1));
    assert!(looped.append(page(2, Some(1), 1)));
    assert_eq!(looped.continuation(), None);
    // So does a page without entries.
    let mut ended = Browser::new(page(1, Some(2), 1));
    assert!(!ended.append(page(2, Some(3), 0)));
    assert_eq!(ended.continuation(), None);
    assert_eq!(ended.publications().count(), 1);
}
