//! The slow-query log and statement latency metric, shared by both backends
//! (ADR-0028 §7.3).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::backend::{SlowQuery, Statement};
use crate::stmtstats::StatementStats;

/// Default slow-query threshold.
pub const DEFAULT_SLOW_QUERY_THRESHOLD: Duration = Duration::from_millis(200);
/// Entries kept in the slow-query ring buffer.
pub const SLOW_QUERY_LOG_CAPACITY: usize = 256;

/// Statement latency histogram; the name matches `forgebase_observability::names`.
const QUERY_DURATION_METRIC: &str = "forgebase_db_query_duration_seconds";

/// Bounded in-memory slow-query log shared by all connections of a database.
#[derive(Debug)]
pub(crate) struct SlowLog {
    threshold_us: AtomicU64,
    entries: Mutex<VecDeque<SlowQuery>>,
    /// `sqlite` or `postgres`: the label of the statement statistics.
    backend: &'static str,
    /// Fingerprints, latency sketches and the redacted slow statements
    /// (ADR-0036); the process-wide collector.
    stats: Arc<StatementStats>,
}

impl SlowLog {
    pub(crate) fn new(backend: &'static str) -> Self {
        Self {
            threshold_us: AtomicU64::new(DEFAULT_SLOW_QUERY_THRESHOLD.as_micros() as u64),
            entries: Mutex::new(VecDeque::with_capacity(SLOW_QUERY_LOG_CAPACITY)),
            backend,
            stats: StatementStats::global().clone(),
        }
    }

    /// Counts a statement that failed (ADR-0036); it has no row count.
    pub(crate) fn record_error(&self, stmt: &Statement, elapsed: Duration, write: bool) {
        let slow = elapsed.as_micros() as u64 >= self.threshold_us.load(Ordering::Relaxed);
        self.stats.record(
            self.backend,
            &stmt.sql,
            stmt.label.as_ref(),
            elapsed,
            0,
            true,
            write,
            slow,
        );
    }

    /// Observes the statement latency (histogram `forgebase_db_query_duration_seconds`, label
    /// `mode`), then logs it when it passed the slow threshold.
    pub(crate) fn record(&self, stmt: &Statement, elapsed: Duration, rows: u64, write: bool) {
        metrics::histogram!(QUERY_DURATION_METRIC, "mode" => if write { "write" } else { "read" })
            .record(elapsed.as_secs_f64());
        let slow = elapsed.as_micros() as u64 >= self.threshold_us.load(Ordering::Relaxed);
        self.stats.record(
            self.backend,
            &stmt.sql,
            stmt.label.as_ref(),
            elapsed,
            rows,
            false,
            write,
            slow,
        );
        if slow {
            let entry = SlowQuery {
                sql: stmt.sql.clone(),
                param_count: stmt.params.len(),
                duration_ms: elapsed.as_secs_f64() * 1000.0,
                at: forgebase_core::now_rfc3339(),
                rows,
                label: stmt.label.clone(),
            };
            tracing::warn!(duration_ms = entry.duration_ms, sql = %entry.sql, "slow query");
            let mut log = self.entries.lock().unwrap_or_else(|p| p.into_inner());
            if log.len() == SLOW_QUERY_LOG_CAPACITY {
                log.pop_front();
            }
            log.push_back(entry);
        }
    }

    /// Entries, oldest first.
    pub(crate) fn entries(&self) -> Vec<SlowQuery> {
        let log = self.entries.lock().unwrap_or_else(|p| p.into_inner());
        log.iter().cloned().collect()
    }

    /// Removes every entry.
    pub(crate) fn clear(&self) {
        self.entries.lock().unwrap_or_else(|p| p.into_inner()).clear();
    }

    /// Current threshold.
    pub(crate) fn threshold(&self) -> Duration {
        Duration::from_micros(self.threshold_us.load(Ordering::Relaxed))
    }

    /// Sets the threshold.
    pub(crate) fn set_threshold(&self, threshold: Duration) {
        self.threshold_us.store(threshold.as_micros() as u64, Ordering::Relaxed);
    }
}
