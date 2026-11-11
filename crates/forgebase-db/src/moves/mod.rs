//! Online tenant moves between database sets (ADR-0029 §3.8, FB-174).
//!
//! A move copies one tenant's rows from its set (`from`) to another set
//! (`to`) while the tenant keeps working, then switches it over in a short
//! freeze. Job `database.tenant.move` drives it; this module is the engine:
//!
//! 1. **Prepare** ([`begin`], then [`run`]): placement `moving`; a plan of
//!    every table holding the tenant's rows ([`plan`]); `REPLICA IDENTITY
//!    FULL` where a row filter needs it; publication `fb_move_<id>` with a
//!    row filter per table; a logical slot on the source.
//! 2. **Copy**: subscription on the target (`copy_data`, `origin = none`,
//!    `binary`, `disable_on_error`, `failover` when the source has
//!    replicas); tables whose rows find their tenant through a parent row
//!    that is itself data (job history, upload parts, keyword rows, record
//!    versions, snapshot items) are *staged*: copied by the engine into
//!    staging tables on the target and reconciled by bucket hashes
//!    ([`sync`]).
//! 3. **Catch up** until every table is ready and the slot is less than
//!    [`MoveOptions::max_lag_bytes`] behind.
//! 4. **Switch** (at most [`MoveOptions::freeze_timeout`]): freeze in the
//!    router, hooks (KV flush, schedules), drain the tenant's in-flight
//!    handles, relays, barrier row through the publication, final staged
//!    sync, one target transaction (staged rows in, renumbering in source
//!    order, references remapped, sequences set, verification), flip of the
//!    placement (`epoch + 1`), unfreeze. Over the timeout: abort.
//! 5. **Cleanup** ([`cleanup`]) after the retention: the tenant's rows on
//!    the source are deleted in batches. Until then [`undo`] moves back.
//! 6. **Abort** ([`abort`]) before the flip: subscription, publication,
//!    slot and copied rows go; the placement is `active` on the source.
//!
//! Services that must cooperate (KV, schedules, queue relays, queue
//! settings, login roles, graph materialisation) are reached through
//! [`MoveHooks`], which the server implements.

pub mod plan;
pub mod rebalance;
pub mod sync;

mod engine;
mod pgsql;
mod switch;

use std::time::Duration;

use forgebase_core::{ErrorCode, ErrorKind};
use serde::{Deserialize, Serialize};

use crate::backend::{Backend, BoxFuture, Statement, TxMode, Value};
use crate::router::{RouterError, SetHandle};
use crate::topology::{DbSet, MoveState};
use crate::{Database, DbError, SecretString};

pub use engine::{BeginMove, RebalanceOutcome, abort, begin, cleanup, cleanup_due, routed_set, run, run_plan, undo};
pub use plan::{Feed, MovePlan, TablePlan};
pub use rebalance::{
    PlanOptions, PlannedMove, RebalancePlan, SetScore, TenantLoad, check_removable, drain_set, plan_drain,
    plan_rebalance,
};

/// Schema of the move's own objects (barrier and staging tables) on both
/// sets; outside `forgebase`, so the placement registry never sees them.
pub const MOVE_SCHEMA: &str = "fb_move";

/// Default freeze limit (`[database.sharding] move_freeze_timeout_ms`).
pub const DEFAULT_FREEZE_TIMEOUT: Duration = Duration::from_secs(10);

/// Default retention of the source rows (`move_source_retention_hours`).
pub const DEFAULT_SOURCE_RETENTION: Duration = Duration::from_secs(24 * 3600);

/// Tuning of a move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveOptions {
    /// Longest freeze; past it the move aborts and the tenant unfreezes.
    pub freeze_timeout: Duration,
    /// How long the source keeps the tenant's rows after the switch.
    pub source_retention: Duration,
    /// Catch-up ends when the slot is less than this behind (1 MiB).
    pub max_lag_bytes: u64,
    /// Tables up to this many of the tenant's rows are verified with a
    /// SHA-256 over every row; larger ones over a 1 % sample.
    pub verify_full_rows: u64,
    /// How long copying and catching up may take before the move aborts.
    pub copy_timeout: Duration,
    /// Rows deleted per statement at cleanup and abort.
    pub delete_batch: u32,
    /// Poll interval while copying and catching up.
    pub poll: Duration,
}

impl Default for MoveOptions {
    fn default() -> Self {
        Self {
            freeze_timeout: DEFAULT_FREEZE_TIMEOUT,
            source_retention: DEFAULT_SOURCE_RETENTION,
            max_lag_bytes: 1 << 20,
            verify_full_rows: 100_000,
            copy_timeout: Duration::from_secs(24 * 3600),
            delete_batch: 10_000,
            poll: Duration::from_millis(200),
        }
    }
}

/// Errors of moves, shard operations and rebalancing.
#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    /// A database error.
    #[error(transparent)]
    Db(#[from] DbError),
    /// A routing error.
    #[error(transparent)]
    Router(#[from] RouterError),
    /// No such move.
    #[error("move {0:?} not found")]
    NotFound(String),
    /// The tenant already has an open move, or is frozen.
    #[error("tenant {0:?} is being moved")]
    TenantMoving(String),
    /// Another topology operation holds one of the sets.
    #[error("{0}")]
    InProgress(String),
    /// The request does not fit the move's or set's state.
    #[error("{0}")]
    State(String),
    /// Invalid input (same set, unknown tenant, bad confirmation).
    #[error("{0}")]
    Invalid(String),
    /// The target already holds rows of the tenant.
    #[error("the target set already holds rows of tenant {tenant:?} in {tables:?}; clean it up first")]
    TargetNotEmpty {
        /// The tenant.
        tenant: String,
        /// Tables with rows.
        tables: Vec<String>,
    },
    /// The freeze would exceed its limit.
    #[error("the switch took longer than the freeze limit of {0} ms; the move was aborted and the tenant unfrozen")]
    FreezeTimeout(u128),
    /// Verification found different rows on the two sets.
    #[error("verification failed for {table}: {detail}")]
    Verification {
        /// Table.
        table: String,
        /// What differed.
        detail: String,
    },
    /// The subscription stopped (`disable_on_error`) or copying timed out.
    #[error("{0}")]
    Replication(String),
    /// The move was cancelled; nothing visible changed.
    #[error("the move was cancelled before the switch; nothing was changed")]
    Cancelled,
    /// A cooperating service failed.
    #[error("{0}")]
    Hook(String),
    /// Moves and shards need PostgreSQL.
    #[error("tenant moves and shards need PostgreSQL")]
    PostgresRequired,
}

impl ErrorCode for MoveError {
    fn code(&self) -> &'static str {
        match self {
            MoveError::Db(e) => e.code(),
            MoveError::Router(e) => e.code(),
            MoveError::NotFound(_) => "move_not_found",
            MoveError::TenantMoving(_) => "tenant_moving",
            MoveError::InProgress(_) => "operation_in_progress",
            MoveError::State(_) => "move_state",
            MoveError::Invalid(_) => "invalid_request",
            MoveError::TargetNotEmpty { .. } => "target_not_empty",
            MoveError::FreezeTimeout(_) => "move_freeze_timeout",
            MoveError::Verification { .. } => "move_verification_failed",
            MoveError::Replication(_) => "move_replication_failed",
            MoveError::Cancelled => "cancelled",
            MoveError::Hook(_) => "move_hook_failed",
            MoveError::PostgresRequired => "postgres_required",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            MoveError::Db(e) => e.kind(),
            MoveError::Router(e) => e.kind(),
            MoveError::NotFound(_) => ErrorKind::NotFound,
            MoveError::TenantMoving(_)
            | MoveError::InProgress(_)
            | MoveError::State(_)
            | MoveError::TargetNotEmpty { .. } => ErrorKind::Conflict,
            MoveError::Invalid(_) | MoveError::PostgresRequired => ErrorKind::InvalidInput,
            MoveError::FreezeTimeout(_) | MoveError::Replication(_) => ErrorKind::Unavailable,
            MoveError::Verification { .. } | MoveError::Hook(_) => ErrorKind::Internal,
            MoveError::Cancelled => ErrorKind::Conflict,
        }
    }
}

impl MoveError {
    /// Whether a job running the move should try again later (the database
    /// or a set was unreachable) rather than fail.
    pub fn is_transient(&self) -> bool {
        match self {
            MoveError::Db(e) => e.kind() == ErrorKind::Unavailable,
            MoveError::Router(e) => e.kind() == ErrorKind::Unavailable,
            _ => false,
        }
    }
}

/// Services that take part in a move (the server implements it). Every
/// method has a default that does nothing, except
/// [`MoveHooks::source_conninfo`].
pub trait MoveHooks: Send + Sync + std::fmt::Debug {
    /// How the target's subscription reaches the source set's primary: a
    /// libpq connection string with credentials.
    fn source_conninfo<'a>(&'a self, source: &'a DbSet) -> BoxFuture<'a, Result<SecretString, MoveError>>;

    /// A connection to `set`'s primary with the rights a move needs there
    /// (publications, replication slots, subscriptions, `REPLICA IDENTITY`,
    /// `session_replication_role`): a superuser, or a role granted them.
    /// `None` (the default): the set's own pool, which must have them. The
    /// handles the other hooks receive are built over it.
    fn admin<'a>(&'a self, set: &'a DbSet) -> BoxFuture<'a, Result<Option<Database>, MoveError>> {
        let _ = set;
        Box::pin(async { Ok(None) })
    }

    /// Before the copy: objects the target needs that are not rows (for
    /// example the tenant's direct-login roles). The engine itself copies
    /// pgmq queues and partial indexes.
    fn prepare_target<'a>(
        &'a self,
        tenant: &'a str,
        source: &'a SetHandle,
        target: &'a SetHandle,
    ) -> BoxFuture<'a, Result<(), MoveError>> {
        let _ = (tenant, source, target);
        Box::pin(async { Ok(()) })
    }

    /// Right after the freeze: write back the tenant's cached state (KV
    /// dirty keys) and stop its schedules from firing on the source.
    fn frozen<'a>(&'a self, tenant: &'a str) -> BoxFuture<'a, Result<(), MoveError>> {
        let _ = tenant;
        Box::pin(async { Ok(()) })
    }

    /// Before the barrier: relay the source's record changes into queue
    /// topics (the source's own relay and control's cursor of the source).
    fn drain<'a>(&'a self, tenant: &'a str, source: &'a SetHandle) -> BoxFuture<'a, Result<(), MoveError>> {
        let _ = (tenant, source);
        Box::pin(async { Ok(()) })
    }

    /// After the flip: the tenant runs on `target` (copy queue settings,
    /// reconcile schedules, mark graphs for rebuild, reset realtime).
    /// Failures are logged; the move stays switched.
    fn switched<'a>(
        &'a self,
        tenant: &'a str,
        source: &'a SetHandle,
        target: &'a SetHandle,
    ) -> BoxFuture<'a, Result<(), MoveError>> {
        let _ = (tenant, source, target);
        Box::pin(async { Ok(()) })
    }

    /// After an abort: the tenant runs on the source again.
    fn unfrozen<'a>(&'a self, tenant: &'a str) -> BoxFuture<'a, Result<(), MoveError>> {
        let _ = tenant;
        Box::pin(async { Ok(()) })
    }
}

/// Progress and cancel of the job running a move.
pub trait MoveControl: Send + Sync {
    /// Reports a stage (`preparing`, `copying`, …), a percentage and a line.
    fn report<'a>(&'a self, stage: &'a str, percent: f64, message: String) -> BoxFuture<'a, ()>;

    /// Whether the operator asked to cancel (honoured before the flip).
    fn cancelled(&self) -> bool;
}

/// A [`MoveControl`] that reports nothing and never cancels.
#[derive(Debug, Clone, Copy, Default)]
pub struct Quiet;

impl MoveControl for Quiet {
    fn report<'a>(&'a self, _stage: &'a str, _percent: f64, _message: String) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn cancelled(&self) -> bool {
        false
    }
}

/// One row of `_forgebase_db_moves`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MoveRecord {
    /// Move id.
    pub id: String,
    /// The tenant (environment id).
    pub tenant: String,
    /// Source set id.
    pub from_set: String,
    /// Target set id.
    pub to_set: String,
    /// Stage.
    pub state: MoveState,
    /// Job running it.
    pub job_id: Option<String>,
    /// Rebalance plan it belongs to.
    pub rebalance_id: Option<String>,
    /// Checkpoint and measurements.
    pub detail: MoveDetail,
    /// Why it was aborted.
    pub error: Option<String>,
    /// Administrator who started it.
    pub created_by: Option<String>,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339.
    pub updated_at: String,
    /// RFC 3339 of the flip.
    pub switched_at: Option<String>,
    /// RFC 3339 of `done` or `aborted`.
    pub finished_at: Option<String>,
}

impl MoveRecord {
    /// Whether the move is finished (`done` or `aborted`).
    pub fn is_finished(&self) -> bool {
        matches!(self.state, MoveState::Done | MoveState::Aborted)
    }

    /// Whether the flip happened (the tenant is on the target).
    pub fn is_switched(&self) -> bool {
        matches!(self.state, MoveState::Cleanup | MoveState::Done)
    }

    /// The replication objects' name (`fb_move_<16 hex>`).
    pub fn object_name(&self) -> String {
        object_name(&self.id)
    }
}

/// Checkpoint of a move (`_forgebase_db_moves.detail`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MoveDetail {
    /// The table plan (set at prepare).
    pub plan: Option<MovePlan>,
    /// Tables given `REPLICA IDENTITY FULL` on the source, with their
    /// previous setting (`d`, `n`, `i`), restored after the switch.
    pub replica_identity: Vec<(String, String)>,
    /// Partial indexes created on the target (dropped on the source at
    /// cleanup, on the target at abort).
    pub indexes: Vec<String>,
    /// pgmq queues created on the target.
    pub queues: Vec<String>,
    /// Whether the subscription exists on the target.
    pub subscribed: bool,
    /// Whether the slot and subscription were dropped after the switch.
    pub replication_dropped: bool,
    /// Rows copied per table (target counts after the switch).
    pub rows: std::collections::BTreeMap<String, u64>,
    /// Freeze length in milliseconds (measured).
    pub freeze_ms: Option<u64>,
    /// Parts of the freeze in milliseconds (drain, barrier, sync, switch
    /// transaction, flip).
    pub freeze_parts: std::collections::BTreeMap<String, u64>,
    /// Rows deleted on the source at cleanup.
    pub cleaned_rows: Option<u64>,
    /// The move this one undoes.
    pub undo_of: Option<String>,
}

/// The replication objects' name of move `id`.
pub fn object_name(id: &str) -> String {
    let hex: String = id
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    format!("fb_move_{}", &hex[..hex.len().min(16)])
}

const MOVE_COLUMNS: &str = "id, tenant, from_set, to_set, state, job_id, rebalance_id, CAST(detail AS TEXT) AS detail, \
                            error, created_by, created_at, updated_at, switched_at, finished_at";

fn text(row: &[Value], i: usize) -> Option<String> {
    row.get(i).and_then(Value::as_str).map(str::to_owned)
}

fn record_from_row(row: &[Value]) -> Result<MoveRecord, DbError> {
    let need = |i: usize| text(row, i).unwrap_or_default();
    let detail = text(row, 7).unwrap_or_else(|| "{}".into());
    Ok(MoveRecord {
        id: need(0),
        tenant: need(1),
        from_set: need(2),
        to_set: need(3),
        state: need(4).parse()?,
        job_id: text(row, 5),
        rebalance_id: text(row, 6),
        detail: serde_json::from_str(&detail).unwrap_or_default(),
        error: text(row, 8),
        created_by: text(row, 9),
        created_at: need(10),
        updated_at: need(11),
        switched_at: text(row, 12),
        finished_at: text(row, 13),
    })
}

/// One move by id.
pub async fn get(control: &dyn Backend, id: &str) -> Result<MoveRecord, MoveError> {
    let rows = control
        .fetch_all(&Statement::new(
            format!("SELECT {MOVE_COLUMNS} FROM _forgebase_db_moves WHERE id = ?1"),
            vec![id.into()],
        ))
        .await?;
    match rows.rows.first() {
        Some(r) => Ok(record_from_row(r)?),
        None => Err(MoveError::NotFound(id.to_owned())),
    }
}

/// Filters of [`list`].
#[derive(Debug, Clone, Default)]
pub struct MoveQuery {
    /// Only this tenant's moves.
    pub tenant: Option<String>,
    /// Only open moves (not `done` / `aborted`).
    pub open: bool,
    /// Only moves of this rebalance.
    pub rebalance_id: Option<String>,
    /// At most this many (default 100).
    pub limit: Option<u32>,
}

/// Moves, newest first.
pub async fn list(control: &dyn Backend, q: &MoveQuery) -> Result<Vec<MoveRecord>, MoveError> {
    let mut sql = format!("SELECT {MOVE_COLUMNS} FROM _forgebase_db_moves WHERE 1 = 1");
    let mut params: Vec<Value> = Vec::new();
    if let Some(t) = &q.tenant {
        params.push(t.clone().into());
        sql.push_str(&format!(" AND tenant = ?{}", params.len()));
    }
    if let Some(r) = &q.rebalance_id {
        params.push(r.clone().into());
        sql.push_str(&format!(" AND rebalance_id = ?{}", params.len()));
    }
    if q.open {
        sql.push_str(" AND state <> 'done' AND state <> 'aborted'");
    }
    sql.push_str(&format!(
        " ORDER BY created_at DESC, id DESC LIMIT {}",
        q.limit.unwrap_or(100).clamp(1, 1000)
    ));
    let rows = control.fetch_all(&Statement::new(sql, params)).await?;
    rows.rows
        .iter()
        .map(|r| record_from_row(r).map_err(MoveError::from))
        .collect()
}

/// Writes a move's state, detail and error (one statement).
async fn store(
    control: &dyn Backend,
    id: &str,
    state: MoveState,
    detail: &MoveDetail,
    error: Option<&str>,
) -> Result<MoveRecord, MoveError> {
    let now = forgebase_core::now_rfc3339();
    let finished = matches!(state, MoveState::Done | MoveState::Aborted);
    let detail = serde_json::to_string(detail).map_err(|e| DbError::Statement(e.to_string()))?;
    let mut tx = control.begin(TxMode::Normal).await?;
    let rows = tx
        .fetch_all(&Statement::new(
            format!(
                "UPDATE _forgebase_db_moves SET state = ?2, detail = CAST(CAST(?3 AS TEXT) AS jsonb), \
                 error = COALESCE(CAST(?4 AS TEXT), error), updated_at = ?5, \
                 switched_at = CASE WHEN ?2 = 'cleanup' AND switched_at IS NULL THEN ?5 ELSE switched_at END, \
                 finished_at = CASE WHEN ?6 = 1 THEN ?5 ELSE finished_at END \
                 WHERE id = ?1 RETURNING {MOVE_COLUMNS}"
            ),
            vec![
                id.into(),
                state.as_str().into(),
                detail.into(),
                error.map(str::to_owned).into(),
                now.into(),
                i64::from(finished).into(),
            ],
        ))
        .await?;
    tx.commit().await?;
    match rows.rows.first() {
        Some(r) => Ok(record_from_row(r)?),
        None => Err(MoveError::NotFound(id.to_owned())),
    }
}

/// Sets the job id of a move (the server enqueues the job after [`begin`]).
pub async fn set_job(control: &dyn Backend, id: &str, job_id: &str) -> Result<(), MoveError> {
    let mut tx = control.begin(TxMode::Normal).await?;
    tx.execute(&Statement::new(
        "UPDATE _forgebase_db_moves SET job_id = ?2 WHERE id = ?1",
        vec![id.into(), job_id.into()],
    ))
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Requires PostgreSQL on `db`.
fn need_postgres(db: &Database) -> Result<(), MoveError> {
    if db.kind() == crate::BackendKind::Postgres {
        Ok(())
    } else {
        Err(MoveError::PostgresRequired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_names_are_short_identifiers() {
        assert_eq!(
            object_name("mv_0123456789abcdef0123456789abcdef"),
            "fb_move_0123456789abcdef"
        );
        assert!(object_name("mv_AB").starts_with("fb_move_ab"));
        assert!(object_name(&"f".repeat(100)).len() <= 63);
    }

    #[test]
    fn errors_have_codes() {
        assert_eq!(MoveError::TenantMoving("t".into()).code(), "tenant_moving");
        assert_eq!(MoveError::TenantMoving("t".into()).kind(), ErrorKind::Conflict);
        assert_eq!(MoveError::FreezeTimeout(10).code(), "move_freeze_timeout");
        assert_eq!(MoveError::InProgress("x".into()).code(), "operation_in_progress");
        assert_eq!(MoveError::PostgresRequired.kind(), ErrorKind::InvalidInput);
        assert!(MoveError::Db(DbError::Unavailable("x".into())).is_transient());
        assert!(!MoveError::Cancelled.is_transient());
    }

    #[test]
    fn details_round_trip() {
        let d = MoveDetail {
            freeze_ms: Some(42),
            rows: [("a".to_owned(), 3)].into_iter().collect(),
            ..MoveDetail::default()
        };
        let s = serde_json::to_string(&d).unwrap();
        let back: MoveDetail = serde_json::from_str(&s).unwrap();
        assert_eq!(back, d);
        let empty: MoveDetail = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, MoveDetail::default());
    }
}
