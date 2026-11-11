//! Which engine holds the deliveries, the pgmq layout check, and
//! `queue.convert` (ADR-0029 §1.6, FB-164).
//!
//! **Selection.** The service resolves its engine once, on first use:
//! PostgreSQL whose `_forgebase_meta` `ext.queue.engine` is `extension`
//! (FB-161's setup pinned it: [`forgebase_db::Database::feature_active`]
//! is true exactly then) with pgmq installed and the queue-table layout
//! FB-164 relies on ([`super::LAYOUT`]) uses pgmq; anything else, the
//! portable table. A layout mismatch or a missing pgmq falls back to the
//! portable engine with a warning.
//!
//! **Reconcile.** At that first use, and after a conversion, every owed
//! delivery is moved to the engine in use: pgmq gives every subscription
//! its queue and takes the portable rows; portable takes back the entries
//! of ForgeBase's pgmq queues and drops them. One transaction per
//! subscription; a conversion that stopped half way is finished by the
//! next start, because the stored engine is written first.
//!
//! **Conversion** ([`QueueService::convert`], job kind [`CONVERT_JOB`])
//! holds the engine lock for writing: claims, acks and every other
//! delivery operation wait for it, so no delivery is read from the store
//! it is leaving.

use forgebase_data::{AccessContext, Action};
use forgebase_db::{Feature, Statement, WriteTx};
use forgebase_jobs::{HandlerError, JobContext, JobRegistry};
use tokio::sync::{RwLock, RwLockReadGuard};

use super::{LAYOUT, PREFIX, create_queue, drop_queue_named, ms, queue_name, table};
use crate::error::QueueError;
use crate::model::{ConvertReport, DeliveryEngine};
use crate::row::Row;
use crate::service::{QueueService, Result, finish};

/// Job kind that converts the deliveries (payload `{"to": "pgmq" |
/// "portable"}`); reserved: only privileged callers may enqueue it.
pub const CONVERT_JOB: &str = "queue.convert";

/// The resolved engine (`None` until the first delivery operation).
pub(crate) type EngineSlot = RwLock<Option<DeliveryEngine>>;

/// Registers the [`CONVERT_JOB`] handler.
#[must_use]
pub fn register_jobs(registry: JobRegistry, svc: QueueService) -> JobRegistry {
    registry.register(CONVERT_JOB, move |ctx: JobContext| {
        let svc = svc.clone();
        async move {
            let to = match ctx.payload().get("to").and_then(|v| v.as_str()) {
                Some("pgmq") => DeliveryEngine::Pgmq,
                Some("portable") => DeliveryEngine::Portable,
                _ => return Err(HandlerError::fatal("payload needs \"to\": \"pgmq\" or \"portable\"")),
            };
            match svc.convert(&AccessContext::system(), to).await {
                Ok(r) => Ok(serde_json::to_value(r).unwrap_or_default()),
                Err(e @ (QueueError::Invalid { .. } | QueueError::Conflict { .. })) => {
                    Err(HandlerError::fatal(e.to_string()))
                }
                Err(e) => Err(HandlerError::retry(e.to_string())),
            }
        }
    })
}

/// Problems of the queue table `table` against [`LAYOUT`], given its
/// columns `(name, format_type)`.
pub(crate) fn layout_problems(table: &str, columns: &[(String, String)]) -> Vec<String> {
    LAYOUT
        .iter()
        .filter_map(|(name, ty)| match columns.iter().find(|(n, _)| n == name) {
            None => Some(format!("{table} has no column {name}")),
            Some((_, t)) if t != ty => Some(format!("{table}.{name} is {t}, not {ty}")),
            Some(_) => None,
        })
        .collect()
}

impl QueueService {
    fn on_postgres(&self) -> bool {
        self.backend().dialect().name() == "postgres"
    }

    /// The delivery engine in use (resolved on first use).
    ///
    /// # Errors
    /// Database errors while resolving it.
    pub async fn delivery_engine(&self) -> Result<DeliveryEngine> {
        Ok(*self.store().await?)
    }

    /// The engine, read-locked: hold the guard for the whole operation so a
    /// conversion cannot move deliveries under it. Never call it again
    /// while holding the guard (a waiting conversion would deadlock).
    pub(crate) async fn store(&self) -> Result<RwLockReadGuard<'_, DeliveryEngine>> {
        let slot = &self.engine;
        if let Ok(g) = RwLockReadGuard::try_map(slot.read().await, Option::as_ref) {
            return Ok(g);
        }
        let mut w = slot.write().await;
        if w.is_none() {
            *w = Some(self.resolve().await?);
        }
        RwLockReadGuard::try_map(w.downgrade(), Option::as_ref)
            .map_err(|_| QueueError::Internal("delivery engine not resolved".into()))
    }

    async fn pgmq_installed(&self) -> Result<bool> {
        let rows = self
            .backend()
            .fetch_all(&Statement::raw(
                "SELECT COUNT(*) FROM pg_extension WHERE extname = 'pgmq'",
            ))
            .await?;
        Ok(crate::row::scalar(&rows) > 0)
    }

    async fn stored_engine(&self) -> Result<Option<String>> {
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT value FROM _forgebase_meta WHERE key = ?1",
                vec![Feature::Queue.engine_key().into()],
            ))
            .await?;
        Ok(Row::all(&rows).next().map(|r| r.text("value")))
    }

    /// Why pgmq cannot hold the deliveries (empty = it can): pgmq must be
    /// installed, and a new queue (created in a transaction that is rolled
    /// back) and every existing ForgeBase queue must have [`LAYOUT`].
    pub(crate) async fn pgmq_problems(&self) -> Result<Vec<String>> {
        if !self.on_postgres() {
            return Ok(vec!["pgmq needs PostgreSQL".into()]);
        }
        if !self.pgmq_installed().await? {
            return Ok(vec!["the pgmq extension is not installed in this database".into()]);
        }
        let mut tx = self.begin().await?;
        let r: Result<Vec<String>> = async {
            if let Err(e) = create_queue(&mut *tx, "forgebase/fb164-layout-probe", false).await {
                return Ok(vec![format!("pgmq.create failed: {e}")]);
            }
            let rows = tx
                .fetch_all(&Statement::raw(format!(
                    "SELECT c.relname AS t, a.attname AS c, format_type(a.atttypid, a.atttypmod) AS ty \
                     FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                     JOIN pg_attribute a ON a.attrelid = c.oid \
                     WHERE n.nspname = 'pgmq' AND c.relkind IN ('r', 'p') AND c.relname LIKE 'q\\_{}%' \
                       AND a.attnum > 0 AND NOT a.attisdropped ORDER BY c.relname, a.attnum",
                    PREFIX.replace('_', "\\_")
                )))
                .await?;
            let mut tables: Vec<(String, Vec<(String, String)>)> = Vec::new();
            for r in Row::all(&rows) {
                let t = r.text("t");
                let col = (r.text("c"), r.text("ty"));
                match tables.last_mut() {
                    Some((name, cols)) if *name == t => cols.push(col),
                    _ => tables.push((t, vec![col])),
                }
            }
            Ok(tables
                .iter()
                .flat_map(|(t, cols)| layout_problems(&format!("pgmq.{t}"), cols))
                .collect())
        }
        .await;
        tx.rollback().await?;
        r
    }

    /// Decides the engine at first use and moves stray deliveries to it.
    async fn resolve(&self) -> Result<DeliveryEngine> {
        if !self.on_postgres() {
            return Ok(DeliveryEngine::Portable);
        }
        let pinned = self.stored_engine().await?.as_deref() == Some("extension");
        if !pinned {
            // Entries left in pgmq by a conversion that stopped half way.
            if self.pgmq_installed().await?
                && let Err(e) = self.reconcile(DeliveryEngine::Portable).await
            {
                tracing::warn!(error = %e, "pgmq queues of an earlier conversion not moved back");
            }
            return Ok(DeliveryEngine::Portable);
        }
        let problems = self.pgmq_problems().await?;
        if !problems.is_empty() {
            tracing::warn!(
                ?problems,
                "this database keeps queue deliveries in pgmq, but pgmq cannot be used as FB-164 expects; \
                 deliveries use the portable engine"
            );
            return Ok(DeliveryEngine::Portable);
        }
        self.reconcile(DeliveryEngine::Pgmq).await?;
        Ok(DeliveryEngine::Pgmq)
    }

    /// Moves every owed delivery to `to`. Returns `(subscriptions,
    /// deliveries)` moved.
    async fn reconcile(&self, to: DeliveryEngine) -> Result<(i64, i64)> {
        let rows = self
            .backend()
            .fetch_all(&Statement::raw(
                "SELECT name, ordering FROM _forgebase_queue_subscriptions ORDER BY name",
            ))
            .await?;
        let subs: Vec<(String, bool)> = Row::all(&rows)
            .map(|r| (r.text("name"), r.int("ordering") != 0))
            .collect();
        let queues: Vec<String> = if self.pgmq_installed().await? {
            let rows = self
                .backend()
                .fetch_all(&Statement::raw(format!(
                    "SELECT queue_name FROM pgmq.meta WHERE queue_name LIKE '{}%' ORDER BY queue_name",
                    PREFIX.replace('_', "\\_")
                )))
                .await?;
            Row::all(&rows).map(|r| r.text("queue_name")).collect()
        } else {
            Vec::new()
        };
        let mut moved = (0, 0);
        match to {
            DeliveryEngine::Pgmq => {
                let rows = self
                    .backend()
                    .fetch_all(&Statement::raw(
                        "SELECT DISTINCT subscription FROM _forgebase_queue_deliveries",
                    ))
                    .await?;
                let portable: Vec<String> = Row::all(&rows).map(|r| r.text("subscription")).collect();
                for (sub, ordering) in &subs {
                    let has_queue = queues.contains(&queue_name(sub));
                    if has_queue && !portable.contains(sub) {
                        continue;
                    }
                    let mut tx = self.begin().await?;
                    let r = async {
                        if !has_queue {
                            create_queue(&mut *tx, sub, *ordering).await?;
                        }
                        to_pgmq(&mut *tx, sub).await
                    }
                    .await;
                    moved.1 += finish(tx, r).await?;
                    moved.0 += 1;
                }
                // Queues of subscriptions that no longer exist.
                for q in queues
                    .iter()
                    .filter(|q| !subs.iter().any(|(s, _)| queue_name(s) == **q))
                {
                    let mut tx = self.begin().await?;
                    let r = drop_queue_named(&mut *tx, q).await;
                    finish(tx, r).await?;
                }
            }
            DeliveryEngine::Portable => {
                for q in &queues {
                    let sub = subs.iter().find(|(s, _)| queue_name(s) == *q).map(|(s, _)| s.clone());
                    let mut tx = self.begin().await?;
                    let r = async {
                        let n = match &sub {
                            Some(s) => to_portable(&mut *tx, s).await?,
                            None => 0,
                        };
                        drop_queue_named(&mut *tx, q).await?;
                        Ok(n)
                    }
                    .await;
                    moved.1 += finish(tx, r).await?;
                    moved.0 += i64::from(sub.is_some());
                }
            }
        }
        if moved.0 > 0 {
            tracing::info!(
                engine = to.as_str(),
                subscriptions = moved.0,
                deliveries = moved.1,
                "queue deliveries moved"
            );
        }
        Ok(moved)
    }

    /// Converts the owed deliveries (a backlog, not history) to `to` and
    /// records it as the database's queue engine (`ext.queue.engine`). An
    /// instance-wide `engine:queue_admin` operation; delivery operations
    /// wait while it runs. The extension report shown by
    /// `/api/system/info` follows at the next start.
    ///
    /// # Errors
    /// `400 pgmq_unavailable` when pgmq cannot hold deliveries here;
    /// authorisation and database errors.
    ///
    /// Sharded (FB-172): every set is converted, one after the other; the
    /// report adds up the sets (`from` is control's).
    pub async fn convert(&self, ctx: &AccessContext, to: DeliveryEngine) -> Result<ConvertReport> {
        let mut views = self.views().into_iter();
        let Some(first) = views.next() else {
            return self.convert_here(ctx, to).await;
        };
        let mut report = first.convert_here(ctx, to).await?;
        for view in views {
            let r = view.convert_here(ctx, to).await?;
            report.subscriptions += r.subscriptions;
            report.deliveries += r.deliveries;
        }
        Ok(report)
    }

    async fn convert_here(&self, ctx: &AccessContext, to: DeliveryEngine) -> Result<ConvertReport> {
        self.allow(ctx, Action::QueueAdmin)?;
        if ctx.tenant.is_some() {
            return Err(QueueError::invalid(
                "invalid_parameter",
                "queue.convert is instance-wide; call it without a tenant",
            ));
        }
        if !self.on_postgres() && to == DeliveryEngine::Portable {
            return Ok(ConvertReport {
                from: DeliveryEngine::Portable,
                to,
                subscriptions: 0,
                deliveries: 0,
            });
        }
        if to == DeliveryEngine::Pgmq {
            let problems = self.pgmq_problems().await?;
            if !problems.is_empty() {
                return Err(QueueError::invalid("pgmq_unavailable", problems.join("; ")));
            }
        }
        let mut slot = self.engine.write().await;
        let from = match *slot {
            Some(e) => e,
            None => self.resolve().await?,
        };
        // Recorded first: a conversion that stops half way is finished by
        // the next start's reconcile, which follows the stored engine.
        let mut tx = self.begin().await?;
        let r = async {
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_meta (key, value, updated_at) VALUES (?1, ?2, ?3) \
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![
                    Feature::Queue.engine_key().into(),
                    match to {
                        DeliveryEngine::Pgmq => "extension",
                        DeliveryEngine::Portable => "portable",
                    }
                    .into(),
                    forgebase_core::now_rfc3339().into(),
                ],
            ))
            .await?;
            Ok(())
        }
        .await;
        finish(tx, r).await?;
        *slot = Some(to);
        let (subscriptions, deliveries) = self.reconcile(to).await?;
        drop(slot);
        self.notify();
        Ok(ConvertReport {
            from,
            to,
            subscriptions,
            deliveries,
        })
    }
}

/// Moves the portable deliveries of `sub` into its queue (state, attempts,
/// lease, history kept; `seq` order). Returns the count.
async fn to_pgmq(tx: &mut dyn WriteTx, sub: &str) -> Result<i64> {
    let t = |e: &str| format!("(TIMESTAMPTZ 'epoch' + ({e}) * INTERVAL '1 millisecond')");
    let n = tx
        .execute(&Statement::new(
            format!(
                "INSERT INTO {} (vt, read_ct, message) SELECT {}, CAST(attempts AS INTEGER), \
                 jsonb_build_object('seq', seq, 'key', ordering_key, 'lease', lease_id, \
                 'consumer', CASE WHEN state = 'leased' THEN consumer END, 'history', CAST(history AS jsonb)) \
                 FROM _forgebase_queue_deliveries WHERE subscription = ?1 ORDER BY seq",
                table(sub),
                t("CASE WHEN state = 'leased' THEN coalesce(lease_until, available_at) ELSE available_at END")
            ),
            vec![sub.into()],
        ))
        .await?
        .rows_affected;
    tx.execute(&Statement::new(
        "DELETE FROM _forgebase_queue_deliveries WHERE subscription = ?1",
        vec![sub.into()],
    ))
    .await?;
    Ok(i64::try_from(n).unwrap_or(0))
}

/// Moves the entries of `sub`'s queue into portable delivery rows (those
/// whose message still exists). Returns the count.
async fn to_portable(tx: &mut dyn WriteTx, sub: &str) -> Result<i64> {
    let vt = ms("q.vt");
    let n = tx
        .execute(&Statement::new(
            format!(
                "INSERT INTO _forgebase_queue_deliveries (subscription, seq, ordering_key, state, attempts, \
                 available_at, lease_id, lease_until, consumer, history) \
                 SELECT ?1, m.seq, q.message->>'key', \
                 CASE WHEN q.message->>'consumer' IS NULL THEN 'ready' ELSE 'leased' END, q.read_ct, {vt}, \
                 q.message->>'lease', CASE WHEN q.message->>'consumer' IS NULL THEN NULL ELSE {vt} END, \
                 q.message->>'consumer', coalesce(CAST(q.message->'history' AS TEXT), '[]') \
                 FROM {} q JOIN _forgebase_queue_messages m ON m.seq = CAST(q.message->>'seq' AS BIGINT) \
                 ORDER BY q.msg_id",
                table(sub)
            ),
            vec![sub.into()],
        ))
        .await?
        .rows_affected;
    Ok(i64::try_from(n).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(a, b)| ((*a).to_owned(), (*b).to_owned())).collect()
    }

    #[test]
    fn the_layout_check_names_missing_and_retyped_columns() {
        let good = cols(&[
            ("msg_id", "bigint"),
            ("read_ct", "integer"),
            ("enqueued_at", "timestamp with time zone"),
            ("last_read_at", "timestamp with time zone"),
            ("vt", "timestamp with time zone"),
            ("message", "jsonb"),
            ("headers", "jsonb"),
        ]);
        assert!(layout_problems("pgmq.q_x", &good).is_empty());
        let bad = cols(&[
            ("msg_id", "bigint"),
            ("read_ct", "bigint"),
            ("enqueued_at", "timestamp with time zone"),
            ("message", "json"),
        ]);
        assert_eq!(
            layout_problems("pgmq.q_x", &bad),
            vec![
                "pgmq.q_x.read_ct is bigint, not integer".to_owned(),
                "pgmq.q_x has no column vt".to_owned(),
                "pgmq.q_x.message is json, not jsonb".to_owned(),
            ]
        );
    }
}
