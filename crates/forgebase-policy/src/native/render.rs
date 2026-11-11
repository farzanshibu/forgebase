//! [`NativeRender`] for policy generation: request references read the
//! claims (`forgebase.req`), relation lookups become `SECURITY DEFINER`
//! functions collected for the generator.

use std::sync::Mutex;

use forgebase_query::compile::ScalarType;
use forgebase_query::native::pg_type;
use forgebase_query::{NativeRender, QueryError};
use sha2::{Digest, Sha256};

use crate::rules::RequestRef;

/// A generated relation-lookup function.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelFn {
    /// Qualified name (`forgebase.fb_rel_<hash>`).
    pub name: String,
    /// `CREATE OR REPLACE FUNCTION ..` and its grants.
    pub statements: Vec<String>,
}

/// Renders one rule of one collection.
#[derive(Debug)]
pub(crate) struct PolicyRender {
    refs: Vec<RequestRef>,
    scoped: bool,
    rels: Mutex<Vec<RelFn>>,
}

/// A dollar-quote tag that does not occur in `body`.
pub(crate) fn dollar_tag(body: &str) -> String {
    let mut n = 0u32;
    loop {
        let tag = if n == 0 { "$fb$".to_owned() } else { format!("$fb{n}$") };
        if !body.contains(&tag) {
            return tag;
        }
        n += 1;
    }
}

/// `"name"` (double quotes doubled).
pub(crate) fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `'text'` (single quotes doubled; the generated SQL never has backslash
/// escapes: `standard_conforming_strings` is on).
pub(crate) fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// The `forgebase.req` path of a reference.
pub(crate) fn ref_path(r: &RequestRef) -> String {
    match r {
        RequestRef::Auth(p) => format!("auth.{}", p.join(".")),
        RequestRef::Tenant => "tenant".into(),
        RequestRef::Realm => "realm".into(),
        RequestRef::Org => "org".into(),
    }
}

/// Short stable hash for generated names.
pub(crate) fn short_hash(parts: &[&str]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p.as_bytes());
        h.update([0]);
    }
    h.finalize()[..8].iter().map(|b| format!("{b:02x}")).collect()
}

impl PolicyRender {
    pub(crate) fn new(refs: Vec<RequestRef>, scoped: bool) -> Self {
        Self {
            refs,
            scoped,
            rels: Mutex::new(Vec::new()),
        }
    }

    /// The relation functions this rule needs.
    pub(crate) fn take_relations(&self) -> Vec<RelFn> {
        std::mem::take(&mut *self.rels.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

impl NativeRender for PolicyRender {
    fn request(&self, name: &str) -> Option<String> {
        let i: usize = name.strip_prefix("__rq_")?.parse().ok()?;
        let r = self.refs.get(i)?;
        Some(format!(
            "(SELECT forgebase.req({}, {}))",
            literal(&ref_path(r)),
            self.scoped
        ))
    }

    fn now(&self) -> String {
        "(SELECT forgebase.now_text())".into()
    }

    fn relation(&self, target: &str, alias: &str, value_sql: &str, ty: ScalarType) -> Result<String, QueryError> {
        let body = format!(
            "SELECT {value_sql} FROM public.{} AS {alias} WHERE {alias}.\"id\" = $1",
            ident(target)
        );
        let name = format!("forgebase.fb_rel_{}", short_hash(&[target, &body, pg_type(ty)]));
        let tag = dollar_tag(&body);
        let statements = vec![
            format!(
                "CREATE OR REPLACE FUNCTION {name}(text) RETURNS {} LANGUAGE sql STABLE SECURITY DEFINER \
                 SET search_path = forgebase, public AS {tag}{body}{tag}",
                pg_type(ty)
            ),
            format!("REVOKE ALL ON FUNCTION {name}(text) FROM PUBLIC"),
            format!("GRANT EXECUTE ON FUNCTION {name}(text) TO {}", super::RLS_ROLE),
        ];
        let mut rels = self.rels.lock().unwrap_or_else(|p| p.into_inner());
        if !rels.iter().any(|r| r.name == name) {
            rels.push(RelFn {
                name: name.clone(),
                statements,
            });
        }
        Ok(name)
    }
}
