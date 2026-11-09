//! HTTP integration: the authentication layer and the `/api/auth/*` routes.
//!
//! [`authenticate`] runs on every request (inside the request-id middleware):
//!
//! 1. Reads one credential, in this order: `Authorization: Bearer <access
//!    token | API key | PAT>`, `X-API-Key: <key>`, the `fb_session` cookie.
//! 2. A bearer token or key that does not verify is rejected with `401`
//!    (never silently downgraded). A stale session cookie is ignored and
//!    cleared, so a browser can always sign in again.
//! 3. API keys and PATs must hold a scope for the request (see [`crate::scopes`]),
//!    otherwise `403 insufficient_scope`.
//! 4. Cookie sessions must send `X-CSRF-Token` on unsafe methods (`403
//!    csrf_failed` otherwise).
//! 5. Inserts the data core's [`AccessContext`] (anonymous when there is no
//!    credential), [`Authenticated`] (when there is one) and [`ClientInfo`]
//!    as request extensions.
//!
//! Error bodies use the API envelope `{"error":{"code","message","request_id"}}`.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, FromRequest, FromRequestParts, Path, Request, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, COOKIE, RETRY_AFTER, SET_COOKIE, USER_AGENT, WWW_AUTHENTICATE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::AccessContext;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value as Json_, json};

use crate::scopes::required_scope;
use crate::service::{AuditQuery, ClientInfo, LoginOptions, LoginOutcome, SessionTokens};
use crate::{AuthError, AuthService, Authenticated, CredentialKind};

/// Session cookie name (HttpOnly).
pub const SESSION_COOKIE: &str = "fb_session";
/// CSRF cookie name (readable by scripts; its value goes in `X-CSRF-Token`).
pub const CSRF_COOKIE: &str = "fb_csrf";
/// Header carrying the CSRF token.
pub const CSRF_HEADER: &str = "x-csrf-token";
/// Alternative header for API keys.
pub const API_KEY_HEADER: &str = "x-api-key";

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// An [`AuthError`] as an HTTP response.
#[derive(Debug)]
pub struct HttpError {
    err: AuthError,
    request_id: String,
}

impl HttpError {
    pub(crate) fn new(err: AuthError, request_id: &str) -> Self {
        Self {
            err,
            request_id: request_id.to_owned(),
        }
    }
}

fn status_of(err: &AuthError) -> StatusCode {
    match err {
        AuthError::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
        AuthError::Mail(_) => StatusCode::SERVICE_UNAVAILABLE,
        AuthError::Provider(_) => StatusCode::BAD_GATEWAY,
        e => match e.kind() {
            ErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
            ErrorKind::Unauthenticated => StatusCode::UNAUTHORIZED,
            ErrorKind::Forbidden => StatusCode::FORBIDDEN,
            ErrorKind::NotFound => StatusCode::NOT_FOUND,
            ErrorKind::Conflict => StatusCode::CONFLICT,
            ErrorKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        },
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let status = status_of(&self.err);
        let message = if self.err.kind().is_client_visible() {
            self.err.to_string()
        } else {
            tracing::error!(error = %self.err, request_id = %self.request_id, "internal error in auth");
            "internal server error".to_owned()
        };
        let mut res = (
            status,
            Json(json!({"error": {"code": self.err.code(), "message": message, "request_id": self.request_id}})),
        )
            .into_response();
        let h = res.headers_mut();
        h.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let AuthError::RateLimited { retry_after } = self.err
            && let Ok(v) = HeaderValue::from_str(&retry_after.to_string())
        {
            h.insert(RETRY_AFTER, v);
        }
        if status == StatusCode::UNAUTHORIZED {
            let v = match self.err {
                AuthError::InvalidToken(_) => "Bearer error=\"invalid_token\"",
                _ => "Bearer",
            };
            h.insert(WWW_AUTHENTICATE, HeaderValue::from_static(v));
        }
        res
    }
}

pub(crate) fn request_id(headers: &HeaderMap) -> String {
    headers
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-")
        .to_owned()
}

// ---------------------------------------------------------------------------
// The layer
// ---------------------------------------------------------------------------

fn client_info(req_headers: &HeaderMap, connect: Option<SocketAddr>, trust_xff: bool) -> ClientInfo {
    let forwarded = trust_xff
        .then(|| req_headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()))
        .flatten()
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .filter(|ip| ip.parse::<std::net::IpAddr>().is_ok())
        .map(str::to_owned);
    ClientInfo {
        ip: forwarded.or_else(|| connect.map(|a| a.ip().to_string())),
        user_agent: req_headers
            .get(USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.chars().filter(|c| !c.is_control()).take(256).collect()),
        request_id: req_headers
            .get("x-request-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
        // An invalid realm id becomes "" (no such realm), never another realm.
        realm: req_headers.get(crate::realm::REALM_HEADER).map(|v| {
            v.to_str()
                .ok()
                .filter(|r| crate::realm::check_realm_id(r).is_ok())
                .unwrap_or("")
                .to_owned()
        }),
        turnstile_token: crate::turnstile::TOKEN_HEADERS
            .iter()
            .find_map(|h| req_headers.get(*h))
            .map(|v| v.to_str().unwrap_or("").chars().take(4096).collect()),
        impersonator: None,
    }
}

/// The value of cookie `name`, if sent.
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim().to_owned())
}

enum Credential {
    Bearer(String),
    ApiKey(String),
    Cookie(String),
}

fn credential(headers: &HeaderMap) -> Result<Option<Credential>, AuthError> {
    if let Some(v) = headers.get(AUTHORIZATION) {
        let v = v
            .to_str()
            .map_err(|_| AuthError::InvalidToken("malformed Authorization header"))?;
        let token = v
            .strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or(AuthError::InvalidToken("Authorization must be 'Bearer <token>'"))?;
        return Ok(Some(Credential::Bearer(token.to_owned())));
    }
    if let Some(v) = headers.get(API_KEY_HEADER) {
        let v = v
            .to_str()
            .map_err(|_| AuthError::InvalidToken("malformed X-API-Key header"))?;
        return Ok(Some(Credential::ApiKey(v.trim().to_owned())));
    }
    Ok(cookie_value(headers, SESSION_COOKIE)
        .filter(|c| !c.is_empty())
        .map(Credential::Cookie))
}

/// Middleware resolving the request's credential into an [`AccessContext`]
/// (and [`Authenticated`] when there is one). Install it with
/// `axum::middleware::from_fn_with_state(auth, authenticate)`.
pub async fn authenticate(State(auth): State<AuthService>, mut req: Request, next: Next) -> Response {
    let rid = request_id(req.headers());
    let connect = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
    let mut ci = client_info(req.headers(), connect, auth.config().trust_forwarded_for);
    let rid_opt = ci.request_id.clone();

    // The identity-provider protocol endpoints authenticate on their own
    // (client credentials, OAuth access tokens, the browser session with a
    // form CSRF token); they never see a resolved credential.
    let cred = if req.uri().path().starts_with("/api/auth/oidc/") {
        Ok(None)
    } else {
        credential(req.headers())
    };
    let cred = match cred {
        Ok(c) => c,
        Err(e) => return HttpError::new(e, &rid).into_response(),
    };
    let mut clear_cookie = false;
    let resolved = match cred {
        None => None,
        Some(Credential::Bearer(tok)) if AuthService::is_api_key(&tok) => Some(auth.authenticate_key(&tok).await),
        Some(Credential::Bearer(tok)) => Some(auth.authenticate_access_token(&tok).await),
        Some(Credential::ApiKey(k)) => Some(auth.authenticate_key(&k).await),
        Some(Credential::Cookie(c)) => match auth.authenticate_cookie(&c).await {
            Ok(a) => Some(Ok(a)),
            Err(AuthError::InvalidToken(_)) => {
                clear_cookie = true;
                None
            }
            Err(e) => Some(Err(e)),
        },
    };
    let ctx = match resolved {
        None => AccessContext::anonymous(),
        Some(Err(e)) => return HttpError::new(e, &rid).into_response(),
        Some(Ok(a)) => {
            if let Some(scopes) = &a.scopes
                && let Some(need) = required_scope(req.method(), req.uri().path())
                && !scopes.iter().any(|s| s.grants(&need))
            {
                return HttpError::new(
                    AuthError::Forbidden(format!("insufficient_scope: this request needs scope {need}")),
                    &rid,
                )
                .into_response();
            }
            if a.credential == CredentialKind::SessionCookie
                && !matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS)
            {
                let sent = req
                    .headers()
                    .get(CSRF_HEADER)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                let expected = auth.csrf_token(&a.credential_id);
                if !crate::crypto::ct_eq(sent.as_bytes(), expected.as_bytes()) {
                    return HttpError::new(
                        AuthError::Forbidden("csrf_failed: missing or wrong X-CSRF-Token".into()),
                        &rid,
                    )
                    .into_response();
                }
            }
            // The identity's home realm, unless the request selects one
            // (the policy tenancy layer refuses a foreign realm).
            let ctx = auth.access_context_for(&a);
            ci.impersonator.clone_from(&a.impersonator);
            req.extensions_mut().insert(a);
            ctx
        }
    };
    let ctx = match rid_opt {
        Some(id) => ctx.with_request_id(id),
        None => ctx,
    };
    req.extensions_mut().insert(ctx);
    req.extensions_mut().insert(ci);
    let mut res = next.run(req).await;
    if clear_cookie {
        let secure = auth.config().cookie_secure;
        for c in clear_cookies(secure) {
            res.headers_mut().append(SET_COOKIE, c);
        }
    }
    res
}

// ---------------------------------------------------------------------------
// Extractors
// ---------------------------------------------------------------------------

/// The authenticated caller; rejects anonymous requests with `401`.
#[derive(Debug, Clone)]
pub struct Caller(pub Authenticated);

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = HttpError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Authenticated>()
            .cloned()
            .map(Caller)
            .ok_or_else(|| HttpError::new(AuthError::Unauthenticated, &request_id(&parts.headers)))
    }
}

/// Client metadata (IP, user agent, request id).
#[derive(Debug, Clone)]
pub struct Client(pub ClientInfo, pub String);

impl<S: Send + Sync> FromRequestParts<S> for Client {
    type Rejection = HttpError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let rid = request_id(&parts.headers);
        let ci = parts.extensions.get::<ClientInfo>().cloned().unwrap_or_else(|| {
            let connect = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0);
            client_info(&parts.headers, connect, false)
        });
        Ok(Client(ci, rid))
    }
}

/// A JSON body whose rejections use the error envelope.
#[derive(Debug)]
pub struct Body<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = HttpError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let rid = request_id(req.headers());
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(Body(v)),
            Err(e) => Err(HttpError::new(AuthError::InvalidInput(e.body_text()), &rid)),
        }
    }
}

pub(crate) type Res = Result<Response, HttpError>;

pub(crate) fn ok(status: StatusCode, body: Json_) -> Response {
    let mut r = (status, Json(body)).into_response();
    r.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn cookie(name: &str, value: &str, max_age: u64, http_only: bool, secure: bool) -> Option<HeaderValue> {
    let mut c = format!("{name}={value}; Path=/; Max-Age={max_age}; SameSite=Lax");
    if http_only {
        c.push_str("; HttpOnly");
    }
    if secure {
        c.push_str("; Secure");
    }
    HeaderValue::from_str(&c).ok()
}

pub(crate) fn clear_cookies(secure: bool) -> Vec<HeaderValue> {
    [
        cookie(SESSION_COOKIE, "", 0, true, secure),
        cookie(CSRF_COOKIE, "", 0, false, secure),
    ]
    .into_iter()
    .flatten()
    .collect()
}

pub(crate) fn session_response(auth: &AuthService, tokens: &SessionTokens, extra: Option<(&str, Json_)>) -> Response {
    let mut body = serde_json::to_value(tokens).unwrap_or(Json_::Null);
    if let (Some((k, v)), Some(obj)) = (extra, body.as_object_mut()) {
        obj.insert(k.to_owned(), v);
    }
    let mut res = ok(StatusCode::OK, body);
    if let (Some(c), Some(csrf)) = (&tokens.cookie, &tokens.csrf_token) {
        let cfg = auth.config();
        let max_age = cfg.session_ttl.as_secs();
        for v in [
            cookie(SESSION_COOKIE, c, max_age, true, cfg.cookie_secure),
            cookie(CSRF_COOKIE, csrf, max_age, false, cfg.cookie_secure),
        ]
        .into_iter()
        .flatten()
        {
            res.headers_mut().append(SET_COOKIE, v);
        }
    }
    res
}

pub(crate) async fn login_response(auth: &AuthService, outcome: LoginOutcome, rid: &str) -> Res {
    match outcome {
        LoginOutcome::Session(tokens) => {
            let extra = match &tokens.subject {
                crate::Subject::User { collection, id } => Some((
                    "user",
                    Json_::Object(
                        auth.user_view(collection, id)
                            .await
                            .map_err(|e| HttpError::new(e, rid))?,
                    ),
                )),
                _ => None,
            };
            Ok(session_response(auth, &tokens, extra))
        }
        mfa @ LoginOutcome::MfaRequired { .. } => {
            Ok(ok(StatusCode::OK, serde_json::to_value(mfa).unwrap_or(Json_::Null)))
        }
    }
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct SignupReq {
    #[serde(default)]
    collection: Option<String>,
    email: String,
    password: String,
    #[serde(default)]
    data: Option<Map<String, Json_>>,
}

#[derive(Deserialize)]
struct LoginReq {
    #[serde(default)]
    collection: Option<String>,
    email: String,
    password: String,
    #[serde(flatten)]
    opts: LoginOptions,
}

#[derive(Deserialize)]
struct EmailReq {
    #[serde(default)]
    collection: Option<String>,
    email: String,
}

#[derive(Deserialize)]
struct TokenReq {
    token: String,
    #[serde(flatten)]
    opts: LoginOptions,
}

#[derive(Deserialize)]
struct ResetConfirmReq {
    token: String,
    password: String,
}

#[derive(Deserialize)]
struct OtpVerifyReq {
    #[serde(default)]
    collection: Option<String>,
    email: String,
    code: String,
    #[serde(flatten)]
    opts: LoginOptions,
}

#[derive(Deserialize)]
struct MfaVerifyReq {
    mfa_token: String,
    code: String,
}

#[derive(Deserialize)]
struct CodeReq {
    code: String,
}

#[derive(Deserialize)]
struct RefreshReq {
    refresh_token: String,
}

#[derive(Deserialize)]
struct ChangePasswordReq {
    current_password: String,
    new_password: String,
}

#[derive(Deserialize)]
struct KeyReq {
    name: String,
    scopes: Vec<String>,
    #[serde(default)]
    expires_in_days: Option<u32>,
}

#[derive(Deserialize)]
struct ServiceAccountReq {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default = "default_role")]
    role: String,
}

fn default_role() -> String {
    "service".into()
}

#[derive(Deserialize)]
struct AdminCreateReq {
    email: String,
    password: String,
}

#[derive(Deserialize)]
struct ImpersonateReq {
    principal: String,
    reason: String,
    #[serde(default)]
    ttl_minutes: Option<u32>,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// The `/api/auth/*` routes, ready to merge into the application router.
pub fn router<S: Clone + Send + Sync + 'static>(auth: AuthService) -> Router<S> {
    let r = Router::new()
        .route("/signup", post(signup))
        .route("/login", post(login))
        .route("/refresh", post(refresh))
        .route("/logout", post(logout))
        .route("/me", get(me))
        .route("/verify-email/request", post(verify_request))
        .route("/verify-email/confirm", post(verify_confirm))
        .route("/password-reset/request", post(reset_request))
        .route("/password-reset/confirm", post(reset_confirm))
        .route("/magic-link/request", post(magic_request))
        .route("/magic-link/confirm", post(magic_confirm))
        .route("/otp/request", post(otp_request))
        .route("/otp/verify", post(otp_verify))
        .route("/mfa/verify", post(mfa_verify))
        .route("/mfa/totp/setup", post(totp_setup))
        .route("/mfa/totp/confirm", post(totp_confirm))
        .route("/mfa/totp/disable", post(totp_disable))
        .route("/mfa/recovery-codes", post(recovery_codes))
        .route("/password/change", post(change_password))
        .route("/sessions", get(list_sessions))
        .route("/sessions/{id}", delete(revoke_session))
        .route("/login-history", get(login_history))
        .route("/tokens", get(list_pats).post(create_pat))
        .route("/tokens/{id}", delete(revoke_key))
        .route("/admins/login", post(admin_login))
        .route("/admins", get(list_admins).post(create_admin))
        .route("/service-accounts", get(list_sas).post(create_sa))
        .route("/service-accounts/{id}", get(get_sa).delete(delete_sa))
        .route("/service-accounts/{id}/keys", get(list_sa_keys).post(create_sa_key))
        .route("/api-keys/{id}", delete(revoke_key))
        .route("/api-keys", get(list_all_keys))
        .route("/users/{collection}/{id}", get(admin_user))
        .route("/users/{collection}/{id}/sessions", get(admin_user_sessions))
        .route(
            "/users/{collection}/{id}/sessions/revoke",
            post(admin_revoke_user_sessions),
        )
        .route("/users/{collection}/{id}/login-history", get(admin_user_history))
        .route("/impersonate", post(impersonate))
        .route("/impersonations", get(list_impersonations))
        .route("/impersonations/{id}/end", post(end_impersonation))
        .route("/audit", get(audit))
        .route("/keys/rotate", post(rotate_keys))
        .merge(crate::social::http::routes())
        .merge(crate::webauthn::http::routes())
        .merge(crate::identity_http::routes())
        .merge(crate::idp::http::routes());
    Router::new().nest("/api/auth", r.with_state(auth))
}

async fn signup(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<SignupReq>) -> Res {
    let user = a
        .signup(&ci, b.collection.as_deref(), &b.email, &b.password, b.data)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::CREATED, json!({"user": user})))
}

async fn login(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<LoginReq>) -> Res {
    let out = a
        .login_password(&ci, b.collection.as_deref(), &b.email, &b.password, &b.opts)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    login_response(&a, out, &rid).await
}

async fn admin_login(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<LoginReq>) -> Res {
    let out = a
        .login_admin(&ci, &b.email, &b.password, &b.opts)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    login_response(&a, out, &rid).await
}

async fn refresh(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<RefreshReq>) -> Res {
    let tokens = a
        .refresh(&b.refresh_token, &ci)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(session_response(&a, &tokens, None))
}

async fn logout(State(a): State<AuthService>, Client(ci, rid): Client, Caller(c): Caller) -> Res {
    a.logout(&c, &ci).await.map_err(|e| HttpError::new(e, &rid))?;
    let mut res = StatusCode::NO_CONTENT.into_response();
    if c.credential == CredentialKind::SessionCookie {
        for v in clear_cookies(a.config().cookie_secure) {
            res.headers_mut().append(SET_COOKIE, v);
        }
    }
    Ok(res)
}

async fn me(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller) -> Res {
    let v = a.whoami(&c).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, v))
}

fn accepted() -> Response {
    ok(
        StatusCode::ACCEPTED,
        json!({"status": "accepted", "message": "if the account exists, a message has been sent"}),
    )
}

async fn verify_request(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<EmailReq>) -> Res {
    a.request_verification(&ci, b.collection.as_deref(), &b.email)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(accepted())
}

async fn verify_confirm(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<TokenReq>) -> Res {
    let user = a
        .confirm_verification(&ci, &b.token)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"user": user})))
}

async fn reset_request(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<EmailReq>) -> Res {
    a.request_password_reset(&ci, b.collection.as_deref(), &b.email)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(accepted())
}

async fn reset_confirm(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<ResetConfirmReq>) -> Res {
    a.confirm_password_reset(&ci, &b.token, &b.password)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn magic_request(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<EmailReq>) -> Res {
    a.request_magic_link(&ci, b.collection.as_deref(), &b.email)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(accepted())
}

async fn magic_confirm(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<TokenReq>) -> Res {
    let out = a
        .login_magic_link(&ci, &b.token, &b.opts)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    login_response(&a, out, &rid).await
}

async fn otp_request(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<EmailReq>) -> Res {
    a.request_email_otp(&ci, b.collection.as_deref(), &b.email)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(accepted())
}

async fn otp_verify(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<OtpVerifyReq>) -> Res {
    let out = a
        .login_email_otp(&ci, b.collection.as_deref(), &b.email, &b.code, &b.opts)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    login_response(&a, out, &rid).await
}

async fn mfa_verify(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<MfaVerifyReq>) -> Res {
    let tokens = a
        .verify_mfa(&ci, &b.mfa_token, &b.code)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    login_response(&a, LoginOutcome::Session(Box::new(tokens)), &rid).await
}

async fn totp_setup(State(a): State<AuthService>, Client(ci, rid): Client, Caller(c): Caller) -> Res {
    let s = a.totp_setup(&c, &ci).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(s).unwrap_or(Json_::Null)))
}

async fn totp_confirm(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<CodeReq>,
) -> Res {
    let codes = a
        .totp_confirm(&c, &ci, &b.code)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"enabled": true, "recovery_codes": codes})))
}

async fn totp_disable(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<CodeReq>,
) -> Res {
    a.totp_disable(&c, &ci, &b.code)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"enabled": false})))
}

async fn recovery_codes(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<CodeReq>,
) -> Res {
    let codes = a
        .regenerate_recovery_codes(&c, &ci, &b.code)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"recovery_codes": codes})))
}

async fn change_password(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<ChangePasswordReq>,
) -> Res {
    a.change_password(&c, &ci, &b.current_password, &b.new_password)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn list_sessions(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller) -> Res {
    let s = a.list_sessions(&c).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"items": s})))
}

async fn revoke_session(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
) -> Res {
    a.revoke_session(&c, &id, &ci)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn login_history(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller) -> Res {
    let h = a.login_history(&c, 50).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"items": h})))
}

async fn list_pats(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller) -> Res {
    let k = a.list_pats(&c).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"items": k})))
}

async fn create_pat(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<KeyReq>,
) -> Res {
    let k = a
        .create_pat(&c, &ci, &b.name, &b.scopes, b.expires_in_days)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::CREATED, serde_json::to_value(k).unwrap_or(Json_::Null)))
}

async fn revoke_key(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
) -> Res {
    a.revoke_key(&c, &ci, &id).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn list_admins(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller) -> Res {
    crate::service::require_admin(&c).map_err(|e| HttpError::new(e, &rid))?;
    let list = a.list_admins().await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"items": list})))
}

async fn create_admin(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<AdminCreateReq>,
) -> Res {
    crate::service::require_admin(&c).map_err(|e| HttpError::new(e, &rid))?;
    let v = a
        .create_admin(&b.email, &b.password, &c.subject.actor(), &ci)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::CREATED, serde_json::to_value(v).unwrap_or(Json_::Null)))
}

async fn list_sas(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller) -> Res {
    let v = a.list_service_accounts(&c).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"items": v})))
}

async fn create_sa(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<ServiceAccountReq>,
) -> Res {
    let v = a
        .create_service_account(&c, &ci, &b.name, &b.description, &b.role)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::CREATED, serde_json::to_value(v).unwrap_or(Json_::Null)))
}

async fn get_sa(
    State(a): State<AuthService>,
    Client(_, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
) -> Res {
    let v = a
        .get_service_account(&c, &id)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(v).unwrap_or(Json_::Null)))
}

async fn delete_sa(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
) -> Res {
    a.delete_service_account(&c, &ci, &id)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn list_sa_keys(
    State(a): State<AuthService>,
    Client(_, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
) -> Res {
    let v = a
        .list_api_keys(&c, Some(&id))
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"items": v})))
}

async fn create_sa_key(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
    Body(b): Body<KeyReq>,
) -> Res {
    let k = a
        .create_api_key(&c, &ci, &id, &b.name, &b.scopes, b.expires_in_days)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::CREATED, serde_json::to_value(k).unwrap_or(Json_::Null)))
}

async fn list_all_keys(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller) -> Res {
    let v = a.list_all_keys(&c).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"items": v})))
}

async fn admin_user(
    State(a): State<AuthService>,
    Client(_, rid): Client,
    Caller(c): Caller,
    Path((collection, id)): Path<(String, String)>,
) -> Res {
    let v = a
        .admin_user_detail(&c, &collection, &id)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, v))
}

async fn admin_user_sessions(
    State(a): State<AuthService>,
    Client(_, rid): Client,
    Caller(c): Caller,
    Path((collection, id)): Path<(String, String)>,
) -> Res {
    let v = a
        .admin_user_sessions(&c, &collection, &id)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"items": v})))
}

async fn admin_revoke_user_sessions(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path((collection, id)): Path<(String, String)>,
) -> Res {
    let n = a
        .admin_revoke_user_sessions(&c, &ci, &collection, &id)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"revoked": n})))
}

async fn admin_user_history(
    State(a): State<AuthService>,
    Client(_, rid): Client,
    Caller(c): Caller,
    Path((collection, id)): Path<(String, String)>,
    uri: axum::http::Uri,
) -> Res {
    let limit = query_param(uri.query().unwrap_or(""), "limit")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(50);
    let v = a
        .admin_user_login_history(&c, &collection, &id, limit)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"items": v})))
}

async fn impersonate(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<ImpersonateReq>,
) -> Res {
    let g = a
        .impersonate(&c, &ci, &b.principal, &b.reason, b.ttl_minutes)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(g).unwrap_or(Json_::Null)))
}

async fn list_impersonations(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller) -> Res {
    let v = a.list_impersonations(&c).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"items": v})))
}

async fn end_impersonation(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
) -> Res {
    a.end_impersonation(&c, &ci, &id)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn query_param(q: &str, name: &str) -> Option<String> {
    q.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        (k == name).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn audit(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller, uri: axum::http::Uri) -> Res {
    let q = uri.query().unwrap_or("");
    let parse_num = |name: &str| -> Result<Option<i64>, HttpError> {
        query_param(q, name)
            .map(|v| {
                v.parse::<i64>()
                    .map_err(|_| HttpError::new(AuthError::invalid(format!("{name} must be a number")), &rid))
            })
            .transpose()
    };
    let query = AuditQuery {
        event: query_param(q, "event").filter(|s| !s.is_empty()),
        subject_id: query_param(q, "subject_id").filter(|s| !s.is_empty()),
        before: parse_num("before")?,
        limit: parse_num("limit")?.map(|v| v.clamp(1, 500) as u32),
    };
    let v = a.audit_log(&c, &query).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"items": v})))
}

async fn rotate_keys(State(a): State<AuthService>, Client(ci, rid): Client, Caller(c): Caller) -> Res {
    let kid = a
        .rotate_signing_key(&c, &ci)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"active_kid": kid})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_and_queries() {
        let mut h = HeaderMap::new();
        h.insert(COOKIE, HeaderValue::from_static("a=1; fb_session=abc; b=2"));
        assert_eq!(cookie_value(&h, SESSION_COOKIE).as_deref(), Some("abc"));
        assert_eq!(cookie_value(&h, "missing"), None);
        assert_eq!(
            query_param("event=login%2Efailed&limit=5", "event").as_deref(),
            Some("login.failed")
        );
        assert_eq!(query_param("limit=5", "limit").as_deref(), Some("5"));
        assert_eq!(percent_decode("a%2"), "a%2");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
