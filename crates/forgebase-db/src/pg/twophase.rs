//! Two-phase commit on PostgreSQL (ADR-0029 §3.7, FB-173): the engine side
//! of [`crate::twophase`].
//!
//! * `PREPARE TRANSACTION '<gid>'` ends a write transaction on the writer;
//!   the connection goes back to the pool and the prepared transaction
//!   lives on in the server (it survives a crash) until `COMMIT PREPARED`
//!   or `ROLLBACK PREPARED` runs on any connection.
//! * Those two cannot run inside a transaction block: they run on the
//!   writer connection outside one (the writer is free once the prepare
//!   returned).
//! * Prepared DDL becomes visible at `COMMIT PREPARED`, so the schema
//!   generation is bumped then (readers clear their statement caches).

use std::sync::atomic::Ordering;

use sqlx::{AssertSqlSafe, Connection};

use super::{PgDatabase, PgTx, error};
use crate::DbError;
use crate::backend::{Statement, TxMode, Value};

/// Longest transaction identifier PostgreSQL takes (`GIDSIZE` − 1).
pub(crate) const MAX_GID_LEN: usize = 199;

/// Lower-case letters, digits and `_`, 1–199 bytes: safe to inline in
/// `PREPARE TRANSACTION '<gid>'` (the statement takes no parameters).
pub(crate) fn check_gid(gid: &str) -> Result<(), DbError> {
    let ok = !gid.is_empty()
        && gid.len() <= MAX_GID_LEN
        && gid
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(DbError::Statement(format!("invalid transaction identifier {gid:?}")))
    }
}

impl PgTx {
    /// `PREPARE TRANSACTION`: the first phase. A failure rolls back.
    pub(super) async fn finish_prepare(mut self, gid: String) -> Result<(), DbError> {
        let Some(mut conn) = self.conn.take() else {
            return Err(DbError::Closed);
        };
        let db = self.db.clone();
        if let Err(e) = check_gid(&gid) {
            if sqlx::raw_sql("ROLLBACK").execute(&mut *conn).await.is_err() {
                conn.close_on_drop();
            }
            return Err(e);
        }
        if self.failed {
            if sqlx::raw_sql("ROLLBACK").execute(&mut *conn).await.is_err() {
                conn.close_on_drop();
            }
            return Err(DbError::Statement(
                "the transaction was rolled back because an earlier statement in it failed".into(),
            ));
        }
        let refresh = self.mode == TxMode::SchemaChange || self.ddl;
        let result = sqlx::raw_sql(AssertSqlSafe(format!("PREPARE TRANSACTION '{gid}'")))
            .execute(&mut *conn)
            .await;
        match result {
            Ok(_) => {
                if refresh {
                    // The writer cleared its own cache after the DDL; readers
                    // clear theirs once `COMMIT PREPARED` bumps again.
                    db.inner
                        .writer_gen
                        .store(db.inner.schema_gen.load(Ordering::Acquire), Ordering::Release);
                }
                Ok(())
            }
            Err(e) => {
                // A failed PREPARE ends the transaction; make sure.
                if sqlx::raw_sql("ROLLBACK").execute(&mut *conn).await.is_err() {
                    conn.close_on_drop();
                }
                Err(prepare_error(&db, e))
            }
        }
    }
}

/// `55000` "prepared transactions are disabled" reads better as what to change.
fn prepare_error(db: &PgDatabase, e: sqlx::Error) -> DbError {
    if error::sqlstate(&e).as_deref() == Some("55000") && e.to_string().contains("prepared transactions are disabled") {
        return DbError::Unsupported(format!(
            "{} has max_prepared_transactions = 0; database shards need it above 0 (16 on managed nodes)",
            db.inner.location
        ));
    }
    db.classify(e)
}

impl PgDatabase {
    /// `COMMIT PREPARED` (`commit`) or `ROLLBACK PREPARED`. `Ok(false)` when
    /// no prepared transaction has that identifier (already finished).
    pub(crate) async fn finish_prepared(&self, gid: &str, commit: bool) -> Result<bool, DbError> {
        check_gid(gid)?;
        let verb = if commit { "COMMIT" } else { "ROLLBACK" };
        let mut conn = self.inner.writer.acquire().await.map_err(|e| self.classify(e))?;
        let r = sqlx::raw_sql(AssertSqlSafe(format!("{verb} PREPARED '{gid}'")))
            .execute(&mut *conn)
            .await;
        match r {
            Ok(_) => {
                if commit {
                    self.bump_schema_generation();
                }
                Ok(true)
            }
            Err(e) if error::sqlstate(&e).as_deref() == Some("42704") => Ok(false),
            Err(e) => {
                // The writer must not go back to the pool in an odd state.
                if matches!(e, sqlx::Error::Io(_) | sqlx::Error::Protocol(_)) {
                    conn.close_on_drop();
                }
                Err(self.classify(e))
            }
        }
    }

    /// Prepared transactions of this database whose identifier starts with
    /// `prefix`: `(gid, seconds since PREPARE, owner)`.
    pub(crate) async fn prepared_xacts(&self, prefix: &str) -> Result<Vec<(String, f64)>, DbError> {
        let rows = self
            .read(&Statement::new(
                "SELECT gid, CAST(extract(epoch FROM (now() - prepared)) AS double precision) AS age \
                 FROM pg_catalog.pg_prepared_xacts \
                 WHERE database = current_database() AND starts_with(gid, ?1) ORDER BY prepared",
                vec![Value::Text(prefix.to_owned())],
            ))
            .await?;
        Ok(rows
            .rows
            .iter()
            .filter_map(|r| {
                let gid = r.first()?.as_str()?.to_owned();
                let age = match r.get(1)? {
                    Value::Real(f) => *f,
                    Value::Integer(i) => *i as f64,
                    _ => 0.0,
                };
                Some((gid, age))
            })
            .collect())
    }

    /// Runs statements that may not run in a transaction block (`CREATE
    /// SUBSCRIPTION`, `CREATE INDEX CONCURRENTLY`) on a connection of their
    /// own with the writer's options, in order, each in autocommit,
    /// stopping at the first error.
    pub(crate) async fn run_autocommit(&self, statements: &[String]) -> Result<(), DbError> {
        if self.inner.read_only {
            return Err(DbError::ReadOnly("these statements need the primary".into()));
        }
        let opts = (*self.inner.writer.connect_options()).clone();
        let mut conn = sqlx::PgConnection::connect_with(&opts)
            .await
            .map_err(|e| self.classify(e))?;
        let mut result = Ok(());
        for sql in statements {
            if let Err(e) = sqlx::raw_sql(AssertSqlSafe(sql.clone())).execute(&mut conn).await {
                result = Err(self.classify(e));
                break;
            }
        }
        let _ = conn.close().await;
        if result.is_ok() {
            self.bump_schema_generation();
        }
        result
    }

    /// `current_setting(name)` as text.
    pub(crate) async fn setting(&self, name: &str) -> Result<Option<String>, DbError> {
        let rows = self
            .read(&Statement::new(
                "SELECT current_setting(?1, true)",
                vec![Value::Text(name.to_owned())],
            ))
            .await?;
        Ok(rows.scalar().and_then(|v| v.as_str()).map(str::to_owned))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_identifiers_are_checked() {
        assert!(check_gid("fb_0123abcd_1").is_ok());
        assert!(check_gid("").is_err());
        assert!(check_gid("fb_'; DROP TABLE x; --").is_err());
        assert!(check_gid("FB_upper").is_err());
        assert!(check_gid(&"a".repeat(MAX_GID_LEN)).is_ok());
        assert!(check_gid(&"a".repeat(MAX_GID_LEN + 1)).is_err());
    }
}
