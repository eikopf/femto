//! Bounded OPDS catalog projection, independent of network and reader frontends.
pub mod auth;
pub mod browser;
pub mod deadline;
pub mod http;
pub mod routing;
pub mod search;
#[cfg(all(feature = "tailnet", unix, not(target_os = "espidf")))]
pub mod tailnet;
pub mod worker;
use serde::Serialize;
use serde_json::Value;
use url::Url;

pub const MAX_FEED_BYTES: usize = 512 * 1024;
const MAX_LINKS: usize = 1024;
const MAX_PUBLICATIONS: usize = 256;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("catalog limit exceeded: {0}")]
    Limit(&'static str),
    #[error("invalid catalog field: {0}")]
    Field(&'static str),
    #[error("invalid catalog URL")]
    Url,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Serialize)]
pub struct Catalog {
    pub source: String,
    pub title: String,
    pub links: Vec<Link>,
    pub navigation: Vec<Link>,
    pub publications: Vec<Publication>,
    pub groups: Vec<Group>,
    pub facets: Vec<Facet>,
}
#[derive(Debug, Serialize)]
pub struct Group {
    pub title: String,
    pub links: Vec<Link>,
    pub navigation: Vec<Link>,
    pub publications: Vec<Publication>,
}
#[derive(Debug, Serialize)]
pub struct Facet {
    pub title: String,
    pub links: Vec<Link>,
}
#[derive(Debug, Serialize)]
pub struct Publication {
    pub title: String,
    pub identifier: Option<String>,
    pub authors: Vec<String>,
    pub links: Vec<Link>,
    pub images: Vec<Link>,
}
#[derive(Debug, Serialize)]
pub struct Link {
    pub href: String,
    pub resolved: Option<String>,
    pub title: Option<String>,
    pub media_type: Option<String>,
    pub relations: Vec<String>,
    pub templated: bool,
    pub indirect: bool,
}
impl Link {
    pub fn has_relation(&self, relation: &str) -> bool {
        self.relations.iter().any(|r| r == relation)
    }
    /// Only direct, explicitly unrestricted EPUB links are eligible for acquisition.
    pub fn direct_epub(&self) -> Option<&str> {
        if !self.indirect
            && !self.templated
            && self.media_type.as_deref().is_some_and(|t| {
                t.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("application/epub+zip")
            })
            && (self.has_relation("download")
                || self.has_relation("http://opds-spec.org/acquisition/open-access"))
        {
            self.resolved.as_deref()
        } else {
            None
        }
    }
}

fn web_url(base: Option<&Url>, href: &str) -> Result<Url> {
    if href.len() > 4096 || href.chars().any(char::is_control) || href.contains('\\') {
        return Err(Error::Url);
    }
    let url = match base {
        Some(base) => base.join(href),
        None => Url::parse(href),
    }
    .map_err(|_| Error::Url)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(Error::Url);
    }
    Ok(url)
}

// Bound the entire decoded tree, including extension properties omitted by the projection.
fn validate(value: &Value, depth: usize, nodes: &mut usize) -> Result<()> {
    *nodes += 1;
    if *nodes > 16384 || depth > 16 {
        return Err(Error::Limit("JSON tree"));
    }
    match value {
        Value::String(s) if s.len() > 4096 => return Err(Error::Limit("string")),
        Value::Array(items) => {
            for item in items {
                validate(item, depth + 1, nodes)?;
            }
        }
        Value::Object(items) => {
            for (key, value) in items {
                if key.len() > 256 {
                    return Err(Error::Limit("property name"));
                }
                validate(value, depth + 1, nodes)?;
            }
        }
        _ => (),
    }
    Ok(())
}
fn array<'a>(object: &'a Value, key: &'static str) -> Result<&'a [Value]> {
    match object.get(key) {
        None => Ok(&[]),
        Some(Value::Array(v)) => Ok(v),
        _ => Err(Error::Field(key)),
    }
}
fn string(value: &Value, key: &'static str) -> Result<String> {
    value
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or(Error::Field(key))
}
fn optional_string(value: Option<&Value>, key: &'static str) -> Result<Option<String>> {
    value.map(|v| string(v, key)).transpose()
}
fn text(value: &Value) -> Result<String> {
    if value.is_string() {
        return string(value, "title");
    }
    let map = value.as_object().ok_or(Error::Field("localized title"))?;
    if map.is_empty() || map.len() > 32 || map.values().any(|v| !v.is_string()) {
        return Err(Error::Field("localized title"));
    }
    string(
        map.get("und")
            .or_else(|| map.get("en"))
            .or_else(|| map.values().next())
            .unwrap(),
        "title",
    )
}
fn title(object: &Value) -> Result<String> {
    text(
        object
            .get("metadata")
            .and_then(|v| v.get("title"))
            .ok_or(Error::Field("metadata.title"))?,
    )
}

struct Parser {
    base: Url,
    links: usize,
    publications: usize,
}
impl Parser {
    fn links(&mut self, object: &Value, key: &'static str, titled: bool) -> Result<Vec<Link>> {
        let values = array(object, key)?;
        self.links += values.len();
        if self.links > MAX_LINKS {
            return Err(Error::Limit("links"));
        }
        values
            .iter()
            .map(|v| {
                let href = string(&v["href"], "href")?;
                let templated = match v.get("templated") {
                    None => false,
                    Some(Value::Bool(b)) => *b,
                    _ => return Err(Error::Field("templated")),
                };
                let resolved = if templated {
                    None
                } else {
                    web_url(Some(&self.base), &href).ok().map(String::from)
                };
                let relations = match v.get("rel") {
                    None => vec![],
                    Some(Value::String(s)) => vec![s.clone()],
                    Some(Value::Array(a)) if a.len() <= 16 => {
                        a.iter().map(|s| string(s, "rel")).collect::<Result<_>>()?
                    }
                    _ => return Err(Error::Field("rel")),
                };
                let title = v.get("title").map(text).transpose()?;
                if titled && title.is_none() {
                    return Err(Error::Field("navigation.title"));
                }
                Ok(Link {
                    href,
                    resolved,
                    title,
                    media_type: optional_string(v.get("type"), "type")?,
                    relations,
                    templated,
                    indirect: v
                        .get("properties")
                        .is_some_and(|p| p.get("indirectAcquisition").is_some()),
                })
            })
            .collect()
    }
    fn publications(&mut self, object: &Value) -> Result<Vec<Publication>> {
        let values = array(object, "publications")?;
        self.publications += values.len();
        if self.publications > MAX_PUBLICATIONS {
            return Err(Error::Limit("publications"));
        }
        values
            .iter()
            .map(|v| {
                let metadata = &v["metadata"];
                let raw = metadata.get("author");
                let authors = match raw {
                    None => vec![],
                    Some(Value::Array(a)) => a.iter().collect(),
                    Some(a) => vec![a],
                };
                if authors.len() > 32 {
                    return Err(Error::Limit("authors"));
                }
                let authors = authors
                    .into_iter()
                    .map(|a| {
                        if a.is_string() {
                            string(a, "author")
                        } else {
                            text(&a["name"])
                        }
                    })
                    .collect::<Result<_>>()?;
                Ok(Publication {
                    title: title(v)?,
                    identifier: optional_string(metadata.get("identifier"), "identifier")?,
                    authors,
                    links: self.links(v, "links", false)?,
                    images: self.links(v, "images", false)?,
                })
            })
            .collect()
    }
}

impl Catalog {
    pub fn parse(bytes: &[u8], source: &str) -> Result<Self> {
        if bytes.len() > MAX_FEED_BYTES {
            return Err(Error::Limit("feed bytes"));
        }
        let base = web_url(None, source)?;
        let value: Value = serde_json::from_slice(bytes)?;
        validate(&value, 0, &mut 0)?;
        if !value.is_object()
            || !["navigation", "publications", "groups"]
                .iter()
                .any(|k| value.get(k).is_some())
        {
            return Err(Error::Field("feed collection"));
        }
        let mut parser = Parser {
            base,
            links: 0,
            publications: 0,
        };
        let links = parser.links(&value, "links", false)?;
        if !links
            .iter()
            .any(|l| l.has_relation("self") && l.resolved.is_some() && !l.templated)
        {
            return Err(Error::Field("self link"));
        }
        let navigation = parser.links(&value, "navigation", true)?;
        let publications = parser.publications(&value)?;
        let raw_groups = array(&value, "groups")?;
        let raw_facets = array(&value, "facets")?;
        if raw_groups.len() > 32 || raw_facets.len() > 32 {
            return Err(Error::Limit("groups or facets"));
        }
        let groups = raw_groups
            .iter()
            .map(|v| {
                if v.get("groups").is_some()
                    || (!v.get("navigation").is_some_and(Value::is_array)
                        && !v.get("publications").is_some_and(Value::is_array))
                {
                    return Err(Error::Field("group collection"));
                }
                Ok(Group {
                    title: title(v)?,
                    links: parser.links(v, "links", false)?,
                    navigation: parser.links(v, "navigation", true)?,
                    publications: parser.publications(v)?,
                })
            })
            .collect::<Result<_>>()?;
        let facets = raw_facets
            .iter()
            .map(|v| {
                Ok(Facet {
                    title: title(v)?,
                    links: parser.links(v, "links", false)?,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            source: source.into(),
            title: title(&value)?,
            links,
            navigation,
            publications,
            groups,
            facets,
        })
    }
}
