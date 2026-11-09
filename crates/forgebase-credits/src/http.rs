//! REST under `/api/v1/credits` (mounted as an `ApiExtension`, tag
//! `credits`): balances, lots, the ledger, top-ups, grants, spends, refunds,
//! reversals, prices, usage charges, statistics and maintenance.
//!
//! Handlers only translate HTTP; the service authorises every call. A debit
//! the balance does not cover is `402 insufficient_credits` with the balance
//! in `details`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use forgebase_api::http::{ApiJson, Query};
use forgebase_api::principal::Access;
use forgebase_api::{ApiError, ApiExtension};
use forgebase_entitlements::model::parse_iso;
use forgebase_policy::PolicyError;
use serde::Serialize;
use serde_json::json;
use tower_http::limit::RequestBodyLimitLayer;
use utoipa::{OpenApi, ToSchema};

use crate::error::CreditError;
use crate::model::{
    AmendInput, AuditReport, Balance, BalanceHistory, BalancePoint, ConsumptionRow, ConsumptionStats, CreditInput,
    CreditSummary, Cursor, EntryKind, Expiring, ExpiryReport, Interval, LedgerEntry, LedgerPage, Lot,
    MaintenanceReport, Posted, Rate, RateBasis, RateInput, Reversal, SpendInput, UsageCharge, UsageChargePage,
    UsageRun,
};
use crate::service::{CreditService, LedgerQuery};
use crate::stats::Window;

type ApiResult = Result<Response, ApiError>;
type Body<T> = Result<ApiJson<T>, ApiError>;

const MAX_BODY: usize = 64 * 1024;

/// The HTTP form of a service error.
pub fn api(e: CreditError) -> ApiError {
    match &e {
        CreditError::InsufficientCredits { balance, requested, .. } => {
            ApiError::new(StatusCode::PAYMENT_REQUIRED, "insufficient_credits", e.to_string())
                .with_details(json!({"balance": balance, "requested": requested}))
        }
        CreditError::Policy(PolicyError::Denied(d)) => ApiError::from_error(&e).with_details(json!({"step": d.step})),
        _ => ApiError::from_error(&e),
    }
}

fn ok<T: Serialize>(v: T) -> ApiResult {
    Ok(Json(v).into_response())
}

fn posted(p: Posted) -> ApiResult {
    let status = if p.replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(p)).into_response())
}

/// A list.
#[derive(Serialize, ToSchema)]
pub struct Items<T> {
    /// Entries.
    pub items: Vec<T>,
}

fn items<T: Serialize>(v: Vec<T>) -> ApiResult {
    ok(Items { items: v })
}

/// Authenticates before surfacing body errors.
fn body<T>(s: &CreditService, ctx: &forgebase_data::AccessContext, b: Body<T>) -> Result<T, ApiError> {
    s.require_caller(ctx).map_err(api)?;
    Ok(b?.0)
}

fn time_param(q: &Query, name: &str) -> Result<Option<i64>, ApiError> {
    match q.get(name) {
        None | Some("") => Ok(None),
        Some(v) => parse_iso(v)
            .map(Some)
            .ok_or_else(|| ApiError::bad_request("invalid_parameter", format!("{name} must be an RFC 3339 time"))),
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

fn window(s: &CreditService, q: &Query) -> Result<Window, ApiError> {
    let d = Window::last_30_days(s.now());
    let interval = match q.get("interval") {
        None | Some("") => d.interval,
        Some(v) => Interval::parse(v)
            .ok_or_else(|| ApiError::bad_request("invalid_parameter", "interval must be hour, day or month"))?,
    };
    let to = time_param(q, "to")?.unwrap_or(d.to);
    let from = time_param(q, "from")?.unwrap_or(to - (d.to - d.from));
    Ok(Window { from, to, interval })
}

/// The routes, relative to `/api/v1`, with their state applied.
pub fn router<S: Clone + Send + Sync + 'static>(svc: CreditService) -> Router<S> {
    Router::<CreditService>::new()
        .route("/credits/balance", get(balance))
        .route("/credits/lots", get(lots))
        .route("/credits/ledger", get(ledger))
        .route("/credits/ledger/{id}", get(entry))
        .route("/credits/audit", get(audit))
        .route("/credits/top-ups", post(top_up))
        .route("/credits/grants", post(grant))
        .route("/credits/spend", post(spend))
        .route("/credits/refunds", post(refund))
        .route("/credits/reversals", post(reverse))
        .route("/credits/rates", get(rates))
        .route("/credits/rates/{feature}", put(put_rate).delete(delete_rate))
        .route("/credits/usage", get(usage_charges))
        .route("/credits/usage/cursor", get(usage_cursor).put(set_usage_cursor))
        .route("/credits/stats/balance", get(balance_history))
        .route("/credits/stats/consumption", get(consumption))
        .route("/credits/stats/summary", get(summary))
        .route("/credits/maintenance", post(maintenance))
        .layer(RequestBodyLimitLayer::new(MAX_BODY))
        .with_state(svc)
}

/// The routes and OpenAPI fragment, ready for `RouterOptions::extensions`.
pub fn extension(svc: CreditService) -> ApiExtension {
    ApiExtension {
        routes: router(svc),
        openapi: openapi(),
    }
}

/// OpenAPI paths and schemas.
pub fn openapi() -> utoipa::openapi::OpenApi {
    CreditsDoc::openapi()
}

#[derive(OpenApi)]
#[openapi(
    paths(
        balance, lots, ledger, entry, audit, top_up, grant, spend, refund, reverse, rates, put_rate, delete_rate,
        usage_charges, usage_cursor, set_usage_cursor, balance_history, consumption, summary, maintenance,
    ),
    components(schemas(
        Balance, Expiring, Lot, EntryKind, LedgerEntry, LedgerPage, Posted, CreditInput, SpendInput, AmendInput,
        Reversal, Rate, RateInput, RateBasis, UsageCharge, UsageChargePage, Cursor, AuditReport, Interval,
        BalanceHistory, BalancePoint, ConsumptionStats, ConsumptionRow, CreditSummary, MaintenanceReport,
        ExpiryReport, UsageRun, Items<Lot>, Items<Rate>,
    )),
    tags((name = "credits", description = "Prepaid credit balances and their append-only ledger per user and organization: top-ups, grants with expiry, consumption from usage events, refunds, reversals and statistics")),
)]
struct CreditsDoc;

const SUBJECT: &str = "org:<id> or a principal key (default: the caller's subject)";

/// Balance of a subject and its next expiries (the subject itself, or `billing:read`).
#[utoipa::path(get, path = "/api/v1/credits/balance", tag = "credits",
    params(("subject" = Option<String>, Query, description = SUBJECT)),
    responses((status = 200, description = "Balance", body = Balance)))]
async fn balance(State(s): State<CreditService>, Access(ctx): Access, q: Query) -> ApiResult {
    ok(s.balance(&ctx, q.get("subject")).await.map_err(api)?)
}

/// Lots of a subject: what is left of each top-up and grant.
#[utoipa::path(get, path = "/api/v1/credits/lots", tag = "credits",
    params(
        ("subject" = Option<String>, Query, description = SUBJECT),
        ("all" = Option<bool>, Query, description = "Include empty and lapsed lots"),
    ),
    responses((status = 200, description = "Lots, oldest first", body = Items<Lot>)))]
async fn lots(State(s): State<CreditService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.lots(&ctx, q.get("subject"), q.bool("all")?).await.map_err(api)?)
}

/// Ledger entries in `seq` order.
#[utoipa::path(get, path = "/api/v1/credits/ledger", tag = "credits",
    params(
        ("subject" = Option<String>, Query, description = SUBJECT),
        ("all" = Option<bool>, Query, description = "Platform administrators: every subject"),
        ("kind" = Option<EntryKind>, Query, description = "Only this kind"),
        ("after" = Option<i64>, Query, description = "Entries after this seq"),
        ("from" = Option<String>, Query, description = "At or after (RFC 3339)"),
        ("to" = Option<String>, Query, description = "Before (RFC 3339)"),
        ("limit" = Option<u32>, Query, description = "Page size (default 100, at most 1000)"),
    ),
    responses((status = 200, description = "Entries", body = LedgerPage)))]
async fn ledger(State(s): State<CreditService>, Access(ctx): Access, q: Query) -> ApiResult {
    let kind = match q.get("kind") {
        None | Some("") => None,
        Some(k) => Some(EntryKind::parse(k).ok_or_else(|| ApiError::bad_request("invalid_parameter", "unknown kind"))?),
    };
    let query = LedgerQuery {
        subject: q.get("subject").map(str::to_owned),
        all: q.bool("all")?,
        kind,
        after: int_param(&q, "after")?,
        from: time_param(&q, "from")?,
        to: time_param(&q, "to")?,
        limit: q.u32("limit")?,
    };
    ok(s.ledger(&ctx, &query).await.map_err(api)?)
}

/// One ledger entry.
#[utoipa::path(get, path = "/api/v1/credits/ledger/{id}", tag = "credits",
    params(("id" = String, Path, description = "Entry id")),
    responses((status = 200, description = "Entry", body = LedgerEntry), (status = 404, description = "No such entry")))]
async fn entry(State(s): State<CreditService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.entry(&ctx, &id).await.map_err(api)?)
}

/// Checks that the balance, the ledger, the lots and their allocations agree.
#[utoipa::path(get, path = "/api/v1/credits/audit", tag = "credits",
    params(("subject" = Option<String>, Query, description = SUBJECT)),
    responses((status = 200, description = "Consistency report", body = AuditReport)))]
async fn audit(State(s): State<CreditService>, Access(ctx): Access, q: Query) -> ApiResult {
    let subject = s.subject_for(&ctx, q.get("subject")).map_err(api)?;
    ok(s.audit(&ctx, &subject).await.map_err(api)?)
}

/// Adds purchased credits (operators with realm-level `billing:write`;
/// FB-70 through `CreditSink`). Idempotent by key: a replay is `200`.
#[utoipa::path(post, path = "/api/v1/credits/top-ups", tag = "credits", request_body = CreditInput,
    responses((status = 201, description = "Credited", body = Posted), (status = 200, description = "Replayed", body = Posted)))]
async fn top_up(State(s): State<CreditService>, Access(ctx): Access, b: Body<CreditInput>) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    posted(s.top_up(&ctx, input).await.map_err(api)?)
}

/// Adds free credits, optionally lapsing at `expires_at` (operators).
#[utoipa::path(post, path = "/api/v1/credits/grants", tag = "credits", request_body = CreditInput,
    responses((status = 201, description = "Granted", body = Posted), (status = 200, description = "Replayed", body = Posted)))]
async fn grant(State(s): State<CreditService>, Access(ctx): Access, b: Body<CreditInput>) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    posted(s.grant(&ctx, input).await.map_err(api)?)
}

/// Debits credits at once, all or nothing (`billing:write` or `usage:write`).
#[utoipa::path(post, path = "/api/v1/credits/spend", tag = "credits", request_body = SpendInput,
    responses(
        (status = 201, description = "Debited", body = Posted),
        (status = 200, description = "Replayed", body = Posted),
        (status = 402, description = "The balance does not cover it"),
    ))]
async fn spend(State(s): State<CreditService>, Access(ctx): Access, b: Body<SpendInput>) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    posted(s.spend(&ctx, input).await.map_err(api)?)
}

/// Gives back all or part of a consumption or spend.
#[utoipa::path(post, path = "/api/v1/credits/refunds", tag = "credits", request_body = AmendInput,
    responses((status = 201, description = "Refunded", body = Posted), (status = 200, description = "Replayed", body = Posted)))]
async fn refund(State(s): State<CreditService>, Access(ctx): Access, b: Body<AmendInput>) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    posted(s.refund(&ctx, input).await.map_err(api)?)
}

/// Takes back what is left of a top-up or grant (operators).
#[utoipa::path(post, path = "/api/v1/credits/reversals", tag = "credits", request_body = AmendInput,
    responses((status = 200, description = "Reversed (or nothing left to take back)", body = Reversal)))]
async fn reverse(State(s): State<CreditService>, Access(ctx): Access, b: Body<AmendInput>) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.reverse(&ctx, input).await.map_err(api)?)
}

/// Prices of metered features in credits.
#[utoipa::path(get, path = "/api/v1/credits/rates", tag = "credits",
    responses((status = 200, description = "Prices", body = Items<Rate>)))]
async fn rates(State(s): State<CreditService>, Access(ctx): Access) -> ApiResult {
    items(s.rates(&ctx).await.map_err(api)?)
}

/// Sets the price of a feature (platform administrators).
#[utoipa::path(put, path = "/api/v1/credits/rates/{feature}", tag = "credits", request_body = RateInput,
    params(("feature" = String, Path, description = "Feature key")),
    responses((status = 200, description = "Price", body = Rate)))]
async fn put_rate(
    State(s): State<CreditService>,
    Access(ctx): Access,
    Path(feature): Path<String>,
    b: Body<RateInput>,
) -> ApiResult {
    s.require_admin(&ctx).map_err(api)?;
    let input = b?.0;
    ok(s.put_rate(&ctx, &feature, input).await.map_err(api)?)
}

/// Removes the price of a feature (platform administrators).
#[utoipa::path(delete, path = "/api/v1/credits/rates/{feature}", tag = "credits",
    params(("feature" = String, Path, description = "Feature key")),
    responses((status = 204, description = "Removed")))]
async fn delete_rate(State(s): State<CreditService>, Access(ctx): Access, Path(feature): Path<String>) -> ApiResult {
    s.delete_rate(&ctx, &feature).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Usage events charged in credits (with the shortfall the balance did not cover).
#[utoipa::path(get, path = "/api/v1/credits/usage", tag = "credits",
    params(
        ("subject" = Option<String>, Query, description = SUBJECT),
        ("all" = Option<bool>, Query, description = "Platform administrators: every subject"),
        ("shortfall" = Option<bool>, Query, description = "Only charges with a shortfall"),
        ("after" = Option<i64>, Query, description = "Charges after this usage event seq"),
        ("limit" = Option<u32>, Query, description = "Page size (default 100, at most 1000)"),
    ),
    responses((status = 200, description = "Charges", body = UsageChargePage)))]
async fn usage_charges(State(s): State<CreditService>, Access(ctx): Access, q: Query) -> ApiResult {
    ok(s.usage_charges(
        &ctx,
        q.get("subject"),
        q.bool("all")?,
        int_param(&q, "after")?,
        q.u32("limit")?,
        q.bool("shortfall")?,
    )
    .await
    .map_err(api)?)
}

/// Position of the usage-event consumer (platform administrators).
#[utoipa::path(get, path = "/api/v1/credits/usage/cursor", tag = "credits",
    responses((status = 200, description = "Cursor", body = Cursor)))]
async fn usage_cursor(State(s): State<CreditService>, Access(ctx): Access) -> ApiResult {
    ok(s.usage_cursor(&ctx).await.map_err(api)?)
}

/// Moves the usage-event consumer (platform administrators); events already
/// charged are never charged again.
#[utoipa::path(put, path = "/api/v1/credits/usage/cursor", tag = "credits", request_body = Cursor,
    responses((status = 200, description = "Cursor", body = Cursor)))]
async fn set_usage_cursor(State(s): State<CreditService>, Access(ctx): Access, b: Body<Cursor>) -> ApiResult {
    s.require_admin(&ctx).map_err(api)?;
    let c = b?.0;
    ok(s.set_usage_cursor(&ctx, c).await.map_err(api)?)
}

const WINDOW_PARAMS: &str = "RFC 3339; default: the last 30 days";

/// Balance history: opening balance, then per bucket credits added, removed and the closing balance.
#[utoipa::path(get, path = "/api/v1/credits/stats/balance", tag = "credits",
    params(
        ("subject" = Option<String>, Query, description = SUBJECT),
        ("from" = Option<String>, Query, description = WINDOW_PARAMS),
        ("to" = Option<String>, Query, description = WINDOW_PARAMS),
        ("interval" = Option<Interval>, Query, description = "hour, day (default) or month"),
    ),
    responses((status = 200, description = "History", body = BalanceHistory)))]
async fn balance_history(State(s): State<CreditService>, Access(ctx): Access, q: Query) -> ApiResult {
    let w = window(&s, &q)?;
    ok(s.balance_history(&ctx, q.get("subject"), w).await.map_err(api)?)
}

/// Credits consumed by feature and period (net of refunds).
#[utoipa::path(get, path = "/api/v1/credits/stats/consumption", tag = "credits",
    params(
        ("subject" = Option<String>, Query, description = SUBJECT),
        ("all" = Option<bool>, Query, description = "Platform administrators: the whole instance"),
        ("from" = Option<String>, Query, description = WINDOW_PARAMS),
        ("to" = Option<String>, Query, description = WINDOW_PARAMS),
        ("interval" = Option<Interval>, Query, description = "hour, day (default) or month"),
    ),
    responses((status = 200, description = "Consumption", body = ConsumptionStats)))]
async fn consumption(State(s): State<CreditService>, Access(ctx): Access, q: Query) -> ApiResult {
    let w = window(&s, &q)?;
    ok(s.consumption(&ctx, q.get("subject"), q.bool("all")?, w)
        .await
        .map_err(api)?)
}

/// Totals by entry kind, the balance and the usage shortfall.
#[utoipa::path(get, path = "/api/v1/credits/stats/summary", tag = "credits",
    params(
        ("subject" = Option<String>, Query, description = SUBJECT),
        ("all" = Option<bool>, Query, description = "Platform administrators: the whole instance"),
        ("from" = Option<String>, Query, description = WINDOW_PARAMS),
        ("to" = Option<String>, Query, description = WINDOW_PARAMS),
    ),
    responses((status = 200, description = "Totals", body = CreditSummary)))]
async fn summary(State(s): State<CreditService>, Access(ctx): Access, q: Query) -> ApiResult {
    let w = window(&s, &q)?;
    ok(s.summary(&ctx, q.get("subject"), q.bool("all")?, w.from, w.to)
        .await
        .map_err(api)?)
}

/// Runs expiry and usage consumption now (platform administrators; the
/// `credits.maintenance` job does it every minute).
#[utoipa::path(post, path = "/api/v1/credits/maintenance", tag = "credits",
    responses((status = 200, description = "What changed", body = MaintenanceReport)))]
async fn maintenance(State(s): State<CreditService>, Access(ctx): Access) -> ApiResult {
    s.require_admin(&ctx).map_err(api)?;
    ok(s.run_maintenance().await.map_err(api)?)
}
