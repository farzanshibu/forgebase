//! Subscription deliveries on pgmq (ADR-0029 §1.6, FB-164).
//!
//! On PostgreSQL with pgmq every subscription owns one pgmq queue,
//! [`queue_name`] (`fbs_` and a hash of its stored name: pgmq names are
//! identifiers of at most 47 characters, stored names are not), created and
//! dropped with the subscription. A queue entry is one owed delivery:
//!
//! | Portable column (`_forgebase_queue_deliveries`) | pgmq entry |
//! |---|---|
//! | `seq`, `ordering_key` | `message->'seq'`, `message->'key'` |
//! | `state = 'leased'`, `consumer` | `message->'consumer'` is not null |
//! | `lease_id` | `message->'lease'` (kept after expiry: a late ack counts) |
//! | `attempts` | `read_ct` |
//! | `available_at` (ready) or `lease_until` (leased) | `vt` |
//! | `history` | `message->'history'` (the same entry shape) |
//!
//! Everything else (topics, the message log in `seq` order, cursors,
//! filters, dedup, dead letters, snapshots, rates, the owner's view of
//! record messages) stays in ForgeBase's tables on both engines. Times are
//! the service clock's epoch milliseconds turned into `timestamptz`, never
//! the server's `now()`, so both engines follow the same clock.
//!
//! Each statement here is ForgeBase's own over pgmq's queue table, which
//! FB-164 pins ([`LAYOUT`], checked at start by `engine.rs`): the claim is
//! pgmq's `read` plus lease tokens and ordering keys; materialisation sends
//! with `pgmq.send_batch`.

mod engine;

use std::fmt::Write as _;

use forgebase_db::{Rows, Statement, WriteTx};
use sha2::{Digest, Sha256};

pub use engine::{CONVERT_JOB, register_jobs};
pub(crate) use engine::EngineSlot;

use crate::history::MAX_ENTRIES;
use crate::row::{Row, scalar};
use crate::service::{MESSAGE_COLS, QueueService, Result, SubRow};

/// pgmq queue-table columns FB-164 relies on, with their types
/// (`format_type`). Checked at start; a mismatch keeps the portable engine.
pub(crate) const LAYOUT: [(&str, &str); 5] = [
    ("msg_id", "bigint"),
    ("read_ct", "integer"),
    ("enqueued_at", "timestamp with time zone"),
    ("vt", "timestamp with time zone"),
    ("message", "jsonb"),
];

/// Prefix of ForgeBase's pgmq queues.
pub(crate) const PREFIX: &str = "fbs_";

/// The pgmq queue of the subscription stored as `sub`: `fbs_` and the first
/// 128 bits of its SHA-256, in hex (36 characters; pgmq allows 47).
pub(crate) fn queue_name(sub: &str) -> String {
    let digest = Sha256::digest(sub.as_bytes());
    let mut name = String::with_capacity(36);
    name.push_str(PREFIX);
    for b in digest.iter().take(16) {
        let _ = write!(name, "{b:02x}");
    }
    name
}

/// The queue table of `sub` (`pgmq.q_<queue>`; safe to splice: hex only).
pub(crate) fn table(sub: &str) -> String {
    format!("pgmq.q_{}", queue_name(sub))
}

/// `timestamptz` of the epoch-millisecond parameter `?n` (exact).
fn ts(n: u8) -> String {
    format!("(TIMESTAMPTZ 'epoch' + CAST(?{n} AS BIGINT) * INTERVAL '1 millisecond')")
}

/// Epoch milliseconds of the `timestamptz` expression `e` (exact).
pub(crate) fn ms(e: &str) -> String {
    format!("CAST(EXTRACT(EPOCH FROM {e}) * 1000 AS BIGINT)")
}

/// A new lease (ack id) for entry `msg_id`: the id leads, so an ack finds
/// its entry by primary key (`DELETE … WHERE msg_id = $1 AND lease = $2`).
pub(crate) fn new_lease(msg_id: i64) -> String {
    format!("ack-{msg_id}-{}", uuid::Uuid::new_v4().simple())
}

/// The entry an ack id of [`new_lease`] names. Ack ids handed out by the
/// portable engine (before a conversion) carry none.
fn entry_of(ack_id: &str) -> Option<i64> {
    ack_id.strip_prefix("ack-")?.split_once('-')?.0.parse().ok()
}

/// The entry's history (a JSON array; `[]` when missing).
const HISTORY: &str = "COALESCE(q.message->'history', '[]'::jsonb)";

/// `h` with `field` of its last entry set to the text expression `value`.
fn set_last(h: &str, field: &str, value: &str) -> String {
    format!(
        "CASE WHEN jsonb_array_length({h}) > 0 THEN jsonb_set({h}, ARRAY[CAST(jsonb_array_length({h}) - 1 AS TEXT), \
         '{field}'], to_jsonb({value})) ELSE {h} END"
    )
}

/// The history with an entry for a new attempt (`?at`, `?consumer`), the
/// oldest dropped at [`MAX_ENTRIES`] (as `history::push`).
fn pushed(at: u8, consumer: u8) -> String {
    format!(
        "(CASE WHEN jsonb_array_length({HISTORY}) >= {MAX_ENTRIES} THEN {HISTORY} - 0 ELSE {HISTORY} END) || \
         jsonb_build_array(jsonb_build_object('attempt', q.read_ct + 1, 'at', CAST(?{at} AS BIGINT), 'consumer', \
         CAST(?{consumer} AS TEXT)))"
    )
}

/// A new entry's body for message `seq` with ordering key `key` (SQL).
fn body(seq: &str, key: &str) -> String {
    format!(
        "jsonb_build_object('seq', {seq}, 'key', {key}, 'lease', NULL, 'consumer', NULL, 'history', '[]'::jsonb)"
    )
}

/// The join of a queue table `q` with the message log.
fn join_messages(t: &str) -> String {
    format!("{t} q JOIN _forgebase_queue_messages m ON m.seq = CAST(q.message->>'seq' AS BIGINT)")
}

/// The claim's candidate query: entries visible at `?1` (ready, or an
/// expired lease the reap just made ready), oldest first, at most `?2`,
/// and with ordering keys only the oldest entry of each key (an entry with
/// a key waits while an earlier entry of that key is owed). Locked with
/// `FOR UPDATE SKIP LOCKED` as pgmq's `read` does.
pub(crate) fn ready_sql(t: &str, ordering: bool) -> String {
    let order = if ordering {
        format!(
            "AND (q.message->>'key' IS NULL OR NOT EXISTS (SELECT 1 FROM {t} e \
             WHERE e.message->>'key' = q.message->>'key' AND e.msg_id < q.msg_id))"
        )
    } else {
        String::new()
    };
    format!(
        "SELECT q.msg_id AS q_id, CAST(q.read_ct AS BIGINT) AS d_attempts, {MESSAGE_COLS} FROM {} \
         WHERE q.vt <= {} {order} ORDER BY q.msg_id LIMIT ?2 FOR UPDATE OF q SKIP LOCKED",
        join_messages(t),
        ts(1)
    )
}

/// Creates the queue of `sub` with ForgeBase's indexes: leased entries by
/// `vt` (the reap) and, for an ordering subscription, entries by key.
pub(crate) async fn create_queue(tx: &mut dyn WriteTx, sub: &str, ordering: bool) -> Result<()> {
    let q = queue_name(sub);
    tx.execute(&Statement::new("SELECT pgmq.create(CAST(?1 AS TEXT))", vec![q.as_str().into()]))
        .await?;
    tx.execute(&Statement::raw(format!(
        "CREATE INDEX IF NOT EXISTS q_{q}_leased ON pgmq.q_{q} (vt) WHERE message->>'consumer' IS NOT NULL"
    )))
    .await?;
    if ordering {
        tx.execute(&Statement::raw(format!(
            "CREATE INDEX IF NOT EXISTS q_{q}_key ON pgmq.q_{q} ((message->>'key'), msg_id) \
             WHERE message->>'key' IS NOT NULL"
        )))
        .await?;
    }
    Ok(())
}

/// Drops the queue of `sub` (no-op when it has none).
pub(crate) async fn drop_queue(tx: &mut dyn WriteTx, sub: &str) -> Result<()> {
    drop_queue_named(tx, &queue_name(sub)).await
}

pub(crate) async fn drop_queue_named(tx: &mut dyn WriteTx, queue: &str) -> Result<()> {
    tx.execute(&Statement::new(
        "SELECT pgmq.drop_queue(CAST(?1 AS TEXT))",
        vec![queue.into()],
    ))
    .await?;
    Ok(())
}

/// Sends one entry per message of `bodies` (JSON array text, in `seq`
/// order) with `pgmq.send_batch`, visible at `now`. Returns the count.
async fn send_batch(tx: &mut dyn WriteTx, sub: &str, bodies: &str, now: i64) -> Result<i64> {
    let rows = tx
        .fetch_all(&Statement::new(
            format!(
                "SELECT COUNT(*) FROM pgmq.send_batch(CAST(?1 AS TEXT), ARRAY(SELECT e FROM \
                 jsonb_array_elements(CAST(?2 AS jsonb)) WITH ORDINALITY AS b(e, i) ORDER BY i), {})",
                ts(3)
            ),
            vec![queue_name(sub).into(), bodies.into(), now.into()],
        ))
        .await?;
    Ok(scalar(&rows))
}

impl QueueService {
    /// Materialisation on pgmq: messages `cursor < seq <= head` of the
    /// topic (all of them) become entries of one `pgmq.send_batch`.
    pub(crate) async fn pgmq_send_range(
        &self,
        tx: &mut dyn WriteTx,
        sub: &SubRow,
        cursor: i64,
        head: i64,
        now: i64,
    ) -> Result<i64> {
        let rows = tx
            .fetch_all(&Statement::new(
                format!(
                    "SELECT COUNT(*) FROM pgmq.send_batch(CAST(?1 AS TEXT), ARRAY(SELECT {} \
                     FROM _forgebase_queue_messages WHERE topic = ?2 AND seq > ?3 AND seq <= ?5 ORDER BY seq), {})",
                    body("seq", "ordering_key"),
                    ts(4)
                ),
                vec![
                    queue_name(&sub.name).into(),
                    sub.topic.as_str().into(),
                    cursor.into(),
                    now.into(),
                    head.into(),
                ],
            ))
            .await?;
        Ok(scalar(&rows))
    }

    /// Materialisation on pgmq of the messages a filter selected
    /// (`(seq, ordering_key)` in `seq` order).
    pub(crate) async fn pgmq_send(
        &self,
        tx: &mut dyn WriteTx,
        sub: &str,
        picked: &[(i64, Option<String>)],
        now: i64,
    ) -> Result<i64> {
        if picked.is_empty() {
            return Ok(0);
        }
        let bodies: Vec<serde_json::Value> = picked
            .iter()
            .map(|(seq, key)| {
                serde_json::json!({"seq": seq, "key": key, "lease": null, "consumer": null, "history": []})
            })
            .collect();
        send_batch(tx, sub, &serde_json::Value::Array(bodies).to_string(), now).await
    }

    /// Entries of `sub` whose lease ended by `now`: `(msg_id, read_ct)`.
    pub(crate) async fn pgmq_expired(&self, tx: &mut dyn WriteTx, sub: &str, now: i64) -> Result<Vec<(i64, i64)>> {
        let rows = tx
            .fetch_all(&Statement::new(
                format!(
                    "SELECT msg_id, CAST(read_ct AS BIGINT) AS attempts FROM {} \
                     WHERE message->>'consumer' IS NOT NULL AND vt <= {} ORDER BY msg_id",
                    table(sub),
                    ts(1)
                ),
                vec![now.into()],
            ))
            .await?;
        Ok(Row::all(&rows).map(|r| (r.int("msg_id"), r.int("attempts"))).collect())
    }

    /// Sets the outcome (and, when given, the error) of the last attempt in
    /// the history of entry `id`.
    pub(crate) async fn pgmq_outcome(
        &self,
        tx: &mut dyn WriteTx,
        sub: &str,
        id: i64,
        outcome: &str,
        error: Option<&str>,
    ) -> Result<()> {
        let mut h = set_last(HISTORY, "outcome", "CAST(?2 AS TEXT)");
        if error.is_some() {
            h = set_last(&h, "error", "CAST(?3 AS TEXT)");
        }
        let mut params = vec![id.into(), outcome.into()];
        if let Some(e) = error {
            params.push(e.chars().take(500).collect::<String>().into());
        }
        tx.execute(&Statement::new(
            format!(
                "UPDATE {} q SET message = q.message || jsonb_build_object('history', {h}) WHERE q.msg_id = ?1",
                table(sub)
            ),
            params,
        ))
        .await?;
        Ok(())
    }

    /// Entry `id` becomes ready at `at`. `keep_lease`: the lease stays
    /// valid for a late ack (an expired lease); else it ends (a nack).
    pub(crate) async fn pgmq_ready(
        &self,
        tx: &mut dyn WriteTx,
        sub: &str,
        id: i64,
        at: i64,
        keep_lease: bool,
    ) -> Result<()> {
        let clear = if keep_lease {
            "jsonb_build_object('consumer', NULL)"
        } else {
            "jsonb_build_object('consumer', NULL, 'lease', NULL)"
        };
        tx.execute(&Statement::new(
            format!(
                "UPDATE {} q SET vt = {}, message = q.message || {clear} WHERE q.msg_id = ?1",
                table(sub),
                ts(2)
            ),
            vec![id.into(), at.into()],
        ))
        .await?;
        Ok(())
    }

    /// Deletes entry `id` of `sub`.
    pub(crate) async fn pgmq_delete(&self, tx: &mut dyn WriteTx, sub: &str, id: i64) -> Result<()> {
        tx.execute(&Statement::new(
            format!("DELETE FROM {} WHERE msg_id = ?1", table(sub)),
            vec![id.into()],
        ))
        .await?;
        Ok(())
    }

    /// Entry `id` with its message, as the dead-letter code reads a
    /// delivery (`MESSAGE_COLS`, `d_attempts`, `d_history`).
    pub(crate) async fn pgmq_delivery(&self, tx: &mut dyn WriteTx, sub: &str, id: i64) -> Result<Rows> {
        Ok(tx
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {MESSAGE_COLS}, CAST(q.read_ct AS BIGINT) AS d_attempts, \
                     CAST({HISTORY} AS TEXT) AS d_history FROM {} WHERE q.msg_id = ?1",
                    join_messages(&table(sub))
                ),
                vec![id.into()],
            ))
            .await?)
    }

    /// The claim's candidates (see [`ready_sql`]).
    pub(crate) async fn pgmq_ready_rows(
        &self,
        tx: &mut dyn WriteTx,
        sub: &SubRow,
        now: i64,
        limit: i64,
    ) -> Result<Rows> {
        Ok(tx
            .fetch_all(&Statement::new(
                ready_sql(&table(&sub.name), sub.ordering),
                vec![now.into(), limit.into()],
            ))
            .await?)
    }

    /// Leases the entries `(msg_id, lease)` to `consumer` until `until`:
    /// one statement for the batch (`vt`, `read_ct + 1`, lease, consumer,
    /// a history entry at `now`).
    pub(crate) async fn pgmq_lease(
        &self,
        tx: &mut dyn WriteTx,
        sub: &str,
        leases: &[(i64, String)],
        consumer: &str,
        now: i64,
        until: i64,
    ) -> Result<()> {
        if leases.is_empty() {
            return Ok(());
        }
        let list: Vec<serde_json::Value> = leases
            .iter()
            .map(|(id, lease)| serde_json::json!({"id": id, "lease": lease}))
            .collect();
        tx.execute(&Statement::new(
            format!(
                "UPDATE {} q SET vt = {}, read_ct = q.read_ct + 1, message = q.message || jsonb_build_object(\
                 'lease', l.lease, 'consumer', CAST(?2 AS TEXT), 'history', {}) \
                 FROM jsonb_to_recordset(CAST(?3 AS jsonb)) AS l(id BIGINT, lease TEXT) WHERE q.msg_id = l.id",
                table(sub),
                ts(1),
                pushed(4, 2)
            ),
            vec![
                until.into(),
                consumer.into(),
                serde_json::Value::Array(list).to_string().into(),
                now.into(),
            ],
        ))
        .await?;
        Ok(())
    }

    /// The entry an ack id names: by the id it carries, else (an ack id of
    /// the portable engine, from before a conversion) by its lease.
    async fn pgmq_entry(&self, tx: &mut dyn WriteTx, sub: &str, ack_id: &str) -> Result<Option<i64>> {
        if let Some(id) = entry_of(ack_id) {
            return Ok(Some(id));
        }
        let rows = tx
            .fetch_all(&Statement::new(
                format!("SELECT msg_id FROM {} WHERE message->>'lease' = ?1 LIMIT 1", table(sub)),
                vec![ack_id.into()],
            ))
            .await?;
        Ok(Row::all(&rows).next().map(|r| r.int("msg_id")))
    }

    /// Ack on pgmq: deletes the entries whose lease is still the ack id
    /// (`DELETE … WHERE msg_id = $1 AND message->>'lease' = $2`, one
    /// statement for the batch). Returns, per ack id, whether it counted.
    pub(crate) async fn pgmq_ack(&self, tx: &mut dyn WriteTx, sub: &str, ack_ids: &[String]) -> Result<Vec<bool>> {
        let mut list = Vec::new();
        for ack in ack_ids {
            if let Some(id) = self.pgmq_entry(tx, sub, ack).await? {
                list.push(serde_json::json!({"id": id, "lease": ack}));
            }
        }
        let mut done: Vec<String> = Vec::new();
        if !list.is_empty() {
            let rows = tx
                .fetch_all(&Statement::new(
                    format!(
                        "DELETE FROM {} q USING jsonb_to_recordset(CAST(?1 AS jsonb)) AS l(id BIGINT, lease TEXT) \
                         WHERE q.msg_id = l.id AND q.message->>'lease' = l.lease RETURNING l.lease AS lease",
                        table(sub)
                    ),
                    vec![serde_json::Value::Array(list).to_string().into()],
                ))
                .await?;
            done = Row::all(&rows).map(|r| r.text("lease")).collect();
        }
        // Each deleted entry counts once, for the first ack id naming it.
        Ok(ack_ids
            .iter()
            .map(|a| match done.iter().position(|d| d == a) {
                Some(i) => {
                    done.swap_remove(i);
                    true
                }
                None => false,
            })
            .collect())
    }

    /// The leased entry an ack id names: `(msg_id, read_ct)`.
    pub(crate) async fn pgmq_leased(
        &self,
        tx: &mut dyn WriteTx,
        sub: &str,
        ack_id: &str,
    ) -> Result<Option<(i64, i64)>> {
        let Some(id) = self.pgmq_entry(tx, sub, ack_id).await? else {
            return Ok(None);
        };
        let rows = tx
            .fetch_all(&Statement::new(
                format!(
                    "SELECT CAST(read_ct AS BIGINT) AS attempts FROM {} \
                     WHERE msg_id = ?1 AND message->>'lease' = ?2 AND message->>'consumer' IS NOT NULL",
                    table(sub)
                ),
                vec![id.into(), ack_id.into()],
            ))
            .await?;
        Ok(Row::all(&rows).next().map(|r| (id, r.int("attempts"))))
    }

    /// Moves the deadline of the leased entry an ack id names to `until`.
    pub(crate) async fn pgmq_deadline(&self, tx: &mut dyn WriteTx, sub: &str, ack_id: &str, until: i64) -> Result<bool> {
        let Some(id) = self.pgmq_entry(tx, sub, ack_id).await? else {
            return Ok(false);
        };
        let n = tx
            .execute(&Statement::new(
                format!(
                    "UPDATE {} SET vt = {} WHERE msg_id = ?1 AND message->>'lease' = ?2 \
                     AND message->>'consumer' IS NOT NULL",
                    table(sub),
                    ts(3)
                ),
                vec![id.into(), ack_id.into(), until.into()],
            ))
            .await?
            .rows_affected;
        Ok(n > 0)
    }

    /// Gives back the leases `consumer` holds without counting the attempt.
    pub(crate) async fn pgmq_release(&self, tx: &mut dyn WriteTx, sub: &str, consumer: &str, now: i64) -> Result<u64> {
        let h = set_last(HISTORY, "outcome", "'released'::text");
        Ok(tx
            .execute(&Statement::new(
                format!(
                    "UPDATE {} q SET vt = {}, read_ct = GREATEST(q.read_ct - 1, 0), message = q.message || \
                     jsonb_build_object('lease', NULL, 'consumer', NULL, 'history', {h}) \
                     WHERE q.message->>'consumer' IS NOT NULL AND q.message->>'consumer' = ?1",
                    table(sub),
                    ts(2)
                ),
                vec![consumer.into(), now.into()],
            ))
            .await?
            .rows_affected)
    }

    /// `(messages, bytes)` `consumer` holds on `sub` with unexpired leases.
    pub(crate) async fn pgmq_in_flight(&self, sub: &str, consumer: &str, now: i64) -> Result<Rows> {
        Ok(self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT COUNT(*) AS n, coalesce(sum(m.bytes), 0) AS b FROM {} \
                     WHERE q.message->>'consumer' IS NOT NULL AND q.message->>'consumer' = ?1 AND q.vt > {}",
                    join_messages(&table(sub)),
                    ts(2)
                ),
                vec![consumer.into(), now.into()],
            ))
            .await?)
    }

    /// Whether `sub` has an entry visible at `now` (ready or expired).
    pub(crate) fn pgmq_has_work_sql(sub: &str) -> String {
        format!("EXISTS (SELECT 1 FROM {} WHERE vt <= {})", table(sub), ts(2))
    }

    /// Drops every entry of `sub` (a seek).
    pub(crate) async fn pgmq_purge(&self, tx: &mut dyn WriteTx, sub: &str) -> Result<()> {
        tx.execute(&Statement::raw(format!("DELETE FROM {}", table(sub))))
            .await?;
        Ok(())
    }

    /// Entries for the messages saved in snapshot `snap` (a seek), in `seq`
    /// order, ready at `now`. Returns the count.
    pub(crate) async fn pgmq_restore(&self, tx: &mut dyn WriteTx, sub: &str, snap: &str, now: i64) -> Result<i64> {
        let n = tx
            .execute(&Statement::new(
                format!(
                    "INSERT INTO {} (vt, message) SELECT {}, {} FROM _forgebase_queue_snapshot_items i \
                     JOIN _forgebase_queue_messages m ON m.seq = i.seq WHERE i.snapshot = ?1 ORDER BY i.seq",
                    table(sub),
                    ts(2),
                    body("i.seq", "m.ordering_key")
                ),
                vec![snap.into(), now.into()],
            ))
            .await?
            .rows_affected;
        Ok(i64::try_from(n).unwrap_or(0))
    }

    /// Saves the owed messages of `sub` in snapshot `snap`.
    pub(crate) async fn pgmq_snapshot(&self, tx: &mut dyn WriteTx, sub: &str, snap: &str) -> Result<()> {
        tx.execute(&Statement::new(
            format!(
                "INSERT INTO _forgebase_queue_snapshot_items (snapshot, seq) SELECT ?1, m.seq FROM {}",
                join_messages(&table(sub))
            ),
            vec![snap.into()],
        ))
        .await?;
        Ok(())
    }

    /// Drops the entries of `sub` whose message retention removed.
    pub(crate) async fn pgmq_prune(&self, tx: &mut dyn WriteTx, sub: &str) -> Result<u64> {
        Ok(tx
            .execute(&Statement::raw(format!(
                "DELETE FROM {} q WHERE NOT EXISTS (SELECT 1 FROM _forgebase_queue_messages m \
                 WHERE m.seq = CAST(q.message->>'seq' AS BIGINT))",
                table(sub)
            )))
            .await?
            .rows_affected)
    }

    /// Delivery counts of one subscription, as the portable metrics query
    /// returns them (`ready`, `delayed`, `in_flight`, `n`, `b`, `oldest`).
    pub(crate) async fn pgmq_metrics(&self, sub: &str, now: i64) -> Result<Rows> {
        Ok(self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT \
                     coalesce(sum(CASE WHEN q.message->>'consumer' IS NULL AND q.vt <= {t} THEN 1 ELSE 0 END), 0) AS ready, \
                     coalesce(sum(CASE WHEN q.message->>'consumer' IS NULL AND q.vt > {t} THEN 1 ELSE 0 END), 0) AS delayed, \
                     coalesce(sum(CASE WHEN q.message->>'consumer' IS NOT NULL THEN 1 ELSE 0 END), 0) AS in_flight, \
                     COUNT(*) AS n, coalesce(sum(m.bytes), 0) AS b, min(m.published_at) AS oldest FROM {}",
                    join_messages(&table(sub)),
                    t = ts(1)
                ),
                vec![now.into()],
            ))
            .await?)
    }

    /// Server-wide delivery counts over every subscription's queue:
    /// `(ready, delayed, in_flight)`, one `UNION ALL` per 100 queues.
    pub(crate) async fn pgmq_backlog(&self, now: i64) -> Result<(i64, i64, i64)> {
        let rows = self
            .backend()
            .fetch_all(&Statement::raw(
                "SELECT s.name FROM _forgebase_queue_subscriptions s ORDER BY s.name",
            ))
            .await?;
        let subs: Vec<String> = Row::all(&rows).map(|r| r.text("name")).collect();
        let mut total = (0, 0, 0);
        for chunk in subs.chunks(100) {
            let parts: Vec<String> = chunk
                .iter()
                .map(|s| {
                    format!(
                        "SELECT \
                         sum(CASE WHEN message->>'consumer' IS NULL AND vt <= {t} THEN 1 ELSE 0 END) AS ready, \
                         sum(CASE WHEN message->>'consumer' IS NULL AND vt > {t} THEN 1 ELSE 0 END) AS delayed, \
                         sum(CASE WHEN message->>'consumer' IS NOT NULL THEN 1 ELSE 0 END) AS in_flight FROM {}",
                        table(s),
                        t = ts(1)
                    )
                })
                .collect();
            let rows = self
                .backend()
                .fetch_all(&Statement::new(
                    format!(
                        "SELECT coalesce(sum(ready), 0) AS ready, coalesce(sum(delayed), 0) AS delayed, \
                         coalesce(sum(in_flight), 0) AS in_flight FROM ({}) x",
                        parts.join(" UNION ALL ")
                    ),
                    vec![now.into()],
                ))
                .await?;
            let r = Row::new(&rows, 0);
            total.0 += r.int("ready");
            total.1 += r.int("delayed");
            total.2 += r.int("in_flight");
        }
        Ok(total)
    }

    /// Query plan of the pgmq claim query over a probe queue (rolled back).
    pub(crate) async fn pgmq_explain(&self, ordering: bool) -> Result<Vec<String>> {
        let probe = "forgebase/fb164-plan-probe";
        let mut tx = self.begin().await?;
        let r: Result<Vec<String>> = async {
            create_queue(&mut *tx, probe, ordering).await?;
            tx.execute(&Statement::raw("SET LOCAL enable_seqscan = off")).await?;
            let rows = tx
                .fetch_all(&Statement::new(
                    format!("EXPLAIN (COSTS OFF) {}", ready_sql(&table(probe), ordering)),
                    vec![0_i64.into(), 10_i64.into()],
                ))
                .await?;
            Ok(Row::all(&rows).map(|r| r.text("QUERY PLAN")).collect())
        }
        .await;
        tx.rollback().await?;
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_names_are_short_stable_identifiers() {
        let a = queue_name("acme/orders-worker");
        assert_eq!(a.len(), 36);
        assert!(a.starts_with("fbs_"));
        assert!(a[4..].chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(a, queue_name("acme/orders-worker"));
        assert_ne!(a, queue_name("other/orders-worker"));
        assert_eq!(table("acme/orders-worker"), format!("pgmq.q_{a}"));
    }

    #[test]
    fn ack_ids_carry_their_entry() {
        let lease = new_lease(42);
        assert_eq!(entry_of(&lease), Some(42));
        // Portable ack ids (`ack-<uuid>`) and garbage name no entry.
        assert_eq!(entry_of(&format!("ack-{}", uuid::Uuid::new_v4().simple())), None);
        assert_eq!(entry_of("nope"), None);
        assert_eq!(entry_of("ack-x-y"), None);
    }
}
