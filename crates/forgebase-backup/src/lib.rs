//! ForgeBase backup, restore, snapshots, export and import (FB-83).
//!
//! * [`backup`]: an online consistent copy of the data directory (database via
//!   `VACUUM INTO`, storage blobs, derived indexes, optionally the master key)
//!   written atomically with a hashed `MANIFEST.json`.
//! * [`verify`](verify::verify): manifest, hashes and database integrity of a backup.
//! * [`restore`](restore::restore): verified restore into a stopped data
//!   directory; the replaced data is kept, an interrupted restore is rolled back.
//! * [`SnapshotStore`]: timestamped snapshots with retention and point-in-time
//!   lookup.
//! * [`transfer`]: export and import of collections and configuration with
//!   ids preserved.
//! * [`extension`]: the admin REST API under `/api/v1/system/backups`.
//!
//! **Master key.** `auth/master.key` seals every stored secret. It is *not*
//! copied into a backup unless `include_master_key` is set; the manifest
//! records its fingerprint and restore refuses a different key.

pub mod backup;
pub mod error;
mod fsutil;
pub mod http;
pub mod manifest;
pub mod restore;
pub mod snapshots;
pub mod transfer;
pub mod verify;

use std::path::PathBuf;

use forgebase_core::DataLayout;
use forgebase_db::Database;

pub use backup::BackupOptions;
pub use error::{BackupError, Result};
pub use http::extension;
pub use manifest::Manifest;
pub use restore::{RestoreOptions, RestoreReport};
pub use snapshots::{Retention, Snapshot, SnapshotStore};
pub use verify::{VerifyReport, verify};

/// Handle the REST API and the server use to take and manage snapshots.
#[derive(Debug, Clone)]
pub struct BackupService {
    db: Database,
    layout: DataLayout,
    store: SnapshotStore,
}

impl BackupService {
    /// A service for the data directory `layout` backed by `db`.
    pub fn new(db: Database, layout: DataLayout) -> Self {
        let store = SnapshotStore::new(&layout);
        Self { db, layout, store }
    }

    /// The snapshot store.
    pub fn store(&self) -> &SnapshotStore {
        &self.store
    }

    /// Where snapshots live.
    pub fn snapshot_dir(&self) -> PathBuf {
        self.store.dir().to_path_buf()
    }

    /// Takes a snapshot now and applies `retention`.
    pub async fn snapshot(&self, opts: BackupOptions, retention: Option<Retention>) -> Result<Manifest> {
        self.store.create(&self.db, &self.layout, opts, retention).await
    }
}
