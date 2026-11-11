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

use forgebase_db::Dialect;
use forgebase_query::{CompileOptions, Compiler, Expr, QueryError, SqlBuilder};
use forgebase_schema::{Collection, Field, FieldKind};
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
    /// Id of the administrator acting as this principal (ADR-0014): set only
    /// for an impersonation session; copied into every audit row.
    pub impersonator: Option<String>,
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
            impersonator: None,
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

    /// Same context acting on behalf of an impersonating administrator.
    pub fn with_impersonator(mut self, admin_id: impl Into<String>) -> Self {
        self.impersonator = Some(admin_id.into());
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
    /// Read graph nodes and edges, run traversals (FB-51). Graph data has no
    /// row-level policies: a `Decision::Filter` is treated as a denial.
    GraphRead,
    /// Create, change or delete graph nodes and edges (FB-51).
    GraphWrite,
    /// Read jobs, job history, schedules, workers and queue statistics (FB-61).
    ReadJobs,
    /// Enqueue a background job (FB-61).
    EnqueueJob,
    /// Cancel, retry or delete jobs; create, change or delete schedules (FB-61).
    ManageJobs,
    /// Act as a job worker: register, claim, heartbeat, complete, fail (FB-61).
    WorkJobs,
    /// Read key-value data: GET-style commands, scans, subscribing (FB-31).
    KvRead,
    /// Write key-value data: SET-style commands, TTL changes, publishing.
    KvWrite,
    /// Key-value administration: flush a namespace, list namespaces, server statistics.
    KvAdmin,
    /// Read vector collections and items, run similarity and hybrid searches,
    /// read index statistics (FB-50). Vector data has no row-level policies: a
    /// `Decision::Filter` is treated as a denial.
    VectorRead,
    /// Upsert and delete vector items (FB-50).
    VectorWrite,
    /// Create and drop vector collections and fields, rebuild indexes, run
    /// recall checks (FB-50).
    VectorAdmin,
    /// Receive messages and presence of a realtime channel (FB-40). Asked
    /// through [`Authorizer::authorize_channel`]; a `Decision::Filter` is a
    /// denial.
    ChannelSubscribe,
    /// Publish to a realtime channel and join its presence (FB-40).
    ChannelPublish,
    /// Read server functions, their versions and execution logs (FB-60).
    ReadFunctions,
    /// Deploy, roll back and delete server functions; manage their secrets (FB-60).
    ManageFunctions,
    /// Invoke an HTTP server function whose access mode is `permission` (FB-60).
    InvokeFunction,
    /// Read any bucket and object, list, download, sign download URLs (FB-30).
    /// Bucket ACLs grant narrower access without this platform permission.
    StorageRead,
    /// Upload, copy, move and delete objects in any bucket (FB-30).
    StorageWrite,
    /// Create and delete buckets, change bucket policies and ACLs, run the
    /// storage reconciler (FB-30).
    StorageAdmin,
    /// Read topics, subscriptions, snapshots, retained messages, dead letters
    /// and queue metrics (FB-63).
    QueueRead,
    /// Publish messages to a topic (FB-63).
    QueuePublish,
    /// Consume a subscription: pull, stream, ack, nack and extend ack
    /// deadlines (FB-63).
    QueueConsume,
    /// Create, change and delete topics, subscriptions and snapshots; seek
    /// subscriptions (FB-63).
    QueueAdmin,
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

    /// The decision for `action` on one named engine resource (FB-134): a KV
    /// namespace, vector collection, graph label, bucket, topic or
    /// subscription. A `Decision::Filter` holds a predicate over the item
    /// fields of the resource's [`ResourceSchema`]; compile it with
    /// [`ResourceSchema::compile`] into the statement that reads or writes the
    /// items. An engine that cannot apply a predicate treats it as a denial.
    ///
    /// Defaults to `authorize(ctx, action, None)`, so authorizers written
    /// before FB-134 (and their wrappers) keep their answers; the policy
    /// engine overrides it with resource permission keys
    /// (`engine:<action>:<name>`) and resource rules.
    fn authorize_resource(&self, ctx: &AccessContext, action: Action, resource: &ResourceRef) -> Decision {
        let _ = resource;
        self.authorize(ctx, action, None)
    }

    /// Tells the authorizer which item fields resource rules of
    /// `schema.kind` may read (FB-134). Engines call it once when they start;
    /// rules are validated against it when saved. Default: ignored.
    fn register_resource_schema(&self, schema: ResourceSchema) {
        let _ = schema;
    }

    /// The decision for `action` on the realtime channel `channel` (FB-40).
    /// Defaults to `authorize(ctx, action, None)`; a policy engine overrides
    /// it to write rules per channel name.
    fn authorize_channel(&self, ctx: &AccessContext, action: Action, channel: &str) -> Decision {
        let _ = channel;
        self.authorize(ctx, action, None)
    }

    /// Who the data core's statements for `action` on `collection` run as
    /// under native PostgreSQL row-level security (`enforce` mode, FB-152):
    /// the RLS role with this caller's claims. `None` (the default): as the
    /// server, with only the data core's own checks.
    fn native_session(
        &self,
        ctx: &AccessContext,
        action: Action,
        collection: &Collection,
    ) -> Option<forgebase_db::RunAs> {
        let _ = (ctx, action, collection);
        None
    }

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

// ---------------------------------------------------------------------------
// Engine resources (FB-134)
// ---------------------------------------------------------------------------

/// Kind of a named engine resource (FB-134). Permission keys name a resource
/// as `engine:<action>:<name>` (`engine:kv_read:cache`); resource rules are
/// stored per (kind, name, operation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ResourceKind {
    /// A key-value namespace ([`Action::KvRead`], `KvWrite`, `KvAdmin`).
    KvNamespace,
    /// A vector collection ([`Action::VectorRead`], `VectorWrite`, `VectorAdmin`).
    VectorCollection,
    /// A graph node label ([`Action::GraphRead`], `GraphWrite`).
    GraphLabel,
    /// A graph edge type ([`Action::GraphRead`], `GraphWrite`).
    GraphEdgeType,
    /// An object-storage bucket ([`Action::StorageRead`], `StorageWrite`, `StorageAdmin`).
    Bucket,
    /// A queue topic ([`Action::QueuePublish`], `QueueRead`, `QueueAdmin`).
    Topic,
    /// A queue subscription ([`Action::QueueConsume`], `QueueRead`, `QueueAdmin`).
    Subscription,
    /// A realtime channel ([`Action::ChannelSubscribe`] as `view`,
    /// `ChannelPublish` as `create`; FB-136). The name is the channel's name
    /// without its tenant prefix (`forgebase_realtime::channel`).
    Channel,
}

impl ResourceKind {
    /// Every kind.
    pub const ALL: [ResourceKind; 8] = [
        ResourceKind::KvNamespace,
        ResourceKind::VectorCollection,
        ResourceKind::GraphLabel,
        ResourceKind::GraphEdgeType,
        ResourceKind::Bucket,
        ResourceKind::Topic,
        ResourceKind::Subscription,
        ResourceKind::Channel,
    ];

    /// Stored and URL name: `kv_namespace`, `vector_collection`,
    /// `graph_label`, `graph_edge_type`, `bucket`, `queue_topic`,
    /// `queue_subscription`, `realtime_channel`.
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceKind::KvNamespace => "kv_namespace",
            ResourceKind::VectorCollection => "vector_collection",
            ResourceKind::GraphLabel => "graph_label",
            ResourceKind::GraphEdgeType => "graph_edge_type",
            ResourceKind::Bucket => "bucket",
            ResourceKind::Topic => "queue_topic",
            ResourceKind::Subscription => "queue_subscription",
            ResourceKind::Channel => "realtime_channel",
        }
    }

    /// The kind with this name.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.as_str() == s)
    }

    /// Engine actions asked about resources of this kind.
    pub fn actions(self) -> &'static [Action] {
        match self {
            ResourceKind::KvNamespace => &[Action::KvRead, Action::KvWrite, Action::KvAdmin],
            ResourceKind::VectorCollection => &[Action::VectorRead, Action::VectorWrite, Action::VectorAdmin],
            ResourceKind::GraphLabel | ResourceKind::GraphEdgeType => &[Action::GraphRead, Action::GraphWrite],
            ResourceKind::Bucket => &[Action::StorageRead, Action::StorageWrite, Action::StorageAdmin],
            ResourceKind::Topic => &[Action::QueuePublish, Action::QueueRead, Action::QueueAdmin],
            ResourceKind::Subscription => &[Action::QueueConsume, Action::QueueRead, Action::QueueAdmin],
            ResourceKind::Channel => &[Action::ChannelSubscribe, Action::ChannelPublish],
        }
    }

    /// The item fields resource rules may read until the engine registers its
    /// own schema ([`Authorizer::register_resource_schema`]):
    ///
    /// | kind | fields |
    /// |---|---|
    /// | `kv_namespace` | `key` |
    /// | `vector_collection` | `id`, `metadata` (JSON) |
    /// | `graph_label` | `id`, `label`, `properties` (JSON) |
    /// | `graph_edge_type` | `id`, `type`, `source`, `target`, `properties` (JSON) |
    /// | `bucket` | `key`, `created_by`, `size`, `content_type`, `folders` (JSON array of the key's folder names) |
    /// | `queue_topic`, `queue_subscription` | none (rules may use `@request` only) |
    /// | `realtime_channel` | none (`@request` only); operations `view` (subscribe) and `create` (publish, presence) |
    pub fn default_schema(self) -> ResourceSchema {
        use ResourceFieldType::{Json as J, Number as N, Text as T};
        let s = ResourceSchema::new(self);
        match self {
            ResourceKind::KvNamespace => s.field("key", T),
            ResourceKind::VectorCollection => s.field("id", T).field("metadata", J),
            ResourceKind::GraphLabel => s.field("id", T).field("label", T).field("properties", J),
            ResourceKind::GraphEdgeType => s
                .field("id", T)
                .field("type", T)
                .field("source", T)
                .field("target", T)
                .field("properties", J),
            ResourceKind::Bucket => s
                .field("key", T)
                .field("created_by", T)
                .field("size", N)
                .field("content_type", T)
                .field("folders", J),
            ResourceKind::Topic | ResourceKind::Subscription => s,
            ResourceKind::Channel => s.with_ops(&[ResourceOp::View, ResourceOp::Create]),
        }
    }
}

impl fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What an operation does to the items of a resource: the action of a
/// resource rule, with the same names as collection rules. Rules may also be
/// saved for `read` (list and view) and `write` (create, update and delete);
/// a rule for the exact operation wins over its alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResourceOp {
    /// Enumerate or search items (KV scans, vector searches, object listings).
    List,
    /// Read one item.
    View,
    /// Add an item (KV set of a new key, upsert of a new vector, upload, publish).
    Create,
    /// Change an item.
    Update,
    /// Remove an item.
    Delete,
}

impl ResourceOp {
    /// Every operation.
    pub const ALL: [ResourceOp; 5] = [
        ResourceOp::List,
        ResourceOp::View,
        ResourceOp::Create,
        ResourceOp::Update,
        ResourceOp::Delete,
    ];

    /// `list`, `view`, `create`, `update` or `delete`.
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceOp::List => "list",
            ResourceOp::View => "view",
            ResourceOp::Create => "create",
            ResourceOp::Update => "update",
            ResourceOp::Delete => "delete",
        }
    }

    /// The operation with this name.
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|o| o.as_str() == s)
    }

    /// The alias covering this operation: `read` or `write`.
    pub fn alias(self) -> &'static str {
        match self {
            ResourceOp::List | ResourceOp::View => "read",
            _ => "write",
        }
    }

    /// The operations a rule action names (`read` and `write` expand).
    pub fn expand(action: &str) -> Option<Vec<ResourceOp>> {
        match action {
            "read" => Some(vec![ResourceOp::List, ResourceOp::View]),
            "write" => Some(vec![ResourceOp::Create, ResourceOp::Update, ResourceOp::Delete]),
            a => Self::parse(a).map(|o| vec![o]),
        }
    }
}

impl fmt::Display for ResourceOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One named engine resource an operation acts on, for
/// [`Authorizer::authorize_resource`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ResourceRef {
    /// Kind.
    pub kind: ResourceKind,
    /// Name as the engine stores it (namespace, collection, label, bucket,
    /// topic, subscription). Names are compared case-sensitively.
    pub name: String,
    /// What the operation does to the resource's items.
    pub op: ResourceOp,
    /// Principal key of the resource's owner (a subscription's creator):
    /// the owner holds every permission on it, an explicit deny still wins.
    pub owner: Option<String>,
}

impl ResourceRef {
    /// A resource without an owner.
    pub fn new(kind: ResourceKind, name: impl Into<String>, op: ResourceOp) -> Self {
        Self {
            kind,
            name: name.into(),
            op,
            owner: None,
        }
    }

    /// Same resource owned by `principal_key` ([`AccessContext::principal_key`]).
    pub fn with_owner(mut self, principal_key: impl Into<String>) -> Self {
        self.owner = Some(principal_key.into());
        self
    }
}

impl fmt::Display for ResourceRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.kind, self.name)
    }
}

/// Type of an item field readable by resource rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceFieldType {
    /// Text.
    Text,
    /// A number.
    Number,
    /// `true` / `false` (stored as 0 / 1).
    Bool,
    /// A UTC timestamp in ForgeBase's text format (compares with `@now`).
    Date,
    /// A JSON value; rules read into it by path (`metadata.team`).
    Json,
}

impl ResourceFieldType {
    /// `text`, `number`, `bool`, `date` or `json`.
    pub fn as_str(self) -> &'static str {
        match self {
            ResourceFieldType::Text => "text",
            ResourceFieldType::Number => "number",
            ResourceFieldType::Bool => "bool",
            ResourceFieldType::Date => "date",
            ResourceFieldType::Json => "json",
        }
    }

    fn field_kind(self) -> FieldKind {
        match self {
            ResourceFieldType::Text => FieldKind::Text {
                min_length: None,
                max_length: None,
            },
            ResourceFieldType::Number => FieldKind::Number {
                min: None,
                max: None,
                integer: false,
            },
            ResourceFieldType::Bool => FieldKind::Bool,
            ResourceFieldType::Date => FieldKind::Date,
            ResourceFieldType::Json => FieldKind::Json { max_size: None },
        }
    }
}

/// One item field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceField {
    /// Name, as rules write it and as the engine's row exposes it.
    pub name: String,
    /// Type.
    pub ty: ResourceFieldType,
}

/// The item fields of one resource kind that resource rules may read, and
/// the operations rules may be written for. The engine supplies it
/// ([`Authorizer::register_resource_schema`]; until then
/// [`ResourceKind::default_schema`]).
///
/// A `Decision::Filter` from [`Authorizer::authorize_resource`] is compiled
/// with [`ResourceSchema::compile`]: the SQL names every field as a column of
/// the row alias, so the engine selects its items as a derived table (or
/// view) exposing exactly these column names. Field names are compared
/// case-insensitively.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceSchema {
    /// Kind.
    pub kind: ResourceKind,
    /// Readable item fields.
    pub fields: Vec<ResourceField>,
    /// Operations rules may be saved for (default: all five).
    pub ops: Vec<ResourceOp>,
}

impl ResourceSchema {
    /// A schema without fields (rules may use `@request` values only).
    pub fn new(kind: ResourceKind) -> Self {
        Self {
            kind,
            fields: Vec::new(),
            ops: ResourceOp::ALL.to_vec(),
        }
    }

    /// Same schema with another field.
    pub fn field(mut self, name: impl Into<String>, ty: ResourceFieldType) -> Self {
        self.fields.push(ResourceField { name: name.into(), ty });
        self
    }

    /// Same schema with only these operations.
    pub fn with_ops(mut self, ops: &[ResourceOp]) -> Self {
        self.ops = ops.to_vec();
        self
    }

    /// The field with this name (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&ResourceField> {
        self.fields.iter().find(|f| f.name.eq_ignore_ascii_case(name))
    }

    /// The fields as a strict collection named after the kind, for the
    /// filter compiler.
    pub fn collection(&self) -> Collection {
        self.fields.iter().fold(Collection::base(self.kind.as_str()), |c, f| {
            c.with_field(Field::new(f.name.clone(), f.ty.field_kind()))
        })
    }

    /// Checks that `p` reads only declared fields and compiles.
    pub fn validate(&self, dialect: &dyn Dialect, p: &Predicate) -> Result<(), QueryError> {
        let mut b = SqlBuilder::new(dialect);
        self.compile(&mut b, "t", p).map(|_| ())
    }

    /// Compiles a predicate of a `Decision::Filter` into a boolean SQL
    /// expression over the row aliased `alias` (an empty alias: bare column
    /// names); parameters are bound into `b`.
    pub fn compile(&self, b: &mut SqlBuilder<'_>, alias: &str, p: &Predicate) -> Result<String, QueryError> {
        let mut unknown = None;
        let mut expr = p.filter.clone();
        expr.for_each_path_mut(&mut |path| {
            if let Some(forgebase_query::PathSeg::Name(n)) = path.segments.first()
                && self.get(n).is_none()
                && unknown.is_none()
            {
                unknown = Some(n.clone());
            }
        });
        if let Some(n) = unknown {
            return Err(QueryError::UnknownField(n));
        }
        let opts = CompileOptions {
            allow_hidden: true,
            params: p.params.clone(),
            ..CompileOptions::default()
        };
        let catalog: HashMap<String, Collection> = HashMap::new();
        Compiler::new(&catalog, &opts).filter(b, &self.collection(), alias, &expr)
    }
}

#[cfg(test)]
mod resource_tests {
    use super::*;

    #[test]
    fn kinds_and_ops_round_trip() {
        for k in ResourceKind::ALL {
            assert_eq!(ResourceKind::parse(k.as_str()), Some(k));
            assert!(!k.actions().is_empty());
        }
        for o in ResourceOp::ALL {
            assert_eq!(ResourceOp::parse(o.as_str()), Some(o));
            assert!(ResourceOp::expand(o.alias()).unwrap().contains(&o));
        }
        assert_eq!(ResourceOp::expand("admin"), None);
    }

    #[test]
    fn schemas_compile_declared_fields_only() {
        let s = ResourceKind::Bucket.default_schema();
        let d = forgebase_db::SqliteDialect;
        let ok = Predicate::parse(
            "created_by = {:me} && size < 100",
            [("me".to_owned(), Json::from("u1"))].into(),
        )
        .unwrap();
        let mut b = SqlBuilder::new(&d);
        let sql = s.compile(&mut b, "o", &ok).unwrap();
        assert!(sql.contains("o.\"created_by\""), "{sql}");
        assert_eq!(b.params().len(), 2);
        for bad in ["id = 'x'", "created = @now", "nope = 1"] {
            let p = Predicate::parse(bad, HashMap::new()).unwrap();
            assert!(s.validate(&d, &p).is_err(), "{bad} must be refused");
        }
        let json = ResourceKind::VectorCollection.default_schema();
        let p = Predicate::parse("metadata.team = 'a'", HashMap::new()).unwrap();
        json.validate(&d, &p).unwrap();
    }
}
