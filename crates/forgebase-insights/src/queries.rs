//! Statement statistics, the slow statement log and regression detection
//! (ADR-0036 §1).

use std::collections::HashMap;
use std::sync::atomic::Ordering;

use forgebase_core::sketch::Sketch;
use forgebase_db::Statement;
use forgebase_db::stmtstats::{Drained, OVERFLOW_TEXT, StatementAggregate};
use serde::Serialize;
use utoipa::ToSchema;

use crate::error::{InsightsError, Result};
use crate::util::{Range, contains_pattern, iso, ms, now_ms, r3, text};
use crate::{FlushReport, InsightsService, util::int};

/// Rows read for one listing: a safety bound on the memory of one request.
const MAX_ROWS: i64 = 400_000;
/// The fingerprint of the row that sums the statements below the top N.
const OTHER_FINGERPRINT: &str = "0000000000000000";

// ---------------------------------------------------------------------------
// Accumulation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(crate) struct Acc {
    pub(crate) fingerprint: String,
    pub(crate) backend: String,
    pub(crate) area: String,
    pub(crate) statement: String,
    pub(crate) calls: u64,
    pub(crate) errors: u64,
    pub(crate) writes: u64,
    pub(crate) slow: u64,
    pub(crate) total_us: u64,
    pub(crate) max_us: u64,
    pub(crate) min_us: u64,
    pub(crate) rows: u64,
    pub(crate) sketch: Sketch,
}

impl Acc {
    fn merge(&mut self, o: &Acc) {
        if self.calls == 0 {
            self.min_us = o.min_us;
        } else if o.calls > 0 {
            self.min_us = self.min_us.min(o.min_us);
        }
        self.calls += o.calls;
        self.errors += o.errors;
        self.writes += o.writes;
        self.slow += o.slow;
        self.total_us += o.total_us;
        self.max_us = self.max_us.max(o.max_us);
        self.rows += o.rows;
        self.sketch.merge(&o.sketch);
    }

    pub(crate) fn from_aggregate(a: &StatementAggregate) -> Self {
        Self {
            fingerprint: a.fingerprint.clone(),
            backend: a.backend.to_owned(),
            area: a.area.to_owned(),
            statement: a.text.clone(),
            calls: a.calls,
            errors: a.errors,
            writes: a.writes,
            slow: a.slow,
            total_us: a.total_us,
            max_us: a.max_us,
            min_us: a.min_us,
            rows: a.rows,
            sketch: a.sketch.clone(),
        }
    }

    fn quantile(&self, q: f64) -> u64 {
        self.sketch.quantile_within(q, self.min_us, self.max_us).unwrap_or(0)
    }

    pub(crate) fn stat(&self) -> StatementStat {
        StatementStat {
            fingerprint: self.fingerprint.clone(),
            statement: self.statement.clone(),
            area: self.area.clone(),
            backend: self.backend.clone(),
            calls: self.calls,
            errors: self.errors,
            error_rate: r3(if self.calls == 0 { 0.0 } else { self.errors as f64 / self.calls as f64 }),
            writes: self.writes,
            slow: self.slow,
            rows: self.rows,
            total_ms: ms(self.total_us),
            mean_ms: ms(self.total_us.checked_div(self.calls).unwrap_or(0)),
            min_ms: ms(self.min_us),
            max_ms: ms(self.max_us),
            p50_ms: ms(self.quantile(0.5)),
            p95_ms: ms(self.quantile(0.95)),
            p99_ms: ms(self.quantile(0.99)),
        }
    }
}

fn acc_from_row(r: &[forgebase_db::Value]) -> Acc {
    // fingerprint, backend, area, statement, calls, errors, writes, slow, total_us, max_us, min_us, rows, sketch
    Acc {
        fingerprint: text(r, 0),
        backend: text(r, 1),
        area: text(r, 2),
        statement: text(r, 3),
        calls: int(r, 4) as u64,
        errors: int(r, 5) as u64,
        writes: int(r, 6) as u64,
        slow: int(r, 7) as u64,
        total_us: int(r, 8) as u64,
        max_us: int(r, 9) as u64,
        min_us: int(r, 10) as u64,
        rows: int(r, 11) as u64,
        sketch: Sketch::from_json(&text(r, 12)),
    }
}

const SELECT_STATS: &str =
    "SELECT fingerprint, backend, area, statement, calls, errors, writes, slow, total_us, max_us, min_us, rows, sketch";

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// Statistics of one statement fingerprint over a time range.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StatementStat {
    /// 16 hex digits identifying the statement shape.
    pub fingerprint: String,
    /// The statement with every value replaced by `?`.
    pub statement: String,
    /// The subsystem the statement belongs to.
    pub area: String,
    /// `sqlite` or `postgres`.
    pub backend: String,
    /// Times it ran.
    pub calls: u64,
    /// Times it failed.
    pub errors: u64,
    /// `errors / calls`.
    pub error_rate: f64,
    /// Runs inside a write transaction.
    pub writes: u64,
    /// Runs at or above the slow threshold.
    pub slow: u64,
    /// Rows returned or changed, total.
    pub rows: u64,
    /// Total wall time in milliseconds.
    pub total_ms: f64,
    /// Mean time in milliseconds.
    pub mean_ms: f64,
    /// Shortest run in milliseconds.
    pub min_ms: f64,
    /// Longest run in milliseconds.
    pub max_ms: f64,
    /// Median in milliseconds (from the latency sketch: within about 11 %).
    pub p50_ms: f64,
    /// 95th percentile in milliseconds.
    pub p95_ms: f64,
    /// 99th percentile in milliseconds.
    pub p99_ms: f64,
}

/// A page of [`StatementStat`].
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StatementList {
    /// The statements.
    pub items: Vec<StatementStat>,
    /// How many statement shapes matched.
    pub total: usize,
    /// Start of the range (RFC 3339).
    pub from: String,
    /// End of the range, exclusive (RFC 3339).
    pub to: String,
    /// Page size used.
    pub limit: usize,
    /// Rows skipped.
    pub offset: usize,
}

/// One time bucket of a statement.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StatementPoint {
    /// Start of the bucket (RFC 3339).
    pub at: String,
    /// Length of the bucket in seconds.
    pub seconds: i64,
    /// Runs.
    pub calls: u64,
    /// Failures.
    pub errors: u64,
    /// Mean in milliseconds.
    pub mean_ms: f64,
    /// 95th percentile in milliseconds.
    pub p95_ms: f64,
    /// Longest in milliseconds.
    pub max_ms: f64,
}

/// One statement shape with its history.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StatementDetail {
    /// The whole range.
    #[serde(flatten)]
    pub stat: StatementStat,
    /// Buckets, oldest first (the not yet flushed part of the last bucket is
    /// the final point).
    pub series: Vec<StatementPoint>,
}

/// Latency numbers of one window of a regression.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WindowStat {
    /// Runs.
    pub calls: u64,
    /// Failures.
    pub errors: u64,
    /// `errors / calls`.
    pub error_rate: f64,
    /// Mean in milliseconds.
    pub mean_ms: f64,
    /// Median in milliseconds.
    pub p50_ms: f64,
    /// 95th percentile in milliseconds.
    pub p95_ms: f64,
    /// 99th percentile in milliseconds.
    pub p99_ms: f64,
}

impl WindowStat {
    fn of(a: &Acc) -> Self {
        let s = a.stat();
        Self {
            calls: s.calls,
            errors: s.errors,
            error_rate: s.error_rate,
            mean_ms: s.mean_ms,
            p50_ms: s.p50_ms,
            p95_ms: s.p95_ms,
            p99_ms: s.p99_ms,
        }
    }
}

/// A statement that got slower, or started failing.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Regression {
    /// `latency` (p95 grew) or `errors` (the error rate grew).
    pub kind: &'static str,
    /// Fingerprint.
    pub fingerprint: String,
    /// The statement with every value replaced by `?`.
    pub statement: String,
    /// Subsystem.
    pub area: String,
    /// `sqlite` or `postgres`.
    pub backend: String,
    /// The recent window.
    pub recent: WindowStat,
    /// The baseline window.
    pub baseline: WindowStat,
    /// `recent p95 / baseline p95` (latency) or the error rate difference (errors).
    pub ratio: f64,
    /// `recent p95 - baseline p95` in milliseconds.
    pub delta_ms: f64,
    /// Sort key: how much extra time the recent runs cost, in milliseconds.
    pub extra_ms: f64,
}

/// What a regression check compared.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RegressionReport {
    /// Start of the recent window (RFC 3339).
    pub recent_from: String,
    /// End of the recent window.
    pub recent_to: String,
    /// Start of the baseline window.
    pub baseline_from: String,
    /// End of the baseline window (the start of the recent one).
    pub baseline_to: String,
    /// Smallest `recent p95 / baseline p95` reported.
    pub min_ratio: f64,
    /// Fewest runs in each window.
    pub min_calls: u64,
    /// Smallest p95 growth reported, in milliseconds.
    pub min_delta_ms: f64,
    /// The regressions, most costly first.
    pub items: Vec<Regression>,
}

/// One slow statement.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SlowStatementEntry {
    /// Row id (the paging cursor).
    pub id: i64,
    /// When it finished (RFC 3339).
    pub at: String,
    /// Fingerprint.
    pub fingerprint: String,
    /// The statement with every value replaced by `?`.
    pub statement: String,
    /// Subsystem.
    pub area: String,
    /// `sqlite` or `postgres`.
    pub backend: String,
    /// Wall time in milliseconds.
    pub duration_ms: f64,
    /// Rows returned or changed.
    pub rows: i64,
    /// It failed.
    pub error: bool,
    /// It ran inside a write transaction.
    pub write: bool,
}

/// A page of slow statements, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SlowStatementPage {
    /// The statements.
    pub items: Vec<SlowStatementEntry>,
    /// Pass as `cursor` for the next page; absent after the last one.
    pub next_cursor: Option<i64>,
}

/// State of the statement statistics.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StatementStatus {
    /// Statistics are collected.
    pub enabled: bool,
    /// Slow statement threshold in milliseconds.
    pub slow_query_ms: u64,
    /// Seconds between flushes.
    pub flush_interval_secs: u64,
    /// Fingerprints per flush.
    pub top_n: u32,
    /// Days kept.
    pub retention_days: u32,
    /// Fingerprints held in memory now.
    pub fingerprints_in_memory: u64,
    /// Start of the interval not yet flushed (RFC 3339).
    pub unflushed_since: String,
    /// Fingerprints stored.
    pub stored_rows: i64,
    /// Slow statements stored.
    pub stored_slow_statements: i64,
}

/// Filters of [`InsightsService::statement_stats`].
#[derive(Debug, Clone, Default)]
pub struct StatementQuery {
    /// Only this subsystem.
    pub area: Option<String>,
    /// Only this backend.
    pub backend: Option<String>,
    /// Only statements containing this text (case-insensitive).
    pub text: Option<String>,
    /// `total_time` (default), `calls`, `mean`, `max`, `p50`, `p95`, `p99`, `errors`, `rows`, `slow`.
    pub sort: Option<String>,
    /// Page size (1 to 200).
    pub limit: usize,
    /// Rows to skip.
    pub offset: usize,
}

/// Filters of [`InsightsService::slow_statements`].
#[derive(Debug, Clone, Default)]
pub struct SlowQuery {
    /// Only this fingerprint.
    pub fingerprint: Option<String>,
    /// Only this subsystem.
    pub area: Option<String>,
    /// At least this long, in milliseconds.
    pub min_ms: Option<f64>,
    /// Only statements containing this text.
    pub text: Option<String>,
    /// Rows with an id below this.
    pub cursor: Option<i64>,
    /// Page size (1 to 500).
    pub limit: u32,
}

/// Parameters of [`InsightsService::regressions`].
#[derive(Debug, Clone)]
pub struct RegressionQuery {
    /// End of the recent window (Unix ms); default now.
    pub until_ms: Option<i64>,
    /// Length of the recent window in seconds.
    pub window_secs: i64,
    /// Length of the baseline window before it, in seconds.
    pub baseline_secs: i64,
    /// Runs needed in each window.
    pub min_calls: u64,
    /// Smallest p95 ratio.
    pub min_ratio: f64,
    /// Smallest p95 growth in milliseconds.
    pub min_delta_ms: f64,
    /// Only this subsystem.
    pub area: Option<String>,
}

impl Default for RegressionQuery {
    fn default() -> Self {
        Self {
            until_ms: None,
            window_secs: 3600,
            baseline_secs: 86_400,
            min_calls: 20,
            min_ratio: 2.0,
            min_delta_ms: 1.0,
            area: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Flush
// ---------------------------------------------------------------------------

impl InsightsService {
    pub(crate) async fn flush_statements(&self, start_ms: i64, len_ms: i64) -> Result<FlushReport> {
        let drained = self.inner.stats.drain();
        self.ingest_statements(drained, start_ms, len_ms).await
    }

    /// Writes drained statistics as one bucket: the busiest
    /// `query_stats_top_n` fingerprints and one summary row for the rest, the
    /// slow statements, and the Prometheus gauges. The flush calls this; tests
    /// call it to place numbers at a chosen time.
    ///
    /// # Errors
    /// A database error.
    pub async fn ingest_statements(&self, d: Drained, start_ms: i64, len_ms: i64) -> Result<FlushReport> {
        let top_n = self.inner.cfg.query_stats_top_n as usize;
        let mut stmts: Vec<Statement> = Vec::new();
        publish_gauges(&d.aggregates);
        let mut other: Option<Acc> = None;
        let mut written = 0usize;
        for (i, a) in d.aggregates.iter().enumerate() {
            if i >= top_n && a.text != OVERFLOW_TEXT {
                let o = other.get_or_insert_with(|| Acc {
                    fingerprint: OTHER_FINGERPRINT.into(),
                    backend: a.backend.into(),
                    area: "other".into(),
                    statement: String::new(),
                    ..Acc::default()
                });
                o.merge(&Acc::from_aggregate(a));
                continue;
            }
            written += 1;
            stmts.push(insert_stat(start_ms, len_ms, &Acc::from_aggregate(a)));
        }
        if let Some(mut o) = other {
            o.statement = "<statement shapes beyond the busiest ones>".into();
            stmts.push(insert_stat(start_ms, len_ms, &o));
        }
        for s in &d.slow {
            stmts.push(Statement::new(
                "INSERT INTO _forgebase_slow_statements (at, fingerprint, backend, area, statement, duration_us, rows, error, write)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                vec![
                    s.at_ms.into(),
                    s.fingerprint.clone().into(),
                    s.backend.into(),
                    s.area.into(),
                    s.text.clone().into(),
                    i64::try_from(s.duration_us).unwrap_or(i64::MAX).into(),
                    i64::try_from(s.rows).unwrap_or(i64::MAX).into(),
                    i64::from(s.error).into(),
                    i64::from(s.write).into(),
                ],
            ));
        }
        if !stmts.is_empty() {
            self.write(stmts).await?;
        }
        Ok(FlushReport {
            statements: written,
            slow_statements: d.slow.len(),
            slow_dropped: d.slow_dropped,
            functions: 0,
        })
    }

    /// Merges 60-second buckets older than two hours into hourly ones, so a
    /// range query over days reads a bounded number of rows.
    pub(crate) async fn compact(&self, now: i64) -> Result<()> {
        const HOUR: i64 = 3_600_000;
        let before = (now - 2 * HOUR).div_euclid(HOUR) * HOUR;
        let rows = self
            .fetch(
                &format!(
                    "{SELECT_STATS}, bucket_start FROM _forgebase_stmt_stats
                     WHERE bucket_start < ?1 AND bucket_ms < ?2 ORDER BY bucket_start LIMIT {MAX_ROWS}"
                ),
                vec![before.into(), HOUR.into()],
            )
            .await?;
        if rows.rows.is_empty() {
            return Ok(());
        }
        let mut merged: HashMap<(i64, String, String), Acc> = HashMap::new();
        for r in &rows.rows {
            let hour = int(r, 13).div_euclid(HOUR) * HOUR;
            let a = acc_from_row(r);
            let m = merged.entry((hour, a.fingerprint.clone(), a.backend.clone())).or_default();
            m.fingerprint.clone_from(&a.fingerprint);
            m.backend.clone_from(&a.backend);
            m.area.clone_from(&a.area);
            m.statement.clone_from(&a.statement);
            m.merge(&a);
        }
        let mut stmts = vec![Statement::new(
            "DELETE FROM _forgebase_stmt_stats WHERE bucket_start < ?1 AND bucket_ms < ?2",
            vec![before.into(), HOUR.into()],
        )];
        // Rows beyond MAX_ROWS (a huge backlog) stay for the next round: the
        // delete above would drop them, so bound it by the last row read.
        if rows.rows.len() as i64 >= MAX_ROWS {
            let last = int(&rows.rows[rows.rows.len() - 1], 13);
            stmts[0] = Statement::new(
                "DELETE FROM _forgebase_stmt_stats WHERE bucket_start <= ?1 AND bucket_ms < ?2",
                vec![last.into(), HOUR.into()],
            );
        }
        for ((hour, _, _), a) in &merged {
            stmts.push(insert_stat(*hour, HOUR, a));
        }
        self.write(stmts).await
    }

    // -----------------------------------------------------------------------
    // Reads
    // -----------------------------------------------------------------------

    /// The state of the statistics.
    ///
    /// # Errors
    /// A database error.
    pub async fn statement_status(&self) -> Result<StatementStatus> {
        let c = |sql: &'static str| async move { Ok::<_, InsightsError>(int(&self.fetch(sql, vec![]).await?.rows[0], 0)) };
        Ok(StatementStatus {
            enabled: self.inner.stats.enabled(),
            slow_query_ms: self.inner.cfg.slow_query_ms,
            flush_interval_secs: self.inner.cfg.flush_interval_secs,
            top_n: self.inner.cfg.query_stats_top_n,
            retention_days: self.inner.cfg.retention_days,
            fingerprints_in_memory: self.inner.stats.fingerprints(),
            unflushed_since: iso(self.inner.last_flush_ms.load(Ordering::Relaxed)),
            stored_rows: c("SELECT COUNT(*) FROM _forgebase_stmt_stats").await?,
            stored_slow_statements: c("SELECT COUNT(*) FROM _forgebase_slow_statements").await?,
        })
    }

    /// Stored buckets starting in `[from, to)` plus, when the range reaches
    /// now, what is still in memory.
    async fn collect(&self, from: i64, to: i64, area: Option<&str>, backend: Option<&str>) -> Result<Vec<(i64, i64, Acc)>> {
        let rows = self
            .fetch(
                &format!(
                    "{SELECT_STATS}, bucket_start, bucket_ms FROM _forgebase_stmt_stats
                     WHERE bucket_start >= ?1 AND bucket_start < ?2
                       AND (CAST(?3 AS TEXT) IS NULL OR area = ?3) AND (CAST(?4 AS TEXT) IS NULL OR backend = ?4)
                     ORDER BY bucket_start LIMIT {MAX_ROWS}"
                ),
                vec![from.into(), to.into(), area.map(str::to_owned).into(), backend.map(str::to_owned).into()],
            )
            .await?;
        let mut out: Vec<(i64, i64, Acc)> = rows.rows.iter().map(|r| (int(r, 13), int(r, 14), acc_from_row(r))).collect();
        let live_start = self.inner.last_flush_ms.load(Ordering::Relaxed);
        if live_start < to && now_ms() >= from {
            for a in self.inner.stats.peek() {
                if area.is_some_and(|x| x != a.area) || backend.is_some_and(|x| x != a.backend) {
                    continue;
                }
                out.push((live_start, (now_ms() - live_start).max(1), Acc::from_aggregate(&a)));
            }
        }
        Ok(out)
    }

    /// Statistics per statement shape over a range, busiest first.
    ///
    /// # Errors
    /// A database error, or `invalid_parameter` for an unknown sort.
    pub async fn statement_stats(&self, range: Range, q: &StatementQuery) -> Result<StatementList> {
        let sort = q.sort.as_deref().unwrap_or("total_time");
        if !["total_time", "calls", "mean", "max", "p50", "p95", "p99", "errors", "rows", "slow"].contains(&sort) {
            return Err(InsightsError::invalid(
                "invalid_parameter",
                "sort must be total_time, calls, mean, max, p50, p95, p99, errors, rows or slow",
            ));
        }
        let mut by: HashMap<(String, String), Acc> = HashMap::new();
        for (_, _, a) in self.collect(range.from_ms, range.to_ms, q.area.as_deref(), q.backend.as_deref()).await? {
            let m = by.entry((a.fingerprint.clone(), a.backend.clone())).or_default();
            m.fingerprint.clone_from(&a.fingerprint);
            m.backend.clone_from(&a.backend);
            m.area.clone_from(&a.area);
            m.statement.clone_from(&a.statement);
            m.merge(&a);
        }
        let needle = q.text.as_deref().map(str::to_lowercase);
        let mut stats: Vec<StatementStat> = by
            .values()
            .filter(|a| needle.as_deref().is_none_or(|n| a.statement.to_lowercase().contains(n)))
            .map(Acc::stat)
            .collect();
        let key = |s: &StatementStat| -> f64 {
            match sort {
                "calls" => s.calls as f64,
                "mean" => s.mean_ms,
                "max" => s.max_ms,
                "p50" => s.p50_ms,
                "p95" => s.p95_ms,
                "p99" => s.p99_ms,
                "errors" => s.errors as f64,
                "rows" => s.rows as f64,
                "slow" => s.slow as f64,
                _ => s.total_ms,
            }
        };
        stats.sort_by(|a, b| key(b).total_cmp(&key(a)).then_with(|| a.fingerprint.cmp(&b.fingerprint)));
        let total = stats.len();
        let limit = q.limit.clamp(1, 200);
        let items = stats.into_iter().skip(q.offset).take(limit).collect();
        Ok(StatementList {
            items,
            total,
            from: iso(range.from_ms),
            to: iso(range.to_ms),
            limit,
            offset: q.offset,
        })
    }

    /// One statement shape with its time series.
    ///
    /// # Errors
    /// `not_found` when the range holds nothing of it.
    pub async fn statement_detail(&self, fingerprint: &str, range: Range) -> Result<StatementDetail> {
        let mut total = Acc::default();
        let mut series: Vec<StatementPoint> = Vec::new();
        for (start, len, a) in self.collect(range.from_ms, range.to_ms, None, None).await? {
            if a.fingerprint != fingerprint {
                continue;
            }
            if total.fingerprint.is_empty() {
                total.fingerprint.clone_from(&a.fingerprint);
                total.backend.clone_from(&a.backend);
                total.area.clone_from(&a.area);
                total.statement.clone_from(&a.statement);
            }
            total.merge(&a);
            let s = a.stat();
            series.push(StatementPoint {
                at: iso(start),
                seconds: len / 1000,
                calls: s.calls,
                errors: s.errors,
                mean_ms: s.mean_ms,
                p95_ms: s.p95_ms,
                max_ms: s.max_ms,
            });
        }
        if total.calls == 0 {
            return Err(InsightsError::NotFound(format!("statement {fingerprint}")));
        }
        Ok(StatementDetail {
            stat: total.stat(),
            series,
        })
    }

    /// Statements whose recent p95 is at least `min_ratio` times their
    /// baseline p95 (and whose error rate did not jump), most costly first.
    ///
    /// # Errors
    /// A database error or an invalid window.
    pub async fn regressions(&self, q: &RegressionQuery) -> Result<RegressionReport> {
        if q.window_secs < 1 || q.baseline_secs < 1 || q.window_secs + q.baseline_secs > 14 * 86_400 {
            return Err(InsightsError::invalid(
                "invalid_range",
                "window and baseline must be positive and together at most 14 days",
            ));
        }
        let until = q.until_ms.unwrap_or_else(|| now_ms() + 1);
        let recent_from = until - q.window_secs * 1000;
        let baseline_from = recent_from - q.baseline_secs * 1000;
        let mut recent: HashMap<(String, String), Acc> = HashMap::new();
        let mut base: HashMap<(String, String), Acc> = HashMap::new();
        for (start, _, a) in self.collect(baseline_from, until, q.area.as_deref(), None).await? {
            if a.fingerprint == OTHER_FINGERPRINT || a.statement == OVERFLOW_TEXT {
                continue;
            }
            let side = if start >= recent_from { &mut recent } else { &mut base };
            let m = side.entry((a.fingerprint.clone(), a.backend.clone())).or_default();
            m.fingerprint.clone_from(&a.fingerprint);
            m.backend.clone_from(&a.backend);
            m.area.clone_from(&a.area);
            m.statement.clone_from(&a.statement);
            m.merge(&a);
        }
        let mut items = Vec::new();
        for (k, r) in &recent {
            let Some(b) = base.get(k) else { continue };
            if r.calls < q.min_calls || b.calls < q.min_calls {
                continue;
            }
            let (rs, bs) = (WindowStat::of(r), WindowStat::of(b));
            let delta_ms = r3(rs.p95_ms - bs.p95_ms);
            let ratio = if bs.p95_ms > 0.0 { r3(rs.p95_ms / bs.p95_ms) } else { f64::INFINITY };
            let extra_ms = r3(delta_ms * r.calls as f64);
            let make = |kind: &'static str, ratio: f64| Regression {
                kind,
                fingerprint: r.fingerprint.clone(),
                statement: r.statement.clone(),
                area: r.area.clone(),
                backend: r.backend.clone(),
                recent: rs.clone(),
                baseline: bs.clone(),
                ratio: if ratio.is_finite() { ratio } else { 1e9 },
                delta_ms,
                extra_ms,
            };
            if ratio >= q.min_ratio && delta_ms >= q.min_delta_ms {
                items.push(make("latency", ratio));
            }
            let jump = rs.error_rate - bs.error_rate;
            if rs.errors >= 3 && rs.error_rate >= 0.05 && jump >= 0.05 {
                let mut e = make("errors", r3(jump));
                e.extra_ms = r.errors as f64;
                items.push(e);
            }
        }
        items.sort_by(|a, b| b.extra_ms.total_cmp(&a.extra_ms).then_with(|| a.fingerprint.cmp(&b.fingerprint)));
        items.truncate(200);
        metrics::gauge!("forgebase_db_regressions").set(items.iter().filter(|i| i.kind == "latency").count() as f64);
        Ok(RegressionReport {
            recent_from: iso(recent_from),
            recent_to: iso(until),
            baseline_from: iso(baseline_from),
            baseline_to: iso(recent_from),
            min_ratio: q.min_ratio,
            min_calls: q.min_calls,
            min_delta_ms: q.min_delta_ms,
            items,
        })
    }

    /// Slow statements, newest first.
    ///
    /// # Errors
    /// A database error.
    pub async fn slow_statements(&self, range: Range, q: &SlowQuery) -> Result<SlowStatementPage> {
        let limit = q.limit.clamp(1, 500);
        let rows = self
            .fetch(
                "SELECT id, at, fingerprint, backend, area, statement, duration_us, rows, error, write
                 FROM _forgebase_slow_statements
                 WHERE at >= ?1 AND at < ?2
                   AND (CAST(?3 AS TEXT) IS NULL OR fingerprint = ?3)
                   AND (CAST(?4 AS TEXT) IS NULL OR area = ?4)
                   AND (CAST(?5 AS BIGINT) IS NULL OR duration_us >= ?5)
                   AND (CAST(?6 AS TEXT) IS NULL OR lower(statement) LIKE ?6 ESCAPE '\\')
                   AND (CAST(?7 AS BIGINT) IS NULL OR id < ?7)
                 ORDER BY id DESC LIMIT ?8",
                vec![
                    range.from_ms.into(),
                    range.to_ms.into(),
                    q.fingerprint.clone().into(),
                    q.area.clone().into(),
                    q.min_ms.map(|m| (m * 1000.0) as i64).into(),
                    q.text.as_deref().map(contains_pattern).into(),
                    q.cursor.into(),
                    (i64::from(limit) + 1).into(),
                ],
            )
            .await?;
        let mut items: Vec<SlowStatementEntry> = rows
            .rows
            .iter()
            .map(|r| SlowStatementEntry {
                id: int(r, 0),
                at: iso(int(r, 1)),
                fingerprint: text(r, 2),
                backend: text(r, 3),
                area: text(r, 4),
                statement: text(r, 5),
                duration_ms: ms(int(r, 6) as u64),
                rows: int(r, 7),
                error: int(r, 8) != 0,
                write: int(r, 9) != 0,
            })
            .collect();
        let more = items.len() > limit as usize;
        items.truncate(limit as usize);
        let next_cursor = if more { items.last().map(|i| i.id) } else { None };
        Ok(SlowStatementPage { items, next_cursor })
    }
}

fn insert_stat(start_ms: i64, len_ms: i64, a: &Acc) -> Statement {
    Statement::new(
        "INSERT INTO _forgebase_stmt_stats
             (bucket_start, bucket_ms, fingerprint, backend, area, statement, calls, errors, writes, slow, total_us, max_us, min_us, rows, sketch)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        vec![
            start_ms.into(),
            len_ms.into(),
            a.fingerprint.clone().into(),
            a.backend.clone().into(),
            a.area.clone().into(),
            a.statement.clone().into(),
            to_i64(a.calls).into(),
            to_i64(a.errors).into(),
            to_i64(a.writes).into(),
            to_i64(a.slow).into(),
            to_i64(a.total_us).into(),
            to_i64(a.max_us).into(),
            to_i64(a.min_us).into(),
            to_i64(a.rows).into(),
            a.sketch.to_json().into(),
        ],
    )
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Prometheus gauges per area for alert rules (ADR-0036 §1): latency
/// quantiles of the interval just flushed, and its statement count.
fn publish_gauges(aggs: &[StatementAggregate]) {
    let mut by: HashMap<&'static str, (Sketch, u64, u64)> = HashMap::new();
    for a in aggs {
        let e = by.entry(a.area).or_default();
        e.0.merge(&a.sketch);
        e.1 += a.calls;
        e.2 += a.errors;
    }
    for (area, (sk, calls, errors)) in by {
        for (q, label) in [(0.5, "0.5"), (0.95, "0.95"), (0.99, "0.99")] {
            if let Some(v) = sk.quantile(q) {
                metrics::gauge!("forgebase_db_statement_latency_seconds", "area" => area, "quantile" => label)
                    .set(v as f64 / 1_000_000.0);
            }
        }
        metrics::gauge!("forgebase_db_statement_calls", "area" => area).set(calls as f64);
        metrics::gauge!("forgebase_db_statement_errors", "area" => area).set(errors as f64);
    }
}
