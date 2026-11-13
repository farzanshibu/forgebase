//! ForgeBase backup, restore, snapshots, export and import (FB-83).
//!
//! * [`backup`]: an online consistent copy of the data directory (database via
//!   `VACUUM INTO`, storage blobs, derived indexes, optionally the master key)
//!   written atomically with a hashed `MANIFEST.json`.
//! * [`verify`](verify::verify): manifest, hashes and database integrity of a backup.
//! * [`restore`](restore::restore): verified restore into a stopped data
//!   directory; the replaced data is kept, an interrupted restore is rolled back.
//! * [`pg`]: the PostgreSQL side (FB-150): `pg_basebackup` backups of the bundled
//!   engine, `pg_dump` backups of an external server, `pg_verifybackup`.
//! * [`topology`]: backups and restores of a cluster with replicas and shards (FB-179):
//!   one backup per set under one directory, a combined manifest, replica sources,
//!   replicas rebuilt and schemas reconciled after a restore.
//! * [`pre_migration`]: the snapshot taken before a schema change on PostgreSQL.
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
pub mod pg;
pub mod pre_migration;
pub mod restore;
pub mod snapshots;
pub mod topology;
pub mod transfer;
pub mod verify;

use std::path::PathBuf;

use forgebase_core::DataLayout;
use forgebase_db::Database;

pub use backup::BackupOptions;
pub use error::{BackupError, Result};
pub use http::extension;
pub use manifest::Manifest;
pub use pg::{DumpPlan, Password, PgEndpoint, PgSource};
pub use restore::{RestoreOptions, RestoreReport};
pub use snapshots::{Retention, Snapshot, SnapshotStore};
pub use verify::{VerifyOptions, VerifyReport, verify, verify_with};

/// Handle the REST API and the server use to take and manage snapshots.
#[derive(Debug, Clone)]
pub struct BackupService {
    db: Database,
    layout: DataLayout,
    store: SnapshotStore,
    pg: Option<PgSource>,
}

impl BackupService {
    /// A service for the data directory `layout` backed by `db`.
    pub fn new(db: Database, layout: DataLayout) -> Self {
        let store = SnapshotStore::new(&layout);
        Self {
            db,
            layout,
            store,
            pg: None,
        }
    }

    /// Gives the service the engine's tools and the way into the PostgreSQL
    /// cluster (required to take snapshots on PostgreSQL).
    #[must_use]
    pub fn with_pg(mut self, pg: PgSource) -> Self {
        self.pg = Some(pg);
        self
    }

    /// The snapshot store.
    pub fn store(&self) -> &SnapshotStore {
        &self.store
    }

    /// Where snapshots live.
    pub fn snapshot_dir(&self) -> PathBuf {
        self.store.dir().to_path_buf()
    }

    /// Verifies the backup at `dir` with the engine's tools when the service
    /// has them (`pg_verifybackup` or `pg_restore --list` on PostgreSQL), like
    /// `forgebase backup verify`.
    pub async fn verify(&self, dir: &std::path::Path) -> VerifyReport {
        verify_with(
            dir,
            &VerifyOptions {
                pg: self.pg.clone(),
                tmp: Some(self.layout.tmp_dir()),
            },
        )
        .await
    }

    /// Takes a snapshot now and applies `retention`.
    pub async fn snapshot(&self, mut opts: BackupOptions, retention: Option<Retention>) -> Result<Manifest> {
        if opts.pg.is_none() {
            opts.pg.clone_from(&self.pg);
        }
        self.store.create(&self.db, &self.layout, opts, retention).await
    }
}
