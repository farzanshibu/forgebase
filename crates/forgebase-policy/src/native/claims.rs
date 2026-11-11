//! The caller's claims (rls-coverage §3.3): what policies read through
//! `forgebase.claims()`. Computed in Rust from the policy snapshot, with the
//! same steps as [`crate::engine::decide`], so the SQL helpers never
//! re-implement memberships, role sets or deny-wins.

use forgebase_data::{AccessContext, Action, Principal, is_tenant_scoped};
use forgebase_schema::Collection;
use serde_json::{Value as Json, json};

use crate::engine::{acl_chain, action_name, effective, home_realm, scopes_allow, step_authentication, tenant_scope};
use crate::perm::{Grant, data_key};
use crate::pipeline::ResourceRef;
use crate::snapshot::{Effective, Snapshot};

/// Version of the claims layout.
pub const CLAIMS_VERSION: i64 = 1;

fn eff_json(e: &Effective) -> Json {
    json!({
        "allow": e.perms.allow.iter().collect::<Vec<_>>(),
        "deny": e.perms.deny.iter().collect::<Vec<_>>(),
        "roles": e.roles.iter().collect::<Vec<_>>(),
    })
}

fn grant_name(g: Grant) -> &'static str {
    match g {
        Grant::Denied => "denied",
        Grant::Granted => "granted",
        Grant::None => "none",
    }
}

/// Claims of `ctx` for `action` on `coll`:
///
/// ```json
/// { "v": 1, "principal": "user:users:u1",
///   "auth": {"id": "u1", "type": "user", "collection": "users", "authenticated": true},
///   "realm": "default", "org": null, "tenant": "env1", "scopes": null, "attributes": {..},
///   "action": "list", "suspended": false,
///   "gate": {"shared": true, "tenant": true},
///   "eff": {"shared": {"allow": [..], "deny": [..], "roles": [..]}, "tenant": {..}},
///   "colls": {"posts": {"list": {"grant": "none", "allow": ["r1"], "deny": []}}} }
/// ```
///
/// `gate` holds the outcome of the steps decided before any query (realm,
/// tenant membership); `eff` the effective permissions and roles for shared
/// and for tenant-scoped collections (organization roles reach only the
/// latter); `colls` the collection ACL (folded with API-key scopes into
/// `grant`) and the record ACL ids of this collection and action.
pub fn claims(snap: &Snapshot, ctx: &AccessContext, action: Action, coll: &Collection) -> Json {
    let principal = ctx.principal_key();
    let p = &ctx.principal;
    let auth = json!({
        "id": p.id(),
        "type": p.kind(),
        "collection": match p {
            Principal::User { collection, .. } => Some(collection.as_str()),
            _ => None,
        },
        "authenticated": *p != Principal::Anonymous,
    });
    let name = action_name(action);
    let key = data_key(&coll.name, &name);
    let suspended = step_authentication(snap, ctx, true).is_err();
    let realm_ok = ctx.realm_id == home_realm(snap, ctx) && snap.realms.contains_key(&ctx.realm_id);
    let scope = tenant_scope(snap, ctx);
    let tenant_ok = realm_ok
        && match &scope {
            Err(_) => false,
            Ok(_) if *p == Principal::Anonymous => true,
            Ok((org, owner, env)) => {
                let member = match (org, owner) {
                    (Some(o), _) => snap.membership(o, &principal).is_some_and(|m| m.status == "active"),
                    (None, Some(u)) => *u == principal,
                    _ => true,
                };
                let shared_env = env.as_ref().is_some_and(|e| {
                    let eff = snap.effective(&principal, &ctx.realm_id, org.as_deref());
                    acl_chain(snap, ctx, &ResourceRef::new("environment", e), &key, &eff) == Grant::Granted
                });
                member || shared_env
            }
        };
    let shared = effective(snap, ctx, None, None);
    let tenant = match &scope {
        Ok((org, owner, _)) => effective(snap, ctx, org.as_deref(), owner.as_deref()),
        Err(_) => shared.clone(),
    };
    let eff = if is_tenant_scoped(coll) { &tenant } else { &shared };
    let write = !matches!(action, Action::List | Action::View);
    let grant = if scopes_allow(ctx, &coll.name, write) {
        grant_name(snap.acl(&ctx.realm_id, "collection", &coll.name, &key, &principal, eff))
    } else {
        "denied"
    };
    let (allow, deny) = if matches!(action, Action::Create) {
        (Vec::new(), Vec::new())
    } else {
        snap.record_acl(&ctx.realm_id, &coll.name, &key, &principal, eff)
    };
    let mut colls = serde_json::Map::new();
    colls.insert(
        coll.name.clone(),
        json!({ name.clone(): { "grant": grant, "allow": allow, "deny": deny } }),
    );
    json!({
        "v": CLAIMS_VERSION,
        "principal": principal,
        "auth": auth,
        "realm": ctx.realm_id,
        "org": ctx.org_id,
        "tenant": ctx.tenant,
        "scopes": ctx.scopes,
        "attributes": ctx.attributes,
        "action": name,
        "suspended": suspended,
        "gate": { "shared": realm_ok, "tenant": tenant_ok },
        "eff": { "shared": eff_json(&shared), "tenant": eff_json(&tenant) },
        "colls": colls,
    })
}
