use femto_catalog::{
    Catalog,
    browser::{Browser, MAX_HISTORY},
};
fn feed(source: &str) -> Catalog {
    Catalog::parse(
        include_bytes!("../../../tests/fixtures/catalog.json"),
        source,
    )
    .unwrap()
}
#[test]
fn navigation_and_history_are_bounded_and_transactional() {
    let mut browser = Browser::new(feed("https://example.org/opds/catalog.json"));
    let destinations = browser.destinations();
    assert_eq!(
        destinations.iter().map(|d| d.source).collect::<Vec<_>>(),
        vec![
            "https://example.org/opds/recent.json",
            "https://example.org/science",
            "https://example.org/opds/catalog.json?language=en",
            "https://example.org/opds/catalog.json?language=de",
            "https://example.org/opds/catalog.json?page=2"
        ]
    );
    assert_eq!(browser.publications().count(), 1);
    assert!(
        !browser
            .back::<()>(|_| panic!("empty history must not load"))
            .unwrap()
    );
    assert!(
        browser
            .follow("https://example.org/fail", |_| Err("offline"))
            .is_err()
    );
    assert_eq!(browser.history_len(), 0);
    for n in 0..20 {
        browser
            .follow::<()>(&format!("https://example.org/{n}"), |s| Ok(feed(s)))
            .unwrap();
    }
    assert_eq!(browser.history_len(), MAX_HISTORY);
    assert!(browser.back(|_| Err("offline")).is_err());
    assert_eq!(browser.current().source, "https://example.org/19");
    assert_eq!(browser.back_source(), Some("https://example.org/18"));
    browser
        .follow::<()>("https://example.org/19", |s| Ok(feed(s)))
        .unwrap();
    assert_eq!(browser.history_len(), MAX_HISTORY);
    for n in (3..19).rev() {
        assert!(browser.back::<()>(|s| Ok(feed(s))).unwrap());
        assert_eq!(browser.current().source, format!("https://example.org/{n}"));
    }
    assert_eq!(browser.history_len(), 0);
}
#[test]
fn unsupported_destinations_are_not_browsable() {
    let mut catalog = feed("https://example.org/");
    catalog.navigation[0].media_type = Some("text/html".into());
    catalog.groups[0].navigation[0].templated = true;
    catalog.facets[0].links[0].resolved = None;
    catalog.facets[0].links[1].indirect = true;
    let browser = Browser::new(catalog);
    assert_eq!(browser.destinations().len(), 1);
    assert_eq!(browser.destinations()[0].label, "Next page");
    assert!(browser.destinations()[0].paging);
}
