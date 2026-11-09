//! `/api/auth/oauth/*` routes.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::header::{CONTENT_TYPE, LOCATION};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde::Deserialize;
use serde_json::json;

use super::flow::{OAuthCallback, OAuthOutcome};
use crate::http::{Caller, Client, HttpError, Res, login_response, ok};
use crate::service::LoginOptions;
use crate::{AuthError, AuthService};

/// The routes (merged under `/api/auth` by the parent router).
pub(crate) fn routes() -> Router<AuthService> {
    Router::new()
        .route("/oauth/providers", get(providers))
        .route("/oauth/identities", get(identities))
        .route("/oauth/identities/{id}", delete(unlink))
        .route("/oauth/{provider}/start", get(start_redirect).post(start))
        .route("/oauth/{provider}/link", post(link))
        .route("/oauth/{provider}/callback", get(callback_get).post(callback_post))
}

#[derive(Deserialize, Default)]
struct StartReq {
    #[serde(default)]
    collection: Option<String>,
    #[serde(flatten)]
    opts: LoginOptions,
}

async fn providers(State(a): State<AuthService>, Client(ci, _): Client) -> Response {
    let realm = ci.realm.as_deref().unwrap_or(crate::social::DEFAULT_REALM);
    ok(StatusCode::OK, json!({"providers": a.oauth_providers_in(realm)}))
}

async fn start(State(a): State<AuthService>, Client(ci, rid): Client, Path(p): Path<String>, body: Bytes) -> Res {
    let b: StartReq = if body.is_empty() {
        StartReq::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| HttpError::new(AuthError::invalid(e.to_string()), &rid))?
    };
    let s = a
        .oauth_start(&ci, &p, b.collection.as_deref(), &b.opts)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(s).unwrap_or_default()))
}

#[derive(Deserialize, Default)]
struct StartQuery {
    #[serde(default)]
    collection: Option<String>,
    #[serde(default)]
    cookie: bool,
    #[serde(default)]
    device_name: Option<String>,
}

/// Browser entry point: redirects to the provider.
async fn start_redirect(
    State(a): State<AuthService>,
    Client(mut ci, rid): Client,
    Path(p): Path<String>,
    uri: Uri,
) -> Res {
    let mut q = StartQuery::default();
    for (k, v) in url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes()) {
        match &*k {
            "collection" => q.collection = Some(v.into_owned()),
            // A browser navigation cannot send X-ForgeBase-Realm.
            "realm" if ci.realm.is_none() => ci.realm = Some(v.into_owned()),
            "cookie" => q.cookie = v == "true" || v == "1",
            "device_name" => q.device_name = Some(v.into_owned()),
            _ => {}
        }
    }
    let opts = LoginOptions {
        cookie: q.cookie,
        device_name: q.device_name,
    };
    let s = a
        .oauth_start(&ci, &p, q.collection.as_deref(), &opts)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    let mut res = StatusCode::FOUND.into_response();
    if let Ok(v) = s.authorization_url.parse() {
        res.headers_mut().insert(LOCATION, v);
    }
    Ok(res)
}

async fn link(State(a): State<AuthService>, Client(ci, rid): Client, Caller(c): Caller, Path(p): Path<String>) -> Res {
    let s = a
        .oauth_link_start(&c, &ci, &p)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(s).unwrap_or_default()))
}

#[derive(Deserialize, Default)]
struct CallbackParams {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn finish(a: AuthService, ci: crate::ClientInfo, rid: String, p: String, cp: CallbackParams) -> Res {
    let cb = OAuthCallback {
        code: cp.code,
        state: cp.state,
        error: cp.error,
    };
    match a
        .oauth_callback(&ci, &p, &cb)
        .await
        .map_err(|e| HttpError::new(e, &rid))?
    {
        OAuthOutcome::Login(out) => login_response(&a, out, &rid).await,
        OAuthOutcome::Linked(identity) => Ok(ok(StatusCode::OK, json!({"linked": true, "identity": identity}))),
    }
}

async fn callback_get(State(a): State<AuthService>, Client(ci, rid): Client, Path(p): Path<String>, uri: Uri) -> Res {
    let pairs = url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
        .into_owned()
        .collect();
    finish(a, ci, rid, p, params_from(pairs)).await
}

fn params_from(pairs: Vec<(String, String)>) -> CallbackParams {
    let mut cp = CallbackParams::default();
    for (k, v) in pairs {
        match k.as_str() {
            "code" => cp.code = Some(v),
            "state" => cp.state = Some(v),
            "error" => cp.error = Some(v),
            _ => {}
        }
    }
    cp
}

/// `POST` callback: JSON (single-page apps) or a form (Apple `form_post`).
async fn callback_post(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Path(p): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Res {
    let is_json = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    let cp = if is_json {
        serde_json::from_slice::<CallbackParams>(&body)
            .map_err(|e| HttpError::new(AuthError::invalid(e.to_string()), &rid))?
    } else {
        params_from(url::form_urlencoded::parse(&body).into_owned().collect())
    };
    finish(a, ci, rid, p, cp).await
}

async fn identities(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller) -> Res {
    let v = a.oauth_identities(&c).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"identities": v})))
}

async fn unlink(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
) -> Res {
    a.oauth_unlink(&c, &ci, &id)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
