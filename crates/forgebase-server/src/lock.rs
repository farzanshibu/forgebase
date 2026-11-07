//! One server per data directory.
//!
//! A running server holds an exclusive OS file lock on
//! `<data_dir>/.forgebase.lock` (`flock` on Unix, `LockFileEx` on Windows) and
//! writes its pid and address into the file. The OS releases the lock when
//! the process exits, even on a crash, so a stale file never blocks a restart.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

use forgebase_core::DataLayout;

use crate::ServerError;

/// Who holds the lock, as recorded in the lock file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockOwner {
    /// Process id.
    pub pid: u32,
    /// Listening address.
    pub addr: String,
    /// RFC 3339 start time.
    pub started_at: String,
}

impl LockOwner {
    fn encode(&self) -> String {
        format!("pid={}\naddr={}\nstarted_at={}\n", self.pid, self.addr, self.started_at)
    }

    fn decode(text: &str) -> Option<Self> {
        let get = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix('=')))
                .map(str::to_owned)
        };
        Some(Self {
            pid: get("pid")?.parse().ok()?,
            addr: get("addr")?,
            started_at: get("started_at").unwrap_or_default(),
        })
    }
}

/// The held lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct DataDirLock {
    file: File,
    path: PathBuf,
}

impl DataDirLock {
    /// Takes the lock or fails with [`ServerError::Locked`].
    pub fn acquire(layout: &DataLayout) -> Result<Self, ServerError> {
        let path = layout.lock_path();
        let io = |source| ServerError::Io {
            path: path.clone(),
            source,
        };
        let mut open = OpenOptions::new();
        open.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(0o600);
        }
        let file = open.open(&path).map_err(io)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { file, path }),
            Err(TryLockError::WouldBlock) => Err(ServerError::Locked {
                path: layout.root().to_path_buf(),
                owner: read_owner(&path),
            }),
            Err(TryLockError::Error(e)) => Err(io(e)),
        }
    }

    /// Records the holder in the lock file (best effort; the lock itself is
    /// what matters).
    pub fn record_owner(&self, owner: &LockOwner) {
        let mut f = &self.file;
        let written = f
            .set_len(0)
            .and_then(|()| f.seek(SeekFrom::Start(0)))
            .and_then(|_| f.write_all(owner.encode().as_bytes()))
            .and_then(|()| f.sync_data());
        if let Err(e) = written {
            tracing::debug!(error = %e, path = %self.path.display(), "could not record lock owner");
        }
    }

    /// Whether a live process holds the lock right now, and who.
    /// Never blocks and never leaves the lock held.
    pub fn probe(layout: &DataLayout) -> Option<Option<LockOwner>> {
        let path = layout.lock_path();
        let file = OpenOptions::new().read(true).write(true).open(&path).ok()?;
        match file.try_lock() {
            Ok(()) => {
                let _ = file.unlock();
                None
            }
            Err(TryLockError::WouldBlock) => Some(read_owner(&path)),
            Err(TryLockError::Error(_)) => None,
        }
    }
}

impl Drop for DataDirLock {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
        let _ = self.file.unlock();
    }
}

fn read_owner(path: &std::path::Path) -> Option<LockOwner> {
    let mut text = String::new();
    File::open(path).ok()?.read_to_string(&mut text).ok()?;
    LockOwner::decode(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_is_exclusive_and_released_on_drop() {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path());
        let lock = DataDirLock::acquire(&layout).unwrap();
        let owner = LockOwner {
            pid: 42,
            addr: "127.0.0.1:1".into(),
            started_at: "t".into(),
        };
        lock.record_owner(&owner);
        assert_eq!(DataDirLock::probe(&layout), Some(Some(owner.clone())));
        match DataDirLock::acquire(&layout) {
            Err(ServerError::Locked { owner: Some(o), .. }) => assert_eq!(o, owner),
            other => panic!("expected Locked, got {other:?}"),
        }
        drop(lock);
        assert_eq!(DataDirLock::probe(&layout), None);
        DataDirLock::acquire(&layout).unwrap();
    }
}
