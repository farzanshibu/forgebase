//! On-disk layout of a ForgeBase data directory.
//!
//! ```text
//! <data_dir>/
//! ├── forgebase.db        primary database (canonical data), plus -wal / -shm
//! ├── storage/            object blobs (canonical data, Phase 4)
//! ├── indexes/            derived, rebuildable index files (safe to delete)
//! ├── backups/            automatic pre-migration snapshots
//! ├── tmp/                staging area for in-flight writes (safe to clean)
//! └── .forgebase.lock     held by a running server (single process per data dir)
//! ```
//!
//! Canonical data (database + storage) must be backed up. Derived data
//! (`indexes/`) can always be rebuilt from canonical data and is never the
//! only copy of anything.

use std::io;
use std::path::{Path, PathBuf};

/// File name of the primary database inside the data directory.
pub const DATABASE_FILE: &str = "forgebase.db";

/// Paths of every well-known entry inside a data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataLayout {
    root: PathBuf,
}

impl DataLayout {
    /// Layout rooted at `root`. Does not touch the filesystem.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The data directory itself.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Primary SQLite database file.
    pub fn database_path(&self) -> PathBuf {
        self.root.join(DATABASE_FILE)
    }

    /// SQLite write-ahead log of the primary database.
    pub fn database_wal_path(&self) -> PathBuf {
        self.root.join(format!("{DATABASE_FILE}-wal"))
    }

    /// Object storage directory (canonical blobs).
    pub fn storage_dir(&self) -> PathBuf {
        self.root.join("storage")
    }

    /// Derived, rebuildable index files.
    pub fn indexes_dir(&self) -> PathBuf {
        self.root.join("indexes")
    }

    /// Automatic pre-migration snapshots.
    pub fn backups_dir(&self) -> PathBuf {
        self.root.join("backups")
    }

    /// Staging area for in-flight writes.
    pub fn tmp_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }

    /// Lock file held by a running server.
    pub fn lock_path(&self) -> PathBuf {
        self.root.join(".forgebase.lock")
    }

    /// All directories that [`DataLayout::ensure`] creates, root first.
    pub fn directories(&self) -> [PathBuf; 5] {
        [
            self.root.clone(),
            self.storage_dir(),
            self.indexes_dir(),
            self.backups_dir(),
            self.tmp_dir(),
        ]
    }

    /// Creates every directory of the layout that is missing.
    ///
    /// New directories are created owner-only (`0700`) on Unix, because the
    /// data directory holds the whole database. Existing directories keep
    /// their permissions; `forgebase doctor` warns about loose ones.
    pub fn ensure(&self) -> io::Result<()> {
        for dir in self.directories() {
            create_private_dir(&dir)?;
        }
        Ok(())
    }

    /// Whether the primary database file exists.
    pub fn is_initialized(&self) -> bool {
        self.database_path().is_file()
    }
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if path.is_dir() {
        return Ok(());
    }
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_creates_every_directory_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().join("data"));
        assert!(!layout.is_initialized());
        layout.ensure().unwrap();
        layout.ensure().unwrap();
        for dir in layout.directories() {
            assert!(dir.is_dir(), "{} missing", dir.display());
        }
        assert_eq!(layout.database_path(), tmp.path().join("data/forgebase.db"));
    }

    #[cfg(unix)]
    #[test]
    fn new_directories_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().join("data"));
        layout.ensure().unwrap();
        let mode = std::fs::metadata(layout.root()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }
}
