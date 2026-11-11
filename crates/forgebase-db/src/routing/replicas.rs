//! The replicas of one set: pools, health monitor, choice and fallback
//! (ADR-0029 §2.5–§2.7).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde::Serialize;

use super::health::{Health, LsnHistory, Sample};
use super::{Consistency, LsnWait, ReadIntent, ReadRouting, ReplicaOptions, scope};
use crate::backend::{Backend, BoxFuture, ExecResult, Rows, Statement, Value, WriteTx};
use crate::routing::Lsn;
use crate::topology::{self, DbNode, NodeState};
use crate::{Database, DbError};

/// Reads served, by set and target (`replica` or `primary`).
pub const READS_METRIC: &str = "forgebase_db_routed_reads_total";
/// Replica-eligible reads that ran on the primary, by set and reason.
pub const FALLBACKS_METRIC: &str = "forgebase_db_replica_fallbacks_total";

const SAMPLE_SQL: &str =
    "SELECT CASE WHEN pg_is_in_recovery() THEN 1 ELSE 0 END, COALESCE(pg_last_wal_replay_lsn()::text, '')";
const REPLAY_SQL: &str = "SELECT COALESCE(pg_last_wal_replay_lsn()::text, '')";

/// One replica to route to: its topology row and a read-only
/// [`Database`] on it (the connector opens it with `DbOptions::read_only`).
#[derive(Debug, Clone)]
pub struct ReplicaNode {
    /// The node as the topology describes it.
    pub node: DbNode,
    /// The standby.
    pub db: Database,
}

/// Why a read a replica could have served ran on the primary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackReason {
    /// `consistency: strong`.
    Strong,
    /// No replica in rotation.
    NoReplica,
    /// The replicas had not replayed the read's LSN within `lsn_wait`.
    Behind,
    /// The replica did not answer.
    Unavailable,
    /// The replica cancelled the statement (recovery conflict) or was busy.
    RecoveryConflict,
}

impl FallbackReason {
    /// Snake-case name (metric label).
    pub fn as_str(self) -> &'static str {
        match self {
            FallbackReason::Strong => "strong",
            FallbackReason::NoReplica => "no_replica",
            FallbackReason::Behind => "behind",
            FallbackReason::Unavailable => "unavailable",
            FallbackReason::RecoveryConflict => "recovery_conflict",
        }
    }
}

/// Live state of one replica (the topology screen and `/api/system/database`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplicaStatus {
    /// Node id.
    pub node_id: String,
    /// Node name.
    pub name: String,
    /// Health state (`streaming` serves reads).
    pub state: NodeState,
    /// Serves reads now.
    pub in_rotation: bool,
    /// Share of reads.
    pub weight: u32,
    /// Last replay position seen.
    pub replay_lsn: Option<Lsn>,
    /// Time lag at the last sample, in milliseconds.
    pub lag_ms: u64,
    /// WAL bytes behind at the last sample.
    pub lag_bytes: u64,
    /// Reads running on it now.
    pub in_flight: usize,
    /// Reads it served.
    pub reads: u64,
    /// Reads that failed on it and were retried on the primary.
    pub fallbacks: u64,
    /// The latest failure, while out of rotation.
    pub last_error: Option<String>,
}

#[derive(Debug)]
struct Replica {
    node: DbNode,
    db: Database,
    health: Mutex<Health>,
    replay: AtomicU64,
    in_flight: AtomicUsize,
    reads: AtomicU64,
    fallbacks: AtomicU64,
}

impl Replica {
    fn health(&self) -> std::sync::MutexGuard<'_, Health> {
        self.health.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn in_rotation(&self) -> bool {
        self.node.weight > 0 && self.health().in_rotation()
    }

    /// Every read connection of the replica is taken by our reads: a new
    /// one would queue behind the pool's acquire timeout.
    fn saturated(&self) -> bool {
        let max = self.db.pool_stats().reader.max.max(1) as usize;
        self.in_flight.load(Ordering::Acquire) >= max
    }

    fn replay(&self) -> Lsn {
        Lsn(self.replay.load(Ordering::Acquire))
    }
}

#[derive(Debug)]
struct Inner {
    set_id: String,
    primary: Database,
    replicas: Vec<Replica>,
    options: ReplicaOptions,
    /// Control, where health transitions are recorded (`None`: memory only).
    persist: Option<Database>,
    history: Mutex<LsnHistory>,
    next: AtomicUsize,
    monitor: Mutex<Option<tokio::task::AbortHandle>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(h) = self.monitor.lock().unwrap_or_else(|p| p.into_inner()).take() {
            h.abort();
        }
    }
}

/// The replicas of one set and the primary they follow. Cheap to clone;
/// the health monitor stops when the last clone is dropped.
#[derive(Debug, Clone)]
pub struct ReplicaSet {
    inner: Arc<Inner>,
}

/// Counts a read in flight on a replica.
struct InFlight<'a>(&'a AtomicUsize);

impl<'a> InFlight<'a> {
    fn new(n: &'a AtomicUsize) -> Self {
        n.fetch_add(1, Ordering::AcqRel);
        Self(n)
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

enum Choice {
    /// A replica that has replayed the read's LSN.
    Ready(usize),
    /// The best replica in rotation, behind the read's LSN.
    Behind(usize),
    /// No replica in rotation.
    None,
}

/// A read that fails on a replica for these reasons is retried once on the
/// primary: the replica is gone or overloaded, or cancelled the statement
/// for a recovery conflict (SQLSTATE `40001`/`40P01` on a standby, which
/// [`DbError::Busy`] carries). Statement errors would fail on the primary too.
fn retry_reason(e: &DbError) -> Option<FallbackReason> {
    match e {
        DbError::Busy(_) => Some(FallbackReason::RecoveryConflict),
        DbError::Unavailable(_) | DbError::Closed | DbError::Sqlx(_) => Some(FallbackReason::Unavailable),
        _ => None,
    }
}

fn parse_lsn(v: Option<&Value>) -> Option<Lsn> {
    v.and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .and_then(|s| s.parse().ok())
}

impl ReplicaSet {
    /// Routes reads of set `set_id` over `nodes`, following `primary`.
    /// Takes one health sample before it returns (so healthy replicas serve
    /// at once), then samples every `options.health_interval` in the
    /// background. Health transitions are written to `persist` (control)
    /// when given.
    pub async fn open(
        set_id: &str,
        primary: Database,
        nodes: Vec<ReplicaNode>,
        options: ReplicaOptions,
        persist: Option<Database>,
    ) -> Self {
        let replicas = nodes
            .into_iter()
            .map(|n| Replica {
                node: n.node,
                db: n.db,
                health: Mutex::new(Health::default()),
                replay: AtomicU64::new(0),
                in_flight: AtomicUsize::new(0),
                reads: AtomicU64::new(0),
                fallbacks: AtomicU64::new(0),
            })
            .collect();
        let set = Self {
            inner: Arc::new(Inner {
                set_id: set_id.to_owned(),
                primary,
                replicas,
                options,
                persist,
                history: Mutex::new(LsnHistory::default()),
                next: AtomicUsize::new(0),
                monitor: Mutex::new(None),
            }),
        };
        set.refresh().await;
        if !options.health_interval.is_zero() {
            let weak = Arc::downgrade(&set.inner);
            let task = tokio::spawn(monitor(weak, options.health_interval));
            *set.inner.monitor.lock().unwrap_or_else(|p| p.into_inner()) = Some(task.abort_handle());
        }
        set
    }

    /// The set these replicas belong to.
    pub fn set_id(&self) -> &str {
        &self.inner.set_id
    }

    /// The options in force.
    pub fn options(&self) -> &ReplicaOptions {
        &self.inner.options
    }

    /// Number of replicas.
    pub fn len(&self) -> usize {
        self.inner.replicas.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.inner.replicas.is_empty()
    }

    /// Node ids, sorted.
    pub fn node_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.inner.replicas.iter().map(|r| r.node.id.clone()).collect();
        ids.sort();
        ids
    }

    /// The open standby databases by node id (a reload keeps them).
    pub fn node_databases(&self) -> Vec<(String, Database)> {
        self.inner
            .replicas
            .iter()
            .map(|r| (r.node.id.clone(), r.db.clone()))
            .collect()
    }

    /// Live state of every replica.
    pub fn status(&self) -> Vec<ReplicaStatus> {
        self.inner
            .replicas
            .iter()
            .map(|r| {
                let h = r.health().clone();
                let replay = r.replay();
                ReplicaStatus {
                    node_id: r.node.id.clone(),
                    name: r.node.name.clone(),
                    state: h.state,
                    in_rotation: r.node.weight > 0 && h.in_rotation(),
                    weight: r.node.weight,
                    replay_lsn: (replay.0 != 0).then_some(replay),
                    lag_ms: u64::try_from(h.lag.as_millis()).unwrap_or(u64::MAX),
                    lag_bytes: h.lag_bytes,
                    in_flight: r.in_flight.load(Ordering::Acquire),
                    reads: r.reads.load(Ordering::Acquire),
                    fallbacks: r.fallbacks.load(Ordering::Acquire),
                    last_error: h.last_error,
                }
            })
            .collect()
    }

    /// Takes one health sample of every replica now.
    pub async fn refresh(&self) {
        sample(&self.inner).await;
    }

    /// Runs a read: on a replica when `intent` allows it and one is
    /// eligible, otherwise (and after a replica failure) on the primary.
    pub async fn fetch(&self, stmt: &Statement, intent: &ReadIntent) -> Result<Rows, DbError> {
        let inner = &self.inner;
        if !intent.replica || inner.options.read_routing == ReadRouting::Primary {
            return inner.primary.fetch_all(stmt).await;
        }
        let scope = scope::current();
        let strong = intent.consistency == Consistency::Strong
            || scope.as_ref().is_some_and(|s| s.consistency() == Consistency::Strong);
        if strong {
            return self.on_primary(stmt, FallbackReason::Strong).await;
        }
        let min = if inner.options.read_your_writes {
            let from_scope = scope.as_ref().and_then(|s| s.read_after_of(&inner.set_id));
            intent.read_after.get(&inner.set_id).max(from_scope)
        } else {
            None
        };
        let index = match self.choose(min) {
            Choice::Ready(i) => i,
            Choice::Behind(i) => match min {
                Some(lsn) if self.wait_for(i, lsn).await => i,
                _ => return self.on_primary(stmt, FallbackReason::Behind).await,
            },
            Choice::None => return self.on_primary(stmt, FallbackReason::NoReplica).await,
        };
        let replica = &inner.replicas[index];
        let result = {
            let _guard = InFlight::new(&replica.in_flight);
            replica.db.fetch_all(stmt).await
        };
        match result {
            Ok(rows) => {
                replica.reads.fetch_add(1, Ordering::AcqRel);
                metrics::counter!(READS_METRIC, "set" => inner.set_id.clone(), "target" => "replica").increment(1);
                Ok(rows)
            }
            Err(e) => match retry_reason(&e) {
                Some(reason) => {
                    replica.fallbacks.fetch_add(1, Ordering::AcqRel);
                    tracing::debug!(
                        set = %inner.set_id, node = %replica.node.name, reason = reason.as_str(), error = %e,
                        "replica read failed; retrying on the primary"
                    );
                    if reason == FallbackReason::Unavailable {
                        let change = replica.health().mark_unreachable(e.to_string());
                        if let Some(state) = change {
                            transition(inner, replica, state);
                        }
                    }
                    self.on_primary(stmt, reason).await
                }
                None => Err(e),
            },
        }
    }

    async fn on_primary(&self, stmt: &Statement, reason: FallbackReason) -> Result<Rows, DbError> {
        let set = self.inner.set_id.clone();
        metrics::counter!(FALLBACKS_METRIC, "set" => set.clone(), "reason" => reason.as_str()).increment(1);
        metrics::counter!(READS_METRIC, "set" => set, "target" => "primary").increment(1);
        self.inner.primary.fetch_all(stmt).await
    }

    /// The replica for a read needing `min`: among those in rotation that
    /// have replayed it, the lowest in-flight reads per weight (ties rotate);
    /// else the most advanced one in rotation, to wait on.
    fn choose(&self, min: Option<Lsn>) -> Choice {
        let replicas = &self.inner.replicas;
        let n = replicas.len();
        if n == 0 {
            return Choice::None;
        }
        let start = self.inner.next.fetch_add(1, Ordering::Relaxed) % n;
        let mut best: Option<(usize, f64)> = None;
        let mut behind: Option<(usize, Lsn)> = None;
        for k in 0..n {
            let i = (start + k) % n;
            let r = &replicas[i];
            if !r.in_rotation() || r.saturated() {
                continue;
            }
            let replay = r.replay();
            if min.is_some_and(|m| replay < m) {
                if behind.is_none_or(|(_, l)| replay > l) {
                    behind = Some((i, replay));
                }
                continue;
            }
            let score = (r.in_flight.load(Ordering::Acquire) as f64 + 1.0) / f64::from(r.node.weight.max(1));
            if best.is_none_or(|(_, s)| score < s) {
                best = Some((i, score));
            }
        }
        match (best, behind) {
            (Some((i, _)), _) => Choice::Ready(i),
            (None, Some((i, _))) => Choice::Behind(i),
            (None, None) => Choice::None,
        }
    }

    /// Waits up to `lsn_wait` for replica `index` to replay `lsn`: `WAIT
    /// FOR LSN` on PostgreSQL 19 and newer, else polling
    /// `pg_last_wal_replay_lsn()` every `lsn_poll`.
    async fn wait_for(&self, index: usize, lsn: Lsn) -> bool {
        let opts = &self.inner.options;
        let r = &self.inner.replicas[index];
        let deadline = Instant::now() + opts.lsn_wait;
        match r.db.wait_for_lsn(lsn, opts.lsn_wait).await {
            Ok(LsnWait::Reached) => {
                r.replay.fetch_max(lsn.0, Ordering::AcqRel);
                return true;
            }
            Ok(LsnWait::TimedOut) => return false,
            Ok(LsnWait::Unsupported) => {}
            Err(e) => {
                tracing::debug!(node = %r.node.name, error = %e, "WAIT FOR LSN failed; polling instead");
            }
        }
        loop {
            let replay = match r.db.fetch_all(&Statement::new(REPLAY_SQL, Vec::new())).await {
                Ok(rows) => parse_lsn(rows.scalar()),
                Err(_) => return false,
            };
            if let Some(replay) = replay {
                r.replay.fetch_max(replay.0, Ordering::AcqRel);
                if replay >= lsn {
                    return true;
                }
            }
            let now = Instant::now();
            if now + opts.lsn_poll > deadline {
                return false;
            }
            tokio::time::sleep(opts.lsn_poll).await;
        }
    }

    /// Records the latest commit on this set's primary in the request scope
    /// (the API answers it as `X-ForgeBase-Commit`).
    pub(crate) fn record_commit(&self) {
        if let Some(lsn) = self.inner.primary.last_commit_lsn() {
            scope::record_commit(&self.inner.set_id, lsn);
        }
    }

    /// Wraps a write transaction on this set's primary so its commit is
    /// recorded ([`ReplicaSet::record_commit`]).
    pub(crate) fn wrap_tx(&self, tx: Box<dyn WriteTx>) -> Box<dyn WriteTx> {
        Box::new(TokenTx {
            tx,
            replicas: self.clone(),
        })
    }
}

/// A write transaction whose commit records a token.
struct TokenTx {
    tx: Box<dyn WriteTx>,
    replicas: ReplicaSet,
}

impl WriteTx for TokenTx {
    fn execute<'a>(&'a mut self, stmt: &'a Statement) -> BoxFuture<'a, Result<ExecResult, DbError>> {
        self.tx.execute(stmt)
    }

    fn fetch_all<'a>(&'a mut self, stmt: &'a Statement) -> BoxFuture<'a, Result<Rows, DbError>> {
        self.tx.fetch_all(stmt)
    }

    fn commit(self: Box<Self>) -> BoxFuture<'static, Result<(), DbError>> {
        let TokenTx { tx, replicas } = *self;
        Box::pin(async move {
            tx.commit().await?;
            replicas.record_commit();
            Ok(())
        })
    }

    fn rollback(self: Box<Self>) -> BoxFuture<'static, Result<(), DbError>> {
        self.tx.rollback()
    }
}

async fn monitor(weak: Weak<Inner>, interval: Duration) {
    loop {
        tokio::time::sleep(interval).await;
        let Some(inner) = weak.upgrade() else {
            return;
        };
        sample(&inner).await;
    }
}

/// One health sample of every replica, against the primary's WAL position.
async fn sample(inner: &Arc<Inner>) {
    let primary_lsn = match inner.primary.current_wal_lsn().await {
        Ok(Some(lsn)) => lsn,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(set = %inner.set_id, error = %e, "replica health: the primary did not answer");
            return;
        }
    };
    inner
        .history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(Instant::now(), primary_lsn);
    let timeout = (inner.options.health_interval * 2).max(Duration::from_secs(1));
    let mut probes = tokio::task::JoinSet::new();
    for (i, r) in inner.replicas.iter().enumerate() {
        let db = r.db.clone();
        probes.spawn(async move {
            let result = tokio::time::timeout(timeout, db.fetch_all(&Statement::new(SAMPLE_SQL, Vec::new()))).await;
            (i, result)
        });
    }
    while let Some(joined) = probes.join_next().await {
        let Ok((i, result)) = joined else { continue };
        let r = &inner.replicas[i];
        let sample = match result {
            Err(_) => Sample::Unreachable(format!("no answer within {} ms", timeout.as_millis())),
            Ok(Err(e)) => Sample::Unreachable(e.to_string()),
            Ok(Ok(rows)) => {
                let row = rows.rows.first();
                let in_recovery = row.and_then(|r| r.first()).and_then(Value::as_i64) == Some(1);
                match (in_recovery, parse_lsn(row.and_then(|r| r.get(1)))) {
                    (false, _) => Sample::NotStandby,
                    (true, replay) => {
                        let replay = replay.unwrap_or_default();
                        r.replay.store(replay.0, Ordering::Release);
                        let lag = inner
                            .history
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .lag(replay, Instant::now());
                        Sample::Standby {
                            replay,
                            lag,
                            lag_bytes: primary_lsn.0.saturating_sub(replay.0),
                        }
                    }
                }
            }
        };
        let change = r.health().observe(&sample, &inner.options);
        if let Some(state) = change {
            transition(inner, r, state);
        }
    }
}

/// Logs a health transition and records it on control in the background
/// (a read never waits for the writer; hysteresis keeps transitions rare).
fn transition(inner: &Inner, r: &Replica, state: NodeState) {
    let error = r.health().last_error.clone();
    if state == NodeState::Streaming {
        tracing::info!(set = %inner.set_id, node = %r.node.name, "replica in rotation");
    } else {
        tracing::warn!(
            set = %inner.set_id, node = %r.node.name, state = state.as_str(), error = error.as_deref().unwrap_or(""),
            "replica out of rotation"
        );
    }
    let Some(control) = inner.persist.clone() else {
        return;
    };
    let id = r.node.id.clone();
    let persist = async move {
        if let Err(e) = topology::update_node_state(&control, &id, state).await {
            tracing::warn!(node = %id, error = %e, "could not record the replica's health state");
        }
    };
    tokio::spawn(persist);
}
