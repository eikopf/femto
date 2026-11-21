//! OPDS 1.x Atom feeds, read into the same model as OPDS 2.
//!
//! Entries with acquisition links are publications; other entries are
//! navigation. Element namespaces are not checked: Atom, Dublin Core and
//! OPDS names used here do not collide by local name.
use crate::{Catalog, Error, Link, MAX_LINKS, MAX_PUBLICATIONS, Publication, Result, web_url};
use quick_xml::{
    escape::resolve_predefined_entity,
    events::{BytesStart, Event},
    reader::Reader,
};
use url::Url;

const MAX_DEPTH: usize = 32;
const MAX_TEXT_BYTES: usize = 4096;
const MAX_NAVIGATION: usize = 1024;
pub(crate) const ACQUISITION: &str = "http://opds-spec.org/acquisition";

fn text_limit(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    text.chars()
        .filter(|c| !c.is_control())
        .take(1024)
        .collect()
}

#[derive(Default)]
struct Entry {
    title: String,
    identifier: Option<String>,
    authors: Vec<String>,
    links: Vec<Link>,
}

struct Parser {
    base: Url,
    links: usize,
}

impl Parser {
    fn link(&mut self, element: &BytesStart<'_>) -> Result<Option<Link>> {
        let (mut href, mut rel, mut media, mut title) = (None, None, None, None);
        for attribute in element.attributes() {
            let attribute = attribute.map_err(|_| Error::Field("feed XML"))?;
            let value = attribute
                .unescape_value()
                .map_err(|_| Error::Field("feed XML"))?;
            if value.len() > MAX_TEXT_BYTES {
                return Err(Error::Limit("link attribute"));
            }
            match attribute.key.local_name().as_ref() {
                b"href" => href = Some(value.into_owned()),
                b"rel" => rel = Some(value.into_owned()),
                b"type" => media = Some(value.into_owned()),
                b"title" => title = Some(text_limit(&value)),
                _ => {}
            }
        }
        let Some(href) = href else {
            return Ok(None);
        };
        self.links += 1;
        if self.links > MAX_LINKS {
            return Err(Error::Limit("links"));
        }
        // OpenSearch templates are not URLs until expanded.
        let templated = href.contains('{');
        let resolved = if templated {
            None
        } else {
            Some(web_url(Some(&self.base), &href)?.to_string())
        };
        let relations: Vec<String> = rel
            .as_deref()
            .unwrap_or("alternate")
            .split_whitespace()
            .map(str::to_owned)
            .collect();
        // Buying, borrowing, subscribing and samples are not direct downloads.
        let indirect = relations.iter().any(|relation| {
            relation
                .strip_prefix(ACQUISITION)
                .is_some_and(|kind| !matches!(kind, "" | "/open-access"))
        });
        Ok(Some(Link {
            href,
            resolved,
            title,
            media_type: media,
            relations,
            templated,
            indirect,
        }))
    }
}

pub(crate) fn parse(bytes: &[u8], base: Url, source: &str) -> Result<Catalog> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Field("feed encoding"))?;
    let mut reader = Reader::from_str(text);
    let mut parser = Parser { base, links: 0 };
    let mut catalog = Catalog {
        source: source.into(),
        title: String::new(),
        links: Vec::new(),
        navigation: Vec::new(),
        publications: Vec::new(),
        groups: Vec::new(),
        facets: Vec::new(),
    };
    // Local names of the open elements, and the text of the innermost one.
    let mut path: Vec<Vec<u8>> = Vec::new();
    let mut content = String::new();
    let mut entry: Option<Entry> = None;
    let mut seen_feed = false;
    loop {
        let event = reader.read_event().map_err(|_| Error::Field("feed XML"))?;
        let event = match event {
            Event::Start(element) => Ok((element, false)),
            Event::Empty(element) => Ok((element, true)),
            other => Err(other),
        };
        match event {
            Ok((element, empty)) => {
                let name = element.local_name().as_ref().to_vec();
                if path.is_empty() {
                    if name != b"feed" {
                        return Err(Error::Field("feed collection"));
                    }
                    seen_feed = true;
                }
                match (path.len(), name.as_slice()) {
                    (1, b"entry") => entry = Some(Entry::default()),
                    (1, b"link") => catalog.links.extend(parser.link(&element)?),
                    (2, b"link") => {
                        if let (Some(entry), Some(link)) = (&mut entry, parser.link(&element)?) {
                            entry.links.push(link);
                        }
                    }
                    _ => {}
                }
                content.clear();
                if !empty {
                    if path.len() == MAX_DEPTH {
                        return Err(Error::Limit("feed depth"));
                    }
                    path.push(name);
                }
            }
            Err(Event::Text(part)) => {
                let part = part.decode().map_err(|_| Error::Field("feed XML"))?;
                if content.len() + part.len() <= MAX_TEXT_BYTES {
                    content.push_str(&part);
                }
            }
            Err(Event::CData(part)) => {
                let part = part.decode().map_err(|_| Error::Field("feed XML"))?;
                if content.len() + part.len() <= MAX_TEXT_BYTES {
                    content.push_str(&part);
                }
            }
            Err(Event::GeneralRef(reference)) => {
                let resolved = match reference
                    .resolve_char_ref()
                    .map_err(|_| Error::Field("feed XML"))?
                {
                    Some(character) => Some(character.to_string()),
                    None => reference
                        .decode()
                        .ok()
                        .and_then(|name| resolve_predefined_entity(&name).map(str::to_owned)),
                };
                // Unknown entities are dropped rather than expanded.
                if let Some(resolved) = resolved
                    && content.len() + resolved.len() <= MAX_TEXT_BYTES
                {
                    content.push_str(&resolved);
                }
            }
            Err(Event::End(_)) => {
                let Some(name) = path.pop() else {
                    return Err(Error::Field("feed XML"));
                };
                let value = text_limit(&content);
                content.clear();
                match (path.len(), name.as_slice(), &mut entry) {
                    (1, b"title", None) => catalog.title = value,
                    (2, b"title", Some(entry)) => entry.title = value,
                    (2, b"id" | b"identifier", Some(entry)) if entry.identifier.is_none() => {
                        entry.identifier = (!value.is_empty()).then_some(value);
                    }
                    (3, b"name", Some(entry)) if path[2] == b"author" && !value.is_empty() => {
                        if entry.authors.len() < 16 {
                            entry.authors.push(value);
                        }
                    }
                    (1, b"entry", slot) => {
                        if let Some(entry) = slot.take() {
                            finish(entry, &mut catalog)?;
                        }
                    }
                    _ => {}
                }
            }
            Err(Event::Eof) => break,
            Err(_) => {}
        }
    }
    if !seen_feed || !path.is_empty() {
        return Err(Error::Field("feed collection"));
    }
    if catalog.title.is_empty() {
        catalog.title = "Catalog".into();
    }
    Ok(catalog)
}

/// The results-feed URL template of an OpenSearch description document.
pub(crate) fn search_template(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::Field("search description"))?;
    let mut reader = Reader::from_str(text);
    loop {
        let element = match reader
            .read_event()
            .map_err(|_| Error::Field("search description"))?
        {
            Event::Start(element) | Event::Empty(element) => element,
            Event::Eof => break,
            _ => continue,
        };
        if element.local_name().as_ref() != b"Url" {
            continue;
        }
        let (mut template, mut atom) = (None, false);
        for attribute in element.attributes() {
            let attribute = attribute.map_err(|_| Error::Field("search description"))?;
            let value = attribute
                .unescape_value()
                .map_err(|_| Error::Field("search description"))?;
            match attribute.key.local_name().as_ref() {
                b"template" if value.len() <= MAX_TEXT_BYTES => template = Some(value.into_owned()),
                b"type" => atom = value.trim_start().starts_with("application/atom+xml"),
                _ => {}
            }
        }
        match template {
            Some(template) if atom => return Ok(template),
            _ => {}
        }
    }
    Err(Error::Field("search description"))
}

fn finish(entry: Entry, catalog: &mut Catalog) -> Result<()> {
    let title = if entry.title.is_empty() {
        "Untitled".to_string()
    } else {
        entry.title
    };
    let acquisition = |link: &Link| link.relations.iter().any(|r| r.starts_with(ACQUISITION));
    if entry.links.iter().any(acquisition) {
        if catalog.publications.len() == MAX_PUBLICATIONS {
            return Err(Error::Limit("publications"));
        }
        let (images, links) = entry.links.into_iter().partition(|link| {
            link.relations
                .iter()
                .any(|relation| relation.starts_with("http://opds-spec.org/image"))
        });
        catalog.publications.push(Publication {
            title,
            identifier: entry.identifier,
            authors: entry.authors,
            links,
            images,
        });
        return Ok(());
    }
    // A navigation entry leads to another feed.
    let feed = |link: &&Link| {
        link.media_type
            .as_deref()
            .is_some_and(|media| media.trim_start().starts_with("application/atom+xml"))
    };
    if let Some(link) = entry.links.iter().find(feed) {
        if catalog.navigation.len() == MAX_NAVIGATION {
            return Err(Error::Limit("navigation"));
        }
        catalog.navigation.push(Link {
            href: link.href.clone(),
            resolved: link.resolved.clone(),
            title: Some(title),
            media_type: link.media_type.clone(),
            relations: link.relations.clone(),
            templated: link.templated,
            indirect: link.indirect,
        });
    }
    Ok(())
}
