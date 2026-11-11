//! Two-phase commit across database sets (ADR-0029 §3.7, FB-173).
//!
//! A schema change on a sharded cluster (collection DDL, native policies)
//! must land on control and on every shard, or nowhere:
//!
//! 1. each node's transaction does its work and ends with `PREPARE
//!    TRANSACTION 'fb_<uuid>_<set number>'` ([`TwoPhase::prepare`]);
//! 2. once all are prepared, the coordinator commits a row in control's
//!    `_forgebase_xact_journal` (`gid`, `nodes`, `state = committing`): that
//!    row **is** the decision ([`TwoPhase::commit`]);
//! 3. then `COMMIT PREPARED` everywhere, and the row becomes `committed`.
//!
//! A failure before the journal row rolls every prepared part back. A crash
//! of the coordinator or of a node after it leaves prepared transactions
//! ("in doubt"); [`recover`] (at start and every minute, [`spawn_recovery`])
//! commits those with a journal row and rolls back those without one, once
//! they are older than a grace period and no coordinator of this process is
//! still working on them. [`in_doubt`] lists them for `forgebase doctor`.
//!
//! One participant (no shards, SQLite) never uses two-phase commit:
//! [`run`] commits that one transaction normally, so single-set deployments
//! behave exactly as before.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use forgebase_core::{ErrorCode, ErrorKind};
use serde::{Deserialize, Serialize};

use crate::backend::{Backend, BoxFuture, Statement, TxMode, Value, WriteTx};
use crate::router::{DbRouter, SetHandle};
use crate::topology::XactState;
use crate::{BackendKind, Database, DbError, Inner};

/// Prefix of every ForgeBase prepared-transaction identifier.
pub const GID_PREFIX: &str = "fb_";
/// How often [`spawn_recovery`] runs (and once at start).
pub const RECOVERY_INTERVAL: Duration = Duration::from_secs(60);
/// A prepared transaction without a journal row is rolled back only once
/// it is this old (a coordinator of another process may still be writing
/// the row; this process's own coordinators are skipped by name).
pub const RECOVERY_GRACE: Duration = Duration::from_secs(30);
/// Committed journal rows are kept this long, then pruned by [`recover`].
pub const JOURNAL_RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);
/// [`in_doubt`] reports a `committing` row older than this.
pub const STUCK_AFTER: Duration = Duration::from_secs(300);

/// One node taking part in a two-phase transaction: a set's primary.
#[derive(Debug, Clone)]
pub struct Participant {
    /// Set id (`control` for control).
    pub set_id: String,
    /// Set number *k* (the suffix of its prepared-transaction identifier).
    pub number: u16,
    /// The set's primary.
    pub db: Database,
}

impl Participant {
    /// A participant.
    pub fn new(set_id: impl Into<String>, number: u16, db: Database) -> Self {
        Self {
            set_id: set_id.into(),
            number,
            db,
        }
    }
}

impl From<&SetHandle> for Participant {
    fn from(h: &SetHandle) -> Self {
        Self::new(h.id(), h.number(), h.database().clone())
    }
}

/// Every open set of `router`, control first (the participants of a
/// cluster-wide schema change).
pub fn participants(router: &DbRouter) -> Vec<Participant> {
    router.sets().iter().map(Participant::from).collect()
}

/// A new transaction identifier: `fb_` and 32 hex digits.
pub fn new_gid() -> String {
    format!("{GID_PREFIX}{}", uuid::Uuid::new_v4().simple())
}

/// The identifier of `gid`'s part on set `number`: `<gid>_<number>`.
pub fn node_gid(gid: &str, number: u16) -> String {
    format!("{gid}_{number}")
}

/// Splits a node identifier into the transaction and the set number.
pub fn parse_node_gid(node: &str) -> Option<(&str, u16)> {
    let (gid, number) = node.rsplit_once('_')?;
    let hex = gid.strip_prefix(GID_PREFIX)?;
    if hex.len() != 32 || !hex.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return None;
    }
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((gid, number.parse().ok()?))
}

/// Coordinators of this process still working: [`recover`] leaves their
/// prepared parts alone.
static IN_FLIGHT: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

#[derive(Debug)]
struct InFlight(String);

impl InFlight {
    fn new(gid: &str) -> Self {
        IN_FLIGHT
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(gid.to_owned());
        Self(gid.to_owned())
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner()).remove(&self.0);
    }
}

fn in_flight(gid: &str) -> bool {
    IN_FLIGHT.lock().unwrap_or_else(|p| p.into_inner()).contains(gid)
}

/// Two-phase commit errors.
#[derive(Debug, thiserror::Error)]
pub enum TwoPhaseError {
    /// Failed before the decision: every part was rolled back.
    #[error(transparent)]
    Db(#[from] DbError),
    /// A node cannot take part (`max_prepared_transactions = 0`).
    #[error(
        "database set {0:?} has max_prepared_transactions = 0; shards need it above 0 (managed nodes use 16)"
    )]
    Unsupported(String),
    /// Writing the decision failed in a way that leaves it unknown: the
    /// prepared parts stay until [`recover`] finds the journal row (commit)
    /// or not (roll back).
    #[error("the outcome of schema change {gid} is not known yet; recovery finishes it within a minute: {source}")]
    Unknown {
        /// Transaction identifier.
        gid: String,
        /// What failed.
        #[source]
        source: DbError,
    },
}

impl ErrorCode for TwoPhaseError {
    fn code(&self) -> &'static str {
        match self {
            TwoPhaseError::Db(e) => e.code(),
            TwoPhaseError::Unsupported(_) => "sharding_unsupported",
            TwoPhaseError::Unknown { .. } => "schema_change_in_doubt",
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            TwoPhaseError::Db(e) => e.kind(),
            TwoPhaseError::Unsupported(_) => ErrorKind::InvalidInput,
            TwoPhaseError::Unknown { .. } => ErrorKind::Unavailable,
        }
    }
}

impl From<TwoPhaseError> for DbError {
    fn from(e: TwoPhaseError) -> Self {
        match e {
            TwoPhaseError::Db(e) => e,
            TwoPhaseError::Unsupported(set) => DbError::Unsupported(format!(
                "database set {set:?} has max_prepared_transactions = 0; shards need it above 0"
            )),
            other @ TwoPhaseError::Unknown { .. } => DbError::Unavailable(other.to_string()),
        }
    }
}

/// A committed two-phase transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Committed {
    /// Transaction identifier (`fb_<uuid>`).
    pub gid: String,
    /// Every participating set.
    pub nodes: Vec<String>,
    /// Sets whose `COMMIT PREPARED` failed (unreachable): the change is
    /// decided and [`recover`] finishes it there.
    pub pending: Vec<String>,
}

/// One two-phase transaction: prepare every part, then [`TwoPhase::commit`]
/// or [`TwoPhase::abort`]. Dropped undecided, it rolls its prepared parts
/// back in the background (and recovery would, too).
#[derive(Debug)]
pub struct TwoPhase {
    control: Database,
    gid: String,
    purpose: String,
    prepared: Vec<Participant>,
    decided: bool,
    flight: Option<InFlight>,
}

impl TwoPhase {
    /// Starts a two-phase transaction coordinated from `control` (where the
    /// journal row is written). `purpose` is stored with it (for doctor).
    pub fn new(control: &Database, purpose: &str) -> Self {
        let gid = new_gid();
        Self {
            control: control.clone(),
            flight: Some(InFlight::new(&gid)),
            gid,
            purpose: purpose.chars().take(200).collect(),
            prepared: Vec::new(),
            decided: false,
        }
    }

    /// Transaction identifier.
    pub fn gid(&self) -> &str {
        &self.gid
    }

    /// Sets prepared so far.
    pub fn prepared(&self) -> Vec<String> {
        self.prepared.iter().map(|p| p.set_id.clone()).collect()
    }

    /// Ends `tx` (open on `node`'s primary) with `PREPARE TRANSACTION`.
    /// On failure `tx` is rolled back; call [`TwoPhase::abort`] next.
    pub async fn prepare(&mut self, node: &Participant, tx: Box<dyn WriteTx>) -> Result<(), DbError> {
        if self.prepared.iter().any(|p| p.number == node.number) {
            let _ = tx.rollback().await;
            return Err(DbError::Statement(format!(
                "set {} is already part of {}",
                node.set_id, self.gid
            )));
        }
        tx.prepare(node_gid(&self.gid, node.number)).await?;
        self.prepared.push(node.clone());
        Ok(())
    }

    /// Writes the decision (the journal row on control), then commits every
    /// prepared part. With one part or none, nothing is journalled.
    pub async fn commit(mut self) -> Result<Committed, TwoPhaseError> {
        let nodes: Vec<String> = self.prepared.iter().map(|p| p.set_id.clone()).collect();
        let decision = journal_insert(&self.control, &self.gid, &nodes, &self.purpose).await;
        if let Err(e) = decision {
            match journal_state(&self.control, &self.gid).await {
                Ok(Some(_)) => {}
                Ok(None) => {
                    self.rollback_all().await;
                    self.decided = true;
                    return Err(TwoPhaseError::Db(e));
                }
                Err(_) => {
                    // Unknown: leave the parts to recovery (not in flight
                    // any more, so it may resolve them).
                    self.decided = true;
                    self.flight = None;
                    return Err(TwoPhaseError::Unknown {
                        gid: self.gid.clone(),
                        source: e,
                    });
                }
            }
        }
        self.decided = true;
        let mut pending = Vec::new();
        for p in &self.prepared {
            match p.db.commit_prepared(&node_gid(&self.gid, p.number)).await {
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(gid = %self.gid, set = %p.set_id, error = %e,
                        "COMMIT PREPARED failed; recovery finishes it");
                    pending.push(p.set_id.clone());
                }
            }
        }
        if pending.is_empty()
            && let Err(e) = journal_close(&self.control, &self.gid).await
        {
            tracing::warn!(gid = %self.gid, error = %e, "could not mark the journal row committed; recovery will");
        }
        Ok(Committed {
            gid: self.gid.clone(),
            nodes,
            pending,
        })
    }

    /// Rolls every prepared part back. Parts that cannot be reached are
    /// left to [`recover`] (no journal row: rolled back). Returns them.
    pub async fn abort(mut self) -> Vec<(String, DbError)> {
        self.decided = true;
        self.rollback_all().await
    }

    async fn rollback_all(&mut self) -> Vec<(String, DbError)> {
        let mut failed = Vec::new();
        for p in std::mem::take(&mut self.prepared) {
            if let Err(e) = p.db.rollback_prepared(&node_gid(&self.gid, p.number)).await {
                tracing::warn!(gid = %self.gid, set = %p.set_id, error = %e,
                    "ROLLBACK PREPARED failed; recovery rolls it back");
                failed.push((p.set_id.clone(), e));
            }
        }
        failed
    }
}

impl Drop for TwoPhase {
    fn drop(&mut self) {
        if self.decided || self.prepared.is_empty() {
            return;
        }
        let parts = std::mem::take(&mut self.prepared);
        let gid = self.gid.clone();
        let flight = self.flight.take();
        if let Ok(rt) = tokio::runtime::Handle::try_current() {
            rt.spawn(async move {
                for p in parts {
                    let _ = p.db.rollback_prepared(&node_gid(&gid, p.number)).await;
                }
                drop(flight);
            });
        }
    }
}

async fn journal_insert(control: &Database, gid: &str, nodes: &[String], purpose: &str) -> Result<(), DbError> {
    let mut tx = control.begin(TxMode::Normal).await?;
    let r = tx
        .execute(&Statement::new(
            "INSERT INTO _forgebase_xact_journal (gid, nodes, state, purpose, created_at) \
             VALUES (?1, CAST(?2 AS jsonb), 'committing', ?3, ?4)",
            vec![
                gid.into(),
                serde_json::to_string(nodes).unwrap_or_else(|_| "[]".into()).into(),
                purpose.into(),
                forgebase_core::now_rfc3339().into(),
            ],
        ))
        .await;
    match r {
        Ok(_) => tx.commit().await,
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

async fn journal_state(control: &Database, gid: &str) -> Result<Option<XactState>, DbError> {
    let rows = control
        .fetch_all(&Statement::new(
            "SELECT state FROM _forgebase_xact_journal WHERE gid = ?1",
            vec![gid.into()],
        ))
        .await?;
    Ok(rows.scalar().and_then(Value::as_str).and_then(|s| s.parse().ok()))
}

async fn journal_close(control: &Database, gid: &str) -> Result<(), DbError> {
    let mut tx = control.begin(TxMode::Normal).await?;
    let r = tx
        .execute(&Statement::new(
            "UPDATE _forgebase_xact_journal SET state = 'committed', resolved_at = ?2 WHERE gid = ?1",
            vec![gid.into(), forgebase_core::now_rfc3339().into()],
        ))
        .await;
    match r {
        Ok(_) => tx.commit().await,
        Err(e) => {
            let _ = tx.rollback().await;
            Err(e)
        }
    }
}

/// Work done on each participant inside its transaction ([`run`]).
pub trait NodeWork<E>: Send + Sync {
    /// Runs on `node` in `tx` (begun on the node's primary).
    fn run<'a>(&'a self, node: &'a Participant, tx: &'a mut dyn WriteTx) -> BoxFuture<'a, Result<(), E>>;
}

/// The same statements on every node.
#[derive(Debug, Clone)]
pub struct SameStatements(pub Vec<Statement>);

impl NodeWork<DbError> for SameStatements {
    fn run<'a>(&'a self, _node: &'a Participant, tx: &'a mut dyn WriteTx) -> BoxFuture<'a, Result<(), DbError>> {
        Box::pin(async move {
            for s in &self.0 {
                tx.execute(s).await?;
            }
            Ok(())
        })
    }
}

/// Runs `work` on every participant (each in its own transaction of
/// `mode`) as one two-phase transaction coordinated from `control`. With a
/// single participant it is one ordinary transaction (`Ok(None)`).
/// Participants run one after the other, in the given order.
pub async fn run<E>(
    control: &Database,
    nodes: &[Participant],
    purpose: &str,
    mode: TxMode,
    work: &dyn NodeWork<E>,
) -> Result<Option<Committed>, E>
where
    E: From<DbError> + From<TwoPhaseError>,
{
    if let [only] = nodes {
        let mut tx = only.db.begin(mode).await.map_err(E::from)?;
        if let Err(e) = work.run(only, &mut *tx).await {
            let _ = tx.rollback().await;
            return Err(e);
        }
        tx.commit().await.map_err(E::from)?;
        return Ok(None);
    }
    for n in nodes {
        check_supported(n).await.map_err(E::from)?;
    }
    let mut two = TwoPhase::new(control, purpose);
    for n in nodes {
        let mut tx = match n.db.begin(mode).await {
            Ok(tx) => tx,
            Err(e) => {
                two.abort().await;
                return Err(E::from(e));
            }
        };
        if let Err(e) = work.run(n, &mut *tx).await {
            let _ = tx.rollback().await;
            two.abort().await;
            return Err(e);
        }
        if let Err(e) = two.prepare(n, tx).await {
            two.abort().await;
            return Err(E::from(e));
        }
    }
    two.commit().await.map(Some).map_err(E::from)
}

/// Refuses a node whose server has `max_prepared_transactions = 0`
/// (`400 sharding_unsupported`, ADR-0029 §3.7). SQLite is refused too.
pub async fn check_supported(node: &Participant) -> Result<(), TwoPhaseError> {
    match node.db.max_prepared_transactions().await? {
        n if n > 0 => Ok(()),
        _ => Err(TwoPhaseError::Unsupported(node.set_id.clone())),
    }
}

/// A prepared transaction on one node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreparedXact {
    /// The node's identifier (`fb_<uuid>_<number>`).
    pub node_gid: String,
    /// The transaction (`fb_<uuid>`).
    pub gid: String,
    /// Seconds since `PREPARE TRANSACTION`.
    pub age_secs: f64,
}

impl Database {
    /// ForgeBase's prepared transactions in this database (`fb_` prefix),
    /// oldest first. Empty on SQLite.
    pub async fn prepared_transactions(&self) -> Result<Vec<PreparedXact>, DbError> {
        let Inner::Postgres(db) = &*self.inner else {
            return Ok(Vec::new());
        };
        Ok(db
            .prepared_xacts(GID_PREFIX)
            .await?
            .into_iter()
            .filter_map(|(node_gid, age_secs)| {
                let gid = parse_node_gid(&node_gid)?.0.to_owned();
                Some(PreparedXact {
                    node_gid,
                    gid,
                    age_secs,
                })
            })
            .collect())
    }

    /// `COMMIT PREPARED '<node_gid>'`; `Ok(false)` if there is none by that name.
    pub async fn commit_prepared(&self, node_gid: &str) -> Result<bool, DbError> {
        match &*self.inner {
            Inner::Postgres(db) => db.finish_prepared(node_gid, true).await,
            Inner::Sqlite(_) => Err(DbError::unsupported("COMMIT PREPARED", BackendKind::Sqlite)),
        }
    }

    /// `ROLLBACK PREPARED '<node_gid>'`; `Ok(false)` if there is none by that name.
    pub async fn rollback_prepared(&self, node_gid: &str) -> Result<bool, DbError> {
        match &*self.inner {
            Inner::Postgres(db) => db.finish_prepared(node_gid, false).await,
            Inner::Sqlite(_) => Err(DbError::unsupported("ROLLBACK PREPARED", BackendKind::Sqlite)),
        }
    }

    /// The server's `max_prepared_transactions` (0 on SQLite).
    pub async fn max_prepared_transactions(&self) -> Result<i64, DbError> {
        match &*self.inner {
            Inner::Postgres(db) => Ok(db
                .setting("max_prepared_transactions")
                .await?
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0)),
            Inner::Sqlite(_) => Ok(0),
        }
    }

    /// Runs statements that may not run in a transaction block (`CREATE
    /// SUBSCRIPTION`, `CREATE INDEX CONCURRENTLY`, `DROP SUBSCRIPTION`) on a
    /// connection of their own, each in autocommit, in order, stopping at
    /// the first error. PostgreSQL only.
    pub async fn run_autocommit(&self, statements: &[String]) -> Result<(), DbError> {
        match &*self.inner {
            Inner::Postgres(db) => db.run_autocommit(statements).await,
            Inner::Sqlite(_) => Err(DbError::unsupported("autocommit statements", BackendKind::Sqlite)),
        }
    }

    /// A server setting (`current_setting(name, true)`); `None` on SQLite or
    /// when unknown.
    pub async fn server_setting(&self, name: &str) -> Result<Option<String>, DbError> {
        match &*self.inner {
            Inner::Postgres(db) => db.setting(name).await,
            Inner::Sqlite(_) => Ok(None),
        }
    }
}

/// What [`recover`] did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct RecoveryReport {
    /// Node identifiers committed (their journal row said so).
    pub committed: Vec<String>,
    /// Node identifiers rolled back (no journal row, older than the grace).
    pub rolled_back: Vec<String>,
    /// Node identifiers left alone (in flight here, or younger than the grace).
    pub skipped: Vec<String>,
    /// Sets that could not be read or finished: `(set id, error)`.
    pub unreachable: Vec<(String, String)>,
    /// Journal rows marked `committed` (no part left anywhere).
    pub closed: Vec<String>,
    /// Committed journal rows pruned (older than [`JOURNAL_RETENTION`]).
    pub pruned: u64,
}

impl RecoveryReport {
    /// Whether recovery changed anything.
    pub fn is_empty(&self) -> bool {
        self.committed.is_empty() && self.rolled_back.is_empty() && self.closed.is_empty() && self.pruned == 0
    }
}

#[derive(Debug, Clone)]
struct JournalRow {
    state: XactState,
    nodes: Vec<String>,
    created_at: String,
    purpose: String,
}

async fn journal_rows(control: &Database, gids: &BTreeSet<String>) -> Result<HashMap<String, JournalRow>, DbError> {
    // The open rows, plus the rows of the prepared parts found.
    let mut sql = String::from(
        "SELECT gid, state, CAST(nodes AS text), created_at, purpose FROM _forgebase_xact_journal WHERE state = 'committing'",
    );
    let mut params: Vec<Value> = Vec::new();
    for (i, g) in gids.iter().enumerate() {
        sql.push_str(if i == 0 { " OR gid IN (" } else { ", " });
        sql.push_str(&format!("?{}", i + 1));
        params.push(g.clone().into());
    }
    if !gids.is_empty() {
        sql.push(')');
    }
    let rows = control.fetch_all(&Statement::new(sql, params)).await?;
    let mut out = HashMap::new();
    for r in &rows.rows {
        let text = |i: usize| r.get(i).and_then(Value::as_str).unwrap_or_default().to_owned();
        let Ok(state) = text(1).parse::<XactState>() else {
            continue;
        };
        let nodes: Vec<String> = serde_json::from_str(&text(2)).unwrap_or_default();
        out.insert(
            text(0),
            JournalRow {
                state,
                nodes,
                created_at: text(3),
                purpose: text(4),
            },
        );
    }
    Ok(out)
}

fn cutoff(age: Duration) -> String {
    let secs = i64::try_from(age.as_secs()).unwrap_or(i64::MAX);
    forgebase_core::format_rfc3339(time::OffsetDateTime::now_utc() - time::Duration::seconds(secs))
}

/// Resolves in-doubt transactions on `nodes` from control's journal
/// (ADR-0029 §3.7): a part whose transaction has a journal row is
/// committed; one without a row is rolled back once older than `grace` and
/// not in flight in this process. A `committing` row whose parts are gone
/// from every listed set it names becomes `committed`; committed rows older
/// than [`JOURNAL_RETENTION`] are pruned. Control must be among `nodes` to
/// resolve its own parts. SQLite: nothing to do.
pub async fn recover(control: &Database, nodes: &[Participant], grace: Duration) -> Result<RecoveryReport, DbError> {
    let mut report = RecoveryReport::default();
    if control.kind() == BackendKind::Sqlite {
        return Ok(report);
    }
    // Prepared parts per reachable set.
    let mut found: Vec<(&Participant, Vec<PreparedXact>)> = Vec::new();
    for n in nodes {
        match n.db.prepared_transactions().await {
            Ok(list) => found.push((n, list)),
            Err(e) => report.unreachable.push((n.set_id.clone(), e.to_string())),
        }
    }
    let gids: BTreeSet<String> = found
        .iter()
        .flat_map(|(_, l)| l.iter().map(|x| x.gid.clone()))
        .collect();
    let journal = journal_rows(control, &gids).await?;
    let grace_secs = grace.as_secs_f64();
    let mut still_open: HashSet<String> = HashSet::new();
    for (n, list) in &found {
        for x in list {
            if parse_node_gid(&x.node_gid).map(|(_, k)| k) != Some(n.number) {
                // Another set's part in the same database: not ours to finish here.
                report.skipped.push(x.node_gid.clone());
                still_open.insert(x.gid.clone());
                continue;
            }
            if in_flight(&x.gid) {
                report.skipped.push(x.node_gid.clone());
                still_open.insert(x.gid.clone());
                continue;
            }
            let result = match journal.get(&x.gid) {
                Some(_) => n.db.commit_prepared(&x.node_gid).await.map(|_| true),
                None if x.age_secs >= grace_secs => n.db.rollback_prepared(&x.node_gid).await.map(|_| false),
                None => {
                    report.skipped.push(x.node_gid.clone());
                    still_open.insert(x.gid.clone());
                    continue;
                }
            };
            match result {
                Ok(true) => report.committed.push(x.node_gid.clone()),
                Ok(false) => report.rolled_back.push(x.node_gid.clone()),
                Err(e) => {
                    still_open.insert(x.gid.clone());
                    report.unreachable.push((n.set_id.clone(), e.to_string()));
                }
            }
        }
    }
    // Close `committing` rows whose every set was read and holds no part.
    let reached: HashSet<&str> = found.iter().map(|(n, _)| n.set_id.as_str()).collect();
    for (gid, row) in &journal {
        if row.state != XactState::Committing || still_open.contains(gid) || in_flight(gid) {
            continue;
        }
        if row.nodes.iter().all(|s| reached.contains(s.as_str())) {
            journal_close(control, gid).await?;
            report.closed.push(gid.clone());
        }
    }
    let pruned = control
        .begin(TxMode::Normal)
        .await?
        .execute_and_commit(&Statement::new(
            "DELETE FROM _forgebase_xact_journal WHERE state = 'committed' AND resolved_at < ?1",
            vec![cutoff(JOURNAL_RETENTION).into()],
        ))
        .await?;
    report.pruned = pruned;
    if !report.is_empty() {
        tracing::info!(committed = report.committed.len(), rolled_back = report.rolled_back.len(),
            closed = report.closed.len(), "resolved in-doubt schema transactions");
    }
    Ok(report)
}

trait ExecuteAndCommit {
    fn execute_and_commit<'a>(self, stmt: &'a Statement) -> BoxFuture<'a, Result<u64, DbError>>;
}

impl ExecuteAndCommit for Box<dyn WriteTx> {
    fn execute_and_commit<'a>(mut self, stmt: &'a Statement) -> BoxFuture<'a, Result<u64, DbError>> {
        Box::pin(async move {
            match self.execute(stmt).await {
                Ok(r) => {
                    self.commit().await?;
                    Ok(r.rows_affected)
                }
                Err(e) => {
                    let _ = self.rollback().await;
                    Err(e)
                }
            }
        })
    }
}

/// Something `forgebase doctor` reports about two-phase transactions.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InDoubt {
    /// A prepared part older than the grace period.
    Prepared {
        /// Set holding it.
        set_id: String,
        /// Its identifier.
        node_gid: String,
        /// Seconds since it was prepared.
        age_secs: f64,
        /// The journal state (`Some(committing)`: recovery commits it;
        /// `None`: recovery rolls it back).
        journal: Option<XactState>,
        /// What the transaction was for.
        purpose: Option<String>,
    },
    /// A decided transaction not finished everywhere for a while (a set
    /// was unreachable).
    Stuck {
        /// Transaction identifier.
        gid: String,
        /// Sets it names.
        nodes: Vec<String>,
        /// When it was decided.
        created_at: String,
        /// What it was for.
        purpose: String,
    },
    /// A set whose prepared transactions could not be read.
    Unreachable {
        /// Set id.
        set_id: String,
        /// Error.
        error: String,
    },
}

/// In-doubt transactions for `forgebase doctor` (read only): prepared parts
/// older than `grace` and `committing` journal rows older than
/// [`STUCK_AFTER`].
pub async fn in_doubt(control: &Database, nodes: &[Participant], grace: Duration) -> Result<Vec<InDoubt>, DbError> {
    let mut out = Vec::new();
    if control.kind() == BackendKind::Sqlite {
        return Ok(out);
    }
    let mut found = Vec::new();
    for n in nodes {
        match n.db.prepared_transactions().await {
            Ok(list) => found.push((n, list)),
            Err(e) => out.push(InDoubt::Unreachable {
                set_id: n.set_id.clone(),
                error: e.to_string(),
            }),
        }
    }
    let gids: BTreeSet<String> = found
        .iter()
        .flat_map(|(_, l)| l.iter().map(|x| x.gid.clone()))
        .collect();
    let journal = journal_rows(control, &gids).await?;
    for (n, list) in &found {
        for x in list.iter().filter(|x| x.age_secs >= grace.as_secs_f64()) {
            let row = journal.get(&x.gid);
            out.push(InDoubt::Prepared {
                set_id: n.set_id.clone(),
                node_gid: x.node_gid.clone(),
                age_secs: x.age_secs,
                journal: row.map(|r| r.state),
                purpose: row.map(|r| r.purpose.clone()),
            });
        }
    }
    let stuck = cutoff(STUCK_AFTER);
    for (gid, row) in &journal {
        if row.state == XactState::Committing && row.created_at < stuck {
            out.push(InDoubt::Stuck {
                gid: gid.clone(),
                nodes: row.nodes.clone(),
                created_at: row.created_at.clone(),
                purpose: row.purpose.clone(),
            });
        }
    }
    Ok(out)
}

/// Runs [`recover`] over the router's sets now and every `interval` until
/// dropped (ADR-0029 §3.7: at start and every minute). Errors are logged.
pub fn spawn_recovery(router: DbRouter, interval: Duration) -> RecoveryTask {
    let handle = tokio::spawn(async move {
        loop {
            let control = router.control().database().clone();
            if control.kind() == BackendKind::Postgres {
                let nodes = participants(&router);
                match recover(&control, &nodes, RECOVERY_GRACE).await {
                    Ok(r) if !r.unreachable.is_empty() => {
                        tracing::warn!(unreachable = ?r.unreachable, "two-phase recovery could not reach every set");
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "two-phase recovery failed"),
                }
            }
            tokio::time::sleep(interval).await;
        }
    });
    RecoveryTask(Arc::new(handle))
}

/// The task of [`spawn_recovery`]; aborted when the last clone is dropped.
#[derive(Debug, Clone)]
pub struct RecoveryTask(Arc<tokio::task::JoinHandle<()>>);

impl Drop for RecoveryTask {
    fn drop(&mut self) {
        if Arc::strong_count(&self.0) == 1 {
            self.0.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_round_trip_and_fit() {
        let gid = new_gid();
        assert!(gid.starts_with(GID_PREFIX) && gid.len() == 35);
        let node = node_gid(&gid, 8190);
        assert_eq!(parse_node_gid(&node), Some((gid.as_str(), 8190)));
        assert!(node.len() <= crate::pg::twophase::MAX_GID_LEN);
        crate::pg::twophase::check_gid(&node).expect("valid identifier");
        assert_eq!(parse_node_gid("fb_abc_1"), None);
        assert_eq!(parse_node_gid("xx_0123456789abcdef0123456789abcdef_1"), None);
        assert_eq!(parse_node_gid("fb_0123456789abcdef0123456789abcdef_"), None);
        assert_eq!(parse_node_gid("fb_0123456789ABCDEF0123456789abcdef_1"), None);
    }

    #[test]
    fn in_flight_registry_tracks_live_coordinators() {
        let gid = new_gid();
        {
            let _f = InFlight::new(&gid);
            assert!(in_flight(&gid));
        }
        assert!(!in_flight(&gid));
    }

    #[tokio::test]
    async fn one_participant_is_an_ordinary_transaction_on_sqlite() {
        let db = crate::testing::test_backend_of(BackendKind::Sqlite).await;
        let nodes = [Participant::new("control", 0, db.database().clone())];
        let work = SameStatements(vec![Statement::new(
            "INSERT INTO _forgebase_meta (key, value, updated_at) VALUES ('fb173', 'x', '')",
            vec![],
        )]);
        let out = run::<DbError>(db.database(), &nodes, "test", TxMode::Normal, &work)
            .await
            .expect("runs");
        assert!(out.is_none());
        assert_eq!(db.meta_get("fb173").await.expect("read").as_deref(), Some("x"));
        assert!(db.prepared_transactions().await.expect("none").is_empty());
        assert_eq!(db.max_prepared_transactions().await.expect("zero"), 0);
        let two = [nodes[0].clone(), Participant::new("s1", 1, db.database().clone())];
        let err = run::<DbError>(db.database(), &two, "test", TxMode::Normal, &work)
            .await
            .expect_err("SQLite cannot shard");
        assert_eq!(err.code(), "database_unsupported");
        let report = recover(db.database(), &nodes, Duration::ZERO).await.expect("no-op");
        assert!(report.is_empty());
    }
}

/// One line for `forgebase doctor` describing [`in_doubt`]'s findings.
pub fn describe_in_doubt(list: &[InDoubt]) -> String {
    let parts: Vec<String> = list
        .iter()
        .map(|d| match d {
            InDoubt::Prepared {
                set_id,
                node_gid,
                age_secs,
                journal,
                purpose,
            } => {
                let next = match journal {
                    Some(_) => "recovery commits it",
                    None => "recovery rolls it back",
                };
                let what = purpose.as_deref().map(|p| format!(" ({p})")).unwrap_or_default();
                format!("{node_gid}{what} on set {set_id} prepared {age_secs:.0} s ago: {next}")
            }
            InDoubt::Stuck {
                gid,
                nodes,
                created_at,
                purpose,
            } => format!(
                "{gid} ({purpose}) decided at {created_at} is not finished on every set of [{}]: a set is unreachable",
                nodes.join(", ")
            ),
            InDoubt::Unreachable { set_id, error } => format!("set {set_id} could not be read: {error}"),
        })
        .collect();
    format!(
        "{} in-doubt schema transaction(s): {}; the server resolves them at start and every minute",
        list.len(),
        parts.join("; ")
    )
}

#[cfg(test)]
mod describe_tests {
    use super::*;

    #[test]
    fn doctor_lines_say_what_recovery_does() {
        let line = describe_in_doubt(&[
            InDoubt::Prepared {
                set_id: "s1".into(),
                node_gid: "fb_x_1".into(),
                age_secs: 75.2,
                journal: Some(XactState::Committing),
                purpose: Some("create collection".into()),
            },
            InDoubt::Prepared {
                set_id: "control".into(),
                node_gid: "fb_y_0".into(),
                age_secs: 40.0,
                journal: None,
                purpose: None,
            },
            InDoubt::Stuck {
                gid: "fb_z".into(),
                nodes: vec!["control".into(), "s2".into()],
                created_at: "2026-11-11T00:00:00Z".into(),
                purpose: "native policies".into(),
            },
        ]);
        assert!(line.starts_with("3 in-doubt"), "{line}");
        assert!(line.contains("fb_x_1 (create collection) on set s1 prepared 75 s ago: recovery commits it"));
        assert!(line.contains("fb_y_0 on set control prepared 40 s ago: recovery rolls it back"));
        assert!(line.contains("[control, s2]"));
    }
}
