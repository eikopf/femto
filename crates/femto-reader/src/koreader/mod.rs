//! KOReader EPUB positions. Text offsets count Unicode scalar values.
mod mapper;
mod text;
pub(crate) use mapper::{decode, decode_image, encode, encode_image};
pub use text::{TextNode, text_nodes};

use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub name: String,
    pub index: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XPointer {
    pub(crate) steps: Vec<Step>,
    pub(crate) offset: Option<usize>,
}
/// The pointer to a chapter's body, for a chapter with nothing in it that a
/// finer pointer could name.
pub(crate) fn chapter_start(chapter: usize) -> String {
    format!("/body/DocFragment[{}]/body", chapter + 1)
}

impl XPointer {
    /// Accepts both implicit first indexes and current explicit `[1]` indexes.
    pub fn parse(value: &str) -> Result<Self> {
        if value.len() > 8192 || !value.starts_with('/') {
            return Err(invalid());
        }
        let (path, offset) = match value.rsplit_once('.') {
            Some((path, offset)) if offset.bytes().all(|b| b.is_ascii_digit()) => {
                (path, Some(number(offset)?))
            }
            _ => (value, None),
        };
        let mut steps = Vec::new();
        for part in path[1..].split('/') {
            if steps.len() >= 128
                || steps
                    .last()
                    .is_some_and(|step: &Step| step.name == "text()")
            {
                return Err(invalid());
            }
            let (name, index) = if let Some((name, index)) = part.split_once('[') {
                (name, number(index.strip_suffix(']').ok_or_else(invalid)?)?)
            } else {
                (part, 1)
            };
            if index == 0
                || name.is_empty()
                || name.len() > 256
                || (name != "text()"
                    && !name
                        .chars()
                        .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | ':')))
            {
                return Err(invalid());
            }
            steps.push(Step {
                name: name.into(),
                index,
            });
        }
        if steps.len() < 3
            || steps[0]
                != (Step {
                    name: "body".into(),
                    index: 1,
                })
            || steps[1].name != "DocFragment"
            || steps[2]
                != (Step {
                    name: "body".into(),
                    index: 1,
                })
        {
            return Err(invalid());
        }
        Ok(Self { steps, offset })
    }
    pub fn chapter(&self) -> usize {
        self.steps[1].index - 1
    }
    pub(crate) fn is_image(&self) -> bool {
        self.steps
            .last()
            .is_some_and(|step| matches!(step.name.as_str(), "img" | "image"))
    }
}
impl std::fmt::Display for XPointer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for step in &self.steps {
            write!(f, "/{}[{}]", step.name, step.index)?;
        }
        if let Some(offset) = self.offset {
            write!(f, ".{offset}")?;
        }
        Ok(())
    }
}
fn number(text: &str) -> Result<usize> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    text.parse().map_err(|_| invalid())
}
fn invalid() -> Error {
    Error::Document("invalid KOReader EPUB position".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_and_current_indexes_refer_to_the_same_nodes() {
        let old = XPointer::parse("/body/DocFragment[2]/body/p[3]/text().17").unwrap();
        let current = XPointer::parse("/body[1]/DocFragment[2]/body[1]/p[3]/text()[1].17").unwrap();
        assert_eq!(old, current);
        assert_eq!(old.chapter(), 1);
        assert_eq!(XPointer::parse(&old.to_string()).unwrap(), old);
    }
    #[test]
    fn malformed_or_unbounded_paths_are_rejected() {
        for value in [
            "42",
            "/",
            "/body/DocFragment[0]/body/p/text().0",
            "/body/DocFragment[1]/body/p/text().-1",
            "/body/DocFragment[1]/body/p/text().",
            "/body/DocFragment[1]/body/p/text()[1]/p.0",
            "/body/DocFragment[9999999999999999999999999999999999999]/body",
        ] {
            assert!(XPointer::parse(value).is_err(), "{value}");
        }
        assert!(
            XPointer::parse(&format!("/body/DocFragment[1]/body{}", "/p".repeat(130))).is_err()
        );
    }
}
