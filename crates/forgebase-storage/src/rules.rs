//! Object rules (FB-140, `rls-coverage.md` G14): bucket resource rules
//! (FB-134) over the fields of an object version, compiled into the object
//! SQL.
//!
//! A caller that no bucket owner, visibility, ACL entry or permission lets in
//! is asked about through [`Authorizer::authorize_resource`] with the
//! operation of the call (`list`, `view`, `create`, `update`, `delete`). A
//! rule that depends on the object comes back as a predicate over the fields
//! of [`object_schema`], which are columns of `_forgebase_storage_objects`:
//!
//! | field | value |
//! |---|---|
//! | `key` | the object key |
//! | `created_by` | principal key of the creator (`user:<collection>/<id>`, `service:<id>`, `admin:<id>`) |
//! | `owner` | the creator's id, as `@request.auth.id` reports it |
//! | `owner_collection` | a user creator's auth collection (`@request.auth.collection`) |
//! | `size` | bytes |
//! | `content_type` | the stored content type |
//! | `folders` | JSON array of the key's folder names: `a/b/c.txt` → `["a","b"]` |
//!
//! Reads add the predicate to their `WHERE` clause; writes check it on the
//! version they replace (`update`, `delete`) and on the version they write
//! (`create`, `update`), inside the transaction that installs it.
//!
//! [`Authorizer::authorize_resource`]: forgebase_data::Authorizer::authorize_resource

use forgebase_data::query::SqlBuilder;
use forgebase_data::{Decision, Predicate, ResourceFieldType, ResourceKind, ResourceSchema};
use forgebase_db::{ColumnType, Dialect, Statement, Value};

use crate::error::{Result, StorageError};

/// The item fields bucket rules may read (registered with the authorizer
/// when the service opens).
pub fn object_schema() -> ResourceSchema {
    use ResourceFieldType::{Json, Number, Text};
    ResourceSchema::new(ResourceKind::Bucket)
        .field("key", Text)
        .field("created_by", Text)
        .field("owner", Text)
        .field("owner_collection", Text)
        .field("size", Number)
        .field("content_type", Text)
        .field("folders", Json)
}

/// What a caller may do to the objects of a bucket after authorisation.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Access {
    /// Every object.
    All,
    /// Objects matching the predicate (a bucket rule over object fields).
    Filter(Predicate),
}

impl Access {
    pub(crate) fn filter(&self) -> Option<&Predicate> {
        match self {
            Access::All => None,
            Access::Filter(p) => Some(p),
        }
    }
}

/// The rule decisions of a write that adds a version: `create` applies when
/// the key holds no live object when the version is installed, `update`
/// otherwise. `None`: the caller is let in without rules.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct WriteCheck {
    pub rules: Option<(Decision, Decision)>,
}

impl WriteCheck {
    /// No rule applies.
    pub(crate) fn open() -> Self {
        Self { rules: None }
    }

    /// The decision for a write onto a key that holds a live object
    /// (`exists`) or not.
    pub(crate) fn decision(&self, exists: bool) -> Option<&Decision> {
        self.rules.as_ref().map(|(c, u)| if exists { u } else { c })
    }
}

/// Folder names of `key`: every `/`-separated segment but the last.
pub(crate) fn folders_of(key: &str) -> Vec<&str> {
    let mut parts: Vec<&str> = key.split('/').collect();
    parts.pop();
    parts
}

/// [`folders_of`] as a JSON array.
pub(crate) fn folders_json(key: &str) -> String {
    serde_json::to_string(&folders_of(key)).unwrap_or_else(|_| "[]".into())
}

/// `(owner, owner_collection)` of a creator's principal key. Matches the
/// backfill of migration 0029.
pub(crate) fn owner_of(principal_key: &str) -> (Option<String>, Option<String>) {
    if let Some((collection, id)) = principal_key.strip_prefix("user:").and_then(|r| r.split_once('/')) {
        return (Some(id.to_owned()), Some(collection.to_owned()));
    }
    for prefix in ["service:", "admin:"] {
        if let Some(id) = principal_key.strip_prefix(prefix) {
            return (Some(id.to_owned()), None);
        }
    }
    (None, None)
}

fn compile(b: &mut SqlBuilder<'_>, p: &Predicate) -> Result<String> {
    object_schema().compile(b, "o", p).map_err(|e| {
        tracing::warn!(error = %e, "a bucket rule could not be compiled; access denied");
        StorageError::Forbidden("this bucket's rule cannot be applied".into())
    })
}

/// `sql` with `{rule}` replaced by `filter` compiled over the objects table
/// aliased `o` (`1 = 1` without a filter). `sql` numbers its own parameters
/// `?1`..`?n` (`params`, bound first); the rule's follow.
pub(crate) fn statement(dialect: &dyn Dialect, sql: &str, params: Vec<Value>, filter: Option<&Predicate>) -> Result<Statement> {
    let mut b = SqlBuilder::new(dialect);
    for p in params {
        b.bind(p);
    }
    let cond = match filter {
        Some(p) => compile(&mut b, p)?,
        None => "1 = 1".to_owned(),
    };
    Ok(Statement::new(sql.replace("{rule}", &cond), b.into_params()))
}

/// The fields of a version about to be written.
#[derive(Debug, Clone)]
pub(crate) struct NewRow<'a> {
    pub key: &'a str,
    pub created_by: &'a str,
    /// `None` while unknown (a signed upload URL being made): a rule that
    /// reads it is then not satisfied.
    pub size: Option<u64>,
    pub content_type: Option<&'a str>,
}

/// A query that returns one row when `row` satisfies `p`.
pub(crate) fn new_row_check(dialect: &dyn Dialect, row: &NewRow<'_>, p: &Predicate) -> Result<Statement> {
    let (owner, collection) = owner_of(row.created_by);
    let opt = |v: Option<String>| v.map_or(Value::Null, Value::Text);
    let text = dialect.column_type(ColumnType::Text);
    let int = dialect.column_type(ColumnType::Integer);
    let json = dialect.column_type(ColumnType::Json);
    let sql = format!(
        "SELECT 1 FROM (SELECT CAST(?1 AS {text}) AS \"key\", CAST(?2 AS {text}) AS \"created_by\", \
         CAST(?3 AS {text}) AS \"owner\", CAST(?4 AS {text}) AS \"owner_collection\", CAST(?5 AS {int}) AS \"size\", \
         CAST(?6 AS {text}) AS \"content_type\", CAST(?7 AS {json}) AS \"folders\") o WHERE {{rule}}"
    );
    statement(
        dialect,
        &sql,
        vec![
            row.key.into(),
            row.created_by.into(),
            opt(owner),
            opt(collection),
            row.size.map_or(Value::Null, crate::service::iv),
            row.content_type.map_or(Value::Null, |c| Value::Text(c.to_owned())),
            folders_json(row.key).into(),
        ],
        Some(p),
    )
}

/// A query that returns one row when the stored version `id` satisfies `p`.
pub(crate) fn stored_row_check(dialect: &dyn Dialect, id: &str, p: &Predicate) -> Result<Statement> {
    statement(
        dialect,
        "SELECT 1 FROM _forgebase_storage_objects o WHERE o.id = ?1 AND {rule}",
        vec![id.into()],
        Some(p),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn folders_and_owners() {
        assert_eq!(folders_of("a/b/c.txt"), vec!["a", "b"]);
        assert_eq!(folders_of("c.txt"), Vec::<&str>::new());
        assert_eq!(folders_of("a/b/"), vec!["a", "b"]);
        assert_eq!(folders_json("x\"y/z"), "[\"x\\\"y\"]");
        assert_eq!(owner_of("user:people/u1"), (Some("u1".into()), Some("people".into())));
        assert_eq!(owner_of("service:s1"), (Some("s1".into()), None));
        assert_eq!(owner_of("admin:root"), (Some("root".into()), None));
        assert_eq!(owner_of("signed-url"), (None, None));
    }

    #[test]
    fn rules_compile_over_object_columns() {
        let d = forgebase_db::SqliteDialect;
        let p = Predicate::parse("folders.0 = 'u1' && size < 10", HashMap::new()).unwrap();
        let s = statement(&d, "SELECT 1 FROM t o WHERE o.bucket = ?1 AND {rule}", vec!["b".into()], Some(&p)).unwrap();
        assert!(s.sql.contains("json_extract(o.\"folders\", '$[0]')"), "{}", s.sql);
        assert_eq!(s.params.len(), 3);
        let pg = forgebase_db::PgDialect;
        let row = NewRow {
            key: "u1/a.png",
            created_by: "user:people/u1",
            size: Some(3),
            content_type: Some("image/png"),
        };
        let s = new_row_check(&pg, &row, &p).unwrap();
        assert!(s.sql.contains("CAST(?7 AS jsonb)"), "{}", s.sql);
        assert!(statement(&d, "{rule}", vec![], Some(&Predicate::parse("id = 'x'", HashMap::new()).unwrap())).is_err());
    }
}
