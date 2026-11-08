//! Transactional catalog navigation with bounded URL history.
use crate::{Catalog, Link, Publication};

pub const MAX_HISTORY: usize = 16;

pub struct Browser {
    current: Catalog,
    history: std::collections::VecDeque<String>,
}

pub struct Destination<'a> {
    pub label: &'a str,
    pub source: &'a str,
}

impl Browser {
    pub fn new(current: Catalog) -> Self {
        Self {
            current,
            history: Default::default(),
        }
    }
    pub fn into_current(self) -> Catalog {
        self.current
    }
    pub fn current(&self) -> &Catalog {
        &self.current
    }
    pub fn back_source(&self) -> Option<&str> {
        self.history.back().map(String::as_str)
    }
    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// The loader must return a validated feed. Failed loads leave all state intact.
    pub fn follow<E>(
        &mut self,
        source: &str,
        load: impl FnOnce(&str) -> Result<Catalog, E>,
    ) -> Result<(), E> {
        let next = load(source)?;
        if next.source != self.current.source {
            if self.history.len() == MAX_HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(self.current.source.clone());
        }
        self.current = next;
        Ok(())
    }
    /// History retains URLs only; back navigation fetches the feed again.
    pub fn back<E>(&mut self, load: impl FnOnce(&str) -> Result<Catalog, E>) -> Result<bool, E> {
        let Some(source) = self.history.back() else {
            return Ok(false);
        };
        let previous = load(source)?;
        self.history.pop_back();
        self.current = previous;
        Ok(true)
    }
    pub fn publications(&self) -> impl Iterator<Item = &Publication> {
        self.current
            .publications
            .iter()
            .chain(self.current.groups.iter().flat_map(|g| &g.publications))
    }
    pub fn searches(&self) -> impl Iterator<Item = &Link> {
        self.current
            .links
            .iter()
            .filter(|link| link.has_relation("search"))
    }
    pub fn search_url(&self, index: usize, query: &str) -> crate::Result<String> {
        self.searches()
            .nth(index)
            .ok_or(crate::Error::Field("search number"))?
            .search_url(&self.current.source, query)
    }
    pub fn destinations(&self) -> Vec<Destination<'_>> {
        fn destination(link: &Link) -> Option<Destination<'_>> {
            if link.templated || link.indirect {
                return None;
            }
            let kind = link
                .media_type
                .as_deref()
                .unwrap_or("application/opds+json")
                .split(';')
                .next()?
                .trim();
            if !kind.eq_ignore_ascii_case("application/opds+json") {
                return None;
            }
            Some(Destination {
                label: link.title.as_deref().unwrap_or_else(|| {
                    link.relations
                        .first()
                        .map(String::as_str)
                        .unwrap_or("Catalog")
                }),
                source: link.resolved.as_deref()?,
            })
        }
        self.current
            .navigation
            .iter()
            .chain(self.current.groups.iter().flat_map(|g| &g.navigation))
            .chain(self.current.facets.iter().flat_map(|f| &f.links))
            .chain(self.current.groups.iter().flat_map(|g| &g.links))
            .chain(self.current.links.iter().filter(|l| {
                ["next", "previous", "first", "last", "start", "up"]
                    .iter()
                    .any(|rel| l.has_relation(rel))
            }))
            .filter_map(destination)
            .collect()
    }
}
