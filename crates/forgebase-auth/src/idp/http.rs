//! Routes of the identity provider.
//!
//! Protocol (`/api/auth/oidc/{realm}`; the auth layer resolves no credential
//! here, each endpoint authenticates on its own):
//! `/.well-known/openid-configuration`, `/jwks`, `/authorize` (GET, POST),
//! `/consent` (POST), `/token`, `/userinfo` (GET, POST), `/introspect`,
//! `/revoke`, `/device_authorization`, `/device` (GET, POST), `/logout`
//! (GET, POST).
//!
//! Management: `/api/auth/clients` (+ `/{id}`, `/{id}/secret`;
//! administrators), `/api/auth/consents` (+ `/{id}`; the signed-in user),
//! `/api/auth/keys/oidc/rotate` (administrators).

use std::collections::HashMap;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::header::{
    ACCEPT, AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, LOCATION, PRAGMA, SET_COOKIE, WWW_AUTHENTICATE,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use super::clients::ClientCreds;
use super::flow::{AuthorizeOutcome, ConsentPrompt, DeviceInfo, LogoutOutcome, SsoSession};
use super::{ClientInput, ClientPatch, GRANT_TYPES, OAuthError, esc};
use crate::http::{Body, CSRF_HEADER, Caller, Client, HttpError, Res, SESSION_COOKIE, clear_cookies, cookie_value, ok};
use crate::{AuthError, AuthService, crypto};

pub(crate) fn routes() -> Router<AuthService> {
    Router::new()
        .route("/oidc/{realm}/.well-known/openid-configuration", get(discovery))
        .route("/oidc/{realm}/jwks", get(jwks))
        .route("/oidc/{realm}/authorize", get(authorize_get).post(authorize_post))
        .route("/oidc/{realm}/consent", post(consent))
        .route("/oidc/{realm}/token", post(token))
        .route("/oidc/{realm}/userinfo", get(userinfo_get).post(userinfo_post))
        .route("/oidc/{realm}/introspect", post(introspect))
        .route("/oidc/{realm}/revoke", post(revoke))
        .route("/oidc/{realm}/device_authorization", post(device_authorization))
        .route("/oidc/{realm}/device", get(device_get).post(device_post))
        .route("/oidc/{realm}/logout", get(logout_get).post(logout_post))
        .route("/clients", get(list_clients).post(create_client))
        .route(
            "/clients/{id}",
            get(get_client).patch(update_client).delete(delete_client),
        )
        .route("/clients/{id}/secret", post(rotate_secret))
        .route("/consents", get(list_consents))
        .route("/consents/{id}", axum::routing::delete(revoke_consent))
        .route("/keys/oidc/rotate", post(rotate_keys))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_params(s: &[u8]) -> Result<HashMap<String, String>, OAuthError> {
    let mut m = HashMap::new();
    for (k, v) in url::form_urlencoded::parse(s) {
        if m.insert(k.into_owned(), v.into_owned()).is_some() {
            return Err(OAuthError::invalid_request("a parameter is repeated"));
        }
    }
    Ok(m)
}

fn is_form(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim_start().starts_with("application/x-www-form-urlencoded"))
}

fn is_json_body(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim_start().starts_with("application/json"))
}

/// A form body (protocol endpoints accept only `application/x-www-form-urlencoded`).
fn form(headers: &HeaderMap, body: &Bytes) -> Result<HashMap<String, String>, OAuthError> {
    if !is_form(headers) {
        return Err(OAuthError::invalid_request(
            "the body must be application/x-www-form-urlencoded",
        ));
    }
    parse_params(body)
}

/// A form or a flat JSON object of strings (browser pages and scripts).
fn form_or_json(headers: &HeaderMap, body: &Bytes) -> Result<HashMap<String, String>, OAuthError> {
    if is_json_body(headers) {
        let v: Json = serde_json::from_slice(body).map_err(|_| OAuthError::invalid_request("malformed JSON body"))?;
        let obj = v
            .as_object()
            .ok_or_else(|| OAuthError::invalid_request("the body must be a JSON object"))?;
        Ok(obj
            .iter()
            .filter_map(|(k, v)| match v {
                Json::String(s) => Some((k.clone(), s.clone())),
                Json::Bool(b) => Some((k.clone(), b.to_string())),
                _ => None,
            })
            .collect())
    } else {
        form(headers, body)
    }
}

fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get(ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("application/json"))
}

fn no_store(res: &mut Response) {
    let h = res.headers_mut();
    h.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(PRAGMA, HeaderValue::from_static("no-cache"));
}

fn oauth_json(status: StatusCode, body: Json) -> Response {
    let mut res = (status, axum::Json(body)).into_response();
    no_store(&mut res);
    res
}

fn oauth_error(e: &OAuthError) -> Response {
    let mut res = oauth_json(e.status_code(), json!(e));
    if e.status == 401 {
        let v = if e.error == "invalid_token" {
            "Bearer error=\"invalid_token\""
        } else {
            "Basic realm=\"forgebase\""
        };
        res.headers_mut().insert(WWW_AUTHENTICATE, HeaderValue::from_static(v));
    }
    if e.status == 403 && e.error == "insufficient_scope" {
        res.headers_mut().insert(
            WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer error=\"insufficient_scope\""),
        );
    }
    res
}

fn redirect(status: StatusCode, to: &str) -> Response {
    let mut res = status.into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        res.headers_mut().insert(LOCATION, v);
    }
    no_store(&mut res);
    res.headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    res
}

const CSS: &str = "body{font-family:system-ui,sans-serif;background:#f5f5f7;color:#1d1d1f;margin:0;padding:2rem 1rem}\
main{max-width:28rem;margin:0 auto;background:#fff;border-radius:12px;padding:1.5rem 1.75rem;box-shadow:0 1px 3px rgba(0,0,0,.12)}\
h1{font-size:1.3rem;margin:0 0 1rem}ul{padding-left:1.2rem}li{margin:.3rem 0}code{font-size:.85rem}\
.row{display:flex;gap:.75rem;margin-top:1.25rem}button{flex:1;padding:.65rem;border-radius:8px;border:1px solid #c7c7cc;background:#fff;font-size:1rem;cursor:pointer}\
button.primary{background:#0a66ff;border-color:#0a66ff;color:#fff}input[type=text]{width:100%;box-sizing:border-box;padding:.6rem;font-size:1.2rem;letter-spacing:.15em;text-transform:uppercase;border:1px solid #c7c7cc;border-radius:8px}\
.err{color:#b00020}.muted{color:#6e6e73;font-size:.9rem}";

fn page(status: StatusCode, title: &str, body: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{t}</title><style>{CSS}</style></head><body><main><h1>{t}</h1>{body}</main></body></html>",
        t = esc(title)
    );
    let mut res = (status, html).into_response();
    let h = res.headers_mut();
    h.insert(CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
    h.insert(
        "content-security-policy",
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    no_store(&mut res);
    res
}

fn error_page(e: &OAuthError) -> Response {
    page(
        e.status_code(),
        "Sign-in request refused",
        &format!(
            "<p class=\"err\"><code>{}</code></p><p>{}</p>",
            esc(e.error),
            esc(&e.error_description)
        ),
    )
}

fn hidden(name: &str, value: &str) -> String {
    format!(
        "<input type=\"hidden\" name=\"{}\" value=\"{}\">",
        esc(name),
        esc(value)
    )
}

fn scope_items(scopes: &[Json]) -> String {
    scopes
        .iter()
        .map(|s| {
            format!(
                "<li>{} <span class=\"muted\"><code>{}</code></span></li>",
                esc(s["description"].as_str().unwrap_or_default()),
                esc(s["scope"].as_str().unwrap_or_default())
            )
        })
        .collect()
}

fn consent_page(p: &ConsentPrompt) -> Response {
    let name = p.client["name"].as_str().unwrap_or_default();
    let body = format!(
        "<p><strong>{}</strong> wants to:</p><ul>{}</ul>\
         <form method=\"post\" action=\"{}\">{}{}<div class=\"row\">\
         <button type=\"submit\" name=\"decision\" value=\"deny\">Deny</button>\
         <button class=\"primary\" type=\"submit\" name=\"decision\" value=\"allow\">Allow</button></div></form>\
         <p class=\"muted\">You can withdraw this later in your account settings.</p>",
        esc(name),
        scope_items(&p.scopes),
        esc(&p.action),
        hidden("request_id", &p.request_id),
        hidden("csrf_token", &p.csrf_token),
    );
    page(StatusCode::OK, "Allow access?", &body)
}

fn client_ip_session_csrf(a: &AuthService, s: &SsoSession, p: &HashMap<String, String>, headers: &HeaderMap) -> bool {
    let sent = p
        .get("csrf_token")
        .map(String::as_str)
        .or_else(|| headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok()))
        .unwrap_or("");
    crypto::ct_eq(sent.as_bytes(), a.csrf_token(&s.id).as_bytes())
}

/// Client credentials from `Authorization: Basic` (RFC 6749 §2.3.1, form
/// encoded) or the `client_id` / `client_secret` form fields; one method only.
fn client_creds(headers: &HeaderMap, f: &HashMap<String, String>) -> Result<ClientCreds, OAuthError> {
    let decode = |s: &str| form_decode(s);
    let basic = match headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        Some(v) if v.len() > 6 && v[..6].eq_ignore_ascii_case("basic ") => {
            let raw = STANDARD
                .decode(v[6..].trim())
                .ok()
                .and_then(|b| String::from_utf8(b).ok())
                .ok_or_else(|| OAuthError::invalid_client("malformed Basic credentials"))?;
            let (id, secret) = raw
                .split_once(':')
                .ok_or_else(|| OAuthError::invalid_client("malformed Basic credentials"))?;
            Some((decode(id), decode(secret)))
        }
        Some(_) => return Err(OAuthError::invalid_client("unsupported client authentication")),
        None => None,
    };
    let form_id = f.get("client_id").filter(|s| !s.is_empty()).cloned();
    let form_secret = f.get("client_secret").filter(|s| !s.is_empty()).cloned();
    match basic {
        Some((id, secret)) => {
            if form_secret.is_some() {
                return Err(OAuthError::invalid_request("use one client authentication method"));
            }
            if form_id.as_ref().is_some_and(|f| *f != id) {
                return Err(OAuthError::invalid_client("client_id does not match the credentials"));
            }
            Ok(ClientCreds {
                id: Some(id),
                secret: Some(secret).filter(|s| !s.is_empty()),
            })
        }
        None => Ok(ClientCreds {
            id: form_id,
            secret: form_secret,
        }),
    }
}

/// `application/x-www-form-urlencoded` decoding of one value (`+` is a
/// space, `%XX` a byte); malformed escapes stay as they are.
fn form_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn check_realm(realm: &str) -> Result<(), OAuthError> {
    crate::realm::check_realm_id(realm).map_err(|_| OAuthError::invalid_request("invalid realm"))
}

fn into_oauth(e: AuthError) -> OAuthError {
    e.into()
}

// ---------------------------------------------------------------------------
// Discovery and keys
// ---------------------------------------------------------------------------

async fn discovery(State(a): State<AuthService>, Path(realm): Path<String>, Client(_, rid): Client) -> Res {
    crate::realm::check_realm_id(&realm).map_err(|e| HttpError::new(e, &rid))?;
    a.realm_identity(&realm).map_err(|e| HttpError::new(e, &rid))?;
    let iss = a.oidc_issuer(&realm);
    let doc = json!({
        "issuer": iss,
        "authorization_endpoint": format!("{iss}/authorize"),
        "token_endpoint": format!("{iss}/token"),
        "userinfo_endpoint": format!("{iss}/userinfo"),
        "jwks_uri": format!("{iss}/jwks"),
        "end_session_endpoint": format!("{iss}/logout"),
        "introspection_endpoint": format!("{iss}/introspect"),
        "revocation_endpoint": format!("{iss}/revoke"),
        "device_authorization_endpoint": format!("{iss}/device_authorization"),
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": GRANT_TYPES,
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["ES256"],
        "userinfo_signing_alg_values_supported": [],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
        "introspection_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
        "revocation_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
        "code_challenge_methods_supported": ["S256"],
        "scopes_supported": super::OIDC_SCOPES,
        "claims_supported": ["sub", "iss", "aud", "exp", "iat", "auth_time", "nonce", "acr", "amr", "azp", "at_hash", "sid",
            "email", "email_verified", "name", "preferred_username", "picture", "updated_at", "phone_number", "phone_number_verified"],
        "prompt_values_supported": ["none", "login", "consent", "select_account"],
        "acr_values_supported": [super::ACR_SINGLE, super::ACR_MULTI],
        "authorization_response_iss_parameter_supported": true,
        "request_parameter_supported": false,
        "request_uri_parameter_supported": false,
        "claims_parameter_supported": false,
        "frontchannel_logout_supported": false,
        "backchannel_logout_supported": false,
    });
    let mut res = (StatusCode::OK, axum::Json(doc)).into_response();
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
    Ok(res)
}

async fn jwks(State(a): State<AuthService>, Path(realm): Path<String>, Client(_, rid): Client) -> Res {
    crate::realm::check_realm_id(&realm).map_err(|e| HttpError::new(e, &rid))?;
    a.realm_identity(&realm).map_err(|e| HttpError::new(e, &rid))?;
    let keys = a.oidc_jwks(&realm).await.map_err(|e| HttpError::new(e, &rid))?;
    let mut res = (StatusCode::OK, axum::Json(keys)).into_response();
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
    res.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/jwk-set+json"));
    Ok(res)
}

// ---------------------------------------------------------------------------
// Authorization endpoint and consent
// ---------------------------------------------------------------------------

async fn authorize_common(
    a: &AuthService,
    realm: &str,
    raw: Result<HashMap<String, String>, OAuthError>,
    headers: &HeaderMap,
    ci: &crate::ClientInfo,
    rid: &str,
) -> Response {
    let json_wanted = wants_json(headers);
    let fail_page = |e: OAuthError| if json_wanted { oauth_error(&e) } else { error_page(&e) };
    if let Err(e) = check_realm(realm) {
        return fail_page(e);
    }
    let raw = match raw {
        Ok(r) => r,
        Err(e) => return fail_page(e),
    };
    let cookie = cookie_value(headers, SESSION_COOKIE);
    match a.authorize(realm, &raw, cookie.as_deref(), ci).await {
        Ok(AuthorizeOutcome::Redirect(u)) => redirect(StatusCode::FOUND, &u),
        Ok(AuthorizeOutcome::Consent(p)) if json_wanted => oauth_json(StatusCode::OK, json!(p)),
        Ok(AuthorizeOutcome::Consent(p)) => consent_page(&p),
        Ok(AuthorizeOutcome::ErrorPage(e)) => fail_page(e),
        Err(e @ AuthError::NotFound(_)) => HttpError::new(e, rid).into_response(),
        Err(e) => fail_page(e.into()),
    }
}

async fn authorize_get(
    State(a): State<AuthService>,
    Path(realm): Path<String>,
    Client(ci, rid): Client,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let raw = parse_params(uri.query().unwrap_or("").as_bytes());
    authorize_common(&a, &realm, raw, &headers, &ci, &rid).await
}

async fn authorize_post(
    State(a): State<AuthService>,
    Path(realm): Path<String>,
    Client(ci, rid): Client,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let raw = form(&headers, &body);
    authorize_common(&a, &realm, raw, &headers, &ci, &rid).await
}

async fn consent(
    State(a): State<AuthService>,
    Path(realm): Path<String>,
    Client(ci, rid): Client,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let json_wanted = wants_json(&headers) || is_json_body(&headers);
    let fail = |e: OAuthError| if json_wanted { oauth_error(&e) } else { error_page(&e) };
    if let Err(e) = check_realm(&realm) {
        return fail(e);
    }
    let p = match form_or_json(&headers, &body) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let cookie = cookie_value(&headers, SESSION_COOKIE);
    let session = match a.sso_session(&realm, cookie.as_deref()).await {
        Ok(Some(s)) => s,
        Ok(None) => return fail(OAuthError::login_required("sign in again to answer this request")),
        Err(e) => return HttpError::new(e, &rid).into_response(),
    };
    if !client_ip_session_csrf(&a, &session, &p, &headers) {
        return HttpError::new(
            AuthError::Forbidden("csrf_failed: missing or wrong csrf_token".into()),
            &rid,
        )
        .into_response();
    }
    let allow = match p.get("decision").map(String::as_str) {
        Some("allow") => true,
        Some("deny") => false,
        _ => return fail(OAuthError::invalid_request("decision must be allow or deny")),
    };
    let Some(request_id) = p.get("request_id") else {
        return fail(OAuthError::invalid_request("request_id is required"));
    };
    match a.consent_decision(&realm, request_id, allow, &session, &ci).await {
        Ok(AuthorizeOutcome::Redirect(u)) if json_wanted => oauth_json(StatusCode::OK, json!({"redirect_to": u})),
        Ok(AuthorizeOutcome::Redirect(u)) => redirect(StatusCode::SEE_OTHER, &u),
        Ok(AuthorizeOutcome::Consent(p)) => consent_page(&p),
        Ok(AuthorizeOutcome::ErrorPage(e)) => fail(e),
        Err(e) => HttpError::new(e, &rid).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Token, userinfo, introspection, revocation, device authorization
// ---------------------------------------------------------------------------

async fn token(
    State(a): State<AuthService>,
    Path(realm): Path<String>,
    Client(ci, _): Client,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let run = async {
        check_realm(&realm)?;
        let f = form(&headers, &body)?;
        let creds = client_creds(&headers, &f)?;
        a.token_endpoint(&realm, &f, &creds, &ci).await
    };
    match run.await {
        Ok(j) => oauth_json(StatusCode::OK, j),
        Err(e) => oauth_error(&e),
    }
}

async fn userinfo_common(
    a: &AuthService,
    realm: &str,
    headers: &HeaderMap,
    f: Option<&HashMap<String, String>>,
) -> Response {
    let run = async {
        check_realm(realm)?;
        let header_token = headers
            .get(AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
            .map(str::trim)
            .filter(|t| !t.is_empty());
        let form_token = f.and_then(|f| f.get("access_token")).map(String::as_str);
        let tok = match (header_token, form_token) {
            (Some(_), Some(_)) => return Err(OAuthError::invalid_request("send the access token once")),
            (Some(t), None) | (None, Some(t)) => t,
            (None, None) => return Err(OAuthError::invalid_token("an access token is required")),
        };
        a.userinfo(realm, tok).await
    };
    match run.await {
        Ok(j) => oauth_json(StatusCode::OK, j),
        Err(e) => oauth_error(&e),
    }
}

async fn userinfo_get(State(a): State<AuthService>, Path(realm): Path<String>, headers: HeaderMap) -> Response {
    userinfo_common(&a, &realm, &headers, None).await
}

async fn userinfo_post(
    State(a): State<AuthService>,
    Path(realm): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let f = if body.is_empty() {
        HashMap::new()
    } else {
        match form(&headers, &body) {
            Ok(f) => f,
            Err(e) => return oauth_error(&e),
        }
    };
    userinfo_common(&a, &realm, &headers, Some(&f)).await
}

async fn introspect(
    State(a): State<AuthService>,
    Path(realm): Path<String>,
    Client(ci, _): Client,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let run = async {
        check_realm(&realm)?;
        let f = form(&headers, &body)?;
        let creds = client_creds(&headers, &f)?;
        let client = a.authenticate_client(&realm, &creds, &ci).await?;
        let tok = f
            .get("token")
            .filter(|t| !t.is_empty())
            .ok_or_else(|| OAuthError::invalid_request("token is required"))?;
        a.introspect(&realm, &client, tok, f.get("token_type_hint").map(String::as_str))
            .await
    };
    match run.await {
        Ok(j) => oauth_json(StatusCode::OK, j),
        Err(e) => oauth_error(&e),
    }
}

async fn revoke(
    State(a): State<AuthService>,
    Path(realm): Path<String>,
    Client(ci, _): Client,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let run = async {
        check_realm(&realm)?;
        let f = form(&headers, &body)?;
        let creds = client_creds(&headers, &f)?;
        let client = a.authenticate_client(&realm, &creds, &ci).await?;
        let tok = f
            .get("token")
            .filter(|t| !t.is_empty())
            .ok_or_else(|| OAuthError::invalid_request("token is required"))?;
        a.revoke_token(&realm, &client, tok, &ci).await
    };
    match run.await {
        Ok(()) => {
            let mut res = StatusCode::OK.into_response();
            no_store(&mut res);
            res
        }
        Err(e) => oauth_error(&e),
    }
}

async fn device_authorization(
    State(a): State<AuthService>,
    Path(realm): Path<String>,
    Client(ci, _): Client,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let run = async {
        check_realm(&realm)?;
        let f = form(&headers, &body)?;
        let creds = client_creds(&headers, &f)?;
        let client = a.authenticate_client(&realm, &creds, &ci).await?;
        a.device_authorize(&realm, &client, f.get("scope").map(String::as_str))
            .await
    };
    match run.await {
        Ok(d) => oauth_json(StatusCode::OK, json!(d)),
        Err(e) => oauth_error(&e),
    }
}

// ---------------------------------------------------------------------------
// Device verification page (RFC 8628 §3.3)
// ---------------------------------------------------------------------------

fn device_input_page(a: &AuthService, realm: &str, error: Option<&str>) -> Response {
    let err = error.map_or(String::new(), |e| format!("<p class=\"err\">{}</p>", esc(e)));
    let body = format!(
        "{err}<p>Enter the code shown on your device.</p>\
         <form method=\"get\" action=\"{}/device\"><input type=\"text\" name=\"user_code\" autocomplete=\"off\" \
         autofocus required maxlength=\"9\" placeholder=\"XXXX-XXXX\"><div class=\"row\">\
         <button class=\"primary\" type=\"submit\">Continue</button></div></form>",
        esc(&a.oidc_issuer(realm))
    );
    page(
        if error.is_some() {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::OK
        },
        "Connect a device",
        &body,
    )
}

fn device_confirm_page(a: &AuthService, realm: &str, s: &SsoSession, d: &DeviceInfo) -> Response {
    let body = format!(
        "<p>Code <code>{}</code>: <strong>{}</strong> wants to:</p><ul>{}</ul>\
         <p class=\"muted\">Only continue if this code is shown on a device you are setting up.</p>\
         <form method=\"post\" action=\"{}/device\">{}{}<div class=\"row\">\
         <button type=\"submit\" name=\"decision\" value=\"deny\">Deny</button>\
         <button class=\"primary\" type=\"submit\" name=\"decision\" value=\"allow\">Allow</button></div></form>",
        esc(&d.user_code),
        esc(d.client["name"].as_str().unwrap_or_default()),
        scope_items(&d.scopes),
        esc(&a.oidc_issuer(realm)),
        hidden("user_code", &d.user_code),
        hidden("csrf_token", &a.csrf_token(&s.id)),
    );
    page(StatusCode::OK, "Connect a device", &body)
}

async fn device_get(
    State(a): State<AuthService>,
    Path(realm): Path<String>,
    Client(ci, rid): Client,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let json_wanted = wants_json(&headers);
    if let Err(e) = check_realm(&realm) {
        return error_page(&e);
    }
    if let Err(e) = a.realm_identity(&realm) {
        return HttpError::new(e, &rid).into_response();
    }
    let q = parse_params(uri.query().unwrap_or("").as_bytes()).unwrap_or_default();
    let user_code = q.get("user_code").filter(|c| !c.is_empty());
    let cookie = cookie_value(&headers, SESSION_COOKIE);
    let session = match a.sso_session(&realm, cookie.as_deref()).await {
        Ok(s) => s,
        Err(e) => return HttpError::new(e, &rid).into_response(),
    };
    let Some(session) = session else {
        let mut back = format!("{}/device", a.oidc_issuer(&realm));
        if let Some(c) = user_code {
            back = super::flow::with_params(&back, &[("user_code", c)]);
        }
        return redirect(StatusCode::FOUND, &a.login_redirect(&realm, &back));
    };
    let Some(code) = user_code else {
        return device_input_page(&a, &realm, None);
    };
    match a.device_lookup(&realm, code, &ci).await {
        Ok(Some(d)) if json_wanted => oauth_json(
            StatusCode::OK,
            json!({"user_code": d.user_code, "client": d.client, "scopes": d.scopes, "csrf_token": a.csrf_token(&session.id)}),
        ),
        Ok(Some(d)) => device_confirm_page(&a, &realm, &session, &d),
        Ok(None) if json_wanted => oauth_error(&OAuthError::invalid_grant("unknown or expired code")),
        Ok(None) => device_input_page(&a, &realm, Some("That code is unknown or has expired.")),
        Err(e) => HttpError::new(e, &rid).into_response(),
    }
}

async fn device_post(
    State(a): State<AuthService>,
    Path(realm): Path<String>,
    Client(ci, rid): Client,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let json_wanted = wants_json(&headers) || is_json_body(&headers);
    let fail = |e: OAuthError| if json_wanted { oauth_error(&e) } else { error_page(&e) };
    if let Err(e) = check_realm(&realm) {
        return fail(e);
    }
    let p = match form_or_json(&headers, &body) {
        Ok(p) => p,
        Err(e) => return fail(e),
    };
    let cookie = cookie_value(&headers, SESSION_COOKIE);
    let session = match a.sso_session(&realm, cookie.as_deref()).await {
        Ok(Some(s)) => s,
        Ok(None) => return fail(OAuthError::login_required("sign in to approve a device")),
        Err(e) => return HttpError::new(e, &rid).into_response(),
    };
    if !client_ip_session_csrf(&a, &session, &p, &headers) {
        return HttpError::new(
            AuthError::Forbidden("csrf_failed: missing or wrong csrf_token".into()),
            &rid,
        )
        .into_response();
    }
    let allow = match p.get("decision").map(String::as_str) {
        Some("allow") => true,
        Some("deny") => false,
        _ => return fail(OAuthError::invalid_request("decision must be allow or deny")),
    };
    let code = p.get("user_code").map(String::as_str).unwrap_or_default();
    match a.device_decide(&realm, code, allow, &session, &ci).await {
        Ok(true) if json_wanted => oauth_json(StatusCode::OK, json!({"approved": allow})),
        Ok(true) => page(
            StatusCode::OK,
            if allow { "Device connected" } else { "Request denied" },
            if allow {
                "<p>You can return to your device.</p>"
            } else {
                "<p>The device was not given access.</p>"
            },
        ),
        Ok(false) if json_wanted => oauth_error(&OAuthError::invalid_grant("unknown or expired code")),
        Ok(false) => device_input_page(&a, &realm, Some("That code is unknown or has expired.")),
        Err(e) => HttpError::new(e, &rid).into_response(),
    }
}

// ---------------------------------------------------------------------------
// RP-initiated logout
// ---------------------------------------------------------------------------

async fn logout_common(
    a: &AuthService,
    realm: &str,
    raw: Result<HashMap<String, String>, OAuthError>,
    headers: &HeaderMap,
    post: bool,
    ci: &crate::ClientInfo,
    rid: &str,
) -> Response {
    if let Err(e) = check_realm(realm) {
        return error_page(&e);
    }
    let raw = match raw {
        Ok(r) => r,
        Err(e) => return error_page(&e),
    };
    let cookie = cookie_value(headers, SESSION_COOKIE);
    let confirmed = post
        && match a.sso_session(realm, cookie.as_deref()).await {
            Ok(Some(s)) => {
                client_ip_session_csrf(a, &s, &raw, headers) && raw.get("confirm").is_some_and(|c| c == "yes")
            }
            Ok(None) => false,
            Err(e) => return HttpError::new(e, rid).into_response(),
        };
    match a.rp_logout(realm, &raw, cookie.as_deref(), confirmed, ci).await {
        Ok(LogoutOutcome::Confirm { csrf_token }) => {
            let fields: String = ["id_token_hint", "client_id", "post_logout_redirect_uri", "state"]
                .iter()
                .filter_map(|k| raw.get(*k).map(|v| hidden(k, v)))
                .collect();
            let body = format!(
                "<p>Do you want to sign out?</p><form method=\"post\" action=\"{}/logout\">{fields}{}{}\
                 <div class=\"row\"><button class=\"primary\" type=\"submit\">Sign out</button></div></form>",
                esc(&a.oidc_issuer(realm)),
                hidden("csrf_token", &csrf_token),
                hidden("confirm", "yes"),
            );
            page(StatusCode::OK, "Sign out", &body)
        }
        Ok(LogoutOutcome::Done { redirect: to, ended }) => {
            let mut res = match to {
                Some(u) => redirect(if post { StatusCode::SEE_OTHER } else { StatusCode::FOUND }, &u),
                None => page(StatusCode::OK, "Signed out", "<p>You are signed out.</p>"),
            };
            if ended {
                for c in clear_cookies(a.config().cookie_secure) {
                    res.headers_mut().append(SET_COOKIE, c);
                }
            }
            res
        }
        Ok(LogoutOutcome::ErrorPage(e)) => error_page(&e),
        Err(e @ AuthError::NotFound(_)) => HttpError::new(e, rid).into_response(),
        Err(e) => error_page(&into_oauth(e)),
    }
}

async fn logout_get(
    State(a): State<AuthService>,
    Path(realm): Path<String>,
    Client(ci, rid): Client,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let raw = parse_params(uri.query().unwrap_or("").as_bytes());
    logout_common(&a, &realm, raw, &headers, false, &ci, &rid).await
}

async fn logout_post(
    State(a): State<AuthService>,
    Path(realm): Path<String>,
    Client(ci, rid): Client,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let raw = form_or_json(&headers, &body);
    logout_common(&a, &realm, raw, &headers, true, &ci, &rid).await
}

// ---------------------------------------------------------------------------
// Management
// ---------------------------------------------------------------------------

async fn create_client(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<ClientInput>,
) -> Res {
    let out = a.create_client(&c, &ci, b).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::CREATED, json!(out)))
}

async fn list_clients(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller, uri: Uri) -> Res {
    let q = parse_params(uri.query().unwrap_or("").as_bytes()).unwrap_or_default();
    let list = a
        .list_clients(&c, q.get("realm").map(String::as_str))
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"items": list})))
}

async fn get_client(
    State(a): State<AuthService>,
    Client(_, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
) -> Res {
    let v = a.get_client(&c, &id).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!(v)))
}

async fn update_client(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
    Body(b): Body<ClientPatch>,
) -> Res {
    let v = a
        .update_client(&c, &ci, &id, b)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!(v)))
}

async fn delete_client(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
) -> Res {
    a.delete_client(&c, &ci, &id)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Debug, Default, Deserialize)]
struct RotateReq {
    #[serde(default)]
    grace_seconds: Option<i64>,
}

async fn rotate_secret(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
    body: Bytes,
) -> Res {
    let req: RotateReq = if body.is_empty() {
        RotateReq::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| HttpError::new(AuthError::invalid(e.to_string()), &rid))?
    };
    let out = a
        .rotate_client_secret(&c, &ci, &id, req.grace_seconds)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!(out)))
}

async fn list_consents(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller) -> Res {
    let list = a.list_consents(&c).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"items": list})))
}

async fn revoke_consent(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
) -> Res {
    a.revoke_consent(&c, &ci, &id)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Debug, Deserialize)]
struct KeyRotateReq {
    #[serde(default)]
    realm: Option<String>,
}

async fn rotate_keys(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<KeyRotateReq>,
) -> Res {
    let realm = b
        .realm
        .or_else(|| ci.realm.clone())
        .unwrap_or_else(|| forgebase_data::DEFAULT_REALM.to_owned());
    let kid = a
        .rotate_oidc_key(&c, &realm, &ci)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"realm": realm, "kid": kid})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_parameters_are_refused() {
        assert!(parse_params(b"a=1&b=2").is_ok());
        assert_eq!(parse_params(b"a=1&a=2").unwrap_err().error, "invalid_request");
    }

    #[test]
    fn basic_client_credentials_are_form_decoded() {
        let mut h = HeaderMap::new();
        let v = STANDARD.encode("my%20client:s%3Acret");
        h.insert(AUTHORIZATION, HeaderValue::from_str(&format!("Basic {v}")).unwrap());
        let c = client_creds(&h, &HashMap::new()).unwrap();
        assert_eq!(c.id.as_deref(), Some("my client"));
        assert_eq!(c.secret.as_deref(), Some("s:cret"));
        let mut f = HashMap::new();
        f.insert("client_secret".to_owned(), "x".to_owned());
        assert!(client_creds(&h, &f).is_err(), "two methods");
    }
}
