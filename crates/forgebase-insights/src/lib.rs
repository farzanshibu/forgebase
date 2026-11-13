//! Operations insights for administrators (ADR-0036).
//!
//! * **Statements** ([`queries`]): fingerprinted statement statistics from
//!   `forgebase_db::stmtstats`, flushed to `_forgebase_stmt_stats`; the slow
//!   statement log; regression detection comparing a recent window with a
//!   baseline window.
//! * **Usage** ([`usage`]): per-tenant usage from metering events, daily
//!   resource snapshots and counters, with cost estimates from the billing
//!   catalog and the credit rates.
//! * **Functions** ([`functions`]): per function and version profiles and the
//!   recent invocations with their host-call timelines.
//! * **Audit** ([`http`] over `forgebase_auth`): search, related events,
//!   summary and CSV/JSONL export of the audit trail.
//!
//! Everything is read through `/api/v1/insights` ([`http`]) and needs the
//! `ReadInsights` / `ReadAudit` actions (platform administrators hold them).
//! The service owns one background task that flushes the in-memory statistics
//! every `[observability] flush_interval_secs` and rolls the tenant usage up.

pub mod error;
pub mod functions;
pub mod http;
pub mod queries;
pub mod usage;
mod util;

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forgebase_auth::AuthService;
use forgebase_billing::BillingService;
use forgebase_core::config::observability_config::ObservabilityConfig;
use forgebase_credits::CreditService;
use forgebase_data::{AccessContext, Action, DataService};
use forgebase_db::stmtstats::StatementStats;
use forgebase_db::{Backend, DbRouter, Rows, Statement, TxMode};
use forgebase_functions::FunctionService;
use tokio::task::JoinHandle;

pub use error::{InsightsError, Result};
pub use http::{extension, openapi, router};
pub use util::{Range, iso, parse_date, parse_time};

/// What the services behind the insights are; each is optional except the
/// database and the access check.
#[derive(Debug, Clone)]
pub struct InsightsDeps {
    /// The control database: the insight tables, the tenancy tree, usage events.
    pub db: Arc<dyn Backend>,
    /// Every database set, to read resource usage of sharded tenants.
    pub router: Option<DbRouter>,
    /// Decides who may read insights.
    pub data: DataService,
    /// The audit trail.
    pub auth: AuthService,
    /// Function profiles; `None` when functions are switched off.
    pub functions: Option<FunctionService>,
    /// Prices for cost estimates; `None` when billing is switched off.
    pub billing: Option<BillingService>,
    /// Credit rates for cost estimates.
    pub credits: Option<CreditService>,
}

/// The insights service. Cheap to clone.
#[derive(Debug, Clone)]
pub struct InsightsService {
    pub(crate) inner: Arc<Inner>,
}

#[derive(Debug)]
pub(crate) struct Inner {
    pub(crate) deps: InsightsDeps,
    pub(crate) cfg: ObservabilityConfig,
    pub(crate) stats: Arc<StatementStats>,
    /// Start of the interval the next flush writes (Unix ms).
    pub(crate) last_flush_ms: AtomicI64,
    pub(crate) last_compact_ms: AtomicI64,
    pub(crate) last_rollup_ms: AtomicI64,
    pub(crate) flushes: AtomicU64,
    pub(crate) tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        for t in self.tasks.get_mut().unwrap_or_else(|p| p.into_inner()).drain(..) {
            t.abort();
        }
    }
}

/// What one flush wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct FlushReport {
    /// Statement fingerprints written.
    pub statements: usize,
    /// Slow statements written.
    pub slow_statements: usize,
    /// Slow statements dropped by the sampling budget.
    pub slow_dropped: u64,
    /// Function (name, version) rows written.
    pub functions: usize,
}

impl InsightsService {
    /// Builds the service on the process-wide statement statistics, and
    /// applies `cfg` to them and to the function profiler.
    pub fn new(deps: InsightsDeps, cfg: ObservabilityConfig) -> Self {
        let stats = StatementStats::global().clone();
        stats.set_enabled(cfg.query_stats);
        if let Some(f) = &deps.functions {
            f.profiler().set_enabled(cfg.function_profiling);
        }
        let now = util::now_ms();
        Self {
            inner: Arc::new(Inner {
                deps,
                cfg,
                stats,
                last_flush_ms: AtomicI64::new(now),
                last_compact_ms: AtomicI64::new(now),
                last_rollup_ms: AtomicI64::new(0),
                flushes: AtomicU64::new(0),
                tasks: Mutex::new(Vec::new()),
            }),
        }
    }

    /// The configuration in force.
    pub fn config(&self) -> &ObservabilityConfig {
        &self.inner.cfg
    }

    pub(crate) fn db(&self) -> &Arc<dyn Backend> {
        &self.inner.deps.db
    }

    pub(crate) async fn fetch(&self, sql: &str, params: Vec<forgebase_db::Value>) -> Result<Rows> {
        Ok(self.db().fetch_all(&Statement::new(sql, params)).await?)
    }

    /// Runs statements in one transaction.
    pub(crate) async fn write(&self, stmts: Vec<Statement>) -> Result<()> {
        let mut tx = self.db().begin(TxMode::Normal).await?;
        for s in &stmts {
            tx.execute(s).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Checks that `ctx` may read operations insights (`ReadInsights`).
    ///
    /// # Errors
    /// `unauthenticated` for anonymous callers, `forbidden` otherwise.
    pub fn authorize_insights(&self, ctx: &AccessContext) -> Result<()> {
        self.authorize(ctx, Action::ReadInsights)
    }

    /// Checks that `ctx` may search the audit trail (`ReadAudit`).
    ///
    /// # Errors
    /// `unauthenticated` for anonymous callers, `forbidden` otherwise.
    pub fn authorize_audit(&self, ctx: &AccessContext) -> Result<()> {
        self.authorize(ctx, Action::ReadAudit)
    }

    fn authorize(&self, ctx: &AccessContext, action: Action) -> Result<()> {
        if ctx.is_privileged() {
            return Ok(());
        }
        Ok(self.inner.deps.data.authorize_action(ctx, action)?)
    }

    /// Writes everything held in memory (statement statistics, slow
    /// statements, function profiles) as one time bucket, then prunes and
    /// compacts old rows when due. The background task calls this every
    /// flush interval, and at shutdown.
    ///
    /// # Errors
    /// A database error; the drained numbers are lost (statistics, not data).
    pub async fn flush(&self) -> Result<FlushReport> {
        let now = util::now_ms();
        let start = self.inner.last_flush_ms.swap(now, Ordering::Relaxed).min(now - 1);
        let mut report = self.flush_statements(start, now - start).await?;
        report.functions = self.flush_functions(start, now - start).await?;
        let n = self.inner.flushes.fetch_add(1, Ordering::Relaxed);
        if n % 10 == 9 {
            self.prune(now).await?;
        }
        if now - self.inner.last_compact_ms.load(Ordering::Relaxed) >= 3_600_000 {
            self.inner.last_compact_ms.store(now, Ordering::Relaxed);
            self.compact(now).await?;
        }
        Ok(report)
    }

    /// Starts the background task: a flush every `flush_interval_secs`, the
    /// tenant usage rollup of today every 15 minutes (and of yesterday once
    /// after midnight).
    pub fn start_background(&self) {
        let me = self.clone();
        let every = Duration::from_secs(self.inner.cfg.flush_interval_secs.max(1));
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                tick.tick().await;
                if let Err(e) = me.flush().await {
                    tracing::warn!(error = %e, "insights flush failed");
                }
                let now = util::now_ms();
                if now - me.inner.last_rollup_ms.load(Ordering::Relaxed) >= 15 * 60_000 {
                    me.inner.last_rollup_ms.store(now, Ordering::Relaxed);
                    if let Err(e) = me.rollup_recent(now).await {
                        tracing::warn!(error = %e, "tenant usage rollup failed");
                    }
                }
            }
        });
        self.inner.tasks.lock().unwrap_or_else(|p| p.into_inner()).push(task);
    }

    /// Stops the background task and writes what is in memory.
    pub async fn shutdown(&self) {
        for t in self.inner.tasks.lock().unwrap_or_else(|p| p.into_inner()).drain(..) {
            t.abort();
        }
        if let Err(e) = self.flush().await {
            tracing::warn!(error = %e, "insights final flush failed");
        }
    }

    /// Deletes rows older than the retention.
    pub(crate) async fn prune(&self, now: i64) -> Result<()> {
        let cutoff = now - i64::from(self.inner.cfg.retention_days) * 86_400_000;
        self.write(vec![
            Statement::new("DELETE FROM _forgebase_stmt_stats WHERE bucket_start < ?1", vec![cutoff.into()]),
            Statement::new("DELETE FROM _forgebase_slow_statements WHERE at < ?1", vec![cutoff.into()]),
            Statement::new(
                "DELETE FROM _forgebase_slow_statements WHERE id <= (
                     SELECT id FROM _forgebase_slow_statements ORDER BY id DESC LIMIT 1 OFFSET 20000)",
                vec![],
            ),
            Statement::new("DELETE FROM _forgebase_fn_stats WHERE bucket_start < ?1", vec![cutoff.into()]),
            // Daily tenant usage is kept for 400 days whatever the retention:
            // a billing year plus a month of margin.
            Statement::new(
                "DELETE FROM _forgebase_tenant_usage_daily WHERE day < ?1",
                vec![util::day_of(now - 400 * 86_400_000).into()],
            ),
        ])
        .await
    }
}
