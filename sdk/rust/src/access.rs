//! Authorization (`/api/v1`): permission checks, resource ACLs, per-principal overrides, collection
//! rules and engine resource rules. `client.access()`.
//!
//! The OpenAPI document types these bodies as plain objects, so the wire types are written by hand
//! after the server's JSON (snake_case).

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::http::{Req, Transport, seg};

/// Allow or deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effect {
    /// Grants the permission.
    Allow,
    /// Denies it (a deny wins).
    Deny,
}

/// The caller's effective access ([`AccessApi::me`]).
#[derive(Debug, Clone, Deserialize)]
pub struct AccessSummary {
    /// `user:<collection>/<id>`, `service:<id>` or `anonymous`.
    pub principal: String,
    /// Realm.
    #[serde(default)]
    pub realm_id: String,
    /// Organization, when scoped to one.
    #[serde(default)]
    pub org_id: Option<String>,
    /// True for a super-admin.
    #[serde(default)]
    pub super_admin: bool,
    /// True when the account is suspended.
    #[serde(default)]
    pub suspended: bool,
    /// Role keys.
    #[serde(default)]
    pub roles: Vec<String>,
    /// Permission patterns granted.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Permission patterns denied.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Team ids the caller belongs to in the organization.
    #[serde(default)]
    pub teams: Vec<String>,
}

/// A permission check ([`AccessApi::check`]).
#[derive(Debug, Clone, Default, Serialize)]
pub struct AccessCheck {
    /// Permission key, e.g. `records:create`.
    pub permission: String,
    /// Organization (default: the caller's `X-Forgebase-Org`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,
    /// Resource type.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    /// Resource id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    /// Check as this principal (`user:<collection>/<id>`; super-admins only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

impl AccessCheck {
    /// Checks `permission` for the caller.
    #[must_use]
    pub fn new(permission: impl Into<String>) -> Self {
        Self {
            permission: permission.into(),
            ..Self::default()
        }
    }

    /// Scopes the check to an organization.
    #[must_use]
    pub fn org(mut self, org: impl Into<String>) -> Self {
        self.org = Some(org.into());
        self
    }

    /// Scopes the check to one resource.
    #[must_use]
    pub fn resource(mut self, kind: impl Into<String>, id: impl Into<String>) -> Self {
        self.resource_type = Some(kind.into());
        self.resource_id = Some(id.into());
        self
    }

    /// Checks as another principal (super-admins only).
    #[must_use]
    pub fn principal(mut self, principal: impl Into<String>) -> Self {
        self.principal = Some(principal.into());
        self
    }
}

/// The answer to an [`AccessCheck`].
#[derive(Debug, Clone, Deserialize)]
pub struct AccessDecision {
    /// Whether the action is allowed.
    pub allowed: bool,
    /// The pipeline step that denied (`authentication`, `tenant`, `permission`, ...).
    #[serde(default)]
    pub step: Option<String>,
    /// Why.
    #[serde(default)]
    pub reason: Option<String>,
}

/// One denial in the authorization audit log ([`AccessApi::audit`]).
#[derive(Debug, Clone, Deserialize)]
pub struct AccessAuditEntry {
    /// Unix seconds.
    #[serde(default)]
    pub at: i64,
    /// Realm.
    #[serde(default)]
    pub realm_id: Option<String>,
    /// Principal.
    #[serde(default)]
    pub principal: Option<String>,
    /// Tenant.
    #[serde(default)]
    pub tenant: Option<String>,
    /// Organization.
    #[serde(default)]
    pub org_id: Option<String>,
    /// Permission asked.
    #[serde(default)]
    pub permission: Option<String>,
    /// Resource.
    #[serde(default)]
    pub resource: Option<String>,
    /// Pipeline step that denied.
    #[serde(default)]
    pub step: Option<String>,
    /// Reason.
    #[serde(default)]
    pub reason: Option<String>,
    /// Request id.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Impersonating admin, when any.
    #[serde(default)]
    pub impersonator: Option<String>,
}

/// One ACL entry.
#[derive(Debug, Clone, Deserialize)]
pub struct AclEntry {
    /// Entry id.
    pub id: String,
    /// Realm.
    #[serde(default)]
    pub realm_id: Option<String>,
    /// `project`, `environment`, `collection`, `record`, `file`, ...
    pub resource_type: String,
    /// `<collection>/<id>` for records.
    pub resource_id: String,
    /// A principal key, `team:<id>` or `role:<key>`.
    pub grantee: String,
    /// Permission pattern.
    pub permission: String,
    /// Allow or deny.
    pub effect: Effect,
}

/// A new ACL entry.
#[derive(Debug, Clone, Serialize)]
pub struct NewAcl {
    /// Resource type.
    pub resource_type: String,
    /// Resource id.
    pub resource_id: String,
    /// A principal key, `team:<id>` or `role:<key>`.
    pub grantee: String,
    /// Permission pattern.
    pub permission: String,
    /// Allow or deny.
    pub effect: Effect,
}

impl NewAcl {
    /// `effect` of `permission` on the resource for `grantee`.
    #[must_use]
    pub fn new(
        resource_type: impl Into<String>,
        resource_id: impl Into<String>,
        grantee: impl Into<String>,
        permission: impl Into<String>,
        effect: Effect,
    ) -> Self {
        Self {
            resource_type: resource_type.into(),
            resource_id: resource_id.into(),
            grantee: grantee.into(),
            permission: permission.into(),
            effect,
        }
    }
}

/// A per-principal permission override.
#[derive(Debug, Clone, Deserialize)]
pub struct PermissionOverride {
    /// Override id.
    pub id: String,
    /// Realm.
    #[serde(default)]
    pub realm_id: Option<String>,
    /// `None`: realm-wide.
    #[serde(default)]
    pub org_id: Option<String>,
    /// Principal.
    pub principal: String,
    /// Permission pattern.
    pub permission: String,
    /// Allow or deny.
    pub effect: Effect,
}

/// A new override.
#[derive(Debug, Clone, Serialize)]
pub struct NewOverride {
    /// Organization; omit for a realm-wide override (super-admins).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    /// Principal.
    pub principal: String,
    /// Permission pattern.
    pub permission: String,
    /// Allow or deny.
    pub effect: Effect,
}

impl NewOverride {
    /// An override of `permission` for `principal`.
    #[must_use]
    pub fn new(principal: impl Into<String>, permission: impl Into<String>, effect: Effect) -> Self {
        Self {
            org_id: None,
            principal: principal.into(),
            permission: permission.into(),
            effect,
        }
    }

    /// Scopes it to an organization.
    #[must_use]
    pub fn org(mut self, org: impl Into<String>) -> Self {
        self.org_id = Some(org.into());
        self
    }
}

/// Read and write rule of one field. `None` locks the operation, `Some("")` opens it to everyone.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FieldRules {
    /// Read rule.
    #[serde(default)]
    pub read: Option<String>,
    /// Write rule.
    #[serde(default)]
    pub write: Option<String>,
}

/// Rules of a collection. A rule is an expression; `None` locks the operation and `Some("")`
/// opens it to everyone (`list_rule: Some("")` is a public list).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CollectionPolicy {
    /// Collection name (ignored on write).
    #[serde(default, skip_serializing)]
    pub collection: String,
    /// List rule.
    #[serde(default)]
    pub list_rule: Option<String>,
    /// View rule.
    #[serde(default)]
    pub view_rule: Option<String>,
    /// Create rule.
    #[serde(default)]
    pub create_rule: Option<String>,
    /// Update rule.
    #[serde(default)]
    pub update_rule: Option<String>,
    /// Delete rule.
    #[serde(default)]
    pub delete_rule: Option<String>,
    /// Rules by field name.
    #[serde(default)]
    pub fields: BTreeMap<String, FieldRules>,
}

/// Rules of one engine resource, by action.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceRuleSet {
    /// Resource kind (`kv`, `bucket`, `queue`, ...).
    #[serde(default)]
    pub kind: String,
    /// Resource name; `*` holds the rules of every resource of the kind without rules of its own.
    #[serde(default)]
    pub name: String,
    /// Action to rule (`None` locks, `Some("")` opens to everyone).
    #[serde(default)]
    pub rules: BTreeMap<String, Option<String>>,
}

/// A field a resource rule may read.
#[derive(Debug, Clone, Deserialize)]
pub struct RuleField {
    /// Field name.
    pub name: String,
    /// Field type.
    #[serde(rename = "type", default)]
    pub ty: String,
}

/// Rules of one resource inside a [`ResourceRuleCatalog`].
#[derive(Debug, Clone, Deserialize)]
pub struct ResourceRuleItem {
    /// Resource name.
    pub name: String,
    /// Rules by action.
    #[serde(default)]
    pub rules: BTreeMap<String, Option<String>>,
}

/// What rules can be written for a kind, and which resources have some.
#[derive(Debug, Clone, Deserialize)]
pub struct ResourceRuleCatalog {
    /// Kind.
    #[serde(default)]
    pub kind: String,
    /// Item fields a rule may read.
    #[serde(default)]
    pub fields: Vec<RuleField>,
    /// Operations.
    #[serde(default)]
    pub operations: Vec<String>,
    /// Actions [`AccessResourceRulesApi::check`] accepts.
    #[serde(default)]
    pub actions: Vec<String>,
    /// Resources with rules.
    #[serde(default)]
    pub items: Vec<ResourceRuleItem>,
}

/// The operation a resource-rule dry run asks about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleOp {
    /// List.
    List,
    /// View.
    View,
    /// Create.
    Create,
    /// Update.
    Update,
    /// Delete.
    Delete,
}

/// A resource-rule dry run.
#[derive(Debug, Clone, Serialize)]
pub struct ResourceCheck {
    /// Action to check.
    pub action: String,
    /// Operation.
    pub op: RuleOp,
    /// Check as this principal (super-admins only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<String>,
}

impl ResourceCheck {
    /// Checks `action` for operation `op`.
    #[must_use]
    pub fn new(action: impl Into<String>, op: RuleOp) -> Self {
        Self {
            action: action.into(),
            op,
            principal: None,
        }
    }
}

/// What a resource rule decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleVerdict {
    /// Allowed.
    Allow,
    /// Allowed for the rows the rule matches.
    Filter,
    /// Denied.
    Deny,
}

/// Answer of a resource-rule dry run.
#[derive(Debug, Clone, Deserialize)]
pub struct ResourceDecision {
    /// Verdict.
    pub decision: RuleVerdict,
    /// Step that decided.
    #[serde(default)]
    pub step: Option<String>,
    /// Why.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Permission checks, ACLs, overrides and rules: `client.access()`.
#[derive(Clone)]
pub struct AccessApi {
    pub(crate) t: Arc<Transport>,
}

impl AccessApi {
    /// Resource ACL entries.
    #[must_use]
    pub fn acl(&self) -> AccessAclApi {
        AccessAclApi { t: self.t.clone() }
    }

    /// Per-principal overrides.
    #[must_use]
    pub fn overrides(&self) -> AccessOverridesApi {
        AccessOverridesApi { t: self.t.clone() }
    }

    /// Collection rules (super-admins).
    #[must_use]
    pub fn policies(&self) -> AccessPoliciesApi {
        AccessPoliciesApi { t: self.t.clone() }
    }

    /// Engine resource rules (super-admins).
    #[must_use]
    pub fn resource_rules(&self) -> AccessResourceRulesApi {
        AccessResourceRulesApi { t: self.t.clone() }
    }

    /// The caller's roles and effective permissions in an organization (default: the caller's current one).
    pub async fn me(&self, org: Option<&str>) -> Result<AccessSummary> {
        self.t.json(Req::get("/api/v1/authz/me").q_opt("org", org)).await
    }

    /// Dry-runs a permission check. On a denial, `step` and `reason` say why.
    pub async fn check(&self, check: &AccessCheck) -> Result<AccessDecision> {
        self.t
            .json(Req::post("/api/v1/authz/check").json(check)?.idempotent())
            .await
    }

    /// [`check`](Self::check), reduced to the boolean.
    pub async fn can(&self, permission: &str) -> Result<bool> {
        Ok(self.check(&AccessCheck::new(permission)).await?.allowed)
    }

    /// Recent authorization denials, newest first (super-admins). The server clamps `limit` to 1..=1000 (default 100).
    pub async fn audit(&self, limit: Option<u32>) -> Result<Vec<AccessAuditEntry>> {
        self.t.json(Req::get("/api/v1/authz/audit").q_opt("limit", limit)).await
    }
}

/// Resource ACL entries: `client.access().acl()`.
#[derive(Clone)]
pub struct AccessAclApi {
    t: Arc<Transport>,
}

impl AccessAclApi {
    /// Entries of one resource.
    pub async fn list(&self, resource_type: &str, resource_id: &str) -> Result<Vec<AclEntry>> {
        self.t
            .json(
                Req::get("/api/v1/acl")
                    .q("resource_type", resource_type)
                    .q("resource_id", resource_id),
            )
            .await
    }

    /// Grants or denies a permission on one resource to a principal, team (`team:<id>`) or role (`role:<key>`).
    pub async fn put(&self, entry: &NewAcl) -> Result<AclEntry> {
        self.t.json(Req::post("/api/v1/acl").json(entry)?).await
    }

    /// Removes an entry.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("/api/v1/acl/{}", seg(id)))).await
    }
}

/// Per-principal allow/deny overrides: `client.access().overrides()`.
#[derive(Clone)]
pub struct AccessOverridesApi {
    t: Arc<Transport>,
}

impl AccessOverridesApi {
    /// Overrides of an organization, or the realm's when `org` is `None` (super-admins).
    pub async fn list(&self, org: Option<&str>) -> Result<Vec<PermissionOverride>> {
        self.t.json(Req::get("/api/v1/overrides").q_opt("org", org)).await
    }

    /// Adds an override.
    pub async fn add(&self, o: &NewOverride) -> Result<PermissionOverride> {
        self.t.json(Req::post("/api/v1/overrides").json(o)?).await
    }

    /// Removes an override.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("/api/v1/overrides/{}", seg(id)))).await
    }
}

/// Collection rules: `client.access().policies()`.
#[derive(Clone)]
pub struct AccessPoliciesApi {
    t: Arc<Transport>,
}

impl AccessPoliciesApi {
    /// Rules of a collection.
    pub async fn get(&self, collection: &str) -> Result<CollectionPolicy> {
        self.t
            .json(Req::get(format!("/api/v1/policies/{}", seg(collection))))
            .await
    }

    /// Replaces the rules of a collection. An unset rule is locked (`null`).
    pub async fn put(&self, collection: &str, policy: &CollectionPolicy) -> Result<CollectionPolicy> {
        self.t
            .json(Req::put(format!("/api/v1/policies/{}", seg(collection))).json(policy)?)
            .await
    }

    /// Removes the rules: the collection is locked again.
    pub async fn delete(&self, collection: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/policies/{}", seg(collection))))
            .await
    }
}

/// Rules of engine resources (KV namespaces, buckets, queues, ...): `client.access().resource_rules()`.
#[derive(Clone)]
pub struct AccessResourceRulesApi {
    t: Arc<Transport>,
}

impl AccessResourceRulesApi {
    /// What the kind supports, and every resource of it with rules.
    pub async fn list(&self, kind: &str) -> Result<ResourceRuleCatalog> {
        self.t
            .json(Req::get(format!("/api/v1/policies/resources/{}", seg(kind))))
            .await
    }

    /// Rules of one resource; name `*` is the default of the kind.
    pub async fn get(&self, kind: &str, name: &str) -> Result<ResourceRuleSet> {
        self.t
            .json(Req::get(format!(
                "/api/v1/policies/resources/{}/{}",
                seg(kind),
                seg(name)
            )))
            .await
    }

    /// Replaces the rules of one resource: action to rule (`None` locks, `Some("")` opens to everyone).
    pub async fn put(
        &self,
        kind: &str,
        name: &str,
        rules: &BTreeMap<String, Option<String>>,
    ) -> Result<ResourceRuleSet> {
        let body = serde_json::json!({ "rules": rules });
        self.t
            .json(Req::put(format!("/api/v1/policies/resources/{}/{}", seg(kind), seg(name))).json_raw(body))
            .await
    }

    /// Removes every rule of the resource.
    pub async fn delete(&self, kind: &str, name: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!(
                "/api/v1/policies/resources/{}/{}",
                seg(kind),
                seg(name)
            )))
            .await
    }

    /// Dry run: would this action be allowed on the resource?
    pub async fn check(&self, kind: &str, name: &str, input: &ResourceCheck) -> Result<ResourceDecision> {
        self.t
            .json(
                Req::post(format!("/api/v1/policies/resources/{}/{}/check", seg(kind), seg(name)))
                    .json(input)?
                    .idempotent(),
            )
            .await
    }
}

/// Convenience: the JSON of a rule map built from pairs.
#[must_use]
pub fn rules<const N: usize>(pairs: [(&str, Option<&str>); N]) -> BTreeMap<String, Option<String>> {
    pairs
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.map(str::to_owned)))
        .collect()
}
