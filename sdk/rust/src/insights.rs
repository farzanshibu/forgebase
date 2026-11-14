//! Operations insights (`/api/v1/insights`, ADR-0036): statement statistics by shape, query
//! regressions and the slow-statement log, per-tenant usage with estimated cost, function
//! profiles and invocations, and audit investigation with streamed CSV / JSONL export.
//! `client.insights()`.
//!
//! Administrator-only reads (`engine:read_insights` / `engine:read_audit`). Ranges are `from` /
//! `to` (RFC 3339 or `YYYY-MM-DD`, `to` exclusive). Response types are the generated wire models
//! ([`generated`](crate::generated)).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::json;

use crate::error::{Error, Result};
use crate::generated::{
    AuditEntryView, AuditPageView, AuditRelatedView, AuditSummaryView, FlushReport, FunctionDetail, FunctionStatList,
    InvocationDetail, InvocationPage, MetricList, RegressionReport, RollupResult, SlowStatementPage, StatementDetail,
    StatementList, StatementStatus, UsageReport, UsageSeries,
};
use crate::http::{Req, Transport, describe, seg};

/// A time range (`from`, `to` exclusive; RFC 3339 or `YYYY-MM-DD`).
#[derive(Debug, Clone, Default)]
pub struct TimeRange {
    /// Start.
    pub from: Option<String>,
    /// End (exclusive).
    pub to: Option<String>,
}

impl TimeRange {
    /// `from` until `to`.
    #[must_use]
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: Some(from.into()),
            to: Some(to.into()),
        }
    }

    fn apply(&self, r: Req) -> Req {
        r.q_opt("from", self.from.as_deref()).q_opt("to", self.to.as_deref())
    }
}

/// Options of [`InsightsApi::queries`].
#[derive(Debug, Clone, Default)]
pub struct QueriesQuery {
    /// Range.
    pub range: TimeRange,
    /// `total_time` (default), `calls`, `mean`, `max`, `p50`, `p95`, `p99`, `errors`, `rows` or `slow`.
    pub sort: Option<String>,
    /// Only this subsystem (`data`, `jobs`, `queue`, `billing`, ...).
    pub area: Option<String>,
    /// `sqlite` or `postgres`.
    pub backend: Option<String>,
    /// Statement text contains.
    pub q: Option<String>,
    /// Page size (default 50, at most 200).
    pub limit: Option<u32>,
    /// Rows to skip.
    pub offset: Option<u32>,
}

/// Options of [`InsightsApi::regressions`].
#[derive(Debug, Clone, Default)]
pub struct RegressionsQuery {
    /// Recent window in seconds (default 3600).
    pub window: Option<u64>,
    /// Baseline window before it, in seconds (default 86400).
    pub baseline: Option<u64>,
    /// End of the recent window (default now).
    pub until: Option<String>,
    /// Runs needed in each window (default 20).
    pub min_calls: Option<u32>,
    /// Smallest p95 ratio (default 2).
    pub min_ratio: Option<f64>,
    /// Smallest p95 growth in milliseconds (default 1).
    pub min_delta_ms: Option<f64>,
    /// Only this subsystem.
    pub area: Option<String>,
}

/// Options of [`InsightsApi::slow_queries`].
#[derive(Debug, Clone, Default)]
pub struct SlowQueriesQuery {
    /// Range.
    pub range: TimeRange,
    /// One statement shape.
    pub fingerprint: Option<String>,
    /// Area.
    pub area: Option<String>,
    /// At least this long (ms).
    pub min_ms: Option<f64>,
    /// Statement text contains.
    pub q: Option<String>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<i64>,
    /// Page size (default 50, at most 500).
    pub limit: Option<u32>,
}

/// Options of [`InsightsApi::usage`] and [`InsightsApi::usage_top`].
#[derive(Debug, Clone, Default)]
pub struct UsageQuery {
    /// `day` or `month` (with `date`), or use `range`.
    pub period: Option<String>,
    /// `YYYY-MM-DD`, or `YYYY-MM` for a month (default today / this month).
    pub date: Option<String>,
    /// Custom range (at most 400 days) instead of `period`.
    pub range: TimeRange,
    /// `environment` (default), `project`, `org` or `realm`.
    pub group_by: Option<String>,
    /// [`InsightsApi::usage`] only: these metrics (comma-separated).
    pub metric: Option<String>,
    /// [`InsightsApi::usage`] only: a metric key or `cost`.
    pub sort: Option<String>,
    /// Only this realm.
    pub realm: Option<String>,
    /// Only this organization.
    pub org: Option<String>,
    /// Only this project.
    pub project: Option<String>,
    /// Currency of the estimate (ISO 4217).
    pub currency: Option<String>,
    /// [`InsightsApi::usage`] only: page size (default 50, at most 500).
    pub limit: Option<u32>,
    /// [`InsightsApi::usage`] only: rows to skip.
    pub offset: Option<u32>,
}

impl UsageQuery {
    fn apply(&self, r: Req) -> Req {
        self.range
            .apply(r)
            .q_opt("period", self.period.as_deref())
            .q_opt("date", self.date.as_deref())
            .q_opt("group_by", self.group_by.as_deref())
            .q_opt("currency", self.currency.as_deref())
    }
}

/// Options of [`InsightsApi::invocations`].
#[derive(Debug, Clone, Default)]
pub struct InvocationsQuery {
    /// Function name.
    pub function: Option<String>,
    /// Function version.
    pub version: Option<u32>,
    /// `ok`, `failed`, `trap`, `fuel_exhausted`, `memory_limit`, `timeout`, ...
    pub status: Option<String>,
    /// `http`, `hook`, `job` or `test`.
    pub trigger: Option<String>,
    /// Executions of one request.
    pub request_id: Option<String>,
    /// Only executions that did not end `ok`.
    pub errors: Option<bool>,
    /// At least this long (ms).
    pub min_duration_ms: Option<u64>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<i64>,
    /// Page size (default 50, at most 500).
    pub limit: Option<u32>,
}

/// Filters of [`InsightsApi::audit`].
#[derive(Debug, Clone, Default)]
pub struct AuditQuery {
    /// Actor.
    pub actor: Option<String>,
    /// `user`, `admin`, `service_account`, `api_key` or `anonymous`.
    pub actor_type: Option<String>,
    /// Events starting with this (`login`, `api_key.`).
    pub action: Option<String>,
    /// Resource type.
    pub resource_type: Option<String>,
    /// Collection.
    pub collection: Option<String>,
    /// Resource id.
    pub resource_id: Option<String>,
    /// `success` or `failure`.
    pub outcome: Option<String>,
    /// Client IP.
    pub ip: Option<String>,
    /// Request id.
    pub request_id: Option<String>,
    /// Range.
    pub range: TimeRange,
    /// Text in the details, event, resource id or user agent.
    pub q: Option<String>,
    /// `desc` (default) or `asc`.
    pub order: Option<String>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<i64>,
    /// Page size (default 50, at most 1000).
    pub limit: Option<u32>,
}

impl AuditQuery {
    fn apply(&self, r: Req) -> Req {
        self.range
            .apply(r)
            .q_opt("actor", self.actor.as_deref())
            .q_opt("actor_type", self.actor_type.as_deref())
            .q_opt("action", self.action.as_deref())
            .q_opt("resource_type", self.resource_type.as_deref())
            .q_opt("collection", self.collection.as_deref())
            .q_opt("resource_id", self.resource_id.as_deref())
            .q_opt("outcome", self.outcome.as_deref())
            .q_opt("ip", self.ip.as_deref())
            .q_opt("request_id", self.request_id.as_deref())
            .q_opt("q", self.q.as_deref())
            .q_opt("order", self.order.as_deref())
    }
}

/// Export format of [`InsightsApi::export_audit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Comma-separated values with a header row.
    Csv,
    /// One JSON object per line.
    Jsonl,
}

/// A streamed audit export: read it chunk by chunk (or line by line) instead of holding the whole file.
pub struct AuditExport {
    res: reqwest::Response,
    pending: Vec<u8>,
    done: bool,
}

impl AuditExport {
    /// `Content-Type` the server answered with (`text/csv` or `application/x-ndjson`).
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.res.headers().get("content-type").and_then(|v| v.to_str().ok())
    }

    /// The next chunk of the body; `None` at the end.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        self.res.chunk().await.map_err(|e| Error::Network(describe(&e)))
    }

    /// The next line (without its line break); `None` at the end. Works for both formats (a CSV
    /// line may be a record's end only when no field holds a line break: use [`chunk`](Self::chunk)
    /// and a CSV parser for general CSV).
    pub async fn line(&mut self) -> Result<Option<String>> {
        loop {
            if let Some(i) = self.pending.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = self.pending.drain(..=i).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
            }
            if self.done {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                let rest = std::mem::take(&mut self.pending);
                return Ok(Some(String::from_utf8_lossy(&rest).into_owned()));
            }
            match self.chunk().await? {
                Some(c) => self.pending.extend_from_slice(&c),
                None => self.done = true,
            }
        }
    }

    /// The whole export.
    pub async fn text(mut self) -> Result<String> {
        let mut out = std::mem::take(&mut self.pending);
        while let Some(c) = self.chunk().await? {
            out.extend_from_slice(&c);
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
}

/// Operations insights: `client.insights()`.
#[derive(Clone)]
pub struct InsightsApi {
    pub(crate) t: Arc<Transport>,
}

const I: &str = "/api/v1/insights";

impl InsightsApi {
    /// Whether statement statistics are collected, since when, and how much is held.
    pub async fn status(&self) -> Result<StatementStatus> {
        self.t.json(Req::get(format!("{I}/status"))).await
    }

    /// Writes the in-memory statistics to the store now.
    pub async fn flush(&self) -> Result<FlushReport> {
        self.t.json(Req::post(format!("{I}/flush")).json_raw(json!({}))).await
    }

    // ---- statements ----

    /// Statement statistics by shape (fingerprint).
    pub async fn queries(&self, q: &QueriesQuery) -> Result<StatementList> {
        self.t
            .json(
                q.range
                    .apply(Req::get(format!("{I}/queries")))
                    .q_opt("sort", q.sort.as_deref())
                    .q_opt("area", q.area.as_deref())
                    .q_opt("backend", q.backend.as_deref())
                    .q_opt("q", q.q.as_deref())
                    .q_opt("limit", q.limit)
                    .q_opt("offset", q.offset),
            )
            .await
    }

    /// One statement shape with its time series and plan hints.
    pub async fn query(&self, fingerprint: &str, range: &TimeRange) -> Result<StatementDetail> {
        self.t
            .json(range.apply(Req::get(format!("{I}/queries/{}", seg(fingerprint)))))
            .await
    }

    /// Statement shapes that got slower against their baseline.
    pub async fn regressions(&self, q: &RegressionsQuery) -> Result<RegressionReport> {
        self.t
            .json(
                Req::get(format!("{I}/queries/regressions"))
                    .q_opt("window", q.window)
                    .q_opt("baseline", q.baseline)
                    .q_opt("until", q.until.as_deref())
                    .q_opt("min_calls", q.min_calls)
                    .q_opt("min_ratio", q.min_ratio)
                    .q_opt("min_delta_ms", q.min_delta_ms)
                    .q_opt("area", q.area.as_deref()),
            )
            .await
    }

    /// The redacted slow-statement log (SQL text only, never parameter values).
    pub async fn slow_queries(&self, q: &SlowQueriesQuery) -> Result<SlowStatementPage> {
        self.t
            .json(
                q.range
                    .apply(Req::get(format!("{I}/slow-queries")))
                    .q_opt("fingerprint", q.fingerprint.as_deref())
                    .q_opt("area", q.area.as_deref())
                    .q_opt("min_ms", q.min_ms)
                    .q_opt("q", q.q.as_deref())
                    .q_opt("cursor", q.cursor)
                    .q_opt("limit", q.limit),
            )
            .await
    }

    // ---- usage ----

    /// Per-tenant usage with estimated cost.
    pub async fn usage(&self, q: &UsageQuery) -> Result<UsageReport> {
        self.t
            .json(
                q.apply(Req::get(format!("{I}/usage")))
                    .q_opt("metric", q.metric.as_deref())
                    .q_opt("sort", q.sort.as_deref())
                    .q_opt("realm", q.realm.as_deref())
                    .q_opt("org", q.org.as_deref())
                    .q_opt("project", q.project.as_deref())
                    .q_opt("limit", q.limit)
                    .q_opt("offset", q.offset),
            )
            .await
    }

    /// The `n` top consumers (default 10, at most 100) of `metric` (a metric key such as
    /// `storage.bytes`, or `cost`).
    pub async fn usage_top(&self, metric: &str, n: Option<u32>, q: &UsageQuery) -> Result<UsageReport> {
        self.t
            .json(q.apply(Req::get(format!("{I}/usage/top")).q("metric", metric).q_opt("n", n)))
            .await
    }

    /// A time series of usage (`scope` is `env:<environment id>`; default range: 30 days).
    pub async fn usage_series(&self, scope: &str, range: &TimeRange) -> Result<UsageSeries> {
        self.t
            .json(range.apply(Req::get(format!("{I}/usage/series")).q("scope", scope)))
            .await
    }

    /// The metrics usage reports know.
    pub async fn usage_metrics(&self) -> Result<MetricList> {
        self.t.json(Req::get(format!("{I}/usage/metrics"))).await
    }

    /// Rolls up one day of usage now (`YYYY-MM-DD`; default today).
    pub async fn usage_rollup(&self, day: Option<&str>) -> Result<RollupResult> {
        self.t
            .json(
                Req::post(format!("{I}/usage/rollup"))
                    .q_opt("day", day)
                    .json_raw(json!({})),
            )
            .await
    }

    // ---- functions ----

    /// Function profiles: calls, errors, latency.
    pub async fn functions(
        &self,
        range: &TimeRange,
        function: Option<&str>,
        version: Option<u32>,
    ) -> Result<FunctionStatList> {
        self.t
            .json(
                range
                    .apply(Req::get(format!("{I}/functions")))
                    .q_opt("function", function)
                    .q_opt("version", version),
            )
            .await
    }

    /// One function's profile.
    pub async fn function(&self, name: &str, range: &TimeRange) -> Result<FunctionDetail> {
        self.t
            .json(range.apply(Req::get(format!("{I}/functions/{}", seg(name)))))
            .await
    }

    /// Function invocations, newest first.
    pub async fn invocations(&self, q: &InvocationsQuery) -> Result<InvocationPage> {
        self.t
            .json(
                Req::get(format!("{I}/functions/invocations"))
                    .q_opt("function", q.function.as_deref())
                    .q_opt("version", q.version)
                    .q_opt("status", q.status.as_deref())
                    .q_opt("trigger", q.trigger.as_deref())
                    .q_opt("request_id", q.request_id.as_deref())
                    .q_opt("errors", q.errors)
                    .q_opt("min_duration_ms", q.min_duration_ms)
                    .q_opt("cursor", q.cursor)
                    .q_opt("limit", q.limit),
            )
            .await
    }

    /// One invocation with its log lines and trace.
    pub async fn invocation(&self, id: &str) -> Result<InvocationDetail> {
        self.t
            .json(Req::get(format!("{I}/functions/invocations/{}", seg(id))))
            .await
    }

    // ---- audit ----

    /// Audit entries, filtered and paged (`engine:read_audit`).
    pub async fn audit(&self, q: &AuditQuery) -> Result<AuditPageView> {
        self.t
            .json(
                q.apply(
                    Req::get(format!("{I}/audit"))
                        .q_opt("cursor", q.cursor)
                        .q_opt("limit", q.limit),
                ),
            )
            .await
    }

    /// One audit entry.
    pub async fn audit_entry(&self, id: &str) -> Result<AuditEntryView> {
        self.t.json(Req::get(format!("{I}/audit/{}", seg(id)))).await
    }

    /// Entries around one entry: `window_secs` either side (default 300, at most 3600), at most `limit` of each (default 100).
    pub async fn audit_related(
        &self,
        id: &str,
        window_secs: Option<u32>,
        limit: Option<u32>,
    ) -> Result<AuditRelatedView> {
        self.t
            .json(
                Req::get(format!("{I}/audit/{}/related", seg(id)))
                    .q_opt("window_secs", window_secs)
                    .q_opt("limit", limit),
            )
            .await
    }

    /// Counts by action, outcome and actor over a range.
    pub async fn audit_summary(&self, range: &TimeRange) -> Result<AuditSummaryView> {
        self.t.json(range.apply(Req::get(format!("{I}/audit/summary")))).await
    }

    /// Streams the entries matching `q` as CSV or JSONL, at most `max_rows` (default 100 000, at
    /// most 1 000 000). Nothing is buffered: read the result with [`AuditExport::chunk`] or
    /// [`AuditExport::line`].
    pub async fn export_audit(
        &self,
        q: &AuditQuery,
        format: ExportFormat,
        max_rows: Option<u32>,
    ) -> Result<AuditExport> {
        let fmt = match format {
            ExportFormat::Csv => "csv",
            ExportFormat::Jsonl => "jsonl",
        };
        let req = q
            .apply(Req::get(format!("{I}/audit/export")))
            .q("format", fmt)
            .q_opt("max_rows", max_rows);
        let res = self.t.send(&req).await?;
        Ok(AuditExport {
            res,
            pending: Vec::new(),
            done: false,
        })
    }
}
