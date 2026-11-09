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

use std::collections::{HashMap, HashSet};

use forgebase_data::{AccessContext, Action};
use forgebase_jobs::{HandlerError, JobContext, JobRegistry};

use crate::error::Result;
use crate::model::ReconcileReport;
use crate::service::{RECONCILE_JOB, StorageService, int, st, text};

impl StorageService {
    /// Runs the reconciler (needs the `storage_admin` platform permission).
    pub async fn reconcile(&self, ctx: &AccessContext) -> Result<ReconcileReport> {
        self.authorize_platform(ctx, Action::StorageAdmin)?;
        self.reconcile_now().await
    }

    /// Runs the reconciler without an authorization check (the job handler).
    pub async fn reconcile_now(&self) -> Result<ReconcileReport> {
        let mut report = ReconcileReport::default();
        let now = self.now();
        let grace = i64::try_from(self.inner.settings.reconcile_grace_secs).unwrap_or(i64::MAX / 2000) * 1000;
        let cutoff = now - grace;
        let driver = &self.inner.driver;

        // 1. Expired sessions.
        let expired = self
            .read(
                "SELECT id FROM _forgebase_storage_uploads WHERE expires_at <= ?",
                vec![now.into()],
            )
            .await?;
        for r in &expired.rows {
            self.drop_session(&text(r, 0)).await?;
            report.uploads_aborted += 1;
        }

        // 2. Staged files nobody refers to.
        let referenced: HashSet<String> = self
            .read("SELECT staging_key FROM _forgebase_storage_parts", vec![])
            .await?
            .rows
            .iter()
            .map(|r| text(r, 0))
            .collect();
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
        let rows = self
            .read(
                "SELECT sha256, refcount, zero_since FROM _forgebase_storage_blobs",
                vec![],
            )
            .await?;
        let mut known: HashMap<String, (i64, Option<i64>)> = HashMap::new();
        for r in &rows.rows {
            known.insert(text(r, 0), (int(r, 1), r.get(2).and_then(forgebase_db::Value::as_i64)));
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
                Some((0, since)) if since.is_some_and(|s| s <= cutoff) => {
                    driver.delete(&obj.key).await?;
                    let mut tx = self.begin().await?;
                    tx.execute(&st(
                        "DELETE FROM _forgebase_storage_blobs WHERE sha256 = ? AND refcount = 0",
                        vec![sha.as_str().into()],
                    ))
                    .await?;
                    tx.commit().await?;
                    report.unreferenced_blobs_deleted += 1;
                }
                Some(_) => {}
            }
        }
        for (sha, (refs, since)) in &known {
            if present.contains(sha) {
                continue;
            }
            if *refs > 0 {
                report.missing_blobs.push(sha.clone());
            } else if since.is_some_and(|s| s <= cutoff) {
                // Nothing references it and nothing is stored: drop the row.
                let mut tx = self.begin().await?;
                tx.execute(&st(
                    "DELETE FROM _forgebase_storage_blobs WHERE sha256 = ? AND refcount = 0",
                    vec![sha.as_str().into()],
                ))
                .await?;
                tx.commit().await?;
            }
        }
        report.missing_blobs.sort();
        Ok(report)
    }
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
