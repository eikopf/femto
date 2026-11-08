use super::{Bindings, PageTurn};
use crate::{Error, Result};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, SeqAccess, Visitor},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAction {
    Previous,
    Next,
}

impl From<RemoteAction> for PageTurn {
    fn from(value: RemoteAction) -> Self {
        match value {
            RemoteAction::Previous => Self::Previous,
            RemoteAction::Next => Self::Next,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteBinding {
    pub usage: u32,
    pub action: RemoteAction,
}

const EMPTY: RemoteBinding = RemoteBinding {
    usage: 0,
    action: RemoteAction::Next,
};

/// A validated custom map. An empty map disables remote page turns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedBindings {
    entries: [RemoteBinding; 16],
    len: usize,
}

impl Default for SavedBindings {
    fn default() -> Self {
        let mut entries = [EMPTY; 16];
        let defaults = Bindings::default();
        for (target, &(usage, action)) in entries.iter_mut().zip(defaults.entries()) {
            *target = RemoteBinding {
                usage,
                action: match action {
                    PageTurn::Previous => RemoteAction::Previous,
                    PageTurn::Next => RemoteAction::Next,
                },
            };
        }
        Self {
            entries,
            len: defaults.entries().len(),
        }
    }
}

impl SavedBindings {
    pub fn new(entries: &[RemoteBinding]) -> Result<Self> {
        if entries.len() > 16 {
            return Err(Error::Limit("remote bindings"));
        }
        let mut result = Self {
            entries: [EMPTY; 16],
            len: entries.len(),
        };
        result.entries[..entries.len()].copy_from_slice(entries);
        result.bindings()?;
        Ok(result)
    }

    /// Cycles one usage through previous, next, and unbound without partial mutation.
    pub fn cycle(&mut self, usage: u32) -> Result<()> {
        let mut next = self.clone();
        if let Some(index) = next.entries().iter().position(|entry| entry.usage == usage) {
            match next.entries[index].action {
                RemoteAction::Previous => next.entries[index].action = RemoteAction::Next,
                RemoteAction::Next => {
                    next.entries.copy_within(index + 1..next.len, index);
                    next.len -= 1;
                    next.entries[next.len] = EMPTY;
                }
            }
        } else {
            if next.len == next.entries.len() {
                return Err(Error::Limit("remote bindings"));
            }
            next.entries[next.len] = RemoteBinding {
                usage,
                action: RemoteAction::Previous,
            };
            next.len += 1;
        }
        next.bindings()?;
        *self = next;
        Ok(())
    }

    pub fn entries(&self) -> &[RemoteBinding] {
        &self.entries[..self.len]
    }

    pub(super) fn bindings(&self) -> Result<Bindings> {
        let mut entries = [(0, PageTurn::Next); 16];
        for (target, source) in entries.iter_mut().zip(self.entries()) {
            *target = (source.usage, source.action.into());
        }
        Bindings::new(&entries[..self.len]).map_err(super::remote_error)
    }
}

impl Serialize for SavedBindings {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        self.entries().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SavedBindings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct BoundedBindings;
        impl<'de> Visitor<'de> for BoundedBindings {
            type Value = SavedBindings;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("at most 16 remote button bindings")
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> std::result::Result<SavedBindings, A::Error> {
                let mut entries = [EMPTY; 16];
                let mut len = 0;
                while let Some(entry) = sequence.next_element::<RemoteBinding>()? {
                    if len == entries.len() {
                        return Err(de::Error::custom("remote binding limit"));
                    }
                    entries[len] = entry;
                    len += 1;
                }
                SavedBindings::new(&entries[..len]).map_err(de::Error::custom)
            }
        }
        deserializer.deserialize_seq(BoundedBindings)
    }
}
