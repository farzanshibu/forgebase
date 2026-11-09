//! `/api/auth/webauthn/*` routes.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use serde::Deserialize;
use serde_json::json;

use super::{AuthenticationCredential, RegistrationCredential};
use crate::AuthService;
use crate::http::{Body, Caller, Client, HttpError, Res, ok, session_response};
use crate::service::LoginOptions;

pub(crate) fn routes() -> Router<AuthService> {
    Router::new()
        .route("/webauthn/register/options", post(register_options))
        .route("/webauthn/register/verify", post(register_verify))
        .route("/webauthn/login/options", post(login_options))
        .route("/webauthn/login/verify", post(login_verify))
        .route("/webauthn/mfa/options", post(mfa_options))
        .route("/webauthn/mfa/verify", post(mfa_verify))
        .route("/webauthn/credentials", get(list))
        .route("/webauthn/credentials/{id}", delete(remove))
}

#[derive(Deserialize, Default)]
struct RegisterOptionsReq {
    #[serde(default)]
    use_as_mfa: bool,
}

async fn register_options(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<RegisterOptionsReq>,
) -> Res {
    let o = a
        .webauthn_register_options(&c, &ci, b.use_as_mfa)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(o).unwrap_or_default()))
}

#[derive(Deserialize)]
struct RegisterVerifyReq {
    ceremony_id: String,
    #[serde(default)]
    name: Option<String>,
    credential: RegistrationCredential,
}

async fn register_verify(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<RegisterVerifyReq>,
) -> Res {
    let v = a
        .webauthn_register_verify(&c, &ci, &b.ceremony_id, b.name.as_deref(), &b.credential)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::CREATED, json!({"credential": v})))
}

#[derive(Deserialize, Default)]
struct LoginOptionsReq {
    #[serde(default)]
    collection: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

async fn login_options(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<LoginOptionsReq>) -> Res {
    let o = a
        .webauthn_login_options(&ci, b.collection.as_deref(), b.email.as_deref())
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(o).unwrap_or_default()))
}

#[derive(Deserialize)]
struct LoginVerifyReq {
    ceremony_id: String,
    credential: AuthenticationCredential,
    #[serde(flatten)]
    opts: LoginOptions,
}

async fn login_verify(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<LoginVerifyReq>) -> Res {
    let tokens = a
        .webauthn_login_verify(&ci, &b.ceremony_id, &b.credential, &b.opts)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    let extra = match &tokens.subject {
        crate::Subject::User { collection, id } => a
            .user_view(collection, id)
            .await
            .ok()
            .map(|u| ("user", serde_json::Value::Object(u))),
        _ => None,
    };
    Ok(session_response(&a, &tokens, extra))
}

#[derive(Deserialize)]
struct MfaOptionsReq {
    mfa_token: String,
}

async fn mfa_options(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<MfaOptionsReq>) -> Res {
    let o = a
        .webauthn_mfa_options(&ci, &b.mfa_token)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(o).unwrap_or_default()))
}

#[derive(Deserialize)]
struct MfaVerifyReq {
    mfa_token: String,
    ceremony_id: String,
    credential: AuthenticationCredential,
}

async fn mfa_verify(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<MfaVerifyReq>) -> Res {
    let tokens = a
        .webauthn_mfa_verify(&ci, &b.mfa_token, &b.ceremony_id, &b.credential)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(session_response(&a, &tokens, None))
}

async fn list(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller) -> Res {
    let v = a.webauthn_credentials(&c).await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"credentials": v})))
}

async fn remove(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(id): Path<String>,
) -> Res {
    a.webauthn_delete(&c, &ci, &id)
        .await
        .map_err(|e| HttpError::new(e, &rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
