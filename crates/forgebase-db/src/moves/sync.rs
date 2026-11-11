//! Engine copies of a move: staged tables, verification digests and
//! batched deletes.
//!
//! **Staged tables** are copied into `fb_move.<staging>` on the target and
//! reconciled by *bucket hashes*: rows are spread over buckets by a hash of
//! their key; each side computes per bucket the row count and an MD5 over
//! the rows' MD5s (sorted, so order and collation do not matter); buckets
//! that differ are deleted from the staging table and copied again. The
//! first pass copies everything, later passes only what changed, and the
//! pass in the freeze makes the staging table exactly equal to the source.
//!
//! **Verification** compares row counts and a SHA-256 over the sorted row
//! digests of the verified columns, or the same over a 1 % sample (by row
//! digest, so the same rows on both sides) for large tables.
//!
//! Rows travel as JSON (`row_to_json` on the source,
//! `json_populate_recordset` on the target), so no parameter typing gets in
//! the way.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::backend::{Backend, Statement, TxMode, Value, WriteTx};
use crate::{Database, DbError};

use super::MOVE_SCHEMA;
use super::pgsql::{ident, int, lit, qualified};
use super::plan::TablePlan;

/// Rows per copied page.
const PAGE: usize = 1000;
/// Buckets per reconciliation statement.
const BUCKETS_PER_STATEMENT: usize = 256;
/// Most buckets of one table.
const MAX_BUCKETS: i64 = 4096;
/// Rows per bucket aimed for.
const ROWS_PER_BUCKET: i64 = 256;

/// `ROW(a, b)::text` of columns.
pub(crate) fn row_text(cols: &[String]) -> String {
    if cols.is_empty() {
        return "''".into();
    }
    let list = cols.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ");
    if cols.len() == 1 {
        format!("CAST(ROW({list}, NULL) AS text)")
    } else {
        format!("CAST(ROW({list}) AS text)")
    }
}

/// Bucket of a row by its key, in `0..buckets`.
fn bucket_expr(key: &[String], buckets: i64) -> String {
    format!(
        "(CAST(CAST(('x' || substr(md5({}), 1, 7)) AS bit(28)) AS int) % {buckets})",
        row_text(key)
    )
}

/// The staging table of `t` on the target.
pub(crate) fn staging_table(t: &TablePlan) -> String {
    qualified(MOVE_SCHEMA, t.staging.as_deref().unwrap_or("unknown"))
}

/// Creates the staging table of `t` (the target's table shape, no
/// constraints but the primary key, no triggers).
pub(crate) async fn create_staging(target: &Database, t: &TablePlan) -> Result<(), DbError> {
    let staging = staging_table(t);
    let mut stmts = vec![format!(
        "CREATE TABLE IF NOT EXISTS {staging} (LIKE {} INCLUDING DEFAULTS)",
        t.qualified()
    )];
    if !t.key.is_empty() {
        let has_key = target
            .fetch_all(&Statement::new(
                "SELECT count(*) FROM pg_catalog.pg_index WHERE indrelid = to_regclass(CAST(?1 AS text)) AND indisprimary",
                vec![staging.clone().into()],
            ))
            .await
            .map(|r| int(&r) > 0)
            .unwrap_or(false);
        if !has_key {
            stmts.push(format!(
                "ALTER TABLE {staging} ADD PRIMARY KEY ({})",
                t.key.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ")
            ));
        }
    }
    target.run_autocommit(&stmts).await
}

/// Drops the staging table of `t`.
pub(crate) async fn drop_staging(target: &Database, t: &TablePlan) -> Result<(), DbError> {
    target
        .run_autocommit(&[format!("DROP TABLE IF EXISTS {}", staging_table(t))])
        .await
}

/// Per-bucket `(rows, digest)` of `table WHERE pred`.
async fn bucket_hashes(
    db: &dyn Backend,
    table: &str,
    pred: &str,
    t: &TablePlan,
    buckets: i64,
) -> Result<BTreeMap<i64, (i64, String)>, DbError> {
    let rows = db
        .fetch_all(&Statement::raw(format!(
            "SELECT b, count(*), md5(string_agg(h, '' ORDER BY h)) FROM \
             (SELECT {} AS b, md5({}) AS h FROM {table} WHERE {pred}) x GROUP BY b",
            bucket_expr(&t.key, buckets),
            row_text(&t.columns)
        )))
        .await?;
    Ok(rows
        .rows
        .iter()
        .filter_map(|r| {
            Some((
                r.first().and_then(Value::as_i64)?,
                (
                    r.get(1).and_then(Value::as_i64).unwrap_or(0),
                    r.get(2).and_then(Value::as_str).unwrap_or_default().to_owned(),
                ),
            ))
        })
        .collect())
}

/// What a reconciliation pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SyncStats {
    /// Rows of the tenant on the source.
    pub rows: u64,
    /// Buckets that differed.
    pub buckets: u64,
    /// Rows copied.
    pub copied: u64,
}

/// Rows of the tenant with a value in a column the target lacks.
pub(crate) async fn values_in_missing(db: &dyn Backend, t: &TablePlan) -> Result<i64, DbError> {
    if t.missing.is_empty() {
        return Ok(0);
    }
    let any = t
        .missing
        .iter()
        .map(|c| format!("{} IS NOT NULL", ident(c)))
        .collect::<Vec<_>>()
        .join(" OR ");
    count(db, &t.qualified(), &format!("({}) AND ({any})", t.predicate)).await
}

/// Number of the tenant's rows of `t` on the source.
pub(crate) async fn count(db: &dyn Backend, table: &str, pred: &str) -> Result<i64, DbError> {
    Ok(int(&db
        .fetch_all(&Statement::raw(format!("SELECT count(*) FROM {table} WHERE {pred}")))
        .await?))
}

/// Makes `t`'s staging table on the target equal to the tenant's rows on
/// the source (one reconciliation pass).
pub async fn sync_staged(source: &dyn Backend, target: &Database, t: &TablePlan) -> Result<SyncStats, DbError> {
    let src_table = t.qualified();
    let n = count(source, &src_table, &t.predicate).await?;
    let buckets = (n / ROWS_PER_BUCKET).clamp(1, MAX_BUCKETS);
    let src = bucket_hashes(source, &src_table, &t.predicate, t, buckets).await?;
    let staging = staging_table(t);
    let dst = bucket_hashes(target, &staging, "true", t, buckets).await?;
    let mut differ: Vec<i64> = src
        .iter()
        .filter(|(b, h)| dst.get(b) != Some(h))
        .map(|(b, _)| *b)
        .collect();
    differ.extend(dst.keys().filter(|b| !src.contains_key(b)));
    differ.sort_unstable();
    differ.dedup();
    let mut stats = SyncStats {
        rows: u64::try_from(n).unwrap_or(0),
        buckets: differ.len() as u64,
        copied: 0,
    };
    for chunk in differ.chunks(BUCKETS_PER_STATEMENT) {
        let list = chunk.iter().map(i64::to_string).collect::<Vec<_>>().join(", ");
        let cond = format!("{} IN ({list})", bucket_expr(&t.key, buckets));
        let mut tx = target.begin(TxMode::Normal).await?;
        tx.execute(&Statement::raw(format!("DELETE FROM {staging} WHERE {cond}")))
            .await?;
        tx.commit().await?;
        stats.copied += copy_rows(
            source,
            &src_table,
            &format!("({}) AND {cond}", t.predicate),
            t,
            |json| insert_json(target, &staging, t, json),
        )
        .await?;
    }
    Ok(stats)
}

/// Copies the rows of `table WHERE pred` page by page in key order,
/// handing each page (a JSON array) to `sink`. Returns the rows copied.
async fn copy_rows<F, Fut>(
    source: &dyn Backend,
    table: &str,
    pred: &str,
    t: &TablePlan,
    mut sink: F,
) -> Result<u64, DbError>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Result<(), DbError>>,
{
    let cols = t.columns.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ");
    let order = if t.key.is_empty() {
        row_text(&t.columns)
    } else {
        t.key.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ")
    };
    let mut after: Option<String> = None;
    let mut total = 0u64;
    loop {
        let page_cond = match &after {
            Some(a) => format!("({pred}) AND {a}"),
            None => pred.to_owned(),
        };
        let rows = source
            .fetch_all(&Statement::raw(format!(
                "SELECT CAST(row_to_json(x) AS text) FROM (SELECT {cols} FROM {table} WHERE {page_cond} \
                 ORDER BY {order} LIMIT {PAGE}) x"
            )))
            .await?;
        let jsons: Vec<&str> = rows
            .rows
            .iter()
            .filter_map(|r| r.first().and_then(Value::as_str))
            .collect();
        if jsons.is_empty() {
            return Ok(total);
        }
        total += jsons.len() as u64;
        let last = jsons.last().map(|s| (*s).to_owned()).unwrap_or_default();
        sink(format!("[{}]", jsons.join(","))).await?;
        if jsons.len() < PAGE || t.key.is_empty() {
            if t.key.is_empty() && jsons.len() == PAGE {
                return Err(DbError::Unsupported(format!(
                    "{table} has no primary key and more than {PAGE} rows to copy"
                )));
            }
            return Ok(total);
        }
        after = Some(key_after(t, &last)?);
    }
}

/// `ROW(k1, k2) > ROW(<literals of the last row>)`.
fn key_after(t: &TablePlan, last_json: &str) -> Result<String, DbError> {
    let row: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(last_json).map_err(|e| DbError::Statement(format!("copied row: {e}")))?;
    let mut lits = Vec::new();
    for k in &t.key {
        let ty = t
            .columns
            .iter()
            .position(|c| c == k)
            .and_then(|i| t.types.get(i))
            .cloned()
            .unwrap_or_else(|| "text".into());
        let v = match row.get(k) {
            Some(serde_json::Value::String(s)) => lit(s),
            Some(serde_json::Value::Null) | None => "NULL".into(),
            Some(other) => lit(&other.to_string()),
        };
        lits.push(format!("CAST({v} AS {ty})"));
    }
    let keys = t.key.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ");
    Ok(if t.key.len() == 1 {
        format!("{keys} > {}", lits[0])
    } else {
        format!("ROW({keys}) > ROW({})", lits.join(", "))
    })
}

/// `INSERT INTO dst (cols) SELECT cols FROM json_populate_recordset(…)`.
fn insert_json_sql(dst: &str, t: &TablePlan) -> String {
    let cols = t.columns.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ");
    format!(
        "INSERT INTO {dst} ({cols}) OVERRIDING SYSTEM VALUE SELECT {cols} FROM \
         json_populate_recordset(CAST(NULL AS {dst}), CAST(CAST(?1 AS text) AS json))"
    )
}

async fn insert_json(target: &Database, dst: &str, t: &TablePlan, json: String) -> Result<(), DbError> {
    let mut tx = target.begin(TxMode::Normal).await?;
    tx.execute(&Statement::raw("SET LOCAL session_replication_role = replica"))
        .await?;
    tx.execute(&Statement::new(insert_json_sql(dst, t), vec![json.into()]))
        .await?;
    tx.commit().await
}

/// Copies the rows of `t` matching `pred` on the source straight into the
/// target's table (rows of containers created during the move, copied in
/// the freeze). Returns the rows copied.
pub(crate) async fn copy_direct(
    source: &dyn Backend,
    target: &Database,
    t: &TablePlan,
    pred: &str,
) -> Result<u64, DbError> {
    let dst = t.qualified();
    copy_rows(source, &dst, pred, t, |json| insert_json(target, &dst, t, json)).await
}

/// The verification digest statement of `t` on `table WHERE pred`:
/// `(rows, sha256)` over all rows or a 1 % sample.
pub(crate) fn digest_sql(t: &TablePlan, table: &str, pred: &str, sample: bool) -> String {
    let h = format!("md5({})", row_text(&t.hashed()));
    let sample = if sample {
        " WHERE CAST(CAST(('x' || substr(h, 1, 7)) AS bit(28)) AS int) % 100 = 0"
    } else {
        ""
    };
    format!(
        "SELECT count(*), encode(sha256(convert_to(COALESCE(string_agg(h, '' ORDER BY h), ''), 'UTF8')), 'hex') \
         FROM (SELECT {h} AS h FROM {table} WHERE {pred}) x{sample}"
    )
}

/// Reads a `(rows, digest)` pair.
pub(crate) fn digest_of(rows: &crate::backend::Rows) -> (i64, String) {
    rows.rows.first().map_or((0, String::new()), |r| {
        (
            r.first().and_then(Value::as_i64).unwrap_or(0),
            r.get(1).and_then(Value::as_str).unwrap_or_default().to_owned(),
        )
    })
}

/// Deletes the rows of `table WHERE pred` in batches of `batch`, each in its
/// own transaction with triggers off (no outbox rows, no cascades: deepest
/// tables are deleted first). Returns the rows deleted.
pub(crate) async fn delete_batched(db: &dyn Backend, table: &str, pred: &str, batch: u32) -> Result<u64, DbError> {
    let mut total = 0u64;
    loop {
        let mut tx = db.begin(TxMode::Normal).await?;
        tx.execute(&Statement::raw("SET LOCAL session_replication_role = replica"))
            .await?;
        let r = tx
            .execute(&Statement::raw(format!(
                "DELETE FROM {table} WHERE ctid = ANY (ARRAY(SELECT ctid FROM {table} WHERE {pred} LIMIT {batch}))"
            )))
            .await?;
        tx.commit().await?;
        total += r.rows_affected;
        if r.rows_affected < u64::from(batch) {
            return Ok(total);
        }
    }
}

/// Runs `sql` in `tx` and returns the first integer.
pub(crate) async fn tx_int(tx: &mut dyn WriteTx, sql: String) -> Result<i64, DbError> {
    Ok(int(&tx.fetch_all(&Statement::raw(sql)).await?))
}

#[cfg(test)]
mod tests {
    use super::super::plan::Feed;
    use super::*;

    fn plan() -> TablePlan {
        TablePlan {
            schema: "forgebase".into(),
            table: "_forgebase_job_events".into(),
            feed: Feed::Stage,
            filter: None,
            predicate: "job_id IN (SELECT id FROM x)".into(),
            container: None,
            key: vec!["id".into()],
            columns: vec!["id".into(), "job_id".into(), "at".into()],
            types: vec!["bigint".into(), "text".into(), "bigint".into()],
            renumber: None,
            unhashed: vec!["id".into()],
            depth: 1,
            identity_full: false,
            staging: Some("0123_7".into()),
            missing: vec![],
        }
    }

    #[test]
    fn statements() {
        let t = plan();
        assert_eq!(row_text(&["a".into()]), "CAST(ROW(\"a\", NULL) AS text)");
        assert_eq!(staging_table(&t), "\"fb_move\".\"0123_7\"");
        assert!(bucket_expr(&t.key, 16).ends_with("% 16)"));
        let sql = insert_json_sql("\"fb_move\".\"0123_7\"", &t);
        assert!(sql.contains("json_populate_recordset(CAST(NULL AS \"fb_move\".\"0123_7\")"));
        let d = digest_sql(&t, "t", "true", true);
        assert!(d.contains("% 100 = 0"));
        assert!(
            !d.contains("\"id\", \"job_id\""),
            "the renumbered id is not verified: {d}"
        );
    }

    #[test]
    fn keyset_pages_continue_after_the_last_key() {
        let t = plan();
        assert_eq!(
            key_after(&t, r#"{"id": 41, "job_id": "j", "at": 3}"#).unwrap(),
            "\"id\" > CAST('41' AS bigint)"
        );
        let mut t2 = plan();
        t2.key = vec!["job_id".into(), "id".into()];
        assert_eq!(
            key_after(&t2, r#"{"id": 41, "job_id": "o'k", "at": 3}"#).unwrap(),
            "ROW(\"job_id\", \"id\") > ROW(CAST('o''k' AS text), CAST('41' AS bigint))"
        );
    }
}
