//! The pre-migration snapshot on PostgreSQL (ADR-0028 §7.6, FB-150).
//!
//! `Database::migrate` calls [`PgPreMigration`] before it applies a schema
//! change to a database that already has data. The snapshot is a
//! database-only physical backup (`pg_basebackup` plus `MANIFEST.json`, no
//! storage or indexes) in `backups/pre-migrate-v<N>-<UTC time>/`; `forgebase
//! restore <that directory> --force` puts the database back and leaves files
//! and keys alone.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use forgebase_core::DataLayout;
use forgebase_db::{DbError, PreMigrationSnapshot};

use crate::backup::{self, BackupOptions};
use crate::error::BackupError;
use crate::pg::PgSource;

/// Takes pre-migration snapshots of the bundled engine's cluster.
#[derive(Debug, Clone)]
pub struct PgPreMigration {
    layout: DataLayout,
    source: PgSource,
}

impl PgPreMigration {
    /// A provider for the data directory `layout`, reaching the cluster
    /// through `source` (the bundled engine; external servers have none).
    pub fn new(layout: DataLayout, source: PgSource) -> Arc<dyn PreMigrationSnapshot> {
        Arc::new(Self { layout, source })
    }
}

/// `pre-migrate-v<N>-<time>` below `backups_dir`.
pub fn snapshot_dir(backups_dir: &Path, from_version: i64) -> PathBuf {
    let stamp = forgebase_core::now_rfc3339().replace([':', '-'], "");
    backups_dir.join(format!("pre-migrate-v{from_version}-{stamp}"))
}

impl PreMigrationSnapshot for PgPreMigration {
    fn take<'a>(
        &'a self,
        backups_dir: &'a Path,
        from_version: i64,
    ) -> Pin<Box<dyn Future<Output = Result<PathBuf, DbError>> + Send + 'a>> {
        Box::pin(async move {
            let dest = snapshot_dir(backups_dir, from_version);
            let opts = BackupOptions {
                include_indexes: false,
                kind: "pre-migration".to_owned(),
                label: Some(format!("before the migration from schema v{from_version}")),
                pg: Some(self.source.clone()),
                database_only: true,
                note: Some("pre-migration".to_owned()),
                ..BackupOptions::default()
            };
            backup::create_physical(&self.layout, &dest, from_version, opts)
                .await
                .map_err(|e: BackupError| {
                    DbError::Unavailable(format!(
                        "the pre-migration snapshot failed, so nothing was migrated: {e}"
                    ))
                })?;
            Ok(dest)
        })
    }
}
