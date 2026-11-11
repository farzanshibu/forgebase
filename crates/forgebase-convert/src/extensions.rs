//! Bringing extension-held data to portable rows before `--to sqlite`
//! (ADR-0029 FB-162, FB-164, FB-165 addenda), with the services' own
//! conversions: `VectorService::convert` (pgvector → bytea),
//! `QueueService::convert` (pgmq queues → `_forgebase_queue_deliveries`)
//! and the pg_cron release of FB-165 (schedules handed back to the tick).
//! The source database is changed by these steps; it stays a valid
//! PostgreSQL project on the portable engines.

use std::path::Path;
use std::sync::Arc;

use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::{Backend, BackendKind, Database, Statement};
use serde::Serialize;

use crate::ConvertError;

/// What [`prepare_portable`] moved.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Prepared {
    /// Vectors moved from pgvector to portable rows.
    pub vectors: Option<u64>,
    /// Deliveries moved from pgmq queues to portable rows.
    pub deliveries: Option<u64>,
    /// Whether pg_cron's ForgeBase jobs were removed (schedules back on the tick).
    pub cron_released: bool,
}

async fn one(db: &Database, sql: &'static str) -> Result<Option<String>, ConvertError> {
    let rows = db.fetch_all(&Statement::raw(sql)).await?;
    Ok(rows.scalar().and_then(|v| match v {
        forgebase_db::Value::Text(s) => Some(s.clone()),
        forgebase_db::Value::Integer(i) => Some(i.to_string()),
        _ => None,
    }))
}

/// Converts pgvector vectors and pgmq deliveries to the portable engines
/// and releases pg_cron, where the source uses them. A no-op on SQLite and
/// on a PostgreSQL database that uses none of them. `indexes_dir` is the
/// data directory's `indexes/` (vector index files).
///
/// # Errors
/// The services' errors.
pub async fn prepare_portable(db: &Database, indexes_dir: &Path) -> Result<Prepared, ConvertError> {
    let mut out = Prepared::default();
    if db.kind() != BackendKind::Postgres {
        return Ok(out);
    }
    let pin = |k: &'static str| async move { db.meta_get(k).await.map(|v| v.as_deref() == Some("extension")) };
    let vec_rows = one(
        db,
        "SELECT count(*)::text FROM pg_attribute WHERE attrelid = to_regclass('forgebase._forgebase_vector_values') \
         AND attname = 'vec' AND NOT attisdropped",
    )
    .await?;
    let has_vec = vec_rows.as_deref().is_some_and(|n| n != "0")
        && one(
            db,
            "SELECT count(*)::text FROM forgebase._forgebase_vector_values WHERE vec IS NOT NULL",
        )
        .await?
        .as_deref()
        .is_some_and(|n| n != "0");
    let pgmq = one(
        db,
        "SELECT count(*)::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'pgmq' AND c.relkind = 'r' AND c.relname LIKE 'q\\_fbs\\_%'",
    )
    .await?;
    let has_pgmq = pgmq.as_deref().is_some_and(|n| n != "0");
    let has_cron = one(db, "SELECT count(*)::text FROM pg_extension WHERE extname = 'pg_cron'")
        .await?
        .as_deref()
        .is_some_and(|n| n != "0");
    let vector_pinned = pin("ext.vector.engine").await?;
    let queue_pinned = pin("ext.queue.engine").await?;
    if !(has_vec || has_pgmq || has_cron || vector_pinned || queue_pinned) {
        return Ok(out);
    }
    let data = DataService::new(Arc::new(db.clone()), Arc::new(DefaultAuthorizer)).await?;
    let ctx = AccessContext::system();
    if has_vec || vector_pinned {
        let svc = forgebase_vector::VectorService::open(
            data.clone(),
            indexes_dir.to_path_buf(),
            forgebase_vector::VectorConfig::default(),
        )
        .await
        .map_err(|e| ConvertError::Prepare(format!("vector service: {e}")))?;
        let report = svc
            .convert(&ctx, forgebase_vector::model::VectorEngine::Portable)
            .await
            .map_err(|e| ConvertError::Prepare(format!("vector.convert to portable: {e}")));
        svc.shutdown().await;
        out.vectors = Some(report?.vectors);
    }
    if has_pgmq || queue_pinned {
        let keyring = forgebase_auth::crypto::Keyring::from_master(&[0u8; 32]);
        let svc = forgebase_queue::QueueService::new(data.clone(), keyring, forgebase_queue::QueueSettings::default());
        let report = svc
            .convert(&ctx, forgebase_queue::DeliveryEngine::Portable)
            .await
            .map_err(|e| ConvertError::Prepare(format!("queue.convert to portable: {e}")))?;
        out.deliveries = Some(u64::try_from(report.deliveries).unwrap_or(0));
    }
    if has_cron {
        // Without the extension setup on this handle the cron feature is not
        // active, so attaching releases: ForgeBase's pg_cron jobs are removed
        // and every schedule is handed back to the tick (FB-165).
        let jobs = forgebase_jobs::JobService::new(data);
        let _ = forgebase_jobs::attach_pgcron(&jobs, db).await;
        out.cron_released = true;
    }
    Ok(out)
}
