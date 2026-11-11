//! The SQLite side: one plain `sqlx` connection for the copy, with foreign
//! keys off (checked with `PRAGMA foreign_key_check` before commit).

use std::path::Path;
use std::time::Duration;

use forgebase_db::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqliteRow};
use sqlx::{AssertSqlSafe, ConnectOptions as _, Row as _, SqliteConnection, TypeInfo as _, ValueRef as _};

use crate::ConvertError;

/// A table of the SQLite file.
#[derive(Debug, Clone)]
pub(crate) struct SqliteTable {
    pub name: String,
    /// `CREATE VIRTUAL TABLE …` (FTS5).
    pub is_virtual: bool,
    /// Columns: name and primary-key position (0 = not in the key).
    pub columns: Vec<(String, i64)>,
}

/// `"ident"`.
pub(crate) fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Opens the file for the copy (foreign keys off, long busy timeout).
pub(crate) async fn connect(path: &Path, read_only: bool) -> Result<SqliteConnection, ConvertError> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .read_only(read_only)
        .create_if_missing(false)
        .foreign_keys(false)
        .busy_timeout(Duration::from_secs(60))
        .disable_statement_logging();
    Ok(opts.connect().await?)
}

/// Runs statements.
pub(crate) async fn exec(conn: &mut SqliteConnection, sql: &str) -> Result<(), ConvertError> {
    sqlx::raw_sql(AssertSqlSafe(sql.to_owned())).execute(&mut *conn).await?;
    Ok(())
}

/// Rows of a query as values.
pub(crate) async fn rows(conn: &mut SqliteConnection, sql: &str) -> Result<Vec<Vec<Value>>, ConvertError> {
    let rows = sqlx::query(AssertSqlSafe(sql.to_owned()))
        .persistent(false)
        .fetch_all(&mut *conn)
        .await?;
    rows.iter().map(row_values).collect()
}

/// A row's values by their stored (dynamic) type.
pub(crate) fn row_values(row: &SqliteRow) -> Result<Vec<Value>, ConvertError> {
    let mut out = Vec::with_capacity(row.len());
    for i in 0..row.len() {
        let raw = row.try_get_raw(i)?;
        if raw.is_null() {
            out.push(Value::Null);
            continue;
        }
        let ty = raw.type_info().name().to_owned();
        out.push(match ty.as_str() {
            "INTEGER" => Value::Integer(row.try_get_unchecked::<i64, _>(i)?),
            "REAL" => Value::Real(row.try_get_unchecked::<f64, _>(i)?),
            "BLOB" => Value::Blob(row.try_get_unchecked::<Vec<u8>, _>(i)?),
            _ => Value::Text(row.try_get_unchecked::<String, _>(i)?),
        });
    }
    Ok(out)
}

/// Every table of the file with its columns.
pub(crate) async fn tables(conn: &mut SqliteConnection) -> Result<Vec<SqliteTable>, ConvertError> {
    let list = rows(
        conn,
        "SELECT name, coalesce(sql, '') FROM sqlite_master WHERE type = 'table' ORDER BY name",
    )
    .await?;
    let mut out = Vec::with_capacity(list.len());
    for r in list {
        let name = r[0].as_str().unwrap_or_default().to_owned();
        let sql = r[1].as_str().unwrap_or_default().to_ascii_uppercase();
        let is_virtual = sql.starts_with("CREATE VIRTUAL TABLE");
        let columns = if is_virtual {
            Vec::new()
        } else {
            rows(
                conn,
                &format!(
                    "SELECT name, pk FROM pragma_table_info({}) ORDER BY cid",
                    literal(&name)
                ),
            )
            .await?
            .into_iter()
            .map(|c| (c[0].as_str().unwrap_or_default().to_owned(), c[1].as_i64().unwrap_or(0)))
            .collect()
        };
        out.push(SqliteTable {
            name,
            is_virtual,
            columns,
        });
    }
    Ok(out)
}

/// `'text'`.
pub(crate) fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// `sqlite_sequence` (AUTOINCREMENT positions) by table name.
pub(crate) async fn sequences(conn: &mut SqliteConnection) -> Result<Vec<(String, i64)>, ConvertError> {
    let has = rows(
        conn,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_sequence'",
    )
    .await?;
    if has.is_empty() {
        return Ok(Vec::new());
    }
    Ok(rows(conn, "SELECT name, seq FROM sqlite_sequence")
        .await?
        .into_iter()
        .filter_map(|r| Some((r[0].as_str()?.to_owned(), r[1].as_i64()?)))
        .collect())
}
