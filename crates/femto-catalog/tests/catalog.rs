use femto_catalog::{Catalog, MAX_FEED_BYTES};
use serde_json::{Value, json};
const SOURCE: &str = "https://catalog.example/opds/catalog.json";
fn feed() -> Value {
    serde_json::from_slice(include_bytes!("../../../tests/fixtures/catalog.json")).unwrap()
}
fn parse(value: &Value) -> Result<Catalog, femto_catalog::Error> {
    Catalog::parse(&serde_json::to_vec(value).unwrap(), SOURCE)
}

#[test]
fn collections_resolve_against_response_url_and_preserve_search_templates() {
    let catalog = parse(&feed()).unwrap();
    assert_eq!(catalog.title, "Femto catalog specimen");
    assert_eq!(
        catalog.links[1].resolved.as_deref(),
        Some("https://catalog.example/opds/catalog.json?page=2")
    );
    assert_eq!(catalog.links[2].href, "search{?query}");
    assert!(catalog.links[2].resolved.is_none());
    assert_eq!(
        catalog.groups[0].navigation[0].resolved.as_deref(),
        Some("https://catalog.example/science")
    );
    assert_eq!(catalog.facets[0].links.len(), 2);
    let book = &catalog.publications[0];
    assert_eq!(book.title, "A specimen");
    assert_eq!(book.authors, ["First Author", "Second Author"]);
    assert_eq!(
        book.links[0].direct_epub(),
        Some("https://catalog.example/books/sample.epub")
    );
    assert!(book.links[1].direct_epub().is_none());
    assert_eq!(
        book.images[0].resolved.as_deref(),
        Some("https://catalog.example/opds/covers/sample.png")
    );
}

#[test]
fn unsupported_or_credential_bearing_links_never_become_download_actions() {
    for href in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://user:secret@example.com/a",
        "https://example.com/\nfile",
        "\\\\example.com\\file",
    ] {
        let mut value = feed();
        value["publications"][0]["links"][0]["href"] = href.into();
        let catalog = parse(&value).unwrap();
        assert!(catalog.publications[0].links[0].resolved.is_none());
        assert!(catalog.publications[0].links[0].direct_epub().is_none());
    }
    let mut value = feed();
    value["publications"][0]["links"][0]["properties"] =
        json!({"indirectAcquisition":[{"type":"application/epub+zip"}]});
    assert!(
        parse(&value).unwrap().publications[0].links[0]
            .direct_epub()
            .is_none()
    );
    assert!(Catalog::parse(b"{}", "https://user:password@example.com/").is_err());
    // Server-provided self links do not replace the trusted response URL base.
    value["links"][0]["href"] = "https://elsewhere.example/".into();
    assert_eq!(
        parse(&value).unwrap().navigation[0].resolved.as_deref(),
        Some("https://catalog.example/opds/recent.json")
    );
}

#[test]
fn malformed_shapes_and_resource_limits_fail_without_a_partial_catalog() {
    for key in [
        "metadata",
        "links",
        "navigation",
        "publications",
        "groups",
        "facets",
    ] {
        let mut value = feed();
        value[key] = false.into();
        assert!(parse(&value).is_err(), "{key}");
    }
    assert!(Catalog::parse(&vec![b' '; MAX_FEED_BYTES + 1], SOURCE).is_err());
    let mut value = feed();
    value["metadata"]["title"] = "x".repeat(4097).into();
    assert!(parse(&value).is_err());
    let mut value = feed();
    value["publications"] = json!(vec![value["publications"][0].clone(); 257]);
    assert!(parse(&value).is_err());
    let mut value = feed();
    value["navigation"] = json!(vec![value["navigation"][0].clone(); 1025]);
    assert!(parse(&value).is_err());
    let mut nested = Value::Null;
    for _ in 0..18 {
        nested = json!({"extension":nested});
    }
    let mut value = feed();
    value["extension"] = nested;
    assert!(parse(&value).is_err());
    let mut value = feed();
    value["links"] = json!([]);
    assert!(parse(&value).is_err());
    let mut value = feed();
    value["navigation"][0]
        .as_object_mut()
        .unwrap()
        .remove("title");
    assert!(parse(&value).is_err());
    let empty = json!({"metadata":{"title":"No results"},"links":[{"rel":"self","href":"catalog.json"}],"publications":[]});
    assert!(parse(&empty).unwrap().publications.is_empty());
}
