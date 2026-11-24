//! Discovery properties for feed parsing: generated OPDS 1 (Atom) and OPDS 2
//! (JSON) feeds come back with the same entries, and their links resolve as
//! RFC 3986 section 5.2 says, computed here without the `url` crate.
use femto_catalog::{Catalog, Link};
use proptest::prelude::*;
use serde_json::json;

const ACQUISITION: &str = "http://opds-spec.org/acquisition";

// ---------------------------------------------------------------- references

#[derive(Debug, Clone)]
struct Base {
    segments: Vec<String>,
    query: Option<String>,
}
impl Base {
    fn path(&self) -> String {
        format!("/{}", self.segments.join("/"))
    }
    fn url(&self) -> String {
        let query = self
            .query
            .as_ref()
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        format!("https://catalog.example{}{query}", self.path())
    }
}
/// A URI reference by its components, as RFC 3986 section 5.2.2 names them.
#[derive(Debug, Clone, Default)]
struct Reference {
    scheme: Option<&'static str>,
    authority: Option<String>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}
impl Reference {
    fn text(&self) -> String {
        let mut out = String::new();
        if let Some(scheme) = self.scheme {
            out += &format!("{scheme}:");
        }
        if let Some(authority) = &self.authority {
            out += &format!("//{authority}");
        }
        out += &self.path;
        if let Some(query) = &self.query {
            out += &format!("?{query}");
        }
        if let Some(fragment) = &self.fragment {
            out += &format!("#{fragment}");
        }
        out
    }
}
/// RFC 3986 section 5.2.4.
fn remove_dot_segments(path: &str) -> String {
    let (mut input, mut output) = (path.to_owned(), String::new());
    let pop = |output: &mut String| output.truncate(output.rfind('/').unwrap_or(0));
    while !input.is_empty() {
        if input.starts_with("../") {
            input.drain(..3);
        } else if input.starts_with("./") {
            input.drain(..2);
        } else if input.starts_with("/./") {
            input.replace_range(..3, "/");
        } else if input == "/." {
            input = "/".into();
        } else if input.starts_with("/../") {
            input.replace_range(..4, "/");
            pop(&mut output);
        } else if input == "/.." {
            input = "/".into();
            pop(&mut output);
        } else if input == "." || input == ".." {
            input.clear();
        } else {
            let start = usize::from(input.starts_with('/'));
            let end = input[start..].find('/').map_or(input.len(), |i| i + start);
            output.push_str(&input[..end]);
            input.drain(..end);
        }
    }
    output
}
/// RFC 3986 sections 5.2.2 and 5.3, for an `https` base with an authority.
fn resolve(base: &Base, reference: &Reference) -> String {
    let (scheme, authority, path, query);
    if let Some(given) = reference.scheme {
        scheme = given;
        authority = reference.authority.clone();
        path = remove_dot_segments(&reference.path);
        query = reference.query.clone();
    } else {
        scheme = "https";
        if reference.authority.is_some() {
            authority = reference.authority.clone();
            path = remove_dot_segments(&reference.path);
            query = reference.query.clone();
        } else {
            authority = Some("catalog.example".into());
            if reference.path.is_empty() {
                path = base.path();
                query = reference.query.clone().or(base.query.clone());
            } else {
                if reference.path.starts_with('/') {
                    path = remove_dot_segments(&reference.path);
                } else {
                    let base_path = base.path();
                    let merged = format!(
                        "{}{}",
                        &base_path[..=base_path.rfind('/').unwrap()],
                        reference.path
                    );
                    path = remove_dot_segments(&merged);
                }
                query = reference.query.clone();
            }
        }
    }
    // An HTTP URL with an empty path names the root.
    let path = if path.is_empty() { "/".into() } else { path };
    let mut out = format!("{scheme}://{}{path}", authority.unwrap());
    if let Some(query) = query {
        out += &format!("?{query}");
    }
    if let Some(fragment) = &reference.fragment {
        out += &format!("#{fragment}");
    }
    out
}
fn segment() -> impl Strategy<Value = String> {
    prop_oneof![
        6 => "[a-z0-9]{1,3}",
        1 => "[a-z]{1,2}\\.[a-z]{1,4}",
        1 => Just("...".to_owned()),
        1 => Just(".a".to_owned()),
    ]
}
fn dotted_segment() -> impl Strategy<Value = String> {
    prop_oneof![5 => segment(), 2 => Just("..".to_owned()), 1 => Just(".".to_owned())]
}
fn query() -> impl Strategy<Value = Option<String>> {
    prop::option::of("[a-z]{1,3}=[a-z0-9]{0,3}(&[a-z]=[0-9])?")
}
fn base() -> impl Strategy<Value = Base> {
    (
        prop::collection::vec(segment(), 0..4),
        any::<bool>(),
        query(),
    )
        .prop_map(|(mut segments, directory, query)| {
            if directory {
                segments.push(String::new());
            }
            Base { segments, query }
        })
}
/// A path of segments, some of them dots, some empty, with or without a
/// leading and a trailing slash.
fn path(rooted: bool) -> impl Strategy<Value = String> {
    (
        segment(),
        prop::collection::vec(
            prop_oneof![8 => dotted_segment(), 1 => Just(String::new())],
            0..5,
        ),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(move |(first, rest, dots_first, slash)| {
            let first = if dots_first { "..".to_owned() } else { first };
            let mut out = if rooted {
                "/".to_owned()
            } else {
                String::new()
            };
            out += &first;
            for part in rest {
                out += "/";
                out += &part;
            }
            if slash {
                out += "/";
            }
            out
        })
}
fn reference() -> impl Strategy<Value = Reference> {
    let fragment = || prop::option::weighted(0.2, "[a-z]{1,3}");
    prop_oneof![
        4 => (path(false), query(), fragment()).prop_map(|(path, query, fragment)| Reference { path, query, fragment, ..Default::default() }),
        2 => (path(true), query(), fragment()).prop_map(|(path, query, fragment)| Reference { path, query, fragment, ..Default::default() }),
        1 => (query(), fragment()).prop_map(|(query, fragment)| Reference { query, fragment, ..Default::default() }),
        1 => (prop::sample::select(vec![".", "..", "./", "../", "/", "../..", "/.", "/.."]), query()).prop_map(|(path, query)| Reference { path: path.into(), query, ..Default::default() }),
        1 => ("[a-z]{1,4}\\.example(:8080)?", prop::option::of(path(true)), query()).prop_map(|(host, path, query)| Reference { authority: Some(host), path: path.unwrap_or_default(), query, ..Default::default() }),
        1 => (prop::sample::select(vec!["http", "https"]), "[a-z]{1,4}\\.example(:8080)?", prop::option::of(path(true)), query(), fragment()).prop_map(|(scheme, host, path, query, fragment)| Reference { scheme: Some(scheme), authority: Some(host), path: path.unwrap_or_default(), query, fragment }),
    ]
}

/// The examples of RFC 3986 sections 5.4.1 and 5.4.2, with the base moved to
/// an HTTP origin, through both feed formats.
#[test]
fn rfc_3986_examples_resolve_in_both_formats() {
    let source = "http://a/b/c/d;p?q";
    let cases = [
        ("g", "http://a/b/c/g"),
        ("./g", "http://a/b/c/g"),
        ("g/", "http://a/b/c/g/"),
        ("/g", "http://a/g"),
        ("//g", "http://g/"),
        ("?y", "http://a/b/c/d;p?y"),
        ("g?y", "http://a/b/c/g?y"),
        ("#s", "http://a/b/c/d;p?q#s"),
        ("g#s", "http://a/b/c/g#s"),
        ("g?y#s", "http://a/b/c/g?y#s"),
        (";x", "http://a/b/c/;x"),
        ("g;x", "http://a/b/c/g;x"),
        ("g;x?y#s", "http://a/b/c/g;x?y#s"),
        ("", "http://a/b/c/d;p?q"),
        (".", "http://a/b/c/"),
        ("./", "http://a/b/c/"),
        ("..", "http://a/b/"),
        ("../", "http://a/b/"),
        ("../g", "http://a/b/g"),
        ("../..", "http://a/"),
        ("../../", "http://a/"),
        ("../../g", "http://a/g"),
        ("../../../g", "http://a/g"),
        ("../../../../g", "http://a/g"),
        ("/./g", "http://a/g"),
        ("/../g", "http://a/g"),
        ("g.", "http://a/b/c/g."),
        (".g", "http://a/b/c/.g"),
        ("g..", "http://a/b/c/g.."),
        ("..g", "http://a/b/c/..g"),
        ("./../g", "http://a/b/g"),
        ("./g/.", "http://a/b/c/g/"),
        ("g/./h", "http://a/b/c/g/h"),
        ("g/../h", "http://a/b/c/h"),
        ("g;x=1/./y", "http://a/b/c/g;x=1/y"),
        ("g;x=1/../y", "http://a/b/c/y"),
        ("g?y/./x", "http://a/b/c/g?y/./x"),
        ("g?y/../x", "http://a/b/c/g?y/../x"),
        ("g#s/./x", "http://a/b/c/g#s/./x"),
        ("g#s/../x", "http://a/b/c/g#s/../x"),
    ];
    for (reference, expected) in cases {
        let atom = format!("<feed><link href=\"{reference}\"/></feed>");
        let catalog = Catalog::parse(atom.as_bytes(), source).unwrap();
        assert_eq!(
            catalog.links[0].resolved.as_deref(),
            Some(expected),
            "{reference}"
        );
        let feed = json!({"metadata": {"title": "t"}, "navigation": [],
            "links": [{"rel": "self", "href": source}, {"href": reference}]});
        // An empty href is not a link in OPDS 2.
        if !reference.is_empty() {
            let catalog = Catalog::parse(feed.to_string().as_bytes(), source).unwrap();
            assert_eq!(
                catalog.links[1].resolved.as_deref(),
                Some(expected),
                "{reference}"
            );
        }
    }
}

// ---------------------------------------------------------------------- model

#[derive(Debug, Clone)]
struct LinkSpec {
    reference: Reference,
    relations: Option<Vec<&'static str>>,
    media: Option<&'static str>,
    title: Option<String>,
}
#[derive(Debug, Clone)]
struct EntrySpec {
    title: String,
    /// Identifiers in document order, each `id` or `dc:identifier`.
    identifiers: Vec<(bool, String)>,
    authors: Vec<String>,
    links: Vec<LinkSpec>,
}
#[derive(Debug, Clone)]
struct FeedSpec {
    base: Base,
    title: String,
    links: Vec<LinkSpec>,
    entries: Vec<EntrySpec>,
}
/// How the document is spelled; none of it changes what the feed means.
#[derive(Debug, Clone)]
struct Spelling {
    prefix: bool,
    declaration: bool,
    noise: bool,
    indent: bool,
    /// Per text: 0 entities, 1 decimal references, 2 hex references, 3 CDATA.
    escapes: Vec<u8>,
    padding: bool,
}

/// Words separated by single spaces: the form the parser normalizes to.
fn words() -> impl Strategy<Value = String> {
    prop::collection::vec("[a-zA-Z0-9&<>'\"é書😀;#\\]\\[]{1,6}", 1..4).prop_map(|w| w.join(" "))
}
fn link_spec() -> impl Strategy<Value = LinkSpec> {
    (
        reference(),
        prop::option::weighted(
            0.85,
            prop::collection::vec(
                prop::sample::select(vec![
                    "alternate",
                    "related",
                    "subsection",
                    "next",
                    "start",
                    "search",
                    "http://opds-spec.org/acquisition",
                    "http://opds-spec.org/acquisition/open-access",
                    "http://opds-spec.org/acquisition/buy",
                    "http://opds-spec.org/acquisition/borrow",
                    "http://opds-spec.org/acquisition/sample",
                    "http://opds-spec.org/image",
                    "http://opds-spec.org/image/thumbnail",
                    "http://opds-spec.org/sort/new",
                ]),
                1..3,
            ),
        ),
        prop::option::weighted(
            0.85,
            prop::sample::select(vec![
                "application/epub+zip",
                "application/atom+xml",
                "application/atom+xml;profile=opds-catalog;kind=navigation",
                "application/atom+xml; profile=opds-catalog; kind=acquisition",
                "application/pdf",
                "image/jpeg",
                "text/html",
            ]),
        ),
        prop::option::weighted(0.3, words()),
    )
        .prop_map(|(reference, relations, media, title)| LinkSpec {
            reference,
            relations,
            media,
            title,
        })
}
fn entry_spec() -> impl Strategy<Value = EntrySpec> {
    (
        words(),
        prop::collection::vec((any::<bool>(), "[a-z0-9:-]{1,12}"), 0..3),
        prop::collection::vec(words(), 0..3),
        prop::collection::vec(link_spec(), 0..4),
    )
        .prop_map(|(title, identifiers, authors, links)| EntrySpec {
            title,
            identifiers,
            authors,
            links,
        })
}
fn feed_spec() -> impl Strategy<Value = FeedSpec> {
    (
        base(),
        words(),
        prop::collection::vec(link_spec(), 0..4),
        prop::collection::vec(entry_spec(), 0..6),
    )
        .prop_map(|(base, title, links, entries)| FeedSpec {
            base,
            title,
            links,
            entries,
        })
}
fn spelling() -> impl Strategy<Value = Spelling> {
    (
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
        prop::collection::vec(0u8..4, 1..8),
        any::<bool>(),
    )
        .prop_map(
            |(prefix, declaration, noise, indent, escapes, padding)| Spelling {
                prefix,
                declaration,
                noise,
                indent,
                escapes,
                padding,
            },
        )
}

// -------------------------------------------------------------------- writing

struct Writer<'a> {
    out: String,
    spelling: &'a Spelling,
    texts: usize,
}
impl Writer<'_> {
    fn text(&mut self, text: &str) -> String {
        let mode = self.spelling.escapes[self.texts % self.spelling.escapes.len()];
        self.texts += 1;
        let body = match mode {
            3 if !text.contains("]]>") => format!("<![CDATA[{text}]]>"),
            _ => text
                .chars()
                .map(|c| match (c, mode) {
                    ('&', 0 | 3) => "&amp;".to_owned(),
                    ('<', 0 | 3) => "&lt;".to_owned(),
                    ('>', 0 | 3) => "&gt;".to_owned(),
                    ('"', 0) => "&quot;".to_owned(),
                    ('\'', 0) => "&apos;".to_owned(),
                    ('&' | '<' | '>' | 'é' | '書' | '😀' | 'a', 1) => {
                        format!("&#{};", c as u32)
                    }
                    ('&' | '<' | '>' | 'é' | '書' | '😀' | 'b', 2) => {
                        format!("&#x{:X};", c as u32)
                    }
                    _ => c.to_string(),
                })
                .collect(),
        };
        if self.spelling.padding {
            format!("\n   {body}\t\n ")
        } else {
            body
        }
    }
    fn attribute(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('"', "&quot;")
    }
    fn name(&self, name: &str) -> String {
        if self.spelling.prefix {
            format!("atom:{name}")
        } else {
            name.to_owned()
        }
    }
    fn gap(&mut self) {
        if self.spelling.indent {
            self.out += "\n  ";
        }
        if self.spelling.noise {
            self.out += "<!-- <entry><title>comment</title></entry> -->";
        }
    }
    fn element(&mut self, name: &str, text: &str) {
        let (name, text) = (self.name(name), self.text(text));
        self.out += &format!("<{name}>{text}</{name}>");
        self.gap();
    }
    fn link(&mut self, link: &LinkSpec) {
        let name = self.name("link");
        self.out += &format!("<{name}");
        if let Some(relations) = &link.relations {
            self.out += &format!(" rel=\"{}\"", relations.join(" "));
        }
        self.out += &format!(" href=\"{}\"", Self::attribute(&link.reference.text()));
        if let Some(media) = link.media {
            self.out += &format!(" type='{media}'");
        }
        if let Some(title) = &link.title {
            self.out += &format!(" title=\"{}\"", Self::attribute(title));
        }
        self.out += if self.spelling.noise { "></" } else { "/>" };
        if self.spelling.noise {
            self.out += &format!("{name}>");
        }
        self.gap();
    }
    /// Elements a real feed carries that the reader has no use for.
    fn unused(&mut self) {
        if self.spelling.noise {
            self.out += "<updated>2026-01-01T00:00:00Z</updated><category term=\"x\" label=\"Other title\"/>";
            self.out +=
                "<content type=\"text\">Some <![CDATA[<b>content</b>]]> &amp; more</content>";
            self.out += "<source><title>Elsewhere</title><id>urn:other</id><link href=\"/elsewhere\" rel=\"http://opds-spec.org/acquisition\"/></source>";
        }
    }
}
fn atom(feed: &FeedSpec, spelling: &Spelling) -> String {
    let mut w = Writer {
        out: String::new(),
        spelling,
        texts: 0,
    };
    if spelling.declaration {
        w.out += "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n";
    }
    let feed_name = w.name("feed");
    w.out += &format!(
        "<{feed_name} xmlns{}=\"http://www.w3.org/2005/Atom\" xmlns:dc=\"http://purl.org/dc/terms/\">",
        if spelling.prefix { ":atom" } else { "" }
    );
    w.gap();
    w.element("id", "urn:feed");
    w.element("title", &feed.title);
    w.unused();
    for link in &feed.links {
        w.link(link);
    }
    for entry in &feed.entries {
        let name = w.name("entry");
        w.out += &format!("<{name}>");
        w.gap();
        w.element("title", &entry.title);
        w.unused();
        for (dublin, identifier) in &entry.identifiers {
            if *dublin {
                let text = w.text(identifier);
                w.out += &format!("<dc:identifier>{text}</dc:identifier>");
            } else {
                w.element("id", identifier);
            }
        }
        for author in &entry.authors {
            let (outer, inner, text) = (w.name("author"), w.name("name"), w.text(author));
            w.out += &format!(
                "<{outer}><{inner}>{text}</{inner}><uri>http://x.example/</uri></{outer}>"
            );
            w.gap();
        }
        for link in &entry.links {
            w.link(link);
        }
        w.out += &format!("</{name}>");
        w.gap();
    }
    w.out += &format!("</{feed_name}>");
    if spelling.indent {
        w.out += "\n";
    }
    w.out
}

// ------------------------------------------------------------------- checking

fn indirect(link: &LinkSpec) -> bool {
    link.relations.iter().flatten().any(|relation| {
        relation
            .strip_prefix(ACQUISITION)
            .is_some_and(|kind| !kind.is_empty() && kind != "/open-access")
    })
}
fn check_link(
    actual: &Link,
    expected: &LinkSpec,
    base: &Base,
    atom: bool,
) -> Result<(), TestCaseError> {
    let href = expected.reference.text();
    prop_assert_eq!(&actual.href, &href);
    let resolved = resolve(base, &expected.reference);
    prop_assert_eq!(
        actual.resolved.as_deref(),
        Some(&resolved[..]),
        "{} against {}",
        href,
        base.url()
    );
    prop_assert_eq!(actual.media_type.as_deref(), expected.media);
    prop_assert_eq!(&actual.title, &expected.title);
    let relations: Vec<&str> = match &expected.relations {
        Some(relations) => relations.clone(),
        // An Atom link without a relation is an alternate (RFC 4287 4.2.7.2).
        None if atom => vec!["alternate"],
        None => vec![],
    };
    prop_assert_eq!(
        actual
            .relations
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        relations.clone()
    );
    prop_assert!(!actual.templated);
    // Only an EPUB that can be had outright is offered for download.
    // OPDS 2 marks indirect acquisition with a link property, not a relation.
    let direct = expected.media == Some("application/epub+zip")
        && !(atom && indirect(expected))
        && relations.iter().any(|r| {
            matches!(
                *r,
                "download"
                    | "http://opds-spec.org/acquisition"
                    | "http://opds-spec.org/acquisition/open-access"
            )
        });
    prop_assert_eq!(actual.direct_epub(), direct.then_some(&resolved[..]));
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1500))]

    /// Every entry of a generated Atom feed comes back as what OPDS 1 says it
    /// is, with its text and links, however the document is spelled.
    #[test]
    fn atom_feeds_round_trip(feed in feed_spec(), spelling in spelling()) {
        let document = atom(&feed, &spelling);
        let source = feed.base.url();
        let catalog = Catalog::parse(document.as_bytes(), &source)
            .map_err(|error| TestCaseError::fail(format!("{error}: {document}")))?;
        prop_assert_eq!(&catalog.title, &feed.title, "{}", document);
        prop_assert_eq!(&catalog.source, &source);
        prop_assert_eq!(catalog.links.len(), feed.links.len());
        for (actual, expected) in catalog.links.iter().zip(&feed.links) {
            check_link(actual, expected, &feed.base, true)?;
        }
        let acquisition = |link: &LinkSpec| link.relations.iter().flatten().any(|r| r.starts_with(ACQUISITION));
        let image = |link: &&LinkSpec| link.relations.iter().flatten().any(|r| r.starts_with("http://opds-spec.org/image"));
        let (mut publications, mut navigation) = (catalog.publications.iter(), catalog.navigation.iter());
        for entry in &feed.entries {
            if entry.links.iter().any(acquisition) {
                let actual = publications.next().ok_or_else(|| TestCaseError::fail("missing publication"))?;
                prop_assert_eq!(&actual.title, &entry.title, "{}", document);
                prop_assert_eq!(&actual.authors, &entry.authors, "{}", document);
                prop_assert_eq!(actual.identifier.as_ref(), entry.identifiers.first().map(|(_, id)| id));
                let images: Vec<_> = entry.links.iter().filter(image).collect();
                let links: Vec<_> = entry.links.iter().filter(|link| !image(link)).collect();
                prop_assert_eq!(actual.images.len(), images.len());
                prop_assert_eq!(actual.links.len(), links.len());
                for (actual, expected) in actual.images.iter().zip(images).chain(actual.links.iter().zip(links)) {
                    check_link(actual, expected, &feed.base, true)?;
                    prop_assert_eq!(actual.indirect, indirect(expected));
                }
            } else if let Some(link) = entry.links.iter().find(|link| link.media.is_some_and(|m| m.starts_with("application/atom+xml"))) {
                let actual = navigation.next().ok_or_else(|| TestCaseError::fail("missing navigation"))?;
                prop_assert_eq!(actual.title.as_ref(), Some(&entry.title), "{}", document);
                check_link(actual, &LinkSpec { title: Some(entry.title.clone()), ..link.clone() }, &feed.base, true)?;
            }
        }
        prop_assert!(publications.next().is_none() && navigation.next().is_none());
        prop_assert!(catalog.groups.is_empty() && catalog.facets.is_empty());
    }

    /// A feed cut short anywhere is refused, never read as a shorter feed.
    #[test]
    fn truncated_atom_feeds_are_refused(feed in feed_spec(), spelling in spelling(), cuts in prop::collection::vec(any::<prop::sample::Index>(), 24)) {
        let document = atom(&feed, &spelling);
        let end = document.trim_end().len();
        // Every cut near the end, and a sample of the rest.
        for cut in cuts.iter().map(|cut| cut.index(end)).chain(end.saturating_sub(24)..end) {
            prop_assert!(Catalog::parse(&document.as_bytes()[..cut], &feed.base.url()).is_err(), "accepted: {}", String::from_utf8_lossy(&document.as_bytes()[..cut]));
        }
    }

    /// Damage to a feed gives an error or a catalog within the limits.
    #[test]
    fn damaged_atom_feeds_never_panic(
        feed in feed_spec(), spelling in spelling(),
        edits in prop::collection::vec((any::<prop::sample::Index>(), prop::sample::select(b"<>/&;\"'= ![]-?x\x00\xff#".to_vec()), 0u8..4, any::<prop::sample::Index>()), 1..5),
    ) {
        let mut bytes = atom(&feed, &spelling).into_bytes();
        for (index, byte, kind, other) in edits {
            let at = index.index(bytes.len());
            match kind {
                0 => bytes[at] = byte,
                1 => bytes.insert(at, byte),
                2 => { bytes.remove(at); }
                // Repeat a stretch of the document elsewhere.
                _ => {
                    let from = other.index(bytes.len());
                    let piece = bytes[from..(from + 40).min(bytes.len())].to_vec();
                    bytes.splice(at..at, piece);
                }
            }
        }
        if let Ok(catalog) = Catalog::parse(&bytes, &feed.base.url()) {
            prop_assert!(catalog.title.chars().count() <= 1024 && !catalog.title.is_empty());
            for link in catalog.links.iter().chain(&catalog.navigation).chain(catalog.publications.iter().flat_map(|p| p.links.iter().chain(&p.images))) {
                if let Some(resolved) = &link.resolved {
                    prop_assert!(resolved.starts_with("http://") || resolved.starts_with("https://"));
                }
            }
        }
    }

    #[test]
    fn arbitrary_markup_never_panics(
        pieces in prop::collection::vec(prop_oneof![
            prop::sample::select(vec!["<feed>", "</feed>", "<entry>", "</entry>", "<entry/>", "<title>", "</title>", "<link href='a' rel='http://opds-spec.org/acquisition'/>", "<link href=", "<author>", "<name>", "</name>", "</author>", "&amp;", "&#x0;", "&#xD800;", "&#99999999999;", "&nbsp;", "&", "<![CDATA[", "]]>", "<!--", "-->", "<?xml", "?>", "<!DOCTYPE x [<!ENTITY a 'aaaa'>]>", "&a;", "text", " ", "\u{feff}", "<a:b c='d'>", "</a:b>"]).prop_map(str::to_owned),
            "\\PC{0,4}",
        ], 0..30),
    ) {
        let document = format!("<{}", pieces.concat());
        let _ = Catalog::parse(document.as_bytes(), "https://catalog.example/feed");
        let _ = Catalog::parse(&document.as_bytes()[1..], "https://catalog.example/feed");
        let _ = femto_catalog::search::described_url(document.as_bytes(), "https://catalog.example/feed", "q");
    }

    /// OPDS 2: the same links through the JSON reader. A link the reader
    /// cannot follow stays in the model without a resolved URL.
    #[test]
    fn json_feeds_round_trip(
        feed in feed_spec(),
        unsupported in prop::collection::vec((any::<prop::sample::Index>(), prop::sample::select(vec![
            "mailto:reader@example.org", "ftp://files.example/book.epub", "data:image/png;base64,AAAA",
            "https://user:secret@catalog.example/book.epub", "https://user@catalog.example/", "urn:isbn:9780000000000", "javascript:alert(1)", "file:///etc/passwd",
        ])), 0..3),
    ) {
        let source = feed.base.url();
        let link = |link: &LinkSpec, href: Option<&str>| {
            let mut value = json!({"href": href.map(str::to_owned).unwrap_or_else(|| link.reference.text())});
            if let Some(relations) = &link.relations {
                value["rel"] = if relations.len() == 1 { json!(relations[0]) } else { json!(relations) };
            }
            if let Some(media) = link.media { value["type"] = json!(media); }
            if let Some(title) = &link.title { value["title"] = json!(title); }
            value
        };
        // An empty href is not a link in OPDS 2.
        let usable = |spec: &&LinkSpec| !spec.reference.text().is_empty();
        let publications: Vec<&EntrySpec> = feed.entries.iter().collect();
        let mut bad = vec![None; publications.len()];
        for (index, href) in &unsupported {
            if !bad.is_empty() { let at = index.index(bad.len()); bad[at] = Some(*href); }
        }
        let document = json!({
            "metadata": {"title": feed.title},
            "links": std::iter::once(json!({"rel": "self", "href": source})).chain(feed.links.iter().filter(usable).map(|l| link(l, None))).collect::<Vec<_>>(),
            "publications": publications.iter().zip(&bad).map(|(entry, bad)| json!({
                "metadata": {"title": entry.title, "author": entry.authors, "identifier": entry.identifiers.first().map(|(_, id)| id.clone()).unwrap_or_else(|| "urn:x".into())},
                "links": entry.links.iter().filter(usable).map(|l| link(l, None)).chain(bad.map(|href| json!({"href": href, "rel": "http://opds-spec.org/acquisition/open-access", "type": "application/epub+zip"}))).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        });
        let catalog = Catalog::parse(document.to_string().as_bytes(), &source)
            .map_err(|error| TestCaseError::fail(format!("{error}: {document}")))?;
        prop_assert_eq!(&catalog.title, &feed.title);
        let expected: Vec<_> = feed.links.iter().filter(usable).collect();
        prop_assert_eq!(catalog.links.len(), expected.len() + 1);
        for (actual, expected) in catalog.links[1..].iter().zip(expected) {
            check_link(actual, expected, &feed.base, false)?;
        }
        prop_assert_eq!(catalog.publications.len(), publications.len());
        for ((actual, entry), bad) in catalog.publications.iter().zip(publications).zip(bad) {
            prop_assert_eq!(&actual.title, &entry.title);
            prop_assert_eq!(&actual.authors, &entry.authors);
            let expected: Vec<_> = entry.links.iter().filter(usable).collect();
            prop_assert_eq!(actual.links.len(), expected.len() + usize::from(bad.is_some()));
            for (actual, expected) in actual.links.iter().zip(expected) {
                check_link(actual, expected, &feed.base, false)?;
            }
            if let Some(href) = bad {
                let actual = actual.links.last().unwrap();
                prop_assert_eq!(&actual.href, href);
                prop_assert_eq!(&actual.resolved, &None);
                prop_assert_eq!(actual.direct_epub(), None);
            }
        }
    }
}

/// docs/catalogs.md: "Only HTTP and HTTPS links without embedded credentials
/// receive resolved URLs. Unsupported or credential-bearing links retain their
/// original fields with a null resolved URL", and OPDS 1 feeds "are read into
/// the same model". OPDS 1.2 section 5.4.1 lets a thumbnail be a `data:` URL.
#[test]
#[ignore = "bug: one mailto:, data: or other unsupported link makes a whole OPDS 1 feed unreadable"]
fn an_atom_feed_with_an_unsupported_link_is_still_read() {
    let source = "https://catalog.example/opds";
    for href in [
        "mailto:librarian@example.org",
        "data:image/png;base64,iVBORw0KGgo=",
        "ftp://files.example/book.epub",
        "https://user:secret@catalog.example/book.epub",
    ] {
        let feed = format!(
            "<feed><title>Books</title><entry><title>A book</title>\
             <link rel=\"related\" href=\"{href}\"/>\
             <link rel=\"http://opds-spec.org/acquisition\" type=\"application/epub+zip\" href=\"/a.epub\"/>\
             </entry></feed>"
        );
        let catalog = Catalog::parse(feed.as_bytes(), source)
            .unwrap_or_else(|error| panic!("{href}: {error}"));
        let links = &catalog.publications[0].links;
        assert_eq!(links[0].href, href);
        assert_eq!(links[0].resolved, None);
        assert_eq!(
            links[1].direct_epub(),
            Some("https://catalog.example/a.epub")
        );
        // The JSON reader already behaves this way.
        let json = json!({"metadata": {"title": "Books"}, "publications": [],
            "links": [{"rel": "self", "href": source}, {"href": href}]});
        let catalog = Catalog::parse(json.to_string().as_bytes(), source).unwrap();
        assert_eq!(catalog.links[1].resolved, None);
    }
}
