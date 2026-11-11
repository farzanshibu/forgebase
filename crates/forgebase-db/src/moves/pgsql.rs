//! PostgreSQL statements of a move: quoting, catalog reads, publication,
//! slot, subscription, barrier and lag.

use std::time::Duration;

use crate::backend::{Backend, Statement, Value};
use crate::{Database, DbError, SecretString};

use super::MOVE_SCHEMA;

/// `'text'` with quotes doubled.
pub(crate) fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// `"ident"` with quotes doubled.
pub(crate) fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `"schema"."table"`.
pub(crate) fn qualified(schema: &str, table: &str) -> String {
    format!("{}.{}", ident(schema), ident(table))
}

pub(crate) fn int(rows: &crate::backend::Rows) -> i64 {
    rows.scalar().and_then(Value::as_i64).unwrap_or(0)
}

pub(crate) fn texts(rows: &crate::backend::Rows) -> Vec<String> {
    rows.rows
        .iter()
        .filter_map(|r| r.first().and_then(Value::as_str).map(str::to_owned))
        .collect()
}

/// A column of a table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Column {
    pub name: String,
    /// `format_type` (e.g. `text`, `bigint`, `jsonb`).
    pub ty: String,
    /// A generated (stored) column: never inserted.
    pub generated: bool,
}

/// Whether `schema.table` is an ordinary or partitioned table.
pub(crate) async fn table_exists(db: &dyn Backend, schema: &str, table: &str) -> Result<bool, DbError> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT count(*) FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ?1 AND c.relname = ?2 AND c.relkind IN ('r', 'p')",
            vec![schema.into(), table.into()],
        ))
        .await?;
    Ok(int(&rows) > 0)
}

/// Columns in table order (dropped ones left out).
pub(crate) async fn columns(db: &dyn Backend, schema: &str, table: &str) -> Result<Vec<Column>, DbError> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), \
                    CASE WHEN a.attgenerated <> '' THEN 1 ELSE 0 END \
             FROM pg_catalog.pg_attribute a \
             WHERE a.attrelid = to_regclass(CAST(?1 AS text)) AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
            vec![qualified(schema, table).into()],
        ))
        .await?;
    Ok(rows
        .rows
        .iter()
        .map(|r| Column {
            name: r.first().and_then(Value::as_str).unwrap_or_default().to_owned(),
            ty: r.get(1).and_then(Value::as_str).unwrap_or_default().to_owned(),
            generated: r.get(2).and_then(Value::as_i64) == Some(1),
        })
        .collect())
}

/// Primary-key columns in key order.
pub(crate) async fn primary_key(db: &dyn Backend, schema: &str, table: &str) -> Result<Vec<String>, DbError> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT a.attname::text FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = ANY (i.indkey) \
             WHERE i.indrelid = to_regclass(CAST(?1 AS text)) AND i.indisprimary \
             ORDER BY array_position(CAST(i.indkey AS int2[]), a.attnum)",
            vec![qualified(schema, table).into()],
        ))
        .await?;
    Ok(texts(&rows))
}

/// The table's replica identity (`d`, `n`, `f`, `i`).
pub(crate) async fn replica_identity(db: &dyn Backend, schema: &str, table: &str) -> Result<String, DbError> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT CAST(relreplident AS text) FROM pg_catalog.pg_class WHERE oid = to_regclass(CAST(?1 AS text))",
            vec![qualified(schema, table).into()],
        ))
        .await?;
    Ok(texts(&rows).into_iter().next().unwrap_or_else(|| "d".into()))
}

/// `ALTER TABLE … REPLICA IDENTITY …` back to `old` (`d` default, `n`
/// nothing, `f` full; an index identity falls back to the default).
pub(crate) fn replica_identity_sql(qualified_table: &str, setting: &str) -> String {
    let what = match setting {
        "f" => "FULL",
        "n" => "NOTHING",
        _ => "DEFAULT",
    };
    format!("ALTER TABLE {qualified_table} REPLICA IDENTITY {what}")
}

/// Index definitions of a table by name.
pub(crate) async fn indexes(db: &dyn Backend, schema: &str, table: &str) -> Result<Vec<(String, String)>, DbError> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT indexname::text, indexdef FROM pg_catalog.pg_indexes WHERE schemaname = ?1 AND tablename = ?2 \
             ORDER BY indexname",
            vec![schema.into(), table.into()],
        ))
        .await?;
    Ok(rows
        .rows
        .iter()
        .map(|r| {
            (
                r.first().and_then(Value::as_str).unwrap_or_default().to_owned(),
                r.get(1).and_then(Value::as_str).unwrap_or_default().to_owned(),
            )
        })
        .collect())
}

/// Creates [`MOVE_SCHEMA`] and its barrier table (idempotent).
pub(crate) async fn ensure_move_schema(db: &Database) -> Result<(), DbError> {
    db.run_autocommit(&[
        format!("CREATE SCHEMA IF NOT EXISTS {MOVE_SCHEMA}"),
        format!(
            "CREATE TABLE IF NOT EXISTS {MOVE_SCHEMA}.barrier (move_id text NOT NULL, n bigint NOT NULL, \
             at text NOT NULL, PRIMARY KEY (move_id, n))"
        ),
    ])
    .await
}

/// Whether a publication exists.
pub(crate) async fn publication_exists(db: &dyn Backend, name: &str) -> Result<bool, DbError> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT count(*) FROM pg_catalog.pg_publication WHERE pubname = ?1",
            vec![name.into()],
        ))
        .await?;
    Ok(int(&rows) > 0)
}

/// Whether a replication slot exists.
pub(crate) async fn slot_exists(db: &dyn Backend, name: &str) -> Result<bool, DbError> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT count(*) FROM pg_catalog.pg_replication_slots WHERE slot_name = ?1",
            vec![name.into()],
        ))
        .await?;
    Ok(int(&rows) > 0)
}

/// Creates the move's logical slot on the source (`failover` keeps it on a
/// promoted replica, PostgreSQL 17+).
pub(crate) async fn create_slot(db: &Database, name: &str, failover: bool) -> Result<(), DbError> {
    if slot_exists(db, name).await? {
        return Ok(());
    }
    let sql = if failover {
        format!(
            "SELECT pg_catalog.pg_create_logical_replication_slot({}, 'pgoutput', false, false, true)",
            lit(name)
        )
    } else {
        format!(
            "SELECT pg_catalog.pg_create_logical_replication_slot({}, 'pgoutput')",
            lit(name)
        )
    };
    db.run_autocommit(&[sql]).await
}

/// Drops a slot if it exists (inactive).
pub(crate) async fn drop_slot(db: &Database, name: &str) -> Result<(), DbError> {
    db.run_autocommit(&[format!(
        "SELECT pg_catalog.pg_drop_replication_slot(slot_name) FROM pg_catalog.pg_replication_slots \
         WHERE slot_name = {} AND NOT active",
        lit(name)
    )])
    .await
}

/// Whether a subscription of this database exists.
pub(crate) async fn subscription_exists(db: &dyn Backend, name: &str) -> Result<bool, DbError> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT count(*) FROM pg_catalog.pg_subscription WHERE subname = ?1 AND subdbid = \
             (SELECT oid FROM pg_catalog.pg_database WHERE datname = current_database())",
            vec![name.into()],
        ))
        .await?;
    Ok(int(&rows) > 0)
}

/// `CREATE SUBSCRIPTION` on the target over the existing slot.
pub(crate) async fn create_subscription(
    target: &Database,
    name: &str,
    conninfo: &SecretString,
    failover: bool,
) -> Result<(), DbError> {
    if subscription_exists(target, name).await? {
        return Ok(());
    }
    let failover = if failover { ", failover = true" } else { "" };
    target
        .run_autocommit(&[format!(
            "CREATE SUBSCRIPTION {name} CONNECTION {} PUBLICATION {name} WITH (create_slot = false, slot_name = {}, \
             copy_data = true, origin = none, binary = true, disable_on_error = true{failover})",
            lit(conninfo.expose()),
            lit(name)
        )])
        .await
}

/// Drops the subscription without reaching the source (the slot is
/// dropped there separately).
pub(crate) async fn drop_subscription(target: &Database, name: &str) -> Result<(), DbError> {
    if !subscription_exists(target, name).await? {
        return Ok(());
    }
    target
        .run_autocommit(&[
            format!("ALTER SUBSCRIPTION {name} DISABLE"),
            format!("ALTER SUBSCRIPTION {name} SET (slot_name = NONE)"),
            format!("DROP SUBSCRIPTION {name}"),
        ])
        .await
}

/// Waits until no walsender uses the slot (after the subscription is
/// gone), at most `timeout`.
pub(crate) async fn wait_slot_inactive(db: &Database, name: &str, timeout: Duration) -> Result<(), DbError> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let rows = db
            .fetch_all(&Statement::new(
                "SELECT count(*) FROM pg_catalog.pg_replication_slots WHERE slot_name = ?1 AND active",
                vec![name.into()],
            ))
            .await?;
        if int(&rows) == 0 || tokio::time::Instant::now() >= deadline {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Subscription state on the target.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SubState {
    pub exists: bool,
    pub enabled: bool,
    pub tables: i64,
    pub ready: i64,
}

pub(crate) async fn subscription_state(target: &dyn Backend, name: &str) -> Result<SubState, DbError> {
    let rows = target
        .fetch_all(&Statement::new(
            "SELECT CASE WHEN s.subenabled THEN 1 ELSE 0 END, \
                    (SELECT count(*) FROM pg_catalog.pg_subscription_rel r WHERE r.srsubid = s.oid), \
                    (SELECT count(*) FROM pg_catalog.pg_subscription_rel r WHERE r.srsubid = s.oid AND r.srsubstate = 'r') \
             FROM pg_catalog.pg_subscription s WHERE s.subname = ?1",
            vec![name.into()],
        ))
        .await?;
    Ok(rows.rows.first().map_or_else(SubState::default, |r| {
        let n = |i: usize| r.get(i).and_then(Value::as_i64).unwrap_or(0);
        SubState {
            exists: true,
            enabled: n(0) == 1,
            tables: n(1),
            ready: n(2),
        }
    }))
}

/// The last apply error of a subscription, when PostgreSQL recorded one
/// (`pg_stat_subscription_stats`).
pub(crate) async fn subscription_errors(target: &dyn Backend, name: &str) -> Result<(i64, i64), DbError> {
    let rows = target
        .fetch_all(&Statement::new(
            "SELECT COALESCE(apply_error_count, 0), COALESCE(sync_error_count, 0) \
             FROM pg_catalog.pg_stat_subscription_stats WHERE subname = ?1",
            vec![name.into()],
        ))
        .await?;
    Ok(rows.rows.first().map_or((0, 0), |r| {
        (
            r.first().and_then(Value::as_i64).unwrap_or(0),
            r.get(1).and_then(Value::as_i64).unwrap_or(0),
        )
    }))
}

/// Bytes the slot's confirmed position is behind the source's WAL.
pub(crate) async fn slot_lag(source: &dyn Backend, slot: &str) -> Result<Option<i64>, DbError> {
    let rows = source
        .fetch_all(&Statement::new(
            "SELECT CAST(pg_catalog.pg_wal_lsn_diff(pg_catalog.pg_current_wal_lsn(), confirmed_flush_lsn) AS bigint) \
             FROM pg_catalog.pg_replication_slots WHERE slot_name = ?1",
            vec![slot.into()],
        ))
        .await?;
    Ok(rows.scalar().and_then(Value::as_i64))
}

/// Bytes copied so far by the subscription's initial copy (sum over the
/// source's `pg_stat_progress_copy`).
pub(crate) async fn copy_bytes(source: &dyn Backend) -> Result<i64, DbError> {
    let rows = source
        .fetch_all(&Statement::raw(
            "SELECT CAST(COALESCE(sum(bytes_processed), 0) AS bigint) FROM pg_catalog.pg_stat_progress_copy \
             WHERE command = 'COPY TO'",
        ))
        .await?;
    Ok(int(&rows))
}

/// Writes barrier `n` of move `id` on the source (published).
pub(crate) async fn write_barrier(source: &dyn Backend, id: &str, n: i64) -> Result<(), DbError> {
    let mut tx = source.begin(crate::TxMode::Normal).await?;
    tx.execute(&Statement::new(
        format!("INSERT INTO {MOVE_SCHEMA}.barrier (move_id, n, at) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING"),
        vec![id.into(), n.into(), forgebase_core::now_rfc3339().into()],
    ))
    .await?;
    tx.commit().await
}

/// Whether barrier `n` of move `id` reached the target.
pub(crate) async fn barrier_arrived(target: &dyn Backend, id: &str, n: i64) -> Result<bool, DbError> {
    let rows = target
        .fetch_all(&Statement::new(
            format!("SELECT count(*) FROM {MOVE_SCHEMA}.barrier WHERE move_id = ?1 AND n = ?2"),
            vec![id.into(), n.into()],
        ))
        .await?;
    Ok(int(&rows) > 0)
}

/// Removes the move's barrier rows (both sets).
pub(crate) async fn clear_barrier(db: &dyn Backend, id: &str) -> Result<(), DbError> {
    let mut tx = db.begin(crate::TxMode::Normal).await?;
    tx.execute(&Statement::new(
        format!("DELETE FROM {MOVE_SCHEMA}.barrier WHERE move_id = ?1"),
        vec![id.into()],
    ))
    .await?;
    tx.commit().await
}

/// Whether the source set has replicas that a `failover` slot should
/// survive onto (PostgreSQL 17+).
pub(crate) async fn supports_failover_slots(db: &dyn Backend) -> Result<bool, DbError> {
    let rows = db
        .fetch_all(&Statement::raw(
            "SELECT CAST(current_setting('server_version_num') AS bigint)",
        ))
        .await?;
    Ok(int(&rows) >= 170_000)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(lit("o'k"), "'o''k'");
        assert_eq!(ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(qualified("forgebase", "t"), "\"forgebase\".\"t\"");
        assert_eq!(replica_identity_sql("x", "f"), "ALTER TABLE x REPLICA IDENTITY FULL");
        assert_eq!(replica_identity_sql("x", "i"), "ALTER TABLE x REPLICA IDENTITY DEFAULT");
    }
}
