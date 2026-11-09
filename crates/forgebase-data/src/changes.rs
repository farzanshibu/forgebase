//! Change hook for the transactional outbox (FB-40, ADR-0011).
//!
//! When a [`ChangeHook`] is installed ([`DataService::set_change_hook`]), every
//! record created, updated or deleted through the service is handed to it
//! *inside the write transaction*; the statements it returns run in that same
//! transaction, so a change and its outbox row commit (or roll back) together.
//! After a transaction with at least one change commits,
//! [`ChangeHook::committed`] is called (the realtime dispatcher's wake-up).
//!
//! [`DataService::view_snapshot`] answers the delivery-time question "what may
//! this caller see of this stored row, right now?" with the caller's *current*
//! access decision, so realtime events go through the same authorisation and
//! field filtering as REST reads.

use std::collections::HashMap;
use std::fmt;

use base64::Engine;
use forgebase_db::{Statement, Value, WriteTx};
use forgebase_query::{CompileOptions, Compiler, Expr, SqlBuilder};
use forgebase_schema::{Collection, FieldKind, Storage};
use serde_json::{Value as Json, json};

use crate::records::{Record, row_to_record};
use crate::{AccessContext, Action, DataError, DataService};

/// Kind of record change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeOp {
    /// A record was created.
    Create,
    /// A record was updated.
    Update,
    /// A record was deleted.
    Delete,
}

impl ChangeOp {
    /// `create`, `update` or `delete`.
    pub fn as_str(self) -> &'static str {
        match self {
            ChangeOp::Create => "create",
            ChangeOp::Update => "update",
            ChangeOp::Delete => "delete",
        }
    }
}

/// A full table row as stored: column names and values. Password columns
/// are never included.
#[derive(Debug, Clone, PartialEq)]
pub struct RowSnapshot {
    /// Column names.
    pub columns: Vec<String>,
    /// Values, one per column.
    pub values: Vec<Value>,
}

impl RowSnapshot {
    /// Builds a snapshot of one row of `coll`, leaving out password columns.
    pub(crate) fn of(coll: &Collection, columns: &[String], row: &[Value]) -> Self {
        let mut s = Self {
            columns: Vec::with_capacity(columns.len()),
            values: Vec::with_capacity(columns.len()),
        };
        for (c, v) in columns.iter().zip(row) {
            if coll
                .field(c)
                .is_some_and(|f| matches!(f.kind, FieldKind::Password { .. }))
            {
                continue;
            }
            s.columns.push(c.clone());
            s.values.push(v.clone());
        }
        s
    }

    /// The value of column `name`.
    pub fn get(&self, name: &str) -> Option<&Value> {
        self.columns.iter().position(|c| c == name).map(|i| &self.values[i])
    }

    /// JSON form `{"c": [names], "v": [values]}`; blobs are `{"$b": base64}`.
    pub fn to_json(&self) -> Json {
        let values: Vec<Json> = self
            .values
            .iter()
            .map(|v| match v {
                Value::Null => Json::Null,
                Value::Integer(i) => Json::from(*i),
                Value::Real(f) => serde_json::Number::from_f64(*f).map_or(Json::Null, Json::Number),
                Value::Text(s) => Json::String(s.clone()),
                Value::Blob(b) => json!({"$b": base64::engine::general_purpose::STANDARD.encode(b)}),
            })
            .collect();
        json!({"c": self.columns, "v": values})
    }

    /// Parses the form written by [`RowSnapshot::to_json`].
    pub fn from_json(j: &Json) -> Option<Self> {
        let columns: Vec<String> = j
            .get("c")?
            .as_array()?
            .iter()
            .map(|c| c.as_str().map(str::to_owned))
            .collect::<Option<_>>()?;
        let values: Vec<Value> = j
            .get("v")?
            .as_array()?
            .iter()
            .map(|v| match v {
                Json::Null => Some(Value::Null),
                Json::Number(n) => Some(n.as_i64().map_or_else(|| Value::Real(n.as_f64().unwrap_or(0.0)), Value::Integer)),
                Json::String(s) => Some(Value::Text(s.clone())),
                Json::Object(o) => o
                    .get("$b")
                    .and_then(Json::as_str)
                    .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
                    .map(Value::Blob),
                _ => None,
            })
            .collect::<Option<_>>()?;
        (columns.len() == values.len()).then_some(Self { columns, values })
    }
}

/// One record change, handed to the hook inside the write transaction.
#[derive(Debug)]
pub struct RecordChange<'a> {
    /// What happened.
    pub op: ChangeOp,
    /// The collection (definition at the time of the change).
    pub collection: &'a Collection,
    /// Record id.
    pub record_id: &'a str,
    /// The row after the change (before it, for deletes).
    pub row: &'a RowSnapshot,
    /// Who made the change.
    pub ctx: &'a AccessContext,
}

/// Receives every record change made through the [`DataService`].
pub trait ChangeHook: Send + Sync + fmt::Debug {
    /// Statements to run in the same transaction as the change (the outbox
    /// insert). An error aborts the change.
    fn statements(&self, change: &RecordChange<'_>) -> Result<Vec<Statement>, String>;

    /// Called after a transaction that produced at least one change committed.
    fn committed(&self);
}

/// Column type used to give a snapshot column the affinity of its table
/// column, so predicates compare values exactly as they do on the table.
fn column_type(coll: &Collection, column: &str) -> Option<&'static str> {
    match column {
        "id" | "created" | "updated" | "_data" => return Some("TEXT"),
        "_version" => return Some("INTEGER"),
        _ => {}
    }
    coll.field(column).map(|f| match f.kind.storage() {
        Storage::Text | Storage::Json => "TEXT",
        Storage::Integer | Storage::Bool => "INTEGER",
        Storage::Real => "REAL",
    })
}

impl DataService {
    /// Installs the change hook (once per service; `false` if one is set).
    pub fn set_change_hook(&self, hook: std::sync::Arc<dyn ChangeHook>) -> bool {
        self.inner.change_hook.set(hook).is_ok()
    }

    pub(crate) fn has_change_hook(&self) -> bool {
        self.inner.change_hook.get().is_some()
    }

    /// Runs the hook's statements for one change inside `tx`.
    pub(crate) async fn record_change(
        &self,
        tx: &mut dyn WriteTx,
        ctx: &AccessContext,
        coll: &Collection,
        op: ChangeOp,
        record_id: &str,
        (columns, row): (&[String], &[Value]),
    ) -> Result<(), DataError> {
        let Some(hook) = self.inner.change_hook.get() else {
            return Ok(());
        };
        let snapshot = RowSnapshot::of(coll, columns, row);
        let change = RecordChange {
            op,
            collection: coll,
            record_id,
            row: &snapshot,
            ctx,
        };
        let stmts = hook
            .statements(&change)
            .map_err(|e| DataError::Internal(format!("change hook failed: {e}")))?;
        for s in &stmts {
            tx.execute(s).await?;
        }
        Ok(())
    }

    /// Tells the hook that a transaction with changes committed.
    pub(crate) fn changes_committed(&self) {
        if let Some(hook) = self.inner.change_hook.get() {
            hook.committed();
        }
    }

    /// Checks that `ctx` may subscribe to changes of `collection` with
    /// `action` (`List` for the collection, `View` for one record) and that
    /// the optional `filter` compiles for this caller (hidden fields only for
    /// privileged callers). Returns the collection.
    pub fn subscription_target(
        &self,
        ctx: &AccessContext,
        collection: &str,
        action: Action,
        filter: Option<(&Expr, &HashMap<String, Json>)>,
    ) -> Result<Collection, DataError> {
        let coll = self.collection(collection)?;
        self.authorize(ctx, action, Some(&coll))?;
        if let Some((expr, params)) = filter {
            let opts = CompileOptions {
                allow_hidden: ctx.is_privileged(),
                params: params.clone(),
                ..CompileOptions::default()
            };
            let cat = self.catalog();
            let mut b = SqlBuilder::new(self.inner.backend.dialect());
            Compiler::new(&*cat, &opts).filter(&mut b, &coll, "t", expr)?;
        }
        Ok(coll)
    }

    /// What `ctx` may see of a stored row of collection `collection_id`,
    /// decided by the caller's **current** access for `action`, or `None` when
    /// the row is not visible (denied, outside the access predicate, outside
    /// `filter`, or the collection no longer exists).
    ///
    /// The access predicate and the filter are evaluated by the database
    /// against the snapshot itself (a one-row derived table with the table's
    /// column types), with the same compiler as list queries; related records
    /// in relation paths are read as they are now. The returned record has
    /// the same fields a REST read would return to this caller (no passwords;
    /// hidden fields only for privileged callers). This is the single place
    /// where realtime delivery applies row- and field-level rules.
    pub async fn view_snapshot(
        &self,
        ctx: &AccessContext,
        action: Action,
        collection_id: &str,
        row: &RowSnapshot,
        filter: Option<(&Expr, &HashMap<String, Json>)>,
    ) -> Result<Option<Record>, DataError> {
        let Some(coll) = self.catalog().get(collection_id).cloned() else {
            return Ok(None);
        };
        let pred = match self.authorize(ctx, action, Some(&coll)) {
            Ok(p) => p,
            Err(DataError::Unauthenticated | DataError::Forbidden(_)) => return Ok(None),
            Err(e) => return Err(e),
        };
        if pred.is_some() || filter.is_some() {
            let d = self.inner.backend.dialect();
            let mut b = SqlBuilder::new(d);
            let cols: Vec<String> = row
                .columns
                .iter()
                .zip(&row.values)
                .map(|(c, v)| {
                    let p = b.bind(v.clone());
                    match column_type(&coll, c) {
                        Some(ty) => format!("CAST({p} AS {ty}) AS {}", d.quote_ident(c)),
                        None => format!("{p} AS {}", d.quote_ident(c)),
                    }
                })
                .collect();
            let mut sql = format!("SELECT 1 FROM (SELECT {}) AS t WHERE 1 = 1", cols.join(", "));
            if let Some(p) = &pred {
                sql.push_str(&format!(" AND ({})", self.compile_predicate(ctx, &mut b, &coll, p)?));
            }
            if let Some((expr, params)) = filter {
                let opts = CompileOptions {
                    allow_hidden: ctx.is_privileged(),
                    params: params.clone(),
                    ..CompileOptions::default()
                };
                let cat = self.catalog();
                let f = Compiler::new(&*cat, &opts).filter(&mut b, &coll, "t", expr)?;
                sql.push_str(&format!(" AND ({f})"));
            }
            let rows = self
                .inner
                .backend
                .fetch_all(&Statement::new(sql, b.into_params()))
                .await?;
            if rows.rows.is_empty() {
                return Ok(None);
            }
        }
        Ok(Some(row_to_record(&coll, &row.columns, &row.values, ctx.is_privileged())))
    }
}
