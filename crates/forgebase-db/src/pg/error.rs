//! SQLSTATE → [`DbError`] (ADR-0028 §7.3).
//!
//! Constraint messages are rendered like SQLite's (`UNIQUE constraint failed:
//! posts.slug, posts.lang`), because callers read the failing columns from
//! them (`forgebase_data::error::unique_fields`).

use std::path::PathBuf;

use sqlx::postgres::PgDatabaseError;

use crate::{ConstraintKind, DbError};

/// The SQLSTATE of a driver error, if the server sent one.
pub(crate) fn sqlstate(e: &sqlx::Error) -> Option<String> {
    match e {
        sqlx::Error::Database(db) => db.code().map(|c| c.into_owned()),
        _ => None,
    }
}

/// "Cached plan must not change result type": a prepared statement outlived
/// a schema change; clearing the statement cache and retrying fixes it.
pub(crate) fn is_stale_plan(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::Database(db) => {
            db.code().as_deref() == Some("0A000") && db.message().contains("cached plan must not change result type")
        }
        _ => false,
    }
}

/// Key columns from a unique violation's detail, `Key (a, b)=(1, 2) already exists.`
fn key_columns(detail: &str) -> Option<Vec<String>> {
    let rest = detail.strip_prefix("Key (")?;
    // Expression keys contain parentheses: find the `)=(` that ends the list.
    let end = rest.find(")=(")?;
    Some(
        rest[..end]
            .split(", ")
            .map(|c| c.trim_matches('"').to_owned())
            .collect(),
    )
}

fn constraint_message(kind: ConstraintKind, pg: &PgDatabaseError) -> String {
    let table = pg.table().unwrap_or_default();
    match kind {
        ConstraintKind::Unique => match pg.detail().and_then(key_columns) {
            Some(cols) if !table.is_empty() => format!(
                "UNIQUE constraint failed: {}",
                cols.iter()
                    .map(|c| format!("{table}.{c}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => format!(
                "UNIQUE constraint failed: {} ({})",
                pg.constraint().unwrap_or("unknown"),
                pg.message()
            ),
        },
        ConstraintKind::NotNull => match pg.column() {
            Some(col) => format!("NOT NULL constraint failed: {table}.{col}"),
            None => format!("NOT NULL constraint failed: {}", pg.message()),
        },
        ConstraintKind::Check if pg.code() == "23514" => {
            format!("CHECK constraint failed: {}", pg.constraint().unwrap_or("unknown"))
        }
        ConstraintKind::ForeignKey => format!(
            "FOREIGN KEY constraint failed: {}{}",
            pg.message(),
            pg.detail().map(|d| format!(" ({d})")).unwrap_or_default()
        ),
        // RAISE EXCEPTION from the immutability triggers: the message is the point.
        _ => pg.message().to_owned(),
    }
}

/// Maps a driver error onto [`DbError`]; `location` names the database in messages.
pub(crate) fn classify(e: sqlx::Error, location: &str) -> DbError {
    match &e {
        sqlx::Error::Database(db) => {
            let Some(pg) = db.try_downcast_ref::<PgDatabaseError>() else {
                return DbError::Sqlx(e);
            };
            let code = pg.code();
            let constraint = |kind| DbError::Constraint {
                kind,
                message: constraint_message(kind, pg),
            };
            match code {
                "23505" => constraint(ConstraintKind::Unique),
                "23503" => constraint(ConstraintKind::ForeignKey),
                "23514" | "P0001" => constraint(ConstraintKind::Check),
                "23502" => constraint(ConstraintKind::NotNull),
                c if c.starts_with("23") => constraint(ConstraintKind::Other),
                "25006" => DbError::ReadOnly(pg.message().to_owned()),
                "40001" | "40P01" | "55P03" => DbError::Busy(pg.message().to_owned()),
                "3D000" => DbError::NotFound(PathBuf::from(location)),
                "XX001" | "XX002" => DbError::Corrupt {
                    path: PathBuf::from(location),
                    message: pg.message().to_owned(),
                },
                c if c.starts_with("08") || c.starts_with("57P0") || c.starts_with("53") => {
                    DbError::Unavailable(pg.message().to_owned())
                }
                // Syntax and access errors (42), data exceptions (22), feature
                // not supported (0A), invalid transaction state (25), invalid
                // cursor/statement names (26, 34), cardinality (21), PL/pgSQL
                // and SQL-function errors (2F, 38, 39, P0): the statement is at fault.
                c if ["42", "22", "0A", "25", "26", "34", "21", "2F", "38", "39", "P0", "44"]
                    .iter()
                    .any(|p| c.starts_with(p)) =>
                {
                    DbError::Statement(match pg.detail() {
                        Some(d) => format!("{} ({d})", pg.message()),
                        None => pg.message().to_owned(),
                    })
                }
                _ => DbError::Sqlx(e),
            }
        }
        sqlx::Error::PoolTimedOut => DbError::Busy("timed out waiting for a database connection".into()),
        sqlx::Error::PoolClosed => DbError::Closed,
        sqlx::Error::Io(io) => DbError::Unavailable(format!("{location}: {io}")),
        sqlx::Error::Tls(tls) => DbError::Unavailable(format!("{location}: TLS: {tls}")),
        _ => DbError::Sqlx(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_detail_columns() {
        assert_eq!(
            key_columns("Key (slug, lang)=(a, en) already exists."),
            Some(vec!["slug".to_owned(), "lang".to_owned()])
        );
        assert_eq!(
            key_columns("Key (lower(email))=(x@y) already exists."),
            Some(vec!["lower(email)".to_owned()])
        );
        assert_eq!(key_columns("something else"), None);
    }
}
