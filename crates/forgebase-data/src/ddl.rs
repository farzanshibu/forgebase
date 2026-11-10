//! DDL rendering for collection tables (ADR-0003 §4, ADR-0028 §7.4-§7.5).
//!
//! Column types come from the dialect's type mapping (`TEXT` / `text`,
//! `INTEGER` / `bigint`, JSON as checked `TEXT` / `jsonb`, booleans as checked
//! integers on both). On PostgreSQL collection tables and views live in
//! schema [`PG_SCHEMA`]: system tables are in schema `forgebase`, which comes
//! first on the server's search path, so every statement that *creates* or
//! *drops* a collection object names the schema ([`rel`]); reads keep
//! unqualified names, which resolve to `public` because collection names can
//! never start with `_forgebase`.

use forgebase_db::{ColumnType, Dialect, Statement, Value, WriteTx};
use forgebase_schema::{Collection, Field, FieldKind, Storage};
use serde_json::Value as Json;

use crate::DataError;
use crate::access::{TENANT_COLUMN, is_tenant_scoped};

/// Prefix of the temporary table used while a table is rebuilt (SQLite).
pub(crate) const TMP_PREFIX: &str = "_forgebase_tmp_";

/// Schema of collection tables and views on PostgreSQL (ADR-0028 §7.6).
pub(crate) const PG_SCHEMA: &str = "public";

/// Whether `d` renders PostgreSQL.
pub(crate) fn is_pg(d: &dyn Dialect) -> bool {
    d.name() == "postgres"
}

/// A collection table, view or index in a DDL statement that names an
/// existing object or creates a table or view: `"name"` on SQLite,
/// `public."name"` on PostgreSQL. (`CREATE INDEX` and `RENAME TO` take a bare
/// name; the object stays in its table's schema.)
pub(crate) fn rel(d: &dyn Dialect, name: &str) -> String {
    if is_pg(d) {
        format!("{PG_SCHEMA}.{}", d.quote_ident(name))
    } else {
        d.quote_ident(name)
    }
}

/// `public."name"` as a text value for `to_regclass(?)` on PostgreSQL.
pub(crate) fn pg_regclass(d: &dyn Dialect, name: &str) -> String {
    format!("{PG_SCHEMA}.{}", d.quote_ident(name))
}

/// Column type of a storage class under the dialect's type mapping.
pub(crate) fn storage_type(d: &dyn Dialect, storage: Storage) -> &'static str {
    d.column_type(match storage {
        Storage::Text => ColumnType::Text,
        Storage::Integer => ColumnType::Integer,
        Storage::Real => ColumnType::Real,
        Storage::Bool => ColumnType::Boolean,
        Storage::Json => ColumnType::Json,
    })
}

/// The `CHECK` a storage class carries (booleans are 0/1 on both engines;
/// JSON text is validated on SQLite, `jsonb` is always valid).
pub(crate) fn storage_check(d: &dyn Dialect, storage: Storage, name: &str) -> Option<String> {
    match storage {
        Storage::Bool => Some(format!("CHECK ({name} IN (0, 1))")),
        Storage::Json if !is_pg(d) => Some(format!("CHECK ({name} IS NULL OR {})", d.json_valid(name))),
        _ => None,
    }
}

/// System columns, in table order, with their DDL.
pub(crate) fn system_columns(d: &dyn Dialect) -> Vec<String> {
    let q = |s: &str| d.quote_ident(s);
    let text = d.column_type(ColumnType::Text);
    let mut data = format!("{} {}", q("_data"), d.column_type(ColumnType::Json));
    if let Some(check) = storage_check(d, Storage::Json, &q("_data")) {
        data.push(' ');
        data.push_str(&check);
    }
    vec![
        format!(
            "{} {text} PRIMARY KEY NOT NULL CHECK (length({}) BETWEEN 1 AND 64)",
            q("id"),
            q("id")
        ),
        format!("{} {text} NOT NULL", q("created")),
        format!("{} {text} NOT NULL", q("updated")),
        format!("{} {} NOT NULL DEFAULT 1", q("_version"), d.column_type(ColumnType::Integer)),
        data,
    ]
}

/// Names of the system columns, in table order.
pub(crate) const SYSTEM_COLUMNS: &[&str] = &["id", "created", "updated", "_version", "_data"];

/// System columns of a collection's table: [`SYSTEM_COLUMNS`] plus `_tenant`
/// for tenant-scoped collections.
pub(crate) fn system_column_names(coll: &Collection) -> Vec<&'static str> {
    let mut cols = SYSTEM_COLUMNS.to_vec();
    if is_tenant_scoped(coll) {
        cols.push(TENANT_COLUMN);
    }
    cols
}

/// Column definition of a field. `target_table` is the table of a relation
/// target.
pub(crate) fn column_sql(d: &dyn Dialect, f: &Field, target_table: Option<&str>) -> String {
    let name = d.quote_ident(&f.name);
    format!("{name} {}", column_signature(d, f, target_table))
}

/// Everything in a column definition except its name. Two fields with the
/// same signature need no table rebuild (SQLite) or column change
/// (PostgreSQL).
pub(crate) fn column_signature(d: &dyn Dialect, f: &Field, target_table: Option<&str>) -> String {
    let name = d.quote_ident(&f.name);
    let storage = f.kind.storage();
    let mut s = storage_type(d, storage).to_owned();
    if let Some(check) = storage_check(d, storage, &name) {
        s.push(' ');
        s.push_str(&check);
    }
    if let Some(fk) = foreign_key_sql(d, f, target_table) {
        s.push(' ');
        s.push_str(&fk);
    }
    s
}

/// `REFERENCES target(id) ON DELETE …` of a single relation field (`None`
/// otherwise). On PostgreSQL the key is `DEFERRABLE INITIALLY IMMEDIATE`, so
/// a schema-change transaction can defer it (ADR-0028 §7.3).
pub(crate) fn foreign_key_sql(d: &dyn Dialect, f: &Field, target_table: Option<&str>) -> Option<String> {
    let (
        FieldKind::Relation {
            max_select: 1,
            cascade_delete,
            ..
        },
        Some(target),
    ) = (&f.kind, target_table)
    else {
        return None;
    };
    let action = if *cascade_delete {
        "CASCADE"
    } else if f.required {
        "RESTRICT"
    } else {
        "SET NULL"
    };
    Some(if is_pg(d) {
        format!(
            "REFERENCES {}({}) ON DELETE {action} DEFERRABLE INITIALLY IMMEDIATE",
            rel(d, target),
            d.quote_ident("id")
        )
    } else {
        format!(
            "REFERENCES {}({}) ON DELETE {action}",
            d.quote_ident(target),
            d.quote_ident("id")
        )
    })
}

/// Table name of a relation target (`None` if the field is not a single relation).
pub(crate) fn relation_table(
    f: &Field,
    coll: &Collection,
    catalog: &crate::catalog::CatalogSnapshot,
) -> Result<Option<String>, DataError> {
    let FieldKind::Relation { collection_id, .. } = &f.kind else {
        return Ok(None);
    };
    if collection_id == &coll.id {
        return Ok(Some(coll.name.clone()));
    }
    let target = catalog.get(collection_id).ok_or_else(|| {
        DataError::invalid(
            "invalid_relation",
            format!("field {}: target collection {collection_id} does not exist", f.name),
        )
    })?;
    if target.is_view() {
        return Err(DataError::invalid(
            "invalid_relation",
            format!("field {}: relations cannot point to view collections", f.name),
        ));
    }
    if is_tenant_scoped(target) && !is_tenant_scoped(coll) {
        return Err(DataError::invalid(
            "invalid_relation",
            format!(
                "field {}: a collection shared by all tenants cannot point to the tenant-scoped {}",
                f.name, target.name
            ),
        ));
    }
    Ok(Some(target.name.clone()))
}

/// `CREATE TABLE` for a base or auth collection, under `table` (in schema
/// `public` on PostgreSQL; `STRICT` on SQLite, PostgreSQL is always strict).
pub(crate) fn create_table_sql(
    d: &dyn Dialect,
    table: &str,
    coll: &Collection,
    catalog: &crate::catalog::CatalogSnapshot,
) -> Result<String, DataError> {
    let mut cols = system_columns(d);
    if is_tenant_scoped(coll) {
        let t = d.quote_ident(TENANT_COLUMN);
        cols.push(format!(
            "{t} {} NOT NULL CHECK (length({t}) BETWEEN 1 AND 64)",
            d.column_type(ColumnType::Text)
        ));
    }
    for f in &coll.fields {
        cols.push(column_sql(d, f, relation_table(f, coll, catalog)?.as_deref()));
    }
    Ok(format!(
        "CREATE TABLE {} (\n  {}\n){}",
        rel(d, table),
        cols.join(",\n  "),
        if is_pg(d) { "" } else { " STRICT" }
    ))
}

/// `CREATE VIEW` for a view collection. On PostgreSQL the view runs with the
/// caller's rights (`security_invoker`), so it never reads with its owner's
/// and native row-level security applies through it (ADR-0028 §7.5).
pub(crate) fn create_view_sql(d: &dyn Dialect, name: &str, query: &str) -> String {
    if is_pg(d) {
        format!("CREATE VIEW {} WITH (security_invoker = true) AS {query}", rel(d, name))
    } else {
        format!("CREATE VIEW {} AS {query}", d.quote_ident(name))
    }
}

/// Runs DDL inside a transaction and remembers what ran (for the history).
pub(crate) struct DdlLog<'t> {
    pub(crate) tx: &'t mut dyn WriteTx,
    pub(crate) statements: Vec<String>,
}

impl<'t> DdlLog<'t> {
    pub(crate) fn new(tx: &'t mut dyn WriteTx) -> Self {
        Self {
            tx,
            statements: Vec::new(),
        }
    }

    /// Runs and records a DDL statement.
    pub(crate) async fn run(&mut self, sql: String) -> Result<(), DataError> {
        self.tx.execute(&Statement::raw(sql.clone())).await?;
        self.statements.push(sql);
        Ok(())
    }

    /// Runs a bookkeeping statement (not recorded).
    pub(crate) async fn exec(&mut self, sql: &str, params: Vec<Value>) -> Result<u64, DataError> {
        Ok(self.tx.execute(&Statement::new(sql, params)).await?.rows_affected)
    }
}

/// Appends a schema-history row.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_history(
    tx: &mut dyn WriteTx,
    collection: Option<&Collection>,
    action: &str,
    statements: &[String],
    before: Option<&Collection>,
    after: Option<&Collection>,
    actor: &str,
) -> Result<(), DataError> {
    let json = |c: Option<&Collection>| -> Result<Value, DataError> {
        Ok(match c {
            Some(c) => Value::Text(serde_json::to_string(c)?),
            None => Value::Null,
        })
    };
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_schema_history (collection_id, collection, version, action, statements, before, after, actor, applied_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        vec![
            collection.map(|c| c.id.clone()).into(),
            collection.map(|c| c.name.clone()).into(),
            collection.map(|c| c.version).into(),
            action.into(),
            Value::Text(serde_json::to_string(&Json::from(statements.to_vec()))?),
            json(before)?,
            json(after)?,
            actor.into(),
            forgebase_schema::now_timestamp().into(),
        ],
    ))
    .await?;
    Ok(())
}

/// Checks that every collection's table or view still has the columns the
/// catalog expects. Run after schema changes, inside their transaction, so a
/// change that would break a collection (or a view built on it) rolls back.
///
/// Columns are compared through the catalog ([`Dialect::introspect_columns`]:
/// `PRAGMA table_info`, `pg_attribute`), not by selecting them: SQLite reads
/// an unknown double-quoted identifier as a string literal, so a
/// `SELECT "missing"` would not fail. Names compare case-insensitively on
/// both engines (field names are unique ignoring case).
pub(crate) async fn verify_collections(
    d: &dyn Dialect,
    tx: &mut dyn WriteTx,
    catalog: &crate::catalog::CatalogSnapshot,
) -> Result<(), DataError> {
    for c in catalog.all() {
        let broken = |why: String| {
            DataError::conflict(
                "schema_conflict",
                format!(
                    "this change would break collection {:?} ({why}); change collections through the collections API",
                    c.name
                ),
            )
        };
        let info = tx
            .fetch_all(&introspect_collection_columns(d, &c.name))
            .await
            .map_err(|e| broken(e.to_string()))?;
        let names: Vec<String> = match info.column("name") {
            Some(i) => info
                .rows
                .iter()
                .filter_map(|r| r[i].as_str().map(str::to_ascii_lowercase))
                .collect(),
            None => Vec::new(),
        };
        if names.is_empty() {
            return Err(broken("its table or view is missing".into()));
        }
        let expected: Vec<String> = if c.is_view() {
            std::iter::once("id".to_owned())
                .chain(c.fields.iter().map(|f| f.name.to_ascii_lowercase()))
                .collect()
        } else {
            system_column_names(c)
                .iter()
                .map(|s| (*s).to_owned())
                .chain(c.fields.iter().map(|f| f.name.to_ascii_lowercase()))
                .collect()
        };
        if let Some(missing) = expected.iter().find(|e| !names.contains(e)) {
            return Err(broken(format!("column {missing} is missing")));
        }
        if c.is_view() {
            // Views compile lazily on SQLite; make the engine resolve it now.
            // (PostgreSQL binds views when they are created; this is cheap.)
            tx.fetch_all(&Statement::raw(format!(
                "SELECT * FROM {} LIMIT 0",
                rel(d, &c.name)
            )))
            .await
            .map_err(|e| broken(e.to_string()))?;
        }
    }
    Ok(())
}

/// Columns of a collection table or view, in [`Dialect::introspect_columns`]
/// form. On PostgreSQL the name is looked up in schema `public` (the
/// dialect's statement resolves it through the search path, where a system
/// table would come first).
pub(crate) fn introspect_collection_columns(d: &dyn Dialect, name: &str) -> Statement {
    if is_pg(d) {
        crate::catalog::pg::columns(&pg_regclass(d, name))
    } else {
        d.introspect_columns(name)
    }
}

/// Whether a table, view, index or other relation with this name exists
/// (any case): in the SQLite schema, or on ForgeBase's PostgreSQL search path
/// (`forgebase`, `public`), so a collection or index can never shadow, or be
/// shadowed by, a system object.
pub(crate) async fn object_exists(d: &dyn Dialect, tx: &mut dyn WriteTx, name: &str) -> Result<bool, DataError> {
    let sql = if is_pg(d) {
        "SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = ANY (current_schemas(false)) AND lower(c.relname::text) = lower(?1)"
    } else {
        "SELECT 1 FROM sqlite_master WHERE lower(name) = lower(?1)"
    };
    let rows = tx.fetch_all(&Statement::new(sql, vec![name.into()])).await?;
    Ok(!rows.is_empty())
}
