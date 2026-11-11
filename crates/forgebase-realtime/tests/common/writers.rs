//! Who writes to the test database: the census behind the "no second
//! writer" tests (FB-146). Realtime and queue cursors read rows by sequence
//! number and never look back, which is correct only while sequence order is
//! commit order: one writer connection (ADR-0011, ADR-0022, ADR-0028 §7.3).
//! On PostgreSQL a second writing session would let a transaction holding a
//! lower sequence number commit after a higher one, and the cursor would
//! skip it. (SQLite allocates row ids under its file write lock, so every
//! SQLite writer is serialized anyway; there the pool check is the test.)
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use forgebase_db::{Backend, BackendKind, Database, Statement, TxMode, Value};

/// What a session that holds a transaction id (it wrote in its open
/// transaction) is, other than ForgeBase's own writer connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Writer {
    /// pg_cron running `forgebase.fire_schedule(…)` (ADR-0029 §1.7): it
    /// writes only schedule and job rows through that function.
    PgCronFireSchedule,
    /// A logical-replication apply worker during a tenant move (ADR-0029
    /// §3.8): it applies the source's committed changes.
    ReplicationApply,
    /// Engine maintenance (autovacuum's ANALYZE writes `pg_statistic`, never
    /// ForgeBase rows).
    Maintenance,
    /// Anything else: a second writer.
    Second(String),
}

impl Writer {
    /// One of the two writers ADR-0029 allows besides ForgeBase's own, or
    /// engine maintenance.
    pub fn allowed(&self) -> bool {
        !matches!(self, Writer::Second(_))
    }
}

/// Classifies a writing session from `pg_stat_activity`.
pub fn classify(backend_type: &str, application_name: &str, query: &str) -> Writer {
    let q = query.trim().trim_end_matches(';').trim().to_ascii_lowercase();
    let fire = q.starts_with("select forgebase.fire_schedule(") && q.ends_with(')') && !q.contains(';');
    match backend_type {
        // pg_cron runs jobs in background workers (`cron.use_background_workers`)
        // or over libpq as a client named `pg_cron`.
        "pg_cron" if fire => Writer::PgCronFireSchedule,
        "client backend" if application_name == "pg_cron" && fire => Writer::PgCronFireSchedule,
        "logical replication apply worker"
        | "logical replication parallel worker"
        | "logical replication tablesync worker"
        | "logical replication worker" => Writer::ReplicationApply,
        "autovacuum worker" => Writer::Maintenance,
        other => Writer::Second(format!("{other} {application_name:?}: {query}")),
    }
}

/// The process id of the writer connection of `db` (PostgreSQL).
pub async fn writer_pid(db: &Database) -> i64 {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    let rows = tx.fetch_all(&Statement::raw("SELECT pg_backend_pid()")).await.unwrap();
    tx.rollback().await.unwrap();
    rows.scalar().and_then(Value::as_i64).unwrap()
}

/// Sessions of the database other than `writer` that hold a transaction id
/// right now, classified. Empty on SQLite.
pub async fn census(db: &Database, writer: i64) -> Vec<(i64, Writer)> {
    if db.kind() != BackendKind::Postgres {
        return Vec::new();
    }
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT pid, backend_type, coalesce(application_name, '') AS app, coalesce(query, '') AS query \
             FROM pg_stat_activity WHERE datname = current_database() AND backend_xid IS NOT NULL \
             AND pid <> pg_backend_pid() AND pid <> ?1",
            vec![writer.into()],
        ))
        .await
        .unwrap();
    rows.rows
        .iter()
        .map(|r| {
            let text = |i: usize| r[i].as_str().unwrap_or_default().to_owned();
            (r[0].as_i64().unwrap(), classify(&text(1), &text(2), &text(3)))
        })
        .collect()
}
