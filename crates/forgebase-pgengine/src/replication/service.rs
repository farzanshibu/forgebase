//! The managed-replica service (FB-168): what the admin API
//! (`/api/system/database/*`, FB-175), the dashboard (FB-176), the
//! `database.node.*` / `database.promote` jobs and the `forgebase node`
//! agent endpoints call. Every function takes the control database and the
//! engine handles it needs; none keeps state of its own.
//!
//! - Local replicas: [`add_local_replica`], [`rebuild_replica`],
//!   [`remove_replica`], [`promote_local`], [`rejoin_local`].
//! - Remote replicas (`forgebase node`, ADR-0029 §2.3): [`issue_join_token`]
//!   → [`redeem_join`] (the agent's first call) → [`poll_commands`] (its
//!   long-poll) and [`heartbeat`] (every 5 s); [`queue_command`] sends it
//!   `rebuild`, `stop`, `promote`, `rejoin` or `follow`.
//! - Read side: [`node_view`] / [`nodes`] (never `url_sealed`), [`slot_health`].
//!
//! Errors carry an HTTP status and a stable code ([`ServiceError::code`]).

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forgebase_db::topology::{self, DbNode, NodeKind, NodeOrigin, NodeRole, NodeState, Topology};
use forgebase_db::{Backend, Database, DbError, Statement, TxMode, Value, WriteTx};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use super::{EngineIdentity, Lsn, SlotInfo};
use crate::error::EngineError;
use crate::layout::{Layout, NodeKind as LayoutKind};
use crate::secret::{Role, Secret};
use crate::standby::{self, LocalReplicas, OldPrimary, PromoteReport, RejoinHow, SslMode, Upstream};
use crate::{Engine, EngineHandle, fsutil};

/// Lifetime of a join token (ADR-0029 §2.3).
pub const JOIN_TOKEN_TTL: Duration = Duration::from_secs(15 * 60);
/// How often an agent sends a heartbeat (ADR-0029 §2.3).
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// Longest wait of one command long-poll.
pub const MAX_POLL_WAIT: Duration = Duration::from_secs(30);
/// Prefix of join tokens.
pub const JOIN_TOKEN_PREFIX: &str = "fbj_";
/// Prefix of agent keys.
pub const AGENT_KEY_PREFIX: &str = "fba_";

/// Paths of the agent endpoints the server mounts (FB-175 wires them; the
/// agent in `forgebase node` calls them). `{id}` is the node id.
pub mod paths {
    /// `POST` [`super::JoinRequest`] → [`super::JoinGrant`] (no other auth: the token is it).
    pub const JOIN: &str = "/api/system/database/agent/join";
    /// `GET ?wait_ms=&resend=1` → [`super::CommandBatch`] (`Authorization:
    /// Bearer <agent key>`); `resend=1` (the agent's first poll after a
    /// start) also returns commands sent before but never answered.
    pub const COMMANDS: &str = "/api/system/database/agent/nodes/{id}/commands";
    /// `POST` [`super::Heartbeat`] → [`super::HeartbeatReply`] (bearer agent key).
    pub const HEARTBEAT: &str = "/api/system/database/agent/nodes/{id}/heartbeat";
}

/// Errors of the service, with their HTTP status.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// 404 `node_not_found`.
    #[error("node {0} not found")]
    NodeNotFound(String),
    /// 404 `shard_not_found`.
    #[error("set {0} not found")]
    SetNotFound(String),
    /// 400 `confirm_mismatch`: the typed confirmation is not the node name.
    #[error("type the node name {0:?} to confirm")]
    ConfirmMismatch(String),
    /// 400 `invalid_request`.
    #[error("{0}")]
    Invalid(String),
    /// 400 `node_unsupported`: not possible for this kind of node.
    #[error("{0}")]
    Unsupported(String),
    /// 400 `postgres_required`.
    #[error("replicas need PostgreSQL; this server runs on SQLite")]
    PostgresRequired,
    /// 409 `node_exists`.
    #[error("a node named {0} already exists")]
    NodeExists(String),
    /// 409 `node_state`: the node's state does not allow the operation.
    #[error("{0}")]
    State(String),
    /// 409 `data_loss_unconfirmed`.
    #[error("{0}")]
    DataLoss(String),
    /// 409 `engine_mismatch`.
    #[error("{0}")]
    EngineMismatch(String),
    /// 401 `join_token_invalid`: unknown, used or expired.
    #[error("the join token is unknown, used or expired; create a new one")]
    TokenInvalid,
    /// 401 `agent_unauthorized`.
    #[error("the agent key does not match this node")]
    AgentUnauthorized,
    /// 503 `node_unreachable`.
    #[error("{0}")]
    Unreachable(String),
    /// 500 `engine_error`.
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// The database's own error (its code and status).
    #[error(transparent)]
    Db(#[from] DbError),
}

impl ServiceError {
    /// Stable error code.
    pub fn code(&self) -> &'static str {
        match self {
            ServiceError::NodeNotFound(_) => "node_not_found",
            ServiceError::SetNotFound(_) => "shard_not_found",
            ServiceError::ConfirmMismatch(_) => "confirm_mismatch",
            ServiceError::Invalid(_) => "invalid_request",
            ServiceError::Unsupported(_) => "node_unsupported",
            ServiceError::PostgresRequired => "postgres_required",
            ServiceError::NodeExists(_) => "node_exists",
            ServiceError::State(_) => "node_state",
            ServiceError::DataLoss(_) => "data_loss_unconfirmed",
            ServiceError::EngineMismatch(_) => "engine_mismatch",
            ServiceError::TokenInvalid => "join_token_invalid",
            ServiceError::AgentUnauthorized => "agent_unauthorized",
            ServiceError::Unreachable(_) => "node_unreachable",
            ServiceError::Engine(_) => "engine_error",
            ServiceError::Db(_) => "database_error",
        }
    }

    /// HTTP status.
    pub fn status(&self) -> u16 {
        match self {
            ServiceError::NodeNotFound(_) | ServiceError::SetNotFound(_) => 404,
            ServiceError::ConfirmMismatch(_)
            | ServiceError::Invalid(_)
            | ServiceError::Unsupported(_)
            | ServiceError::PostgresRequired => 400,
            ServiceError::NodeExists(_)
            | ServiceError::State(_)
            | ServiceError::DataLoss(_)
            | ServiceError::EngineMismatch(_) => 409,
            ServiceError::TokenInvalid | ServiceError::AgentUnauthorized => 401,
            ServiceError::Unreachable(_) => 503,
            ServiceError::Engine(_) | ServiceError::Db(_) => 500,
        }
    }
}

/// Result of the service.
pub type ServiceResult<T> = std::result::Result<T, ServiceError>;

fn sha256_hex(s: &str) -> String {
    fsutil::hex(&sha2::Sha256::digest(s.as_bytes()))
}

fn random_secret(prefix: &str) -> ServiceResult<String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| EngineError::Config(format!("system random generator failed: {e}")))?;
    Ok(format!("{prefix}{}", fsutil::hex(&buf)))
}

fn new_id(prefix: char) -> String {
    format!("{prefix}{}", &uuid::Uuid::new_v4().simple().to_string()[..14])
}

fn now() -> String {
    forgebase_core::now_rfc3339()
}

fn parse_time(s: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
}

fn require_pg(control: &dyn Backend) -> ServiceResult<()> {
    if control.dialect().name() == "postgres" {
        Ok(())
    } else {
        Err(ServiceError::PostgresRequired)
    }
}

/// Node names follow [`Layout::node`]'s rules (they name directories and slots).
fn check_name(name: &str) -> ServiceResult<()> {
    Layout::node("/", name, LayoutKind::Standby)
        .map(|_| ())
        .map_err(|e| ServiceError::Invalid(e.to_string()))
}

async fn node_by_id(control: &dyn Backend, id: &str) -> ServiceResult<DbNode> {
    topology::load(control)
        .await?
        .nodes
        .into_iter()
        .find(|n| n.id == id && n.state != NodeState::Retired)
        .ok_or_else(|| ServiceError::NodeNotFound(id.to_owned()))
}

async fn commit(tx: Box<dyn WriteTx>) -> ServiceResult<()> {
    tx.commit().await.map_err(ServiceError::from)
}

/// Sets a node's role, state and engine facts in one transaction (bumps the epoch).
async fn set_node(
    tx: &mut dyn WriteTx,
    id: &str,
    role: Option<NodeRole>,
    state: NodeState,
    facts: Option<&NodeFacts>,
) -> ServiceResult<()> {
    let r = tx
        .execute(&Statement::new(
            "UPDATE _forgebase_db_nodes SET role = COALESCE(?2, role), state = ?3, \
             engine_version = COALESCE(?4, engine_version), system_identifier = COALESCE(?5, system_identifier), \
             timeline = COALESCE(?6, timeline), updated_at = ?7 WHERE id = ?1",
            vec![
                id.into(),
                role.map(NodeRole::as_str).into(),
                state.as_str().into(),
                facts.and_then(|f| f.engine_version.clone()).into(),
                facts.and_then(|f| f.system_identifier.clone()).into(),
                facts.and_then(|f| f.timeline).into(),
                now().into(),
            ],
        ))
        .await?;
    if r.rows_affected == 0 {
        return Err(ServiceError::NodeNotFound(id.to_owned()));
    }
    Ok(())
}

async fn update_node(
    control: &dyn Backend,
    id: &str,
    role: Option<NodeRole>,
    state: NodeState,
    facts: Option<&NodeFacts>,
) -> ServiceResult<()> {
    let mut tx = control.begin(TxMode::Normal).await?;
    set_node(tx.as_mut(), id, role, state, facts).await?;
    topology::bump_epoch(tx.as_mut()).await?;
    commit(tx).await
}

async fn insert_node_tx(tx: &mut dyn WriteTx, new: &topology::NewNode) -> ServiceResult<String> {
    let id = new_id('n');
    let ts = now();
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_db_nodes (id, set_id, name, kind, role, url_sealed, state, weight, origin, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
        vec![
            id.clone().into(),
            new.set_id.clone().into(),
            new.name.clone().into(),
            new.kind.as_str().into(),
            new.role.as_str().into(),
            new.url_sealed.clone().into(),
            new.state.as_str().into(),
            i64::from(new.weight).into(),
            new.origin.as_str().into(),
            ts.into(),
        ],
    ))
    .await?;
    Ok(id)
}

/// Engine facts stored on a node row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeFacts {
    /// [`EngineIdentity::label`].
    pub engine_version: Option<String>,
    /// `pg_control_system().system_identifier`.
    pub system_identifier: Option<String>,
    /// Timeline.
    pub timeline: Option<i64>,
}

async fn facts_of(handle: &EngineHandle) -> NodeFacts {
    let engine_version = Some(EngineIdentity::of(handle.engine()).label());
    match standby::facts(handle).await {
        Ok(f) => NodeFacts {
            engine_version,
            system_identifier: Some(f.system_identifier),
            timeline: f.timeline,
        },
        Err(_) => NodeFacts {
            engine_version,
            ..NodeFacts::default()
        },
    }
}

async fn ensure_unused_name(control: &dyn Backend, name: &str) -> ServiceResult<Topology> {
    let topo = topology::load(control).await?;
    if topo.nodes.iter().any(|n| n.name.eq_ignore_ascii_case(name)) || name == standby::OLD_PRIMARY {
        return Err(ServiceError::NodeExists(name.to_owned()));
    }
    Ok(topo)
}

// ---------------------------------------------------------------------------
// Read side (FB-175 / FB-176).
// ---------------------------------------------------------------------------

/// A node as the admin API shows it: the stored row without `url_sealed`,
/// plus the agent's last heartbeat for remote nodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeView {
    /// Id.
    pub id: String,
    /// Set.
    pub set_id: String,
    /// Name.
    pub name: String,
    /// `local`, `remote`, `external`.
    pub kind: NodeKind,
    /// `primary` or `replica`.
    pub role: NodeRole,
    /// Stored state.
    pub state: NodeState,
    /// Read weight.
    pub weight: u32,
    /// Engine label (`18.6-r1`).
    pub engine_version: Option<String>,
    /// System identifier.
    pub system_identifier: Option<String>,
    /// Timeline.
    pub timeline: Option<i64>,
    /// `api` or `config` (read-only in the dashboard).
    pub origin: NodeOrigin,
    /// Last agent heartbeat (remote nodes).
    pub agent: Option<AgentView>,
    /// Created.
    pub created_at: String,
    /// Updated.
    pub updated_at: String,
}

/// The last heartbeat of a remote node's agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentView {
    /// Agent state.
    pub state: AgentState,
    /// Address the agent called from.
    pub address: Option<String>,
    /// Engine label the agent reported.
    pub engine_version: String,
    /// Replay position.
    pub replay_lsn: Option<Lsn>,
    /// Free bytes in its data directory.
    pub disk_free: Option<u64>,
    /// Last heartbeat.
    pub last_seen_at: Option<String>,
}

fn view(n: &DbNode, agent: Option<AgentView>) -> NodeView {
    NodeView {
        id: n.id.clone(),
        set_id: n.set_id.clone(),
        name: n.name.clone(),
        kind: n.kind,
        role: n.role,
        state: n.state,
        weight: n.weight,
        engine_version: n.engine_version.clone(),
        system_identifier: n.system_identifier.clone(),
        timeline: n.timeline,
        origin: n.origin,
        agent,
        created_at: n.created_at.clone(),
        updated_at: n.updated_at.clone(),
    }
}

async fn agent_view(control: &dyn Backend, node_id: &str) -> ServiceResult<Option<AgentView>> {
    let rows = control
        .fetch_all(&Statement::new(
            "SELECT state, address, engine_version, replay_lsn, disk_free, last_seen_at \
             FROM _forgebase_db_node_agents WHERE node_id = ?1",
            vec![node_id.into()],
        ))
        .await?;
    let Some(r) = rows.rows.first() else {
        return Ok(None);
    };
    let text = |i: usize| r.get(i).and_then(Value::as_str).map(str::to_owned);
    Ok(Some(AgentView {
        state: text(0)
            .and_then(|s| AgentState::parse(&s))
            .unwrap_or(AgentState::Joining),
        address: text(1),
        engine_version: text(2).unwrap_or_default(),
        replay_lsn: text(3).and_then(|s| s.parse().ok()),
        disk_free: r.get(4).and_then(Value::as_i64).and_then(|v| u64::try_from(v).ok()),
        last_seen_at: text(5),
    }))
}

/// Every node that is not retired, optionally of one set.
///
/// # Errors
/// Database errors.
pub async fn nodes(control: &dyn Backend, set_id: Option<&str>) -> ServiceResult<Vec<NodeView>> {
    let topo = topology::load(control).await?;
    let mut out = Vec::new();
    for n in topo
        .nodes
        .iter()
        .filter(|n| n.state != NodeState::Retired && set_id.is_none_or(|s| n.set_id == s))
    {
        let agent = if n.kind == NodeKind::Remote {
            agent_view(control, &n.id).await?
        } else {
            None
        };
        out.push(view(n, agent));
    }
    Ok(out)
}

/// One node.
///
/// # Errors
/// Unknown node, or database errors.
pub async fn node_view(control: &dyn Backend, id: &str) -> ServiceResult<NodeView> {
    let n = node_by_id(control, id).await?;
    let agent = if n.kind == NodeKind::Remote {
        agent_view(control, id).await?
    } else {
        None
    };
    Ok(view(&n, agent))
}

/// The slots on `primary` with ForgeBase's prefix, each with
/// [`SlotInfo::needs_rebuild`] (the health monitor marks those nodes).
///
/// # Errors
/// SQL errors.
pub async fn slot_health(primary: &EngineHandle) -> ServiceResult<Vec<SlotInfo>> {
    let mut c = super::connect(&primary.url_for(Role::Super, "postgres")).await?;
    Ok(super::slots(&mut c)
        .await?
        .into_iter()
        .filter(|s| s.name.starts_with(super::SLOT_PREFIX))
        .collect())
}

/// Marks every node whose slot on `primary` was invalidated (past
/// `max_slot_wal_keep_size` or the idle timeout) `needs_rebuild`. Returns
/// the nodes it changed. Run it with the health monitor (every minute is
/// enough: invalidation is final).
///
/// # Errors
/// SQL or database errors.
pub async fn mark_lost_slots(
    control: &dyn Backend,
    set_id: &str,
    primary: &EngineHandle,
) -> ServiceResult<Vec<String>> {
    let lost: Vec<String> = slot_health(primary)
        .await?
        .into_iter()
        .filter(SlotInfo::needs_rebuild)
        .map(|s| s.name)
        .collect();
    if lost.is_empty() {
        return Ok(Vec::new());
    }
    let topo = topology::load(control).await?;
    let mut changed = Vec::new();
    for n in topo.nodes_of(set_id) {
        if n.role == NodeRole::Replica
            && n.kind != NodeKind::External
            && !matches!(
                n.state,
                NodeState::NeedsRebuild | NodeState::Retired | NodeState::Provisioning
            )
            && lost.contains(&super::slot_name(&n.name))
        {
            topology::update_node_state(control, &n.id, NodeState::NeedsRebuild).await?;
            changed.push(n.name.clone());
        }
    }
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Local replicas (ADR-0029 §2.2).
// ---------------------------------------------------------------------------

/// `POST /api/system/database/nodes {set, name, kind: "local"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddLocalReplica {
    /// The set (`control` for the local primary).
    pub set_id: String,
    /// Node name (`a-z0-9-_`, 1-32).
    pub name: String,
    /// Share of replica reads (default 1).
    #[serde(default = "one")]
    pub weight: u32,
}

fn one() -> u32 {
    1
}

/// Job `database.node.add` for a local replica: the node row
/// (`provisioning`), slot, clone and start, then `catching_up` with the
/// engine facts. The router opens it on its next `reload()`. A failed clone
/// leaves the row `needs_rebuild` (rebuild or remove it).
///
/// # Errors
/// Invalid or used name, SQLite, or a provisioning step.
pub async fn add_local_replica(
    control: &dyn Backend,
    replicas: &LocalReplicas,
    primary: &EngineHandle,
    req: &AddLocalReplica,
) -> ServiceResult<NodeView> {
    require_pg(control)?;
    check_name(&req.name)?;
    let topo = ensure_unused_name(control, &req.name).await?;
    if topo.set(&req.set_id).is_none() {
        return Err(ServiceError::SetNotFound(req.set_id.clone()));
    }
    let node = topology::insert_node(
        control,
        &topology::NewNode {
            set_id: req.set_id.clone(),
            name: req.name.clone(),
            kind: NodeKind::Local,
            role: NodeRole::Replica,
            url_sealed: None,
            state: NodeState::Provisioning,
            weight: req.weight,
            origin: NodeOrigin::Api,
        },
    )
    .await?;
    match replicas.add(primary, &req.name).await {
        Ok(h) => {
            let facts = facts_of(&h).await;
            update_node(control, &node.id, None, NodeState::CatchingUp, Some(&facts)).await?;
            node_view(control, &node.id).await
        }
        Err(e) => {
            let _ = topology::update_node_state(control, &node.id, NodeState::NeedsRebuild).await;
            Err(e.into())
        }
    }
}

/// Job `database.node.rebuild`: re-clone a local replica, or send a remote
/// one's agent `rebuild`. External replicas are the operator's.
///
/// # Errors
/// Unknown node, an external or primary node, or a provisioning step.
pub async fn rebuild_replica(
    control: &dyn Backend,
    replicas: &LocalReplicas,
    primary: Option<&EngineHandle>,
    node_id: &str,
) -> ServiceResult<NodeView> {
    require_pg(control)?;
    let node = node_by_id(control, node_id).await?;
    if node.role != NodeRole::Replica {
        return Err(ServiceError::Unsupported(format!(
            "{} is the primary; promote another node first",
            node.name
        )));
    }
    match node.kind {
        NodeKind::External => Err(ServiceError::Unsupported(format!(
            "{} is an external replica: ForgeBase does not rebuild it",
            node.name
        ))),
        NodeKind::Remote => {
            queue_command(control, node_id, CommandKind::Rebuild, serde_json::json!({}), None).await?;
            update_node(control, node_id, None, NodeState::Provisioning, None).await?;
            node_view(control, node_id).await
        }
        NodeKind::Local => {
            let primary = primary.ok_or_else(|| ServiceError::Unreachable("the primary is not running here".into()))?;
            update_node(control, node_id, None, NodeState::Provisioning, None).await?;
            match replicas.rebuild(primary, &node.name).await {
                Ok(h) => {
                    let facts = facts_of(&h).await;
                    update_node(control, node_id, None, NodeState::CatchingUp, Some(&facts)).await?;
                    node_view(control, node_id).await
                }
                Err(e) => {
                    let _ = topology::update_node_state(control, node_id, NodeState::NeedsRebuild).await;
                    Err(e.into())
                }
            }
        }
    }
}

/// `DELETE /api/system/database/nodes/{id}?confirm=<name>`: stops a local
/// replica and deletes its directory, or tells a remote agent to stop;
/// drops the slot on the primary; retires the row (its name is freed:
/// renamed `<name>~<id>`).
///
/// # Errors
/// Unknown node, a primary, or the confirmation does not match.
pub async fn remove_replica(
    control: &dyn Backend,
    replicas: &LocalReplicas,
    primary: Option<&EngineHandle>,
    node_id: &str,
    confirm: &str,
) -> ServiceResult<()> {
    let node = node_by_id(control, node_id).await?;
    if confirm != node.name {
        return Err(ServiceError::ConfirmMismatch(node.name));
    }
    if node.role == NodeRole::Primary {
        return Err(ServiceError::Unsupported(format!(
            "{} is the primary of {}; promote another node first",
            node.name, node.set_id
        )));
    }
    match node.kind {
        NodeKind::Local => replicas.remove(primary, &node.name).await?,
        NodeKind::Remote => {
            queue_command(
                control,
                node_id,
                CommandKind::Stop,
                serde_json::json!({"remove": true}),
                None,
            )
            .await?;
            if let Some(p) = primary {
                let mut c = super::connect(&p.url_for(Role::Super, "postgres")).await?;
                super::drop_slot(&mut c, &super::slot_name(&node.name)).await?;
            }
        }
        NodeKind::External => {}
    }
    let mut tx = control.begin(TxMode::Normal).await?;
    tx.execute(&Statement::new(
        "UPDATE _forgebase_db_nodes SET state = 'retired', name = ?2, updated_at = ?3 WHERE id = ?1",
        vec![
            node_id.into(),
            format!("{}~{}", node.name, node.id).into(),
            now().into(),
        ],
    ))
    .await?;
    topology::bump_epoch(tx.as_mut()).await?;
    commit(tx).await
}

// ---------------------------------------------------------------------------
// Promote (ADR-0029 §2.8).
// ---------------------------------------------------------------------------

/// `POST /api/system/database/nodes/{id}/promote`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromoteRequest {
    /// The node name, typed by the operator.
    pub confirm: String,
    /// "The old primary is down": promote even though commits after the
    /// target's replay position may be lost.
    #[serde(default)]
    pub accept_data_loss: bool,
}

/// What the server does around a promote: it owns the pools.
pub trait PoolSwitch: Send + Sync {
    /// Step 2: close the set's writer pool (no new writes reach the old primary).
    fn fence<'a>(&'a self, set_id: &'a str) -> forgebase_db::BoxFuture<'a, ()>;

    /// Step 5: point the set's writer and read pools at `new_primary`
    /// (running here as `handle`; `None` for a remote node, reached by its
    /// sealed URL) and return the control database to write the topology
    /// change to: for the control set, the new primary itself.
    fn repoint<'a>(
        &'a self,
        set_id: &'a str,
        new_primary: &'a DbNode,
        handle: Option<Arc<EngineHandle>>,
    ) -> forgebase_db::BoxFuture<'a, ServiceResult<Database>>;
}

/// The data-loss estimate shown before a promote (§2.8 step 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromoteCheck {
    /// The target's replay position.
    pub replayed: Option<Lsn>,
    /// The primary's current position (when it answers).
    pub primary_lsn: Option<Lsn>,
    /// Bytes the target is behind (when both are known).
    pub behind_bytes: Option<u64>,
    /// The old primary answers (a clean, lossless fence is possible).
    pub primary_reachable: bool,
}

/// Step 1 for a local target: its replay position against the primary's.
///
/// # Errors
/// The target is not running or not a standby.
pub async fn promote_check(
    replicas: &LocalReplicas,
    primary: Option<&EngineHandle>,
    target: &str,
) -> ServiceResult<PromoteCheck> {
    let h = replicas
        .get(target)
        .await
        .ok_or_else(|| ServiceError::Unreachable(format!("replica {target} is not running here")))?;
    let t = standby::facts(&h).await?;
    if !t.in_recovery {
        return Err(ServiceError::State(format!("{target} is not a standby")));
    }
    let p = match primary {
        Some(p) => standby::facts(p).await.ok(),
        None => None,
    };
    let primary_lsn = p.as_ref().and_then(|f| f.lsn);
    Ok(PromoteCheck {
        replayed: t.lsn,
        primary_lsn,
        behind_bytes: match (primary_lsn, t.lsn) {
            (Some(a), Some(b)) => Some(a.0.saturating_sub(b.0)),
            _ => None,
        },
        primary_reachable: p.is_some(),
    })
}

/// What [`promote_local`] returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromoteOutcome {
    /// Engine-side report (clean, positions, timeline, re-followed replicas).
    pub report: PromoteReport,
    /// The new topology epoch.
    pub epoch: u64,
    /// The old primary's node id (`needs_rebuild`; rejoin it with [`rejoin_local`]).
    pub old_primary: String,
}

/// Job `database.promote` for a local replica of the set whose primary is
/// `old_primary` (running here, or already stopped):
///
/// 1. confirmation and target checks;
/// 2. [`PoolSwitch::fence`], then stop the old primary;
/// 3. wait for its last record when it stopped cleanly (else
///    `accept_data_loss` is required);
/// 4. promote;
/// 5. [`PoolSwitch::repoint`], swap the roles on control (the old primary
///    becomes `needs_rebuild`; control's primary gets a row named
///    `primary` if it had none), bump the epoch, rewrite `topology.json`;
/// 6. the other local replicas follow the new primary (new slots, reload).
///
/// The new primary's handle leaves `replicas` (it is the primary now);
/// the caller keeps the returned handle running.
///
/// # Errors
/// Any step; a refusal before step 2 changes nothing.
pub async fn promote_local(
    control: &dyn Backend,
    replicas: &LocalReplicas,
    old_primary: OldPrimary<'_>,
    node_id: &str,
    req: &PromoteRequest,
    pools: &dyn PoolSwitch,
    topology_file: Option<&Path>,
) -> ServiceResult<(PromoteOutcome, Arc<EngineHandle>)> {
    require_pg(control)?;
    let topo = topology::load(control).await?;
    let node = topo
        .nodes
        .iter()
        .find(|n| n.id == node_id && n.state != NodeState::Retired)
        .cloned()
        .ok_or_else(|| ServiceError::NodeNotFound(node_id.to_owned()))?;
    if req.confirm != node.name {
        return Err(ServiceError::ConfirmMismatch(node.name));
    }
    if node.role != NodeRole::Replica {
        return Err(ServiceError::State(format!("{} is already the primary", node.name)));
    }
    if node.kind != NodeKind::Local {
        return Err(ServiceError::Unsupported(format!(
            "{} is a {} node: promote_local promotes replicas on this host (remote: promote_remote)",
            node.name, node.kind
        )));
    }
    let target = replicas
        .get(&node.name)
        .await
        .ok_or_else(|| ServiceError::Unreachable(format!("replica {} is not running here", node.name)))?;
    let old_row = topo.primary_of(&node.set_id).cloned();
    update_node(control, node_id, None, NodeState::Promoting, None).await?;
    pools.fence(&node.set_id).await;
    let report = match standby::promote_local(
        replicas,
        &node.name,
        old_primary,
        req.accept_data_loss,
        Duration::from_secs(60),
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            let msg = e.to_string();
            // The control database may be the stopped primary: best effort.
            let _ = topology::update_node_state(control, node_id, NodeState::CatchingUp).await;
            if msg.contains("accept_data_loss") {
                return Err(ServiceError::DataLoss(msg));
            }
            return Err(e.into());
        }
    };
    let handle = replicas.take(&node.name).await.unwrap_or(target);
    let mut promoted = node.clone();
    promoted.role = NodeRole::Primary;
    promoted.state = NodeState::Ready;
    let new_control = pools.repoint(&node.set_id, &promoted, Some(handle.clone())).await?;
    let facts = facts_of(&handle).await;
    let (old_id, epoch) = swap_roles(&new_control, &node, old_row.as_ref(), &facts, topology_file).await?;
    Ok((
        PromoteOutcome {
            report,
            epoch,
            old_primary: old_id,
        },
        handle,
    ))
}

/// Step 5 on control: the old primary becomes a `needs_rebuild` replica
/// (a row named [`standby::OLD_PRIMARY`] when control's primary had none),
/// the target the `ready` primary; epoch + 1; `topology.json` rewritten.
async fn swap_roles(
    new_control: &Database,
    node: &DbNode,
    old_row: Option<&DbNode>,
    facts: &NodeFacts,
    topology_file: Option<&Path>,
) -> ServiceResult<(String, u64)> {
    let mut tx = new_control.begin(TxMode::Normal).await?;
    let old_id = match old_row {
        Some(o) => {
            set_node(
                tx.as_mut(),
                &o.id,
                Some(NodeRole::Replica),
                NodeState::NeedsRebuild,
                None,
            )
            .await?;
            o.id.clone()
        }
        None => {
            insert_node_tx(
                tx.as_mut(),
                &topology::NewNode {
                    set_id: node.set_id.clone(),
                    name: standby::OLD_PRIMARY.to_owned(),
                    kind: NodeKind::Local,
                    role: NodeRole::Replica,
                    url_sealed: None,
                    state: NodeState::NeedsRebuild,
                    weight: 1,
                    origin: NodeOrigin::Api,
                },
            )
            .await?
        }
    };
    set_node(
        tx.as_mut(),
        &node.id,
        Some(NodeRole::Primary),
        NodeState::Ready,
        Some(facts),
    )
    .await?;
    let epoch = topology::bump_epoch(tx.as_mut()).await?;
    commit(tx).await?;
    if let Some(path) = topology_file {
        let topo = topology::load(new_control).await?;
        topology::write_topology_file(path, &topo)?;
    }
    Ok((old_id, epoch))
}

/// What [`promote_remote`] returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemotePromoteOutcome {
    /// The old primary stopped cleanly and the agent waited for its last record.
    pub clean: bool,
    /// The position the agent waited for (`None` without a clean stop).
    pub min_lsn: Option<Lsn>,
    /// The agent's answer (`{"lsn", "timeline"}`).
    pub agent: Option<String>,
    /// The new topology epoch.
    pub epoch: u64,
    /// The old primary's node id (`needs_rebuild`).
    pub old_primary: String,
    /// `follow` commands queued for the set's other remote replicas.
    pub follow_commands: Vec<String>,
}

/// Job `database.promote` for a remote replica (`forgebase node`), with the
/// set's primary running here: fence and stop it, then the agent's
/// `promote` command waits for the old primary's last record (clean stop)
/// and runs `pg_promote`, creating slots for the other remote replicas;
/// then [`PoolSwitch::repoint`] (no local handle), the role swap on
/// control, and a `follow` command per other remote replica. The new
/// primary is reached at the `listen` its agent reported at join.
///
/// # Errors
/// Any step; `node_unreachable` when the agent does not answer in `timeout`.
#[allow(clippy::too_many_arguments)]
pub async fn promote_remote(
    control: &dyn Backend,
    old_primary: OldPrimary<'_>,
    node_id: &str,
    req: &PromoteRequest,
    pools: &dyn PoolSwitch,
    topology_file: Option<&Path>,
    timeout: Duration,
) -> ServiceResult<RemotePromoteOutcome> {
    require_pg(control)?;
    let topo = topology::load(control).await?;
    let node = topo
        .nodes
        .iter()
        .find(|n| n.id == node_id && n.state != NodeState::Retired)
        .cloned()
        .ok_or_else(|| ServiceError::NodeNotFound(node_id.to_owned()))?;
    if req.confirm != node.name {
        return Err(ServiceError::ConfirmMismatch(node.name));
    }
    if node.kind != NodeKind::Remote || node.role != NodeRole::Replica {
        return Err(ServiceError::Unsupported(format!(
            "{} is a {} {}: promote_remote promotes a remote replica",
            node.name, node.kind, node.role
        )));
    }
    let (_, listen) = agent_addresses(control, node_id).await?;
    let listen = listen.ok_or_else(|| {
        ServiceError::Unsupported(format!(
            "{} reported no listen address at join: the server cannot reach it as a primary",
            node.name
        ))
    })?;
    let old_row = topo.primary_of(&node.set_id).cloned();
    let others: Vec<DbNode> = topo
        .nodes_of(&node.set_id)
        .filter(|n| n.id != node.id && n.role == NodeRole::Replica && n.kind == NodeKind::Remote)
        .filter(|n| !matches!(n.state, NodeState::Retired | NodeState::NeedsRebuild))
        .cloned()
        .collect();
    update_node(control, node_id, None, NodeState::Promoting, None).await?;
    pools.fence(&node.set_id).await;
    let (layout, engine) = match &old_primary {
        OldPrimary::Running(h) => {
            h.stop().await?;
            (h.layout().clone(), h.engine().clone())
        }
        OldPrimary::Stopped { layout, engine } => ((*layout).clone(), (*engine).clone()),
    };
    let control_data = if layout.pgdata().join("global").join("pg_control").is_file() {
        Some(super::control_data(&engine, &layout.pgdata()).await?)
    } else {
        None
    };
    let min_lsn = control_data
        .as_ref()
        .filter(|c| c.primary_stopped_cleanly())
        .map(|c| Lsn(c.checkpoint.0 + 1));
    if min_lsn.is_none() && !req.accept_data_loss {
        return Err(ServiceError::DataLoss(
            "the old primary did not stop cleanly, so commits may be lost: confirm with accept_data_loss".into(),
        ));
    }
    let slots: Vec<String> = others.iter().map(|n| super::slot_name(&n.name)).collect();
    let cmd = queue_command(
        control,
        node_id,
        CommandKind::Promote,
        serde_json::json!({ "min_lsn": min_lsn, "slots": slots }),
        None,
    )
    .await?;
    let out = wait_command(control, &cmd.id, timeout).await?;
    if out.state != "done" {
        return Err(ServiceError::State(format!(
            "{} did not promote: {}",
            node.name,
            out.result.unwrap_or_default()
        )));
    }
    let mut promoted = node.clone();
    promoted.role = NodeRole::Primary;
    promoted.state = NodeState::Ready;
    let new_control = pools.repoint(&node.set_id, &promoted, None).await?;
    let (old_id, epoch) = swap_roles(
        &new_control,
        &node,
        old_row.as_ref(),
        &NodeFacts::default(),
        topology_file,
    )
    .await?;
    let (host, port) = listen
        .rsplit_once(':')
        .and_then(|(h, p)| {
            p.parse::<u16>()
                .ok()
                .map(|p| (h.trim_matches(['[', ']']).to_owned(), p))
        })
        .ok_or_else(|| ServiceError::Invalid(format!("bad listen address {listen:?}")))?;
    let mut follow_commands = Vec::new();
    for other in &others {
        let grant = UpstreamGrantArgs {
            host: host.clone(),
            port,
            slot: super::slot_name(&other.name),
        };
        let c = queue_command(
            &new_control,
            &other.id,
            CommandKind::Follow,
            serde_json::json!({ "upstream_change": grant }),
            None,
        )
        .await?;
        follow_commands.push(c.id);
    }
    Ok(RemotePromoteOutcome {
        clean: min_lsn.is_some(),
        min_lsn,
        agent: out.result,
        epoch,
        old_primary: old_id,
        follow_commands,
    })
}

/// The part of an upstream a `follow` after a remote promote changes (the
/// agent keeps its replicator login: the new primary is a clone with the
/// same roles).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamGrantArgs {
    /// New primary host.
    pub host: String,
    /// Port.
    pub port: u16,
    /// Slot on the new primary.
    pub slot: String,
}

/// "Rejoin" for a local old primary (`needs_rebuild`): `pg_rewind` from
/// the new primary when the engine has it (falls back to a fresh clone),
/// start it as a standby with slot `fbr_<name>`, `catching_up`.
///
/// # Errors
/// Unknown node, not local or not `needs_rebuild`, or a step fails.
pub async fn rejoin_local(
    control: &dyn Backend,
    replicas: &LocalReplicas,
    new_primary: &EngineHandle,
    node_id: &str,
) -> ServiceResult<(NodeView, RejoinHow)> {
    let node = node_by_id(control, node_id).await?;
    if node.kind != NodeKind::Local || node.role != NodeRole::Replica || node.state != NodeState::NeedsRebuild {
        return Err(ServiceError::State(format!(
            "{} is a {} {} in state {}: rejoin takes a local replica that needs a rebuild",
            node.name, node.kind, node.role, node.state
        )));
    }
    if replicas.get(&node.name).await.is_some() {
        replicas.stop(&node.name).await?;
    }
    let layout = replicas.layout(&node.name)?;
    let up = Upstream::local(new_primary, &node.name)?;
    {
        let mut c = super::connect(&new_primary.url_for(Role::Super, "postgres")).await?;
        super::ensure_slot(&mut c, &up.slot).await?;
    }
    let rewind_src = up.clone().as_super(new_primary);
    update_node(control, node_id, None, NodeState::Provisioning, None).await?;
    let opts = standby::CloneOptions {
        no_sync: replicas.config().unsafe_fast,
    };
    let how = match standby::rejoin(new_primary.engine(), &layout, &up, Some(&rewind_src), opts).await {
        Ok(h) => h,
        Err(e) => {
            let _ = topology::update_node_state(control, node_id, NodeState::NeedsRebuild).await;
            return Err(e.into());
        }
    };
    let h = Arc::new(Engine::prepare(&layout, replicas.config()).await?);
    replicas.adopt(&node.name, h.clone()).await;
    let facts = facts_of(&h).await;
    update_node(control, node_id, None, NodeState::CatchingUp, Some(&facts)).await?;
    Ok((node_view(control, node_id).await?, how))
}

// ---------------------------------------------------------------------------
// Join tokens and agents (ADR-0029 §2.3).
// ---------------------------------------------------------------------------

/// `POST /api/system/database/join-tokens {set, role}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinTokenRequest {
    /// The set the node will serve.
    #[serde(rename = "set")]
    pub set_id: String,
    /// `replica` (a shard's primary joins with FB-174).
    pub role: NodeRole,
}

/// A new join token: shown once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedJoinToken {
    /// Token row id (for the audit log).
    pub id: String,
    /// The token (`fbj_` + 64 hex); only its SHA-256 is stored.
    pub token: String,
    /// Set.
    #[serde(rename = "set")]
    pub set_id: String,
    /// Role.
    pub role: NodeRole,
    /// Expiry (RFC 3339; 15 minutes).
    pub expires_at: String,
    /// The command to run on the other host.
    pub command: String,
}

/// Creates a one-time join token (15 minutes). The caller audit-logs the
/// returned `id` (never the token).
///
/// # Errors
/// SQLite, an unknown set, or `role = primary`.
pub async fn issue_join_token(
    control: &dyn Backend,
    req: &JoinTokenRequest,
    created_by: Option<&str>,
    server_url: &str,
) -> ServiceResult<IssuedJoinToken> {
    require_pg(control)?;
    if req.role != NodeRole::Replica {
        return Err(ServiceError::Unsupported(
            "join tokens add replicas; shard primaries on other hosts come with FB-174".into(),
        ));
    }
    let topo = topology::load(control).await?;
    if topo.set(&req.set_id).is_none() {
        return Err(ServiceError::SetNotFound(req.set_id.clone()));
    }
    let token = random_secret(JOIN_TOKEN_PREFIX)?;
    let id = new_id('j');
    let expires = time::OffsetDateTime::now_utc() + JOIN_TOKEN_TTL;
    let expires_at = forgebase_core::format_rfc3339(expires);
    let mut tx = control.begin(TxMode::Normal).await?;
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_join_tokens (id, token_hash, set_id, role, created_by, created_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        vec![
            id.clone().into(),
            sha256_hex(&token).into(),
            req.set_id.clone().into(),
            req.role.as_str().into(),
            created_by.map(str::to_owned).into(),
            now().into(),
            expires_at.clone().into(),
        ],
    ))
    .await?;
    commit(tx).await?;
    let command = format!(
        "forgebase node join {} --token {token} --data-dir <DIR> --name <NODE NAME>",
        server_url.trim_end_matches('/')
    );
    Ok(IssuedJoinToken {
        id,
        token,
        set_id: req.set_id.clone(),
        role: req.role,
        expires_at,
        command,
    })
}

/// Where remote replicas of a set stream from: the primary's TCP address
/// as agents reach it, and the credentials the join hands out.
#[derive(Debug, Clone)]
pub struct ReplicationSource {
    /// The primary's exact engine.
    pub engine: EngineIdentity,
    /// Host agents connect to (the advertised address of `[database.postgres] listen`).
    pub host: String,
    /// Port.
    pub port: u16,
    /// TLS mode agents use (`verify-full` once FB-153's certificate exists;
    /// `disable` only for loopback tests).
    pub sslmode: SslMode,
    /// The replicator's password.
    pub replicator_password: Secret,
    /// Superuser URL of the primary's `postgres` database (slots).
    pub admin_url: Secret,
}

impl ReplicationSource {
    /// From the local primary: it must listen on TCP (`[database.postgres]
    /// listen`), or remote replicas cannot reach it. `advertise` replaces
    /// the listen address agents dial (a public name for a wildcard bind).
    ///
    /// # Errors
    /// The primary does not listen on TCP.
    pub fn from_handle(primary: &EngineHandle, advertise: Option<&str>, sslmode: SslMode) -> ServiceResult<Self> {
        let addr = primary.tcp_addr().ok_or_else(|| {
            ServiceError::Unsupported(
                "remote replicas need the primary on TCP: set [database.postgres] listen (and FB-153's TLS)".into(),
            )
        })?;
        let password = primary
            .ctx
            .secrets
            .password(Role::Replicator)
            .ok_or_else(|| EngineError::Config("roles.secret has no forgebase_replicator password".into()))?;
        Ok(Self {
            engine: EngineIdentity::of(primary.engine()),
            host: advertise.map_or_else(|| addr.ip().to_string(), str::to_owned),
            port: addr.port(),
            sslmode,
            replicator_password: Secret::new(password),
            admin_url: primary.url_for(Role::Super, "postgres"),
        })
    }
}

/// The agent's first call (`POST` [`paths::JOIN`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    /// The join token.
    pub token: String,
    /// Node name (`a-z0-9-_`, 1-32).
    pub name: String,
    /// The agent's engine (must equal the primary's).
    pub engine: EngineIdentity,
    /// `host:port` the server reads the replica at (its TCP listen), if any.
    #[serde(default)]
    pub listen: Option<String>,
}

/// How a remote standby reaches the primary (part of [`JoinGrant`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpstreamGrant {
    /// Primary host.
    pub host: String,
    /// Port.
    pub port: u16,
    /// TLS mode.
    pub sslmode: SslMode,
    /// `forgebase_replicator`.
    pub user: String,
    /// Its password (sent once over HTTPS; the agent keeps it in a 0600 passfile).
    pub password: String,
    /// Slot on the primary.
    pub slot: String,
}

impl UpstreamGrant {
    /// As a [`Upstream`] for node `name`.
    pub fn upstream(&self, name: &str) -> Upstream {
        Upstream {
            host: self.host.clone(),
            port: self.port,
            sslmode: self.sslmode,
            user: self.user.clone(),
            password: Secret::new(self.password.clone()),
            slot: self.slot.clone(),
            application_name: name.to_owned(),
        }
    }
}

/// The answer to a join: who the agent is now and how it streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinGrant {
    /// The node's id.
    pub node_id: String,
    /// Set.
    #[serde(rename = "set")]
    pub set_id: String,
    /// Node name.
    pub name: String,
    /// The agent's key (`fba_` + 64 hex) for commands and heartbeats; only its hash is stored.
    pub agent_key: String,
    /// Replication login.
    pub upstream: UpstreamGrant,
    /// The primary's engine.
    pub engine: EngineIdentity,
    /// Heartbeat interval.
    pub heartbeat_ms: u64,
    /// Longest long-poll wait.
    pub poll_wait_ms: u64,
}

/// Redeems a join token (`POST` [`paths::JOIN`]): checks the token (one
/// time, 15 minutes) and the exact engine version (a mismatch keeps the
/// token for a retry with the right binary), inserts the node (`remote`,
/// `replica`, `provisioning`) and its agent, creates its slot on the
/// primary, and hands out the replication login and the agent key. `peer`
/// is the address the request came from (stored; FB-153's `pg_hba.conf`
/// entries use it).
///
/// # Errors
/// [`ServiceError::TokenInvalid`], [`ServiceError::EngineMismatch`], a
/// used or invalid name, or SQL errors.
pub async fn redeem_join(
    control: &dyn Backend,
    source: &ReplicationSource,
    req: &JoinRequest,
    peer: Option<IpAddr>,
) -> ServiceResult<JoinGrant> {
    require_pg(control)?;
    let hash = sha256_hex(&req.token);
    let rows = control
        .fetch_all(&Statement::new(
            "SELECT id, set_id, role, expires_at, used_at FROM _forgebase_join_tokens WHERE token_hash = ?1",
            vec![hash.into()],
        ))
        .await?;
    let row = rows.rows.first().ok_or(ServiceError::TokenInvalid)?;
    let text = |i: usize| row.get(i).and_then(Value::as_str).map(str::to_owned);
    let token_id = text(0).ok_or(ServiceError::TokenInvalid)?;
    let set_id = text(1).ok_or(ServiceError::TokenInvalid)?;
    let expired = text(3)
        .and_then(|s| parse_time(&s))
        .is_none_or(|t| t <= time::OffsetDateTime::now_utc());
    if expired || text(4).is_some() {
        return Err(ServiceError::TokenInvalid);
    }
    req.engine
        .check_same(&source.engine)
        .map_err(ServiceError::EngineMismatch)?;
    check_name(&req.name)?;
    ensure_unused_name(control, &req.name).await?;
    let agent_key = random_secret(AGENT_KEY_PREFIX)?;
    let mut tx = control.begin(TxMode::Normal).await?;
    let used = tx
        .execute(&Statement::new(
            "UPDATE _forgebase_join_tokens SET used_at = ?2 WHERE id = ?1 AND used_at IS NULL",
            vec![token_id.clone().into(), now().into()],
        ))
        .await?;
    if used.rows_affected != 1 {
        return Err(ServiceError::TokenInvalid);
    }
    let node_id = insert_node_tx(
        tx.as_mut(),
        &topology::NewNode {
            set_id: set_id.clone(),
            name: req.name.clone(),
            kind: NodeKind::Remote,
            role: NodeRole::Replica,
            url_sealed: None,
            state: NodeState::Provisioning,
            weight: 1,
            origin: NodeOrigin::Api,
        },
    )
    .await?;
    tx.execute(&Statement::new(
        "UPDATE _forgebase_join_tokens SET node_id = ?2 WHERE id = ?1",
        vec![token_id.into(), node_id.clone().into()],
    ))
    .await?;
    let detail = serde_json::json!({ "listen": req.listen }).to_string();
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_db_node_agents (node_id, key_hash, address, engine_version, state, detail, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, 'joining', ?5, ?6, ?6)",
        vec![
            node_id.clone().into(),
            sha256_hex(&agent_key).into(),
            peer.map(|p| p.to_string()).into(),
            req.engine.label().into(),
            detail.into(),
            now().into(),
        ],
    ))
    .await?;
    set_node(
        tx.as_mut(),
        &node_id,
        None,
        NodeState::Provisioning,
        Some(&NodeFacts {
            engine_version: Some(req.engine.label()),
            ..NodeFacts::default()
        }),
    )
    .await?;
    topology::bump_epoch(tx.as_mut()).await?;
    let slot = super::slot_name(&req.name);
    // The slot first: a failure rolls the join back and keeps the token unused.
    let mut admin = super::connect(&source.admin_url).await?;
    super::ensure_slot(&mut admin, &slot).await?;
    commit(tx).await?;
    Ok(JoinGrant {
        node_id,
        set_id,
        name: req.name.clone(),
        agent_key,
        upstream: UpstreamGrant {
            host: source.host.clone(),
            port: source.port,
            sslmode: source.sslmode,
            user: Role::Replicator.name().to_owned(),
            password: source.replicator_password.expose().to_owned(),
            slot,
        },
        engine: source.engine.clone(),
        heartbeat_ms: u64::try_from(HEARTBEAT_INTERVAL.as_millis()).unwrap_or(5_000),
        poll_wait_ms: u64::try_from(MAX_POLL_WAIT.as_millis()).unwrap_or(30_000),
    })
}

/// The read URL of a remote replica for the server's read pool: the
/// replica has the primary's roles (it is a clone), so the reader logs in
/// with the primary's password. Seal it with the keyring into the node's
/// `url_sealed` (FB-175), never store it plain.
pub fn reader_url(listen: &str, reader_password: &Secret, sslmode: SslMode) -> Secret {
    Secret::new(format!(
        "postgres://{}:{}@{listen}/{}?sslmode={}",
        Role::Reader.name(),
        reader_password.expose(),
        crate::DATABASE,
        sslmode.as_str()
    ))
}

/// The address and `listen` a remote node's agent reported at join.
///
/// # Errors
/// Database errors.
pub async fn agent_addresses(control: &dyn Backend, node_id: &str) -> ServiceResult<(Option<String>, Option<String>)> {
    let rows = control
        .fetch_all(&Statement::new(
            "SELECT address, detail FROM _forgebase_db_node_agents WHERE node_id = ?1",
            vec![node_id.into()],
        ))
        .await?;
    let Some(r) = rows.rows.first() else {
        return Err(ServiceError::NodeNotFound(node_id.to_owned()));
    };
    let listen = r
        .get(1)
        .and_then(Value::as_str)
        .and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok())
        .and_then(|v| v.get("listen").and_then(|l| l.as_str()).map(str::to_owned));
    Ok((r.first().and_then(Value::as_str).map(str::to_owned), listen))
}

/// Checks an agent key (`Authorization: Bearer`) for `node_id`.
///
/// # Errors
/// [`ServiceError::AgentUnauthorized`] on any mismatch (also an unknown node).
pub async fn authenticate_agent(control: &dyn Backend, node_id: &str, key: &str) -> ServiceResult<()> {
    let rows = control
        .fetch_all(&Statement::new(
            "SELECT a.key_hash FROM _forgebase_db_node_agents a WHERE a.node_id = ?1",
            vec![node_id.into()],
        ))
        .await?;
    let stored = rows
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(Value::as_str)
        .ok_or(ServiceError::AgentUnauthorized)?;
    let given = sha256_hex(key);
    // Equal-length hex strings; compare without an early exit.
    let diff = stored
        .bytes()
        .zip(given.bytes())
        .fold(stored.len() ^ given.len(), |acc, (a, b)| acc | usize::from(a ^ b));
    if diff == 0 {
        Ok(())
    } else {
        Err(ServiceError::AgentUnauthorized)
    }
}

macro_rules! wire_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $v:ident => $t:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vm])* $v),+ }

        impl $name {
            /// Every value.
            pub const ALL: &'static [$name] = &[$($name::$v),+];

            /// The stored text.
            pub fn as_str(self) -> &'static str {
                match self { $($name::$v => $t),+ }
            }

            /// Parses the stored text.
            pub fn parse(s: &str) -> Option<Self> {
                match s { $($t => Some($name::$v),)+ _ => None }
            }
        }
    };
}

wire_enum! {
    /// A command the server queues for an agent.
    CommandKind {
        /// Stop, delete the data, clone again.
        Rebuild => "rebuild",
        /// Stop the engine (`{"remove": true}`: also delete the data and exit).
        Stop => "stop",
        /// `{"min_lsn", "slots"}`: wait for `min_lsn`, `pg_promote`, create `slots`.
        Promote => "promote",
        /// `{"upstream"}`: rewind (or re-clone) and follow a new primary.
        Rejoin => "rejoin",
        /// `{"upstream"}`: point `primary_conninfo` at a new primary and reload.
        Follow => "follow",
    }
}

wire_enum! {
    /// What an agent reports in its heartbeat.
    AgentState {
        /// Joined, nothing done yet.
        Joining => "joining",
        /// `pg_basebackup` running.
        Cloning => "cloning",
        /// Engine starting.
        Starting => "starting",
        /// Replaying towards the primary.
        CatchingUp => "catching_up",
        /// WAL receiver streaming.
        Streaming => "streaming",
        /// Engine stopped (command or failure).
        Stopped => "stopped",
        /// Re-cloning or rewinding.
        Rebuilding => "rebuilding",
        /// Promotion under way.
        Promoting => "promoting",
        /// Promoted: this node is a primary now.
        Primary => "primary",
        /// The engine cannot run (see `detail`).
        Failed => "failed",
    }
}

/// One queued command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCommand {
    /// Id (answer it in a heartbeat).
    pub id: String,
    /// Kind.
    pub kind: CommandKind,
    /// Arguments (JSON object).
    pub args: serde_json::Value,
    /// Queued at.
    pub created_at: String,
}

/// The long-poll's answer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandBatch {
    /// Commands in queue order (empty after the wait).
    pub commands: Vec<NodeCommand>,
}

/// Queues `kind` for a remote node's agent.
///
/// # Errors
/// Unknown node, not remote, or database errors.
pub async fn queue_command(
    control: &dyn Backend,
    node_id: &str,
    kind: CommandKind,
    args: serde_json::Value,
    created_by: Option<&str>,
) -> ServiceResult<NodeCommand> {
    let node = node_by_id(control, node_id).await?;
    if node.kind != NodeKind::Remote {
        return Err(ServiceError::Unsupported(format!(
            "{} is a {} node; only remote nodes take agent commands",
            node.name, node.kind
        )));
    }
    let cmd = NodeCommand {
        id: new_id('c'),
        kind,
        args,
        created_at: now(),
    };
    let mut tx = control.begin(TxMode::Normal).await?;
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_db_node_commands (id, node_id, kind, args, created_by, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        vec![
            cmd.id.clone().into(),
            node_id.into(),
            kind.as_str().into(),
            cmd.args.to_string().into(),
            created_by.map(str::to_owned).into(),
            cmd.created_at.clone().into(),
        ],
    ))
    .await?;
    commit(tx).await?;
    Ok(cmd)
}

/// The agent's long-poll (`GET` [`paths::COMMANDS`]): returns queued
/// commands at once (marking them `sent`), else waits up to `wait`
/// (at most [`MAX_POLL_WAIT`]) polling every 250 ms. Commands sent but not
/// answered are sent again after an agent restart (`resend_sent`).
///
/// # Errors
/// Bad agent key, or database errors.
pub async fn poll_commands(
    control: &dyn Backend,
    node_id: &str,
    agent_key: &str,
    wait: Duration,
    resend_sent: bool,
) -> ServiceResult<CommandBatch> {
    authenticate_agent(control, node_id, agent_key).await?;
    let deadline = Instant::now() + wait.min(MAX_POLL_WAIT);
    let states = if resend_sent {
        "state IN ('queued', 'sent')"
    } else {
        "state = 'queued'"
    };
    loop {
        let rows = control
            .fetch_all(&Statement::new(
                format!(
                    "SELECT id, kind, args, created_at FROM _forgebase_db_node_commands \
                     WHERE node_id = ?1 AND {states} ORDER BY created_at, id"
                ),
                vec![node_id.into()],
            ))
            .await?;
        if !rows.rows.is_empty() {
            let mut out = Vec::new();
            let mut tx = control.begin(TxMode::Normal).await?;
            for r in &rows.rows {
                let text = |i: usize| r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned();
                let Some(kind) = CommandKind::parse(&text(1)) else {
                    continue;
                };
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_db_node_commands SET state = 'sent', sent_at = ?2 WHERE id = ?1",
                    vec![text(0).into(), now().into()],
                ))
                .await?;
                out.push(NodeCommand {
                    id: text(0),
                    kind,
                    args: serde_json::from_str(&text(2)).unwrap_or(serde_json::Value::Null),
                    created_at: text(3),
                });
            }
            commit(tx).await?;
            return Ok(CommandBatch { commands: out });
        }
        if Instant::now() >= deadline {
            return Ok(CommandBatch::default());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The answer to one command, carried by a heartbeat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandResult {
    /// Command id.
    pub id: String,
    /// Whether it succeeded.
    pub ok: bool,
    /// What happened (an error's text when `ok` is false).
    #[serde(default)]
    pub message: String,
}

/// The agent's heartbeat (`POST` [`paths::HEARTBEAT`], every 5 s).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Heartbeat {
    /// What the agent is doing.
    pub state: AgentState,
    /// Replay position (standby) or current position (after promotion).
    #[serde(default)]
    pub replay_lsn: Option<Lsn>,
    /// Free bytes in its data directory.
    #[serde(default)]
    pub disk_free: Option<u64>,
    /// Its engine.
    pub engine: EngineIdentity,
    /// Free text for the dashboard (last error).
    #[serde(default)]
    pub detail: Option<String>,
    /// Answers to commands since the last heartbeat.
    #[serde(default)]
    pub results: Vec<CommandResult>,
}

/// The heartbeat's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeartbeatReply {
    /// The node's stored state after the heartbeat.
    pub node_state: NodeState,
    /// Commands are waiting: poll now.
    pub commands_pending: bool,
    /// Next heartbeat in.
    pub heartbeat_ms: u64,
}

/// The node state a heartbeat implies, given the stored one; `None` keeps
/// it. The router's monitor owns lag (`catching_up` ↔ `streaming` ↔
/// `lagging`); heartbeats only move a node into and out of service.
pub fn state_after_heartbeat(stored: NodeState, reported: AgentState) -> Option<NodeState> {
    use AgentState as A;
    use NodeState as N;
    let next = match (stored, reported) {
        (N::Retired | N::Promoting, _) | (_, A::Joining | A::Promoting | A::Primary) => return None,
        (N::Provisioning | N::NeedsRebuild | N::Unreachable, A::CatchingUp | A::Streaming) => N::CatchingUp,
        (_, A::Cloning | A::Starting | A::Rebuilding) => N::Provisioning,
        (_, A::Stopped) => N::Unreachable,
        (_, A::Failed) => N::NeedsRebuild,
        _ => return None,
    };
    (next != stored).then_some(next)
}

/// Records a heartbeat: the agent row, command answers, and the node state
/// transition ([`state_after_heartbeat`]).
///
/// # Errors
/// Bad agent key, or database errors.
pub async fn heartbeat(
    control: &dyn Backend,
    node_id: &str,
    agent_key: &str,
    hb: &Heartbeat,
) -> ServiceResult<HeartbeatReply> {
    authenticate_agent(control, node_id, agent_key).await?;
    // A removed (retired) node still reports once: the answer to its `stop`.
    let node = topology::load(control)
        .await?
        .nodes
        .into_iter()
        .find(|n| n.id == node_id)
        .ok_or_else(|| ServiceError::NodeNotFound(node_id.to_owned()))?;
    let ts = now();
    let mut tx = control.begin(TxMode::Normal).await?;
    tx.execute(&Statement::new(
        "UPDATE _forgebase_db_node_agents SET state = ?2, replay_lsn = ?3, disk_free = ?4, engine_version = ?5, \
         last_seen_at = ?6, updated_at = ?6, detail = COALESCE(CAST(?7 AS TEXT), detail) WHERE node_id = ?1",
        vec![
            node_id.into(),
            hb.state.as_str().into(),
            hb.replay_lsn.map(|l| l.to_string()).into(),
            hb.disk_free.and_then(|d| i64::try_from(d).ok()).into(),
            hb.engine.label().into(),
            ts.clone().into(),
            hb.detail
                .as_ref()
                .map(|d| serde_json::json!({ "detail": d }).to_string())
                .into(),
        ],
    ))
    .await?;
    for r in &hb.results {
        tx.execute(&Statement::new(
            "UPDATE _forgebase_db_node_commands SET state = ?3, result = ?4, done_at = ?5 \
             WHERE id = ?1 AND node_id = ?2 AND state IN ('queued', 'sent')",
            vec![
                r.id.clone().into(),
                node_id.into(),
                if r.ok { "done" } else { "failed" }.into(),
                r.message.clone().into(),
                ts.clone().into(),
            ],
        ))
        .await?;
    }
    let mut node_state = node.state;
    if let Some(next) = state_after_heartbeat(node.state, hb.state) {
        set_node(tx.as_mut(), node_id, None, next, None).await?;
        topology::bump_epoch(tx.as_mut()).await?;
        node_state = next;
    }
    let pending = tx
        .fetch_all(&Statement::new(
            "SELECT 1 FROM _forgebase_db_node_commands WHERE node_id = ?1 AND state = 'queued' LIMIT 1",
            vec![node_id.into()],
        ))
        .await?;
    commit(tx).await?;
    Ok(HeartbeatReply {
        node_state,
        commands_pending: !pending.rows.is_empty(),
        heartbeat_ms: u64::try_from(HEARTBEAT_INTERVAL.as_millis()).unwrap_or(5_000),
    })
}

/// A command's outcome once the agent answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutcome {
    /// `queued`, `sent`, `done`, `failed`, `cancelled`.
    pub state: String,
    /// The agent's message.
    pub result: Option<String>,
}

/// Waits until command `id` is `done` or `failed` (an agent answers in its
/// next heartbeat), polling every 250 ms, at most `timeout`.
///
/// # Errors
/// Timeout (`node_unreachable`), unknown command, or database errors.
pub async fn wait_command(control: &dyn Backend, id: &str, timeout: Duration) -> ServiceResult<CommandOutcome> {
    let deadline = Instant::now() + timeout;
    loop {
        let rows = control
            .fetch_all(&Statement::new(
                "SELECT state, result FROM _forgebase_db_node_commands WHERE id = ?1",
                vec![id.into()],
            ))
            .await?;
        let r = rows
            .rows
            .first()
            .ok_or_else(|| ServiceError::Invalid(format!("unknown command {id}")))?;
        let state = r.first().and_then(Value::as_str).unwrap_or_default().to_owned();
        if state == "done" || state == "failed" || state == "cancelled" {
            return Ok(CommandOutcome {
                state,
                result: r.get(1).and_then(Value::as_str).map(str::to_owned),
            });
        }
        if Instant::now() >= deadline {
            return Err(ServiceError::Unreachable(format!(
                "the agent did not answer command {id} within {timeout:?}"
            )));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_values_match_the_migration_checks() {
        let pg = include_str!("../../../../migrations/postgres/0043_db_node_agents.sql");
        let lite = include_str!("../../../../migrations/system/0043_db_node_agents.sql");
        for sql in [pg, lite] {
            for k in CommandKind::ALL {
                assert!(sql.contains(&format!("'{}'", k.as_str())), "{k:?}");
            }
            for s in AgentState::ALL {
                assert!(sql.contains(&format!("'{}'", s.as_str())), "{s:?}");
            }
        }
        for k in CommandKind::ALL {
            assert_eq!(CommandKind::parse(k.as_str()), Some(*k));
            assert_eq!(serde_json::to_value(k).expect("json"), k.as_str());
        }
        assert_eq!(AgentState::parse("nope"), None);
    }

    #[test]
    fn heartbeat_transitions() {
        use AgentState as A;
        use NodeState as N;
        assert_eq!(
            state_after_heartbeat(N::Provisioning, A::Streaming),
            Some(N::CatchingUp)
        );
        assert_eq!(state_after_heartbeat(N::Provisioning, A::Cloning), None);
        assert_eq!(state_after_heartbeat(N::Streaming, A::Streaming), None);
        assert_eq!(state_after_heartbeat(N::Lagging, A::CatchingUp), None);
        assert_eq!(
            state_after_heartbeat(N::Streaming, A::Rebuilding),
            Some(N::Provisioning)
        );
        assert_eq!(state_after_heartbeat(N::Streaming, A::Stopped), Some(N::Unreachable));
        assert_eq!(state_after_heartbeat(N::Unreachable, A::Streaming), Some(N::CatchingUp));
        assert_eq!(state_after_heartbeat(N::Streaming, A::Failed), Some(N::NeedsRebuild));
        assert_eq!(state_after_heartbeat(N::Promoting, A::Failed), None);
        assert_eq!(state_after_heartbeat(N::Retired, A::Streaming), None);
        assert_eq!(state_after_heartbeat(N::Streaming, A::Primary), None);
    }

    #[test]
    fn errors_have_codes_and_statuses() {
        assert_eq!(ServiceError::TokenInvalid.status(), 401);
        assert_eq!(ServiceError::EngineMismatch("x".into()).code(), "engine_mismatch");
        assert_eq!(ServiceError::EngineMismatch("x".into()).status(), 409);
        assert_eq!(ServiceError::PostgresRequired.status(), 400);
        assert_eq!(ServiceError::NodeNotFound("n".into()).status(), 404);
        assert_eq!(ServiceError::Unreachable("n".into()).status(), 503);
    }

    #[test]
    fn secrets_and_hashes() {
        let t = random_secret(JOIN_TOKEN_PREFIX).expect("token");
        assert!(t.starts_with("fbj_") && t.len() == 68);
        assert_eq!(sha256_hex(&t).len(), 64);
        assert_ne!(t, random_secret(JOIN_TOKEN_PREFIX).expect("token"));
        let u = reader_url("10.0.0.7:5433", &Secret::new("pw"), SslMode::VerifyFull);
        assert_eq!(
            u.expose(),
            "postgres://forgebase_reader:pw@10.0.0.7:5433/forgebase?sslmode=verify-full"
        );
    }
}
