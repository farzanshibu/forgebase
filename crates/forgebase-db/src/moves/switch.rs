//! The target transaction of a switch (ADR-0029 §3.8 step 4.4): staged rows
//! go into the real tables, order-defining rows are renumbered with the
//! target's sequences in source order, references to them are remapped,
//! and the sequences of whole moved tables are set. Runs with triggers off
//! (`session_replication_role = replica`), like the subscription's apply.

use std::collections::BTreeMap;

use crate::DbError;
use crate::backend::{Statement, WriteTx};

use super::pgsql::{ident, lit, qualified};
use super::plan::{Feed, MovePlan, SYSTEM_SCHEMA, TablePlan};
use super::sync::{staging_table, tx_int};

const MESSAGES: &str = "_forgebase_queue_messages";
const DELIVERIES: &str = "_forgebase_queue_deliveries";
const SUBSCRIPTIONS: &str = "_forgebase_queue_subscriptions";
const SNAPSHOTS: &str = "_forgebase_queue_snapshots";
const SNAPSHOT_ITEMS: &str = "_forgebase_queue_snapshot_items";
const TOPICS: &str = "_forgebase_queue_topics";
const OUTBOX: &str = "_forgebase_realtime_outbox";
/// Map of renumbered message positions (temporary, dropped at commit).
const SEQ_MAP: &str = "fbm_seq_map";

fn sys<'a>(plan: &'a MovePlan, table: &str) -> Option<&'a TablePlan> {
    plan.table(&format!("{SYSTEM_SCHEMA}.{table}"))
}

async fn exec(tx: &mut dyn WriteTx, sql: String) -> Result<u64, DbError> {
    Ok(tx.execute(&Statement::raw(sql)).await?.rows_affected)
}

/// Runs the switch inside `tx` (on the target). Returns rows written per
/// table (`schema.table`).
pub(crate) async fn apply(tx: &mut dyn WriteTx, plan: &MovePlan) -> Result<BTreeMap<String, u64>, DbError> {
    let mut written = BTreeMap::new();
    exec(tx, "SET LOCAL session_replication_role = replica".into()).await?;
    let messages = sys(plan, MESSAGES).filter(|t| t.renumber.is_some());
    if let Some(m) = messages {
        renumber_messages(tx, plan, m, &mut written).await?;
    }
    for t in plan.of(Feed::Stage) {
        let n = insert_staged(tx, t, messages.is_some()).await?;
        written.insert(t.name(), n);
    }
    if let Some(topics) = sys(plan, TOPICS)
        && tx_int(
            tx,
            format!(
                "SELECT count(*) FROM pg_catalog.pg_class WHERE oid = to_regclass({})",
                lit(&qualified(SYSTEM_SCHEMA, OUTBOX))
            ),
        )
        .await?
            > 0
    {
        // Record-source topics relay from this set's outbox from now on.
        exec(
            tx,
            format!(
                "UPDATE {} SET source_after_seq = COALESCE((SELECT max(seq) FROM {}), 0) \
                 WHERE ({}) AND source_collection IS NOT NULL",
                topics.qualified(),
                qualified(SYSTEM_SCHEMA, OUTBOX),
                topics.predicate
            ),
        )
        .await?;
    }
    for q in &plan.queues {
        set_queue_sequence(tx, q).await?;
    }
    Ok(written)
}

/// Renumbers the tenant's queue messages in `seq` order and remaps
/// deliveries, pgmq messages, subscription and snapshot cursors.
async fn renumber_messages(
    tx: &mut dyn WriteTx,
    plan: &MovePlan,
    m: &TablePlan,
    written: &mut BTreeMap<String, u64>,
) -> Result<(), DbError> {
    let Some(r) = &m.renumber else { return Ok(()) };
    let seq = lit(&r.sequence);
    exec(
        tx,
        format!(
            "CREATE TEMPORARY TABLE {SEQ_MAP} ON COMMIT DROP AS \
             SELECT o.seq AS old, o.topic AS topic, nextval({seq}) AS new \
             FROM (SELECT seq, topic FROM {} WHERE {} ORDER BY seq) o",
            m.qualified(),
            m.predicate
        ),
    )
    .await?;
    exec(tx, format!("CREATE UNIQUE INDEX ON {SEQ_MAP} (old)")).await?;
    let disorder = tx_int(
        tx,
        format!("SELECT count(*) FROM (SELECT new < lag(new) OVER (ORDER BY old) AS bad FROM {SEQ_MAP}) x WHERE bad"),
    )
    .await?;
    if disorder > 0 {
        return Err(DbError::Statement("renumbering queue messages lost their order".into()));
    }
    let n = exec(
        tx,
        format!(
            "UPDATE {} t SET seq = s.new FROM {SEQ_MAP} s WHERE t.seq = s.old",
            m.qualified()
        ),
    )
    .await?;
    written.insert(m.name(), n);
    if let Some(d) = sys(plan, DELIVERIES) {
        exec(
            tx,
            format!(
                "UPDATE {} t SET seq = s.new FROM {SEQ_MAP} s WHERE t.seq = s.old AND ({})",
                d.qualified(),
                d.predicate
            ),
        )
        .await?;
    }
    for q in plan.of(Feed::Whole).filter(|t| t.schema == "pgmq") {
        exec(
            tx,
            format!(
                "UPDATE {} t SET message = jsonb_set(t.message, '{{seq}}', to_jsonb(s.new)) FROM {SEQ_MAP} s \
                 WHERE CAST(t.message ->> 'seq' AS bigint) = s.old",
                q.qualified()
            ),
        )
        .await?;
    }
    // A cursor keeps its place between the renumbered messages of its
    // topic; with none left it is the target sequence's last value.
    let last = format!(
        "(SELECT CASE WHEN is_called THEN last_value ELSE last_value - 1 END FROM {})",
        r.sequence
    );
    for table in [SUBSCRIPTIONS, SNAPSHOTS] {
        let Some(t) = sys(plan, table) else { continue };
        exec(
            tx,
            format!(
                "UPDATE {} c SET cursor_seq = COALESCE(\
                   (SELECT max(s.new) FROM {SEQ_MAP} s WHERE s.topic = c.topic AND s.old <= c.cursor_seq), \
                   (SELECT min(s.new) - 1 FROM {SEQ_MAP} s WHERE s.topic = c.topic AND s.old > c.cursor_seq), \
                   {last}) \
                 WHERE {}",
                t.qualified(),
                t.predicate
            ),
        )
        .await?;
    }
    Ok(())
}

/// Inserts a staged table's rows into the real table: renumbered tables
/// take the target's sequence in source order; snapshot items follow the
/// renumbered messages.
async fn insert_staged(tx: &mut dyn WriteTx, t: &TablePlan, have_map: bool) -> Result<u64, DbError> {
    let staging = staging_table(t);
    let cols: Vec<String> = t.columns.iter().map(|c| ident(c)).collect();
    let list = cols.join(", ");
    let sql = if let Some(r) = &t.renumber {
        let select: Vec<String> = t
            .columns
            .iter()
            .map(|c| {
                if *c == r.column {
                    format!("nextval({})", lit(&r.sequence))
                } else {
                    ident(c)
                }
            })
            .collect();
        format!(
            "INSERT INTO {} ({list}) OVERRIDING SYSTEM VALUE SELECT {} FROM \
             (SELECT * FROM {staging} ORDER BY {}) s",
            t.qualified(),
            select.join(", "),
            ident(&r.column)
        )
    } else if t.table == SNAPSHOT_ITEMS && t.schema == SYSTEM_SCHEMA && have_map {
        let select: Vec<String> = t
            .columns
            .iter()
            .map(|c| {
                if c == "seq" {
                    "COALESCE(m.new, s.seq)".to_owned()
                } else {
                    format!("s.{}", ident(c))
                }
            })
            .collect();
        format!(
            "INSERT INTO {} ({list}) SELECT {} FROM {staging} s LEFT JOIN {SEQ_MAP} m ON m.old = s.seq",
            t.qualified(),
            select.join(", ")
        )
    } else {
        format!(
            "INSERT INTO {} ({list}) OVERRIDING SYSTEM VALUE SELECT {list} FROM {staging}",
            t.qualified()
        )
    };
    exec(tx, sql).await
}

/// Sets the msg_id sequence of a moved pgmq queue past its rows
/// (PostgreSQL does not replicate sequences).
async fn set_queue_sequence(tx: &mut dyn WriteTx, queue: &str) -> Result<(), DbError> {
    let q = qualified("pgmq", &format!("q_{queue}"));
    let a = qualified("pgmq", &format!("a_{queue}"));
    let has_archive = tx_int(
        tx,
        format!(
            "SELECT count(*) FROM pg_catalog.pg_class WHERE oid = to_regclass({})",
            lit(&a)
        ),
    )
    .await?
        > 0;
    let max = if has_archive {
        format!("GREATEST(COALESCE((SELECT max(msg_id) FROM {q}), 0), COALESCE((SELECT max(msg_id) FROM {a}), 0))")
    } else {
        format!("COALESCE((SELECT max(msg_id) FROM {q}), 0)")
    };
    exec(
        tx,
        format!(
            "SELECT setval(pg_catalog.pg_get_serial_sequence({}, 'msg_id'), GREATEST({max}, 1), {max} > 0)",
            lit(&q)
        ),
    )
    .await?;
    Ok(())
}
