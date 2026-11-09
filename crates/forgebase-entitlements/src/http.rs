//! REST under `/api/v1` (mounted as an `ApiExtension`):
//!
//! * `entitlements`: effective entitlements, `has()`, subscriptions,
//!   trials, add-ons, usage packs held, overrides, enterprise limits;
//! * `entitlements-catalog`: features, plans, add-ons, usage packs, quota
//!   policies;
//! * `usage`: quota checks, consumption, reservations, leases, gauges,
//!   usage summaries, events (FB-70's billing feed) and alerts.
//!
//! Handlers only translate HTTP; the service authorises every call. A quota
//! denial is `429 quota_exceeded` with the decision in `details`; a feature
//! outside the plan is `403 feature_not_entitled`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use forgebase_api::http::{ApiJson, Query};
use forgebase_api::principal::Access;
use forgebase_api::{ApiError, ApiExtension};
use forgebase_policy::PolicyError;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower_http::limit::RequestBodyLimitLayer;
use utoipa::{OpenApi, ToSchema};

use crate::error::EntitlementError;
use crate::model::{
    Addon, AddonFeature, AddonInput, AddonQuantity, Consumption, CustomLimit, Decision, Enforcement, Entitlements,
    Feature, FeatureInput, FeatureKind, GaugeRequest, Grant, GrantSource, HasAnswer, Lease, MaintenanceReport,
    Override, OverrideInput, PackGrant, PackGrantInput, Plan, PlanFeature, PlanInput, QuotaPolicy, QuotaScope,
    Reservation, ReservationStatus, Subscription, SubscriptionStatus, SubscriptionSync, SyncOutcome, TrialInput,
    UsageAlert, UsageEvent, UsageEventPage, UsagePack, UsagePackInput, UsageRequest, UsageSummary,
};
use crate::period::Period;
use crate::service::EntitlementService;

type ApiResult = Result<Response, ApiError>;
type Body<T> = Result<ApiJson<T>, ApiError>;

const MAX_BODY: usize = 256 * 1024;

/// The HTTP form of a service error.
pub fn api(e: EntitlementError) -> ApiError {
    match &e {
        EntitlementError::QuotaExceeded(d) => {
            ApiError::new(StatusCode::TOO_MANY_REQUESTS, "quota_exceeded", e.to_string()).with_details(json!(d))
        }
        EntitlementError::FeatureDisabled { feature, .. } => {
            ApiError::from_error(&e).with_details(json!({"feature": feature, "step": "entitlement"}))
        }
        EntitlementError::Policy(PolicyError::Denied(d)) => ApiError::from_error(&e).with_details(json!({"step": d.step})),
        _ => ApiError::from_error(&e),
    }
}

fn ok<T: Serialize>(v: T) -> ApiResult {
    Ok(Json(v).into_response())
}

fn created<T: Serialize>(v: T, new: bool) -> ApiResult {
    let status = if new { StatusCode::CREATED } else { StatusCode::OK };
    Ok((status, Json(v)).into_response())
}

/// The routes, relative to `/api/v1`, with their state applied.
pub fn router<S: Clone + Send + Sync + 'static>(svc: EntitlementService) -> Router<S> {
    Router::<EntitlementService>::new()
        .route("/entitlements", get(entitlements))
        .route("/entitlements/has", get(has))
        .route("/entitlements/subscription", get(subscription).put(sync_subscription))
        .route("/entitlements/trials", post(start_trial))
        .route("/entitlements/addons", put(set_addon))
        .route("/entitlements/usage-packs", get(pack_grants).post(grant_pack))
        .route("/entitlements/overrides", get(overrides).post(add_override))
        .route("/entitlements/overrides/{id}", axum::routing::delete(delete_override))
        .route(
            "/entitlements/limits",
            get(custom_limits).put(put_custom_limit).delete(delete_custom_limit),
        )
        .route("/features", get(features))
        .route("/features/{key}", get(feature).put(put_feature).delete(delete_feature))
        .route("/plans", get(plans))
        .route("/plans/{key}", get(plan).put(put_plan).delete(delete_plan))
        .route("/addons", get(addons))
        .route("/addons/{key}", put(put_addon).delete(delete_addon))
        .route("/usage-packs", get(usage_packs))
        .route("/usage-packs/{key}", put(put_usage_pack).delete(delete_usage_pack))
        .route("/quota-policies", get(quota_policies))
        .route(
            "/quota-policies/{feature}",
            put(put_quota_policy).delete(delete_quota_policy),
        )
        .route("/quotas/check", post(check))
        .route("/usage", get(usage))
        .route("/usage/consume", post(consume))
        .route("/usage/reservations", post(reserve))
        .route("/usage/reservations/{id}/commit", post(commit))
        .route("/usage/reservations/{id}/release", post(release))
        .route("/usage/leases", post(acquire_lease))
        .route("/usage/leases/{id}/renew", post(renew_lease))
        .route("/usage/leases/{id}", axum::routing::delete(release_lease))
        .route("/usage/gauge", post(gauge))
        .route("/usage/events", get(events))
        .route("/usage/alerts", get(alerts))
        .route("/usage/maintenance", post(maintenance))
        .layer(RequestBodyLimitLayer::new(MAX_BODY))
        .with_state(svc)
}

/// The routes and OpenAPI fragment, ready for `RouterOptions::extensions`.
pub fn extension(svc: EntitlementService) -> ApiExtension {
    ApiExtension {
        routes: router(svc),
        openapi: openapi(),
    }
}

/// OpenAPI paths and schemas.
pub fn openapi() -> utoipa::openapi::OpenApi {
    EntitlementsDoc::openapi()
}

#[derive(OpenApi)]
#[openapi(
    paths(
        entitlements, has, subscription, sync_subscription, start_trial, set_addon, pack_grants, grant_pack,
        overrides, add_override, delete_override, custom_limits, put_custom_limit, delete_custom_limit,
        features, feature, put_feature, delete_feature, plans, plan, put_plan, delete_plan, addons, put_addon,
        delete_addon, usage_packs, put_usage_pack, delete_usage_pack, quota_policies, put_quota_policy,
        delete_quota_policy, check, usage, consume, reserve, commit, release, acquire_lease, renew_lease,
        release_lease, gauge, events, alerts, maintenance,
    ),
    components(schemas(
        Entitlements, Grant, GrantSource, HasAnswer, Subscription, SubscriptionStatus, SubscriptionSync, SyncOutcome,
        TrialInput, AddonQuantity, SubjectAddon, PackGrant, PackGrantInput, Override, OverrideInput, CustomLimit,
        Feature, FeatureInput, FeatureKind, Plan, PlanInput, PlanFeature, Addon, AddonInput, AddonFeature, UsagePack,
        UsagePackInput, QuotaPolicy, QuotaScope, Enforcement, Period, UsageRequest, Decision, Consumption,
        Reservation, ReservationStatus, CommitBody, Lease, RenewBody, GaugeRequest, UsageSummary, UsageEvent,
        UsageEventPage, UsageAlert, MaintenanceReport, Items<UsageSummary>,
    )),
    tags(
        (name = "entitlements", description = "Effective entitlements of users and organizations, has() checks, subscriptions, trials, overrides and enterprise limits"),
        (name = "entitlements-catalog", description = "Plans, features, add-ons, usage packs and quota policies"),
        (name = "usage", description = "Quota checks, metering (consume, reserve/commit/release, leases, gauges), usage, events and alerts"),
    ),
)]
struct EntitlementsDoc;

/// A list.
#[derive(Serialize, ToSchema)]
pub struct Items<T> {
    /// Entries.
    pub items: Vec<T>,
}

fn items<T: Serialize>(v: Vec<T>) -> ApiResult {
    ok(Items { items: v })
}

/// Body of `PUT /entitlements/addons`.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SubjectAddon {
    /// Subject.
    pub subject: String,
    /// Add-on key.
    pub addon: String,
    /// Quantity (0 removes it).
    pub quantity: i64,
}

/// Body of a reservation commit.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CommitBody {
    /// Units used (default: all reserved units; the rest is released).
    #[serde(default)]
    pub units: Option<i64>,
}

/// Body of a lease renewal.
#[derive(Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RenewBody {
    /// New hold from now (default: the policy's lease length).
    #[serde(default)]
    pub ttl_secs: Option<i64>,
}

fn opt_body<T: Default>(b: Body<T>) -> Result<T, ApiError> {
    match b {
        Ok(ApiJson(v)) => Ok(v),
        Err(e) if e.code() == "unsupported_media_type" => Ok(T::default()),
        Err(e) => Err(e),
    }
}

/// Authorises administrators before surfacing body errors.
fn admin_body<T>(s: &EntitlementService, ctx: &forgebase_data::AccessContext, b: Body<T>) -> Result<T, ApiError> {
    s.require_admin(ctx).map_err(api)?;
    Ok(b?.0)
}

fn caller_body<T>(s: &EntitlementService, ctx: &forgebase_data::AccessContext, b: Body<T>) -> Result<T, ApiError> {
    s.require_caller(ctx).map_err(api)?;
    Ok(b?.0)
}

// ---------------------------------------------------------------------------
// entitlements
// ---------------------------------------------------------------------------

/// Effective entitlements of the caller's subject (the active organization,
/// else the caller), or of `subject`.
#[utoipa::path(get, path = "/api/v1/entitlements", tag = "entitlements",
    params(("subject" = Option<String>, Query, description = "org:<id> or a principal key (default: the caller's subject)")),
    responses((status = 200, description = "Plan, status and grants by feature", body = Entitlements)))]
async fn entitlements(State(s): State<EntitlementService>, Access(ctx): Access, q: Query) -> ApiResult {
    ok(s.entitlements(&ctx, q.get("subject")).map_err(api)?)
}

/// `has({feature})`, `has({permission})` or `has({plan})`: exactly one of the parameters.
#[utoipa::path(get, path = "/api/v1/entitlements/has", tag = "entitlements",
    params(
        ("feature" = Option<String>, Query, description = "Feature key"),
        ("permission" = Option<String>, Query, description = "Permission key (role permission and the features gating it)"),
        ("plan" = Option<String>, Query, description = "Plan key"),
        ("subject" = Option<String>, Query, description = "Subject for feature and plan checks"),
    ),
    responses((status = 200, description = "The answer", body = HasAnswer)))]
async fn has(State(s): State<EntitlementService>, Access(ctx): Access, q: Query) -> ApiResult {
    s.require_caller(&ctx).map_err(api)?;
    let answer = match (q.get("feature"), q.get("permission"), q.get("plan")) {
        (Some(f), None, None) => s.has_feature(&ctx, f, q.get("subject")),
        (None, Some(p), None) => s.has_permission(&ctx, p),
        (None, None, Some(p)) => s.has_plan(&ctx, p, q.get("subject")),
        _ => {
            return Err(ApiError::bad_request(
                "invalid_parameter",
                "give exactly one of feature, permission and plan",
            ));
        }
    };
    ok(answer.map_err(api)?)
}

/// The subscription of a subject (`billing:read`).
#[utoipa::path(get, path = "/api/v1/entitlements/subscription", tag = "entitlements",
    params(("subject" = Option<String>, Query, description = "Subject (default: the caller's)")),
    responses((status = 200, description = "Subscription", body = Subscription), (status = 404, description = "No subscription")))]
async fn subscription(State(s): State<EntitlementService>, Access(ctx): Access, q: Query) -> ApiResult {
    let subject = s.subject_for(&ctx, q.get("subject")).map_err(api)?;
    match s.subscription(&ctx, &subject).map_err(api)? {
        Some(sub) => ok(sub),
        None => Err(ApiError::not_found("no subscription")),
    }
}

/// Sets a subject's plan, status and add-ons (platform administrators; FB-70
/// uses the Rust seam). Idempotent by `event_id`, ordered by `event_at`.
#[utoipa::path(put, path = "/api/v1/entitlements/subscription", tag = "entitlements",
    request_body = SubscriptionSync,
    responses((status = 200, description = "applied, replayed or stale", body = SyncOutcome)))]
async fn sync_subscription(
    State(s): State<EntitlementService>,
    Access(ctx): Access,
    b: Body<SubscriptionSync>,
) -> ApiResult {
    let b = admin_body(&s, &ctx, b)?;
    ok(s.apply_subscription(&ctx, b).await.map_err(api)?)
}

/// Starts a trial (platform administrators).
#[utoipa::path(post, path = "/api/v1/entitlements/trials", tag = "entitlements",
    request_body = TrialInput, responses((status = 201, description = "Trialing subscription", body = Subscription)))]
async fn start_trial(State(s): State<EntitlementService>, Access(ctx): Access, b: Body<TrialInput>) -> ApiResult {
    let b = admin_body(&s, &ctx, b)?;
    created(s.start_trial(&ctx, b).await.map_err(api)?, true)
}

/// Sets the quantity of an add-on of a subject (platform administrators).
#[utoipa::path(put, path = "/api/v1/entitlements/addons", tag = "entitlements",
    request_body = SubjectAddon, responses((status = 200, description = "Subscription", body = Subscription)))]
async fn set_addon(State(s): State<EntitlementService>, Access(ctx): Access, b: Body<SubjectAddon>) -> ApiResult {
    let b = admin_body(&s, &ctx, b)?;
    let a = AddonQuantity {
        addon: b.addon,
        quantity: b.quantity,
    };
    ok(s.set_addon(&ctx, &b.subject, a).await.map_err(api)?)
}

/// Usage packs a subject holds (`billing:read`).
#[utoipa::path(get, path = "/api/v1/entitlements/usage-packs", tag = "entitlements",
    params(("subject" = Option<String>, Query, description = "Subject (default: the caller's)")),
    responses((status = 200, description = "Grants", body = Items<PackGrant>)))]
async fn pack_grants(State(s): State<EntitlementService>, Access(ctx): Access, q: Query) -> ApiResult {
    let subject = s.subject_for(&ctx, q.get("subject")).map_err(api)?;
    items(s.pack_grants(&ctx, &subject).await.map_err(api)?)
}

/// Grants usage packs (platform administrators; FB-70 after a purchase).
#[utoipa::path(post, path = "/api/v1/entitlements/usage-packs", tag = "entitlements",
    request_body = PackGrantInput, responses((status = 201, description = "Grant (200: replay)", body = PackGrant)))]
async fn grant_pack(State(s): State<EntitlementService>, Access(ctx): Access, b: Body<PackGrantInput>) -> ApiResult {
    let b = admin_body(&s, &ctx, b)?;
    created(s.grant_pack(&ctx, b).await.map_err(api)?, true)
}

/// Overrides (platform administrators).
#[utoipa::path(get, path = "/api/v1/entitlements/overrides", tag = "entitlements",
    params(("subject" = Option<String>, Query, description = "Only this subject")),
    responses((status = 200, description = "Overrides", body = Items<Override>)))]
async fn overrides(State(s): State<EntitlementService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.overrides(&ctx, q.get("subject")).map_err(api)?)
}

/// Adds an override (platform administrators). Deny always wins.
#[utoipa::path(post, path = "/api/v1/entitlements/overrides", tag = "entitlements",
    request_body = OverrideInput, responses((status = 201, description = "Override", body = Override)))]
async fn add_override(State(s): State<EntitlementService>, Access(ctx): Access, b: Body<OverrideInput>) -> ApiResult {
    let b = admin_body(&s, &ctx, b)?;
    created(s.add_override(&ctx, b).await.map_err(api)?, true)
}

/// Deletes an override.
#[utoipa::path(delete, path = "/api/v1/entitlements/overrides/{id}", tag = "entitlements",
    params(("id" = String, Path, description = "Override id")),
    responses((status = 204, description = "Deleted")))]
async fn delete_override(State(s): State<EntitlementService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    s.delete_override(&ctx, &id).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Enterprise limits (platform administrators).
#[utoipa::path(get, path = "/api/v1/entitlements/limits", tag = "entitlements",
    params(("subject" = Option<String>, Query, description = "Only this subject")),
    responses((status = 200, description = "Limits", body = Items<CustomLimit>)))]
async fn custom_limits(State(s): State<EntitlementService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.custom_limits(&ctx, q.get("subject")).map_err(api)?)
}

/// Sets an enterprise limit of a subject (replaces the plan's grant).
#[utoipa::path(put, path = "/api/v1/entitlements/limits", tag = "entitlements",
    request_body = CustomLimit, responses((status = 200, description = "Limit", body = CustomLimit)))]
async fn put_custom_limit(State(s): State<EntitlementService>, Access(ctx): Access, b: Body<CustomLimit>) -> ApiResult {
    let b = admin_body(&s, &ctx, b)?;
    ok(s.put_custom_limit(&ctx, b).await.map_err(api)?)
}

/// Removes an enterprise limit.
#[utoipa::path(delete, path = "/api/v1/entitlements/limits", tag = "entitlements",
    params(("subject" = String, Query, description = "Subject"), ("feature" = String, Query, description = "Feature")),
    responses((status = 204, description = "Removed")))]
async fn delete_custom_limit(State(s): State<EntitlementService>, Access(ctx): Access, q: Query) -> ApiResult {
    s.require_admin(&ctx).map_err(api)?;
    let (Some(subject), Some(feature)) = (q.get("subject"), q.get("feature")) else {
        return Err(ApiError::bad_request("invalid_parameter", "subject and feature are required"));
    };
    s.delete_custom_limit(&ctx, subject, feature).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// catalog
// ---------------------------------------------------------------------------

/// Features of the catalog.
#[utoipa::path(get, path = "/api/v1/features", tag = "entitlements-catalog",
    responses((status = 200, description = "Features", body = Items<Feature>)))]
async fn features(State(s): State<EntitlementService>, Access(ctx): Access) -> ApiResult {
    items(s.features(&ctx).map_err(api)?)
}

/// One feature.
#[utoipa::path(get, path = "/api/v1/features/{key}", tag = "entitlements-catalog",
    params(("key" = String, Path, description = "Feature key")),
    responses((status = 200, description = "Feature", body = Feature)))]
async fn feature(State(s): State<EntitlementService>, Access(ctx): Access, Path(key): Path<String>) -> ApiResult {
    ok(s.feature(&ctx, &key).map_err(api)?)
}

/// Creates or replaces a feature (platform administrators).
#[utoipa::path(put, path = "/api/v1/features/{key}", tag = "entitlements-catalog",
    params(("key" = String, Path, description = "Feature key")),
    request_body = FeatureInput, responses((status = 200, description = "Feature", body = Feature)))]
async fn put_feature(
    State(s): State<EntitlementService>,
    Access(ctx): Access,
    Path(key): Path<String>,
    b: Body<FeatureInput>,
) -> ApiResult {
    let b = admin_body(&s, &ctx, b)?;
    ok(s.put_feature(&ctx, &key, b).await.map_err(api)?)
}

/// Deletes a feature.
#[utoipa::path(delete, path = "/api/v1/features/{key}", tag = "entitlements-catalog",
    params(("key" = String, Path, description = "Feature key")), responses((status = 204, description = "Deleted")))]
async fn delete_feature(State(s): State<EntitlementService>, Access(ctx): Access, Path(key): Path<String>) -> ApiResult {
    s.delete_feature(&ctx, &key).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Plans.
#[utoipa::path(get, path = "/api/v1/plans", tag = "entitlements-catalog",
    responses((status = 200, description = "Plans", body = Items<Plan>)))]
async fn plans(State(s): State<EntitlementService>, Access(ctx): Access) -> ApiResult {
    items(s.plans(&ctx).map_err(api)?)
}

/// One plan.
#[utoipa::path(get, path = "/api/v1/plans/{key}", tag = "entitlements-catalog",
    params(("key" = String, Path, description = "Plan key")), responses((status = 200, description = "Plan", body = Plan)))]
async fn plan(State(s): State<EntitlementService>, Access(ctx): Access, Path(key): Path<String>) -> ApiResult {
    ok(s.plan(&ctx, &key).map_err(api)?)
}

/// Creates or replaces a plan and its features (platform administrators).
#[utoipa::path(put, path = "/api/v1/plans/{key}", tag = "entitlements-catalog",
    params(("key" = String, Path, description = "Plan key")),
    request_body = PlanInput, responses((status = 200, description = "Plan", body = Plan)))]
async fn put_plan(
    State(s): State<EntitlementService>,
    Access(ctx): Access,
    Path(key): Path<String>,
    b: Body<PlanInput>,
) -> ApiResult {
    let b = admin_body(&s, &ctx, b)?;
    ok(s.put_plan(&ctx, &key, b).await.map_err(api)?)
}

/// Deletes a plan no subscription uses.
#[utoipa::path(delete, path = "/api/v1/plans/{key}", tag = "entitlements-catalog",
    params(("key" = String, Path, description = "Plan key")), responses((status = 204, description = "Deleted")))]
async fn delete_plan(State(s): State<EntitlementService>, Access(ctx): Access, Path(key): Path<String>) -> ApiResult {
    s.delete_plan(&ctx, &key).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Add-ons.
#[utoipa::path(get, path = "/api/v1/addons", tag = "entitlements-catalog",
    responses((status = 200, description = "Add-ons", body = Items<Addon>)))]
async fn addons(State(s): State<EntitlementService>, Access(ctx): Access) -> ApiResult {
    items(s.addons(&ctx).map_err(api)?)
}

/// Creates or replaces an add-on (platform administrators).
#[utoipa::path(put, path = "/api/v1/addons/{key}", tag = "entitlements-catalog",
    params(("key" = String, Path, description = "Add-on key")),
    request_body = AddonInput, responses((status = 200, description = "Add-on", body = Addon)))]
async fn put_addon(
    State(s): State<EntitlementService>,
    Access(ctx): Access,
    Path(key): Path<String>,
    b: Body<AddonInput>,
) -> ApiResult {
    let b = admin_body(&s, &ctx, b)?;
    ok(s.put_addon(&ctx, &key, b).await.map_err(api)?)
}

/// Deletes an add-on.
#[utoipa::path(delete, path = "/api/v1/addons/{key}", tag = "entitlements-catalog",
    params(("key" = String, Path, description = "Add-on key")), responses((status = 204, description = "Deleted")))]
async fn delete_addon(State(s): State<EntitlementService>, Access(ctx): Access, Path(key): Path<String>) -> ApiResult {
    s.delete_addon(&ctx, &key).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Usage packs of the catalog.
#[utoipa::path(get, path = "/api/v1/usage-packs", tag = "entitlements-catalog",
    responses((status = 200, description = "Usage packs", body = Items<UsagePack>)))]
async fn usage_packs(State(s): State<EntitlementService>, Access(ctx): Access) -> ApiResult {
    items(s.usage_packs(&ctx).map_err(api)?)
}

/// Creates or replaces a usage pack (platform administrators).
#[utoipa::path(put, path = "/api/v1/usage-packs/{key}", tag = "entitlements-catalog",
    params(("key" = String, Path, description = "Pack key")),
    request_body = UsagePackInput, responses((status = 200, description = "Usage pack", body = UsagePack)))]
async fn put_usage_pack(
    State(s): State<EntitlementService>,
    Access(ctx): Access,
    Path(key): Path<String>,
    b: Body<UsagePackInput>,
) -> ApiResult {
    let b = admin_body(&s, &ctx, b)?;
    ok(s.put_usage_pack(&ctx, &key, b).await.map_err(api)?)
}

/// Deletes a usage pack from the catalog.
#[utoipa::path(delete, path = "/api/v1/usage-packs/{key}", tag = "entitlements-catalog",
    params(("key" = String, Path, description = "Pack key")), responses((status = 204, description = "Deleted")))]
async fn delete_usage_pack(
    State(s): State<EntitlementService>,
    Access(ctx): Access,
    Path(key): Path<String>,
) -> ApiResult {
    s.delete_usage_pack(&ctx, &key).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Quota policies.
#[utoipa::path(get, path = "/api/v1/quota-policies", tag = "entitlements-catalog",
    responses((status = 200, description = "Policies", body = Items<QuotaPolicy>)))]
async fn quota_policies(State(s): State<EntitlementService>, Access(ctx): Access) -> ApiResult {
    items(s.quota_policies(&ctx).map_err(api)?)
}

/// Creates or replaces a feature's quota policy (platform administrators).
#[utoipa::path(put, path = "/api/v1/quota-policies/{feature}", tag = "entitlements-catalog",
    params(("feature" = String, Path, description = "Feature key")),
    request_body = QuotaPolicy, responses((status = 200, description = "Policy", body = QuotaPolicy)))]
async fn put_quota_policy(
    State(s): State<EntitlementService>,
    Access(ctx): Access,
    Path(feature): Path<String>,
    b: Body<QuotaPolicy>,
) -> ApiResult {
    let b = admin_body(&s, &ctx, b)?;
    ok(s.put_quota_policy(&ctx, &feature, b).await.map_err(api)?)
}

/// Deletes a quota policy.
#[utoipa::path(delete, path = "/api/v1/quota-policies/{feature}", tag = "entitlements-catalog",
    params(("feature" = String, Path, description = "Feature key")), responses((status = 204, description = "Deleted")))]
async fn delete_quota_policy(
    State(s): State<EntitlementService>,
    Access(ctx): Access,
    Path(feature): Path<String>,
) -> ApiResult {
    s.delete_quota_policy(&ctx, &feature).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ---------------------------------------------------------------------------
// usage
// ---------------------------------------------------------------------------

/// Would the units be granted now? No side effects.
#[utoipa::path(post, path = "/api/v1/quotas/check", tag = "usage",
    request_body = UsageRequest, responses((status = 200, description = "Decision", body = Decision)))]
async fn check(State(s): State<EntitlementService>, Access(ctx): Access, b: Body<UsageRequest>) -> ApiResult {
    let b = caller_body(&s, &ctx, b)?;
    ok(s.check(&ctx, &b).await.map_err(api)?)
}

/// Current usage of a subject (`billing:read`).
#[utoipa::path(get, path = "/api/v1/usage", tag = "usage",
    params(
        ("subject" = Option<String>, Query, description = "Subject (default: the caller's)"),
        ("scope_key" = Option<String>, Query, description = "Counter of per-user, team or resource quotas"),
    ),
    responses((status = 200, description = "Usage by feature", body = Items<UsageSummary>)))]
async fn usage(State(s): State<EntitlementService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.usage(&ctx, q.get("subject"), q.get("scope_key")).await.map_err(api)?)
}

/// Records usage at once (`usage:write`). 200 for a replayed idempotency key.
#[utoipa::path(post, path = "/api/v1/usage/consume", tag = "usage",
    request_body = UsageRequest,
    responses(
        (status = 201, description = "Recorded", body = Consumption),
        (status = 200, description = "Replay of an earlier request", body = Consumption),
        (status = 429, description = "Quota exhausted (details: the decision)"),
    ))]
async fn consume(State(s): State<EntitlementService>, Access(ctx): Access, b: Body<UsageRequest>) -> ApiResult {
    let b = caller_body(&s, &ctx, b)?;
    let c = s.consume(&ctx, b).await.map_err(api)?;
    let new = !c.replayed;
    created(c, new)
}

/// Holds units until committed, released or expired (`usage:write`).
#[utoipa::path(post, path = "/api/v1/usage/reservations", tag = "usage",
    request_body = UsageRequest,
    responses(
        (status = 201, description = "Held", body = Reservation),
        (status = 200, description = "Replay", body = Reservation),
        (status = 429, description = "Quota exhausted"),
    ))]
async fn reserve(State(s): State<EntitlementService>, Access(ctx): Access, b: Body<UsageRequest>) -> ApiResult {
    let b = caller_body(&s, &ctx, b)?;
    let r = s.reserve(&ctx, b).await.map_err(api)?;
    let new = !r.replayed;
    created(r, new)
}

/// Commits a reservation (all or part of it; the rest is released).
#[utoipa::path(post, path = "/api/v1/usage/reservations/{id}/commit", tag = "usage",
    params(("id" = String, Path, description = "Reservation id")),
    request_body = CommitBody, responses((status = 200, description = "Committed", body = Reservation)))]
async fn commit(
    State(s): State<EntitlementService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    b: Body<CommitBody>,
) -> ApiResult {
    s.require_caller(&ctx).map_err(api)?;
    let b = opt_body(b)?;
    ok(s.commit(&ctx, &id, b.units).await.map_err(api)?)
}

/// Releases a reservation.
#[utoipa::path(post, path = "/api/v1/usage/reservations/{id}/release", tag = "usage",
    params(("id" = String, Path, description = "Reservation id")),
    responses((status = 200, description = "Released", body = Reservation)))]
async fn release(State(s): State<EntitlementService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.release(&ctx, &id).await.map_err(api)?)
}

/// Acquires a concurrency lease (`usage:write`).
#[utoipa::path(post, path = "/api/v1/usage/leases", tag = "usage",
    request_body = UsageRequest,
    responses(
        (status = 201, description = "Lease", body = Lease),
        (status = 200, description = "Replay", body = Lease),
        (status = 429, description = "Every lease is in use"),
    ))]
async fn acquire_lease(State(s): State<EntitlementService>, Access(ctx): Access, b: Body<UsageRequest>) -> ApiResult {
    let b = caller_body(&s, &ctx, b)?;
    let l = s.acquire_lease(&ctx, b).await.map_err(api)?;
    let new = !l.replayed;
    created(l, new)
}

/// Renews an active lease.
#[utoipa::path(post, path = "/api/v1/usage/leases/{id}/renew", tag = "usage",
    params(("id" = String, Path, description = "Lease id")),
    request_body = RenewBody, responses((status = 200, description = "Lease", body = Lease)))]
async fn renew_lease(
    State(s): State<EntitlementService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    b: Body<RenewBody>,
) -> ApiResult {
    s.require_caller(&ctx).map_err(api)?;
    let b = opt_body(b)?;
    ok(s.renew_lease(&ctx, &id, b.ttl_secs).await.map_err(api)?)
}

/// Releases a lease.
#[utoipa::path(delete, path = "/api/v1/usage/leases/{id}", tag = "usage",
    params(("id" = String, Path, description = "Lease id")),
    responses((status = 200, description = "Released", body = Lease)))]
async fn release_lease(State(s): State<EntitlementService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.release_lease(&ctx, &id).await.map_err(api)?)
}

/// Changes a gauge (storage, seats) by `delta` or to `value` (`usage:write`).
#[utoipa::path(post, path = "/api/v1/usage/gauge", tag = "usage",
    request_body = GaugeRequest,
    responses(
        (status = 201, description = "Recorded", body = Consumption),
        (status = 200, description = "Replay", body = Consumption),
        (status = 429, description = "Over the limit"),
    ))]
async fn gauge(State(s): State<EntitlementService>, Access(ctx): Access, b: Body<GaugeRequest>) -> ApiResult {
    let b = caller_body(&s, &ctx, b)?;
    let c = s.adjust_gauge(&ctx, b).await.map_err(api)?;
    let new = !c.replayed;
    created(c, new)
}

/// Immutable usage events in `seq` order (`billing:read`; FB-70's feed).
#[utoipa::path(get, path = "/api/v1/usage/events", tag = "usage",
    params(
        ("subject" = Option<String>, Query, description = "Subject (administrators may omit it: all subjects)"),
        ("feature" = Option<String>, Query, description = "Only this feature"),
        ("after" = Option<i64>, Query, description = "Cursor: events with a larger seq"),
        ("limit" = Option<u32>, Query, description = "Page size, default 100, at most 1000"),
    ),
    responses((status = 200, description = "A page", body = UsageEventPage)))]
async fn events(State(s): State<EntitlementService>, Access(ctx): Access, q: Query) -> ApiResult {
    let after = match q.get("after") {
        None | Some("") => None,
        Some(v) => Some(
            v.parse::<i64>()
                .map_err(|_| ApiError::bad_request("invalid_parameter", "after must be an integer"))?,
        ),
    };
    let subject = if q.get("subject").is_none() && !ctx.is_privileged() {
        Some(crate::service::subject_key(&ctx.subject()))
    } else {
        q.get("subject").map(str::to_owned)
    };
    ok(s
        .events(&ctx, subject.as_deref(), q.get("feature"), after, q.u32("limit")?)
        .await
        .map_err(api)?)
}

/// Usage alerts of a subject (`billing:read`), newest first.
#[utoipa::path(get, path = "/api/v1/usage/alerts", tag = "usage",
    params(("subject" = Option<String>, Query, description = "Subject (default: the caller's)")),
    responses((status = 200, description = "Alerts", body = Items<UsageAlert>)))]
async fn alerts(State(s): State<EntitlementService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.alerts(&ctx, q.get("subject")).await.map_err(api)?)
}

/// Runs period resets and expiries now (platform administrators; the
/// `entitlements.maintenance` job does it every minute).
#[utoipa::path(post, path = "/api/v1/usage/maintenance", tag = "usage",
    responses((status = 200, description = "What changed", body = MaintenanceReport)))]
async fn maintenance(State(s): State<EntitlementService>, Access(ctx): Access) -> ApiResult {
    s.require_admin(&ctx).map_err(api)?;
    ok(s.run_maintenance().await.map_err(api)?)
}
