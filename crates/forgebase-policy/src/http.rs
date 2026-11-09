//! HTTP: the tenancy layer and the `/api/v1` policy and tenancy routes
//! (ADR-0008 §10).
//!
//! [`tenancy`] runs after authentication and before handlers. It reads
//! `X-ForgeBase-Realm`, `X-ForgeBase-Org` and `X-ForgeBase-Tenant`, checks
//! them (pipeline step 2: realm, active membership, tenant ownership) and
//! completes the request's `AccessContext` with the realm, the active
//! organization, the tenant, the caller's effective roles and, for users,
//! the fields of their auth record (ABAC attributes).

use axum::extract::{FromRequestParts, Path, RawQuery, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::{AccessContext, GetOptions, Principal};
use serde::Deserialize;
use serde_json::{Value as Json2, json};

use crate::engine::{Check, home_realm};
use crate::pipeline::{Denial, ResourceRef, Step};
use crate::snapshot::Effect;
use crate::store::{PolicyInput, RoleInput};
use crate::{PolicyError, PolicyService};

/// Realm header.
pub const REALM_HEADER: &str = "x-forgebase-realm";
/// Active organization header.
pub const ORG_HEADER: &str = "x-forgebase-org";
/// Tenant (environment id) header.
pub const TENANT_HEADER: &str = "x-forgebase-tenant";

/// An error response with the standard envelope.
#[derive(Debug)]
pub struct HttpError(PolicyError, Option<String>);

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let kind = self.0.kind();
        let status = match kind {
            ErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
            ErrorKind::Unauthenticated => StatusCode::UNAUTHORIZED,
            ErrorKind::Forbidden => StatusCode::FORBIDDEN,
            ErrorKind::NotFound => StatusCode::NOT_FOUND,
            ErrorKind::Conflict => StatusCode::CONFLICT,
            ErrorKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = if kind.is_client_visible() {
            self.0.to_string()
        } else {
            tracing::error!(error = %self.0, "internal error in policy API");
            "internal server error".to_owned()
        };
        let mut body = json!({"error": {"code": self.0.code(), "message": message, "request_id": self.1}});
        if let PolicyError::Denied(d) = &self.0 {
            body["error"]["details"] = json!({"step": d.step});
        }
        (status, Json(body)).into_response()
    }
}

type ApiResult<T> = Result<T, HttpError>;

/// The request's context (anonymous when no layer stored one).
#[derive(Debug, Clone)]
pub struct Caller(pub AccessContext);

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = HttpError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Caller(
            parts
                .extensions
                .get::<AccessContext>()
                .cloned()
                .unwrap_or_else(AccessContext::anonymous),
        ))
    }
}

trait Ctx<T> {
    fn ctx(self, c: &AccessContext) -> ApiResult<T>;
}

impl<T> Ctx<T> for Result<T, PolicyError> {
    fn ctx(self, c: &AccessContext) -> ApiResult<T> {
        self.map_err(|e| HttpError(e, c.request_id.clone()))
    }
}

fn header(h: &HeaderMap, name: &str) -> Result<Option<String>, PolicyError> {
    let Some(v) = h.get(name) else { return Ok(None) };
    let v = v
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|v| {
            !v.is_empty() && v.len() <= 64 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        .ok_or_else(|| PolicyError::invalid("invalid_header", format!("{name} must be 1-64 of [A-Za-z0-9_-]")))?;
    Ok(Some(v.to_owned()))
}

/// The tenancy layer (see the module docs).
pub async fn tenancy(State(svc): State<PolicyService>, mut req: Request, next: Next) -> Response {
    let mut ctx = req
        .extensions()
        .get::<AccessContext>()
        .cloned()
        .unwrap_or_else(AccessContext::anonymous);
    match resolve(&svc, &mut ctx, req.headers()).await {
        Ok(()) => {}
        Err(e) => return HttpError(e, ctx.request_id.clone()).into_response(),
    }
    req.extensions_mut().insert(ctx);
    next.run(req).await
}

async fn resolve(svc: &PolicyService, ctx: &mut AccessContext, h: &HeaderMap) -> Result<(), PolicyError> {
    let realm = header(h, REALM_HEADER)?;
    let org = header(h, ORG_HEADER)?;
    let tenant = header(h, TENANT_HEADER)?;
    let snap = svc.snapshot();
    let deny = |ctx: &AccessContext, d: Denial| {
        svc.audit()
            .record(crate::pipeline::audit_record(ctx, "tenancy:select", None, &d));
        PolicyError::Denied(d)
    };
    if let Some(r) = realm {
        ctx.realm_id = r;
    }
    let privileged = ctx.is_privileged();
    if !privileged && ctx.principal != Principal::System {
        crate::engine::step_authentication(&snap, ctx, true).map_err(|d| deny(ctx, d))?;
    }
    if !snap.realms.contains_key(&ctx.realm_id) || (!privileged && ctx.realm_id != home_realm(ctx)) {
        return Err(deny(ctx, Denial::at(Step::Tenant, "cross-realm access is denied")));
    }
    let principal = ctx.principal_key();
    let anonymous = ctx.principal == Principal::Anonymous;
    if let Some(o) = &org {
        let ok = snap.orgs.get(o).is_some_and(|x| x.realm_id == ctx.realm_id)
            && (privileged || snap.membership(o, &principal).is_some_and(|m| m.status == "active"));
        if !ok {
            return Err(deny(
                ctx,
                Denial::at(Step::Tenant, "not an active member of this organization"),
            ));
        }
        ctx.org_id = Some(o.clone());
    }
    if let Some(t) = &tenant {
        match snap.tenant(t) {
            Some((env, p)) => {
                let same_realm = p.realm_id == ctx.realm_id;
                let org_ok = match (&p.org_id, &ctx.org_id) {
                    (Some(po), Some(active)) => po == active,
                    (None, Some(_)) => false,
                    _ => true,
                };
                let member = privileged
                    || anonymous
                    || match (&p.org_id, &p.owner_user) {
                        (Some(o), _) => snap.membership(o, &principal).is_some_and(|m| m.status == "active"),
                        (None, Some(u)) => *u == principal,
                        _ => false,
                    }
                    || {
                        let eff = snap.effective(&principal, &ctx.realm_id, p.org_id.as_deref());
                        snap.acl(&ctx.realm_id, "environment", &env.id, "*", &principal, &eff)
                            != crate::perm::Grant::None
                            || snap
                                .acl
                                .get(&("environment".to_owned(), env.id.to_ascii_lowercase()))
                                .into_iter()
                                .chain(snap.acl.get(&("project".to_owned(), p.id.to_ascii_lowercase())))
                                .flatten()
                                .any(|e| {
                                    e.effect == Effect::Allow && snap.grantee_matches(&e.grantee, &principal, &eff)
                                })
                    };
                if !same_realm || !org_ok || !member {
                    return Err(deny(ctx, Denial::at(Step::Tenant, "this tenant is not yours")));
                }
                if ctx.org_id.is_none() {
                    ctx.org_id = p.org_id.clone();
                }
            }
            None if anonymous || privileged => {}
            None => return Err(deny(ctx, Denial::at(Step::Tenant, "this tenant is not yours"))),
        }
        ctx.tenant = Some(t.clone());
    }
    let eff = snap.effective(&principal, &ctx.realm_id, ctx.org_id.as_deref());
    ctx.roles = eff.roles.iter().cloned().collect();
    if let Principal::User { collection, id } = &ctx.principal
        && let Ok(data) = svc.data()
        && let Ok(mut rec) = data
            .get_record(&AccessContext::system(), collection, id, &GetOptions::default())
            .await
    {
        rec.remove("token_key");
        rec.remove("_tenant");
        ctx.attributes = rec;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

/// `{name, slug}` / `{id, name}`.
#[derive(Debug, Deserialize)]
pub struct NameBody {
    /// Id (realms) or slug (orgs, projects).
    #[serde(default, alias = "id")]
    pub slug: String,
    /// Display name.
    pub name: String,
}

/// A membership.
#[derive(Debug, Deserialize)]
pub struct MemberBody {
    /// Principal key.
    pub principal: String,
    /// Role keys.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Role-set keys.
    #[serde(default)]
    pub role_sets: Vec<String>,
    /// For the status route.
    #[serde(default)]
    pub active: Option<bool>,
}

/// A team.
#[derive(Debug, Deserialize)]
pub struct TeamBody {
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Parent team id.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Roles of every member.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Denied patterns.
    #[serde(default)]
    pub deny: Vec<String>,
}

/// A role set.
#[derive(Debug, Deserialize)]
pub struct RoleSetBody {
    /// Key.
    pub key: String,
    /// Display name.
    pub name: String,
    /// Role keys.
    pub roles: Vec<String>,
}

/// An override.
#[derive(Debug, Deserialize)]
pub struct OverrideBody {
    /// Organization (none: realm-wide).
    #[serde(default)]
    pub org_id: Option<String>,
    /// Principal key.
    pub principal: String,
    /// Pattern.
    pub permission: String,
    /// `allow` or `deny`.
    pub effect: Effect,
}

/// An ACL entry.
#[derive(Debug, Deserialize)]
pub struct AclBody {
    /// Resource type.
    pub resource_type: String,
    /// Resource id.
    pub resource_id: String,
    /// Principal key, `team:<id>` or `role:<key>`.
    pub grantee: String,
    /// Pattern.
    pub permission: String,
    /// `allow` or `deny`.
    pub effect: Effect,
}

/// ACL query.
#[derive(Debug, Deserialize)]
pub struct AclQuery {
    /// Resource type.
    pub resource_type: String,
    /// Resource id.
    pub resource_id: String,
}

/// A suspension.
#[derive(Debug, Deserialize)]
pub struct SuspendBody {
    /// Principal key.
    pub principal: String,
    /// Reason.
    #[serde(default)]
    pub reason: String,
    /// End (Unix seconds); none = until lifted.
    #[serde(default)]
    pub until: Option<i64>,
}

/// An invitation.
#[derive(Debug, Deserialize)]
pub struct InviteBody {
    /// Email.
    pub email: String,
    /// Role keys.
    #[serde(default)]
    pub roles: Vec<String>,
}

/// A token.
#[derive(Debug, Deserialize)]
pub struct TokenBody {
    /// The invitation token.
    pub token: String,
}

/// An environment.
#[derive(Debug, Deserialize)]
pub struct EnvBody {
    /// Name.
    pub name: String,
}

/// `?org=`.
#[derive(Debug, Deserialize)]
pub struct OrgQuery {
    /// Organization.
    #[serde(default)]
    pub org: Option<String>,
}

/// `?limit=`.
#[derive(Debug, Deserialize)]
pub struct LimitQuery {
    /// Most rows.
    #[serde(default)]
    pub limit: Option<u32>,
}

/// A permission check.
#[derive(Debug, Deserialize)]
pub struct CheckBody {
    /// Permission key.
    pub permission: String,
    /// Organization.
    #[serde(default)]
    pub org: Option<String>,
    /// Resource type.
    #[serde(default)]
    pub resource_type: Option<String>,
    /// Resource id.
    #[serde(default)]
    pub resource_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

macro_rules! doc {
    ($method:ident, $path:literal, $tag:literal, $desc:literal, $name:ident) => {
        #[utoipa::path($method, path = $path, tag = $tag, description = $desc,
                            responses((status = 200, description = "OK", body = Object)))]
        #[allow(dead_code)]
        #[doc = $desc]
        pub fn $name() {}
    };
}

/// OpenAPI stubs for the routes of this module (one per operation).
pub mod docs {
    doc!(get, "/api/v1/realms", "tenancy", "Realms (super-admins).", list_realms);
    doc!(
        post,
        "/api/v1/realms",
        "tenancy",
        "Create a realm `{id, name}` (super-admins).",
        create_realm
    );
    doc!(
        get,
        "/api/v1/orgs",
        "tenancy",
        "Organizations the caller belongs to.",
        list_orgs
    );
    doc!(
        post,
        "/api/v1/orgs",
        "tenancy",
        "Create an organization `{slug, name}`; the caller becomes owner.",
        create_org
    );
    doc!(
        get,
        "/api/v1/orgs/{org}",
        "tenancy",
        "One organization (`org:read`).",
        get_org
    );
    doc!(
        patch,
        "/api/v1/orgs/{org}",
        "tenancy",
        "Rename `{name}` (`org:update`).",
        update_org
    );
    doc!(
        delete,
        "/api/v1/orgs/{org}",
        "tenancy",
        "Delete (`org:delete`).",
        delete_org
    );
    doc!(
        get,
        "/api/v1/orgs/{org}/projects",
        "tenancy",
        "Projects (`projects:read`).",
        list_org_projects
    );
    doc!(
        post,
        "/api/v1/orgs/{org}/projects",
        "tenancy",
        "Create `{slug, name}` with development/staging/production (`projects:create`).",
        create_org_project
    );
    doc!(
        get,
        "/api/v1/projects",
        "tenancy",
        "The caller's personal projects.",
        list_personal_projects
    );
    doc!(
        post,
        "/api/v1/projects",
        "tenancy",
        "Create a personal project `{slug, name}`.",
        create_personal_project
    );
    doc!(
        get,
        "/api/v1/projects/{project}",
        "tenancy",
        "A project with its environments (`projects:read`).",
        get_project
    );
    doc!(
        delete,
        "/api/v1/projects/{project}",
        "tenancy",
        "Delete (`projects:delete`).",
        delete_project
    );
    doc!(
        post,
        "/api/v1/projects/{project}/environments",
        "tenancy",
        "Add an environment `{name}` (`projects:update`).",
        create_environment
    );
    doc!(
        get,
        "/api/v1/orgs/{org}/members",
        "tenancy",
        "Members (`users:read`).",
        list_members
    );
    doc!(
        put,
        "/api/v1/orgs/{org}/members",
        "tenancy",
        "Add or change `{principal, roles, role_sets}` (`users:update`).",
        put_member
    );
    doc!(
        post,
        "/api/v1/orgs/{org}/members/remove",
        "tenancy",
        "Remove `{principal}` (`users:delete`).",
        remove_member
    );
    doc!(
        post,
        "/api/v1/orgs/{org}/members/status",
        "tenancy",
        "Suspend or reactivate `{principal, active}` (`users:update`).",
        member_status
    );
    doc!(
        get,
        "/api/v1/orgs/{org}/teams",
        "tenancy",
        "Teams (`teams:read`).",
        list_teams
    );
    doc!(
        post,
        "/api/v1/orgs/{org}/teams",
        "tenancy",
        "Create `{key, name, parent_id, roles, deny}` (`teams:create`).",
        create_team
    );
    doc!(
        delete,
        "/api/v1/teams/{team}",
        "tenancy",
        "Delete with sub-teams (`teams:delete`).",
        delete_team
    );
    doc!(
        post,
        "/api/v1/teams/{team}/members",
        "tenancy",
        "Add `{principal}` (`teams:members:manage`).",
        add_team_member
    );
    doc!(
        post,
        "/api/v1/teams/{team}/members/remove",
        "tenancy",
        "Remove `{principal}` (`teams:members:manage`).",
        remove_team_member
    );
    doc!(
        get,
        "/api/v1/orgs/{org}/roles",
        "authorization",
        "Roles usable in the org (`roles:read`).",
        list_org_roles
    );
    doc!(
        put,
        "/api/v1/orgs/{org}/roles",
        "authorization",
        "Create or replace an org role `{key, name, allow, deny}` (`roles:manage`).",
        put_org_role
    );
    doc!(
        delete,
        "/api/v1/orgs/{org}/roles/{key}",
        "authorization",
        "Delete a custom org role (`roles:manage`).",
        delete_org_role
    );
    doc!(
        put,
        "/api/v1/orgs/{org}/role-sets",
        "authorization",
        "Create or replace a role set `{key, name, roles}`.",
        put_org_role_set
    );
    doc!(get, "/api/v1/roles", "authorization", "Realm roles.", list_realm_roles);
    doc!(
        put,
        "/api/v1/roles",
        "authorization",
        "Create or replace a realm role (super-admins).",
        put_realm_role
    );
    doc!(
        delete,
        "/api/v1/roles/{key}",
        "authorization",
        "Delete a custom realm role (super-admins).",
        delete_realm_role
    );
    doc!(
        put,
        "/api/v1/role-sets",
        "authorization",
        "Create or replace a realm role set (super-admins).",
        put_realm_role_set
    );
    doc!(
        put,
        "/api/v1/realm-bindings",
        "authorization",
        "Bind realm roles `{principal, roles, role_sets}` (super-admins).",
        put_realm_binding
    );
    doc!(
        post,
        "/api/v1/overrides",
        "authorization",
        "Explicit allow/deny `{org_id, principal, permission, effect}`.",
        add_override
    );
    doc!(
        delete,
        "/api/v1/overrides/{id}",
        "authorization",
        "Remove an override.",
        delete_override
    );
    doc!(
        get,
        "/api/v1/acl",
        "authorization",
        "ACL of `?resource_type=&resource_id=` (`acl:manage`).",
        list_acl
    );
    doc!(
        post,
        "/api/v1/acl",
        "authorization",
        "Add `{resource_type, resource_id, grantee, permission, effect}` (`acl:manage`).",
        put_acl
    );
    doc!(
        delete,
        "/api/v1/acl/{id}",
        "authorization",
        "Remove an ACL entry (`acl:manage`).",
        delete_acl
    );
    doc!(
        post,
        "/api/v1/suspensions",
        "authorization",
        "Suspend `{principal, reason, until}` everywhere (super-admins).",
        suspend
    );
    doc!(
        post,
        "/api/v1/suspensions/lift",
        "authorization",
        "Lift `{principal}` (super-admins).",
        unsuspend
    );
    doc!(
        get,
        "/api/v1/policies/{collection}",
        "authorization",
        "Collection rules (super-admins).",
        get_policy
    );
    doc!(
        put,
        "/api/v1/policies/{collection}",
        "authorization",
        "Set `{list_rule, view_rule, create_rule, update_rule, delete_rule, fields}`; `null` locks, `\"\"` opens (super-admins).",
        put_policy
    );
    doc!(
        delete,
        "/api/v1/policies/{collection}",
        "authorization",
        "Remove all rules (locked).",
        delete_policy
    );
    doc!(
        post,
        "/api/v1/orgs/{org}/invitations",
        "tenancy",
        "Invite `{email, roles}` (`users:invite`); returns the one-time token.",
        invite
    );
    doc!(
        post,
        "/api/v1/invitations/accept",
        "tenancy",
        "Accept `{token}` as the invited user.",
        accept
    );
    doc!(
        get,
        "/api/v1/authz/me",
        "authorization",
        "The caller's effective roles and permissions (`?org=`).",
        me
    );
    doc!(
        post,
        "/api/v1/authz/check",
        "authorization",
        "Dry-run the pipeline for `{permission, org, resource_type, resource_id}`.",
        check
    );
    doc!(
        get,
        "/api/v1/authz/audit",
        "authorization",
        "Recent denials with their pipeline step (super-admins).",
        audit
    );
}

/// The routes, with the service as state. Merge into the `/api` router.
pub fn router(svc: PolicyService) -> Router {
    Router::new()
        .route("/api/v1/realms", get(list_realms).post(create_realm))
        .route("/api/v1/orgs", get(list_orgs).post(create_org))
        .route("/api/v1/orgs/{org}", get(get_org).patch(update_org).delete(delete_org))
        .route(
            "/api/v1/orgs/{org}/projects",
            get(list_org_projects).post(create_org_project),
        )
        .route(
            "/api/v1/projects",
            get(list_personal_projects).post(create_personal_project),
        )
        .route("/api/v1/projects/{project}", get(get_project).delete(delete_project))
        .route("/api/v1/projects/{project}/environments", post(create_environment))
        .route("/api/v1/orgs/{org}/members", get(list_members).put(put_member))
        .route("/api/v1/orgs/{org}/members/remove", post(remove_member))
        .route("/api/v1/orgs/{org}/members/status", post(member_status))
        .route("/api/v1/orgs/{org}/teams", get(list_teams).post(create_team))
        .route("/api/v1/teams/{team}", axum::routing::delete(delete_team))
        .route("/api/v1/teams/{team}/members", post(add_team_member))
        .route("/api/v1/teams/{team}/members/remove", post(remove_team_member))
        .route("/api/v1/orgs/{org}/roles", get(list_org_roles).put(put_org_role))
        .route("/api/v1/orgs/{org}/roles/{key}", axum::routing::delete(delete_org_role))
        .route("/api/v1/orgs/{org}/role-sets", put(put_org_role_set))
        .route("/api/v1/roles", get(list_realm_roles).put(put_realm_role))
        .route("/api/v1/roles/{key}", axum::routing::delete(delete_realm_role))
        .route("/api/v1/role-sets", put(put_realm_role_set))
        .route("/api/v1/realm-bindings", put(put_realm_binding))
        .route("/api/v1/overrides", post(add_override))
        .route("/api/v1/overrides/{id}", axum::routing::delete(delete_override))
        .route("/api/v1/acl", get(list_acl).post(put_acl))
        .route("/api/v1/acl/{id}", axum::routing::delete(delete_acl))
        .route("/api/v1/suspensions", post(suspend))
        .route("/api/v1/suspensions/lift", post(unsuspend))
        .route(
            "/api/v1/policies/{collection}",
            get(get_policy).put(put_policy).delete(delete_policy),
        )
        .route("/api/v1/orgs/{org}/invitations", post(invite))
        .route("/api/v1/invitations/accept", post(accept))
        .route("/api/v1/authz/me", get(me))
        .route("/api/v1/authz/check", post(check))
        .route("/api/v1/authz/audit", get(audit))
        .with_state(svc)
}

type S = State<PolicyService>;

/// Query-string values (`axum`'s `query` feature is not enabled).
struct Query<T>(T);

impl<S2: Send + Sync, T: serde::de::DeserializeOwned> FromRequestParts<S2> for Query<T> {
    type Rejection = HttpError;

    async fn from_request_parts(parts: &mut Parts, state: &S2) -> Result<Self, Self::Rejection> {
        let RawQuery(raw) = RawQuery::from_request_parts(parts, state)
            .await
            .unwrap_or(RawQuery(None));
        let map: serde_json::Map<String, Json2> = url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes())
            .map(|(k, v)| {
                let v = match k.as_ref() {
                    "limit" => v.parse::<u64>().map_or(Json2::String(v.to_string()), Json2::from),
                    _ => Json2::String(v.into_owned()),
                };
                (k.into_owned(), v)
            })
            .collect();
        serde_json::from_value(Json2::Object(map))
            .map(Query)
            .map_err(|e| HttpError(PolicyError::invalid("invalid_query", e.to_string()), None))
    }
}

fn ok(v: Json2) -> ApiResult<Response> {
    Ok(Json(v).into_response())
}

fn created(v: Json2) -> ApiResult<Response> {
    Ok((StatusCode::CREATED, Json(v)).into_response())
}

fn done() -> ApiResult<Response> {
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn list_realms(State(s): S, Caller(c): Caller) -> ApiResult<Response> {
    ok(s.list_realms(&c).ctx(&c)?)
}
async fn create_realm(State(s): S, Caller(c): Caller, Json(b): Json<NameBody>) -> ApiResult<Response> {
    created(s.create_realm(&c, &b.slug, &b.name).await.ctx(&c)?)
}
async fn list_orgs(State(s): S, Caller(c): Caller) -> ApiResult<Response> {
    ok(s.list_orgs(&c).ctx(&c)?)
}
async fn create_org(State(s): S, Caller(c): Caller, Json(b): Json<NameBody>) -> ApiResult<Response> {
    created(s.create_org(&c, &b.slug, &b.name).await.ctx(&c)?)
}
async fn get_org(State(s): S, Caller(c): Caller, Path(o): Path<String>) -> ApiResult<Response> {
    ok(s.get_org(&c, &o).ctx(&c)?)
}
async fn update_org(
    State(s): S,
    Caller(c): Caller,
    Path(o): Path<String>,
    Json(b): Json<NameBody>,
) -> ApiResult<Response> {
    ok(s.update_org(&c, &o, &b.name).await.ctx(&c)?)
}
async fn delete_org(State(s): S, Caller(c): Caller, Path(o): Path<String>) -> ApiResult<Response> {
    s.delete_org(&c, &o).await.ctx(&c)?;
    done()
}
async fn list_org_projects(State(s): S, Caller(c): Caller, Path(o): Path<String>) -> ApiResult<Response> {
    ok(s.list_projects(&c, Some(&o)).ctx(&c)?)
}
async fn create_org_project(
    State(s): S,
    Caller(c): Caller,
    Path(o): Path<String>,
    Json(b): Json<NameBody>,
) -> ApiResult<Response> {
    created(s.create_project(&c, Some(&o), &b.slug, &b.name).await.ctx(&c)?)
}
async fn list_personal_projects(State(s): S, Caller(c): Caller) -> ApiResult<Response> {
    ok(s.list_projects(&c, None).ctx(&c)?)
}
async fn create_personal_project(State(s): S, Caller(c): Caller, Json(b): Json<NameBody>) -> ApiResult<Response> {
    created(s.create_project(&c, None, &b.slug, &b.name).await.ctx(&c)?)
}
async fn get_project(State(s): S, Caller(c): Caller, Path(p): Path<String>) -> ApiResult<Response> {
    ok(s.get_project(&c, &p).ctx(&c)?)
}
async fn delete_project(State(s): S, Caller(c): Caller, Path(p): Path<String>) -> ApiResult<Response> {
    s.delete_project(&c, &p).await.ctx(&c)?;
    done()
}
async fn create_environment(
    State(s): S,
    Caller(c): Caller,
    Path(p): Path<String>,
    Json(b): Json<EnvBody>,
) -> ApiResult<Response> {
    created(s.create_environment(&c, &p, &b.name).await.ctx(&c)?)
}
async fn list_members(State(s): S, Caller(c): Caller, Path(o): Path<String>) -> ApiResult<Response> {
    ok(s.list_members(&c, &o).ctx(&c)?)
}
async fn put_member(
    State(s): S,
    Caller(c): Caller,
    Path(o): Path<String>,
    Json(b): Json<MemberBody>,
) -> ApiResult<Response> {
    ok(s.set_member(&c, &o, &b.principal, &b.roles, &b.role_sets)
        .await
        .ctx(&c)?)
}
async fn remove_member(
    State(s): S,
    Caller(c): Caller,
    Path(o): Path<String>,
    Json(b): Json<MemberBody>,
) -> ApiResult<Response> {
    s.remove_member(&c, &o, &b.principal).await.ctx(&c)?;
    done()
}
async fn member_status(
    State(s): S,
    Caller(c): Caller,
    Path(o): Path<String>,
    Json(b): Json<MemberBody>,
) -> ApiResult<Response> {
    s.set_member_status(&c, &o, &b.principal, b.active.unwrap_or(true))
        .await
        .ctx(&c)?;
    done()
}
async fn list_teams(State(s): S, Caller(c): Caller, Path(o): Path<String>) -> ApiResult<Response> {
    ok(s.list_teams(&c, &o).ctx(&c)?)
}
async fn create_team(
    State(s): S,
    Caller(c): Caller,
    Path(o): Path<String>,
    Json(b): Json<TeamBody>,
) -> ApiResult<Response> {
    created(
        s.create_team(&c, &o, &b.key, &b.name, b.parent_id.as_deref(), &b.roles, &b.deny)
            .await
            .ctx(&c)?,
    )
}
async fn delete_team(State(s): S, Caller(c): Caller, Path(t): Path<String>) -> ApiResult<Response> {
    s.delete_team(&c, &t).await.ctx(&c)?;
    done()
}
async fn add_team_member(
    State(s): S,
    Caller(c): Caller,
    Path(t): Path<String>,
    Json(b): Json<MemberBody>,
) -> ApiResult<Response> {
    s.add_team_member(&c, &t, &b.principal).await.ctx(&c)?;
    done()
}
async fn remove_team_member(
    State(s): S,
    Caller(c): Caller,
    Path(t): Path<String>,
    Json(b): Json<MemberBody>,
) -> ApiResult<Response> {
    s.remove_team_member(&c, &t, &b.principal).await.ctx(&c)?;
    done()
}
async fn list_org_roles(State(s): S, Caller(c): Caller, Path(o): Path<String>) -> ApiResult<Response> {
    ok(s.list_roles(&c, Some(&o)).ctx(&c)?)
}
async fn put_org_role(
    State(s): S,
    Caller(c): Caller,
    Path(o): Path<String>,
    Json(b): Json<RoleInput>,
) -> ApiResult<Response> {
    ok(s.put_role(&c, Some(&o), &b).await.ctx(&c)?)
}
async fn delete_org_role(State(s): S, Caller(c): Caller, Path((o, k)): Path<(String, String)>) -> ApiResult<Response> {
    s.delete_role(&c, Some(&o), &k).await.ctx(&c)?;
    done()
}
async fn put_org_role_set(
    State(s): S,
    Caller(c): Caller,
    Path(o): Path<String>,
    Json(b): Json<RoleSetBody>,
) -> ApiResult<Response> {
    ok(s.put_role_set(&c, Some(&o), &b.key, &b.name, &b.roles).await.ctx(&c)?)
}
async fn list_realm_roles(State(s): S, Caller(c): Caller) -> ApiResult<Response> {
    ok(s.list_roles(&c, None).ctx(&c)?)
}
async fn put_realm_role(State(s): S, Caller(c): Caller, Json(b): Json<RoleInput>) -> ApiResult<Response> {
    ok(s.put_role(&c, None, &b).await.ctx(&c)?)
}
async fn delete_realm_role(State(s): S, Caller(c): Caller, Path(k): Path<String>) -> ApiResult<Response> {
    s.delete_role(&c, None, &k).await.ctx(&c)?;
    done()
}
async fn put_realm_role_set(State(s): S, Caller(c): Caller, Json(b): Json<RoleSetBody>) -> ApiResult<Response> {
    ok(s.put_role_set(&c, None, &b.key, &b.name, &b.roles).await.ctx(&c)?)
}
async fn put_realm_binding(State(s): S, Caller(c): Caller, Json(b): Json<MemberBody>) -> ApiResult<Response> {
    ok(s.put_realm_binding(&c, &b.principal, &b.roles, &b.role_sets)
        .await
        .ctx(&c)?)
}
async fn add_override(State(s): S, Caller(c): Caller, Json(b): Json<OverrideBody>) -> ApiResult<Response> {
    created(
        s.add_override(&c, b.org_id.as_deref(), &b.principal, &b.permission, b.effect)
            .await
            .ctx(&c)?,
    )
}
async fn delete_override(State(s): S, Caller(c): Caller, Path(id): Path<String>) -> ApiResult<Response> {
    s.delete_override(&c, &id).await.ctx(&c)?;
    done()
}
async fn list_acl(State(s): S, Caller(c): Caller, Query(q): Query<AclQuery>) -> ApiResult<Response> {
    ok(s.list_acl(&c, &q.resource_type, &q.resource_id).ctx(&c)?)
}
async fn put_acl(State(s): S, Caller(c): Caller, Json(b): Json<AclBody>) -> ApiResult<Response> {
    created(
        s.put_acl(
            &c,
            &b.resource_type,
            &b.resource_id,
            &b.grantee,
            &b.permission,
            b.effect,
        )
        .await
        .ctx(&c)?,
    )
}
async fn delete_acl(State(s): S, Caller(c): Caller, Path(id): Path<String>) -> ApiResult<Response> {
    s.delete_acl(&c, &id).await.ctx(&c)?;
    done()
}
async fn suspend(State(s): S, Caller(c): Caller, Json(b): Json<SuspendBody>) -> ApiResult<Response> {
    s.suspend(&c, &b.principal, &b.reason, b.until).await.ctx(&c)?;
    done()
}
async fn unsuspend(State(s): S, Caller(c): Caller, Json(b): Json<SuspendBody>) -> ApiResult<Response> {
    s.unsuspend(&c, &b.principal).await.ctx(&c)?;
    done()
}
async fn get_policy(State(s): S, Caller(c): Caller, Path(coll): Path<String>) -> ApiResult<Response> {
    ok(s.get_policy(&c, &coll).await.ctx(&c)?)
}
async fn put_policy(
    State(s): S,
    Caller(c): Caller,
    Path(coll): Path<String>,
    Json(b): Json<PolicyInput>,
) -> ApiResult<Response> {
    ok(s.put_policy(&c, &coll, &b).await.ctx(&c)?)
}
async fn delete_policy(State(s): S, Caller(c): Caller, Path(coll): Path<String>) -> ApiResult<Response> {
    s.delete_policy(&c, &coll).await.ctx(&c)?;
    done()
}
async fn invite(
    State(s): S,
    Caller(c): Caller,
    Path(o): Path<String>,
    Json(b): Json<InviteBody>,
) -> ApiResult<Response> {
    let (mut v, token) = s.invite(&c, &o, &b.email, &b.roles).await.ctx(&c)?;
    s.mail_invitation(&b.email, &o, &token).await;
    v["token"] = Json2::String(token);
    created(v)
}
async fn accept(State(s): S, Caller(c): Caller, Json(b): Json<TokenBody>) -> ApiResult<Response> {
    ok(s.accept_invitation(&c, &b.token).await.ctx(&c)?)
}
async fn me(State(s): S, Caller(c): Caller, Query(q): Query<OrgQuery>) -> ApiResult<Response> {
    let org = q.org.or_else(|| c.org_id.clone());
    ok(s.my_permissions(&c, org.as_deref()).ctx(&c)?)
}
async fn check(State(s): S, Caller(c): Caller, Json(b): Json<CheckBody>) -> ApiResult<Response> {
    let resource = match (b.resource_type, b.resource_id) {
        (Some(t), Some(id)) => Some(ResourceRef::new(t, id)),
        _ => None,
    };
    let owner = resource.as_ref().and_then(|r| {
        let snap = s.snapshot();
        match r.kind.as_str() {
            "project" => snap.projects.get(&r.id).and_then(|p| p.owner_user.clone()),
            _ => None,
        }
    });
    let chk = Check {
        permission: b.permission,
        org: b.org.or_else(|| c.org_id.clone()),
        resource,
        owner,
        allow_anonymous: false,
    };
    ok(match s.dry_run(&c, &chk) {
        Ok(()) => json!({"allowed": true}),
        Err(d) => json!({"allowed": false, "step": d.step, "reason": d.reason}),
    })
}
async fn audit(State(s): S, Caller(c): Caller, Query(q): Query<LimitQuery>) -> ApiResult<Response> {
    ok(s.audit_log(&c, q.limit.unwrap_or(100)).await.ctx(&c)?)
}
