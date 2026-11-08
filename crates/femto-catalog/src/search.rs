//! Bounded OPDS keyword search using RFC 6570 template expansion.
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
    /// Expand one keyword search link against the final feed response URL.
    pub fn search_url(&self, source: &str, query: &str) -> Result<String> {
        if query.len() > MAX_QUERY_BYTES {
            return Err(Error::Limit("search query"));
        }
        if query.trim().is_empty() || query.chars().any(char::is_control) {
            return Err(Error::Field("search query"));
        }
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
