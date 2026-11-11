//! SQL of the vector tables (migration 0012). Canonical data only: every
//! function here reads or writes the database, never the ANN index. Vector
//! reads and writes take the pgvector installation when the vectors live in
//! the `vec` column (FB-162, [`crate::pg`]); they still hand the service the
//! portable byte encoding.
//!
//! Every statement on items and vectors is restricted by `collection_id`,
//! which the service resolved from the caller's tenant; user filters are
//! compiled into the same statement and ANDed in parentheses.
//! SQLite-specific SQL (FTS5, `json_each`, `RETURNING`) stays in this module.
//! Statements that apply a [`Filter`] read the items through [`ITEMS`], so a
//! collection rule over `metadata` compiles (FB-139).

use std::collections::{HashMap, HashSet};

use forgebase_data::query::SqlBuilder;
use forgebase_db::{Backend, ConstraintKind, DbError, Rows, Statement, Value, WriteTx};
use serde_json::{Map, Value as Json};

use crate::error::VectorError;
use crate::filter::Filter;
use crate::model::{Metric, Quantization, VectorField};
use crate::pg::{self, Pgv};

type Result<T> = std::result::Result<T, VectorError>;

/// The items table as rules and filters see it: every column, plus `_data`
/// again as `metadata` (the field name of [`crate::filter::item_schema`]).
/// Both engines flatten the derived table, so indexes still apply.
pub(crate) const ITEMS: &str = "(SELECT key, collection_id, id, _data, _data AS metadata, content, _version, \
     created, updated FROM _forgebase_vector_items)";

/// A collection row.
#[derive(Debug, Clone)]
pub(crate) struct CollectionRow {
    pub id: i64,
    pub tenant: String,
    pub name: String,
    pub description: String,
    pub options: Map<String, Json>,
    pub created: String,
}

/// An item row without vectors.
#[derive(Debug, Clone)]
pub(crate) struct ItemRow {
    pub key: u64,
    pub id: String,
    pub metadata: Map<String, Json>,
    pub text: Option<String>,
    pub version: i64,
    pub created: String,
    pub updated: String,
}

fn bad(what: &str) -> VectorError {
    VectorError::Internal(format!("unexpected stored value in {what}"))
}

fn int(v: &Value, what: &str) -> Result<i64> {
    v.as_i64().ok_or_else(|| bad(what))
}

fn key(v: &Value) -> Result<u64> {
    u64::try_from(int(v, "key")?).map_err(|_| bad("key"))
}

fn text(v: &Value, what: &str) -> Result<String> {
    v.as_str().map(str::to_owned).ok_or_else(|| bad(what))
}

fn opt_text(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}

fn blob(v: Value) -> Result<Vec<u8>> {
    match v {
        Value::Blob(b) => Ok(b),
        _ => Err(bad("vector")),
    }
}

fn object(v: &Value, what: &str) -> Result<Map<String, Json>> {
    match serde_json::from_str::<Json>(v.as_str().ok_or_else(|| bad(what))?) {
        Ok(Json::Object(m)) => Ok(m),
        _ => Err(bad(what)),
    }
}

fn keys_json(keys: &[u64]) -> Value {
    Value::Text(serde_json::to_string(keys).unwrap_or_else(|_| "[]".into()))
}

/// `expr` is one of the integer keys in the JSON array bound to `param`.
/// PostgreSQL compares the elements as text, so they are cast to bigint (the
/// key stays a plain bigint comparison, usable by the index).
fn key_in_list(dialect: &dyn forgebase_db::Dialect, expr: &str, param: &str) -> String {
    if dialect.name() == "postgres" {
        format!("{expr} = ANY (SELECT CAST(jsonb_array_elements_text(CAST({param} AS jsonb)) AS bigint))")
    } else {
        dialect.in_json_list(expr, param)
    }
}

fn int_of(n: u64) -> Value {
    Value::Integer(i64::try_from(n).unwrap_or(i64::MAX))
}

fn u(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

// ------------------------------------------------------------- collections

/// Every collection with its fields (all tenants), for start-up.
pub(crate) async fn load_all(db: &dyn Backend) -> Result<Vec<(CollectionRow, Vec<VectorField>)>> {
    load(db, None).await
}

/// The collections of one tenant with their fields (a tenant taken over
/// from another set after a move, FB-172).
pub(crate) async fn load_tenant(db: &dyn Backend, tenant: &str) -> Result<Vec<(CollectionRow, Vec<VectorField>)>> {
    load(db, Some(tenant)).await
}

async fn load(db: &dyn Backend, tenant: Option<&str>) -> Result<Vec<(CollectionRow, Vec<VectorField>)>> {
    let (only, only_fields, params) = match tenant {
        None => ("", "", vec![]),
        Some(t) => (
            " WHERE tenant = ?1",
            " WHERE collection_id IN (SELECT id FROM _forgebase_vector_collections WHERE tenant = ?1)",
            vec![Value::Text(t.to_owned())],
        ),
    };
    let rows = db
        .fetch_all(&Statement::new(
            format!(
                "SELECT id, tenant, name, description, options, created_at \
                 FROM _forgebase_vector_collections{only} ORDER BY id"
            ),
            params.clone(),
        ))
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows.rows {
        out.push((
            CollectionRow {
                id: int(&r[0], "collection id")?,
                tenant: text(&r[1], "tenant")?,
                name: text(&r[2], "name")?,
                description: text(&r[3], "description")?,
                options: object(&r[4], "options")?,
                created: text(&r[5], "created_at")?,
            },
            Vec::new(),
        ));
    }
    let fields = db
        .fetch_all(&Statement::new(
            format!(
                "SELECT collection_id, name, dimension, metric, quantization, connectivity, expansion_add, \
                 expansion_search, created_at FROM _forgebase_vector_fields{only_fields} ORDER BY collection_id, name"
            ),
            params,
        ))
        .await?;
    let mut by_id: HashMap<i64, usize> = out.iter().enumerate().map(|(i, (c, _))| (c.id, i)).collect();
    for r in &fields.rows {
        let cid = int(&r[0], "collection_id")?;
        let field = field_row(&r[1..])?;
        if let Some(i) = by_id.get_mut(&cid) {
            out[*i].1.push(field);
        }
    }
    Ok(out)
}

fn field_row(r: &[Value]) -> Result<VectorField> {
    let u32_of = |v: &Value, what: &str| u32::try_from(int(v, what)?).map_err(|_| bad(what));
    Ok(VectorField {
        name: text(&r[0], "field name")?,
        dimension: u32_of(&r[1], "dimension")?,
        metric: Metric::parse(&text(&r[2], "metric")?).ok_or_else(|| bad("metric"))?,
        quantization: Quantization::parse(&text(&r[3], "quantization")?).ok_or_else(|| bad("quantization"))?,
        connectivity: u32_of(&r[4], "connectivity")?,
        expansion_add: u32_of(&r[5], "expansion_add")?,
        expansion_search: u32_of(&r[6], "expansion_search")?,
        created: text(&r[7], "field created_at")?,
    })
}

/// Inserts a collection; returns its id.
pub(crate) async fn insert_collection(
    tx: &mut dyn WriteTx,
    tenant: &str,
    name: &str,
    description: &str,
    options: &Map<String, Json>,
    now: &str,
) -> Result<i64> {
    let stmt = Statement::new(
        "INSERT INTO _forgebase_vector_collections (tenant, name, description, options, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?5) RETURNING id",
        vec![
            tenant.into(),
            name.into(),
            description.into(),
            Value::Text(Json::Object(options.clone()).to_string()),
            now.into(),
        ],
    );
    match tx.fetch_all(&stmt).await {
        Ok(rows) => int(rows.scalar().ok_or_else(|| bad("insert"))?, "collection id"),
        Err(DbError::Constraint {
            kind: ConstraintKind::Unique,
            ..
        }) => Err(VectorError::CollectionExists(name.to_owned())),
        Err(e) => Err(e.into()),
    }
}

/// Inserts a field definition.
pub(crate) async fn insert_field(tx: &mut dyn WriteTx, cid: i64, f: &VectorField) -> Result<()> {
    let stmt = Statement::new(
        "INSERT INTO _forgebase_vector_fields (collection_id, name, dimension, metric, quantization, connectivity, \
         expansion_add, expansion_search, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        vec![
            cid.into(),
            f.name.as_str().into(),
            i64::from(f.dimension).into(),
            f.metric.as_str().into(),
            f.quantization.as_str().into(),
            i64::from(f.connectivity).into(),
            i64::from(f.expansion_add).into(),
            i64::from(f.expansion_search).into(),
            f.created.as_str().into(),
        ],
    );
    match tx.execute(&stmt).await {
        Ok(_) => Ok(()),
        Err(DbError::Constraint {
            kind: ConstraintKind::Unique,
            ..
        }) => Err(VectorError::FieldExists(f.name.clone())),
        Err(e) => Err(e.into()),
    }
}

/// Records a committed change: increments and returns the change sequence.
pub(crate) async fn bump(tx: &mut dyn WriteTx, cid: i64, now: &str) -> Result<i64> {
    let rows = tx
        .fetch_all(&Statement::new(
            "UPDATE _forgebase_vector_collections SET change_seq = change_seq + 1, updated_at = ?2 \
             WHERE id = ?1 RETURNING change_seq",
            vec![cid.into(), now.into()],
        ))
        .await?;
    int(rows.scalar().ok_or_else(|| bad("change_seq"))?, "change_seq")
}

/// Current change sequence and update time.
pub(crate) async fn change_seq(db: &dyn Backend, cid: i64) -> Result<(i64, String)> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT change_seq, updated_at FROM _forgebase_vector_collections WHERE id = ?1",
            vec![cid.into()],
        ))
        .await?;
    let r = rows.rows.first().ok_or_else(|| bad("change_seq"))?;
    Ok((int(&r[0], "change_seq")?, text(&r[1], "updated_at")?))
}

/// Deletes a collection and everything in it.
pub(crate) async fn delete_collection(tx: &mut dyn WriteTx, cid: i64) -> Result<()> {
    // Items first, so the FTS delete trigger runs for each; vectors cascade.
    tx.execute(&Statement::new(
        "DELETE FROM _forgebase_vector_items WHERE collection_id = ?1",
        vec![cid.into()],
    ))
    .await?;
    tx.execute(&Statement::new(
        "DELETE FROM _forgebase_vector_collections WHERE id = ?1",
        vec![cid.into()],
    ))
    .await?;
    Ok(())
}

/// Deletes a field and its vectors.
pub(crate) async fn delete_field(tx: &mut dyn WriteTx, cid: i64, name: &str) -> Result<()> {
    tx.execute(&Statement::new(
        "DELETE FROM _forgebase_vector_values WHERE collection_id = ?1 AND field = ?2",
        vec![cid.into(), name.into()],
    ))
    .await?;
    tx.execute(&Statement::new(
        "DELETE FROM _forgebase_vector_fields WHERE collection_id = ?1 AND name = ?2",
        vec![cid.into(), name.into()],
    ))
    .await?;
    Ok(())
}

// ------------------------------------------------------------------- items

/// Inserts or replaces an item; returns its key and new version.
pub(crate) async fn upsert_item(
    tx: &mut dyn WriteTx,
    cid: i64,
    id: &str,
    metadata: &Map<String, Json>,
    content: Option<&str>,
    now: &str,
) -> Result<(u64, i64)> {
    let rows = tx
        .fetch_all(&Statement::new(
            "INSERT INTO _forgebase_vector_items (collection_id, id, _data, content, created, updated) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?5) \
             ON CONFLICT (collection_id, id) DO UPDATE SET _data = excluded._data, content = excluded.content, \
             _version = _forgebase_vector_items._version + 1, updated = excluded.updated \
             RETURNING key, _version",
            vec![
                cid.into(),
                id.into(),
                Value::Text(Json::Object(metadata.clone()).to_string()),
                content.map(str::to_owned).into(),
                now.into(),
            ],
        ))
        .await?;
    let r = rows.rows.first().ok_or_else(|| bad("upsert"))?;
    Ok((key(&r[0])?, int(&r[1], "_version")?))
}

/// Replaces every vector of an item: `vector` bytea on the portable
/// engine, `vec` on pgvector.
pub(crate) async fn replace_vectors(
    tx: &mut dyn WriteTx,
    cid: i64,
    k: u64,
    vectors: &[(&str, &[f32])],
    pgv: Option<&Pgv>,
) -> Result<()> {
    tx.execute(&Statement::new(
        "DELETE FROM _forgebase_vector_values WHERE collection_id = ?1 AND key = ?2",
        vec![cid.into(), int_of(k)],
    ))
    .await?;
    for (field, v) in vectors {
        let stmt = match pgv {
            None => Statement::new(
                "INSERT INTO _forgebase_vector_values (collection_id, field, key, vector) VALUES (?1, ?2, ?3, ?4)",
                vec![
                    cid.into(),
                    (*field).into(),
                    int_of(k),
                    Value::Blob(crate::metric::encode(v)),
                ],
            ),
            Some(p) => Statement::new(
                format!(
                    "INSERT INTO _forgebase_vector_values (collection_id, field, key, vec) VALUES (?1, ?2, ?3, {})",
                    p.vector_param("?4")
                ),
                vec![cid.into(), (*field).into(), int_of(k), Value::Text(pg::text_of(v))],
            ),
        };
        tx.execute(&stmt).await?;
    }
    Ok(())
}

/// The vector column as the service reads it.
fn vector_col(pgv: Option<&Pgv>) -> &'static str {
    match pgv {
        None => "vector",
        Some(_) => "CAST(vec AS text)",
    }
}

/// A selected vector column in the portable byte encoding.
fn stored(v: Value, pgv: Option<&Pgv>) -> Result<Vec<u8>> {
    match pgv {
        None => blob(v),
        Some(_) => pg::bytes_of(&v),
    }
}

/// Deletes items by id, only those matching `filter` (the caller's delete
/// rule) when given; returns the keys removed.
pub(crate) async fn delete_items(
    tx: &mut dyn WriteTx,
    dialect: &dyn forgebase_db::Dialect,
    cid: i64,
    ids: &[String],
    filter: Option<&Filter>,
) -> Result<Vec<u64>> {
    let mut b = SqlBuilder::new(dialect);
    let p_cid = b.bind(cid);
    let p_ids = b.bind(Value::Text(serde_json::to_string(ids).unwrap_or_else(|_| "[]".into())));
    let id_in = dialect.in_json_list("id", &p_ids);
    let cond = match filter {
        Some(f) => format!(
            " AND key IN (SELECT i.key FROM {ITEMS} i WHERE i.collection_id = {p_cid} AND ({}))",
            f.compile(&mut b, "i")?
        ),
        None => String::new(),
    };
    let rows = tx
        .fetch_all(&Statement::new(
            format!(
                "DELETE FROM _forgebase_vector_items WHERE collection_id = {p_cid} \
                 AND {id_in}{cond} RETURNING key"
            ),
            b.into_params(),
        ))
        .await?;
    rows.rows.iter().map(|r| key(&r[0])).collect()
}

/// The key of item `id`, inside a write transaction.
pub(crate) async fn item_key_tx(tx: &mut dyn WriteTx, cid: i64, id: &str) -> Result<Option<u64>> {
    let rows = tx
        .fetch_all(&Statement::new(
            "SELECT key FROM _forgebase_vector_items WHERE collection_id = ?1 AND id = ?2",
            vec![cid.into(), id.into()],
        ))
        .await?;
    rows.rows.first().map(|r| key(&r[0])).transpose()
}

/// Whether item `k` matches `filter`, inside a write transaction (rule
/// checks of the stored and the written row).
pub(crate) async fn matches_tx(
    tx: &mut dyn WriteTx,
    dialect: &dyn forgebase_db::Dialect,
    cid: i64,
    k: u64,
    filter: &Filter,
) -> Result<bool> {
    let mut b = SqlBuilder::new(dialect);
    let p_cid = b.bind(cid);
    let p_key = b.bind(int_of(k));
    let cond = filter.compile(&mut b, "i")?;
    let rows = tx
        .fetch_all(&Statement::new(
            format!("SELECT 1 FROM {ITEMS} i WHERE i.collection_id = {p_cid} AND i.key = {p_key} AND ({cond})"),
            b.into_params(),
        ))
        .await?;
    Ok(!rows.rows.is_empty())
}

const ITEM_COLS: &str = "i.key, i.id, i._data, i.content, i._version, i.created, i.updated";

fn item_row(r: &[Value]) -> Result<ItemRow> {
    Ok(ItemRow {
        key: key(&r[0])?,
        id: text(&r[1], "id")?,
        metadata: object(&r[2], "_data")?,
        text: opt_text(&r[3]),
        version: int(&r[4], "_version")?,
        created: text(&r[5], "created")?,
        updated: text(&r[6], "updated")?,
    })
}

/// One item by id, when it matches `filter` (the caller's view rule).
pub(crate) async fn get_item(db: &dyn Backend, cid: i64, id: &str, filter: Option<&Filter>) -> Result<Option<ItemRow>> {
    let mut b = SqlBuilder::new(db.dialect());
    let p_cid = b.bind(cid);
    let p_id = b.bind(id);
    let cond = match filter {
        Some(f) => format!(" AND ({})", f.compile(&mut b, "i")?),
        None => String::new(),
    };
    let rows = db
        .fetch_all(&Statement::new(
            format!("SELECT {ITEM_COLS} FROM {ITEMS} i WHERE i.collection_id = {p_cid} AND i.id = {p_id}{cond}"),
            b.into_params(),
        ))
        .await?;
    rows.rows.first().map(|r| item_row(r)).transpose()
}

/// Every vector of one item, by field.
pub(crate) async fn item_vectors(
    db: &dyn Backend,
    cid: i64,
    k: u64,
    pgv: Option<&Pgv>,
) -> Result<Vec<(String, Vec<u8>)>> {
    let rows = db
        .fetch_all(&Statement::new(
            format!(
                "SELECT field, {} FROM _forgebase_vector_values WHERE collection_id = ?1 AND key = ?2 ORDER BY field",
                vector_col(pgv)
            ),
            vec![cid.into(), int_of(k)],
        ))
        .await?;
    rows.rows
        .into_iter()
        .map(|mut r| {
            let v = stored(r.pop().unwrap_or(Value::Null), pgv)?;
            Ok((text(&r[0], "field")?, v))
        })
        .collect()
}

/// Items by key (missing keys are left out).
pub(crate) async fn items_by_keys(db: &dyn Backend, cid: i64, keys: &[u64]) -> Result<HashMap<u64, ItemRow>> {
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = db
        .fetch_all(&Statement::new(
            format!(
                "SELECT {ITEM_COLS} FROM _forgebase_vector_items i \
                 WHERE i.collection_id = ?1 AND {}",
                key_in_list(db.dialect(), "i.key", "?2")
            ),
            vec![cid.into(), keys_json(keys)],
        ))
        .await?;
    rows.rows.iter().map(|r| item_row(r).map(|i| (i.key, i))).collect()
}

fn vectors_rows(rows: Rows, pgv: Option<&Pgv>) -> Result<HashMap<u64, Vec<u8>>> {
    rows.rows
        .into_iter()
        .map(|mut r| {
            let v = stored(r.pop().unwrap_or(Value::Null), pgv)?;
            Ok((key(&r[0])?, v))
        })
        .collect()
}

fn vectors_by_keys_stmt(
    dialect: &dyn forgebase_db::Dialect,
    cid: i64,
    field: &str,
    keys: &[u64],
    pgv: Option<&Pgv>,
) -> Statement {
    Statement::new(
        format!(
            "SELECT key, {} FROM _forgebase_vector_values \
             WHERE collection_id = ?1 AND field = ?2 AND {}",
            vector_col(pgv),
            key_in_list(dialect, "key", "?3")
        ),
        vec![cid.into(), field.into(), keys_json(keys)],
    )
}

/// One field's vectors of the given keys.
pub(crate) async fn vectors_by_keys(
    db: &dyn Backend,
    cid: i64,
    field: &str,
    keys: &[u64],
    pgv: Option<&Pgv>,
) -> Result<HashMap<u64, Vec<u8>>> {
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    vectors_rows(
        db.fetch_all(&vectors_by_keys_stmt(db.dialect(), cid, field, keys, pgv))
            .await?,
        pgv,
    )
}

/// One page of a field's vectors in key order, after `after`, optionally only
/// of items matching `filter`.
pub(crate) async fn scan(
    db: &dyn Backend,
    cid: i64,
    field: &str,
    filter: Option<&Filter>,
    after: u64,
    limit: usize,
) -> Result<Vec<(u64, Vec<u8>)>> {
    let mut b = SqlBuilder::new(db.dialect());
    let p_cid = b.bind(cid);
    let p_field = b.bind(field);
    let p_after = b.bind(int_of(after));
    let p_limit = b.bind(int_of(limit as u64));
    let sql = match filter {
        None => format!(
            "SELECT v.key, v.vector FROM _forgebase_vector_values v \
             WHERE v.collection_id = {p_cid} AND v.field = {p_field} AND v.key > {p_after} \
             ORDER BY v.key LIMIT {p_limit}"
        ),
        Some(f) => {
            let cond = f.compile(&mut b, "i")?;
            format!(
                "SELECT v.key, v.vector FROM _forgebase_vector_values v \
                 JOIN {ITEMS} i ON i.collection_id = v.collection_id AND i.key = v.key \
                 WHERE v.collection_id = {p_cid} AND v.field = {p_field} AND v.key > {p_after} AND ({cond}) \
                 ORDER BY v.key LIMIT {p_limit}"
            )
        }
    };
    let rows = db.fetch_all(&Statement::new(sql, b.into_params())).await?;
    rows.rows
        .into_iter()
        .map(|mut r| {
            let v = blob(r.pop().unwrap_or(Value::Null))?;
            Ok((key(&r[0])?, v))
        })
        .collect()
}

/// Keys of items that have a vector in `field` and match `filter`, at most
/// `limit`.
pub(crate) async fn matching_keys(
    db: &dyn Backend,
    cid: i64,
    field: &str,
    filter: &Filter,
    limit: usize,
) -> Result<Vec<u64>> {
    let mut b = SqlBuilder::new(db.dialect());
    let p_cid = b.bind(cid);
    let p_field = b.bind(field);
    let cond = filter.compile(&mut b, "i")?;
    let p_limit = b.bind(int_of(limit as u64));
    let sql = format!(
        "SELECT v.key FROM _forgebase_vector_values v \
         JOIN {ITEMS} i ON i.collection_id = v.collection_id AND i.key = v.key \
         WHERE v.collection_id = {p_cid} AND v.field = {p_field} AND ({cond}) LIMIT {p_limit}"
    );
    let rows = db.fetch_all(&Statement::new(sql, b.into_params())).await?;
    rows.rows.iter().map(|r| key(&r[0])).collect()
}

/// The subset of `keys` whose items match `filter`.
pub(crate) async fn verify(db: &dyn Backend, cid: i64, keys: &[u64], filter: &Filter) -> Result<HashSet<u64>> {
    if keys.is_empty() {
        return Ok(HashSet::new());
    }
    let mut b = SqlBuilder::new(db.dialect());
    let p_cid = b.bind(cid);
    let p_keys = b.bind(keys_json(keys));
    let cond = filter.compile(&mut b, "i")?;
    let key_in = key_in_list(db.dialect(), "i.key", &p_keys);
    let sql = format!(
        "SELECT i.key FROM {ITEMS} i WHERE i.collection_id = {p_cid} \
         AND {key_in} AND ({cond})"
    );
    let rows = db.fetch_all(&Statement::new(sql, b.into_params())).await?;
    rows.rows.iter().map(|r| key(&r[0])).collect()
}

/// Keyword search: keys and BM25 relevance (higher is better), best first.
pub(crate) async fn keyword(
    db: &dyn Backend,
    cid: i64,
    match_expr: &str,
    filter: Option<&Filter>,
    limit: usize,
) -> Result<Vec<(u64, f64)>> {
    let mut b = SqlBuilder::new(db.dialect());
    let p_match = b.bind(match_expr);
    let p_cid = b.bind(cid);
    let cond = match filter {
        Some(f) => format!(" AND ({})", f.compile(&mut b, "i")?),
        None => String::new(),
    };
    let p_limit = b.bind(int_of(limit as u64));
    let pg = db.dialect().name() == "postgres";
    let sql = if pg {
        // tsvector hybrid search (ADR-0028 §7.4): the generated, GIN-indexed
        // column of _forgebase_vector_fts; the rule and caller filter stay
        // pre-filters on the items (FB-139).
        format!(
            "SELECT i.key, ts_rank(f.fts, q.query) AS rank FROM _forgebase_vector_fts f \
             JOIN {ITEMS} i ON i.key = f.rowid \
             CROSS JOIN (SELECT websearch_to_tsquery('simple', _forgebase_unaccent({p_match})) AS query) q \
             WHERE f.fts @@ q.query AND i.collection_id = {p_cid}{cond} \
             ORDER BY rank DESC, i.key LIMIT {p_limit}"
        )
    } else {
        format!(
            "SELECT i.key, bm25(_forgebase_vector_fts) AS rank FROM _forgebase_vector_fts \
             JOIN {ITEMS} i ON i.key = _forgebase_vector_fts.rowid \
             WHERE _forgebase_vector_fts MATCH {p_match} AND i.collection_id = {p_cid}{cond} \
             ORDER BY rank, i.key LIMIT {p_limit}"
        )
    };
    let rows = db.fetch_all(&Statement::new(sql, b.into_params())).await?;
    rows.rows
        .iter()
        .map(|r| {
            let rank = match &r[1] {
                Value::Real(f) => *f,
                #[allow(clippy::cast_precision_loss)]
                Value::Integer(i) => *i as f64,
                _ => return Err(bad("bm25")),
            };
            // bm25 is lower-is-better, ts_rank higher-is-better.
            Ok((key(&r[0])?, if pg { rank } else { -rank }))
        })
        .collect()
}

/// Items and items with text (only those matching `filter`, the caller's
/// list rule, when given).
pub(crate) async fn count_items(db: &dyn Backend, cid: i64, filter: Option<&Filter>) -> Result<(u64, u64)> {
    let mut b = SqlBuilder::new(db.dialect());
    let p_cid = b.bind(cid);
    let cond = match filter {
        Some(f) => format!(" AND ({})", f.compile(&mut b, "i")?),
        None => String::new(),
    };
    let rows = db
        .fetch_all(&Statement::new(
            format!("SELECT count(*), count(i.content) FROM {ITEMS} i WHERE i.collection_id = {p_cid}{cond}"),
            b.into_params(),
        ))
        .await?;
    let r = rows.rows.first().ok_or_else(|| bad("count"))?;
    Ok((u(int(&r[0], "count")?), u(int(&r[1], "count")?)))
}

/// Canonical vectors of one field.
pub(crate) async fn count_vectors(db: &dyn Backend, cid: i64, field: &str) -> Result<u64> {
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT count(*) FROM _forgebase_vector_values WHERE collection_id = ?1 AND field = ?2",
            vec![cid.into(), field.into()],
        ))
        .await?;
    Ok(u(int(rows.scalar().ok_or_else(|| bad("count"))?, "count")?))
}
