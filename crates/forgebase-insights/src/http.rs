//! REST under `/api/v1/insights` (mounted as an `ApiExtension`, tag
//! `insights`; ADR-0036). Administrators only: every handler asks the
//! `ReadInsights` (statements, usage, functions) or `ReadAudit` action first,
//! which platform administrators hold. Every list is bounded and paged.

use axum::Json;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use forgebase_api::http::Query;
use forgebase_api::principal::Access;
use forgebase_api::{ApiError, ApiExtension};
use forgebase_auth::{AUDIT_PAGE_MAX, AuditEntry, AuditSearch};
use forgebase_core::ErrorCode;
use forgebase_data::AccessContext;
use forgebase_functions::{InvocationDetail, InvocationQuery};
use serde::Serialize;
use serde_json::Value as Json_;
use utoipa::{OpenApi, ToSchema};

use crate::error::InsightsError;
use crate::functions::{FunctionDetail, FunctionPoint, FunctionStat, FunctionStatList, HostBreakdown, InvocationPage};
use crate::queries::{
    Regression, RegressionQuery, RegressionReport, SlowQuery, SlowStatementEntry, SlowStatementPage, StatementDetail,
    StatementList, StatementPoint, StatementQuery, StatementStat, StatementStatus, WindowStat,
};
use crate::usage::{
    CostEstimate, CostLine, Level, MetricInfo, MetricValue, UsageQuery, UsageReport, UsageRow, UsageSeries,
    builtin_metrics,
};
use crate::util::{Range, parse_date, parse_time};
use crate::{FlushReport, InsightsService};

type ApiResult = Result<Response, ApiError>;

fn api(e: InsightsError) -> ApiError {
    ApiError::from_error(&e)
}

fn ok<T: Serialize>(v: T) -> ApiResult {
    Ok(Json(v).into_response())
}

fn time_param(q: &Query, name: &str) -> Result<Option<i64>, ApiError> {
    match q.get(name) {
        None | Some("") => Ok(None),
        Some(v) => parse_time(v).map(Some).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_parameter",
                format!("{name} must be an RFC 3339 time or a date, YYYY-MM-DD"),
            )
        }),
    }
}

fn int_param(q: &Query, name: &str) -> Result<Option<i64>, ApiError> {
    match q.get(name) {
        None | Some("") => Ok(None),
        Some(v) => v
            .parse::<i64>()
            .map(Some)
            .map_err(|_| ApiError::bad_request("invalid_parameter", format!("{name} must be an integer"))),
    }
}

fn opt(q: &Query, name: &str) -> Option<String> {
    q.get(name).filter(|s| !s.is_empty()).map(str::to_owned)
}

fn range(q: &Query, default_secs: i64, max_days: i64) -> Result<Range, ApiError> {
    Range::resolve(time_param(q, "from")?, time_param(q, "to")?, default_secs, max_days).map_err(api)
}

/// The period of a usage query: `period=day|month` with `date`, else `from`/`to`
/// (default: the current month so far).
fn usage_range(q: &Query) -> Result<Range, ApiError> {
    if let Some(p) = q.get("period").filter(|p| !p.is_empty()) {
        let today = time::OffsetDateTime::now_utc().date();
        let date = match q.get("date").filter(|d| !d.is_empty()) {
            None => today,
            Some(d) => parse_date(d)
                .or_else(|| parse_date(&format!("{d}-01")))
                .ok_or_else(|| ApiError::bad_request("invalid_parameter", "date must be YYYY-MM-DD or YYYY-MM"))?,
        };
        let start = match p {
            "day" => date,
            "month" => date.replace_day(1).unwrap_or(date),
            _ => {
                return Err(ApiError::bad_request(
                    "invalid_parameter",
                    "period must be day or month",
                ));
            }
        };
        let end = if p == "day" {
            start.next_day().unwrap_or(start)
        } else {
            let (y, m) = (start.year(), start.month());
            let next = if m == time::Month::December {
                time::Date::from_calendar_date(y + 1, time::Month::January, 1)
            } else {
                time::Date::from_calendar_date(y, m.next(), 1)
            };
            next.unwrap_or(start)
        };
        let ms = |d: time::Date| d.midnight().assume_utc().unix_timestamp() * 1000;
        return Ok(Range {
            from_ms: ms(start),
            to_ms: ms(end),
        });
    }
    if q.get("from").is_none() && q.get("to").is_none() {
        let now = time::OffsetDateTime::now_utc();
        let start = now.date().replace_day(1).unwrap_or(now.date());
        return Ok(Range {
            from_ms: start.midnight().assume_utc().unix_timestamp() * 1000,
            to_ms: now.unix_timestamp() * 1000 + 1,
        });
    }
    range(q, 30 * 86_400, 400)
}

fn page(q: &Query, default: u32, max: u32) -> Result<(usize, usize), ApiError> {
    let limit = q.u32("limit")?.unwrap_or(default).clamp(1, max) as usize;
    let offset = q.u32("offset")?.unwrap_or(0) as usize;
    Ok((limit, offset))
}

/// The routes, relative to `/api/v1`, with their state applied.
pub fn router<S: Clone + Send + Sync + 'static>(svc: InsightsService) -> Router<S> {
    Router::<InsightsService>::new()
        .route("/insights/status", get(status))
        .route("/insights/flush", post(flush))
        .route("/insights/queries", get(queries))
        .route("/insights/queries/regressions", get(regressions))
        .route("/insights/queries/{fingerprint}", get(query_detail))
        .route("/insights/slow-queries", get(slow_queries))
        .route("/insights/usage", get(usage))
        .route("/insights/usage/top", get(usage_top))
        .route("/insights/usage/metrics", get(usage_metrics))
        .route("/insights/usage/series", get(usage_series))
        .route("/insights/usage/rollup", post(usage_rollup))
        .route("/insights/functions", get(functions))
        .route("/insights/functions/invocations", get(invocations))
        .route("/insights/functions/invocations/{id}", get(invocation))
        .route("/insights/functions/{name}", get(function_detail))
        .route("/insights/audit", get(audit_search))
        .route("/insights/audit/summary", get(audit_summary))
        .route("/insights/audit/export", get(audit_export))
        .route("/insights/audit/{id}", get(audit_entry))
        .route("/insights/audit/{id}/related", get(audit_related))
        .with_state(svc)
}

/// The routes and OpenAPI fragment, ready for `RouterOptions::extensions`.
pub fn extension(svc: InsightsService) -> ApiExtension {
    ApiExtension {
        routes: router(svc),
        openapi: openapi(),
    }
}

/// OpenAPI paths and schemas.
pub fn openapi() -> utoipa::openapi::OpenApi {
    InsightsDoc::openapi()
}

#[derive(OpenApi)]
#[openapi(
    paths(
        status, flush, queries, regressions, query_detail, slow_queries, usage, usage_top, usage_metrics, usage_series,
        usage_rollup, functions, invocations, invocation, function_detail, audit_search, audit_summary, audit_export,
        audit_entry, audit_related,
    ),
    components(schemas(
        StatementStatus, FlushReport, StatementStat, StatementList, StatementPoint, StatementDetail, WindowStat,
        Regression, RegressionReport, SlowStatementEntry, SlowStatementPage, UsageReport, UsageRow, MetricValue,
        CostEstimate, CostLine, UsageSeries, MetricInfo, Level, MetricList, FunctionStat, FunctionStatList,
        FunctionPoint, FunctionDetail, HostBreakdown, InvocationPage, InvocationDetail, AuditEntryView, AuditPageView,
        AuditRelatedView, AuditSummaryView, RollupResult,
    )),
    tags((name = "insights", description = "Operations insights for administrators: slow statements and query regressions, per-tenant usage and cost estimates, function profiles and audit investigation"))
)]
struct InsightsDoc;

// ---------------------------------------------------------------------------
// Access
// ---------------------------------------------------------------------------

fn insights(s: &InsightsService, ctx: &AccessContext) -> Result<(), ApiError> {
    s.authorize_insights(ctx).map_err(api)
}

fn audit_realm(s: &InsightsService, ctx: &AccessContext) -> Result<Option<String>, ApiError> {
    s.authorize_audit(ctx).map_err(api)?;
    Ok((!ctx.is_privileged()).then(|| ctx.realm_id.clone()))
}

// ---------------------------------------------------------------------------
// Statements
// ---------------------------------------------------------------------------

/// State of the statement statistics and the slow statement threshold.
#[utoipa::path(get, path = "/api/v1/insights/status", operation_id = "insights_status", tag = "insights",
    responses((status = 200, description = "Status", body = StatementStatus)))]
async fn status(State(s): State<InsightsService>, Access(ctx): Access) -> ApiResult {
    insights(&s, &ctx)?;
    ok(s.statement_status().await.map_err(api)?)
}

/// Writes the statistics held in memory to the database now (they are written
/// every `[observability] flush_interval_secs` anyway).
#[utoipa::path(post, path = "/api/v1/insights/flush", operation_id = "insights_flush", tag = "insights",
    responses((status = 200, description = "What was written", body = FlushReport)))]
async fn flush(State(s): State<InsightsService>, Access(ctx): Access) -> ApiResult {
    insights(&s, &ctx)?;
    ok(s.flush().await.map_err(api)?)
}

/// Statistics per statement shape (fingerprint) over a time range: calls,
/// errors, rows, total, mean and maximum time and the 50th, 95th and 99th
/// percentile. The text has every value replaced by `?`.
#[utoipa::path(get, path = "/api/v1/insights/queries", operation_id = "insights_queries_list", tag = "insights",
    params(
        ("from" = Option<String>, Query, description = "Start (RFC 3339 or YYYY-MM-DD); default one hour before `to`"),
        ("to" = Option<String>, Query, description = "End, exclusive; default now"),
        ("sort" = Option<String>, Query, description = "total_time (default), calls, mean, max, p50, p95, p99, errors, rows, slow"),
        ("area" = Option<String>, Query, description = "Only this subsystem (data, jobs, queue, billing, ...)"),
        ("backend" = Option<String>, Query, description = "sqlite or postgres"),
        ("q" = Option<String>, Query, description = "Only statements containing this text"),
        ("limit" = Option<u32>, Query, description = "Page size (default 50, at most 200)"),
        ("offset" = Option<u32>, Query, description = "Rows to skip"),
    ),
    responses((status = 200, description = "Statements", body = StatementList)))]
async fn queries(State(s): State<InsightsService>, Access(ctx): Access, q: Query) -> ApiResult {
    insights(&s, &ctx)?;
    let (limit, offset) = page(&q, 50, 200)?;
    let query = StatementQuery {
        area: opt(&q, "area"),
        backend: opt(&q, "backend"),
        text: opt(&q, "q"),
        sort: opt(&q, "sort"),
        limit,
        offset,
    };
    ok(s.statement_stats(range(&q, 3600, 31)?, &query).await.map_err(api)?)
}

/// One statement shape with its time series.
#[utoipa::path(get, path = "/api/v1/insights/queries/{fingerprint}", operation_id = "insights_queries_get", tag = "insights",
    params(
        ("fingerprint" = String, Path, description = "Fingerprint from the list"),
        ("from" = Option<String>, Query, description = "Start; default 24 hours before `to`"),
        ("to" = Option<String>, Query, description = "End, exclusive; default now"),
    ),
    responses((status = 200, description = "Statement", body = StatementDetail), (status = 404, description = "Nothing in the range")))]
async fn query_detail(
    State(s): State<InsightsService>,
    Access(ctx): Access,
    Path(fp): Path<String>,
    q: Query,
) -> ApiResult {
    insights(&s, &ctx)?;
    ok(s.statement_detail(&fp, range(&q, 86_400, 31)?).await.map_err(api)?)
}

/// Statements whose 95th percentile in a recent window is at least `min_ratio`
/// times their baseline (the window before), or whose error rate jumped.
#[utoipa::path(get, path = "/api/v1/insights/queries/regressions", operation_id = "insights_queries_regressions", tag = "insights",
    params(
        ("window" = Option<u32>, Query, description = "Recent window in seconds (default 3600)"),
        ("baseline" = Option<u32>, Query, description = "Baseline window before it, in seconds (default 86400)"),
        ("until" = Option<String>, Query, description = "End of the recent window; default now"),
        ("min_calls" = Option<u32>, Query, description = "Runs needed in each window (default 20)"),
        ("min_ratio" = Option<f64>, Query, description = "Smallest p95 ratio (default 2)"),
        ("min_delta_ms" = Option<f64>, Query, description = "Smallest p95 growth in milliseconds (default 1)"),
        ("area" = Option<String>, Query, description = "Only this subsystem"),
    ),
    responses((status = 200, description = "Regressions, most costly first", body = RegressionReport)))]
async fn regressions(State(s): State<InsightsService>, Access(ctx): Access, q: Query) -> ApiResult {
    insights(&s, &ctx)?;
    let d = RegressionQuery::default();
    let query = RegressionQuery {
        until_ms: time_param(&q, "until")?,
        window_secs: i64::from(q.u32("window")?.unwrap_or(d.window_secs as u32)),
        baseline_secs: i64::from(q.u32("baseline")?.unwrap_or(d.baseline_secs as u32)),
        min_calls: u64::from(q.u32("min_calls")?.unwrap_or(d.min_calls as u32)),
        min_ratio: q.f64("min_ratio")?.unwrap_or(d.min_ratio),
        min_delta_ms: q.f64("min_delta_ms")?.unwrap_or(d.min_delta_ms),
        area: opt(&q, "area"),
    };
    ok(s.regressions(&query).await.map_err(api)?)
}

/// Statements at or above `[observability] slow_query_ms`, newest first, with
/// their text (every value replaced by `?`) and duration. At most 200 are kept
/// per flush interval.
#[utoipa::path(get, path = "/api/v1/insights/slow-queries", operation_id = "insights_slow_queries_list", tag = "insights",
    params(
        ("from" = Option<String>, Query, description = "Start; default 24 hours before `to`"),
        ("to" = Option<String>, Query, description = "End, exclusive; default now"),
        ("fingerprint" = Option<String>, Query, description = "Only this statement shape"),
        ("area" = Option<String>, Query, description = "Only this subsystem"),
        ("min_ms" = Option<f64>, Query, description = "At least this long"),
        ("q" = Option<String>, Query, description = "Only statements containing this text"),
        ("cursor" = Option<i64>, Query, description = "`next_cursor` of the previous page"),
        ("limit" = Option<u32>, Query, description = "Page size (default 50, at most 500)"),
    ),
    responses((status = 200, description = "Slow statements", body = SlowStatementPage)))]
async fn slow_queries(State(s): State<InsightsService>, Access(ctx): Access, q: Query) -> ApiResult {
    insights(&s, &ctx)?;
    let query = SlowQuery {
        fingerprint: opt(&q, "fingerprint"),
        area: opt(&q, "area"),
        min_ms: q.f64("min_ms")?,
        text: opt(&q, "q"),
        cursor: int_param(&q, "cursor")?,
        limit: q.u32("limit")?.unwrap_or(50),
    };
    ok(s.slow_statements(range(&q, 86_400, 31)?, &query).await.map_err(api)?)
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

fn usage_query(q: &Query, sort: Option<String>, limit: usize, offset: usize) -> Result<UsageQuery, ApiError> {
    let group_by = match q.get("group_by").filter(|g| !g.is_empty()) {
        None => Level::Environment,
        Some(g) => Level::parse(g).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_parameter",
                "group_by must be environment, project, org or realm",
            )
        })?,
    };
    Ok(UsageQuery {
        group_by,
        metrics: q.list("metric"),
        sort,
        realm: opt(q, "realm"),
        org: opt(q, "org"),
        project: opt(q, "project"),
        currency: opt(q, "currency"),
        limit,
        offset,
    })
}

const USAGE_PARAMS: &str =
    "period=day|month with date (YYYY-MM-DD or YYYY-MM), or from/to; group_by=environment|project|org|realm";

/// Usage per tenant over a period (a day, a month or a custom range), grouped
/// by environment, project, organization or realm, with an **estimated** cost:
/// each counter priced through the metered price of the same feature in the
/// billing catalog, and the credits the credit rates would charge. Sorted by
/// `sort` (a metric key or `cost`), largest first.
#[utoipa::path(get, path = "/api/v1/insights/usage", operation_id = "insights_usage_list", tag = "insights",
    description = USAGE_PARAMS,
    params(
        ("period" = Option<String>, Query, description = "day or month"),
        ("date" = Option<String>, Query, description = "YYYY-MM-DD, or YYYY-MM for a month; default today / this month"),
        ("from" = Option<String>, Query, description = "Custom range start (at most 400 days)"),
        ("to" = Option<String>, Query, description = "Custom range end, exclusive"),
        ("group_by" = Option<String>, Query, description = "environment (default), project, org or realm"),
        ("metric" = Option<String>, Query, description = "Only these metrics (comma-separated)"),
        ("sort" = Option<String>, Query, description = "A metric key or `cost`"),
        ("realm" = Option<String>, Query, description = "Only this realm"),
        ("org" = Option<String>, Query, description = "Only this organization"),
        ("project" = Option<String>, Query, description = "Only this project"),
        ("currency" = Option<String>, Query, description = "Currency of the estimate (ISO 4217)"),
        ("limit" = Option<u32>, Query, description = "Page size (default 50, at most 500)"),
        ("offset" = Option<u32>, Query, description = "Rows to skip"),
    ),
    responses((status = 200, description = "Usage", body = UsageReport)))]
async fn usage(State(s): State<InsightsService>, Access(ctx): Access, q: Query) -> ApiResult {
    insights(&s, &ctx)?;
    let (limit, offset) = page(&q, 50, 500)?;
    let query = usage_query(&q, opt(&q, "sort"), limit, offset)?;
    ok(s.usage(usage_range(&q)?, &query).await.map_err(api)?)
}

/// The top `n` tenants by one metric (or by `cost`).
#[utoipa::path(get, path = "/api/v1/insights/usage/top", operation_id = "insights_usage_top", tag = "insights",
    params(
        ("metric" = String, Query, description = "A metric key (storage.bytes, usage.api.requests, ...) or `cost`"),
        ("n" = Option<u32>, Query, description = "How many (default 10, at most 100)"),
        ("period" = Option<String>, Query, description = "day or month"),
        ("date" = Option<String>, Query, description = "YYYY-MM-DD or YYYY-MM"),
        ("from" = Option<String>, Query, description = "Custom range start"),
        ("to" = Option<String>, Query, description = "Custom range end, exclusive"),
        ("group_by" = Option<String>, Query, description = "environment (default), project, org or realm"),
        ("currency" = Option<String>, Query, description = "Currency of the estimate"),
    ),
    responses((status = 200, description = "Largest first", body = UsageReport)))]
async fn usage_top(State(s): State<InsightsService>, Access(ctx): Access, q: Query) -> ApiResult {
    insights(&s, &ctx)?;
    let metric = opt(&q, "metric").ok_or_else(|| ApiError::bad_request("invalid_parameter", "metric is required"))?;
    let n = q.u32("n")?.unwrap_or(10).clamp(1, 100) as usize;
    let mut query = usage_query(&q, Some(metric), n, 0)?;
    query.metrics = Vec::new();
    ok(s.usage(usage_range(&q)?, &query).await.map_err(api)?)
}

/// A list of metrics.
#[derive(Debug, Serialize, ToSchema)]
pub struct MetricList {
    /// The built-in metrics.
    pub items: Vec<MetricInfo>,
}

/// The built-in usage metrics and what they mean.
#[utoipa::path(get, path = "/api/v1/insights/usage/metrics", operation_id = "insights_usage_metrics", tag = "insights",
    responses((status = 200, description = "Metrics", body = MetricList)))]
async fn usage_metrics(State(s): State<InsightsService>, Access(ctx): Access) -> ApiResult {
    insights(&s, &ctx)?;
    ok(MetricList {
        items: builtin_metrics(),
    })
}

/// Daily values of one scope (`env:<environment id>`).
#[utoipa::path(get, path = "/api/v1/insights/usage/series", operation_id = "insights_usage_series", tag = "insights",
    params(
        ("scope" = String, Query, description = "env:<environment id>"),
        ("from" = Option<String>, Query, description = "Start; default 30 days before `to`"),
        ("to" = Option<String>, Query, description = "End, exclusive"),
    ),
    responses((status = 200, description = "Daily values", body = UsageSeries)))]
async fn usage_series(State(s): State<InsightsService>, Access(ctx): Access, q: Query) -> ApiResult {
    insights(&s, &ctx)?;
    let scope = opt(&q, "scope").ok_or_else(|| ApiError::bad_request("invalid_parameter", "scope is required"))?;
    ok(s.usage_series(&scope, range(&q, 30 * 86_400, 400)?)
        .await
        .map_err(api)?)
}

/// Result of a rollup.
#[derive(Debug, Serialize, ToSchema)]
pub struct RollupResult {
    /// The day rolled up.
    pub day: String,
    /// Values written.
    pub values: usize,
}

/// Recomputes the resource and work numbers of a day from the live tables
/// (every 15 minutes the server does this for today and yesterday).
#[utoipa::path(post, path = "/api/v1/insights/usage/rollup", operation_id = "insights_usage_rollup", tag = "insights",
    params(("day" = Option<String>, Query, description = "YYYY-MM-DD; default today")),
    responses((status = 200, description = "Done", body = RollupResult)))]
async fn usage_rollup(State(s): State<InsightsService>, Access(ctx): Access, q: Query) -> ApiResult {
    insights(&s, &ctx)?;
    let day = opt(&q, "day").unwrap_or_else(|| crate::util::day_of(crate::util::now_ms()));
    let values = s.rollup_day(&day).await.map_err(api)?;
    ok(RollupResult { day, values })
}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

/// Statistics per function version over a range: executions, errors, timeouts,
/// cold starts, duration percentiles, fuel used against its limit, peak memory
/// and the host-call breakdown.
#[utoipa::path(get, path = "/api/v1/insights/functions", operation_id = "insights_functions_list", tag = "insights",
    params(
        ("from" = Option<String>, Query, description = "Start; default one hour before `to`"),
        ("to" = Option<String>, Query, description = "End, exclusive"),
        ("function" = Option<String>, Query, description = "Only this function"),
        ("version" = Option<i64>, Query, description = "Only this version"),
    ),
    responses((status = 200, description = "Functions", body = FunctionStatList)))]
async fn functions(State(s): State<InsightsService>, Access(ctx): Access, q: Query) -> ApiResult {
    insights(&s, &ctx)?;
    ok(s.function_stats(
        range(&q, 3600, 31)?,
        opt(&q, "function").as_deref(),
        int_param(&q, "version")?,
    )
    .await
    .map_err(api)?)
}

/// One function: statistics per version and a time series.
#[utoipa::path(get, path = "/api/v1/insights/functions/{name}", operation_id = "insights_functions_get", tag = "insights",
    params(
        ("name" = String, Path, description = "Function name"),
        ("from" = Option<String>, Query, description = "Start; default 24 hours before `to`"),
        ("to" = Option<String>, Query, description = "End, exclusive"),
    ),
    responses((status = 200, description = "Function", body = FunctionDetail), (status = 404, description = "Nothing in the range")))]
async fn function_detail(
    State(s): State<InsightsService>,
    Access(ctx): Access,
    Path(name): Path<String>,
    q: Query,
) -> ApiResult {
    insights(&s, &ctx)?;
    ok(s.function_detail(&name, range(&q, 86_400, 31)?).await.map_err(api)?)
}

/// Recent executions with their profile: cold start, fuel and memory against
/// their limits, per-operation host-call counts and times, a timeline of the
/// first host calls, the log lines, the outcome and the trigger. Newest first;
/// kept as long as the execution log (`log_retention` rows per function).
#[utoipa::path(get, path = "/api/v1/insights/functions/invocations", operation_id = "insights_invocations_list", tag = "insights",
    params(
        ("function" = Option<String>, Query, description = "Only this function"),
        ("version" = Option<i64>, Query, description = "Only this version"),
        ("status" = Option<String>, Query, description = "ok, failed, trap, fuel_exhausted, memory_limit, timeout, ..."),
        ("trigger" = Option<String>, Query, description = "http, hook, job or test"),
        ("request_id" = Option<String>, Query, description = "Executions of one request"),
        ("errors" = Option<bool>, Query, description = "Only executions that did not end ok"),
        ("min_duration_ms" = Option<i64>, Query, description = "At least this long"),
        ("cursor" = Option<i64>, Query, description = "`next_cursor` of the previous page"),
        ("limit" = Option<u32>, Query, description = "Page size (default 50, at most 500)"),
    ),
    responses((status = 200, description = "Executions", body = InvocationPage)))]
async fn invocations(State(s): State<InsightsService>, Access(ctx): Access, q: Query) -> ApiResult {
    insights(&s, &ctx)?;
    let query = InvocationQuery {
        function: opt(&q, "function"),
        version: int_param(&q, "version")?,
        status: opt(&q, "status"),
        trigger: opt(&q, "trigger"),
        request_id: opt(&q, "request_id"),
        before_id: int_param(&q, "cursor")?,
        only_errors: q.bool("errors")?,
        min_duration_ms: int_param(&q, "min_duration_ms")?,
        limit: q.u32("limit")?.unwrap_or(50),
    };
    ok(s.invocations(query).await.map_err(api)?)
}

/// One execution with its profile.
#[utoipa::path(get, path = "/api/v1/insights/functions/invocations/{id}", operation_id = "insights_invocations_get", tag = "insights",
    params(("id" = i64, Path, description = "Execution id")),
    responses((status = 200, description = "Execution", body = InvocationDetail), (status = 404, description = "No such execution")))]
async fn invocation(State(s): State<InsightsService>, Access(ctx): Access, Path(id): Path<i64>) -> ApiResult {
    insights(&s, &ctx)?;
    ok(s.invocation(id).await.map_err(api)?)
}

// ---------------------------------------------------------------------------
// Audit
// ---------------------------------------------------------------------------

/// An audit entry.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditEntryView {
    /// Row id (increasing; the paging cursor).
    pub id: i64,
    /// When (RFC 3339).
    pub at: String,
    /// The action, such as `login` or `api_key.create`.
    pub event: String,
    /// `success` or `failure`.
    pub outcome: String,
    /// Who acted: `user:<collection>/<id>`, `admin:<id>`, `service_account:<id>`, `anonymous`.
    pub actor: String,
    /// Kind of the resource concerned.
    pub resource_type: Option<String>,
    /// Collection of the resource concerned.
    pub collection: Option<String>,
    /// Id of the resource concerned.
    pub resource_id: Option<String>,
    /// Client address.
    pub ip: Option<String>,
    /// User agent.
    pub user_agent: Option<String>,
    /// Request id.
    pub request_id: Option<String>,
    /// Administrator acting as the actor (impersonation).
    pub impersonator: Option<String>,
    /// Non-secret details.
    #[schema(value_type = Object)]
    pub detail: Json_,
}

impl From<AuditEntry> for AuditEntryView {
    fn from(e: AuditEntry) -> Self {
        Self {
            id: e.id,
            at: e.at,
            event: e.event,
            outcome: e.outcome,
            actor: e.actor,
            resource_type: e.subject_type,
            collection: e.collection,
            resource_id: e.subject_id,
            ip: e.ip,
            user_agent: e.user_agent,
            request_id: e.request_id,
            impersonator: e.impersonator,
            detail: e.detail,
        }
    }
}

/// A page of audit entries.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditPageView {
    /// The entries.
    pub items: Vec<AuditEntryView>,
    /// Pass as `cursor` for the next page; absent after the last one.
    pub next_cursor: Option<i64>,
}

/// An entry and the entries around it.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditRelatedView {
    /// The entry.
    pub event: AuditEntryView,
    /// Other entries of the same request, oldest first.
    pub same_request: Vec<AuditEntryView>,
    /// Other entries of the same actor within the window, oldest first.
    pub same_actor: Vec<AuditEntryView>,
}

/// A count of something.
#[derive(Debug, Serialize, ToSchema)]
pub struct Count {
    /// The value counted (an event, actor, address or outcome).
    pub key: String,
    /// Entries.
    pub count: i64,
}

/// Counts of an audit time range.
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditSummaryView {
    /// Entries in the range.
    pub total: i64,
    /// Per outcome.
    pub outcomes: Vec<Count>,
    /// The most frequent events.
    pub events: Vec<Count>,
    /// The most active actors.
    pub actors: Vec<Count>,
    /// The most frequent client addresses.
    pub ips: Vec<Count>,
}

fn search_of(q: &Query) -> Result<AuditSearch, ApiError> {
    let secs = |name: &str| -> Result<Option<i64>, ApiError> { Ok(time_param(q, name)?.map(|ms| ms.div_euclid(1000))) };
    Ok(AuditSearch {
        actor: opt(q, "actor"),
        actor_type: opt(q, "actor_type"),
        action: opt(q, "action"),
        resource_type: opt(q, "resource_type"),
        collection: opt(q, "collection"),
        resource_id: opt(q, "resource_id"),
        outcome: opt(q, "outcome"),
        ip: opt(q, "ip"),
        request_id: opt(q, "request_id"),
        from: secs("from")?,
        to: secs("to")?,
        text: opt(q, "q"),
        ascending: match q.get("order") {
            None | Some("" | "desc") => false,
            Some("asc") => true,
            Some(_) => return Err(ApiError::bad_request("invalid_parameter", "order must be asc or desc")),
        },
        cursor: int_param(q, "cursor")?,
        limit: q.u32("limit")?,
    })
}

const AUDIT_FILTERS: &str = "Filters: actor, actor_type (user, admin, service_account, api_key, anonymous), action (prefix), resource_type, collection, resource_id, outcome (success|failure), ip, request_id, from, to, q (text in the details), order (asc|desc)";

/// Searches the audit trail. Newest first by default; page with `cursor`.
#[utoipa::path(get, path = "/api/v1/insights/audit", operation_id = "insights_audit_search", tag = "insights",
    description = AUDIT_FILTERS,
    params(
        ("actor" = Option<String>, Query, description = "Exactly this actor"),
        ("actor_type" = Option<String>, Query, description = "user, admin, service_account, api_key or anonymous"),
        ("action" = Option<String>, Query, description = "Events starting with this (login, api_key.)"),
        ("resource_type" = Option<String>, Query, description = "Kind of the resource concerned"),
        ("collection" = Option<String>, Query, description = "Collection of the resource concerned"),
        ("resource_id" = Option<String>, Query, description = "Id of the resource concerned"),
        ("outcome" = Option<String>, Query, description = "success or failure"),
        ("ip" = Option<String>, Query, description = "Client address"),
        ("request_id" = Option<String>, Query, description = "Request id"),
        ("from" = Option<String>, Query, description = "At or after (RFC 3339 or YYYY-MM-DD)"),
        ("to" = Option<String>, Query, description = "Before"),
        ("q" = Option<String>, Query, description = "Text in the details, event, resource id or user agent"),
        ("order" = Option<String>, Query, description = "desc (default) or asc"),
        ("cursor" = Option<i64>, Query, description = "`next_cursor` of the previous page"),
        ("limit" = Option<u32>, Query, description = "Page size (default 50, at most 1000)"),
    ),
    responses((status = 200, description = "Entries", body = AuditPageView)))]
async fn audit_search(State(s): State<InsightsService>, Access(ctx): Access, q: Query) -> ApiResult {
    let realm = audit_realm(&s, &ctx)?;
    let page = s
        .inner
        .deps
        .auth
        .audit_search(realm.as_deref(), &search_of(&q)?)
        .await
        .map_err(|e| api(e.into()))?;
    ok(AuditPageView {
        items: page.items.into_iter().map(Into::into).collect(),
        next_cursor: page.next_cursor,
    })
}

/// Counts of the audit trail in a range: per outcome, and the top events,
/// actors and client addresses.
#[utoipa::path(get, path = "/api/v1/insights/audit/summary", operation_id = "insights_audit_summary", tag = "insights",
    params(
        ("from" = Option<String>, Query, description = "At or after"),
        ("to" = Option<String>, Query, description = "Before"),
    ),
    responses((status = 200, description = "Counts", body = AuditSummaryView)))]
async fn audit_summary(State(s): State<InsightsService>, Access(ctx): Access, q: Query) -> ApiResult {
    let realm = audit_realm(&s, &ctx)?;
    let secs =
        |name: &str| -> Result<Option<i64>, ApiError> { Ok(time_param(&q, name)?.map(|ms| ms.div_euclid(1000))) };
    let sum = s
        .inner
        .deps
        .auth
        .audit_summary(realm.as_deref(), secs("from")?, secs("to")?)
        .await
        .map_err(|e| api(e.into()))?;
    let c = |v: Vec<(String, i64)>| v.into_iter().map(|(key, count)| Count { key, count }).collect();
    ok(AuditSummaryView {
        total: sum.total,
        outcomes: c(sum.outcomes),
        events: c(sum.events),
        actors: c(sum.actors),
        ips: c(sum.ips),
    })
}

/// One audit entry.
#[utoipa::path(get, path = "/api/v1/insights/audit/{id}", operation_id = "insights_audit_get", tag = "insights",
    params(("id" = i64, Path, description = "Entry id")),
    responses((status = 200, description = "Entry", body = AuditEntryView), (status = 404, description = "No such entry")))]
async fn audit_entry(State(s): State<InsightsService>, Access(ctx): Access, Path(id): Path<i64>) -> ApiResult {
    let realm = audit_realm(&s, &ctx)?;
    let e = s
        .inner
        .deps
        .auth
        .audit_entry_by_id(realm.as_deref(), id)
        .await
        .map_err(|e| api(e.into()))?
        .ok_or_else(|| api(InsightsError::NotFound(format!("audit entry {id}"))))?;
    ok(AuditEntryView::from(e))
}

/// The entries of the same request, and of the same actor within a time
/// window (default 5 minutes either side, at most one hour).
#[utoipa::path(get, path = "/api/v1/insights/audit/{id}/related", operation_id = "insights_audit_related", tag = "insights",
    params(
        ("id" = i64, Path, description = "Entry id"),
        ("window_secs" = Option<u32>, Query, description = "Seconds either side (default 300, at most 3600)"),
        ("limit" = Option<u32>, Query, description = "At most this many of each (default 100, at most 500)"),
    ),
    responses((status = 200, description = "Entries", body = AuditRelatedView), (status = 404, description = "No such entry")))]
async fn audit_related(
    State(s): State<InsightsService>,
    Access(ctx): Access,
    Path(id): Path<i64>,
    q: Query,
) -> ApiResult {
    let realm = audit_realm(&s, &ctx)?;
    let window = i64::from(q.u32("window_secs")?.unwrap_or(300).min(3600));
    let limit = q.u32("limit")?.unwrap_or(100).clamp(1, 500);
    let r = s
        .inner
        .deps
        .auth
        .audit_related(realm.as_deref(), id, window, limit)
        .await
        .map_err(|e| api(e.into()))?
        .ok_or_else(|| api(InsightsError::NotFound(format!("audit entry {id}"))))?;
    ok(AuditRelatedView {
        event: r.event.into(),
        same_request: r.same_request.into_iter().map(Into::into).collect(),
        same_actor: r.same_actor.into_iter().map(Into::into).collect(),
    })
}

const CSV_HEADER: &str =
    "id,at,event,outcome,actor,resource_type,collection,resource_id,ip,user_agent,request_id,impersonator,detail\r\n";
/// Most rows one export streams unless asked for more.
const EXPORT_DEFAULT_ROWS: usize = 100_000;
/// Hard cap of one export.
const EXPORT_MAX_ROWS: usize = 1_000_000;

/// A CSV cell: quoted when needed, and a leading `=`, `+`, `-`, `@`, tab or
/// carriage return is neutralised so a spreadsheet does not run a value an
/// attacker put in a user agent or a detail as a formula.
fn csv_cell(s: &str, out: &mut String) {
    let danger = s.starts_with(['=', '+', '-', '@', '\t', '\r']);
    if !danger && !s.contains(['"', ',', '\n', '\r']) {
        out.push_str(s);
        return;
    }
    out.push('"');
    if danger {
        out.push('\'');
    }
    for c in s.chars() {
        if c == '"' {
            out.push('"');
        }
        out.push(c);
    }
    out.push('"');
}

fn csv_row(e: &AuditEntry, out: &mut String) {
    let id = e.id.to_string();
    let detail = if e.detail.is_null() {
        String::new()
    } else {
        e.detail.to_string()
    };
    let cells: [&str; 13] = [
        &id,
        &e.at,
        &e.event,
        &e.outcome,
        &e.actor,
        e.subject_type.as_deref().unwrap_or(""),
        e.collection.as_deref().unwrap_or(""),
        e.subject_id.as_deref().unwrap_or(""),
        e.ip.as_deref().unwrap_or(""),
        e.user_agent.as_deref().unwrap_or(""),
        e.request_id.as_deref().unwrap_or(""),
        e.impersonator.as_deref().unwrap_or(""),
        &detail,
    ];
    for (i, c) in cells.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        csv_cell(c, out);
    }
    out.push_str("\r\n");
}

struct Export {
    svc: InsightsService,
    realm: Option<String>,
    search: AuditSearch,
    csv: bool,
    sent: usize,
    max: usize,
    started: bool,
    done: bool,
}

/// Streams the entries matching the filters of `/insights/audit` as CSV
/// (`format=csv`, the default) or JSON lines (`format=jsonl`), oldest first
/// unless `order=desc`, in pages of 1000, so a large export never sits in
/// memory. At most `max_rows` (default 100 000, at most 1 000 000). In CSV,
/// cells that start like a formula are prefixed with `'`.
#[utoipa::path(get, path = "/api/v1/insights/audit/export", operation_id = "insights_audit_export", tag = "insights",
    description = AUDIT_FILTERS,
    params(
        ("format" = Option<String>, Query, description = "csv (default) or jsonl"),
        ("max_rows" = Option<u32>, Query, description = "At most this many entries (default 100000, at most 1000000)"),
    ),
    responses((status = 200, description = "The entries as a download", content_type = "text/csv")))]
async fn audit_export(State(s): State<InsightsService>, Access(ctx): Access, q: Query) -> ApiResult {
    let realm = audit_realm(&s, &ctx)?;
    let csv = match q.get("format") {
        None | Some("" | "csv") => true,
        Some("jsonl" | "ndjson") => false,
        Some(_) => {
            return Err(ApiError::bad_request(
                "invalid_parameter",
                "format must be csv or jsonl",
            ));
        }
    };
    let mut search = search_of(&q)?;
    if q.get("order").is_none() {
        search.ascending = true;
    }
    let max = (q.u32("max_rows")?.map_or(EXPORT_DEFAULT_ROWS, |n| n as usize)).clamp(1, EXPORT_MAX_ROWS);
    let state = Export {
        svc: s,
        realm,
        search,
        csv,
        sent: 0,
        max,
        started: false,
        done: false,
    };
    let stream = futures_util::stream::unfold(state, |mut st| async move {
        if st.done {
            return None;
        }
        let mut out = String::new();
        if !st.started {
            st.started = true;
            if st.csv {
                out.push_str(CSV_HEADER);
            }
        }
        st.search.limit =
            Some(u32::try_from((st.max - st.sent).min(AUDIT_PAGE_MAX as usize)).unwrap_or(AUDIT_PAGE_MAX));
        match st
            .svc
            .inner
            .deps
            .auth
            .audit_search(st.realm.as_deref(), &st.search)
            .await
        {
            Ok(page) => {
                for e in &page.items {
                    if st.csv {
                        csv_row(e, &mut out);
                    } else {
                        out.push_str(&serde_json::to_string(&AuditEntryView::from(e.clone())).unwrap_or_default());
                        out.push('\n');
                    }
                }
                st.sent += page.items.len();
                st.search.cursor = page.next_cursor;
                if page.next_cursor.is_none() || st.sent >= st.max {
                    st.done = true;
                }
                Some((Ok::<_, std::io::Error>(Bytes::from(out)), st))
            }
            Err(e) => {
                tracing::warn!(error = %e, code = e.code(), "audit export stopped");
                st.done = true;
                Some((Err(std::io::Error::other("the audit export failed")), st))
            }
        }
    });
    let (mime, ext) = if csv {
        ("text/csv; charset=utf-8", "csv")
    } else {
        ("application/x-ndjson", "jsonl")
    };
    let mut res = Response::new(Body::from_stream(stream));
    *res.status_mut() = StatusCode::OK;
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "attachment; filename=\"audit-{}.{ext}\"",
            crate::util::day_of(crate::util::now_ms())
        ))
        .unwrap_or_else(|_| HeaderValue::from_static("attachment")),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_cells_are_quoted_and_formulas_neutralised() {
        let mut s = String::new();
        csv_cell("plain", &mut s);
        assert_eq!(s, "plain");
        let (mut a, mut b, mut c) = (String::new(), String::new(), String::new());
        csv_cell("a,\"b\"", &mut a);
        csv_cell("=HYPERLINK(\"http://x\")", &mut b);
        csv_cell("-2+3", &mut c);
        assert_eq!(a, "\"a,\"\"b\"\"\"");
        assert_eq!(b, "\"'=HYPERLINK(\"\"http://x\"\")\"");
        assert_eq!(c, "\"'-2+3\"");
    }
}
