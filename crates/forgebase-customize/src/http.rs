//! REST under `/api/v1/customize` (mounted as an `ApiExtension`, tag
//! `customize`): domains, branding, email templates, sending and the delivery
//! log, legal agreements and acceptance, the public views and the branded API
//! documentation page.
//!
//! Handlers only translate HTTP; the service authorises every call. Routes
//! under `/customize/public/` and `/customize/docs` need no credentials: they
//! show what a hosted sign-in page needs (branding, current agreements, the
//! API description). The realm they describe is, in order: the realm of the
//! verified custom domain the request arrived on, the `realm` query parameter,
//! the realm of the request (`X-ForgeBase-Realm`, else `default`).

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use forgebase_api::http::{ApiJson, Query};
use forgebase_api::principal::Access;
use forgebase_api::{ApiError, ApiExtension};
use forgebase_data::AccessContext;
use serde::Serialize;
use tower_http::limit::RequestBodyLimitLayer;
use utoipa::{OpenApi, ToSchema};

use crate::agreements::{AcceptSource, AcceptanceQuery};
use crate::branding::{MAX_ASSET_BYTES, theme_css};
use crate::email::DeliveryQuery;
use crate::error::CustomizeError;
use crate::host::ResolvedHost;
use crate::model::{
    AcceptInput, Acceptance, Agreement, AgreementInput, AgreementRef, AssetKind, Branding, BrandingInput, Delivery,
    DeliveryStatus, Domain, DomainInput, DomainStatus, EmailTemplate, Pending, Preview, PreviewInput, PublicBranding,
    SendInput, TemplateInput, Verification, VerifyResult,
};
use crate::render::escape_html;
use crate::service::CustomizeService;

type ApiResult = Result<Response, ApiError>;
type Body<T> = Result<ApiJson<T>, ApiError>;

const MAX_BODY: usize = 512 * 1024;

/// The HTTP form of a service error.
pub fn api(e: CustomizeError) -> ApiError {
    ApiError::from_error(&e)
}

fn ok<T: Serialize>(v: T) -> ApiResult {
    Ok(Json(v).into_response())
}

fn created<T: Serialize>(v: T) -> ApiResult {
    Ok((StatusCode::CREATED, Json(v)).into_response())
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
fn body<T>(s: &CustomizeService, ctx: &AccessContext, b: Body<T>) -> Result<T, ApiError> {
    s.require_caller(ctx).map_err(api)?;
    Ok(b?.0)
}

/// The routes, relative to `/api/v1`, with their state applied.
pub fn router<S: Clone + Send + Sync + 'static>(svc: CustomizeService) -> Router<S> {
    Router::<CustomizeService>::new()
        .route("/customize/domains", get(domains).post(add_domain))
        .route("/customize/domains/{id}", get(domain).delete(delete_domain))
        .route("/customize/domains/{id}/verify", post(verify_domain))
        .route("/customize/domains/{id}/reissue", post(reissue_domain))
        .route("/customize/branding", get(branding).put(put_branding))
        .route("/customize/branding/assets/{kind}", put(put_asset).delete(delete_asset))
        .route("/customize/email/templates", get(templates))
        .route(
            "/customize/email/templates/{key}",
            get(template).put(put_template).delete(delete_template),
        )
        .route("/customize/email/preview", post(preview))
        .route("/customize/email/send", post(send))
        .route("/customize/email/deliveries", get(deliveries))
        .route("/customize/email/deliveries/{id}", get(delivery))
        .route("/customize/email/deliveries/{id}/retry", post(retry_delivery))
        .route("/customize/agreements", get(agreements).post(publish_agreement))
        .route("/customize/agreements/pending", get(pending))
        .route("/customize/agreements/accept", post(accept))
        .route("/customize/agreements/accepted", get(accepted))
        .route("/customize/agreements/acceptances", get(acceptances))
        .route("/customize/public/branding", get(public_branding))
        .route("/customize/public/branding/{kind}", get(public_asset))
        .route("/customize/public/theme.css", get(public_theme))
        .route("/customize/public/agreements", get(public_agreements))
        .route("/customize/docs", get(docs))
        .layer(RequestBodyLimitLayer::new(MAX_BODY))
        .with_state(svc)
}

/// The routes and OpenAPI fragment, ready for `RouterOptions::extensions`.
pub fn extension(svc: CustomizeService) -> ApiExtension {
    ApiExtension {
        routes: router(svc),
        openapi: openapi(),
    }
}

/// OpenAPI paths and schemas.
pub fn openapi() -> utoipa::openapi::OpenApi {
    CustomizeDoc::openapi()
}

#[derive(OpenApi)]
#[openapi(
    paths(
        domains, add_domain, domain, delete_domain, verify_domain, reissue_domain, branding, put_branding, put_asset,
        delete_asset, templates, template, put_template, delete_template, preview, send, deliveries, delivery,
        retry_delivery, agreements, publish_agreement, pending, accept, accepted, acceptances, public_branding,
        public_asset, public_theme, public_agreements, docs,
    ),
    components(schemas(
        Domain, DomainStatus, DomainInput, Verification, VerifyResult, Branding, BrandingInput, PublicBranding,
        EmailTemplate, TemplateInput, PreviewInput, Preview, SendInput, Delivery, DeliveryStatus, Agreement,
        AgreementInput, AgreementRef, AcceptInput, Acceptance, Pending, Items<Domain>, Items<EmailTemplate>,
        Items<Delivery>, Items<Agreement>, Items<Acceptance>,
    )),
    tags((name = "customize", description = "Product customization per realm: custom domains verified by DNS TXT record and routed by Host header, white-label branding, transactional email templates with a delivery log, versioned legal agreements with acceptance records, and a branded public API docs page")),
)]
struct CustomizeDoc;

const REALM: &str = "platform administrators: the realm to act on (default: the realm of the request)";

// ---------------------------------------------------------------------------
// Domains
// ---------------------------------------------------------------------------

/// The custom domains of a realm.
#[utoipa::path(get, path = "/api/v1/customize/domains", tag = "customize",
    params(("realm" = Option<String>, Query, description = REALM)),
    responses((status = 200, description = "Domains", body = Items<Domain>)))]
async fn domains(State(s): State<CustomizeService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.domains(&ctx, q.get("realm")).await.map_err(api)?)
}

/// Claims a host for a realm. The response says which TXT record to publish;
/// requests for the host reach the realm once `verify` has found it.
#[utoipa::path(post, path = "/api/v1/customize/domains", tag = "customize", request_body = DomainInput,
    responses((status = 201, description = "Pending domain", body = Domain),
              (status = 409, description = "domain_taken")))]
async fn add_domain(State(s): State<CustomizeService>, Access(ctx): Access, b: Body<DomainInput>) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    created(s.add_domain(&ctx, input).await.map_err(api)?)
}

/// One domain.
#[utoipa::path(get, path = "/api/v1/customize/domains/{id}", tag = "customize",
    responses((status = 200, description = "Domain", body = Domain)))]
async fn domain(State(s): State<CustomizeService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.domain(&ctx, &id).await.map_err(api)?)
}

/// Removes a domain: requests for its host stop routing to the realm.
#[utoipa::path(delete, path = "/api/v1/customize/domains/{id}", tag = "customize",
    responses((status = 204, description = "Removed")))]
async fn delete_domain(State(s): State<CustomizeService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    s.delete_domain(&ctx, &id).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Looks the TXT record up. `verified: false` carries the reason; an expired
/// token is `409 token_expired` (re-issue it).
#[utoipa::path(post, path = "/api/v1/customize/domains/{id}/verify", tag = "customize",
    responses((status = 200, description = "Result", body = VerifyResult),
              (status = 409, description = "token_expired")))]
async fn verify_domain(State(s): State<CustomizeService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.verify_domain(&ctx, &id).await.map_err(api)?)
}

/// Issues a new token for a pending domain.
#[utoipa::path(post, path = "/api/v1/customize/domains/{id}/reissue", tag = "customize",
    responses((status = 200, description = "Domain with the new record", body = Domain)))]
async fn reissue_domain(State(s): State<CustomizeService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.reissue_domain(&ctx, &id).await.map_err(api)?)
}

// ---------------------------------------------------------------------------
// Branding
// ---------------------------------------------------------------------------

/// The branding of a realm.
#[utoipa::path(get, path = "/api/v1/customize/branding", tag = "customize",
    params(("realm" = Option<String>, Query, description = REALM)),
    responses((status = 200, description = "Branding", body = Branding)))]
async fn branding(State(s): State<CustomizeService>, Access(ctx): Access, q: Query) -> ApiResult {
    ok(s.branding(&ctx, q.get("realm")).await.map_err(api)?)
}

/// Replaces the branding (fields left out are cleared).
#[utoipa::path(put, path = "/api/v1/customize/branding", tag = "customize", request_body = BrandingInput,
    params(("realm" = Option<String>, Query, description = REALM)),
    responses((status = 200, description = "Branding", body = Branding)))]
async fn put_branding(
    State(s): State<CustomizeService>,
    Access(ctx): Access,
    q: Query,
    b: Body<BrandingInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.put_branding(&ctx, q.get("realm"), input).await.map_err(api)?)
}

fn asset_kind(k: &str) -> Result<AssetKind, ApiError> {
    AssetKind::parse(k).ok_or_else(|| ApiError::not_found("asset kind (logo or favicon)"))
}

/// Uploads the logo or favicon: the raw image as the body with its
/// `Content-Type` (png, jpeg, gif, webp, x-icon or svg+xml; max 256 KiB).
#[utoipa::path(put, path = "/api/v1/customize/branding/assets/{kind}", tag = "customize",
    params(("kind" = String, Path, description = "logo or favicon"),
           ("realm" = Option<String>, Query, description = REALM)),
    request_body(content = Vec<u8>, content_type = "image/png"),
    responses((status = 200, description = "Branding", body = Branding)))]
async fn put_asset(
    State(s): State<CustomizeService>,
    Access(ctx): Access,
    Path(kind): Path<String>,
    q: Query,
    headers: HeaderMap,
    data: Bytes,
) -> ApiResult {
    s.require_caller(&ctx).map_err(api)?;
    let kind = asset_kind(&kind)?;
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if data.len() > MAX_ASSET_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "an image is at most 256 KiB",
        ));
    }
    ok(s.put_asset(&ctx, q.get("realm"), kind, ct, &data).await.map_err(api)?)
}

/// Removes the logo or favicon.
#[utoipa::path(delete, path = "/api/v1/customize/branding/assets/{kind}", tag = "customize",
    params(("kind" = String, Path, description = "logo or favicon"),
           ("realm" = Option<String>, Query, description = REALM)),
    responses((status = 200, description = "Branding", body = Branding)))]
async fn delete_asset(
    State(s): State<CustomizeService>,
    Access(ctx): Access,
    Path(kind): Path<String>,
    q: Query,
) -> ApiResult {
    s.require_caller(&ctx).map_err(api)?;
    ok(s.delete_asset(&ctx, q.get("realm"), asset_kind(&kind)?)
        .await
        .map_err(api)?)
}

// ---------------------------------------------------------------------------
// Email
// ---------------------------------------------------------------------------

/// Templates of a realm: its own and the built-ins it has not overridden.
#[utoipa::path(get, path = "/api/v1/customize/email/templates", tag = "customize",
    params(("realm" = Option<String>, Query, description = REALM)),
    responses((status = 200, description = "Templates", body = Items<EmailTemplate>)))]
async fn templates(State(s): State<CustomizeService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.templates(&ctx, q.get("realm")).await.map_err(api)?)
}

/// One template.
#[utoipa::path(get, path = "/api/v1/customize/email/templates/{key}", tag = "customize",
    params(("key" = String, Path, description = "template key"),
           ("realm" = Option<String>, Query, description = REALM)),
    responses((status = 200, description = "Template", body = EmailTemplate)))]
async fn template(
    State(s): State<CustomizeService>,
    Access(ctx): Access,
    Path(key): Path<String>,
    q: Query,
) -> ApiResult {
    ok(s.template(&ctx, q.get("realm"), &key).await.map_err(api)?)
}

/// Saves a template. Variables are written `{{ name }}` and always escaped
/// (HTML-escaped in the HTML body; control characters removed from the
/// subject); `link`, `url` and `*_url` variables must be http(s) or mailto URLs.
#[utoipa::path(put, path = "/api/v1/customize/email/templates/{key}", tag = "customize", request_body = TemplateInput,
    params(("key" = String, Path, description = "template key"),
           ("realm" = Option<String>, Query, description = REALM)),
    responses((status = 200, description = "Template", body = EmailTemplate)))]
async fn put_template(
    State(s): State<CustomizeService>,
    Access(ctx): Access,
    Path(key): Path<String>,
    q: Query,
    b: Body<TemplateInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.put_template(&ctx, q.get("realm"), &key, input).await.map_err(api)?)
}

/// Removes a realm's template (built-in keys go back to the built-in text).
#[utoipa::path(delete, path = "/api/v1/customize/email/templates/{key}", tag = "customize",
    params(("key" = String, Path, description = "template key"),
           ("realm" = Option<String>, Query, description = REALM)),
    responses((status = 204, description = "Removed")))]
async fn delete_template(
    State(s): State<CustomizeService>,
    Access(ctx): Access,
    Path(key): Path<String>,
    q: Query,
) -> ApiResult {
    s.delete_template(&ctx, q.get("realm"), &key).await.map_err(api)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Renders a stored template (`template`) or a draft (`draft`) with variables, without sending.
#[utoipa::path(post, path = "/api/v1/customize/email/preview", tag = "customize", request_body = PreviewInput,
    params(("realm" = Option<String>, Query, description = REALM)),
    responses((status = 200, description = "Rendered message", body = Preview)))]
async fn preview(State(s): State<CustomizeService>, Access(ctx): Access, q: Query, b: Body<PreviewInput>) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    ok(s.preview(&ctx, q.get("realm"), input).await.map_err(api)?)
}

/// Renders a template and queues the message (`202`); the delivery log shows its progress.
#[utoipa::path(post, path = "/api/v1/customize/email/send", tag = "customize", request_body = SendInput,
    params(("realm" = Option<String>, Query, description = REALM)),
    responses((status = 202, description = "Queued", body = Delivery)))]
async fn send(State(s): State<CustomizeService>, Access(ctx): Access, q: Query, b: Body<SendInput>) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(s.send(&ctx, q.get("realm"), input).await.map_err(api)?),
    )
        .into_response())
}

/// The delivery log, newest first.
#[utoipa::path(get, path = "/api/v1/customize/email/deliveries", tag = "customize",
    params(("realm" = Option<String>, Query, description = REALM),
           ("status" = Option<DeliveryStatus>, Query, description = "queued, retrying, sent or failed"),
           ("to" = Option<String>, Query, description = "recipient"),
           ("template" = Option<String>, Query, description = "template key"),
           ("limit" = Option<u32>, Query, description = "default 50, max 200"),
           ("before" = Option<i64>, Query, description = "only rows created before this epoch ms")),
    responses((status = 200, description = "Deliveries", body = Items<Delivery>)))]
async fn deliveries(State(s): State<CustomizeService>, Access(ctx): Access, q: Query) -> ApiResult {
    let status = match q.get("status") {
        None | Some("") => None,
        Some(v) => Some(
            ["queued", "retrying", "sent", "failed"]
                .contains(&v)
                .then(|| DeliveryStatus::parse(v))
                .ok_or_else(|| {
                    ApiError::bad_request("invalid_parameter", "status must be queued, retrying, sent or failed")
                })?,
        ),
    };
    let num = |name: &str| -> Result<Option<i64>, ApiError> {
        match q.get(name) {
            None | Some("") => Ok(None),
            Some(v) => v
                .parse::<i64>()
                .map(Some)
                .map_err(|_| ApiError::bad_request("invalid_parameter", format!("{name} must be an integer"))),
        }
    };
    let query = DeliveryQuery {
        status,
        to: q.get("to").map(str::to_owned),
        template: q.get("template").map(str::to_owned),
        limit: num("limit")?,
        before: num("before")?,
    };
    items(s.deliveries(&ctx, q.get("realm"), query).await.map_err(api)?)
}

/// One delivery-log row (never the message body).
#[utoipa::path(get, path = "/api/v1/customize/email/deliveries/{id}", tag = "customize",
    responses((status = 200, description = "Delivery", body = Delivery)))]
async fn delivery(State(s): State<CustomizeService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(s.delivery(&ctx, &id).await.map_err(api)?)
}

/// Queues a failed delivery again (while its body is still stored).
#[utoipa::path(post, path = "/api/v1/customize/email/deliveries/{id}/retry", tag = "customize",
    responses((status = 202, description = "Queued", body = Delivery)))]
async fn retry_delivery(State(s): State<CustomizeService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    Ok((
        StatusCode::ACCEPTED,
        Json(s.retry_delivery(&ctx, &id).await.map_err(api)?),
    )
        .into_response())
}

// ---------------------------------------------------------------------------
// Agreements
// ---------------------------------------------------------------------------

/// Every published version of a realm's agreements (administrators).
#[utoipa::path(get, path = "/api/v1/customize/agreements", tag = "customize",
    params(("realm" = Option<String>, Query, description = REALM),
           ("kind" = Option<String>, Query, description = "only this kind")),
    responses((status = 200, description = "Versions", body = Items<Agreement>)))]
async fn agreements(State(s): State<CustomizeService>, Access(ctx): Access, q: Query) -> ApiResult {
    items(s.agreements(&ctx, q.get("realm"), q.get("kind")).await.map_err(api)?)
}

/// Publishes the next version of an agreement. Every user of the realm must
/// accept it before using the API again (the re-accept gate).
#[utoipa::path(post, path = "/api/v1/customize/agreements", tag = "customize", request_body = AgreementInput,
    params(("realm" = Option<String>, Query, description = REALM)),
    responses((status = 201, description = "Published version", body = Agreement)))]
async fn publish_agreement(
    State(s): State<CustomizeService>,
    Access(ctx): Access,
    q: Query,
    b: Body<AgreementInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    created(s.publish_agreement(&ctx, q.get("realm"), input).await.map_err(api)?)
}

/// The current agreement versions the caller has not accepted.
#[utoipa::path(get, path = "/api/v1/customize/agreements/pending", tag = "customize",
    responses((status = 200, description = "Pending", body = Pending)))]
async fn pending(State(s): State<CustomizeService>, Access(ctx): Access) -> ApiResult {
    ok(Pending {
        pending: s.pending(&ctx).map_err(api)?,
    })
}

fn client_ip(parts: &Parts, trust_proxy: bool) -> Option<String> {
    if trust_proxy
        && let Some(f) = parts.headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(first) = f.split(',').next().map(str::trim).filter(|s| !s.is_empty())
    {
        return Some(first.to_owned());
    }
    parts
        .extensions
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|c| c.0.ip().to_string())
}

/// Records that the caller accepts the given (current) versions, with time,
/// address and client.
#[utoipa::path(post, path = "/api/v1/customize/agreements/accept", operation_id = "customize_accept_agreement", tag = "customize", request_body = AcceptInput,
    responses((status = 200, description = "Acceptance records", body = Items<Acceptance>),
              (status = 409, description = "agreement_outdated")))]
async fn accept(
    State(s): State<CustomizeService>,
    Access(ctx): Access,
    parts: Parts,
    b: Body<AcceptInput>,
) -> ApiResult {
    let input = body(&s, &ctx, b)?;
    let from = AcceptSource {
        ip: client_ip(&parts, s.config().trust_forwarded_host),
        user_agent: parts
            .headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    };
    items(s.accept(&ctx, input, from).await.map_err(api)?)
}

/// The caller's own acceptance records.
#[utoipa::path(get, path = "/api/v1/customize/agreements/accepted", tag = "customize",
    responses((status = 200, description = "Records", body = Items<Acceptance>)))]
async fn accepted(State(s): State<CustomizeService>, Access(ctx): Access) -> ApiResult {
    items(s.my_acceptances(&ctx).await.map_err(api)?)
}

/// Acceptance records of a realm (administrators).
#[utoipa::path(get, path = "/api/v1/customize/agreements/acceptances", tag = "customize",
    params(("realm" = Option<String>, Query, description = REALM),
           ("principal" = Option<String>, Query, description = "user:<collection>/<id>"),
           ("kind" = Option<String>, Query, description = "agreement kind"),
           ("limit" = Option<u32>, Query, description = "default 100, max 500")),
    responses((status = 200, description = "Records", body = Items<Acceptance>)))]
async fn acceptances(State(s): State<CustomizeService>, Access(ctx): Access, q: Query) -> ApiResult {
    let limit = match q.get("limit") {
        None | Some("") => None,
        Some(v) => Some(
            v.parse::<i64>()
                .map_err(|_| ApiError::bad_request("invalid_parameter", "limit must be an integer"))?,
        ),
    };
    let query = AcceptanceQuery {
        principal: q.get("principal").map(str::to_owned),
        kind: q.get("kind").map(str::to_owned),
        limit,
    };
    items(s.acceptances(&ctx, q.get("realm"), query).await.map_err(api)?)
}

// ---------------------------------------------------------------------------
// Public
// ---------------------------------------------------------------------------

/// The realm a public request describes.
async fn public_realm(s: &CustomizeService, ctx: &AccessContext, parts: &Parts, q: &Query) -> Result<String, ApiError> {
    let realm = if let Some(h) = parts.extensions.get::<ResolvedHost>() {
        h.realm.clone()
    } else if let Some(r) = q.get("realm").filter(|r| !r.is_empty()) {
        r.to_owned()
    } else {
        ctx.realm_id.clone()
    };
    if !s.realm_exists(&realm).await {
        return Err(ApiError::not_found("realm"));
    }
    Ok(realm)
}

/// What a dashboard or hosted page shows for a realm: name, colours, sign-in
/// text, logo and favicon URLs and verified domains. No credentials needed.
#[utoipa::path(get, path = "/api/v1/customize/public/branding", tag = "customize",
    params(("realm" = Option<String>, Query, description = "realm id; ignored on a verified custom domain")),
    responses((status = 200, description = "Branding", body = PublicBranding)))]
async fn public_branding(State(s): State<CustomizeService>, Access(ctx): Access, parts: Parts, q: Query) -> ApiResult {
    let realm = public_realm(&s, &ctx, &parts, &q).await?;
    ok(s.public_branding(&realm).await.map_err(api)?)
}

fn nosniff() -> [(HeaderName, HeaderValue); 2] {
    [
        (
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ),
        (header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300")),
    ]
}

/// The logo or favicon image. Served with `nosniff` and a sandboxing
/// Content-Security-Policy.
#[utoipa::path(get, path = "/api/v1/customize/public/branding/{kind}", tag = "customize",
    params(("kind" = String, Path, description = "logo or favicon"),
           ("realm" = Option<String>, Query, description = "realm id; ignored on a verified custom domain")),
    responses((status = 200, description = "The image"), (status = 304, description = "Not modified"), (status = 404, description = "None uploaded")))]
async fn public_asset(
    State(s): State<CustomizeService>,
    Access(ctx): Access,
    Path(kind): Path<String>,
    parts: Parts,
    q: Query,
) -> ApiResult {
    let realm = public_realm(&s, &ctx, &parts, &q).await?;
    let a = s.asset(&realm, asset_kind(&kind)?).await.map_err(api)?;
    let etag = format!("\"{}\"", a.etag);
    let mut res = if parts.headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        (StatusCode::OK, [(header::CONTENT_TYPE, a.content_type)], a.bytes).into_response()
    };
    let h = res.headers_mut();
    for (k, v) in nosniff() {
        h.insert(k, v);
    }
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
    );
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(header::ETAG, v);
    }
    Ok(res)
}

/// CSS custom properties (`--fb-primary`, `--fb-background`, `--fb-text`,
/// `--fb-accent`) with the realm's colours, to include from hosted pages.
#[utoipa::path(get, path = "/api/v1/customize/public/theme.css", tag = "customize",
    params(("realm" = Option<String>, Query, description = "realm id; ignored on a verified custom domain")),
    responses((status = 200, description = "CSS", content_type = "text/css")))]
async fn public_theme(State(s): State<CustomizeService>, Access(ctx): Access, parts: Parts, q: Query) -> ApiResult {
    let realm = public_realm(&s, &ctx, &parts, &q).await?;
    let b = s.public_branding(&realm).await.map_err(api)?;
    let mut res = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        theme_css(&b),
    )
        .into_response();
    for (k, v) in nosniff() {
        res.headers_mut().insert(k, v);
    }
    Ok(res)
}

/// The current version of every agreement of the realm, with text.
#[utoipa::path(get, path = "/api/v1/customize/public/agreements", tag = "customize",
    params(("realm" = Option<String>, Query, description = "realm id; ignored on a verified custom domain")),
    responses((status = 200, description = "Current versions", body = Items<Agreement>)))]
async fn public_agreements(
    State(s): State<CustomizeService>,
    Access(ctx): Access,
    parts: Parts,
    q: Query,
) -> ApiResult {
    let realm = public_realm(&s, &ctx, &parts, &q).await?;
    items(s.current_agreements(&realm).await.map_err(api)?)
}

// ---------------------------------------------------------------------------
// Docs page
// ---------------------------------------------------------------------------

const DOCS_SCRIPT: &str = r#"
(function () {
  var main = document.getElementById('ops');
  var q = document.getElementById('q');
  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined) e.textContent = text;
    return e;
  }
  fetch('/api/openapi.json', { headers: { accept: 'application/json' } })
    .then(function (r) { if (!r.ok) throw new Error('HTTP ' + r.status); return r.json(); })
    .then(function (doc) {
      document.getElementById('ver').textContent = (doc.info && (doc.info.title + ' ' + doc.info.version)) || '';
      var groups = {};
      Object.keys(doc.paths || {}).sort().forEach(function (p) {
        Object.keys(doc.paths[p]).forEach(function (m) {
          var op = doc.paths[p][m];
          if (!op || typeof op !== 'object' || !op.responses) return;
          var tag = (op.tags && op.tags[0]) || 'other';
          (groups[tag] = groups[tag] || []).push({ method: m, path: p, op: op });
        });
      });
      Object.keys(groups).sort().forEach(function (tag) {
        var sec = el('section', 'group');
        sec.appendChild(el('h2', '', tag));
        groups[tag].forEach(function (o) {
          var d = el('details', 'op');
          d.dataset.search = (o.method + ' ' + o.path + ' ' + (o.op.summary || '') + ' ' + (o.op.description || '')).toLowerCase();
          var s = el('summary');
          s.appendChild(el('span', 'm m-' + o.method, o.method.toUpperCase()));
          s.appendChild(el('code', '', o.path));
          s.appendChild(el('span', 'sum', (o.op.summary || (o.op.description || '').split('\n')[0] || '')));
          d.appendChild(s);
          if (o.op.description) d.appendChild(el('p', 'desc', o.op.description));
          var ps = (o.op.parameters || []);
          if (ps.length) {
            var ul = el('ul', 'params');
            ps.forEach(function (p) {
              ul.appendChild(el('li', '', p.name + ' (' + p.in + (p.required ? ', required' : '') + ')' + (p.description ? ': ' + p.description : '')));
            });
            d.appendChild(ul);
          }
          var rs = Object.keys(o.op.responses || {});
          if (rs.length) {
            var ul2 = el('ul', 'resp');
            rs.forEach(function (c) { ul2.appendChild(el('li', '', c + ' ' + ((o.op.responses[c] || {}).description || ''))); });
            d.appendChild(ul2);
          }
          sec.appendChild(d);
        });
        main.appendChild(sec);
      });
      q.addEventListener('input', function () {
        var t = q.value.toLowerCase();
        Array.prototype.forEach.call(main.querySelectorAll('details.op'), function (d) {
          d.hidden = t && d.dataset.search.indexOf(t) < 0;
        });
      });
    })
    .catch(function (e) { main.appendChild(el('p', 'err', 'Could not load the API description: ' + e.message)); });
})();
"#;

fn docs_page(b: &PublicBranding) -> String {
    let name = escape_html(&b.display_name);
    let realm_q: String = b
        .realm
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    let logo = b
        .logo_url
        .as_ref()
        .map(|u| {
            format!(
                "<img class=\"logo\" src=\"{}?realm={realm_q}\" alt=\"\">",
                escape_html(u)
            )
        })
        .unwrap_or_default();
    let favicon = b
        .favicon_url
        .as_ref()
        .map(|u| format!("<link rel=\"icon\" href=\"{}?realm={realm_q}\">", escape_html(u)))
        .unwrap_or_default();
    let support = b
        .support_email
        .as_ref()
        .map(|e| format!("<p class=\"foot\">Questions? {}</p>", escape_html(e)))
        .unwrap_or_default();
    let theme = theme_css(b);
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>{name} API</title>{favicon}<style>{theme}\
body{{margin:0;font:15px/1.5 system-ui,sans-serif;background:var(--fb-background,#fff);color:var(--fb-text,#1b1f24)}}\
header{{display:flex;align-items:center;gap:12px;padding:16px 24px;border-bottom:1px solid #8884}}\
.logo{{height:36px;width:auto}}h1{{font-size:20px;margin:0;color:var(--fb-primary,inherit)}}\
main{{max-width:960px;margin:0 auto;padding:16px 24px}}#q{{width:100%;box-sizing:border-box;padding:8px 10px;margin:8px 0 16px;font:inherit}}\
h2{{text-transform:capitalize;color:var(--fb-primary,inherit);border-bottom:1px solid #8884;padding-bottom:4px}}\
details.op{{border:1px solid #8884;border-radius:6px;margin:6px 0;padding:6px 10px}}summary{{cursor:pointer;display:flex;gap:10px;align-items:baseline;flex-wrap:wrap}}\
.m{{font:600 11px monospace;padding:2px 6px;border-radius:4px;background:var(--fb-accent,#555);color:#fff;min-width:44px;text-align:center}}\
.sum{{opacity:.75}}.desc{{white-space:pre-wrap}}.params,.resp{{margin:4px 0 8px 18px;padding:0}}.err{{color:#b00020}}.foot{{opacity:.7;font-size:13px}}\
</style></head><body><header>{logo}<h1>{name} API</h1><span id=\"ver\"></span>\
<a style=\"margin-left:auto\" href=\"/api/openapi.json\">openapi.json</a></header>\
<main><input id=\"q\" type=\"search\" placeholder=\"Filter operations\" aria-label=\"Filter operations\"><div id=\"ops\"></div>{support}</main>\
<script>{DOCS_SCRIPT}</script></body></html>"
    )
}

/// The public API documentation page, branded for the realm (name, logo,
/// colours): renders `/api/openapi.json` in the browser.
#[utoipa::path(get, path = "/api/v1/customize/docs", tag = "customize",
    params(("realm" = Option<String>, Query, description = "realm id; ignored on a verified custom domain")),
    responses((status = 200, description = "HTML page", content_type = "text/html")))]
async fn docs(State(s): State<CustomizeService>, Access(ctx): Access, parts: Parts, q: Query) -> ApiResult {
    let realm = public_realm(&s, &ctx, &parts, &q).await?;
    let b = s.public_branding(&realm).await.map_err(api)?;
    let mut res = (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        docs_page(&b),
    )
        .into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; img-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );
    h.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    Ok(res)
}
