//! DDL rendering for collection tables (ADR-0003 §4).

use forgebase_db::{ColumnType, Dialect, Statement, Value, WriteTx};
use forgebase_schema::{Collection, Field, FieldKind, Storage};
use serde_json::Value as Json;

use crate::DataError;
use crate::access::{TENANT_COLUMN, is_tenant_scoped};

/// Prefix of the temporary table used while a table is rebuilt.
pub(crate) const TMP_PREFIX: &str = "_forgebase_tmp_";

/// System columns, in table order, with their DDL.
pub(crate) fn system_columns(d: &dyn Dialect) -> Vec<String> {
    let q = |s: &str| d.quote_ident(s);
    vec![
        format!(
            "{} TEXT PRIMARY KEY NOT NULL CHECK (length({}) BETWEEN 1 AND 64)",
            q("id"),
            q("id")
        ),
        format!("{} TEXT NOT NULL", q("created")),
        format!("{} TEXT NOT NULL", q("updated")),
        format!("{} INTEGER NOT NULL DEFAULT 1", q("_version")),
        format!(
            "{} TEXT CHECK ({} IS NULL OR {})",
            q("_data"),
            q("_data"),
            d.json_valid(&q("_data"))
        ),
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
/// same signature need no table rebuild.
pub(crate) fn column_signature(d: &dyn Dialect, f: &Field, target_table: Option<&str>) -> String {
    let name = d.quote_ident(&f.name);
    let storage = f.kind.storage();
    let ty = d.column_type(match storage {
        Storage::Text => ColumnType::Text,
        Storage::Integer => ColumnType::Integer,
        Storage::Real => ColumnType::Real,
        Storage::Bool => ColumnType::Boolean,
        Storage::Json => ColumnType::Json,
    });
    let mut s = ty.to_owned();
    match storage {
        Storage::Bool => s.push_str(&format!(" CHECK ({name} IN (0, 1))")),
        Storage::Json => s.push_str(&format!(" CHECK ({name} IS NULL OR {})", d.json_valid(&name))),
        _ => {}
    }
    if let (
        FieldKind::Relation {
            max_select: 1,
            cascade_delete,
            ..
        },
        Some(target),
    ) = (&f.kind, target_table)
    {
        let action = if *cascade_delete {
            "CASCADE"
        } else if f.required {
            "RESTRICT"
        } else {
            "SET NULL"
        };
        s.push_str(&format!(
            " REFERENCES {}({}) ON DELETE {action}",
            d.quote_ident(target),
            d.quote_ident("id")
        ));
    }
    s
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

/// `CREATE TABLE` for a base or auth collection, under `table`.
pub(crate) fn create_table_sql(
    d: &dyn Dialect,
    table: &str,
    coll: &Collection,
    catalog: &crate::catalog::CatalogSnapshot,
) -> Result<String, DataError> {
    let mut cols = system_columns(d);
    if is_tenant_scoped(coll) {
        let t = d.quote_ident(TENANT_COLUMN);
        cols.push(format!("{t} TEXT NOT NULL CHECK (length({t}) BETWEEN 1 AND 64)"));
    }
    for f in &coll.fields {
        cols.push(column_sql(d, f, relation_table(f, coll, catalog)?.as_deref()));
    }
    Ok(format!(
        "CREATE TABLE {} (\n  {}\n) STRICT",
        d.quote_ident(table),
        cols.join(",\n  ")
    ))
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
/// Columns are compared through `PRAGMA table_info`, not by selecting them:
/// SQLite reads an unknown double-quoted identifier as a string literal, so a
/// `SELECT "missing"` would not fail.
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
            .fetch_all(&Statement::raw(format!(
                "PRAGMA table_info({})",
                d.quote_ident(&c.name)
            )))
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
            // Views compile lazily; make the engine resolve it now.
            tx.fetch_all(&Statement::raw(format!(
                "SELECT * FROM {} LIMIT 0",
                d.quote_ident(&c.name)
            )))
            .await
            .map_err(|e| broken(e.to_string()))?;
        }
    }
    Ok(())
}

/// Whether a table, view or index with this name exists (any case).
pub(crate) async fn object_exists(tx: &mut dyn WriteTx, name: &str) -> Result<bool, DataError> {
    let rows = tx
        .fetch_all(&Statement::new(
            "SELECT 1 FROM sqlite_master WHERE lower(name) = lower(?1)",
            vec![name.into()],
        ))
        .await?;
    Ok(!rows.is_empty())
}
