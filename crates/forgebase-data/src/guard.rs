//! Access enforcement shared by every service method (FB-21, ADR-0008).
//!
//! * [`DataService::resolve`]: looks a collection up and authorises the call.
//!   For callers who are not privileged a missing collection is reported
//!   exactly like a forbidden one (no existence oracle).
//! * Tenant clauses: every statement on a tenant-scoped collection carries
//!   `_tenant = <ctx.tenant>`; inserts stamp it; relations stay in one tenant.
//! * Field masks: hidden fields and the authorizer's field-level rules decide
//!   which fields a caller can read (output, expand, filter, sort, group-by,
//!   aggregates, full-text search) and write.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use forgebase_query::{FieldGuard, SqlBuilder};
use forgebase_schema::{Collection, Field, FieldKind};
use serde_json::{Map, Value as Json};

use crate::access::{TENANT_COLUMN, is_tenant_scoped, reads_tenant_data};
use crate::{AccessContext, Action, Authorizer, DataError, DataService, Decision, Predicate, Principal};

/// Which fields of one collection a caller may read.
#[derive(Debug, Clone, Default)]
pub(crate) struct FieldMask {
    /// Lower-case names of fields left out.
    hidden: BTreeSet<String>,
    /// Privileged callers also see the `_tenant` column.
    show_tenant: bool,
}

impl FieldMask {
    /// Everything (system, administrators, internal reads).
    pub(crate) fn all() -> Self {
        Self {
            hidden: BTreeSet::new(),
            show_tenant: true,
        }
    }

    /// Whether `field` is returned.
    pub(crate) fn allows(&self, field: &Field) -> bool {
        !self.hidden.contains(&field.name.to_ascii_lowercase())
    }

    pub(crate) fn show_tenant(&self) -> bool {
        self.show_tenant
    }

    /// Removes masked fields from a record (version history, write results).
    pub(crate) fn strip(&self, coll: &Collection, rec: &mut Map<String, Json>) {
        for f in coll.fields.iter().filter(|f| !self.allows(f)) {
            rec.remove(&f.name);
        }
        if !self.show_tenant {
            rec.remove(TENANT_COLUMN);
        }
    }
}

/// The data core's [`FieldGuard`] for one caller and one root collection: a
/// field is readable when the caller's mask allows it, and fields of *other*
/// collections (relation paths) only when the caller may list that collection
/// without a row restriction (otherwise a path such as `owner.email ~ 'a'`
/// would read rows the caller cannot see).
#[derive(Debug)]
pub(crate) struct DataGuard {
    ctx: AccessContext,
    authorizer: Arc<dyn Authorizer>,
    root: String,
    cache: Mutex<HashMap<String, Option<FieldMask>>>,
}

impl DataGuard {
    fn mask_of(&self, coll: &Collection) -> Option<FieldMask> {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache
            .entry(coll.id.clone())
            .or_insert_with(|| {
                if coll.id != self.root {
                    let open = matches!(self.authorizer.authorize(&self.ctx, Action::List, Some(coll)), Decision::Allow);
                    if !open || reads_tenant_data(coll) {
                        return None;
                    }
                }
                Some(read_mask(self.authorizer.as_ref(), &self.ctx, coll))
            })
            .clone()
    }
}

impl FieldGuard for DataGuard {
    fn readable(&self, collection: &Collection, field: &Field) -> bool {
        self.mask_of(collection).is_some_and(|m| m.allows(field))
    }
}

/// The read mask of `ctx` on `coll`.
pub(crate) fn read_mask(authorizer: &dyn Authorizer, ctx: &AccessContext, coll: &Collection) -> FieldMask {
    if ctx.is_privileged() {
        return FieldMask::all();
    }
    let mut hidden: BTreeSet<String> = coll
        .fields
        .iter()
        .filter(|f| f.hidden)
        .map(|f| f.name.to_ascii_lowercase())
        .collect();
    hidden.extend(authorizer.field_access(ctx, coll).unreadable);
    FieldMask {
        hidden,
        show_tenant: false,
    }
}

impl DataService {
    /// The error for a denied call. It names the collection as the caller
    /// spelled it, so a denial of an existing collection and of a missing one
    /// look the same.
    pub(crate) fn denied(&self, ctx: &AccessContext, action: Action, requested: Option<&str>) -> DataError {
        if ctx.principal == Principal::Anonymous {
            DataError::Unauthenticated
        } else {
            DataError::Forbidden(format!(
                "{action:?}{} is not allowed",
                requested.map(|c| format!(" on {c}")).unwrap_or_default()
            ))
        }
    }

    /// Looks up `name` and authorises `action` on it. Non-privileged callers
    /// get the same denial for a missing collection as for a forbidden one,
    /// and are refused tenant-scoped collections without a tenant.
    pub(crate) fn resolve(
        &self,
        ctx: &AccessContext,
        action: Action,
        name: &str,
    ) -> Result<(Collection, Option<Predicate>), DataError> {
        let coll = match self.collection(name) {
            Ok(c) => c,
            Err(DataError::CollectionNotFound(_)) if !ctx.is_privileged() => {
                return Err(self.denied(ctx, action, Some(name)));
            }
            Err(e) => return Err(e),
        };
        if reads_tenant_data(&coll) && !ctx.is_privileged() {
            return Err(self.denied(ctx, action, Some(name)));
        }
        let pred = match self.authorize(ctx, action, Some(&coll)) {
            Ok(p) => p,
            Err(DataError::Forbidden(_) | DataError::Unauthenticated) => {
                return Err(self.denied(ctx, action, Some(name)));
            }
            Err(e) => return Err(e),
        };
        let record_action = matches!(
            action,
            Action::List | Action::View | Action::Create | Action::Update | Action::Delete
        );
        if record_action && is_tenant_scoped(&coll) && ctx.tenant.is_none() && !ctx.is_privileged() {
            return Err(DataError::invalid(
                "tenant_required",
                format!("{name} is tenant-scoped: select a tenant (X-ForgeBase-Tenant header)"),
            ));
        }
        Ok((coll, pred))
    }

    /// `<alias>._tenant = ?` for a tenant-scoped collection, when the caller
    /// has a tenant. Privileged callers without a tenant see every tenant;
    /// anyone else without one sees nothing (`resolve` already refused them).
    pub(crate) fn tenant_clause(
        &self,
        ctx: &AccessContext,
        b: &mut SqlBuilder<'_>,
        coll: &Collection,
        alias: &str,
    ) -> Option<String> {
        if !is_tenant_scoped(coll) {
            return None;
        }
        let col = if alias.is_empty() {
            b.ident(TENANT_COLUMN)
        } else {
            format!("{alias}.{}", b.ident(TENANT_COLUMN))
        };
        match &ctx.tenant {
            Some(t) => Some(format!("{col} = {}", b.bind(t.clone()))),
            None if ctx.is_privileged() => None,
            None => Some("1 = 0".to_owned()),
        }
    }

    /// The tenant stamped on a new row of `coll` (`None` if not scoped).
    pub(crate) fn tenant_for_insert(&self, ctx: &AccessContext, coll: &Collection) -> Result<Option<String>, DataError> {
        if !is_tenant_scoped(coll) {
            return Ok(None);
        }
        ctx.tenant.clone().map(Some).ok_or_else(|| {
            DataError::invalid(
                "tenant_required",
                format!("{} is tenant-scoped: select a tenant to create records", coll.name),
            )
        })
    }

    /// Compiles an access predicate against `coll` without running it, so a
    /// rule that names unknown fields is refused when it is saved.
    pub fn validate_predicate(&self, coll: &Collection, p: &Predicate) -> Result<(), DataError> {
        let opts = forgebase_query::CompileOptions {
            allow_hidden: true,
            params: p.params.clone(),
            ..forgebase_query::CompileOptions::default()
        };
        let cat = self.catalog();
        let mut b = SqlBuilder::new(self.inner.backend.dialect());
        forgebase_query::Compiler::new(&*cat, &opts).filter(&mut b, coll, "t", &p.filter)?;
        Ok(())
    }

    /// The caller's read mask on `coll`.
    pub(crate) fn mask(&self, ctx: &AccessContext, coll: &Collection) -> FieldMask {
        read_mask(self.inner.authorizer.as_ref(), ctx, coll)
    }

    /// The field guard for compiling a caller's filter, sort or aggregate on
    /// `root` (`None` for privileged callers).
    pub(crate) fn field_guard(&self, ctx: &AccessContext, root: &Collection) -> Option<Arc<dyn FieldGuard>> {
        if ctx.is_privileged() {
            return None;
        }
        Some(Arc::new(DataGuard {
            ctx: ctx.clone(),
            authorizer: self.inner.authorizer.clone(),
            root: root.id.clone(),
            cache: Mutex::new(HashMap::new()),
        }))
    }

    /// Refuses writes to fields the caller may not write. `doc` is the
    /// document about to be validated (for an update: after the merge patch or
    /// replacement), `sent` what the caller sent, `current` the stored record
    /// (`None` on create). A create may not set a read-only field; an update
    /// may not change one, and a replacing update that leaves one out keeps its
    /// stored value.
    pub(crate) fn check_writable(
        &self,
        ctx: &AccessContext,
        coll: &Collection,
        doc: &mut Map<String, Json>,
        current: Option<&Map<String, Json>>,
        sent: &Map<String, Json>,
    ) -> Result<(), DataError> {
        if ctx.is_privileged() {
            return Ok(());
        }
        let read_only = self.inner.authorizer.field_access(ctx, coll).read_only;
        if read_only.is_empty() {
            return Ok(());
        }
        let refuse = |f: &Field| DataError::Forbidden(format!("field {} is read-only for you", f.name));
        for f in coll
            .fields
            .iter()
            .filter(|f| read_only.contains(&f.name.to_ascii_lowercase()))
        {
            let Some(cur) = current else {
                if sent.get(&f.name).is_some_and(|v| !v.is_null()) {
                    return Err(refuse(f));
                }
                continue;
            };
            if matches!(f.kind, FieldKind::Password { .. }) {
                // Never read back: sending any value is a change.
                if sent.contains_key(&f.name) {
                    return Err(refuse(f));
                }
                continue;
            }
            let stored = cur.get(&f.name).cloned().unwrap_or(Json::Null);
            match doc.get(&f.name) {
                None => {
                    doc.insert(f.name.clone(), stored);
                }
                Some(v) if *v != stored => return Err(refuse(f)),
                Some(_) => {}
            }
        }
        Ok(())
    }
}
