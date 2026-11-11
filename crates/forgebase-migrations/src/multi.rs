//! System migrations over every node of a sharded cluster (ADR-0029 §3.7,
//! FB-173).
//!
//! * **Control first.** If control fails, no shard is touched.
//! * **Then every shard, at most [`MAX_PARALLEL`] at a time** (each with its
//!   own `_forgebase_migrations` and pre-migration snapshot: both belong to
//!   the node, [`MigrationNode::migrate`]).
//! * A shard below head is announced to [`MigrationHooks::starting`] before
//!   its migrations run and to [`MigrationHooks::finished`] after, so the
//!   caller can mark its set `migrating` (its tenants get `503
//!   shard_migrating`) and back. Shards already at head are not announced.
//! * One shard failing does not stop the others; the report says which.
//!
//! The paired-file format is unchanged: every node runs the same embedded
//! migrations ([`crate::postgres::POSTGRES_MIGRATIONS`]). This module only
//! orders and limits the runs; `forgebase-db` implements the nodes.

use std::future::Future;
use std::pin::Pin;

use futures_util::stream::{self, StreamExt};

use crate::{AppliedMigration, MigrationError, MigrationStatus};

/// Why a node could not be read, marked or migrated: a stable code and a
/// message (the node may fail for reasons other than a migration, such as
/// a connection error).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeError {
    /// Stable error code.
    pub code: &'static str,
    /// Message.
    pub message: String,
}

impl NodeError {
    /// An error with `code`.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for NodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl From<MigrationError> for NodeError {
    fn from(e: MigrationError) -> Self {
        use forgebase_core::ErrorCode;
        Self::new(e.code(), e.to_string())
    }
}

/// A boxed `Send` future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Shards migrated at the same time (ADR-0029 §3.7).
pub const MAX_PARALLEL: usize = 4;

/// One node to migrate: a set's primary.
pub trait MigrationNode: Send + Sync {
    /// Set id, for reports and hooks.
    fn id(&self) -> &str;

    /// Applied and pending migrations, read without changing anything.
    fn status(&self) -> BoxFuture<'_, Result<MigrationStatus, NodeError>>;

    /// Applies the pending migrations (and takes the node's pre-migration
    /// snapshot first, where it has one).
    fn migrate(&self) -> BoxFuture<'_, Result<Vec<AppliedMigration>, NodeError>>;
}

/// Told around each shard that has migrations to apply.
pub trait MigrationHooks: Send + Sync {
    /// Before the shard's migrations run (mark the set `migrating`). An
    /// error skips the shard.
    fn starting<'a>(&'a self, node: &'a str) -> BoxFuture<'a, Result<(), NodeError>>;

    /// After they ran (`ok` is whether all applied): mark the set back.
    fn finished<'a>(&'a self, node: &'a str, ok: bool) -> BoxFuture<'a, ()>;
}

/// Hooks that do nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHooks;

impl MigrationHooks for NoHooks {
    fn starting<'a>(&'a self, _node: &'a str) -> BoxFuture<'a, Result<(), NodeError>> {
        Box::pin(async { Ok(()) })
    }

    fn finished<'a>(&'a self, _node: &'a str, _ok: bool) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }
}

/// What happened on one node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeOutcome {
    /// Already at head; nothing ran.
    UpToDate,
    /// These migrations were applied.
    Applied(Vec<AppliedMigration>),
    /// It failed: error code and message (nothing of the failing migration
    /// was applied; earlier ones of this run stay).
    Failed {
        /// Stable error code.
        code: &'static str,
        /// Message.
        message: String,
    },
    /// Not attempted because control failed.
    Skipped,
}

/// One node's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeReport {
    /// Set id.
    pub id: String,
    /// Outcome.
    pub outcome: NodeOutcome,
}

impl NodeReport {
    /// Whether the node is at head now.
    pub fn is_ok(&self) -> bool {
        matches!(self.outcome, NodeOutcome::UpToDate | NodeOutcome::Applied(_))
    }
}

/// Result of [`migrate_all`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiReport {
    /// Control.
    pub control: NodeReport,
    /// Every shard, in the order given.
    pub shards: Vec<NodeReport>,
}

impl MultiReport {
    /// Whether every node is at head.
    pub fn is_ok(&self) -> bool {
        self.control.is_ok() && self.shards.iter().all(NodeReport::is_ok)
    }

    /// Nodes that failed or were skipped.
    pub fn failed(&self) -> Vec<&NodeReport> {
        std::iter::once(&self.control)
            .chain(self.shards.iter())
            .filter(|r| !r.is_ok())
            .collect()
    }
}

/// Options of [`migrate_all`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MultiOptions {
    /// Shards migrated at the same time (at least 1).
    pub max_parallel: usize,
}

impl Default for MultiOptions {
    fn default() -> Self {
        Self {
            max_parallel: MAX_PARALLEL,
        }
    }
}

fn failed(e: &NodeError) -> NodeOutcome {
    NodeOutcome::Failed {
        code: e.code,
        message: e.message.clone(),
    }
}

/// Migrates `control`, then every shard (at most `options.max_parallel` at
/// a time). See the module documentation.
pub async fn migrate_all(
    control: &dyn MigrationNode,
    shards: &[&dyn MigrationNode],
    options: MultiOptions,
    hooks: &dyn MigrationHooks,
) -> MultiReport {
    let control_report = NodeReport {
        id: control.id().to_owned(),
        outcome: match control.migrate().await {
            Ok(applied) if applied.is_empty() => NodeOutcome::UpToDate,
            Ok(applied) => NodeOutcome::Applied(applied),
            Err(e) => failed(&e),
        },
    };
    if !control_report.is_ok() {
        tracing::error!(node = %control_report.id, outcome = ?control_report.outcome,
            "control failed to migrate; shards left as they are");
        return MultiReport {
            control: control_report,
            shards: shards
                .iter()
                .map(|s| NodeReport {
                    id: s.id().to_owned(),
                    outcome: NodeOutcome::Skipped,
                })
                .collect(),
        };
    }
    let mut results: Vec<(usize, NodeReport)> = stream::iter(shards.iter().copied().enumerate())
        .map(|(i, node)| async move { (i, migrate_shard(node, hooks).await) })
        .buffer_unordered(options.max_parallel.max(1))
        .collect()
        .await;
    results.sort_by_key(|(i, _)| *i);
    MultiReport {
        control: control_report,
        shards: results.into_iter().map(|(_, r)| r).collect(),
    }
}

async fn migrate_shard(node: &dyn MigrationNode, hooks: &dyn MigrationHooks) -> NodeReport {
    let id = node.id().to_owned();
    let outcome = match node.status().await {
        Err(e) => failed(&e),
        Ok(status) if !status.problems.is_empty() => match status.problems.into_iter().next() {
            Some(p) => failed(&NodeError::from(MigrationError::from(p))),
            None => NodeOutcome::UpToDate,
        },
        Ok(status) if status.pending.is_empty() => NodeOutcome::UpToDate,
        Ok(_) => match hooks.starting(&id).await {
            Err(e) => failed(&e),
            Ok(()) => {
                let r = node.migrate().await;
                hooks.finished(&id, r.is_ok()).await;
                match r {
                    Ok(applied) if applied.is_empty() => NodeOutcome::UpToDate,
                    Ok(applied) => {
                        tracing::info!(node = %id, applied = applied.len(), "shard migrated");
                        NodeOutcome::Applied(applied)
                    }
                    Err(e) => {
                        tracing::error!(node = %id, error = %e, "shard failed to migrate");
                        failed(&e)
                    }
                }
            }
        },
    };
    NodeReport { id, outcome }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::{MigrationProblem, PendingMigration};

    #[derive(Debug, Default)]
    struct Log {
        events: Mutex<Vec<String>>,
        running: AtomicUsize,
        peak: AtomicUsize,
    }

    impl Log {
        fn push(&self, e: String) {
            self.events.lock().unwrap_or_else(|p| p.into_inner()).push(e);
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap_or_else(|p| p.into_inner()).clone()
        }
    }

    struct Fake<'a> {
        id: String,
        pending: bool,
        fail: bool,
        log: &'a Log,
    }

    fn applied(version: i64) -> AppliedMigration {
        AppliedMigration {
            version,
            name: "x".into(),
            checksum: String::new(),
            applied_at: String::new(),
            execution_ms: 0,
        }
    }

    impl MigrationNode for Fake<'_> {
        fn id(&self) -> &str {
            &self.id
        }

        fn status(&self) -> BoxFuture<'_, Result<MigrationStatus, NodeError>> {
            Box::pin(async move {
                Ok(MigrationStatus {
                    current_version: 1,
                    latest_version: if self.pending { 2 } else { 1 },
                    applied: vec![applied(1)],
                    pending: if self.pending {
                        vec![PendingMigration {
                            version: 2,
                            name: "next".into(),
                        }]
                    } else {
                        Vec::new()
                    },
                    problems: Vec::<MigrationProblem>::new(),
                })
            })
        }

        fn migrate(&self) -> BoxFuture<'_, Result<Vec<AppliedMigration>, NodeError>> {
            Box::pin(async move {
                let now = self.log.running.fetch_add(1, Ordering::SeqCst) + 1;
                self.log.peak.fetch_max(now, Ordering::SeqCst);
                self.log.push(format!("start {}", self.id));
                tokio::time::sleep(Duration::from_millis(30)).await;
                self.log.push(format!("end {}", self.id));
                self.log.running.fetch_sub(1, Ordering::SeqCst);
                if self.fail {
                    return Err(NodeError::new("migration_failed", "boom"));
                }
                Ok(if self.pending { vec![applied(2)] } else { Vec::new() })
            })
        }
    }

    struct Hooks<'a>(&'a Log);

    impl MigrationHooks for Hooks<'_> {
        fn starting<'a>(&'a self, node: &'a str) -> BoxFuture<'a, Result<(), NodeError>> {
            self.0.push(format!("migrating {node}"));
            Box::pin(async { Ok(()) })
        }

        fn finished<'a>(&'a self, node: &'a str, ok: bool) -> BoxFuture<'a, ()> {
            self.0.push(format!("finished {node} {ok}"));
            Box::pin(async {})
        }
    }

    fn fakes(log: &Log, n: usize) -> Vec<Fake<'_>> {
        (1..=n)
            .map(|k| Fake {
                id: format!("s{k}"),
                pending: true,
                fail: false,
                log,
            })
            .collect()
    }

    #[tokio::test]
    async fn control_first_then_at_most_four_shards_at_a_time() {
        let log = Log::default();
        let control = Fake {
            id: "control".into(),
            pending: true,
            fail: false,
            log: &log,
        };
        let shards = fakes(&log, 10);
        let refs: Vec<&dyn MigrationNode> = shards.iter().map(|s| s as &dyn MigrationNode).collect();
        let report = migrate_all(&control, &refs, MultiOptions::default(), &Hooks(&log)).await;
        assert!(report.is_ok(), "{report:?}");
        let events = log.events();
        assert_eq!(&events[..2], ["start control", "end control"]);
        assert_eq!(log.peak.load(Ordering::SeqCst), MAX_PARALLEL);
        assert_eq!(
            report.shards.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            (1..=10).map(|k| format!("s{k}")).collect::<Vec<_>>()
        );
        // Every shard was marked before it started and after it ended.
        for k in 1..=10 {
            let pos = |e: String| events.iter().position(|x| *x == e).expect("event");
            assert!(pos(format!("migrating s{k}")) < pos(format!("start s{k}")));
            assert!(pos(format!("end s{k}")) < pos(format!("finished s{k} true")));
        }
        // A cap of 1 runs them one by one.
        let log = Log::default();
        let shards = fakes(&log, 3);
        let refs: Vec<&dyn MigrationNode> = shards.iter().map(|s| s as &dyn MigrationNode).collect();
        let control = Fake {
            id: "control".into(),
            pending: false,
            fail: false,
            log: &log,
        };
        let report = migrate_all(&control, &refs, MultiOptions { max_parallel: 1 }, &NoHooks).await;
        assert!(report.is_ok());
        assert_eq!(report.control.outcome, NodeOutcome::UpToDate);
        assert_eq!(log.peak.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_control_failure_leaves_every_shard_alone() {
        let log = Log::default();
        let control = Fake {
            id: "control".into(),
            pending: true,
            fail: true,
            log: &log,
        };
        let shards = fakes(&log, 3);
        let refs: Vec<&dyn MigrationNode> = shards.iter().map(|s| s as &dyn MigrationNode).collect();
        let report = migrate_all(&control, &refs, MultiOptions::default(), &Hooks(&log)).await;
        assert!(!report.is_ok());
        assert!(matches!(report.control.outcome, NodeOutcome::Failed { .. }));
        assert!(report.shards.iter().all(|r| r.outcome == NodeOutcome::Skipped));
        assert_eq!(log.events(), ["start control", "end control"]);
        assert_eq!(report.failed().len(), 4);
    }

    #[tokio::test]
    async fn a_shard_failure_does_not_stop_the_others_and_current_shards_are_not_marked() {
        let log = Log::default();
        let control = Fake {
            id: "control".into(),
            pending: false,
            fail: false,
            log: &log,
        };
        let mut shards = fakes(&log, 3);
        shards[0].fail = true;
        shards[2].pending = false;
        let refs: Vec<&dyn MigrationNode> = shards.iter().map(|s| s as &dyn MigrationNode).collect();
        let report = migrate_all(&control, &refs, MultiOptions::default(), &Hooks(&log)).await;
        assert!(matches!(report.shards[0].outcome, NodeOutcome::Failed { .. }));
        assert!(matches!(report.shards[1].outcome, NodeOutcome::Applied(ref a) if a.len() == 1));
        assert_eq!(report.shards[2].outcome, NodeOutcome::UpToDate);
        assert_eq!(report.failed().iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["s1"]);
        let events = log.events();
        assert!(events.contains(&"finished s1 false".to_owned()));
        assert!(!events.iter().any(|e| e.contains("s3")), "{events:?}");
    }
}
