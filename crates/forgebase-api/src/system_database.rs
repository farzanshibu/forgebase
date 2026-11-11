//! Database topology admin API (FB-175, ADR-0029 §4.3):
//! `/api/system/database/*`.
//!
//! What the Database topology screen (FB-176) and operators call to see and
//! change where the data lives: the primary server, its read replicas and
//! the shards. This module is the HTTP layer: authentication
//! (administrators only), MFA step-up, the audit log, input checks, error
//! codes, `202 {operation}` for long operations and the OpenAPI document.
//! The work itself is the server's ([`TopologyOps`], implemented by
//! `forgebase-server` over `forgebase-pgengine`'s replica service, the
//! router and the job queue: this crate depends on neither).
//!
//! * **Who.** Every route but the agent routes needs an administrator
//!   (`401 unauthenticated`, `403 forbidden`).
//! * **MFA step-up.** Promote, remove (a node or a shard), move and running
//!   a rebalance plan need, when the administrator has a second factor, a
//!   session that passed it in the last [`STEP_UP_SECS`] seconds (`403
//!   mfa_step_up_required`: sign in again). API keys of such an
//!   administrator cannot run them.
//! * **Audit.** Every change and every join token is recorded
//!   (`db.topology.*` in `GET /api/auth/audit`, with the node, set or
//!   operation id; never a token or URL), refusals as
//!   `db.topology.refused`. Reads are not recorded: the screen polls every
//!   5 s.
//! * **Operations** are jobs of reserved kinds ([`kinds`]) on queue
//!   [`OPERATIONS_QUEUE`], with progress (`percent`, `stage`, `message`) on
//!   channel `jobs:<id>` and cancel at safe points.
//! * **SQLite** answers `400 postgres_required`; `GET …/topology` shows the
//!   one SQLite server.
//! * **Agents** (`forgebase node`, FB-168): `…/agent/*` sit outside
//!   administrator authentication; the join token or the agent key
//!   (`Authorization: Bearer`) is checked by the server.
//! * Tenant moves and rebalancing answer `501 not_implemented` until
//!   FB-174.

use std::borrow::Cow;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use forgebase_auth::Authenticated;
use forgebase_data::{AccessContext, Principal};
use forgebase_db::{Backend, BackendKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json_, json};
use utoipa::{OpenApi, ToSchema};

use crate::AppState;
use crate::error::ApiError;
use crate::http::ApiJson;
use crate::principal::Access;

/// Result of the handlers.
pub type ApiResult<T> = Result<T, ApiError>;

/// How recent the second factor must be for the step-up operations (10 minutes).
pub const STEP_UP_SECS: i64 = 600;

/// Queue of the topology operations (a filtered view of jobs).
pub const OPERATIONS_QUEUE: &str = "database";

/// Reserved job kinds of the topology operations (ADR-0029 §4.3).
pub mod kinds {
    /// Add a local replica (clone and start).
    pub const NODE_ADD: &str = "database.node.add";
    /// Re-clone a replica, or rejoin an old primary.
    pub const NODE_REBUILD: &str = "database.node.rebuild";
    /// Promote a replica to primary.
    pub const PROMOTE: &str = "database.promote";
    /// Add a shard.
    pub const SHARD_ADD: &str = "database.shard.add";
    /// Move a shard's tenants out (FB-174).
    pub const SHARD_DRAIN: &str = "database.shard.drain";
    /// Move one tenant (FB-174).
    pub const TENANT_MOVE: &str = "database.tenant.move";
    /// Run a rebalance plan (FB-174).
    pub const REBALANCE: &str = "database.rebalance";
    /// Every kind.
    pub const ALL: &[&str] = &[
        NODE_ADD,
        NODE_REBUILD,
        PROMOTE,
        SHARD_ADD,
        SHARD_DRAIN,
        TENANT_MOVE,
        REBALANCE,
    ];
}

/// Error codes of this API besides the replica service's own
/// (`node_not_found`, `confirm_mismatch`, `engine_mismatch`,
/// `data_loss_unconfirmed`, `join_token_invalid`, …).
pub mod codes {
    /// 409: one topology change per set at a time.
    pub const OPERATION_IN_PROGRESS: &str = "operation_in_progress";
    /// 409: the tenant is being moved.
    pub const TENANT_MOVING: &str = "tenant_moving";
    /// 409: a shard with tenants cannot be removed.
    pub const SHARD_NOT_EMPTY: &str = "shard_not_empty";
    /// 400: the node does not belong to the set or is not a copy of its primary.
    pub const NODE_MISMATCH: &str = "node_mismatch";
    /// 400: the servers cannot hold shards (for example `max_prepared_transactions = 0`).
    pub const SHARDING_UNSUPPORTED: &str = "sharding_unsupported";
    /// 503: the node does not answer.
    pub const NODE_UNREACHABLE: &str = "node_unreachable";
    /// 400: replicas and shards need PostgreSQL.
    pub const POSTGRES_REQUIRED: &str = "postgres_required";
    /// 403: sign in again with the second factor.
    pub const MFA_STEP_UP_REQUIRED: &str = "mfa_step_up_required";
    /// 404.
    pub const OPERATION_NOT_FOUND: &str = "operation_not_found";
    /// 409: the operation has passed its last safe point.
    pub const NOT_CANCELLABLE: &str = "operation_not_cancellable";
    /// 503: this server runs no topology service.
    pub const TOPOLOGY_UNAVAILABLE: &str = "topology_unavailable";
    /// 501: tenant moves and rebalancing (FB-174).
    pub const NOT_IMPLEMENTED: &str = "not_implemented";
}

// ---------------------------------------------------------------------------
// The server's side.
// ---------------------------------------------------------------------------

/// Future of a [`TopologyOps`] call.
pub type OpsFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, TopologyError>> + Send + 'a>>;

/// An error of the topology service: HTTP status, stable code, message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TopologyError {
    /// HTTP status.
    pub status: u16,
    /// Stable code.
    pub code: Cow<'static, str>,
    /// What happened and what to do.
    pub message: String,
}

impl TopologyError {
    /// An error with `status` and `code`.
    pub fn new(status: u16, code: impl Into<Cow<'static, str>>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
        }
    }

    /// `400 postgres_required`.
    pub fn postgres_required() -> Self {
        Self::new(
            400,
            codes::POSTGRES_REQUIRED,
            "replicas and shards need PostgreSQL; this server runs on SQLite (forgebase db convert --to postgres)",
        )
    }

    /// `400 invalid_request`.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(400, "invalid_request", message)
    }

    /// `500 internal_error` with the message (logged, not secret).
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(500, "internal_error", message)
    }
}

impl std::fmt::Display for TopologyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl From<TopologyError> for ApiError {
    fn from(e: TopologyError) -> Self {
        let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        ApiError::new(status, e.code, e.message)
    }
}

/// What the server does for the routes. Every method may assume the caller
/// is an administrator who passed the step-up where one is needed; `ctx`
/// is passed for job ownership and audit details.
pub trait TopologyOps: Send + Sync + std::fmt::Debug + 'static {
    /// Sets, nodes with health and lag, tenant counts, epoch, running operations.
    fn topology(&self) -> OpsFuture<'_, TopologyView>;
    /// One node with its lag over the last hour.
    fn node<'a>(&'a self, id: &'a str) -> OpsFuture<'a, NodeDetail>;
    /// Adds a replica: `local` → operation, `external` → the node at once.
    fn add_node<'a>(&'a self, ctx: &'a AccessContext, req: AddNode) -> OpsFuture<'a, Added>;
    /// Changes a node's weight.
    fn update_node<'a>(&'a self, id: &'a str, req: UpdateNode) -> OpsFuture<'a, NodeInfo>;
    /// Removes a replica (its slot is dropped).
    fn remove_node<'a>(&'a self, id: &'a str, confirm: &'a str) -> OpsFuture<'a, Removed>;
    /// The data-loss estimate for the promote dialog.
    fn promote_check<'a>(&'a self, id: &'a str) -> OpsFuture<'a, PromoteCheck>;
    /// Promotes a replica (operation).
    fn promote<'a>(&'a self, ctx: &'a AccessContext, id: &'a str, req: PromoteBody) -> OpsFuture<'a, Operation>;
    /// Re-clones a replica or rejoins an old primary (operation).
    fn rebuild<'a>(&'a self, ctx: &'a AccessContext, id: &'a str) -> OpsFuture<'a, Operation>;
    /// A one-time join token for `forgebase node join`.
    fn join_token<'a>(
        &'a self,
        ctx: &'a AccessContext,
        req: JoinTokenBody,
        server_url: &'a str,
    ) -> OpsFuture<'a, JoinToken>;
    /// Adds a shard (operation).
    fn add_shard<'a>(&'a self, ctx: &'a AccessContext, req: AddShard) -> OpsFuture<'a, Operation>;
    /// Changes a shard's state (`active`, `draining`), weight or `hosts_tenants`.
    fn update_shard<'a>(&'a self, id: &'a str, req: UpdateShard) -> OpsFuture<'a, SetInfo>;
    /// Removes an empty shard.
    fn remove_shard<'a>(&'a self, id: &'a str, confirm: &'a str) -> OpsFuture<'a, Removed>;
    /// Tenants with their set, newest filter semantics in [`PlacementQuery`].
    fn placements(&self, q: PlacementQuery) -> OpsFuture<'_, PlacementPage>;
    /// Topology operations, newest first.
    fn operations<'a>(&'a self, ctx: &'a AccessContext, q: OperationQuery) -> OpsFuture<'a, OperationList>;
    /// One operation.
    fn operation<'a>(&'a self, ctx: &'a AccessContext, id: &'a str) -> OpsFuture<'a, Operation>;
    /// Cancels an operation (at its next safe point).
    fn cancel_operation<'a>(&'a self, ctx: &'a AccessContext, id: &'a str) -> OpsFuture<'a, Operation>;
    /// `POST …/agent/join` (the token is the credential).
    fn agent_join(&self, body: Json_, peer: Option<IpAddr>) -> OpsFuture<'_, Json_>;
    /// `GET …/agent/nodes/{id}/commands` (agent key).
    fn agent_commands<'a>(
        &'a self,
        id: &'a str,
        key: &'a str,
        wait_ms: Option<u64>,
        resend: bool,
    ) -> OpsFuture<'a, Json_>;
    /// `POST …/agent/nodes/{id}/heartbeat` (agent key).
    fn agent_heartbeat<'a>(&'a self, id: &'a str, key: &'a str, body: Json_) -> OpsFuture<'a, Json_>;
}

// ---------------------------------------------------------------------------
// Shapes.
// ---------------------------------------------------------------------------

/// `GET /api/system/database/topology`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TopologyView {
    /// `postgres` or `sqlite`.
    pub backend: String,
    /// PostgreSQL version of the control primary, when known.
    pub engine_version: Option<String>,
    /// Topology epoch (grows on every change).
    pub epoch: u64,
    /// Control first, then shards by number.
    pub sets: Vec<SetInfo>,
    /// Every node that is not retired.
    pub nodes: Vec<NodeInfo>,
    /// Operations queued or running.
    pub operations: Vec<Operation>,
    /// Set after a promote of control's primary: the server must be
    /// restarted to move its connections to the new primary.
    pub restart_required: Option<String>,
}

/// One set (control or a shard).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetInfo {
    /// Id (`control` for control).
    pub id: String,
    /// Display name.
    pub name: String,
    /// `control` or `shard`.
    pub kind: String,
    /// Set number (0 for control).
    pub number: u16,
    /// `provisioning`, `active`, `draining`, `migrating` or `retired`.
    pub state: String,
    /// Relative capacity.
    pub weight: u32,
    /// Takes new tenants.
    pub hosts_tenants: bool,
    /// Tenants placed on it (control also serves tenants with no placement row).
    pub tenants: u64,
    /// Sum of the tenants' estimated sizes in bytes.
    pub est_bytes: u64,
}

/// One server of a set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct NodeInfo {
    /// Id.
    pub id: String,
    /// Set id.
    pub set: String,
    /// Name (`postgres/nodes/<name>` for local nodes).
    pub name: String,
    /// `local` (this server), `remote` (`forgebase node`) or `external`.
    pub kind: String,
    /// `primary` or `replica`.
    pub role: String,
    /// `provisioning`, `ready`, `catching_up`, `streaming`, `lagging`,
    /// `unreachable`, `needs_rebuild` or `promoting`.
    pub state: String,
    /// Share of replica reads.
    pub weight: u32,
    /// PostgreSQL version.
    pub engine_version: Option<String>,
    /// Cluster system identifier.
    pub system_identifier: Option<String>,
    /// Timeline.
    pub timeline: Option<i64>,
    /// `api`, or `config` (declared in `forgebase.toml`: read-only).
    pub origin: String,
    /// Serves reads now (replicas).
    pub in_rotation: Option<bool>,
    /// Time behind the primary at the last sample (replicas).
    pub lag_ms: Option<u64>,
    /// Bytes of changes behind (replicas).
    pub lag_bytes: Option<u64>,
    /// Last replayed position (`X/Y`).
    pub replay_lsn: Option<String>,
    /// The latest failure while out of rotation.
    pub last_error: Option<String>,
    /// The `forgebase node` agent of a remote node: `state`, `address`,
    /// `replay_lsn`, `disk_free`, `last_seen_at`.
    #[schema(value_type = Option<Object>)]
    pub agent: Option<Json_>,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339.
    pub updated_at: String,
}

/// One lag sample.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LagSample {
    /// RFC 3339 minute.
    pub at: String,
    /// Highest time lag in that minute.
    pub lag_ms: u64,
    /// Highest byte lag in that minute.
    pub lag_bytes: u64,
}

/// `GET /api/system/database/nodes/{id}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct NodeDetail {
    /// The node.
    pub node: NodeInfo,
    /// Last hour, one-minute buckets, oldest first (kept in memory: empty
    /// after a restart).
    pub lag_history: Vec<LagSample>,
}

/// Body of `POST /api/system/database/nodes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddNode {
    /// Set id (default `control`).
    #[serde(default = "control")]
    pub set: String,
    /// Node name: 1 to 32 of `a-z 0-9 - _`, not `primary`.
    pub name: String,
    /// `local` (a replica on this server, cloned now), or `external` (a
    /// replica you run, at `url`). Replicas on other hosts join with a
    /// join token instead.
    pub kind: String,
    /// `external` only: `postgres://` URL (stored sealed, never shown again).
    #[serde(default)]
    pub url: Option<String>,
    /// Share of replica reads (default 1).
    #[serde(default)]
    pub weight: Option<u32>,
}

fn control() -> String {
    "control".to_owned()
}

/// Answer of `POST /api/system/database/nodes`: an operation (`local`) or the node (`external`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Added {
    /// The clone-and-start operation (`local`).
    pub operation: Option<Operation>,
    /// The node (`external`).
    pub node: Option<NodeInfo>,
}

/// Body of `PATCH /api/system/database/nodes/{id}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateNode {
    /// Share of replica reads, 0 to 1000 (0 takes it out of rotation).
    pub weight: u32,
}

/// Answer of the removals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Removed {
    /// The removed node or set id.
    pub removed: String,
}

/// Body of `POST /api/system/database/nodes/{id}/promote`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PromoteBody {
    /// The node name, typed by the operator.
    pub confirm: String,
    /// "The old primary is down": promote even though recent commits may be lost.
    #[serde(default)]
    pub accept_data_loss: bool,
}

/// `GET /api/system/database/nodes/{id}/promote-check`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PromoteCheck {
    /// The replica's replay position.
    pub replayed: Option<String>,
    /// The primary's position (when it answers).
    pub primary_lsn: Option<String>,
    /// Bytes the replica is behind (when both are known).
    pub behind_bytes: Option<u64>,
    /// The old primary answers: a promote loses nothing. `false`: the
    /// dialog shows "the old primary is down" and needs `accept_data_loss`.
    pub primary_reachable: bool,
}

/// Body of `POST /api/system/database/join-tokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct JoinTokenBody {
    /// Set id (default `control`).
    #[serde(default = "control")]
    pub set: String,
    /// `replica` (default). Shard primaries on other hosts come with FB-174.
    #[serde(default = "replica")]
    pub role: String,
}

fn replica() -> String {
    "replica".to_owned()
}

/// A join token, shown once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JoinToken {
    /// Token id (in the audit log).
    pub id: String,
    /// The token (`fbj_…`); only its hash is stored.
    pub token: String,
    /// Set id.
    pub set: String,
    /// Role.
    pub role: String,
    /// RFC 3339 (15 minutes).
    pub expires_at: String,
    /// The command to run on the other host.
    pub command: String,
}

/// Where a new shard's primary runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ShardNode {
    /// Node name.
    pub name: String,
    /// `external` (a server you run, at `url`). `local` and `remote` shard
    /// primaries come with FB-174.
    pub kind: String,
    /// `external` only: `postgres://` URL of an empty database (stored sealed).
    #[serde(default)]
    pub url: Option<String>,
}

/// Body of `POST /api/system/database/shards`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AddShard {
    /// Display name (unique ignoring case).
    pub name: String,
    /// Its primary.
    pub node: ShardNode,
    /// Relative capacity (default 1).
    #[serde(default)]
    pub weight: Option<u32>,
}

/// Body of `PATCH /api/system/database/shards/{id}`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateShard {
    /// `active` or `draining` (takes no new tenants).
    #[serde(default)]
    pub state: Option<String>,
    /// Relative capacity, 1 to 1000.
    #[serde(default)]
    pub weight: Option<u32>,
    /// Takes new tenants.
    #[serde(default)]
    pub hosts_tenants: Option<bool>,
}

/// Filters of `GET /api/system/database/placements`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementQuery {
    /// Only this set.
    pub set: Option<String>,
    /// Only this project (environment ids of a project share its prefix).
    pub project: Option<String>,
    /// Tenant id contains this text.
    pub q: Option<String>,
    /// From the `next_cursor` of the previous page.
    pub cursor: Option<String>,
    /// Page size (default 50, at most 200).
    pub limit: Option<u32>,
}

/// One tenant's placement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlacementInfo {
    /// Tenant (environment id).
    pub tenant: String,
    /// Its project, when known.
    pub project: Option<String>,
    /// Its organization, when known.
    pub organization: Option<String>,
    /// Set id.
    pub set: String,
    /// `active`, `moving`, `frozen` or `cleanup`.
    pub state: String,
    /// Estimated rows.
    pub est_rows: u64,
    /// Estimated bytes.
    pub est_bytes: u64,
    /// RFC 3339 of the last move.
    pub moved_at: Option<String>,
}

/// A page of placements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlacementPage {
    /// Tenants by id.
    pub items: Vec<PlacementInfo>,
    /// Cursor of the next page (`None`: last page).
    pub next_cursor: Option<String>,
}

/// Filters of `GET /api/system/database/operations`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationQuery {
    /// Only queued and running ones.
    pub active: bool,
    /// Page size (default 50, at most 200).
    pub limit: Option<u32>,
}

/// Progress of an operation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct OperationProgress {
    /// 0 to 100.
    pub percent: Option<f64>,
    /// Stage (`cloning`, `starting`, `fencing`, `promoting`, …).
    pub stage: Option<String>,
    /// Detail.
    pub message: Option<String>,
}

/// A topology operation (a job of a reserved `database.*` kind).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Operation {
    /// Job id; progress streams on channel `jobs:<id>`.
    pub id: String,
    /// `database.node.add`, `database.promote`, …
    pub kind: String,
    /// `pending`, `running`, `succeeded`, `failed`, `cancelled`, `dead`.
    pub status: String,
    /// Set it changes.
    pub set: Option<String>,
    /// Node it changes.
    pub node: Option<String>,
    /// Latest progress.
    pub progress: Option<OperationProgress>,
    /// Cancel was asked for.
    pub cancel_requested: bool,
    /// It can still be cancelled.
    pub cancellable: bool,
    /// Result of a finished operation.
    #[schema(value_type = Option<Object>)]
    pub result: Option<Json_>,
    /// Why it failed.
    pub last_error: Option<String>,
    /// Who started it.
    pub created_by: String,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339.
    pub started_at: Option<String>,
    /// RFC 3339.
    pub finished_at: Option<String>,
}

/// A page of operations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct OperationList {
    /// Newest first.
    pub items: Vec<Operation>,
}

/// Body of `POST /api/system/database/placements/{tenant}/move`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MoveBody {
    /// Target set id.
    pub to: String,
    /// The tenant id, typed by the operator.
    pub confirm: String,
}

/// Body of `POST /api/system/database/rebalance/plan`.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct RebalancePlanBody {
    /// Sets to balance (default all).
    #[serde(default)]
    pub sets: Option<Vec<String>>,
    /// Most moves in the plan.
    #[serde(default)]
    pub max_moves: Option<u32>,
}

/// Body of `POST /api/system/database/rebalance`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RebalanceBody {
    /// From the plan.
    pub plan_id: String,
    /// The plan id, typed by the operator.
    pub confirm: String,
}

/// `202 Accepted` body.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Accepted {
    /// The operation.
    pub operation: Operation,
}

// ---------------------------------------------------------------------------
// Routes.
// ---------------------------------------------------------------------------

/// State of the routes.
#[derive(Clone)]
pub struct DbApi {
    app: AppState,
    ops: Option<Arc<dyn TopologyOps>>,
}

/// Every route under `/api/system/database` except the agent routes,
/// which [`agent_router`] serves outside authentication.
pub fn router(app: AppState, ops: Option<Arc<dyn TopologyOps>>) -> Router {
    Router::new()
        .route("/api/system/database/topology", get(topology))
        .route("/api/system/database/nodes", post(add_node))
        .route(
            "/api/system/database/nodes/{id}",
            get(node).patch(update_node).delete(remove_node),
        )
        .route("/api/system/database/nodes/{id}/promote-check", get(promote_check))
        .route("/api/system/database/nodes/{id}/promote", post(promote))
        .route("/api/system/database/nodes/{id}/rebuild", post(rebuild))
        .route("/api/system/database/join-tokens", post(join_token))
        .route("/api/system/database/shards", post(add_shard))
        .route(
            "/api/system/database/shards/{id}",
            axum::routing::patch(update_shard).delete(remove_shard),
        )
        .route("/api/system/database/placements", get(placements))
        .route("/api/system/database/placements/{tenant}/move", post(move_tenant))
        .route("/api/system/database/rebalance/plan", post(rebalance_plan))
        .route("/api/system/database/rebalance", post(rebalance))
        .route("/api/system/database/operations", get(operations))
        .route("/api/system/database/operations/{id}", get(operation))
        .route("/api/system/database/operations/{id}/cancel", post(cancel_operation))
        .with_state(DbApi { app, ops })
}

/// The agent routes (`forgebase node`): mounted outside authentication;
/// the token or agent key is the credential.
pub fn agent_router(app: AppState, ops: Option<Arc<dyn TopologyOps>>) -> Router {
    Router::new()
        .route("/api/system/database/agent/join", post(agent_join))
        .route("/api/system/database/agent/nodes/{id}/commands", get(agent_commands))
        .route("/api/system/database/agent/nodes/{id}/heartbeat", post(agent_heartbeat))
        .with_state(DbApi { app, ops })
}

fn require_admin(ctx: &AccessContext) -> ApiResult<()> {
    if ctx.is_privileged() {
        Ok(())
    } else if ctx.principal == Principal::Anonymous {
        Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "the database topology API needs an administrator's credentials",
        ))
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "the database topology is managed by administrators",
        ))
    }
}

impl DbApi {
    fn is_sqlite(&self) -> bool {
        self.app.db().kind() == BackendKind::Sqlite
    }

    /// The service, after the admin and backend checks.
    fn ops(&self, ctx: &AccessContext) -> ApiResult<&dyn TopologyOps> {
        require_admin(ctx)?;
        if self.is_sqlite() {
            return Err(TopologyError::postgres_required().into());
        }
        self.ops.as_deref().ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                codes::TOPOLOGY_UNAVAILABLE,
                "this server runs no topology service (the database is external, or it is starting)",
            )
        })
    }

    async fn audit(&self, ctx: &AccessContext, event: &str, detail: Json_) {
        if let Some(p) = self.app.policy()
            && let Err(e) = p.audit_event(ctx, event, detail).await
        {
            tracing::warn!(error = %e, event, "topology audit entry not written");
        }
    }

    /// Records the outcome of a change: `event` on success,
    /// `db.topology.refused` with the code otherwise.
    async fn audited<T>(&self, ctx: &AccessContext, event: &str, detail: Json_, r: ApiResult<T>) -> ApiResult<T> {
        match &r {
            Ok(_) => self.audit(ctx, event, detail).await,
            Err(e) => {
                let mut d = detail;
                if let Some(o) = d.as_object_mut() {
                    o.insert("action".into(), json!(event));
                    o.insert("code".into(), json!(e.code()));
                }
                self.audit(ctx, "db.topology.refused", d).await;
            }
        }
        r
    }

    /// MFA step-up (module docs).
    async fn step_up(&self, ctx: &AccessContext, caller: Option<&Authenticated>, what: &str) -> ApiResult<()> {
        if ctx.is_system() {
            return Ok(());
        }
        let (Some(auth), Some(caller)) = (self.app.auth(), caller) else {
            return Ok(());
        };
        let on = auth
            .mfa_enabled(&caller.subject)
            .await
            .map_err(|e| ApiError::from_error(&e))?;
        if !on {
            return Ok(());
        }
        let refuse = |why: &str| {
            ApiError::new(
                StatusCode::FORBIDDEN,
                codes::MFA_STEP_UP_REQUIRED,
                format!(
                    "{what} needs a recent second factor: {why} Sign in again with your authenticator, then retry within {} minutes.",
                    STEP_UP_SECS / 60
                ),
            )
        };
        let Some(sid) = caller.session_id() else {
            return Err(refuse("API keys and access tokens of an administrator with MFA cannot run it."));
        };
        if !caller.mfa {
            return Err(refuse("this session did not pass the second factor."));
        }
        let rows = self
            .app
            .db()
            .fetch_all(&forgebase_db::Statement::new(
                "SELECT created_at FROM _forgebase_auth_sessions WHERE id = ?1",
                vec![sid.to_owned().into()],
            ))
            .await
            .map_err(|e| ApiError::from_error(&e))?;
        let created = rows
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(forgebase_db::Value::as_i64)
            .unwrap_or(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));
        if now - created > STEP_UP_SECS {
            return Err(refuse("the session signed in more than 10 minutes ago."));
        }
        Ok(())
    }
}

fn caller(ext: Option<&Extension<Authenticated>>) -> Option<&Authenticated> {
    ext.map(|Extension(a)| a)
}

fn accepted(op: Operation) -> Response {
    (StatusCode::ACCEPTED, Json(Accepted { operation: op })).into_response()
}

fn query_pairs(raw: Option<&str>) -> Vec<(String, String)> {
    raw.map(|q| url::form_urlencoded::parse(q.as_bytes()).into_owned().collect())
        .unwrap_or_default()
}

fn query_value(raw: Option<&str>, key: &str) -> Option<String> {
    query_pairs(raw).into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn check_name(name: &str) -> ApiResult<()> {
    if forgebase_core::config::topology_config::is_node_name(name) {
        Ok(())
    } else {
        Err(ApiError::bad_request(
            "invalid_request",
            format!(
                "node name {name:?} must be 1 to 32 lowercase letters, digits, - or _, start with a letter or digit, and not be \"primary\", such as read-1"
            ),
        ))
    }
}

fn check_url(url: Option<&str>, what: &str) -> ApiResult<String> {
    match url.map(str::trim) {
        Some(u) if u.starts_with("postgres://") || u.starts_with("postgresql://") => Ok(u.to_owned()),
        _ => Err(ApiError::bad_request(
            "invalid_request",
            format!("{what} needs url, a PostgreSQL address such as postgres://forgebase:secret@db.example.com/forgebase"),
        )),
    }
}

/// The SQLite answer of `GET …/topology`: one server, nothing to change.
fn sqlite_view(app: &AppState) -> TopologyView {
    let now = forgebase_core::now_rfc3339();
    TopologyView {
        backend: "sqlite".into(),
        engine_version: None,
        epoch: 0,
        sets: vec![SetInfo {
            id: "control".into(),
            name: "control".into(),
            kind: "control".into(),
            number: 0,
            state: "active".into(),
            weight: 1,
            hosts_tenants: true,
            tenants: 0,
            est_bytes: 0,
        }],
        nodes: vec![NodeInfo {
            id: "sqlite".into(),
            set: "control".into(),
            name: "primary".into(),
            kind: "local".into(),
            role: "primary".into(),
            state: "ready".into(),
            weight: 1,
            engine_version: None,
            system_identifier: None,
            timeline: None,
            origin: "config".into(),
            in_rotation: None,
            lag_ms: None,
            lag_bytes: None,
            replay_lsn: None,
            last_error: None,
            agent: Some(json!({ "sqlite_file": app.db().location() })),
            created_at: now.clone(),
            updated_at: now,
        }],
        operations: Vec::new(),
        restart_required: None,
    }
}

/// Sets, nodes, health, lag, tenant counts, epoch and running operations.
#[utoipa::path(
    get, path = "/api/system/database/topology", operation_id = "database_topology", tag = "system",
    responses(
        (status = 200, description = "The topology (SQLite: one server)", body = TopologyView),
        (status = 401, description = "No credentials"), (status = 403, description = "Not an administrator"),
    )
)]
pub async fn topology(State(s): State<DbApi>, Access(ctx): Access) -> ApiResult<Response> {
    require_admin(&ctx)?;
    if s.is_sqlite() {
        return Ok(Json(sqlite_view(&s.app)).into_response());
    }
    let view = s.ops(&ctx)?.topology().await?;
    Ok(Json(view).into_response())
}

/// One node with its lag history (last hour, one-minute buckets).
#[utoipa::path(
    get, path = "/api/system/database/nodes/{id}", operation_id = "database_node", tag = "system",
    params(("id" = String, Path, description = "Node id")),
    responses((status = 200, description = "The node", body = NodeDetail), (status = 404, description = "node_not_found"))
)]
pub async fn node(State(s): State<DbApi>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult<Response> {
    let d = s.ops(&ctx)?.node(&id).await?;
    Ok(Json(d).into_response())
}

/// Adds a replica: `local` clones one on this server (`202` operation
/// `database.node.add`); `external` registers one you run (`201` node).
#[utoipa::path(
    post, path = "/api/system/database/nodes", operation_id = "database_node_add", tag = "system",
    request_body = AddNode,
    responses(
        (status = 202, description = "Clone started", body = Accepted),
        (status = 201, description = "External replica added", body = Added),
        (status = 400, description = "invalid_request, postgres_required, node_mismatch"),
        (status = 409, description = "node_exists, operation_in_progress"),
        (status = 503, description = "node_unreachable"),
    )
)]
pub async fn add_node(
    State(s): State<DbApi>,
    Access(ctx): Access,
    ApiJson(body): ApiJson<AddNode>,
) -> ApiResult<Response> {
    let ops = s.ops(&ctx)?;
    check_name(&body.name)?;
    let detail = json!({ "set": body.set, "name": body.name, "kind": body.kind });
    let r = async {
        match body.kind.as_str() {
            "local" => {}
            "external" => {
                check_url(body.url.as_deref(), "an external replica")?;
            }
            "remote" => {
                return Err(ApiError::bad_request(
                    "invalid_request",
                    "replicas on other hosts join with a token: POST /api/system/database/join-tokens, then run its command there",
                ));
            }
            other => {
                return Err(ApiError::bad_request(
                    "invalid_request",
                    format!("kind {other:?} must be local or external"),
                ));
            }
        }
        if body.weight.is_some_and(|w| w > 1_000) {
            return Err(ApiError::bad_request("invalid_request", "weight must be from 0 to 1000"));
        }
        Ok(ops.add_node(&ctx, body.clone()).await?)
    }
    .await;
    let added = s.audited(&ctx, "db.topology.node.add", detail, r).await?;
    Ok(match added.operation {
        Some(op) => accepted(op),
        None => (StatusCode::CREATED, Json(added)).into_response(),
    })
}

/// Changes a replica's weight (share of reads; 0 takes it out of rotation).
#[utoipa::path(
    patch, path = "/api/system/database/nodes/{id}", operation_id = "database_node_update", tag = "system",
    params(("id" = String, Path, description = "Node id")),
    request_body = UpdateNode,
    responses((status = 200, description = "The node", body = NodeInfo))
)]
pub async fn update_node(
    State(s): State<DbApi>,
    Access(ctx): Access,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateNode>,
) -> ApiResult<Response> {
    let ops = s.ops(&ctx)?;
    let detail = json!({ "node": id, "weight": body.weight });
    let r = async {
        if body.weight > 1_000 {
            return Err(ApiError::bad_request("invalid_request", "weight must be from 0 to 1000"));
        }
        Ok(ops.update_node(&id, body).await?)
    }
    .await;
    let n = s.audited(&ctx, "db.topology.node.update", detail, r).await?;
    Ok(Json(n).into_response())
}

/// Removes a replica (`?confirm=<name>`); its replication slot is dropped.
/// MFA step-up.
#[utoipa::path(
    delete, path = "/api/system/database/nodes/{id}", operation_id = "database_node_remove", tag = "system",
    params(("id" = String, Path, description = "Node id"), ("confirm" = String, Query, description = "The node name")),
    responses(
        (status = 200, description = "Removed", body = Removed),
        (status = 400, description = "confirm_mismatch, node_unsupported (a primary)"),
        (status = 403, description = "mfa_step_up_required"),
    )
)]
pub async fn remove_node(
    State(s): State<DbApi>,
    Access(ctx): Access,
    auth: Option<Extension<Authenticated>>,
    Path(id): Path<String>,
    RawQuery(q): RawQuery,
) -> ApiResult<Response> {
    let ops = s.ops(&ctx)?;
    let confirm = query_value(q.as_deref(), "confirm").unwrap_or_default();
    let detail = json!({ "node": id });
    let r = async {
        s.step_up(&ctx, caller(auth.as_ref()), "Removing a server").await?;
        Ok(ops.remove_node(&id, &confirm).await?)
    }
    .await;
    let out = s.audited(&ctx, "db.topology.node.remove", detail, r).await?;
    Ok(Json(out).into_response())
}

/// The promote dialog's estimate: the replica's position against the
/// primary's, and whether the primary answers.
#[utoipa::path(
    get, path = "/api/system/database/nodes/{id}/promote-check", operation_id = "database_node_promote_check", tag = "system",
    params(("id" = String, Path, description = "Node id")),
    responses((status = 200, description = "Estimate", body = PromoteCheck))
)]
pub async fn promote_check(
    State(s): State<DbApi>,
    Access(ctx): Access,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let c = s.ops(&ctx)?.promote_check(&id).await?;
    Ok(Json(c).into_response())
}

/// Promotes a replica to primary (`202` operation `database.promote`).
/// MFA step-up. After a promote of control's primary the server must be
/// restarted (`restart_required` in the topology).
#[utoipa::path(
    post, path = "/api/system/database/nodes/{id}/promote", operation_id = "database_node_promote", tag = "system",
    params(("id" = String, Path, description = "Node id")),
    request_body = PromoteBody,
    responses(
        (status = 202, description = "Promote started", body = Accepted),
        (status = 400, description = "confirm_mismatch"),
        (status = 403, description = "mfa_step_up_required"),
        (status = 409, description = "operation_in_progress, node_state"),
    )
)]
pub async fn promote(
    State(s): State<DbApi>,
    Access(ctx): Access,
    auth: Option<Extension<Authenticated>>,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<PromoteBody>,
) -> ApiResult<Response> {
    let ops = s.ops(&ctx)?;
    let detail = json!({ "node": id, "accept_data_loss": body.accept_data_loss });
    let r = async {
        s.step_up(&ctx, caller(auth.as_ref()), "Promoting a replica").await?;
        Ok(ops.promote(&ctx, &id, body).await?)
    }
    .await;
    let op = s.audited(&ctx, "db.topology.node.promote", detail, r).await?;
    Ok(accepted(op))
}

/// Rebuilds a replica (fresh clone), or rejoins an old primary that needs
/// a rebuild (`pg_rewind`, else a fresh clone): `202` operation
/// `database.node.rebuild`.
#[utoipa::path(
    post, path = "/api/system/database/nodes/{id}/rebuild", operation_id = "database_node_rebuild", tag = "system",
    params(("id" = String, Path, description = "Node id")),
    responses((status = 202, description = "Rebuild started", body = Accepted))
)]
pub async fn rebuild(State(s): State<DbApi>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult<Response> {
    let ops = s.ops(&ctx)?;
    let detail = json!({ "node": id });
    let r = async { Ok(ops.rebuild(&ctx, &id).await?) }.await;
    let op = s.audited(&ctx, "db.topology.node.rebuild", detail, r).await?;
    Ok(accepted(op))
}

fn server_url(s: &DbApi, headers: &HeaderMap) -> String {
    if let Some(u) = s
        .app
        .auth()
        .map(|a| a.config().public_url.clone())
        .filter(|u| !u.is_empty() && headers.get(axum::http::header::HOST).is_none())
    {
        return u;
    }
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("localhost");
    format!("http://{host}")
}

/// A one-time token (15 minutes) and the `forgebase node join` command
/// that adds a replica on another host. The token is shown once; the
/// audit log records its id.
#[utoipa::path(
    post, path = "/api/system/database/join-tokens", operation_id = "database_join_token", tag = "system",
    request_body = JoinTokenBody,
    responses((status = 201, description = "The token", body = JoinToken))
)]
pub async fn join_token(
    State(s): State<DbApi>,
    Access(ctx): Access,
    headers: HeaderMap,
    ApiJson(body): ApiJson<JoinTokenBody>,
) -> ApiResult<Response> {
    let ops = s.ops(&ctx)?;
    let url = server_url(&s, &headers);
    let r = async { Ok(ops.join_token(&ctx, body.clone(), &url).await?) }.await;
    let detail = json!({
        "set": body.set,
        "role": body.role,
        "token_id": r.as_ref().ok().map(|t| t.id.clone()),
    });
    let t = s.audited(&ctx, "db.topology.join_token", detail, r).await?;
    let mut res = (StatusCode::CREATED, Json(t)).into_response();
    res.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(res)
}

/// Adds a shard with its primary (`202` operation `database.shard.add`):
/// migrated, catalog replayed, subscribed to control, then `active`.
#[utoipa::path(
    post, path = "/api/system/database/shards", operation_id = "database_shard_add", tag = "system",
    request_body = AddShard,
    responses(
        (status = 202, description = "Shard add started", body = Accepted),
        (status = 400, description = "sharding_unsupported, invalid_request"),
        (status = 501, description = "local and remote shard primaries (FB-174)"),
    )
)]
pub async fn add_shard(
    State(s): State<DbApi>,
    Access(ctx): Access,
    ApiJson(body): ApiJson<AddShard>,
) -> ApiResult<Response> {
    let ops = s.ops(&ctx)?;
    let detail = json!({ "name": body.name, "node": body.node.name, "kind": body.node.kind });
    let r = async {
        let name = body.name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(ApiError::bad_request("invalid_request", "a shard needs a name of 1 to 64 characters, such as shard-2"));
        }
        check_name(&body.node.name)?;
        match body.node.kind.as_str() {
            "external" => {
                check_url(body.node.url.as_deref(), "an external shard primary")?;
            }
            "local" | "remote" => {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    codes::NOT_IMPLEMENTED,
                    "shard primaries on this server or another host come with FB-174; add an external one (a PostgreSQL server you run)",
                ));
            }
            other => {
                return Err(ApiError::bad_request(
                    "invalid_request",
                    format!("node kind {other:?} must be external"),
                ));
            }
        }
        if body.weight.is_some_and(|w| !(1..=1_000).contains(&w)) {
            return Err(ApiError::bad_request("invalid_request", "weight must be from 1 to 1000"));
        }
        Ok(ops.add_shard(&ctx, body.clone()).await?)
    }
    .await;
    let op = s.audited(&ctx, "db.topology.shard.add", detail, r).await?;
    Ok(accepted(op))
}

/// Changes a shard: `state` `active` or `draining` (takes no new tenants;
/// moving its tenants out comes with FB-174), `weight`, `hosts_tenants`.
#[utoipa::path(
    patch, path = "/api/system/database/shards/{id}", operation_id = "database_shard_update", tag = "system",
    params(("id" = String, Path, description = "Set id")),
    request_body = UpdateShard,
    responses((status = 200, description = "The shard", body = SetInfo))
)]
pub async fn update_shard(
    State(s): State<DbApi>,
    Access(ctx): Access,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateShard>,
) -> ApiResult<Response> {
    let ops = s.ops(&ctx)?;
    let detail = json!({ "set": id, "state": body.state, "weight": body.weight, "hosts_tenants": body.hosts_tenants });
    let r = async {
        if let Some(st) = body.state.as_deref()
            && !matches!(st, "active" | "draining")
        {
            return Err(ApiError::bad_request(
                "invalid_request",
                format!("state {st:?} must be active or draining"),
            ));
        }
        if body.weight.is_some_and(|w| !(1..=1_000).contains(&w)) {
            return Err(ApiError::bad_request("invalid_request", "weight must be from 1 to 1000"));
        }
        if body == UpdateShard::default() {
            return Err(ApiError::bad_request(
                "invalid_request",
                "send state, weight or hosts_tenants",
            ));
        }
        Ok(ops.update_shard(&id, body.clone()).await?)
    }
    .await;
    let set = s.audited(&ctx, "db.topology.shard.update", detail, r).await?;
    Ok(Json(set).into_response())
}

/// Removes an empty shard (`?confirm=<name>`; `409 shard_not_empty`).
/// MFA step-up.
#[utoipa::path(
    delete, path = "/api/system/database/shards/{id}", operation_id = "database_shard_remove", tag = "system",
    params(("id" = String, Path, description = "Set id"), ("confirm" = String, Query, description = "The shard name")),
    responses((status = 200, description = "Removed", body = Removed), (status = 409, description = "shard_not_empty"))
)]
pub async fn remove_shard(
    State(s): State<DbApi>,
    Access(ctx): Access,
    auth: Option<Extension<Authenticated>>,
    Path(id): Path<String>,
    RawQuery(q): RawQuery,
) -> ApiResult<Response> {
    let ops = s.ops(&ctx)?;
    let confirm = query_value(q.as_deref(), "confirm").unwrap_or_default();
    let detail = json!({ "set": id });
    let r = async {
        s.step_up(&ctx, caller(auth.as_ref()), "Removing a shard").await?;
        Ok(ops.remove_shard(&id, &confirm).await?)
    }
    .await;
    let out = s.audited(&ctx, "db.topology.shard.remove", detail, r).await?;
    Ok(Json(out).into_response())
}

/// Tenants with their set, project, size estimate and state; filters
/// `set`, `project`, `q`; `cursor` and `limit` pages.
#[utoipa::path(
    get, path = "/api/system/database/placements", operation_id = "database_placements", tag = "system",
    params(
        ("set" = Option<String>, Query, description = "Set id"),
        ("project" = Option<String>, Query, description = "Project id"),
        ("q" = Option<String>, Query, description = "Tenant id contains"),
        ("cursor" = Option<String>, Query, description = "next_cursor of the previous page"),
        ("limit" = Option<u32>, Query, description = "1 to 200 (50)"),
    ),
    responses((status = 200, description = "A page", body = PlacementPage))
)]
pub async fn placements(State(s): State<DbApi>, Access(ctx): Access, RawQuery(q): RawQuery) -> ApiResult<Response> {
    let ops = s.ops(&ctx)?;
    let raw = q.as_deref();
    let limit = match query_value(raw, "limit") {
        Some(v) => Some(
            v.parse::<u32>()
                .ok()
                .filter(|n| (1..=200).contains(n))
                .ok_or_else(|| ApiError::bad_request("invalid_request", "limit must be from 1 to 200"))?,
        ),
        None => None,
    };
    let query = PlacementQuery {
        set: query_value(raw, "set").filter(|v| !v.is_empty()),
        project: query_value(raw, "project").filter(|v| !v.is_empty()),
        q: query_value(raw, "q").filter(|v| !v.is_empty()),
        cursor: query_value(raw, "cursor").filter(|v| !v.is_empty()),
        limit,
    };
    let page = ops.placements(query).await?;
    Ok(Json(page).into_response())
}

fn not_yet(what: &str) -> ApiError {
    ApiError::new(
        StatusCode::NOT_IMPLEMENTED,
        codes::NOT_IMPLEMENTED,
        format!("{what} comes with FB-174 (online tenant moves); nothing was changed"),
    )
}

/// Moves a tenant to another set (`{to, confirm}`, job
/// `database.tenant.move`). MFA step-up. `501` until FB-174.
#[utoipa::path(
    post, path = "/api/system/database/placements/{tenant}/move", operation_id = "database_tenant_move", tag = "system",
    params(("tenant" = String, Path, description = "Tenant id")),
    request_body = MoveBody,
    responses((status = 501, description = "not_implemented (FB-174)"), (status = 409, description = "tenant_moving"))
)]
pub async fn move_tenant(
    State(s): State<DbApi>,
    Access(ctx): Access,
    auth: Option<Extension<Authenticated>>,
    Path(tenant): Path<String>,
    ApiJson(_body): ApiJson<MoveBody>,
) -> ApiResult<Response> {
    s.ops(&ctx)?;
    let detail = json!({ "tenant": tenant });
    let r: ApiResult<()> = async {
        s.step_up(&ctx, caller(auth.as_ref()), "Moving a tenant").await?;
        Err(not_yet("Moving a tenant"))
    }
    .await;
    s.audited(&ctx, "db.topology.tenant.move", detail, r).await?;
    Ok(StatusCode::NOT_IMPLEMENTED.into_response())
}

/// A rebalance plan (moves, sizes, freeze estimates). `501` until FB-174.
#[utoipa::path(
    post, path = "/api/system/database/rebalance/plan", operation_id = "database_rebalance_plan", tag = "system",
    request_body = RebalancePlanBody,
    responses((status = 501, description = "not_implemented (FB-174)"))
)]
pub async fn rebalance_plan(
    State(s): State<DbApi>,
    Access(ctx): Access,
    ApiJson(_body): ApiJson<RebalancePlanBody>,
) -> ApiResult<Response> {
    s.ops(&ctx)?;
    Err(not_yet("Planning a rebalance"))
}

/// Runs a rebalance plan (`{plan_id, confirm}`, job `database.rebalance`).
/// MFA step-up. `501` until FB-174.
#[utoipa::path(
    post, path = "/api/system/database/rebalance", operation_id = "database_rebalance", tag = "system",
    request_body = RebalanceBody,
    responses((status = 501, description = "not_implemented (FB-174)"))
)]
pub async fn rebalance(
    State(s): State<DbApi>,
    Access(ctx): Access,
    auth: Option<Extension<Authenticated>>,
    ApiJson(body): ApiJson<RebalanceBody>,
) -> ApiResult<Response> {
    s.ops(&ctx)?;
    let detail = json!({ "plan_id": body.plan_id });
    let r: ApiResult<()> = async {
        s.step_up(&ctx, caller(auth.as_ref()), "Running a rebalance").await?;
        Err(not_yet("Running a rebalance"))
    }
    .await;
    s.audited(&ctx, "db.topology.rebalance", detail, r).await?;
    Ok(StatusCode::NOT_IMPLEMENTED.into_response())
}

/// Topology operations, newest first (`?active=1`: queued and running only).
#[utoipa::path(
    get, path = "/api/system/database/operations", operation_id = "database_operations", tag = "system",
    params(("active" = Option<bool>, Query, description = "Queued and running only"), ("limit" = Option<u32>, Query, description = "1 to 200 (50)")),
    responses((status = 200, description = "Operations", body = OperationList))
)]
pub async fn operations(State(s): State<DbApi>, Access(ctx): Access, RawQuery(q): RawQuery) -> ApiResult<Response> {
    let ops = s.ops(&ctx)?;
    let raw = q.as_deref();
    let active = query_value(raw, "active").is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes"));
    let limit = query_value(raw, "limit").and_then(|v| v.parse::<u32>().ok()).map(|n| n.clamp(1, 200));
    let list = ops.operations(&ctx, OperationQuery { active, limit }).await?;
    Ok(Json(list).into_response())
}

/// One operation.
#[utoipa::path(
    get, path = "/api/system/database/operations/{id}", operation_id = "database_operation", tag = "system",
    params(("id" = String, Path, description = "Operation (job) id")),
    responses((status = 200, description = "The operation", body = Operation), (status = 404, description = "operation_not_found"))
)]
pub async fn operation(State(s): State<DbApi>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult<Response> {
    let op = s.ops(&ctx)?.operation(&ctx, &id).await?;
    Ok(Json(op).into_response())
}

/// Cancels an operation at its next safe point (`409
/// operation_not_cancellable` past it, for example after a promote's fence).
#[utoipa::path(
    post, path = "/api/system/database/operations/{id}/cancel", operation_id = "database_operation_cancel", tag = "system",
    params(("id" = String, Path, description = "Operation (job) id")),
    responses((status = 200, description = "Cancel requested", body = Operation))
)]
pub async fn cancel_operation(
    State(s): State<DbApi>,
    Access(ctx): Access,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let ops = s.ops(&ctx)?;
    let detail = json!({ "operation": id });
    let r = async { Ok(ops.cancel_operation(&ctx, &id).await?) }.await;
    let op = s.audited(&ctx, "db.topology.operation.cancel", detail, r).await?;
    Ok(Json(op).into_response())
}

// ---------------------------------------------------------------------------
// Agent routes (no administrator session).
// ---------------------------------------------------------------------------

fn agent_ops(s: &DbApi) -> ApiResult<&dyn TopologyOps> {
    if s.is_sqlite() {
        return Err(TopologyError::postgres_required().into());
    }
    s.ops.as_deref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            codes::TOPOLOGY_UNAVAILABLE,
            "this server runs no topology service",
        )
    })
}

fn bearer(headers: &HeaderMap) -> ApiResult<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|k| k.trim().to_owned())
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "agent_unauthorized",
                "send the agent key as Authorization: Bearer <key>",
            )
        })
}

/// The peer address of a request: the connection's, or the last
/// `X-Forwarded-For` entry when `[auth] trust_forwarded_for` is on.
fn peer(s: &DbApi, headers: &HeaderMap, connect: Option<SocketAddr>) -> Option<IpAddr> {
    let trust = s.app.auth().is_some_and(|a| a.config().trust_forwarded_for);
    if trust
        && let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
    {
        return Some(ip);
    }
    connect.map(|a| a.ip())
}

/// `forgebase node join`: redeems a join token (`401 join_token_invalid`,
/// `409 engine_mismatch`) and returns the agent's grant.
#[utoipa::path(
    post, path = "/api/system/database/agent/join", operation_id = "database_agent_join", tag = "system",
    request_body = Object,
    responses((status = 200, description = "The grant (agent key, replication login)", body = Object))
)]
pub async fn agent_join(
    State(s): State<DbApi>,
    connect: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<Json_>,
) -> ApiResult<Response> {
    let ops = agent_ops(&s)?;
    let ip = peer(&s, &headers, connect.map(|Extension(c)| c.0));
    let grant = ops.agent_join(body, ip).await?;
    let mut res = Json(grant).into_response();
    res.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    Ok(res)
}

/// The agent's long-poll for commands (`?wait_ms=`, `resend=1`).
#[utoipa::path(
    get, path = "/api/system/database/agent/nodes/{id}/commands", operation_id = "database_agent_commands", tag = "system",
    params(("id" = String, Path, description = "Node id"), ("wait_ms" = Option<u64>, Query, description = "Longest wait"), ("resend" = Option<u8>, Query, description = "1: also commands sent but never answered")),
    responses((status = 200, description = "Commands", body = Object))
)]
pub async fn agent_commands(
    State(s): State<DbApi>,
    headers: HeaderMap,
    Path(id): Path<String>,
    RawQuery(q): RawQuery,
) -> ApiResult<Response> {
    let ops = agent_ops(&s)?;
    let key = bearer(&headers)?;
    let raw = q.as_deref();
    let wait = query_value(raw, "wait_ms").and_then(|v| v.parse::<u64>().ok());
    let resend = query_value(raw, "resend").is_some_and(|v| v == "1" || v == "true");
    let out = ops.agent_commands(&id, &key, wait, resend).await?;
    Ok(Json(out).into_response())
}

/// The agent's heartbeat (state, replay position, free disk, command answers).
#[utoipa::path(
    post, path = "/api/system/database/agent/nodes/{id}/heartbeat", operation_id = "database_agent_heartbeat", tag = "system",
    params(("id" = String, Path, description = "Node id")),
    request_body = Object,
    responses((status = 200, description = "Reply", body = Object))
)]
pub async fn agent_heartbeat(
    State(s): State<DbApi>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<Json_>,
) -> ApiResult<Response> {
    let ops = agent_ops(&s)?;
    let key = bearer(&headers)?;
    let out = ops.agent_heartbeat(&id, &key, body).await?;
    Ok(Json(out).into_response())
}

/// The OpenAPI fragment of these routes.
#[derive(OpenApi)]
#[openapi(
    paths(
        topology, node, add_node, update_node, remove_node, promote_check, promote, rebuild, join_token,
        add_shard, update_shard, remove_shard, placements, move_tenant, rebalance_plan, rebalance,
        operations, operation, cancel_operation, agent_join, agent_commands, agent_heartbeat,
    ),
    components(schemas(
        TopologyView, SetInfo, NodeInfo, NodeDetail, LagSample, AddNode, Added, UpdateNode, Removed, PromoteBody,
        PromoteCheck, JoinTokenBody, JoinToken, ShardNode, AddShard, UpdateShard, PlacementInfo, PlacementPage,
        OperationProgress, Operation, OperationList, MoveBody, RebalancePlanBody, RebalanceBody, Accepted,
    ))
)]
pub struct DatabaseApiDoc;

/// [`DatabaseApiDoc`] as a document.
pub fn openapi() -> utoipa::openapi::OpenApi {
    DatabaseApiDoc::openapi()
}
