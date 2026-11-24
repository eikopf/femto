//! Bounded OPDS keyword search: RFC 6570 templates in OPDS 2, OpenSearch in OPDS 1.
use crate::{Error, Link, Result, web_url};
use iri_string::{
    spec::UriSpec,
    template::{
        UriTemplateStr,
        context::{Context, Visitor},
    },
};
use std::fmt::{self, Write};

pub const MAX_QUERY_BYTES: usize = 256;
const MAX_URL_BYTES: usize = 4096;

const OPENSEARCH: &str = "application/opensearchdescription+xml";

/// Where a keyword search leads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Search {
    /// The results feed.
    Feed(String),
    /// An OpenSearch description document that names the results feed.
    Described { description: String, query: String },
}

fn check_query(query: &str) -> Result<()> {
    if query.len() > MAX_QUERY_BYTES {
        return Err(Error::Limit("search query"));
    }
    if query.trim().is_empty() || query.chars().any(char::is_control) {
        return Err(Error::Field("search query"));
    }
    Ok(())
}

/// Expand an OpenSearch URL template against the document that carried it.
pub fn opensearch_url(template: &str, source: &str, query: &str) -> Result<String> {
    check_query(query)?;
    let mut output = String::new();
    let mut rest = template;
    let mut searched = false;
    while let Some((before, after)) = rest.split_once('{') {
        output.push_str(before);
        let (parameter, after) = after
            .split_once('}')
            .ok_or(Error::Field("search template"))?;
        let (name, optional) = match parameter.strip_suffix('?') {
            Some(name) => (name, true),
            None => (parameter, false),
        };
        match name {
            "searchTerms" => {
                // In a path, a segment of dots names another resource, and
                // percent-encoding the dots does not change that.
                if matches!(query, "." | "..") && !output.contains('?') {
                    return Err(Error::Field("search query"));
                }
                searched = true;
                for byte in query.bytes() {
                    if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                        output.push(byte as char);
                    } else {
                        write!(output, "%{byte:02X}").map_err(|_| Error::Field("search URL"))?;
                    }
                }
            }
            _ if optional => {}
            "startPage" | "startIndex" => output.push('1'),
            "inputEncoding" | "outputEncoding" => output.push_str("UTF-8"),
            "language" => output.push('*'),
            _ => return Err(Error::Field("search template parameter")),
        }
        if output.len() > MAX_URL_BYTES {
            return Err(Error::Limit("search URL"));
        }
        rest = after;
    }
    output.push_str(rest);
    if !searched {
        return Err(Error::Field("search query variable"));
    }
    let base = web_url(None, source)?;
    let mut url = web_url(Some(&base), &output)?;
    url.set_fragment(None);
    if url.as_str().len() > MAX_URL_BYTES {
        return Err(Error::Limit("search URL"));
    }
    Ok(url.into())
}

/// Expand the template named by a fetched OpenSearch description.
pub fn described_url(description: &[u8], source: &str, query: &str) -> Result<String> {
    opensearch_url(&crate::atom::search_template(description)?, source, query)
}

struct Keyword<'a>(&'a str);
impl Context for Keyword<'_> {
    fn visit<V: Visitor>(&self, visitor: V) -> V::Result {
        if visitor.var_name().as_str() == "query" {
            visitor.visit_string(self.0)
        } else {
            visitor.visit_undefined()
        }
    }
}
struct Bounded(String);
impl Write for Bounded {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        if value.len() > MAX_URL_BYTES - self.0.len() {
            return Err(fmt::Error);
        }
        self.0.push_str(value);
        Ok(())
    }
}

impl Link {
    /// Resolve a keyword search as far as possible without the network.
    pub fn search(&self, source: &str, query: &str) -> Result<Search> {
        check_query(query)?;
        let kind = self
            .media_type
            .as_deref()
            .and_then(|kind| kind.split(';').next())
            .unwrap_or("")
            .trim();
        if !self.has_relation("search") || self.indirect {
            return Err(Error::Field("search link"));
        }
        if self.href.contains("{searchTerms") {
            return opensearch_url(&self.href, source, query).map(Search::Feed);
        }
        if kind.eq_ignore_ascii_case(OPENSEARCH) {
            return Ok(Search::Described {
                description: self.resolved.clone().ok_or(Error::Field("search link"))?,
                query: query.to_owned(),
            });
        }
        self.search_url(source, query).map(Search::Feed)
    }
    /// Expand one OPDS 2 search template against the final feed response URL.
    pub fn search_url(&self, source: &str, query: &str) -> Result<String> {
        check_query(query)?;
        if !self.has_relation("search")
            || !self.templated
            || self.indirect
            || !self.media_type.as_deref().is_some_and(|kind| {
                kind.split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .eq_ignore_ascii_case("application/opds+json")
            })
        {
            return Err(Error::Field("search link"));
        }
        if self.href.len() > MAX_URL_BYTES {
            return Err(Error::Limit("search template"));
        }
        let template =
            UriTemplateStr::new(&self.href).map_err(|_| Error::Field("search template"))?;
        if !template.variables().any(|v| v.as_str() == "query") {
            return Err(Error::Field("search query variable"));
        }
        let context = Keyword(query);
        let expanded = template
            .expand::<UriSpec, _>(&context)
            .map_err(|_| Error::Field("search expansion"))?;
        let mut output = Bounded(String::new());
        write!(&mut output, "{expanded}").map_err(|_| Error::Limit("search URL"))?;
        let base = web_url(None, source)?;
        let url = web_url(Some(&base), &output.0)?;
        if url.as_str().len() > MAX_URL_BYTES {
            return Err(Error::Limit("search URL"));
        }
        // A fragment is never sent to the server. Duplicate or truncated keywords are ambiguous.
        let mut keywords = url.query_pairs().filter(|(key, _)| key == "query");
        if url.fragment().is_some()
            || keywords.next().is_none_or(|(_, value)| value != query)
            || keywords.next().is_some()
        {
            return Err(Error::Field("search query parameter"));
        }
        Ok(url.into())
    }
}
