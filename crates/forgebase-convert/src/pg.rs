//! The PostgreSQL side: one plain `sqlx` connection for the copy (`COPY`
//! both ways), the catalog reads and the identity resets.

use futures_util::TryStreamExt as _;
use sqlx::{AssertSqlSafe, Connection as _, PgConnection, Row as _};

use crate::ConvertError;
use crate::canon::Kind;

/// A column of a PostgreSQL table.
#[derive(Debug, Clone)]
pub(crate) struct PgColumn {
    pub name: String,
    pub kind: Kind,
    pub generated: bool,
    pub identity: bool,
}

/// A base table in schema `forgebase` or `public`.
#[derive(Debug, Clone)]
pub(crate) struct PgTable {
    pub schema: String,
    pub name: String,
    pub columns: Vec<PgColumn>,
    /// Primary-key column names, in key order (empty: none).
    pub pk: Vec<String>,
}

impl PgTable {
    /// `schema."name"`.
    pub fn qualified(&self) -> String {
        format!("{}.{}", quote(&self.schema), quote(&self.name))
    }
}

/// `"ident"`.
pub(crate) fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// `'text'`.
pub(crate) fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Opens the copy connection with the session settings the text format
/// relies on.
pub(crate) async fn connect(url: &str) -> Result<PgConnection, ConvertError> {
    let mut conn = PgConnection::connect(url).await?;
    sqlx::raw_sql(
        "SET search_path = forgebase, public; SET bytea_output = 'hex'; SET extra_float_digits = 1; \
         SET client_encoding = 'UTF8'; SET statement_timeout = 0; SET lock_timeout = 0; \
         SET idle_in_transaction_session_timeout = 0",
    )
    .execute(&mut conn)
    .await?;
    Ok(conn)
}

/// Runs statements (simple query protocol).
pub(crate) async fn exec(conn: &mut PgConnection, sql: &str) -> Result<(), ConvertError> {
    sqlx::raw_sql(AssertSqlSafe(sql.to_owned())).execute(&mut *conn).await?;
    Ok(())
}

/// One `bigint` (or NULL) from a query.
pub(crate) async fn scalar_i64(conn: &mut PgConnection, sql: &str) -> Result<Option<i64>, ConvertError> {
    let row = sqlx::query(AssertSqlSafe(sql.to_owned()))
        .persistent(false)
        .fetch_optional(&mut *conn)
        .await?;
    Ok(match row {
        Some(r) => r.try_get::<Option<i64>, _>(0)?,
        None => None,
    })
}

/// Rows of text columns from a query (every column cast to text by the caller).
pub(crate) async fn text_rows(conn: &mut PgConnection, sql: &str) -> Result<Vec<Vec<Option<String>>>, ConvertError> {
    let rows = sqlx::query(AssertSqlSafe(sql.to_owned()))
        .persistent(false)
        .fetch_all(&mut *conn)
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let mut vals = Vec::with_capacity(r.len());
        for i in 0..r.len() {
            vals.push(r.try_get::<Option<String>, _>(i)?);
        }
        out.push(vals);
    }
    Ok(out)
}

/// Whether the connection's role is a superuser (may set
/// `session_replication_role`).
pub(crate) async fn is_superuser(conn: &mut PgConnection) -> Result<bool, ConvertError> {
    let v = scalar_i64(
        conn,
        "SELECT CASE WHEN rolsuper THEN 1 ELSE 0 END::bigint FROM pg_roles WHERE rolname = current_user",
    )
    .await?;
    Ok(v == Some(1))
}

/// [`forgebase_db::placement::EXTENSION_TABLES`] as a SQL list of literals.
pub(crate) fn extension_tables_sql() -> String {
    forgebase_db::placement::EXTENSION_TABLES
        .iter()
        .map(|t| literal(t))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Base tables of schemas `forgebase` and `public` with their columns and
/// primary keys: system tables first, then collection tables, by name.
///
/// The tables an accelerator's extension DDL creates
/// ([`forgebase_db::placement::EXTENSION_TABLES`], the Apache AGE id map and
/// build state) are left out: they are derived, exist only where the
/// extension runs, and are rebuilt there after a conversion.
pub(crate) async fn tables(conn: &mut PgConnection) -> Result<Vec<PgTable>, ConvertError> {
    let list = text_rows(
        conn,
        &format!(
            "SELECT n.nspname::text, c.relname::text, c.oid::bigint::text FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.relkind IN ('r', 'p') AND NOT c.relispartition AND n.nspname IN ('forgebase', 'public') \
             AND NOT (n.nspname = 'forgebase' AND c.relname IN ({derived})) \
             ORDER BY n.nspname = 'public', c.relname COLLATE \"C\"",
            derived = extension_tables_sql()
        ),
    )
    .await?;
    let mut out = Vec::with_capacity(list.len());
    for row in list {
        let [Some(schema), Some(name), Some(oid)] = <[Option<String>; 3]>::try_from(row).unwrap_or_default() else {
            continue;
        };
        let oid: i64 = oid.parse().unwrap_or_default();
        let cols = text_rows(
            conn,
            &format!(
                "SELECT a.attname::text, (CASE WHEN t.typtype = 'd' THEN bt.typname ELSE t.typname END)::text, \
                 (a.attgenerated <> '')::text, (a.attidentity <> '' OR pg_get_serial_sequence({rel}, a.attname) IS NOT NULL)::text \
                 FROM pg_attribute a JOIN pg_type t ON t.oid = a.atttypid LEFT JOIN pg_type bt ON bt.oid = t.typbasetype \
                 WHERE a.attrelid = {oid}::oid AND a.attnum > 0 AND NOT a.attisdropped ORDER BY a.attnum",
                rel = literal(&format!("{}.{}", quote(&schema), quote(&name))),
            ),
        )
        .await?;
        let columns = cols
            .into_iter()
            .filter_map(|c| {
                let mut it = c.into_iter();
                let name = it.next().flatten()?;
                let typname = it.next().flatten().unwrap_or_default();
                let generated = it.next().flatten().as_deref() == Some("true");
                let identity = it.next().flatten().as_deref() == Some("true");
                Some(PgColumn {
                    kind: Kind::of_pg(&typname),
                    name,
                    generated,
                    identity,
                })
            })
            .collect();
        let pk = text_rows(
            conn,
            &format!(
                "SELECT a.attname::text FROM pg_index i \
                 CROSS JOIN LATERAL unnest(i.indkey) WITH ORDINALITY AS k(attnum, ord) \
                 JOIN pg_attribute a ON a.attrelid = i.indrelid AND a.attnum = k.attnum \
                 WHERE i.indrelid = {oid}::oid AND i.indisprimary ORDER BY k.ord"
            ),
        )
        .await?
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect();
        out.push(PgTable {
            schema,
            name,
            columns,
            pk,
        });
    }
    Ok(out)
}

/// Index names on tables of schema `public` that are not primary-key or
/// unique-constraint indexes, with their table and definition.
pub(crate) async fn public_indexes(conn: &mut PgConnection) -> Result<Vec<(String, String, String)>, ConvertError> {
    let rows = text_rows(
        conn,
        "SELECT ic.relname::text, tc.relname::text, pg_get_indexdef(i.indexrelid) FROM pg_index i \
         JOIN pg_class ic ON ic.oid = i.indexrelid JOIN pg_class tc ON tc.oid = i.indrelid \
         JOIN pg_namespace n ON n.oid = tc.relnamespace \
         WHERE n.nspname = 'public' AND NOT EXISTS (SELECT 1 FROM pg_constraint c WHERE c.conindid = i.indexrelid) \
         ORDER BY 2, 1",
    )
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let mut it = r.into_iter();
            Some((it.next().flatten()?, it.next().flatten()?, it.next().flatten()?))
        })
        .collect())
}

/// Sets the sequence of every identity (or serial) column of `table` past
/// the highest value in use and past `floor` (the source's sequence
/// position). An empty table restarts at 1, or after `floor`.
pub(crate) async fn reset_identities(
    conn: &mut PgConnection,
    table: &PgTable,
    floor: impl Fn(&str) -> Option<i64>,
) -> Result<usize, ConvertError> {
    let mut n = 0;
    for c in table.columns.iter().filter(|c| c.identity && c.kind == Kind::Int) {
        let rel = literal(&table.qualified());
        let Some(seq) = text_rows(
            conn,
            &format!("SELECT pg_get_serial_sequence({rel}, {})", literal(&c.name)),
        )
        .await?
        .into_iter()
        .next()
        .and_then(|r| r.into_iter().next().flatten()) else {
            continue;
        };
        let max = scalar_i64(
            conn,
            &format!("SELECT max({})::bigint FROM {}", quote(&c.name), table.qualified()),
        )
        .await?;
        let top = match (max, floor(&c.name)) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        let sql = match top {
            Some(v) if v >= 1 => format!("SELECT setval({}, {v}, true)", literal(&seq)),
            _ => format!("SELECT setval({}, 1, false)", literal(&seq)),
        };
        exec(conn, &sql).await?;
        n += 1;
    }
    Ok(n)
}

/// Position of the sequence of `column` of `table` (the last value handed
/// out), if it has one.
pub(crate) async fn sequence_position(
    conn: &mut PgConnection,
    table: &PgTable,
    column: &str,
) -> Result<Option<i64>, ConvertError> {
    let rel = literal(&table.qualified());
    let Some(seq) = text_rows(
        conn,
        &format!("SELECT pg_get_serial_sequence({rel}, {})", literal(column)),
    )
    .await?
    .into_iter()
    .next()
    .and_then(|r| r.into_iter().next().flatten()) else {
        return Ok(None);
    };
    // `pg_sequence_last_value` is NULL until the sequence hands out a value.
    scalar_i64(
        conn,
        &format!("SELECT pg_sequence_last_value({}::regclass)", literal(&seq)),
    )
    .await
}

/// One row of `COPY … TO STDOUT` text output: raw fields, `None` = NULL.
pub(crate) type CopyRow = Vec<Option<Vec<u8>>>;

/// Streams `COPY (<select>) TO STDOUT` and hands the rows to `rows`, a
/// batch per chunk the server sends.
pub(crate) async fn copy_out(
    conn: &mut PgConnection,
    select: &str,
    mut rows: impl AsyncFnMut(Vec<CopyRow>) -> Result<(), ConvertError>,
) -> Result<(), ConvertError> {
    let sql = format!("COPY ({select}) TO STDOUT");
    let mut stream = conn.copy_out_raw(&sql).await?;
    let mut reader = crate::copytext::RowReader::default();
    let mut batch = Vec::new();
    while let Some(chunk) = stream.try_next().await? {
        reader.feed(&chunk, |r| {
            batch.push(r);
            Ok::<_, ConvertError>(())
        })?;
        if !batch.is_empty() {
            rows(std::mem::take(&mut batch)).await?;
        }
    }
    if reader.has_partial() {
        return Err(ConvertError::Copy {
            table: select.to_owned(),
            message: "the server ended COPY in the middle of a row".into(),
        });
    }
    Ok(())
}
