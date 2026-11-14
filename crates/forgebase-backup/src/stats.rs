//! Record and object counts of a database (ADR-0041).
//!
//! A backup stores them in its manifest ([`BackupStats`]); a restore drill
//! counts the restored database again and compares.

use std::collections::BTreeMap;

use forgebase_db::{Backend, Database, Statement, Value};

use crate::error::{BackupError, Result};
use crate::manifest::{BackupStats, CountRange};

/// One counting pass over a database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    /// Records per table-backed collection.
    pub collections: BTreeMap<String, u64>,
    /// Rows of the storage object table.
    pub storage_objects: u64,
    /// Rows of the storage blob table.
    pub storage_blobs: u64,
}

async fn scalar(db: &Database, sql: &str) -> Result<u64> {
    let rows = db
        .fetch_all(&Statement::new(sql, vec![]))
        .await
        .map_err(|e| BackupError::Internal(format!("{sql}: {e}")))?;
    Ok(rows
        .scalar()
        .and_then(Value::as_i64)
        .and_then(|n| u64::try_from(n).ok())
        .unwrap_or(0))
}

/// Counts the records of every table-backed collection and the storage
/// tables of `db`. A collection whose table cannot be counted is left out.
///
/// # Errors
/// The catalog or a storage table cannot be read.
pub async fn collect(db: &Database) -> Result<Counts> {
    let postgres = db.dialect().name() == "postgres";
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT name FROM _forgebase_collections WHERE type <> 'view' ORDER BY name",
            vec![],
        ))
        .await
        .map_err(|e| BackupError::Internal(format!("collections cannot be listed: {e}")))?;
    let mut out = Counts::default();
    for row in rows.rows {
        let Some(name) = row.first().and_then(Value::as_str) else {
            continue;
        };
        let quoted = db.dialect().quote_ident(name);
        let target = if postgres { format!("public.{quoted}") } else { quoted };
        if let Ok(n) = scalar(db, &format!("SELECT COUNT(*) FROM {target}")).await {
            out.collections.insert(name.to_owned(), n);
        }
    }
    out.storage_objects = scalar(db, "SELECT COUNT(*) FROM _forgebase_storage_objects")
        .await
        .unwrap_or(0);
    out.storage_blobs = scalar(db, "SELECT COUNT(*) FROM _forgebase_storage_blobs")
        .await
        .unwrap_or(0);
    Ok(out)
}

impl Counts {
    /// The counts as an exact [`BackupStats`].
    pub fn exact(&self) -> BackupStats {
        self.window(self)
    }

    /// The range two passes span (`self` first, `after` last).
    pub fn window(&self, after: &Counts) -> BackupStats {
        let mut collections = BTreeMap::new();
        for (name, &a) in &self.collections {
            if let Some(&b) = after.collections.get(name) {
                collections.insert(name.clone(), CountRange::span(a, b));
            }
        }
        BackupStats {
            collections,
            storage_objects: CountRange::span(self.storage_objects, after.storage_objects),
            storage_blobs: CountRange::span(self.storage_blobs, after.storage_blobs),
        }
    }
}

/// Differences between what a backup recorded and what a restore holds.
/// Empty when they agree.
pub fn compare(expected: &BackupStats, got: &Counts) -> Vec<String> {
    let mut diffs = Vec::new();
    for (name, range) in &expected.collections {
        match got.collections.get(name) {
            None => diffs.push(format!("collection {name} is missing after the restore")),
            Some(n) if !range.contains(*n) => diffs.push(format!(
                "collection {name} has {n} record(s), the backup recorded {}",
                range.describe()
            )),
            Some(_) => {}
        }
    }
    if !expected.storage_objects.contains(got.storage_objects) {
        diffs.push(format!(
            "{} storage object(s) after the restore, the backup recorded {}",
            got.storage_objects,
            expected.storage_objects.describe()
        ));
    }
    if !expected.storage_blobs.contains(got.storage_blobs) {
        diffs.push(format!(
            "{} storage blob(s) after the restore, the backup recorded {}",
            got.storage_blobs,
            expected.storage_blobs.describe()
        ));
    }
    diffs
}
