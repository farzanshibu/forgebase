//! `/api/v1/alerts` and `/api/v1/slos` (ADR-0035 §6). Platform
//! administrators only: the check runs before a body is read.

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use forgebase_api::http::{ApiJson, Query};
use forgebase_api::principal::Access;
use forgebase_api::{ApiError, ApiExtension};
use forgebase_core::ErrorCode;
use forgebase_data::AccessContext;
use serde::Serialize;
use utoipa::OpenApi;

use crate::error::AlertError;
use crate::model::*;
use crate::service::AlertService;

type ApiResult = Result<Response, ApiError>;

fn api(e: AlertError) -> ApiError {
    match &e {
        AlertError::RateLimited(m) => ApiError::new(StatusCode::TOO_MANY_REQUESTS, e.code(), m.clone()),
        _ => ApiError::from_error(&e),
    }
}

fn ok<T: Serialize>(v: T) -> ApiResult {
    Ok(Json(v).into_response())
}

fn created<T: Serialize>(v: T) -> ApiResult {
    Ok((StatusCode::CREATED, Json(v)).into_response())
}

fn time_param(q: &Query, name: &str) -> Result<Option<i64>, ApiError> {
    match q.get(name) {
        None => Ok(None),
        Some(v) => parse_iso(v)
            .map(Some)
            .ok_or_else(|| ApiError::bad_request("invalid_query", format!("{name} must be an RFC 3339 time"))),
    }
}

/// Refuses everyone but platform administrators before the handler runs.
async fn admin_only(State(s): State<AlertService>, req: Request, next: Next) -> Response {
    let ctx = req
        .extensions()
        .get::<AccessContext>()
        .cloned()
        .unwrap_or_else(AccessContext::anonymous);
    match s.require_admin(&ctx) {
        Ok(()) => next.run(req).await,
        Err(e) => api(e).into_response(),
    }
}

/// The routes, relative to `/api/v1`.
pub fn router<S: Clone + Send + Sync + 'static>(svc: AlertService) -> Router<S> {
    Router::<AlertService>::new()
        .route("/alerts", get(list_active))
        .route("/alerts/history", get(history))
        .route("/alerts/query", post(query))
        .route("/alerts/rules", get(list_rules).post(create_rule))
        .route("/alerts/rules/{id}", get(get_rule).patch(update_rule).delete(delete_rule))
        .route("/alerts/silences", get(list_silences).post(create_silence))
        .route(
            "/alerts/silences/{id}",
            get(get_silence).put(update_silence).delete(expire_silence),
        )
        .route("/alerts/notifiers", get(list_notifiers).post(create_notifier))
        .route(
            "/alerts/notifiers/{id}",
            get(get_notifier).put(update_notifier).delete(delete_notifier),
        )
        .route("/alerts/notifiers/{id}/test", post(test_notifier))
        .route("/alerts/notifications", get(list_deliveries))
        .route("/alerts/{id}", get(get_alert))
        .route("/slos", get(list_slos).post(create_slo))
        .route("/slos/status", get(list_slo_status))
        .route("/slos/{id}", get(get_slo).put(update_slo).delete(delete_slo))
        .route("/slos/{id}/status", get(slo_status))
        .route_layer(middleware::from_fn_with_state(svc.clone(), admin_only))
        .with_state(svc)
}

/// Routes and OpenAPI document for the server.
pub fn extension(svc: AlertService) -> ApiExtension {
    ApiExtension {
        routes: router(svc),
        openapi: openapi(),
    }
}

/// OpenAPI paths and schemas.
pub fn openapi() -> utoipa::openapi::OpenApi {
    AlertsDoc::openapi()
}

#[derive(OpenApi)]
#[openapi(
    paths(
        list_active, history, query, list_rules, create_rule, get_rule, update_rule, delete_rule, list_silences,
        create_silence, get_silence, update_silence, expire_silence, list_notifiers, create_notifier, get_notifier,
        update_notifier, delete_notifier, test_notifier, list_deliveries, get_alert, list_slos, create_slo,
        list_slo_status, get_slo, update_slo, delete_slo, slo_status,
    ),
    components(schemas(
        Severity, RuleSource, Rule, RuleInput, RulePatch, QueryInput, QuerySample, QueryResult, AlertState, Alert,
        AlertEvent, AlertDetail, AlertPage, LabelMatcher, Silence, SilenceInput, NotifierKind, Notifier,
        NotifierInput, Delivery, DeliveryPage, SloKind, Slo, SloInput, ErrorBudget, BurnTier, SloStatus,
        crate::expr::CmpOp,
    )),
    tags(
        (name = "alerts", description = "Alert rules, active alerts and history, silences and notifiers (administrators)"),
        (name = "slos", description = "Service level objectives, error budgets and burn rates (administrators)"),
    )
)]
struct AlertsDoc;

/// Open alerts (pending and firing), most severe first.
#[utoipa::path(get, path = "/api/v1/alerts", tag = "alerts", operation_id = "alerts_list_active",
    params(
        ("state" = Option<String>, Query, description = "pending or firing"),
        ("severity" = Option<String>, Query, description = "info, warning or critical"),
    ),
    responses((status = 200, description = "Open alerts", body = [Alert]),
        (status = 401, description = "Not signed in"), (status = 403, description = "Not a platform administrator")))]
async fn list_active(State(s): State<AlertService>, Access(ctx): Access, q: Query) -> ApiResult {
    ok(s.active(&ctx, q.get("state"), q.get("severity")).await.map_err(api)?)
}

/// Every alert, newest first, resolved ones included.
#[utoipa::path(get, path = "/api/v1/alerts/history", tag = "alerts", operation_id = "alerts_history",
    params(
        ("state" = Option<String>, Query, description = "pending, firing or resolved"),
        ("rule_id" = Option<String>, Query, description = "Only this rule's alerts"),
        ("from" = Option<String>, Query, description = "Active at or after (RFC 3339)"),
        ("to" = Option<String>, Query, description = "Active before (RFC 3339)"),
        ("before" = Option<String>, Query, description = "next_before of the previous page"),
        ("limit" = Option<u32>, Query, description = "1-500, default 100"),
    ),
    responses((status = 200, description = "Alerts", body = AlertPage), (status = 403, description = "Not a platform administrator")))]
async fn history(State(s): State<AlertService>, Access(ctx): Access, q: Query) -> ApiResult {
    let page = s
        .history(
            &ctx,
            q.get("state"),
            q.get("rule_id"),
            time_param(&q, "from")?,
            time_param(&q, "to")?,
            q.get("before"),
            q.u32("limit")?,
        )
        .await
        .map_err(api)?;
    ok(page)
}

/// Evaluates an expression now (nothing changes).
#[utoipa::path(post, path = "/api/v1/alerts/query", tag = "alerts", operation_id = "alerts_query",
    request_body = QueryInput,
    responses((status = 200, description = "Samples", body = QueryResult), (status = 400, description = "Invalid expression")))]
async fn query(State(s): State<AlertService>, Access(ctx): Access, ApiJson(input): ApiJson<QueryInput>) -> ApiResult {
    ok(s.query(&ctx, input).await.map_err(api)?)
}

/// Every rule: built-in, from the file, custom and SLO burn-rate rules, with their state.
#[utoipa::path(get, path = "/api/v1/alerts/rules", tag = "alerts", operation_id = "alerts_list_rules",
    responses((status = 200, description = "Rules", body = [Rule]), (status = 403, description = "Not a platform administrator")))]
async fn list_rules(State(s): State<AlertService>, Access(ctx): Access) -> ApiResult {
    ok(s.rules(&ctx).await.map_err(api)?)
}

/// Creates a custom rule.
#[utoipa::path(post, path = "/api/v1/alerts/rules", tag = "alerts", operation_id = "alerts_create_rule",
    request_body = RuleInput,
    responses((status = 201, description = "Created", body = Rule), (status = 400, description = "invalid_alert_rule"),
        (status = 409, description = "alert_rule_exists")))]
async fn create_rule(State(s): State<AlertService>, Access(ctx): Access, ApiJson(input): ApiJson<RuleInput>) -> ApiResult {
    created(s.create_rule(&ctx, input).await.map_err(api)?)
}

/// One rule.
#[utoipa::path(get, path = "/api/v1/alerts/rules/{id}", tag = "alerts", operation_id = "alerts_get_rule",
    params(("id" = String, Path, description = "Rule id")),
    responses((status = 200, description = "Rule", body = Rule), (status = 404, description = "alert_rule_not_found")))]
async fn get_rule(State(s): State<AlertService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.rule(&ctx, &id).await.map_err(api)?)
}

/// Changes a rule (built-in, file and SLO rules: `enabled` only).
#[utoipa::path(patch, path = "/api/v1/alerts/rules/{id}", tag = "alerts", operation_id = "alerts_update_rule",
    params(("id" = String, Path, description = "Rule id")), request_body = RulePatch,
    responses((status = 200, description = "Rule", body = Rule), (status = 404, description = "alert_rule_not_found"),
        (status = 409, description = "rule_read_only or rule_disabled_by_config")))]
async fn update_rule(
    State(s): State<AlertService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    ApiJson(p): ApiJson<RulePatch>,
) -> ApiResult {
    ok(s.patch_rule(&ctx, &id, p).await.map_err(api)?)
}

/// Deletes a custom rule.
#[utoipa::path(delete, path = "/api/v1/alerts/rules/{id}", tag = "alerts", operation_id = "alerts_delete_rule",
    params(("id" = String, Path, description = "Rule id")),
    responses((status = 204, description = "Deleted"), (status = 409, description = "rule_read_only")))]
async fn delete_rule(State(s): State<AlertService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    s.delete_rule(&ctx, &id).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Silences that have not ended (`all=true`: expired ones too).
#[utoipa::path(get, path = "/api/v1/alerts/silences", tag = "alerts", operation_id = "alerts_list_silences",
    params(("all" = Option<bool>, Query, description = "Include expired silences")),
    responses((status = 200, description = "Silences", body = [Silence])))]
async fn list_silences(State(s): State<AlertService>, Access(ctx): Access, q: Query) -> ApiResult {
    ok(s.silences(&ctx, q.get("all") == Some("true")).await.map_err(api)?)
}

/// Creates a silence: matching alerts send no notifications until it ends.
#[utoipa::path(post, path = "/api/v1/alerts/silences", tag = "alerts", operation_id = "alerts_create_silence",
    request_body = SilenceInput,
    responses((status = 201, description = "Created", body = Silence), (status = 400, description = "invalid_silence")))]
async fn create_silence(
    State(s): State<AlertService>,
    Access(ctx): Access,
    ApiJson(input): ApiJson<SilenceInput>,
) -> ApiResult {
    created(s.create_silence(&ctx, input).await.map_err(api)?)
}

/// One silence.
#[utoipa::path(get, path = "/api/v1/alerts/silences/{id}", tag = "alerts", operation_id = "alerts_get_silence",
    params(("id" = String, Path, description = "Silence id")),
    responses((status = 200, description = "Silence", body = Silence), (status = 404, description = "silence_not_found")))]
async fn get_silence(State(s): State<AlertService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.silence(&ctx, &id).await.map_err(api)?)
}

/// Replaces a silence.
#[utoipa::path(put, path = "/api/v1/alerts/silences/{id}", tag = "alerts", operation_id = "alerts_update_silence",
    params(("id" = String, Path, description = "Silence id")), request_body = SilenceInput,
    responses((status = 200, description = "Silence", body = Silence), (status = 404, description = "silence_not_found")))]
async fn update_silence(
    State(s): State<AlertService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    ApiJson(input): ApiJson<SilenceInput>,
) -> ApiResult {
    ok(s.update_silence(&ctx, &id, input).await.map_err(api)?)
}

/// Ends a silence now.
#[utoipa::path(delete, path = "/api/v1/alerts/silences/{id}", tag = "alerts", operation_id = "alerts_expire_silence",
    params(("id" = String, Path, description = "Silence id")),
    responses((status = 200, description = "Expired", body = Silence), (status = 404, description = "silence_not_found")))]
async fn expire_silence(State(s): State<AlertService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.expire_silence(&ctx, &id).await.map_err(api)?)
}

/// Notifiers (secrets never listed).
#[utoipa::path(get, path = "/api/v1/alerts/notifiers", tag = "alerts", operation_id = "alerts_list_notifiers",
    responses((status = 200, description = "Notifiers", body = [Notifier])))]
async fn list_notifiers(State(s): State<AlertService>, Access(ctx): Access) -> ApiResult {
    ok(s.notifiers(&ctx).await.map_err(api)?)
}

/// Creates a notifier. A webhook's signing secret is in this answer only.
#[utoipa::path(post, path = "/api/v1/alerts/notifiers", tag = "alerts", operation_id = "alerts_create_notifier",
    request_body = NotifierInput,
    responses((status = 201, description = "Created", body = Notifier), (status = 400, description = "invalid_notifier"),
        (status = 409, description = "notifier_exists")))]
async fn create_notifier(
    State(s): State<AlertService>,
    Access(ctx): Access,
    ApiJson(input): ApiJson<NotifierInput>,
) -> ApiResult {
    created(s.create_notifier(&ctx, input).await.map_err(api)?)
}

/// One notifier (by id or name).
#[utoipa::path(get, path = "/api/v1/alerts/notifiers/{id}", tag = "alerts", operation_id = "alerts_get_notifier",
    params(("id" = String, Path, description = "Notifier id or name")),
    responses((status = 200, description = "Notifier", body = Notifier), (status = 404, description = "notifier_not_found")))]
async fn get_notifier(State(s): State<AlertService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.notifier(&ctx, &id).await.map_err(api)?)
}

/// Replaces a notifier's settings (secrets kept unless given).
#[utoipa::path(put, path = "/api/v1/alerts/notifiers/{id}", tag = "alerts", operation_id = "alerts_update_notifier",
    params(("id" = String, Path, description = "Notifier id or name")), request_body = NotifierInput,
    responses((status = 200, description = "Notifier", body = Notifier), (status = 404, description = "notifier_not_found")))]
async fn update_notifier(
    State(s): State<AlertService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    ApiJson(input): ApiJson<NotifierInput>,
) -> ApiResult {
    ok(s.update_notifier(&ctx, &id, input).await.map_err(api)?)
}

/// Deletes a notifier and its deliveries.
#[utoipa::path(delete, path = "/api/v1/alerts/notifiers/{id}", tag = "alerts", operation_id = "alerts_delete_notifier",
    params(("id" = String, Path, description = "Notifier id or name")),
    responses((status = 204, description = "Deleted"), (status = 404, description = "notifier_not_found")))]
async fn delete_notifier(State(s): State<AlertService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    s.delete_notifier(&ctx, &id).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Sends a test notification now (10 per notifier and minute).
#[utoipa::path(post, path = "/api/v1/alerts/notifiers/{id}/test", tag = "alerts", operation_id = "alerts_test_notifier",
    params(("id" = String, Path, description = "Notifier id or name")),
    responses((status = 200, description = "The delivery (status sent or failed)", body = Delivery),
        (status = 429, description = "rate_limited")))]
async fn test_notifier(State(s): State<AlertService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.test_notifier(&ctx, &id).await.map_err(api)?)
}

/// Notification deliveries, newest first.
#[utoipa::path(get, path = "/api/v1/alerts/notifications", tag = "alerts", operation_id = "alerts_list_deliveries",
    params(
        ("notifier" = Option<String>, Query, description = "Notifier id or name"),
        ("status" = Option<String>, Query, description = "pending, sent or failed"),
        ("limit" = Option<u32>, Query, description = "1-500, default 100"),
    ),
    responses((status = 200, description = "Deliveries", body = DeliveryPage)))]
async fn list_deliveries(State(s): State<AlertService>, Access(ctx): Access, q: Query) -> ApiResult {
    ok(s.deliveries(&ctx, q.get("notifier"), q.get("status"), q.u32("limit")?)
        .await
        .map_err(api)?)
}

/// One alert with its state changes.
#[utoipa::path(get, path = "/api/v1/alerts/{id}", tag = "alerts", operation_id = "alerts_get",
    params(("id" = String, Path, description = "Alert id")),
    responses((status = 200, description = "Alert", body = AlertDetail), (status = 404, description = "alert_not_found")))]
async fn get_alert(State(s): State<AlertService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.alert(&ctx, &id).await.map_err(api)?)
}

/// SLOs.
#[utoipa::path(get, path = "/api/v1/slos", tag = "slos", operation_id = "slos_list",
    responses((status = 200, description = "SLOs", body = [Slo])))]
async fn list_slos(State(s): State<AlertService>, Access(ctx): Access) -> ApiResult {
    ok(s.slos(&ctx).await.map_err(api)?)
}

/// Creates an SLO (counting starts now).
#[utoipa::path(post, path = "/api/v1/slos", tag = "slos", operation_id = "slos_create",
    request_body = SloInput,
    responses((status = 201, description = "Created", body = Slo), (status = 400, description = "invalid_slo"),
        (status = 409, description = "slo_exists")))]
async fn create_slo(State(s): State<AlertService>, Access(ctx): Access, ApiJson(input): ApiJson<SloInput>) -> ApiResult {
    created(s.create_slo(&ctx, input).await.map_err(api)?)
}

/// The status of every SLO.
#[utoipa::path(get, path = "/api/v1/slos/status", tag = "slos", operation_id = "slos_list_status",
    responses((status = 200, description = "Statuses", body = [SloStatus])))]
async fn list_slo_status(State(s): State<AlertService>, Access(ctx): Access) -> ApiResult {
    ok(s.slo_statuses(&ctx).await.map_err(api)?)
}

/// One SLO (by id or name).
#[utoipa::path(get, path = "/api/v1/slos/{id}", tag = "slos", operation_id = "slos_get",
    params(("id" = String, Path, description = "SLO id or name")),
    responses((status = 200, description = "SLO", body = Slo), (status = 404, description = "slo_not_found")))]
async fn get_slo(State(s): State<AlertService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.slo(&ctx, &id).await.map_err(api)?)
}

/// Replaces an SLO's definition.
#[utoipa::path(put, path = "/api/v1/slos/{id}", tag = "slos", operation_id = "slos_update",
    params(("id" = String, Path, description = "SLO id or name")), request_body = SloInput,
    responses((status = 200, description = "SLO", body = Slo), (status = 404, description = "slo_not_found")))]
async fn update_slo(
    State(s): State<AlertService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    ApiJson(input): ApiJson<SloInput>,
) -> ApiResult {
    ok(s.update_slo(&ctx, &id, input).await.map_err(api)?)
}

/// Deletes an SLO and its counts.
#[utoipa::path(delete, path = "/api/v1/slos/{id}", tag = "slos", operation_id = "slos_delete",
    params(("id" = String, Path, description = "SLO id or name")),
    responses((status = 204, description = "Deleted"), (status = 404, description = "slo_not_found")))]
async fn delete_slo(State(s): State<AlertService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    s.delete_slo(&ctx, &id).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// SLI, error budget, burn rates and burn-rate alerts of an SLO.
#[utoipa::path(get, path = "/api/v1/slos/{id}/status", tag = "slos", operation_id = "slos_status",
    params(("id" = String, Path, description = "SLO id or name")),
    responses((status = 200, description = "Status", body = SloStatus), (status = 404, description = "slo_not_found")))]
async fn slo_status(State(s): State<AlertService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.slo_status(&ctx, &id).await.map_err(api)?)
}
