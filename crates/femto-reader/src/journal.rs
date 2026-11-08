//! Single-owner state slots for filesystems without replacement rename.
use crate::{Error, Result, state::ReaderState};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

const LIMIT: usize = 4 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"FEMTOST1";

pub struct StateJournal {
    directory: PathBuf,
    generation: u64,
    active: Option<usize>,
    failed: bool,
}

impl StateJournal {
    /// The caller must exclude concurrent owners and keep the filesystem mounted.
    pub fn open(directory: impl AsRef<Path>) -> Result<(Self, ReaderState)> {
        let directory = directory.as_ref().to_path_buf();
        let (generation, active, state) =
            load_slots(|slot| read_slot(&directory.join(format!("state{slot}.bin"))))?;
        Ok((
            Self {
                directory,
                generation,
                active,
                failed: false,
            },
            state,
        ))
    }

    pub fn save(&mut self, state: &ReaderState) -> Result<()> {
        if self.failed {
            return Err(Error::Document(
                "state journal requires reopen after write failure".into(),
            ));
        }
        validate(state)?;
        let payload = state.encode(false)?;
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(Error::Limit("state generation"))?;
        let slot = self.active.map_or(0, |slot| 1 - slot);
        let generation_bytes = generation.to_le_bytes();
        let mut checksum = Sha256::new();
        checksum.update(MAGIC);
        checksum.update(generation_bytes);
        checksum.update(&payload);
        let digest = checksum.finalize();
        self.failed = true;
        fs::create_dir_all(&self.directory)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(self.directory.join(format!("state{slot}.bin")))?;
        file.write_all(MAGIC)?;
        file.write_all(&generation_bytes)?;
        file.write_all(&payload)?;
        file.write_all(&digest)?;
        file.sync_all()?;
        self.generation = generation;
        self.active = Some(slot);
        self.failed = false;
        Ok(())
    }
}

fn validate(state: &ReaderState) -> Result<()> {
    if state.version != ReaderState::default().version {
        return Err(Error::Document("unsupported state version".into()));
    }
    state.layout.validate()?;
    state.text_raster.validate()?;
    state.frontlight.validate()?;
    state.validate_books()
}

type Slot = (u64, ReaderState, [u8; 32]);

fn load_slots(
    mut read: impl FnMut(usize) -> Result<Option<Slot>>,
) -> Result<(u64, Option<usize>, ReaderState)> {
    let mut found = Vec::new();
    let mut invalid = false;
    for slot in 0..2 {
        match read(slot) {
            Ok(Some((generation, _state, digest))) => found.push((generation, slot, digest)),
            Ok(None) => (),
            Err(Error::Io(error)) => return Err(error.into()),
            Err(_) => invalid = true,
        }
    }
    found.sort_by_key(|(generation, _, _)| *generation);
    if found.len() == 2 && found[0].0 == found[1].0 {
        return Err(Error::Document("ambiguous state generations".into()));
    }
    let Some((generation, slot, digest)) = found.pop() else {
        return if invalid {
            Err(Error::Document("no valid state slot".into()))
        } else {
            Ok((0, None, ReaderState::default()))
        };
    };
    match read(slot)? {
        Some((again, state, checksum)) if again == generation && checksum == digest => {
            Ok((generation, Some(slot), state))
        }
        _ => Err(Error::Document("state slot changed during open".into())),
    }
}

fn read_slot(path: &Path) -> Result<Option<Slot>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    file.take((LIMIT + 49) as u64).read_to_end(&mut bytes)?;
    if bytes.len() < 48 || bytes.len() > LIMIT + 48 || &bytes[..8] != MAGIC {
        return Err(Error::Document("invalid state slot".into()));
    }
    let split = bytes.len() - 32;
    if Sha256::digest(&bytes[..split])[..] != bytes[split..] {
        return Err(Error::Document("state checksum mismatch".into()));
    }
    let generation = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    if generation == 0 {
        return Err(Error::Document("invalid state generation".into()));
    }
    let state: ReaderState = serde_json::from_slice(&bytes[16..split])?;
    validate(&state)?;
    Ok(Some((
        generation,
        state,
        bytes[split..].try_into().unwrap(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_reloads_only_the_newest_valid_slot() {
        let mut calls = Vec::new();
        let (generation, slot, state) = load_slots(|slot| {
            calls.push(slot);
            let mut state = ReaderState::default();
            state.layout.font_size = if slot == 0 { 20.0 } else { 30.0 };
            Ok(Some((slot as u64 + 1, state, [slot as u8; 32])))
        })
        .unwrap();
        assert_eq!(calls, [0, 1, 1]);
        assert_eq!(generation, 2);
        assert_eq!(slot, Some(1));
        assert_eq!(state.layout.font_size, 30.0);
    }

    #[test]
    fn changed_or_missing_winner_cannot_publish_a_mixed_snapshot() {
        for mode in 0..4 {
            let mut calls = 0;
            let result = load_slots(|slot| {
                calls += 1;
                if calls == 3 {
                    return match mode {
                        0 => Ok(None),
                        1 => Ok(Some((2, ReaderState::default(), [99; 32]))),
                        2 => Ok(Some((3, ReaderState::default(), [1; 32]))),
                        _ => Err(std::io::Error::other("read failure").into()),
                    };
                }
                Ok(Some((
                    slot as u64 + 1,
                    ReaderState::default(),
                    [slot as u8; 32],
                )))
            });
            assert!(result.is_err());
            assert_eq!(calls, 3);
        }
    }

    #[test]
    fn equal_valid_generations_remain_ambiguous_without_reloading() {
        let mut calls = 0;
        assert!(
            load_slots(|_| {
                calls += 1;
                Ok(Some((1, ReaderState::default(), [0; 32])))
            })
            .is_err()
        );
        assert_eq!(calls, 2);
    }
}
