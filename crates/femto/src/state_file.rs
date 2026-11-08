//! Advisory ownership of a host state pathname across atomic file replacement.
use anyhow::{Context, Result};
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub struct StateFile {
    path: PathBuf,
    _lock: File,
}
impl StateFile {
    pub fn acquire(path: &Path) -> Result<Self> {
        let name = path.file_name().context("state path requires a filename")?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let parent = fs::canonicalize(parent)?;
        let path = parent.join(name);
        let mut lock_name = name.to_os_string();
        lock_name.push(".femto-lock");
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(parent.join(lock_name))
            .context("open state lock")?;
        let metadata = lock.metadata()?;
        anyhow::ensure!(
            metadata.is_file() && metadata.nlink() == 1,
            "state lock must be a regular file with one link"
        );
        fs2::FileExt::try_lock_exclusive(&lock)
            .context("state file is in use or cannot be locked")?;
        match fs::symlink_metadata(&path) {
            Ok(metadata) => anyhow::ensure!(
                metadata.is_file() && metadata.nlink() == 1,
                "state must be a regular file with one link; symlinks are unsupported"
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        // The sidecar stays in place: unlinking it could create two independently locked inodes.
        Ok(Self { path, _lock: lock })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lock_survives_state_replacement_and_parent_aliases() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let alias = directory.path().join("alias");
        std::os::unix::fs::symlink(directory.path(), &alias).unwrap();
        let first = StateFile::acquire(&path).unwrap();
        femto_reader::state::ReaderState::default()
            .save(first.path())
            .unwrap();
        assert!(StateFile::acquire(&alias.join("state.json")).is_err());
        femto_reader::state::ReaderState::default()
            .save(first.path())
            .unwrap();
        assert!(StateFile::acquire(&path).is_err());
        drop(first);
        assert!(StateFile::acquire(&path).is_ok());
    }
    #[test]
    fn locks_reject_state_aliases_and_nonregular_sidecars() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let target = directory.path().join("target");
        fs::write(&target, b"{}").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(StateFile::acquire(&path).is_err());
        fs::remove_file(&path).unwrap();
        fs::hard_link(&target, &path).unwrap();
        assert!(StateFile::acquire(&path).is_err());
        fs::remove_file(&path).unwrap();
        let lock = directory.path().join("state.json.femto-lock");
        fs::remove_file(&lock).unwrap();
        std::os::unix::fs::symlink(&target, &lock).unwrap();
        assert!(StateFile::acquire(&path).is_err());
        fs::remove_file(&lock).unwrap();
        fs::create_dir(&lock).unwrap();
        assert!(StateFile::acquire(&path).is_err());
    }
}
