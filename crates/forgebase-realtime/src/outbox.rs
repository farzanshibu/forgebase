//! The transactional outbox (`_forgebase_realtime_outbox`, migration 0011).
//!
//! [`OutboxHook`] is installed on the data service: every record change adds
//! one outbox row in the change's own transaction. The dispatcher tails the
//! table by `seq` and publishes each committed row once to the in-process
//! bus; connections replay from it after a reconnect.

use std::sync::Arc;

use forgebase_data::db::{Backend, Rows, Statement, TxMode, Value};
use forgebase_data::{ChangeHook, ChangeOp, RecordChange, RowSnapshot};
use serde_json::Value as Json;
use tokio::sync::Notify;

use crate::RealtimeError;

/// Rows read per outbox page.
pub(crate) const PAGE: i64 = 500;

/// One committed change, as stored in the outbox.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Cursor: strictly increasing, never reused.
    pub seq: i64,
    /// What happened.
    pub op: ChangeOp,
    /// Stable collection id (survives renames).
    pub collection_id: String,
    /// Collection name at the time of the change.
    pub collection: String,
    /// Record id.
    pub record_id: String,
    /// Tenant of the writer's context.
    pub tenant: Option<String>,
    /// Who made the change.
    pub actor: String,
    /// The row after the change (before it, for deletes).
    pub snapshot: RowSnapshot,
    /// When (UTC, millisecond precision).
    pub created_at: String,
}

impl Event {
    /// Event type: `record.created`, `record.updated` or `record.deleted`.
    pub fn kind(&self) -> &'static str {
        match self.op {
            ChangeOp::Create => "record.created",
            ChangeOp::Update => "record.updated",
            ChangeOp::Delete => "record.deleted",
        }
    }
}

/// Writes outbox rows inside data-service transactions and wakes the
/// dispatcher after commit.
#[derive(Debug)]
pub(crate) struct OutboxHook {
    pub(crate) notify: Arc<Notify>,
}

impl ChangeHook for OutboxHook {
    fn statements(&self, c: &RecordChange<'_>) -> Result<Vec<Statement>, String> {
        Ok(vec![Statement::new(
            "INSERT INTO _forgebase_realtime_outbox
                 (op, collection_id, collection, record_id, tenant, actor, snapshot, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            vec![
                c.op.as_str().into(),
                c.collection.id.clone().into(),
                c.collection.name.clone().into(),
                c.record_id.into(),
                c.ctx.tenant.clone().map_or(Value::Null, Value::Text),
                c.ctx.actor().into(),
                c.row.to_json().to_string().into(),
                forgebase_data::schema::now_timestamp().into(),
            ],
        )])
    }

    fn committed(&self) {
        self.notify.notify_one();
    }
}

fn parse_rows(rows: &Rows) -> Result<Vec<Event>, RealtimeError> {
    let mut out = Vec::with_capacity(rows.rows.len());
    for r in &rows.rows {
        let text = |i: usize| r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned();
        let op = match r.get(1).and_then(Value::as_str) {
            Some("create") => ChangeOp::Create,
            Some("update") => ChangeOp::Update,
            Some("delete") => ChangeOp::Delete,
            other => return Err(RealtimeError::Internal(format!("unknown outbox op {other:?}"))),
        };
        let snapshot = serde_json::from_str::<Json>(&text(7))
            .ok()
            .as_ref()
            .and_then(RowSnapshot::from_json)
            .ok_or_else(|| RealtimeError::Internal("outbox snapshot does not decode".into()))?;
        out.push(Event {
            seq: r.first().and_then(Value::as_i64).unwrap_or_default(),
            op,
            collection_id: text(2),
            collection: text(3),
            record_id: text(4),
            tenant: r.get(5).and_then(Value::as_str).map(str::to_owned),
            actor: text(6),
            snapshot,
            created_at: text(8),
        });
    }
    Ok(out)
}

/// Events with `seq > after`, oldest first, at most `limit`. Public for
/// other consumers of the outbox (server functions' after-hooks, FB-60).
pub async fn read_after(backend: &dyn Backend, after: i64, limit: i64) -> Result<Vec<Event>, RealtimeError> {
    let rows = backend
        .fetch_all(&Statement::new(
            "SELECT seq, op, collection_id, collection, record_id, tenant, actor, snapshot, created_at
             FROM _forgebase_realtime_outbox WHERE seq > ?1 ORDER BY seq LIMIT ?2",
            vec![after.into(), limit.into()],
        ))
        .await?;
    parse_rows(&rows)
}

/// `(oldest retained seq, newest seq)`; `(0, 0)` while the outbox is empty.
pub async fn bounds(backend: &dyn Backend) -> Result<(i64, i64), RealtimeError> {
    let rows = backend
        .fetch_all(&Statement::new(
            "SELECT coalesce(min(seq), 0), coalesce(max(seq), 0) FROM _forgebase_realtime_outbox",
            vec![],
        ))
        .await?;
    let row = rows.rows.first();
    let get = |i: usize| row.and_then(|r| r.get(i)).and_then(Value::as_i64).unwrap_or(0);
    Ok((get(0), get(1)))
}

/// Deletes events older than `cutoff` (RFC 3339 text) or beyond the newest
/// `keep`, but never the newest event (so the head cursor stays known).
/// Returns the number of deleted rows.
pub(crate) async fn prune(backend: &dyn Backend, cutoff: &str, keep: i64) -> Result<u64, RealtimeError> {
    let mut tx = backend.begin(TxMode::Normal).await?;
    let res = tx
        .execute(&Statement::new(
            "DELETE FROM _forgebase_realtime_outbox
             WHERE seq < (SELECT max(seq) FROM _forgebase_realtime_outbox)
               AND (created_at < ?1 OR seq <= (SELECT max(seq) FROM _forgebase_realtime_outbox) - ?2)",
            vec![cutoff.into(), keep.into()],
        ))
        .await;
    match res {
        Ok(r) => {
            tx.commit().await?;
            Ok(r.rows_affected)
        }
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e.into())
        }
    }
}
