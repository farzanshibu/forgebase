//! Access context and the authorisation hook (ADR-0003 §7).
//!
//! Every service call takes an [`AccessContext`]. Before touching data the
//! service asks the [`Authorizer`] for a [`Decision`]; a `Decision::Filter`
//! predicate is compiled into the same SQL statement as the caller's filter, so
//! rows the caller may not see are never read. FB-21 plugs its policy engine
//! in here without changing any service signature.
//!
//! The default, [`DefaultAuthorizer`], fails closed: only the system and
//! administrators are allowed anything. The policy engine (FB-21,
//! `forgebase-policy`, ADR-0008) implements the same trait.
//!
//! Tenancy (ADR-0008 §4): a collection created with the option
//! `tenant_scoped: true` gets a `_tenant` column. The data core — not the
//! authorizer — adds `_tenant = <AccessContext::tenant>` to every statement on
//! such a collection, stamps it on every insert and keeps relations inside one
//! tenant, so isolation holds whatever the authorizer answers.

use std::collections::{BTreeSet, HashMap};
use std::fmt;

use forgebase_query::Expr;
use forgebase_schema::Collection;
use serde_json::Value as Json;

/// Who is calling.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Principal {
    /// ForgeBase itself (migrations, internal jobs). Bypasses authorisation and
    /// may change system collections.
    System,
    /// An authenticated administrator.
    Admin {
        /// Administrator id.
        id: String,
    },
    /// An authenticated user record of an auth collection.
    User {
        /// Auth collection name.
        collection: String,
        /// Record id.
        id: String,
    },
    /// No credentials.
    Anonymous,
    /// A service account (machine identity, FB-20). With `admin` it is
    /// privileged like an administrator; otherwise policies decide, like for a
    /// user.
    Service {
        /// Service-account id.
        id: String,
        /// Has the `admin` role.
        admin: bool,
    },
}

impl Principal {
    /// `anonymous`, `user`, `admin`, `service` or `system`: the value of
    /// `@request.auth.type` in policy rules.
    pub fn kind(&self) -> &'static str {
        match self {
            Principal::System => "system",
            Principal::Admin { .. } => "admin",
            Principal::User { .. } => "user",
            Principal::Anonymous => "anonymous",
            Principal::Service { .. } => "service",
        }
    }

    /// Id of the caller (`None` for anonymous and the system).
    pub fn id(&self) -> Option<&str> {
        match self {
            Principal::Admin { id } | Principal::User { id, .. } | Principal::Service { id, .. } => Some(id),
            Principal::System | Principal::Anonymous => None,
        }
    }
}

/// Identity and request metadata passed to every service call.
///
/// Build it with the constructors and `with_*` methods: fields may be added
/// (FB-21 added `scopes`, `roles` and `attributes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessContext {
    /// Caller.
    pub principal: Principal,
    /// Realm (top isolation level, ADR-0008 §3) the request acts in.
    /// [`DEFAULT_REALM`] unless the tenancy layer selected another one;
    /// crossing realms is denied to everyone but platform super-admins.
    pub realm_id: String,
    /// Active organization (B2B subject) of the request; `None` = the
    /// caller's personal scope (B2C subject).
    pub org_id: Option<String>,
    /// Tenant (project environment id) the request is scoped to. Every
    /// statement on a tenant-scoped collection is restricted to it.
    pub tenant: Option<String>,
    /// Request id, for audit and logs.
    pub request_id: Option<String>,
    /// Scopes of an API key or personal access token (`collections:read:posts`,
    /// ...); `None` = not restricted by scopes (sessions, the system).
    pub scopes: Option<Vec<String>>,
    /// RBAC roles of the caller for this request (global role bindings plus
    /// memberships of the selected tenant), resolved before the call.
    pub roles: Vec<String>,
    /// ABAC attributes of the caller (`@request.auth.<name>` in rules): the
    /// fields of a user's auth record, resolved before the call.
    pub attributes: serde_json::Map<String, Json>,
}

impl AccessContext {
    /// The system principal.
    pub fn system() -> Self {
        Self::of(Principal::System)
    }

    /// An administrator.
    pub fn admin(id: impl Into<String>) -> Self {
        Self::of(Principal::Admin { id: id.into() })
    }

    /// A user record.
    pub fn user(collection: impl Into<String>, id: impl Into<String>) -> Self {
        Self::of(Principal::User {
            collection: collection.into(),
            id: id.into(),
        })
    }

    /// No credentials.
    pub fn anonymous() -> Self {
        Self::of(Principal::Anonymous)
    }

    /// A service account.
    pub fn service(id: impl Into<String>, admin: bool) -> Self {
        Self::of(Principal::Service { id: id.into(), admin })
    }

    fn of(principal: Principal) -> Self {
        Self {
            principal,
            realm_id: DEFAULT_REALM.to_owned(),
            org_id: None,
            tenant: None,
            request_id: None,
            scopes: None,
            roles: Vec::new(),
            attributes: serde_json::Map::new(),
        }
    }

    /// Same context with a request id.
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }

    /// Same context in another realm.
    pub fn with_realm(mut self, realm: impl Into<String>) -> Self {
        self.realm_id = realm.into();
        self
    }

    /// Same context with an active organization.
    pub fn with_org(mut self, org: impl Into<String>) -> Self {
        self.org_id = Some(org.into());
        self
    }

    /// The principal as a string (`user:<collection>/<id>`, `service:<id>`,
    /// `admin:<id>`, `anonymous`, `system`), as stored in tenancy tables.
    pub fn principal_key(&self) -> String {
        match &self.principal {
            Principal::Service { id, .. } => format!("service:{id}"),
            _ => self.actor(),
        }
    }

    /// Who a check is billed and metered against: the active organization,
    /// else the caller (B2C + B2B, ADR-0008 §2).
    pub fn subject(&self) -> SubjectRef {
        match &self.org_id {
            Some(org) => SubjectRef::Org(org.clone()),
            None => SubjectRef::Principal(self.principal_key()),
        }
    }

    /// Same context scoped to a tenant.
    pub fn with_tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = Some(tenant.into());
        self
    }

    /// Same context restricted to scopes.
    pub fn with_scopes(mut self, scopes: Vec<String>) -> Self {
        self.scopes = Some(scopes);
        self
    }

    /// Same context with RBAC roles.
    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles = roles;
        self
    }

    /// Same context with ABAC attributes.
    pub fn with_attributes(mut self, attributes: serde_json::Map<String, Json>) -> Self {
        self.attributes = attributes;
        self
    }

    /// System, administrator or admin service account: may see hidden fields
    /// and use admin tools.
    pub fn is_privileged(&self) -> bool {
        matches!(
            self.principal,
            Principal::System | Principal::Admin { .. } | Principal::Service { admin: true, .. }
        )
    }

    /// The system principal.
    pub fn is_system(&self) -> bool {
        self.principal == Principal::System
    }

    /// Short actor string recorded in history tables.
    pub fn actor(&self) -> String {
        match &self.principal {
            Principal::System => "system".into(),
            Principal::Admin { id } => format!("admin:{id}"),
            Principal::User { collection, id } => format!("user:{collection}/{id}"),
            Principal::Anonymous => "anonymous".into(),
            Principal::Service { id, .. } => format!("service:{id}"),
        }
    }
}

/// The realm everything belongs to unless placed elsewhere.
pub const DEFAULT_REALM: &str = "default";

/// The subject of a check: an organization or the caller's personal scope.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SubjectRef {
    /// An organization id.
    Org(String),
    /// A principal key (`user:users/abc`, `service:xyz`).
    Principal(String),
}

/// What the caller wants to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// Read collection definitions, indexes, plans and history.
    ReadSchema,
    /// Create, alter or drop collections.
    ManageSchema,
    /// Create, drop or rebuild indexes; change the slow-query threshold.
    ManageIndexes,
    /// List records.
    List,
    /// Read one record.
    View,
    /// Create a record.
    Create,
    /// Update a record.
    Update,
    /// Delete a record.
    Delete,
    /// Run read-only SQL.
    ReadSql,
    /// Run SQL that writes or changes the schema.
    WriteSql,
}

/// A row-level predicate in the filter language, with its parameters.
#[derive(Debug, Clone, PartialEq)]
pub struct Predicate {
    /// Expression evaluated against the collection's rows.
    pub filter: Expr,
    /// Values for `{:name}` parameters in `filter`.
    pub params: HashMap<String, Json>,
}

impl Predicate {
    /// Parses a predicate from filter syntax.
    pub fn parse(filter: &str, params: HashMap<String, Json>) -> Result<Self, forgebase_query::QueryError> {
        Ok(Self {
            filter: forgebase_query::parse_filter(filter)?.unwrap_or(Expr::Const(true)),
            params,
        })
    }
}

/// Outcome of an authorisation check.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Decision {
    /// Allowed without restriction.
    Allow,
    /// Allowed only for rows matching the predicate. For `Create` and
    /// `Update` the predicate is also checked on the written row before
    /// commit.
    Filter(Predicate),
    /// Not allowed.
    Deny,
}

/// Field-level access of one caller to one collection (lower-case field
/// names). Fields named here are, for that caller, treated like `hidden`
/// fields (`unreadable`: never returned, expanded, filtered, sorted, grouped,
/// aggregated or searched) or read-only (`read_only`: a create may not set
/// them, an update may not change them).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FieldAccess {
    /// Fields the caller may not read.
    pub unreadable: BTreeSet<String>,
    /// Fields the caller may not write.
    pub read_only: BTreeSet<String>,
}

impl FieldAccess {
    /// Whether nothing is restricted.
    pub fn is_empty(&self) -> bool {
        self.unreadable.is_empty() && self.read_only.is_empty()
    }
}

/// Decides what a caller may do. Called once per service call.
///
/// Methods added after FB-10 have default implementations, so existing
/// implementations keep compiling.
pub trait Authorizer: Send + Sync + fmt::Debug {
    /// The decision for `action` on `collection` (`None` for actions that are
    /// not about one collection, e.g. SQL and index-wide operations).
    fn authorize(&self, ctx: &AccessContext, action: Action, collection: Option<&Collection>) -> Decision;

    /// Field-level rules for `collection` (FB-21). Only asked for
    /// non-privileged callers. Default: no restriction beyond `hidden`.
    fn field_access(&self, ctx: &AccessContext, collection: &Collection) -> FieldAccess {
        let _ = (ctx, collection);
        FieldAccess::default()
    }
}

/// Column holding the owning tenant of each row of a tenant-scoped collection.
pub const TENANT_COLUMN: &str = "_tenant";

/// Collection option that makes a collection tenant-scoped (set at creation,
/// immutable afterwards).
pub const TENANT_SCOPED_OPTION: &str = "tenant_scoped";

/// Computed option of view collections whose query reads a tenant-scoped
/// table; such views are closed to non-privileged callers.
pub const READS_TENANT_DATA_OPTION: &str = "reads_tenant_data";

/// Whether rows of `collection` belong to tenants (`options.tenant_scoped`).
pub fn is_tenant_scoped(collection: &Collection) -> bool {
    !collection.is_view() && collection.options.get(TENANT_SCOPED_OPTION) == Some(&Json::Bool(true))
}

/// Whether a view collection reads tenant-scoped tables.
pub fn reads_tenant_data(collection: &Collection) -> bool {
    collection.is_view() && collection.options.get(READS_TENANT_DATA_OPTION) == Some(&Json::Bool(true))
}

/// Fail-closed default: the system and administrators may do everything,
/// everybody else nothing. FB-21 replaces it with the policy engine.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultAuthorizer;

impl Authorizer for DefaultAuthorizer {
    fn authorize(&self, ctx: &AccessContext, _action: Action, _collection: Option<&Collection>) -> Decision {
        if ctx.is_privileged() {
            Decision::Allow
        } else {
            Decision::Deny
        }
    }
}

/// Hashes secrets (password fields). Provided by the auth package (FB-20,
/// Argon2id). Without one, writing a password field fails closed.
pub trait SecretHasher: Send + Sync + fmt::Debug {
    /// Returns the encoded hash of `secret`.
    fn hash(&self, secret: &str) -> Result<String, String>;
}
