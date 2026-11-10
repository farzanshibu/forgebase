//! Records and JSON documents (ADR-0003 §3).
//!
//! A record is a JSON object: system keys (`id`, `created`, `updated`,
//! `_version`), declared fields, and — in flexible collections — any other
//! top-level keys (the dynamic document, stored in `_data`). Updates are JSON
//! merge patches (RFC 7386) applied to the whole document and re-validated.

use std::collections::HashMap;

use base64::Engine;
use forgebase_db::{ColumnType, ConstraintKind, DbError, Dialect, Rows, Statement, TxMode, Value, WriteTx};
use forgebase_query::{CompileOptions, Compiler, SqlBuilder};
use forgebase_schema::values::is_blank;
use forgebase_schema::{
    Collection, FieldError, FieldKind, SchemaMode, Storage, ValidationErrors, generate_id, is_valid_id, now_timestamp,
};
use serde::Serialize;
use serde_json::{Map, Value as Json};

use crate::access::{TENANT_COLUMN, is_tenant_scoped, reads_tenant_data};
use crate::changes::ChangeOp;
use crate::ddl::{SYSTEM_COLUMNS, system_column_names};
use crate::error::unique_fields;
use crate::guard::FieldMask;
use crate::{AccessContext, Action, DataError, DataService, MAX_BATCH, Predicate, finish};

/// A record as returned by the service.
pub type Record = Map<String, Json>;

/// Options for reading one record.
#[derive(Debug, Clone, Default)]
pub struct GetOptions {
    /// Paths to return (all when `None`).
    pub fields: Option<Vec<String>>,
    /// Relation paths to expand (`author`, `author.org`, `tags`).
    pub expand: Vec<String>,
}

/// Options for updates.
#[derive(Debug, Clone, Copy, Default)]
pub struct UpdateOptions {
    /// Fail with a conflict unless the record is at this version.
    pub expected_version: Option<i64>,
    /// Replace the whole document instead of merging the patch.
    pub replace: bool,
}

/// A stored version of a record.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecordVersion {
    /// Version number.
    pub version: i64,
    /// `create`, `update` or `delete`.
    pub op: String,
    /// The record at this version (`None` for deletes).
    pub data: Option<Json>,
    /// When.
    pub changed_at: String,
    /// Who.
    pub actor: String,
}

/// Keys that are never part of the writable document.
const READ_ONLY_KEYS: &[&str] = &["id", "created", "updated", "_version", "expand", TENANT_COLUMN];

/// SQL value of a JSON value for a column.
pub(crate) fn json_to_sql(storage: Storage, v: &Json) -> Value {
    match (storage, v) {
        (_, Json::Null) => Value::Null,
        (Storage::Bool, Json::Bool(b)) => Value::from(*b),
        (Storage::Integer, Json::Number(n)) => n
            .as_i64()
            .map_or_else(|| Value::Real(n.as_f64().unwrap_or(0.0)), Value::Integer),
        (Storage::Real, Json::Number(n)) => Value::Real(n.as_f64().unwrap_or(0.0)),
        (Storage::Text, Json::String(s)) => Value::Text(s.clone()),
        (_, other) => Value::Text(other.to_string()),
    }
}

/// Column type of a storage class in `d` (`TEXT` / `jsonb`, ...).
pub(crate) fn storage_sql_type(d: &dyn Dialect, s: Storage) -> &'static str {
    d.column_type(match s {
        Storage::Text => ColumnType::Text,
        Storage::Integer => ColumnType::Integer,
        Storage::Real => ColumnType::Real,
        Storage::Bool => ColumnType::Boolean,
        Storage::Json => ColumnType::Json,
    })
}

/// Column type of a collection table's column, for the one-row derived
/// tables that run access rules over a stored row (change snapshots, record
/// versions). PostgreSQL types a bare parameter from its value (text,
/// `bigint`), so each column is cast to its table type, as the rule expects
/// (`jsonb` for JSON, `smallint` 0/1 for booleans). `None` for columns that
/// are not fields (view columns).
pub(crate) fn column_sql_type(d: &dyn Dialect, coll: &Collection, column: &str) -> Option<&'static str> {
    match column {
        "id" | "created" | "updated" | TENANT_COLUMN => return Some(d.column_type(ColumnType::Text)),
        "_data" => return Some(d.column_type(ColumnType::Json)),
        "_version" => return Some(d.column_type(ColumnType::Integer)),
        _ => {}
    }
    coll.field(column).map(|f| storage_sql_type(d, f.kind.storage()))
}

/// JSON value of a SQL value.
pub(crate) fn sql_to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Integer(i) => Json::from(*i),
        Value::Real(f) => serde_json::Number::from_f64(*f).map_or(Json::Null, Json::Number),
        Value::Text(s) => Json::String(s.clone()),
        Value::Blob(b) => Json::String(base64::engine::general_purpose::STANDARD.encode(b)),
    }
}

/// Converts a table row to a record. Password fields are never included;
/// fields the mask hides (hidden fields, field-level rules) are left out, and
/// so is `_tenant` unless the mask shows it. Columns that are not fields (view
/// columns, extra query columns) are passed through unless they start with
/// `__`.
pub(crate) fn row_to_record(coll: &Collection, columns: &[String], row: &[Value], mask: &FieldMask) -> Record {
    let mut rec = Map::new();
    let mut dynamic: Option<Map<String, Json>> = None;
    for (name, value) in columns.iter().zip(row) {
        if name.starts_with("__") || (name == TENANT_COLUMN && !mask.show_tenant()) {
            continue;
        }
        if name == "_data" {
            if let Some(Json::Object(m)) = value.as_str().and_then(|s| serde_json::from_str(s).ok()) {
                dynamic = Some(m);
            }
            continue;
        }
        match coll.field(name) {
            Some(f) => {
                if matches!(f.kind, FieldKind::Password { .. }) || !mask.allows(f) {
                    continue;
                }
                let j = match (f.kind.storage(), value) {
                    (Storage::Bool, Value::Integer(i)) => Json::Bool(*i != 0),
                    (Storage::Json, Value::Text(s)) => serde_json::from_str(s).unwrap_or(Json::Null),
                    (Storage::Integer, Value::Real(r)) if r.fract() == 0.0 => Json::from(*r as i64),
                    (_, v) => sql_to_json(v),
                };
                rec.insert(f.name.clone(), j);
            }
            None => {
                rec.insert(name.clone(), sql_to_json(value));
            }
        }
    }
    if let Some(m) = dynamic {
        for (k, v) in m {
            rec.entry(k).or_insert(v);
        }
    }
    rec
}

/// RFC 7386 JSON merge patch.
pub fn merge_patch(target: &mut Json, patch: &Json) {
    match patch {
        Json::Object(p) => {
            if !target.is_object() {
                *target = Json::Object(Map::new());
            }
            if let Json::Object(t) = target {
                for (k, v) in p {
                    if v.is_null() {
                        t.remove(k);
                    } else {
                        merge_patch(t.entry(k.clone()).or_insert(Json::Null), v);
                    }
                }
            }
        }
        other => *target = other.clone(),
    }
}

/// Column values of a validated document.
struct Prepared {
    columns: Vec<(String, Value)>,
    /// (field, target collection id, ids) to check for existence.
    relations: Vec<(String, String, Vec<String>)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WriteMode {
    Create,
    Update,
}

impl DataService {
    fn prepare(&self, coll: &Collection, doc: &Map<String, Json>, mode: WriteMode) -> Result<Prepared, DataError> {
        let mut errors = ValidationErrors::default();
        let mut columns = Vec::new();
        let mut relations = Vec::new();
        for f in &coll.fields {
            let mut value = doc.get(&f.name).cloned();
            if value.is_none() && mode == WriteMode::Create {
                value = f.default.clone();
            }
            if matches!(f.kind, FieldKind::Password { .. }) && value.is_none() && mode == WriteMode::Update {
                continue; // keep the stored hash
            }
            let raw = value.unwrap_or(Json::Null);
            let normalized = match forgebase_schema::normalize_value(f, &raw) {
                Ok(v) => v,
                Err(e) => {
                    errors.add(f.name.clone(), e);
                    continue;
                }
            };
            if f.required && is_blank(&normalized) {
                errors.add(f.name.clone(), FieldError::new("required", "cannot be blank"));
                continue;
            }
            let value = match (&f.kind, &normalized) {
                (FieldKind::Password { .. }, Json::String(plain)) => {
                    let hasher = self.inner.hasher.as_ref().ok_or(DataError::HasherUnavailable)?;
                    let hash = hasher
                        .hash(plain)
                        .map_err(|e| DataError::Internal(format!("hashing failed: {e}")))?;
                    Value::Text(hash)
                }
                _ => json_to_sql(f.kind.storage(), &normalized),
            };
            if let FieldKind::Relation { collection_id, .. } = &f.kind {
                let ids: Vec<String> = match &normalized {
                    Json::String(s) => vec![s.clone()],
                    Json::Array(a) => a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect(),
                    _ => vec![],
                };
                if !ids.is_empty() {
                    relations.push((f.name.clone(), collection_id.clone(), ids));
                }
            }
            columns.push((f.name.clone(), value));
        }
        let mut dynamic = Map::new();
        for (k, v) in doc {
            if coll.field(k).is_some() || READ_ONLY_KEYS.contains(&k.as_str()) {
                continue;
            }
            if coll.schema_mode == SchemaMode::Strict {
                errors.add(
                    k.clone(),
                    FieldError::new("unknown_field", "not a field of this collection"),
                );
            } else if k.starts_with('_') || k.is_empty() || k.len() > 255 {
                errors.add(
                    k.clone(),
                    FieldError::new("invalid_key", "document keys cannot be empty or start with _"),
                );
            } else {
                dynamic.insert(k.clone(), v.clone());
            }
        }
        if !errors.is_empty() {
            return Err(errors.into());
        }
        let data = if dynamic.is_empty() {
            Value::Null
        } else {
            let text = Json::Object(dynamic).to_string();
            if text.len() > forgebase_schema::values::DEFAULT_MAX_JSON as usize {
                return Err(
                    ValidationErrors::single("_data", FieldError::new("too_large", "document is too large")).into(),
                );
            }
            Value::Text(text)
        };
        columns.push(("_data".into(), data));
        Ok(Prepared { columns, relations })
    }

    /// Checks that related records exist — in `tenant` when the target is
    /// tenant-scoped, so relations never cross tenants.
    async fn check_relations(&self, tx: &mut dyn WriteTx, p: &Prepared, tenant: Option<&str>) -> Result<(), DataError> {
        let d = self.inner.backend.dialect();
        let cat = self.catalog();
        let mut errors = ValidationErrors::default();
        for (field, target_id, ids) in &p.relations {
            let Some(target) = cat.get(target_id) else {
                errors.add(
                    field.clone(),
                    FieldError::new("invalid_relation", "target collection is missing"),
                );
                continue;
            };
            let mut b = SqlBuilder::new(d);
            let ph: Vec<String> = ids.iter().map(|i| b.bind(i.clone())).collect();
            let mut sql = format!(
                "SELECT count(*) FROM {} WHERE {} IN ({})",
                d.quote_ident(&target.name),
                d.quote_ident("id"),
                ph.join(", ")
            );
            if is_tenant_scoped(target) {
                let Some(t) = tenant else {
                    errors.add(
                        field.clone(),
                        FieldError::new("invalid_relation", "tenant-scoped targets need a tenant"),
                    );
                    continue;
                };
                sql.push_str(&format!(
                    " AND {} = {}",
                    d.quote_ident(TENANT_COLUMN),
                    b.bind(t.to_owned())
                ));
            }
            let rows = tx.fetch_all(&Statement::new(sql, b.into_params())).await?;
            if rows.scalar().and_then(Value::as_i64) != Some(ids.len() as i64) {
                errors.add(
                    field.clone(),
                    FieldError::new("not_found", "related record does not exist"),
                );
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(errors.into()) }
    }

    /// `SELECT t.* FROM table AS t WHERE t.id = ? [AND predicate]`.
    pub(crate) fn select_by_id(
        &self,
        ctx: &AccessContext,
        coll: &Collection,
        id: &str,
        pred: Option<&Predicate>,
    ) -> Result<Statement, DataError> {
        let d = self.inner.backend.dialect();
        let mut b = SqlBuilder::new(d);
        let mut sql = format!(
            "SELECT t.* FROM {} AS t WHERE t.{} = {}",
            d.quote_ident(&coll.name),
            d.quote_ident("id"),
            b.bind(id.to_owned())
        );
        if let Some(t) = self.tenant_clause(ctx, &mut b, coll, "t") {
            sql.push_str(&format!(" AND {t}"));
        }
        if let Some(p) = pred {
            sql.push_str(&format!(" AND ({})", self.compile_predicate(ctx, &mut b, coll, p)?));
        }
        Ok(Statement::new(sql, b.into_params()))
    }

    pub(crate) fn compile_predicate(
        &self,
        _ctx: &AccessContext,
        b: &mut SqlBuilder<'_>,
        coll: &Collection,
        p: &Predicate,
    ) -> Result<String, DataError> {
        let opts = CompileOptions {
            allow_hidden: true,
            params: p.params.clone(),
            ..CompileOptions::default()
        };
        let cat = self.catalog();
        // A rule that no longer compiles (e.g. it names a dropped field) must
        // not turn into a client error that leaks schema: fail closed.
        Compiler::new(&*cat, &opts)
            .filter(b, coll, "t", &p.filter)
            .map_err(|e| {
                tracing::warn!(collection = %coll.name, error = %e, "access rule cannot be evaluated; denying");
                DataError::Forbidden("the access rule cannot be evaluated".into())
            })
    }

    async fn insert_one(
        &self,
        ctx: &AccessContext,
        tx: &mut dyn WriteTx,
        coll: &Collection,
        data: &Json,
        pred: Option<&Predicate>,
    ) -> Result<Record, DataError> {
        let d = self.inner.backend.dialect();
        let Json::Object(doc) = data else {
            return Err(DataError::invalid("invalid_body", "a record must be a JSON object"));
        };
        let id = match doc.get("id") {
            None | Some(Json::Null) => generate_id(),
            Some(Json::String(s)) if is_valid_id(s) => s.clone(),
            Some(_) => {
                return Err(ValidationErrors::single(
                    "id",
                    FieldError::new("invalid_id", "1-64 characters of [A-Za-z0-9_-]"),
                )
                .into());
            }
        };
        let mut doc = doc.clone();
        let sent = doc.clone();
        self.check_writable(ctx, coll, &mut doc, None, &sent)?;
        let tenant = self.tenant_for_insert(ctx, coll)?;
        let prepared = self.prepare(coll, &doc, WriteMode::Create)?;
        self.check_relations(tx, &prepared, tenant.as_deref()).await?;
        let now = now_timestamp();
        let mut b = SqlBuilder::new(d);
        let mut cols = vec![d.quote_ident("id"), d.quote_ident("created"), d.quote_ident("updated")];
        let mut vals = vec![b.bind(id.clone()), b.bind(now.clone()), b.bind(now)];
        if let Some(t) = tenant {
            cols.push(d.quote_ident(TENANT_COLUMN));
            vals.push(b.bind(t));
        }
        for (c, v) in &prepared.columns {
            cols.push(d.quote_ident(c));
            vals.push(b.bind(v.clone()));
        }
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({})",
            d.quote_ident(&coll.name),
            cols.join(", "),
            vals.join(", ")
        );
        tx.execute(&Statement::new(sql, b.into_params()))
            .await
            .map_err(|e| self.write_error(coll, e))?;
        self.load_after_write(ctx, tx, coll, &id, pred, "create").await
    }

    /// Reads the written row back inside the transaction, checks the access
    /// predicate on it, and stores a version if the collection is versioned.
    async fn load_after_write(
        &self,
        ctx: &AccessContext,
        tx: &mut dyn WriteTx,
        coll: &Collection,
        id: &str,
        pred: Option<&Predicate>,
        op: &str,
    ) -> Result<Record, DataError> {
        if pred.is_some() {
            let allowed = tx.fetch_all(&self.select_by_id(ctx, coll, id, pred)?).await?;
            if allowed.is_empty() {
                return Err(DataError::Forbidden(format!(
                    "the {op}d record does not satisfy the access rule"
                )));
            }
        }
        let rows = tx.fetch_all(&self.select_by_id(ctx, coll, id, None)?).await?;
        let row = rows
            .rows
            .first()
            .ok_or_else(|| DataError::Internal("written row not found".into()))?;
        let rec = row_to_record(coll, &rows.columns, row, &FieldMask::all());
        let change_op = if op == "create" {
            ChangeOp::Create
        } else {
            ChangeOp::Update
        };
        self.record_change(tx, ctx, coll, change_op, id, (&rows.columns, row))
            .await?;
        if coll.versioned {
            let version = rec.get("_version").and_then(Json::as_i64).unwrap_or(1);
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_record_versions (collection_id, record_id, version, op, data, changed_at, actor)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                vec![
                    coll.id.clone().into(),
                    id.into(),
                    version.into(),
                    op.into(),
                    Json::Object(rec.clone()).to_string().into(),
                    rec.get("updated").and_then(Json::as_str).unwrap_or_default().into(),
                    ctx.actor().into(),
                ],
            ))
            .await?;
        }
        Ok(rec)
    }

    fn write_error(&self, coll: &Collection, e: DbError) -> DataError {
        if let Some(fields) = unique_fields(&coll.name, &e) {
            let mut v = ValidationErrors::default();
            for f in fields.into_iter().filter(|f| !f.eq_ignore_ascii_case(TENANT_COLUMN)) {
                v.add(f, FieldError::new("unique", "value is already in use"));
            }
            return v.into();
        }
        match e {
            DbError::Constraint {
                kind: ConstraintKind::ForeignKey,
                ..
            } => DataError::conflict(
                "record_referenced",
                "a relation would point to a missing record, or the record is still referenced",
            ),
            DbError::Constraint {
                kind: ConstraintKind::Unique,
                message,
            } => DataError::conflict("unique_violation", message),
            other => other.into(),
        }
    }

    fn strip_hidden(&self, ctx: &AccessContext, coll: &Collection, mut rec: Record) -> Record {
        self.mask(ctx, coll).strip(coll, &mut rec);
        rec
    }

    fn writable(&self, coll: &Collection) -> Result<(), DataError> {
        if coll.is_view() {
            return Err(DataError::invalid(
                "read_only_collection",
                "view collections are read-only",
            ));
        }
        Ok(())
    }

    /// Creates a record.
    pub async fn create_record(&self, ctx: &AccessContext, collection: &str, data: Json) -> Result<Record, DataError> {
        Ok(self.create_records(ctx, collection, vec![data]).await?.remove(0))
    }

    /// Creates records in one transaction: all or nothing (bulk insert).
    pub async fn create_records(
        &self,
        ctx: &AccessContext,
        collection: &str,
        data: Vec<Json>,
    ) -> Result<Vec<Record>, DataError> {
        let (coll, pred) = self.resolve(ctx, Action::Create, collection)?;
        self.writable(&coll)?;
        if data.is_empty() || data.len() > MAX_BATCH {
            return Err(DataError::invalid(
                "invalid_batch",
                format!("send 1 to {MAX_BATCH} records"),
            ));
        }
        for item in &data {
            let id = item.get("id").and_then(Json::as_str);
            self.guard_write(ctx, &coll, ChangeOp::Create, id, Some(item)).await?;
        }
        let mut tx = self.begin(TxMode::Normal).await?;
        let result = async {
            let mut out = Vec::with_capacity(data.len());
            for (i, item) in data.iter().enumerate() {
                let rec = self
                    .insert_one(ctx, &mut *tx, &coll, item, pred.as_ref())
                    .await
                    .map_err(|e| match e {
                        DataError::Validation(mut v) if data.len() > 1 => {
                            v.fields = v.fields.into_iter().map(|(k, e)| (format!("{i}.{k}"), e)).collect();
                            DataError::Validation(v)
                        }
                        other => other,
                    })?;
                out.push(rec);
            }
            Ok(out)
        }
        .await;
        let recs = finish(tx, result).await?;
        self.changes_committed();
        Ok(recs.into_iter().map(|r| self.strip_hidden(ctx, &coll, r)).collect())
    }

    /// Reads one record.
    pub async fn get_record(
        &self,
        ctx: &AccessContext,
        collection: &str,
        id: &str,
        opts: &GetOptions,
    ) -> Result<Record, DataError> {
        let (coll, pred) = self.resolve(ctx, Action::View, collection)?;
        let rows = self
            .inner
            .backend
            .fetch_all(&self.select_by_id(ctx, &coll, id, pred.as_ref())?)
            .await?;
        let row = rows.rows.first().ok_or_else(|| DataError::RecordNotFound {
            collection: coll.name.clone(),
            id: id.to_owned(),
        })?;
        let mut recs = vec![row_to_record(&coll, &rows.columns, row, &self.mask(ctx, &coll))];
        if !opts.expand.is_empty() {
            self.expand(ctx, &coll, &mut recs, &opts.expand, 0).await?;
        }
        let rec = recs.remove(0);
        Ok(match &opts.fields {
            Some(f) => project(&rec, f),
            None => rec,
        })
    }

    /// Updates a record with a JSON merge patch (or replaces its document).
    pub async fn update_record(
        &self,
        ctx: &AccessContext,
        collection: &str,
        id: &str,
        patch: Json,
        opts: UpdateOptions,
    ) -> Result<Record, DataError> {
        let (coll, pred) = self.resolve(ctx, Action::Update, collection)?;
        self.writable(&coll)?;
        let Json::Object(patch_obj) = &patch else {
            return Err(DataError::invalid("invalid_body", "a patch must be a JSON object"));
        };
        if let Some(pid) = patch_obj.get("id")
            && pid.as_str() != Some(id)
        {
            return Err(ValidationErrors::single("id", FieldError::new("read_only", "the id cannot change")).into());
        }
        self.guard_write(ctx, &coll, ChangeOp::Update, Some(id), Some(&patch))
            .await?;
        let d = self.inner.backend.dialect();
        let mut tx = self.begin(TxMode::Normal).await?;
        let result = async {
            let rows = tx.fetch_all(&self.select_by_id(ctx, &coll, id, pred.as_ref())?).await?;
            let row = rows.rows.first().ok_or_else(|| DataError::RecordNotFound {
                collection: coll.name.clone(),
                id: id.to_owned(),
            })?;
            let current = row_to_record(&coll, &rows.columns, row, &FieldMask::all());
            let actual = current.get("_version").and_then(Json::as_i64).unwrap_or(1);
            if let Some(expected) = opts.expected_version
                && expected != actual
            {
                return Err(DataError::VersionConflict { expected, actual });
            }
            let mut doc = Json::Object(
                current
                    .iter()
                    .filter(|(k, _)| !READ_ONLY_KEYS.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            );
            if opts.replace {
                doc = Json::Object(patch_obj.clone());
            } else {
                merge_patch(&mut doc, &patch);
            }
            let Json::Object(mut doc) = doc else {
                unreachable!("merge of objects is an object")
            };
            doc.remove(TENANT_COLUMN);
            self.check_writable(ctx, &coll, &mut doc, Some(&current), patch_obj)?;
            let tenant = current.get(TENANT_COLUMN).and_then(Json::as_str);
            let prepared = self.prepare(&coll, &doc, WriteMode::Update)?;
            self.check_relations(&mut *tx, &prepared, tenant).await?;
            let mut b = SqlBuilder::new(d);
            let mut sets = vec![
                format!("{} = {}", d.quote_ident("updated"), b.bind(now_timestamp())),
                format!("{0} = {0} + 1", d.quote_ident("_version")),
            ];
            for (c, v) in &prepared.columns {
                sets.push(format!("{} = {}", d.quote_ident(c), b.bind(v.clone())));
            }
            let sql = format!(
                "UPDATE {} SET {} WHERE {} = {}",
                d.quote_ident(&coll.name),
                sets.join(", "),
                d.quote_ident("id"),
                b.bind(id.to_owned())
            );
            tx.execute(&Statement::new(sql, b.into_params()))
                .await
                .map_err(|e| self.write_error(&coll, e))?;
            self.load_after_write(ctx, &mut *tx, &coll, id, pred.as_ref(), "update")
                .await
        }
        .await;
        let rec = finish(tx, result).await?;
        self.changes_committed();
        Ok(self.strip_hidden(ctx, &coll, rec))
    }

    /// Deletes a record. Relations follow their field settings: single
    /// relations through foreign keys (cascade / set null / restrict), multiple
    /// relations by cascading or removing the id from referencing arrays.
    pub async fn delete_record(
        &self,
        ctx: &AccessContext,
        collection: &str,
        id: &str,
        expected_version: Option<i64>,
    ) -> Result<(), DataError> {
        let (coll, pred) = self.resolve(ctx, Action::Delete, collection)?;
        self.writable(&coll)?;
        self.guard_write(ctx, &coll, ChangeOp::Delete, Some(id), None).await?;
        let mut tx = self.begin(TxMode::Normal).await?;
        let result = async {
            let rows = tx.fetch_all(&self.select_by_id(ctx, &coll, id, pred.as_ref())?).await?;
            let row = rows.rows.first().ok_or_else(|| DataError::RecordNotFound {
                collection: coll.name.clone(),
                id: id.to_owned(),
            })?;
            let current = row_to_record(&coll, &rows.columns, row, &FieldMask::all());
            let actual = current.get("_version").and_then(Json::as_i64).unwrap_or(1);
            if let Some(expected) = expected_version
                && expected != actual
            {
                return Err(DataError::VersionConflict { expected, actual });
            }
            self.delete_in_tx(ctx, &mut *tx, &coll, id, actual, 0).await
        }
        .await;
        finish(tx, result).await?;
        self.changes_committed();
        Ok(())
    }

    /// Deletes one record and applies the relation actions of the records
    /// that reference it.
    ///
    /// **Cascade semantics** (rls-coverage G13, ADR-0028 FB-135 addendum):
    /// the caller's Delete rule is checked on the record they delete, never
    /// on the records a cascade reaches. Referencing records are deleted
    /// (`cascade_delete`), have the id pulled out of their multi-value
    /// relation, or block the delete (a required relation that would be left
    /// empty), **without** their collection's Delete or Update rules, field rules or
    /// before-write guards: the same semantics as PostgreSQL foreign-key
    /// actions (`ON DELETE CASCADE` / `SET NULL` / `RESTRICT`), which bypass
    /// row-level security. Cascades stay in the tenant, because relations
    /// never cross tenants. Multi-value relations are walked here (change
    /// events and tombstones are written for what they delete); single-value
    /// relations are engine foreign keys, so the engine removes or nulls
    /// those rows without change events or tombstones.
    fn delete_in_tx<'a>(
        &'a self,
        ctx: &'a AccessContext,
        tx: &'a mut dyn WriteTx,
        coll: &'a Collection,
        id: &'a str,
        version: i64,
        depth: usize,
    ) -> forgebase_db::BoxFuture<'a, Result<(), DataError>> {
        Box::pin(async move {
            if depth > 8 {
                return Err(DataError::conflict(
                    "cascade_too_deep",
                    "cascading deletes nest deeper than 8 levels",
                ));
            }
            let d = self.inner.backend.dialect();
            let cat = self.catalog();
            // Multi-value relations pointing at this collection.
            for other in cat.all() {
                for f in &other.fields {
                    let FieldKind::Relation {
                        collection_id,
                        max_select,
                        cascade_delete,
                    } = &f.kind
                    else {
                        continue;
                    };
                    if collection_id != &coll.id || *max_select <= 1 {
                        continue;
                    }
                    let (t, c) = (d.quote_ident(&other.name), d.quote_ident(&f.name));
                    let (each, elem) = d.json_array_elements(&c, "e");
                    let refs = tx
                        .fetch_all(&Statement::new(
                            format!(
                                "SELECT {}, {}, {} FROM {t} WHERE EXISTS (SELECT 1 FROM {each} WHERE {elem} = ?1)",
                                d.quote_ident("id"),
                                d.quote_ident("_version"),
                                d.json_array_length(&c),
                            ),
                            vec![id.into()],
                        ))
                        .await?;
                    for r in &refs.rows {
                        let rid = r[0].as_str().unwrap_or_default().to_owned();
                        if *cascade_delete {
                            if !(other.id == coll.id && rid == id) {
                                self.delete_in_tx(ctx, &mut *tx, other, &rid, r[1].as_i64().unwrap_or(1), depth + 1)
                                    .await?;
                            }
                        } else if f.required && r[2].as_i64() == Some(1) {
                            return Err(DataError::conflict(
                                "record_referenced",
                                format!("{}.{} of record {rid} requires this record", other.name, f.name),
                            ));
                        } else {
                            tx.execute(&Statement::new(
                                format!(
                                    "UPDATE {t} SET {c} = (SELECT {} FROM {each} WHERE {elem} {} ?1),
                                     {u} = ?2, {v} = {v} + 1 WHERE {i} = ?3",
                                    d.json_array_agg(&elem),
                                    d.null_safe_ne(),
                                    u = d.quote_ident("updated"),
                                    v = d.quote_ident("_version"),
                                    i = d.quote_ident("id")
                                ),
                                vec![id.into(), now_timestamp().into(), rid.clone().into()],
                            ))
                            .await?;
                            if self.has_change_hook() {
                                let after = tx.fetch_all(&self.select_by_id(ctx, other, &rid, None)?).await?;
                                if let Some(row) = after.rows.first() {
                                    self.record_change(tx, ctx, other, ChangeOp::Update, &rid, (&after.columns, row))
                                        .await?;
                                }
                            }
                        }
                    }
                }
            }
            let before = if self.has_change_hook() {
                Some(tx.fetch_all(&self.select_by_id(ctx, coll, id, None)?).await?)
            } else {
                None
            };
            let done = tx
                .execute(&Statement::new(
                    format!(
                        "DELETE FROM {} WHERE {} = ?1",
                        d.quote_ident(&coll.name),
                        d.quote_ident("id")
                    ),
                    vec![id.into()],
                ))
                .await
                .map_err(|e| match e {
                    DbError::Constraint {
                        kind: ConstraintKind::ForeignKey,
                        ..
                    } => DataError::conflict(
                        "record_referenced",
                        "the record is still referenced by a required relation",
                    ),
                    other => other.into(),
                })?;
            if done.rows_affected == 0 {
                return Ok(()); // already removed by an engine cascade
            }
            if let Some(before) = &before
                && let Some(row) = before.rows.first()
            {
                self.record_change(tx, ctx, coll, ChangeOp::Delete, id, (&before.columns, row))
                    .await?;
            }
            if coll.versioned {
                tx.execute(&Statement::new(
                    "INSERT INTO _forgebase_record_versions (collection_id, record_id, version, op, data, changed_at, actor)
                     VALUES (?1, ?2, ?3, 'delete', NULL, ?4, ?5)",
                    vec![
                        coll.id.clone().into(),
                        id.into(),
                        (version + 1).into(),
                        now_timestamp().into(),
                        ctx.actor().into(),
                    ],
                ))
                .await?;
            }
            Ok(())
        })
    }

    /// Stored versions of a record (versioned collections), oldest first.
    /// Visibility is always checked (rls-coverage G13): a live record must be
    /// readable by the caller (tenant and access rule); a deleted one through
    /// its last stored version (that version's `_tenant` and the access rule
    /// evaluated over its data). An unknown record is not found.
    pub async fn record_versions(
        &self,
        ctx: &AccessContext,
        collection: &str,
        id: &str,
    ) -> Result<Vec<RecordVersion>, DataError> {
        let (coll, pred) = self.resolve(ctx, Action::View, collection)?;
        let rows = self
            .inner
            .backend
            .fetch_all(&Statement::new(
                "SELECT version, op, data, changed_at, actor FROM _forgebase_record_versions
                 WHERE collection_id = ?1 AND record_id = ?2 ORDER BY version",
                vec![coll.id.clone().into(), id.into()],
            ))
            .await?;
        if !self.history_visible(ctx, &coll, id, pred.as_ref(), &rows).await? {
            return Err(DataError::RecordNotFound {
                collection: coll.name.clone(),
                id: id.to_owned(),
            });
        }
        Ok(versions_from(&coll, &self.mask(ctx, &coll), &rows))
    }

    /// Whether the caller may read the history of `id` (`versions`: its
    /// stored versions, oldest first).
    async fn history_visible(
        &self,
        ctx: &AccessContext,
        coll: &Collection,
        id: &str,
        pred: Option<&Predicate>,
        versions: &Rows,
    ) -> Result<bool, DataError> {
        let b = self.inner.backend.as_ref();
        if !b.fetch_all(&self.select_by_id(ctx, coll, id, pred)?).await?.is_empty() {
            return Ok(true);
        }
        // A live record the caller cannot see hides its history.
        let d = b.dialect();
        let live = b
            .fetch_all(&Statement::new(
                format!(
                    "SELECT 1 FROM {} WHERE {} = ?1",
                    d.quote_ident(&coll.name),
                    d.quote_ident("id")
                ),
                vec![id.into()],
            ))
            .await?;
        if !live.is_empty() {
            return Ok(false);
        }
        // Deleted (a tombstone) or removed by an engine cascade (no
        // tombstone): judge it by the last version that holds data.
        let last = versions.rows.iter().rev().find_map(|r| {
            r.get(2)
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str::<Map<String, Json>>(s).ok())
        });
        match last {
            Some(data) => self.snapshot_visible(ctx, coll, &data, pred).await,
            // Only a tombstone (versioning enabled late): no data to check a
            // tenant or a rule against, so only unrestricted callers see it.
            None => Ok(!versions.rows.is_empty()
                && pred.is_none()
                && !(is_tenant_scoped(coll) && (ctx.tenant.is_some() || !ctx.is_privileged()))),
        }
    }

    /// Whether a stored record document (a version's data) is visible to the
    /// caller: its `_tenant` must be the caller's tenant (tenant-scoped
    /// collections) and the access predicate must hold over it. The predicate
    /// runs over a one-row derived table that rebuilds the record's columns.
    async fn snapshot_visible(
        &self,
        ctx: &AccessContext,
        coll: &Collection,
        data: &Map<String, Json>,
        pred: Option<&Predicate>,
    ) -> Result<bool, DataError> {
        if is_tenant_scoped(coll) {
            match &ctx.tenant {
                Some(t) if data.get(TENANT_COLUMN).and_then(Json::as_str) == Some(t.as_str()) => {}
                None if ctx.is_privileged() => {}
                _ => return Ok(false),
            }
        }
        let Some(p) = pred else { return Ok(true) };
        let d = self.inner.backend.dialect();
        let mut b = SqlBuilder::new(d);
        let mut cols = Vec::new();
        let mut dynamic = Map::new();
        for (k, v) in data {
            if coll.field(k).is_none() && !SYSTEM_COLUMNS.contains(&k.as_str()) && k != TENANT_COLUMN {
                dynamic.insert(k.clone(), v.clone());
            }
        }
        for name in system_column_names(coll) {
            let v = match name {
                "_data" if dynamic.is_empty() => Value::Null,
                "_data" => Value::Text(Json::Object(dynamic.clone()).to_string()),
                "_version" => data
                    .get(name)
                    .and_then(Json::as_i64)
                    .map_or(Value::Null, Value::Integer),
                _ => data.get(name).and_then(Json::as_str).map_or(Value::Null, Value::from),
            };
            let ty = column_sql_type(d, coll, name).unwrap_or("TEXT");
            cols.push(format!("CAST({} AS {ty}) AS {}", b.bind(v), d.quote_ident(name)));
        }
        for f in &coll.fields {
            let v = match (&f.kind, data.get(&f.name)) {
                (FieldKind::Password { .. }, _) | (_, None) => Value::Null,
                (k, Some(v)) => json_to_sql(k.storage(), v),
            };
            let ty = storage_sql_type(d, f.kind.storage());
            cols.push(format!("CAST({} AS {ty}) AS {}", b.bind(v), d.quote_ident(&f.name)));
        }
        let cond = self.compile_predicate(ctx, &mut b, coll, p)?;
        let sql = format!("SELECT 1 FROM (SELECT {}) AS t WHERE {cond}", cols.join(", "));
        Ok(!self
            .inner
            .backend
            .fetch_all(&Statement::new(sql, b.into_params()))
            .await?
            .is_empty())
    }
}

fn versions_from(coll: &Collection, mask: &FieldMask, rows: &Rows) -> Vec<RecordVersion> {
    rows.rows
        .iter()
        .map(|r| RecordVersion {
            version: r[0].as_i64().unwrap_or_default(),
            op: r[1].as_str().unwrap_or_default().to_owned(),
            data: r[2]
                .as_str()
                .and_then(|s| serde_json::from_str::<Json>(s).ok())
                .map(|mut j| {
                    if let Json::Object(m) = &mut j {
                        mask.strip(coll, m);
                    }
                    j
                }),
            changed_at: r[3].as_str().unwrap_or_default().to_owned(),
            actor: r[4].as_str().unwrap_or_default().to_owned(),
        })
        .collect()
}

/// Keeps only the requested paths (`name`, `metadata.brand`, `expand.author`).
pub(crate) fn project(rec: &Record, fields: &[String]) -> Record {
    let mut out = Map::new();
    for path in fields {
        let segs: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
        let Some((first, rest)) = segs.split_first() else {
            continue;
        };
        let Some(mut value) = rec.get(*first) else { continue };
        let mut ok = true;
        for s in rest {
            match value.get(*s) {
                Some(v) => value = v,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if !ok {
            continue;
        }
        // Rebuild the nested shape.
        let mut slot = out.entry((*first).to_owned()).or_insert_with(|| {
            if rest.is_empty() {
                Json::Null
            } else {
                Json::Object(Map::new())
            }
        });
        for (i, s) in rest.iter().enumerate() {
            if !slot.is_object() {
                *slot = Json::Object(Map::new());
            }
            let Json::Object(m) = slot else { break };
            slot = m.entry((*s).to_owned()).or_insert_with(|| {
                if i + 1 == rest.len() {
                    Json::Null
                } else {
                    Json::Object(Map::new())
                }
            });
        }
        *slot = value.clone();
    }
    out
}

impl DataService {
    /// Attaches related records under `expand`. Targets the caller may not
    /// list are left out silently.
    pub(crate) fn expand<'a>(
        &'a self,
        ctx: &'a AccessContext,
        coll: &'a Collection,
        recs: &'a mut [Record],
        paths: &'a [String],
        depth: usize,
    ) -> forgebase_db::BoxFuture<'a, Result<(), DataError>> {
        Box::pin(async move {
            if depth >= 4 {
                return Err(DataError::invalid("expand_too_deep", "expand at most 4 levels deep"));
            }
            let mut groups: Vec<(String, Vec<String>)> = Vec::new();
            for p in paths {
                let (first, rest) = match p.split_once('.') {
                    Some((a, b)) => (a.to_owned(), Some(b.to_owned())),
                    None => (p.clone(), None),
                };
                match groups.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(&first)) {
                    Some((_, nested)) => nested.extend(rest),
                    None => groups.push((first, rest.into_iter().collect())),
                }
            }
            let d = self.inner.backend.dialect();
            let mask = self.mask(ctx, coll);
            for (name, nested) in groups {
                let field = coll
                    .field(&name)
                    .filter(|f| mask.allows(f))
                    .ok_or_else(|| DataError::invalid("invalid_expand", format!("{name} is not a relation field")))?;
                let FieldKind::Relation {
                    collection_id,
                    max_select,
                    ..
                } = &field.kind
                else {
                    return Err(DataError::invalid(
                        "invalid_expand",
                        format!("{name} is not a relation field"),
                    ));
                };
                let target = self.collection(collection_id)?;
                if !ctx.is_privileged()
                    && (reads_tenant_data(&target) || (is_tenant_scoped(&target) && ctx.tenant.is_none()))
                {
                    continue;
                }
                let pred = match self.authorize(ctx, Action::List, Some(&target)) {
                    Ok(p) => p,
                    Err(DataError::Forbidden(_) | DataError::Unauthenticated) => continue,
                    Err(e) => return Err(e),
                };
                let target_mask = self.mask(ctx, &target);
                let mut ids: Vec<String> = Vec::new();
                for r in recs.iter() {
                    match r.get(&field.name) {
                        Some(Json::String(s)) => ids.push(s.clone()),
                        Some(Json::Array(a)) => ids.extend(a.iter().filter_map(|v| v.as_str().map(str::to_owned))),
                        _ => {}
                    }
                }
                ids.sort();
                ids.dedup();
                let mut found: HashMap<String, Record> = HashMap::new();
                for chunk in ids.chunks(500) {
                    let mut b = SqlBuilder::new(d);
                    let ph: Vec<String> = chunk.iter().map(|i| b.bind(i.clone())).collect();
                    let mut sql = format!(
                        "SELECT t.* FROM {} AS t WHERE t.{} IN ({})",
                        d.quote_ident(&target.name),
                        d.quote_ident("id"),
                        ph.join(", ")
                    );
                    if let Some(t) = self.tenant_clause(ctx, &mut b, &target, "t") {
                        sql.push_str(&format!(" AND {t}"));
                    }
                    if let Some(p) = &pred {
                        sql.push_str(&format!(" AND ({})", self.compile_predicate(ctx, &mut b, &target, p)?));
                    }
                    let rows = self
                        .inner
                        .backend
                        .fetch_all(&Statement::new(sql, b.into_params()))
                        .await?;
                    for row in &rows.rows {
                        let rec = row_to_record(&target, &rows.columns, row, &target_mask);
                        if let Some(id) = rec.get("id").and_then(Json::as_str) {
                            found.insert(id.to_owned(), rec);
                        }
                    }
                }
                if !nested.is_empty() {
                    let mut list: Vec<Record> = found.values().cloned().collect();
                    self.expand(ctx, &target, &mut list, &nested, depth + 1).await?;
                    found = list
                        .into_iter()
                        .filter_map(|r| r.get("id").and_then(Json::as_str).map(|id| (id.to_owned(), r.clone())))
                        .collect();
                }
                for r in recs.iter_mut() {
                    let value = match r.get(&field.name) {
                        Some(Json::String(s)) if *max_select <= 1 => found.get(s).cloned().map(Json::Object),
                        Some(Json::Array(a)) => Some(Json::Array(
                            a.iter()
                                .filter_map(|v| v.as_str().and_then(|s| found.get(s)).cloned().map(Json::Object))
                                .collect(),
                        )),
                        _ => None,
                    };
                    if let Some(v) = value {
                        let exp = r.entry("expand").or_insert_with(|| Json::Object(Map::new()));
                        if let Json::Object(m) = exp {
                            m.insert(field.name.clone(), v);
                        }
                    }
                }
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merge_patch_follows_rfc7386() {
        let mut doc = json!({"a": "b", "c": {"d": "e", "f": "g"}, "arr": [1, 2]});
        merge_patch(
            &mut doc,
            &json!({"a": "z", "c": {"f": null}, "arr": [3], "n": {"x": 1}}),
        );
        assert_eq!(doc, json!({"a": "z", "c": {"d": "e"}, "arr": [3], "n": {"x": 1}}));
    }

    #[test]
    fn projection_keeps_nested_paths() {
        let rec: Record =
            serde_json::from_value(json!({"id": "1", "name": "x", "meta": {"a": 1, "b": {"c": 2}}})).unwrap();
        let p = project(&rec, &["id".into(), "meta.b.c".into(), "missing".into()]);
        assert_eq!(Json::Object(p), json!({"id": "1", "meta": {"b": {"c": 2}}}));
    }
}
