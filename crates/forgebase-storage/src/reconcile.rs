//! The reconciler: brings the driver and the metadata back in line after
//! crashes, dropped connections and abandoned uploads.
//!
//! It removes, each only after the grace period (`reconcile_grace_secs`) so
//! that work in flight is never touched:
//!
//! * expired upload sessions and their staged parts;
//! * staged files no session or upload refers to;
//! * blobs without metadata (a crash between placing a blob and committing
//!   its object version);
//! * blobs no object version references any more (reference count zero).
//!
//! Blobs the metadata names but the driver lacks are reported, never
//! "repaired". Blob deletion holds the write side of the blob gate, so it
//! cannot interleave with an upload placing a blob.
//!
//! **Database sets** (FB-172). The job is instance-wide (it runs once, on
//! control) and walks every set of the router. Sessions are per set: each
//! set drops its own expired sessions, skipping the tenants a move excludes
//! there ([`forgebase_jobs::excluded_tenants`]). Staged and blob files are
//! shared by all sets, so their fate is decided over all of them: a staged
//! file is kept while any set's part names it, and a blob file is deleted
//! only when no set references it: no set holds a reference count above
//! zero or a zero count younger than the grace period, and no object of an
//! excluded (moving) tenant on any set names it (a move's copy has no
//! reference counts until they are rebuilt on the target). Every set is
//! read before anything is deleted; a set that cannot be read fails the run
//! (the job retries) without deleting a blob.

use std::collections::{BTreeSet, HashMap, HashSet};

use forgebase_data::{AccessContext, Action};
use forgebase_db::Value;
use forgebase_jobs::{HandlerError, JobContext, JobRegistry};

use crate::error::Result;
use crate::model::ReconcileReport;
use crate::service::{Db, RECONCILE_JOB, StorageService, int, st, text};

/// What the sets hold about one blob.
#[derive(Default)]
struct BlobRefs {
    /// Sets (indexes) with a row, and whether that row may be collected
    /// (reference count zero since before the cutoff).
    rows: Vec<(usize, bool)>,
    /// Named by an object of an excluded tenant on some set.
    pinned: bool,
}

impl BlobRefs {
    /// No set references the blob (an entry has a row or is pinned).
    fn collectable(&self) -> bool {
        !self.pinned && self.rows.iter().all(|(_, ok)| *ok)
    }
}

/// `(sql, params)` for the expired sessions on a set, skipping the
/// sessions of `excluded` tenants' buckets. Without exclusions the
/// statement of a single database.
fn expired_sessions(now: i64, excluded: &[String]) -> (String, Vec<Value>) {
    let mut params = vec![Value::from(now)];
    if excluded.is_empty() {
        return (
            "SELECT id FROM _forgebase_storage_uploads WHERE expires_at <= ?".to_owned(),
            params,
        );
    }
    // Sessions have no tenant column: their bucket row has it. A session
    // whose bucket row is not there (a move's copy in progress) is skipped.
    let guard = forgebase_jobs::tenant_guard("tenant", excluded, &mut params, 0);
    (
        format!(
            "SELECT id FROM _forgebase_storage_uploads WHERE expires_at <= ?1 \
             AND bucket IN (SELECT name FROM _forgebase_storage_buckets WHERE 1 = 1{guard})"
        ),
        params,
    )
}

impl StorageService {
    /// Runs the reconciler (needs the `storage_admin` platform permission).
    pub async fn reconcile(&self, ctx: &AccessContext) -> Result<ReconcileReport> {
        self.authorize_platform(ctx, Action::StorageAdmin)?;
        self.reconcile_now().await
    }

    /// Runs the reconciler without an authorization check (the job handler).
    /// With a router, over every set (see the module documentation).
    pub async fn reconcile_now(&self) -> Result<ReconcileReport> {
        let mut report = ReconcileReport::default();
        let now = self.now();
        let grace = i64::try_from(self.inner.settings.reconcile_grace_secs).unwrap_or(i64::MAX / 2000) * 1000;
        let cutoff = now - grace;
        let driver = &self.inner.driver;
        let sets = self.all_sets();

        // 1. Expired sessions, on each set.
        for (db, excluded) in &sets {
            let (sql, params) = expired_sessions(now, excluded);
            let expired = db.read(&sql, params).await?;
            for r in &expired.rows {
                self.drop_session(db, &text(r, 0)).await?;
                report.uploads_aborted += 1;
            }
        }

        // 2. Staged files no set's part refers to.
        let mut referenced: HashSet<String> = HashSet::new();
        for (db, _) in &sets {
            let parts = db
                .read("SELECT staging_key FROM _forgebase_storage_parts", vec![])
                .await?;
            referenced.extend(parts.rows.iter().map(|r| text(r, 0)));
        }
        for prefix in ["staging/", ".tmp/"] {
            for obj in driver.list(prefix).await? {
                if !referenced.contains(&obj.key) && obj.modified_ms <= cutoff {
                    driver.delete(&obj.key).await?;
                    report.staging_deleted += 1;
                }
            }
        }

        // 3. Blobs, with uploads kept out.
        let _gate = self.inner.gate.write().await;
        let mut known: HashMap<String, BlobRefs> = HashMap::new();
        let mut live: BTreeSet<String> = BTreeSet::new();
        for (i, (db, excluded)) in sets.iter().enumerate() {
            let rows = db
                .read(
                    "SELECT sha256, refcount, zero_since FROM _forgebase_storage_blobs",
                    vec![],
                )
                .await?;
            for r in &rows.rows {
                let (refs, since) = (int(r, 1), r.get(2).and_then(Value::as_i64));
                if refs > 0 {
                    live.insert(text(r, 0));
                }
                let collectable = refs == 0 && since.is_some_and(|s| s <= cutoff);
                known.entry(text(r, 0)).or_default().rows.push((i, collectable));
            }
            if !excluded.is_empty() {
                let mut params = Vec::new();
                let guard = forgebase_jobs::tenant_guard("tenant", excluded, &mut params, 0);
                let pinned = db
                    .read(
                        &format!(
                            "SELECT DISTINCT blob_sha FROM _forgebase_storage_objects WHERE blob_sha IS NOT NULL \
                             AND bucket NOT IN (SELECT name FROM _forgebase_storage_buckets WHERE 1 = 1{guard})"
                        ),
                        params,
                    )
                    .await?;
                for r in &pinned.rows {
                    known.entry(text(r, 0)).or_default().pinned = true;
                }
            }
        }
        let mut present = HashSet::new();
        for obj in driver.list("blobs/").await? {
            let sha = obj.key.rsplit('/').next().unwrap_or_default().to_owned();
            present.insert(sha.clone());
            match known.get(&sha) {
                None => {
                    if obj.modified_ms <= cutoff {
                        driver.delete(&obj.key).await?;
                        report.orphan_blobs_deleted += 1;
                    }
                }
                Some(refs) if refs.collectable() => {
                    driver.delete(&obj.key).await?;
                    for (i, _) in &refs.rows {
                        drop_zero_row(&sets[*i].0, &sha).await?;
                    }
                    report.unreferenced_blobs_deleted += 1;
                }
                Some(_) => {}
            }
        }
        for (sha, refs) in &known {
            if present.contains(sha) || refs.pinned {
                continue;
            }
            // Nothing is stored: rows nothing references any more go.
            for (i, collectable) in &refs.rows {
                if *collectable {
                    drop_zero_row(&sets[*i].0, sha).await?;
                }
            }
        }
        report.missing_blobs = live.into_iter().filter(|sha| !present.contains(sha)).collect();
        Ok(report)
    }
}

/// Deletes the reference-count row of `sha` on `db` if it is still zero.
async fn drop_zero_row(db: &Db, sha: &str) -> Result<()> {
    let mut tx = db.begin().await?;
    tx.execute(&st(
        "DELETE FROM _forgebase_storage_blobs WHERE sha256 = ? AND refcount = 0",
        vec![sha.into()],
    ))
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Registers the `storage.reconcile` job handler.
pub fn register_jobs(registry: JobRegistry, svc: StorageService) -> JobRegistry {
    registry.register(RECONCILE_JOB, move |_ctx: JobContext| {
        let svc = svc.clone();
        async move {
            let report = svc
                .reconcile_now()
                .await
                .map_err(|e| HandlerError::retry(e.to_string()))?;
            Ok(serde_json::to_value(report).unwrap_or_default())
        }
    })
}
