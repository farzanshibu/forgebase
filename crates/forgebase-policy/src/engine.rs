//! Step logic of the pipeline over one [`Snapshot`] (ADR-0008 §8).

use std::collections::HashMap;
use std::sync::Arc;

use forgebase_data::{
    AccessContext, Action, DEFAULT_REALM, Decision, FieldAccess, Predicate, Principal, ResourceRef as EngineResource,
    SubjectRef, is_tenant_scoped,
};
use forgebase_query::{Compare, Expr, Op, Operand};
use forgebase_schema::Collection;
use serde_json::Value as Json;

use crate::perm::{self, Grant, data_key};
use crate::pipeline::{Denial, HookOutcome, HookRequest, Hooks, ResourceRef, Step};
use crate::rules::{Eval, RequestRef, RequestValue, RequestValues};
use crate::snapshot::{Effective, RuleSpec, Snapshot};

/// A protected operation that is not a data-core record call.
#[derive(Debug, Clone, Default)]
pub struct Check {
    /// Permission key (`projects:create`).
    pub permission: String,
    /// Organization the operation acts in (`None`: the caller's personal
    /// scope, or realm level).
    pub org: Option<String>,
    /// The resource, for ACL checks (`project:<id>`).
    pub resource: Option<ResourceRef>,
    /// Personal-scope owner of the resource (principal key): holds every
    /// permission on it.
    pub owner: Option<String>,
    /// Anonymous callers may pass step 1.
    pub allow_anonymous: bool,
}

/// The realm a caller may act in. Users and service accounts belong to the
/// realm of their identity (FB-25: the auth service's realm directory);
/// anonymous callers act in the realm they select. Without a directory
/// (the engine used on its own) every identity lives in `default`.
pub fn home_realm(snap: &Snapshot, ctx: &AccessContext) -> String {
    match &snap.realm_dir {
        None => DEFAULT_REALM.to_owned(),
        Some(dir) => dir.home_realm(&ctx.principal).unwrap_or_else(|| ctx.realm_id.clone()),
    }
}

/// Step 1 for everyone.
pub(crate) fn step_authentication(snap: &Snapshot, ctx: &AccessContext, anonymous_ok: bool) -> Result<(), Denial> {
    match ctx.principal {
        Principal::Anonymous if !anonymous_ok => Err(Denial::at(Step::Authentication, "authentication required")),
        Principal::User { .. } | Principal::Service { .. } if snap.is_suspended(&ctx.principal_key(), crate::now()) => {
            Err(Denial::at(Step::Authentication, "account suspended"))
        }
        _ => Ok(()),
    }
}

/// Step 2: realm and organization of the request. Returns the org scope.
pub(crate) fn step_tenant(snap: &Snapshot, ctx: &AccessContext, org: Option<&str>) -> Result<(), Denial> {
    if !snap.realms.contains_key(&ctx.realm_id) {
        return Err(Denial::at(Step::Tenant, "unknown realm"));
    }
    if ctx.realm_id != home_realm(snap, ctx) {
        return Err(Denial::at(Step::Tenant, "cross-realm access is denied"));
    }
    if let Some(o) = org {
        let in_realm = snap.orgs.get(o).is_some_and(|x| x.realm_id == ctx.realm_id);
        let member = snap
            .membership(o, &ctx.principal_key())
            .is_some_and(|m| m.status == "active");
        if !in_realm || !member {
            return Err(Denial::at(Step::Tenant, "not an active member of this organization"));
        }
    }
    Ok(())
}

/// The subject a check is billed against: the organization it acts in, else
/// the caller's personal scope.
fn subject_of(ctx: &AccessContext, org: Option<&str>) -> SubjectRef {
    match org {
        Some(o) => SubjectRef::Org(o.to_owned()),
        None => ctx.subject(),
    }
}

fn hooks(
    hooks: &Hooks,
    ctx: &AccessContext,
    subject: &SubjectRef,
    permission: &str,
    resource: Option<&ResourceRef>,
    step: Step,
) -> Result<(), Denial> {
    let req = HookRequest {
        ctx,
        subject: subject.clone(),
        permission,
        resource,
    };
    let out = match step {
        Step::Entitlement => hooks.entitlement.check(&req),
        _ => hooks.quota.check(&req),
    };
    match out {
        HookOutcome::Allow => Ok(()),
        HookOutcome::Deny(reason) => Err(Denial::at(step, reason)),
    }
}

/// Effective permissions with the personal-scope owner rule.
pub(crate) fn effective(
    snap: &Snapshot,
    ctx: &AccessContext,
    org: Option<&str>,
    owner: Option<&str>,
) -> Arc<Effective> {
    let principal = ctx.principal_key();
    let eff = snap.effective(&principal, &ctx.realm_id, org);
    if owner.is_some_and(|o| o == principal) {
        let mut e = (*eff).clone();
        e.perms.allow.insert("*".into());
        e.roles.insert("owner".into());
        return Arc::new(e);
    }
    eff
}

/// Runs the pipeline for a generic operation.
pub(crate) fn run_check(snap: &Snapshot, h: &Hooks, ctx: &AccessContext, c: &Check) -> Result<(), Denial> {
    if ctx.is_privileged() {
        return Ok(()); // platform super-admin
    }
    step_authentication(snap, ctx, c.allow_anonymous)?;
    let personal = c.org.is_none() && c.owner.is_some();
    if !personal {
        step_tenant(snap, ctx, c.org.as_deref())?;
    } else if ctx.realm_id != home_realm(snap, ctx) {
        return Err(Denial::at(Step::Tenant, "cross-realm access is denied"));
    }
    let eff = effective(snap, ctx, c.org.as_deref(), c.owner.as_deref());
    let perm = eff.perms.check(&c.permission);
    if perm == Grant::Denied {
        return Err(Denial::at(Step::Permission, format!("{} is denied", c.permission)));
    }
    if perm == Grant::None && c.resource.is_none() {
        return Err(Denial::at(
            Step::Permission,
            format!("missing permission {}", c.permission),
        ));
    }
    let subject = subject_of(ctx, c.org.as_deref());
    hooks(h, ctx, &subject, &c.permission, c.resource.as_ref(), Step::Entitlement)?;
    if let Some(r) = &c.resource {
        let acl = acl_chain(snap, ctx, r, &c.permission, &eff);
        match (acl, perm) {
            (Grant::Denied, _) => return Err(Denial::at(Step::Resource, format!("{} is denied on {r}", c.permission))),
            (Grant::None, Grant::None) => {
                return Err(Denial::at(Step::Resource, format!("no access to {r}")));
            }
            _ => {}
        }
    }
    hooks(h, ctx, &subject, &c.permission, c.resource.as_ref(), Step::Quota)
}

/// ACL of a resource and its parents (an environment inherits its project's
/// entries); deny anywhere wins.
pub(crate) fn acl_chain(snap: &Snapshot, ctx: &AccessContext, r: &ResourceRef, key: &str, eff: &Effective) -> Grant {
    let principal = ctx.principal_key();
    let mut chain = vec![(r.kind.clone(), r.id.clone())];
    if r.kind == "environment"
        && let Some(e) = snap.envs.get(&r.id)
    {
        chain.push(("project".into(), e.project_id.clone()));
    }
    let mut out = Grant::None;
    for (t, id) in chain {
        match snap.acl(&ctx.realm_id, &t, &id, key, &principal, eff) {
            Grant::Denied => return Grant::Denied,
            Grant::Granted => out = Grant::Granted,
            Grant::None => {}
        }
    }
    out
}

/// The action name used in keys: `list`, `view`, ... or a snake-case engine
/// action (`KvRead` → `kv_read`).
pub fn action_name(action: Action) -> String {
    let debug = format!("{action:?}");
    let mut out = String::new();
    for (i, ch) in debug.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn rule_index(action: Action) -> Option<usize> {
    Some(match action {
        Action::List => 0,
        Action::View => 1,
        Action::Create => 2,
        Action::Update => 3,
        Action::Delete => 4,
        _ => return None,
    })
}

/// Whether API-key / PAT scopes cover a record action (defence in depth for
/// callers that do not come through the HTTP scope check).
pub(crate) fn scopes_allow(ctx: &AccessContext, coll: &str, write: bool) -> bool {
    let Some(scopes) = &ctx.scopes else { return true };
    let access = if write { "write" } else { "read" };
    scopes.iter().any(|s| {
        let p: Vec<&str> = s.split(':').collect();
        match p.as_slice() {
            ["*"] => true,
            [area, "*"] => matches!(*area, "collections" | "documents"),
            [area, a] => matches!(*area, "collections" | "documents") && *a == access,
            [area, a, r] => {
                matches!(*area, "collections" | "documents") && *a == access && r.eq_ignore_ascii_case(coll)
            }
            _ => false,
        }
    })
}

/// Request values of one caller for rule evaluation.
struct CtxValues<'a> {
    ctx: &'a AccessContext,
    eff: &'a Effective,
}

impl RequestValues for CtxValues<'_> {
    fn value(&self, r: &RequestRef) -> RequestValue {
        let one = |s: Option<String>| s.map_or(RequestValue::Absent, |v| RequestValue::One(Json::String(v)));
        match r {
            RequestRef::Tenant => one(self.ctx.tenant.clone()),
            RequestRef::Org => one(self.ctx.org_id.clone()),
            RequestRef::Realm => one(Some(self.ctx.realm_id.clone())),
            RequestRef::Auth(path) => {
                let p = &self.ctx.principal;
                match path.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                    ["id"] => one(p.id().map(str::to_owned)),
                    ["type"] => one(Some(p.kind().to_owned())),
                    ["authenticated"] => RequestValue::One(Json::Bool(*p != Principal::Anonymous)),
                    ["collection"] => match p {
                        Principal::User { collection, .. } => one(Some(collection.clone())),
                        _ => RequestValue::Absent,
                    },
                    ["roles"] => RequestValue::Many(self.eff.roles.iter().cloned().map(Json::String).collect()),
                    ["permissions"] => {
                        RequestValue::Many(self.eff.perms.allow.iter().cloned().map(Json::String).collect())
                    }
                    ["scopes"] => match &self.ctx.scopes {
                        Some(s) => RequestValue::Many(s.iter().cloned().map(Json::String).collect()),
                        None => RequestValue::Absent,
                    },
                    [first, rest @ ..] => {
                        let mut v = self.ctx.attributes.get(*first);
                        for seg in rest {
                            v = v.and_then(|x| x.get(*seg));
                        }
                        match v {
                            None | Some(Json::Null) => RequestValue::Absent,
                            Some(Json::Array(a)) => RequestValue::Many(a.clone()),
                            Some(Json::Object(_)) => RequestValue::Absent,
                            Some(j) => RequestValue::One(j.clone()),
                        }
                    }
                    [] => RequestValue::Absent,
                }
            }
        }
    }
}

/// Evaluates a rule for a caller: `Ok(None)` allow all rows, `Ok(Some(p))`
/// rows matching `p`, `Err(())` deny.
fn eval_rule(spec: &RuleSpec, ctx: &AccessContext, eff: &Effective) -> Result<Option<Predicate>, ()> {
    let rule = match spec {
        RuleSpec::Public => return Ok(None),
        RuleSpec::Expr(r) => r,
    };
    let (ev, params) = rule.evaluate(&CtxValues { ctx, eff }, &forgebase_schema::now_timestamp());
    match ev {
        Eval::True => Ok(None),
        Eval::False | Eval::Unknown => Err(()),
        Eval::Sql(e) => Ok(Some(Predicate { filter: e, params })),
    }
}

/// `id = 'a' || id = 'b'` over record ids (bound as parameters).
fn ids_expr(ids: &[String], params: &mut HashMap<String, Json>, tag: &str) -> Expr {
    Expr::Or(
        ids.iter()
            .enumerate()
            .map(|(i, id)| {
                let name = format!("__acl_{tag}_{i}");
                params.insert(name.clone(), Json::String(id.clone()));
                Expr::Compare(Compare {
                    left: Operand::Path(forgebase_query::FieldPath::parse("id").expect("id is a path")),
                    op: Op::Eq,
                    any: false,
                    right: Operand::Param(name),
                })
            })
            .collect(),
    )
}

/// (owning organization, personal owner, environment) of a tenant.
pub(crate) type TenantScope = (Option<String>, Option<String>, Option<String>);

/// The organization owning a tenant, and its personal owner.
pub(crate) fn tenant_scope(snap: &Snapshot, ctx: &AccessContext) -> Result<TenantScope, Denial> {
    let Some(t) = &ctx.tenant else {
        return Ok((None, None, None));
    };
    let Some((env, project)) = snap.tenant(t) else {
        if ctx.principal == Principal::Anonymous {
            return Ok((None, None, None)); // no rows carry an unknown tenant
        }
        return Err(Denial::at(Step::Tenant, "unknown tenant"));
    };
    if project.realm_id != ctx.realm_id {
        return Err(Denial::at(Step::Tenant, "cross-realm access is denied"));
    }
    if let (Some(o), Some(active)) = (&project.org_id, &ctx.org_id)
        && o != active
    {
        return Err(Denial::at(Step::Tenant, "the tenant belongs to another organization"));
    }
    Ok((project.org_id.clone(), project.owner_user.clone(), Some(env.id.clone())))
}

/// The data-core decision (the `Authorizer` path).
pub(crate) fn decide(
    snap: &Snapshot,
    h: &Hooks,
    ctx: &AccessContext,
    action: Action,
    coll: Option<&Collection>,
) -> Result<Decision, (Denial, String)> {
    if ctx.is_privileged() {
        // A privileged caller's scoped API key still only reaches the
        // collections its scopes name: the HTTP scope check sees the path's
        // collection only, not those read through `expand` or relation
        // filters (FB-80).
        if let Some(c) = coll.filter(|_| rule_index(action).is_some()) {
            let write = !matches!(action, Action::List | Action::View);
            if !scopes_allow(ctx, &c.name, write) {
                let key = data_key(&c.name, &action_name(action));
                return Err((Denial::at(Step::Permission, "insufficient_scope"), key));
            }
        }
        return Ok(Decision::Allow);
    }
    let name = action_name(action);
    let Some(coll) = coll.filter(|_| rule_index(action).is_some()) else {
        return decide_engine(snap, h, ctx, action, &name).map(|()| Decision::Allow);
    };
    let key = data_key(&coll.name, &name);
    let fail = |d: Denial| (d, key.clone());
    step_authentication(snap, ctx, true).map_err(fail)?;
    if ctx.realm_id != home_realm(snap, ctx) || !snap.realms.contains_key(&ctx.realm_id) {
        return Err(fail(Denial::at(Step::Tenant, "cross-realm access is denied")));
    }
    let scoped = is_tenant_scoped(coll);
    let (org, owner, env) = if scoped {
        tenant_scope(snap, ctx).map_err(fail)?
    } else {
        (None, None, None)
    };
    let principal = ctx.principal_key();
    if scoped && ctx.principal != Principal::Anonymous {
        let member = match (&org, &owner) {
            (Some(o), _) => snap.membership(o, &principal).is_some_and(|m| m.status == "active"),
            (None, Some(u)) => *u == principal,
            _ => true,
        };
        let shared = env.as_ref().is_some_and(|e| {
            let eff = snap.effective(&principal, &ctx.realm_id, org.as_deref());
            acl_chain(snap, ctx, &ResourceRef::new("environment", e), &key, &eff) == Grant::Granted
        });
        if !member && !shared {
            return Err(fail(Denial::at(
                Step::Tenant,
                "not a member of the tenant's organization",
            )));
        }
    }
    // Org roles reach only tenant-scoped data of that org's tenants;
    // shared collections see realm-level grants only.
    let eff = effective(snap, ctx, org.as_deref(), owner.as_deref());
    let write = !matches!(action, Action::List | Action::View);
    if !scopes_allow(ctx, &coll.name, write) {
        return Err(fail(Denial::at(Step::Permission, "insufficient_scope")));
    }
    let perm = eff.perms.check(&key);
    if perm == Grant::Denied {
        return Err(fail(Denial::at(Step::Permission, format!("{key} is denied"))));
    }
    let subject = subject_of(ctx, org.as_deref());
    hooks(h, ctx, &subject, &key, None, Step::Entitlement).map_err(fail)?;
    let coll_acl = snap.acl(&ctx.realm_id, "collection", &coll.name, &key, &principal, &eff);
    if coll_acl == Grant::Denied {
        return Err(fail(Denial::at(
            Step::Resource,
            format!("{key} is denied on this collection"),
        )));
    }
    let (acl_allow, acl_deny) = if matches!(action, Action::Create) {
        (Vec::new(), Vec::new())
    } else {
        snap.record_acl(&ctx.realm_id, &coll.name, &key, &principal, &eff)
    };
    let mut params = HashMap::new();
    let granted = perm == Grant::Granted || coll_acl == Grant::Granted;
    let mut allowed: Option<Expr> = if granted {
        None // every row
    } else {
        let idx = rule_index(action).unwrap_or_default();
        let rule = snap.policies.get(&coll.id).and_then(|p| p.rules[idx].as_ref());
        let by_rule = match rule.map(|r| eval_rule(r, ctx, &eff)) {
            Some(Ok(None)) => Some(None),
            Some(Ok(Some(p))) => {
                params.extend(p.params);
                Some(Some(p.filter))
            }
            Some(Err(())) | None => None,
        };
        match (by_rule, acl_allow.is_empty()) {
            (Some(None), _) => None,
            (Some(Some(e)), true) => Some(e),
            (Some(Some(e)), false) => Some(Expr::Or(vec![e, ids_expr(&acl_allow, &mut params, "a")])),
            (None, false) => Some(ids_expr(&acl_allow, &mut params, "a")),
            (None, true) => {
                let why = if rule.is_some() {
                    "the collection rule does not match"
                } else {
                    "no rule grants this"
                };
                return Err(fail(Denial::at(Step::Resource, why)));
            }
        }
    };
    if !acl_deny.is_empty() {
        let not = Expr::Not(Box::new(ids_expr(&acl_deny, &mut params, "d")));
        allowed = Some(match allowed {
            Some(e) => Expr::And(vec![e, not]),
            None => not,
        });
    }
    hooks(h, ctx, &subject, &key, None, Step::Quota).map_err(fail)?;
    Ok(match allowed {
        None => Decision::Allow,
        Some(filter) => Decision::Filter(Predicate { filter, params }),
    })
}

/// Engine and schema actions (`engine:kv_read`, `schema:read_schema`): a
/// granted permission is required. SQL and schema changes stay privileged.
fn decide_engine(
    snap: &Snapshot,
    h: &Hooks,
    ctx: &AccessContext,
    action: Action,
    name: &str,
) -> Result<(), (Denial, String)> {
    let key = match action {
        Action::ReadSchema => "schema:read".to_owned(),
        Action::ManageSchema | Action::ManageIndexes | Action::ReadSql | Action::WriteSql => {
            return Err((
                Denial::at(Step::Permission, "administrators only"),
                format!("admin:{name}"),
            ));
        }
        _ => format!("engine:{name}"),
    };
    let (org, owner) = engine_tenant(snap, ctx, &key).map_err(|d| (d, key.clone()))?;
    let check = Check {
        permission: key.clone(),
        org: if owner.is_some() { None } else { org },
        resource: None,
        owner,
        allow_anonymous: false,
    };
    run_check(snap, h, ctx, &check).map_err(|d| (d, key))
}

/// (organization, personal owner) of the caller's tenant for an engine
/// action. Engines (KV, graph, jobs, ...) scope their data by `ctx.tenant`,
/// so the tenant is validated as strictly as for records: it must exist in
/// the caller's realm, belong to the active organization if one is set, and
/// be the caller's (membership via step 2, or personal ownership, or an ACL
/// grant on the environment / project). Without a tenant: the active
/// organization.
fn engine_tenant(snap: &Snapshot, ctx: &AccessContext, key: &str) -> Result<(Option<String>, Option<String>), Denial> {
    let fail = |why: &str| Denial::at(Step::Tenant, why);
    let Some(t) = &ctx.tenant else {
        return Ok((ctx.org_id.clone(), None));
    };
    let Some((env, p)) = snap.tenant(t) else {
        return Err(fail("unknown tenant"));
    };
    if p.realm_id != ctx.realm_id {
        return Err(fail("cross-realm access is denied"));
    }
    if ctx.org_id.is_some() && ctx.org_id != p.org_id {
        return Err(fail("the tenant belongs to another organization"));
    }
    if let Some(u) = &p.owner_user {
        let principal = ctx.principal_key();
        // Anonymous callers own nothing; only a resource rule can grant them
        // anything (FB-134), as for records.
        if *u != principal && ctx.principal != Principal::Anonymous {
            let eff = snap.effective(&principal, &ctx.realm_id, None);
            let shared = acl_chain(snap, ctx, &ResourceRef::new("environment", &env.id), key, &eff);
            if shared != Grant::Granted {
                return Err(fail("this tenant is not yours"));
            }
        }
    }
    Ok((p.org_id.clone(), p.owner_user.clone()))
}

/// Engine actions that only a permission grants: resource rules never do.
fn admin_only(action: Action) -> bool {
    matches!(
        action,
        Action::KvAdmin | Action::VectorAdmin | Action::StorageAdmin | Action::QueueAdmin
    )
}

/// The decision for an engine action on one named resource (FB-134,
/// `Authorizer::authorize_resource`). In pipeline order:
///
/// 1. authentication (anonymous callers continue: only a rule can grant
///    them anything);
/// 2. realm and tenant as for [`decide_engine`]; signed-in callers must be
///    members of the tenant's (or active) organization;
/// 3. permission: `engine:<action>:<name>` or `engine:<action>` (every
///    resource); the resource's owner holds every permission on it; an
///    explicit deny of either key wins;
/// 4. entitlement hook (asked with `engine:<action>` and the resource);
/// 5. resource rule, when no permission grants: the rule of the operation
///    ([`Snapshot::resource_rule`]); none is a denial, and admin actions
///    are never granted by a rule;
/// 6. quota hook.
///
/// Actions without resource keys get the plain [`decide`] answer.
pub(crate) fn decide_resource(
    snap: &Snapshot,
    h: &Hooks,
    ctx: &AccessContext,
    action: Action,
    r: &EngineResource,
) -> Result<Decision, (Denial, String)> {
    let name = action_name(action);
    if !perm::RESOURCE_ACTIONS.contains(&name.as_str()) {
        return decide(snap, h, ctx, action, None);
    }
    if ctx.is_privileged() {
        return Ok(Decision::Allow);
    }
    let base = format!("engine:{name}");
    let key = perm::resource_key(&name, &r.name);
    let fail = |d: Denial| (d, key.clone());
    step_authentication(snap, ctx, true).map_err(fail)?;
    if !snap.realms.contains_key(&ctx.realm_id) {
        return Err(fail(Denial::at(Step::Tenant, "unknown realm")));
    }
    if ctx.realm_id != home_realm(snap, ctx) {
        return Err(fail(Denial::at(Step::Tenant, "cross-realm access is denied")));
    }
    let (org, tenant_owner) = engine_tenant(snap, ctx, &key).map_err(fail)?;
    let principal = ctx.principal_key();
    let personal = tenant_owner.is_some();
    if ctx.principal != Principal::Anonymous && !personal {
        step_tenant(snap, ctx, org.as_deref()).map_err(fail)?;
    }
    let org = if personal { None } else { org };
    let mut eff = effective(snap, ctx, org.as_deref(), tenant_owner.as_deref());
    if r.owner.as_deref().is_some_and(|o| o == principal) && ctx.principal != Principal::Anonymous {
        let mut e = (*eff).clone();
        e.perms.allow.insert("*".into());
        e.roles.insert("owner".into());
        eff = Arc::new(e);
    }
    let anonymous = ctx.principal == Principal::Anonymous;
    let perm = if anonymous { Grant::None } else { eff.perms.check(&key) };
    if perm == Grant::Denied {
        return Err(fail(Denial::at(Step::Permission, format!("{key} is denied"))));
    }
    let subject = subject_of(ctx, org.as_deref());
    let hook_ref = ResourceRef::new(r.kind.as_str(), r.name.clone());
    hooks(h, ctx, &subject, &base, Some(&hook_ref), Step::Entitlement).map_err(fail)?;
    let decision = if perm == Grant::Granted {
        Decision::Allow
    } else if admin_only(action) {
        return Err(fail(Denial::at(Step::Permission, format!("missing permission {key}"))));
    } else {
        let Some(spec) = snap.resource_rule(r.kind.as_str(), &r.name, r.op) else {
            return Err(fail(Denial::at(
                Step::Resource,
                format!("missing permission {key} and no rule grants {} on {}", r.op, r),
            )));
        };
        match eval_rule(spec, ctx, &eff) {
            Ok(None) => Decision::Allow,
            Ok(Some(p)) => Decision::Filter(p),
            Err(()) => return Err(fail(Denial::at(Step::Resource, "the resource rule does not match"))),
        }
    };
    hooks(h, ctx, &subject, &base, Some(&hook_ref), Step::Quota).map_err(fail)?;
    Ok(decision)
}

/// Field-level access from static field rules.
pub(crate) fn field_access(snap: &Snapshot, ctx: &AccessContext, coll: &Collection) -> FieldAccess {
    let mut out = FieldAccess::default();
    let Some(policy) = snap.policies.get(&coll.id) else {
        return out;
    };
    if policy.fields.is_empty() {
        return out;
    }
    let org = if is_tenant_scoped(coll) {
        tenant_scope(snap, ctx).ok().and_then(|(o, _, _)| o)
    } else {
        None
    };
    let eff = snap.effective(&ctx.principal_key(), &ctx.realm_id, org.as_deref());
    for (name, f) in &policy.fields {
        let pass = |spec: &Option<RuleSpec>| match spec {
            None => true,
            Some(s) => matches!(eval_rule(s, ctx, &eff), Ok(None)),
        };
        if !pass(&f.read) {
            out.unreadable.insert(name.clone());
        }
        if !pass(&f.write) {
            out.read_only.insert(name.clone());
        }
    }
    out
}
