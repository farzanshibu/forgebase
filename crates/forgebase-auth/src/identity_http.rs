//! FB-25 routes under `/api/auth`: realm identity, phone + SMS, LDAP,
//! account lifecycle (ADR-0015 §8).

use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use serde::Deserialize;
use serde_json::json;

use crate::AuthService;
use crate::http::{Body, Caller, Client, HttpError, Res, login_response, ok};
use crate::realm::{RealmCreate, RealmInput};
use crate::service::LoginOptions;

pub(crate) fn routes() -> Router<AuthService> {
    Router::new()
        .route("/realm", get(realm_info))
        .route("/realms", get(list_realms).post(create_realm))
        .route("/realms/{realm}", get(get_realm).put(put_realm))
        .route("/realms/{realm}/keys", get(realm_keys))
        .route("/realms/{realm}/consents", get(realm_consents))
        .route("/realms/{realm}/consents/{id}", delete(realm_consent_revoke))
        .route("/realms/{realm}/oauth-providers", get(provider_list))
        .route(
            "/realms/{realm}/oauth-providers/{id}",
            get(provider_get).put(provider_put).delete(provider_delete),
        )
        .route("/realms/{realm}/sms", get(sms_get).put(sms_put).delete(sms_delete))
        .route("/phone/request", post(phone_request))
        .route("/phone/verify", post(phone_verify))
        .route("/phone", get(phone_get).delete(phone_delete))
        .route("/phone/add", post(phone_add))
        .route("/phone/confirm", post(phone_confirm))
        .route("/ldap/login", post(ldap_login))
        .route("/account/deactivate", post(account_deactivate))
        .route("/account/delete", post(account_delete))
        .route("/account/export", get(account_export))
        .route("/users/{collection}/{id}/status", get(user_status))
        .route("/users/{collection}/{id}/suspend", post(user_suspend))
        .route("/users/{collection}/{id}/unsuspend", post(user_unsuspend))
        .route("/users/{collection}/{id}/deactivate", post(user_deactivate))
        .route("/users/{collection}/{id}/reactivate", post(user_reactivate))
        .route("/users/{collection}/{id}/anonymise", post(user_anonymise))
        .route("/users/{collection}/{id}/delete", post(user_delete))
        .route("/users/{collection}/{id}/export", get(user_export))
}

#[derive(Deserialize)]
struct PhoneReq {
    #[serde(default)]
    collection: Option<String>,
    phone: String,
}

#[derive(Deserialize)]
struct PhoneVerifyReq {
    #[serde(default)]
    collection: Option<String>,
    phone: String,
    code: String,
    #[serde(flatten)]
    opts: LoginOptions,
}

#[derive(Deserialize)]
struct PhoneAddReq {
    phone: String,
}

#[derive(Deserialize)]
struct PhoneConfirmReq {
    phone: String,
    code: String,
}

#[derive(Deserialize)]
struct LdapReq {
    username: String,
    password: String,
    #[serde(flatten)]
    opts: LoginOptions,
}

#[derive(Deserialize, Default)]
struct ReauthReq {
    #[serde(default)]
    current_password: Option<String>,
    #[serde(default)]
    anonymise: bool,
}

#[derive(Deserialize, Default)]
struct ReasonReq {
    #[serde(default)]
    reason: String,
    /// Suspension end, Unix seconds.
    #[serde(default)]
    until: Option<i64>,
}

type P2 = Path<(String, String)>;

fn err(rid: &str) -> impl Fn(crate::AuthError) -> HttpError + '_ {
    move |e| HttpError::new(e, rid)
}

async fn realm_info(State(a): State<AuthService>, Client(ci, rid): Client) -> Res {
    Ok(ok(StatusCode::OK, a.realm_info(&ci).map_err(err(&rid))?))
}

async fn list_realms(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller) -> Res {
    let v = a.list_realm_identities(&c).map_err(err(&rid))?;
    let items: Vec<&crate::realm::RealmIdentity> = v.iter().map(|r| &**r).collect();
    Ok(ok(StatusCode::OK, json!({"items": items})))
}

async fn create_realm(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<RealmCreate>,
) -> Res {
    let v = a.create_realm_with_identity(&c, &ci, b).await.map_err(err(&rid))?;
    Ok(ok(StatusCode::CREATED, serde_json::to_value(&*v).unwrap_or_default()))
}

async fn get_realm(
    State(a): State<AuthService>,
    Client(_, rid): Client,
    Caller(c): Caller,
    Path(r): Path<String>,
) -> Res {
    let v = a.get_realm_identity(&c, &r).map_err(err(&rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(&*v).unwrap_or_default()))
}

async fn put_realm(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(r): Path<String>,
    Body(b): Body<RealmInput>,
) -> Res {
    let v = a.put_realm_identity(&c, &ci, &r, b).await.map_err(err(&rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(&*v).unwrap_or_default()))
}

async fn phone_request(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<PhoneReq>) -> Res {
    a.request_phone_code(&ci, b.collection.as_deref(), &b.phone)
        .await
        .map_err(err(&rid))?;
    Ok(ok(
        StatusCode::ACCEPTED,
        json!({"status": "accepted", "message": "if the number can sign in, a code has been sent"}),
    ))
}

async fn phone_verify(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<PhoneVerifyReq>) -> Res {
    let out = a
        .login_phone(&ci, b.collection.as_deref(), &b.phone, &b.code, &b.opts)
        .await
        .map_err(err(&rid))?;
    login_response(&a, out, &rid).await
}

async fn phone_get(State(a): State<AuthService>, Client(_, rid): Client, Caller(c): Caller) -> Res {
    let v = a.phone_of(&c).await.map_err(err(&rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(v).unwrap_or_default()))
}

async fn phone_add(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<PhoneAddReq>,
) -> Res {
    a.phone_add_start(&c, &ci, &b.phone).await.map_err(err(&rid))?;
    Ok(ok(StatusCode::ACCEPTED, json!({"status": "code_sent"})))
}

async fn phone_confirm(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<PhoneConfirmReq>,
) -> Res {
    let v = a
        .phone_add_confirm(&c, &ci, &b.phone, &b.code)
        .await
        .map_err(err(&rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(v).unwrap_or_default()))
}

async fn phone_delete(State(a): State<AuthService>, Client(ci, rid): Client, Caller(c): Caller) -> Res {
    a.phone_remove(&c, &ci).await.map_err(err(&rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn ldap_login(State(a): State<AuthService>, Client(ci, rid): Client, Body(b): Body<LdapReq>) -> Res {
    let out = a
        .login_ldap(&ci, &b.username, &b.password, &b.opts)
        .await
        .map_err(err(&rid))?;
    login_response(&a, out, &rid).await
}

async fn account_deactivate(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<ReauthReq>,
) -> Res {
    a.deactivate_self(&c, &ci, b.current_password.as_deref())
        .await
        .map_err(err(&rid))?;
    Ok(ok(StatusCode::OK, json!({"status": "deactivated"})))
}

async fn account_delete(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Body(b): Body<ReauthReq>,
) -> Res {
    let v = a
        .delete_self(&c, &ci, b.current_password.as_deref(), b.anonymise)
        .await
        .map_err(err(&rid))?;
    Ok(ok(StatusCode::OK, v))
}

async fn account_export(State(a): State<AuthService>, Client(ci, rid): Client, Caller(c): Caller) -> Res {
    Ok(ok(StatusCode::OK, a.export_self(&c, &ci).await.map_err(err(&rid))?))
}

async fn user_status(
    State(a): State<AuthService>,
    Client(_, rid): Client,
    Caller(c): Caller,
    Path((col, id)): P2,
) -> Res {
    let v = a.user_status(&c, &col, &id).await.map_err(err(&rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(v).unwrap_or_default()))
}

async fn user_suspend(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path((col, id)): P2,
    Body(b): Body<ReasonReq>,
) -> Res {
    let v = a
        .suspend_user(&c, &ci, &col, &id, &b.reason, b.until)
        .await
        .map_err(err(&rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(v).unwrap_or_default()))
}

async fn user_unsuspend(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path((col, id)): P2,
) -> Res {
    let v = a.unsuspend_user(&c, &ci, &col, &id).await.map_err(err(&rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(v).unwrap_or_default()))
}

async fn user_deactivate(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path((col, id)): P2,
    Body(b): Body<ReasonReq>,
) -> Res {
    let v = a
        .deactivate_user(&c, &ci, &col, &id, &b.reason)
        .await
        .map_err(err(&rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(v).unwrap_or_default()))
}

async fn user_reactivate(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path((col, id)): P2,
) -> Res {
    let v = a.reactivate_user(&c, &ci, &col, &id).await.map_err(err(&rid))?;
    Ok(ok(StatusCode::OK, serde_json::to_value(v).unwrap_or_default()))
}

async fn user_anonymise(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path((col, id)): P2,
) -> Res {
    Ok(ok(
        StatusCode::OK,
        a.anonymise_user(&c, &ci, &col, &id).await.map_err(err(&rid))?,
    ))
}

async fn user_delete(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path((col, id)): P2,
) -> Res {
    a.delete_user(&c, &ci, &col, &id).await.map_err(err(&rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn user_export(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path((col, id)): P2,
) -> Res {
    Ok(ok(
        StatusCode::OK,
        a.export_user(&c, &ci, &col, &id).await.map_err(err(&rid))?,
    ))
}

// ---------------------------------------------------------------------------
// FIX-4: realm administration (consents, keys, social providers, SMS gateway)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ConsentQuery {
    client_id: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

fn consent_query(raw: Option<&str>) -> ConsentQuery {
    let mut q = ConsentQuery::default();
    for (k, v) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
        match &*k {
            "client_id" => q.client_id = Some(v.into_owned()),
            "limit" => q.limit = v.parse().ok(),
            "offset" => q.offset = v.parse().ok(),
            _ => {}
        }
    }
    q
}

async fn realm_keys(
    State(a): State<AuthService>,
    Client(_, rid): Client,
    Caller(c): Caller,
    Path(r): Path<String>,
) -> Res {
    Ok(ok(StatusCode::OK, a.list_oidc_keys(&c, &r).await.map_err(err(&rid))?))
}

async fn realm_consents(
    State(a): State<AuthService>,
    Client(_, rid): Client,
    Caller(c): Caller,
    Path(r): Path<String>,
    RawQuery(raw): RawQuery,
) -> Res {
    let q = consent_query(raw.as_deref());
    let (limit, offset) = (q.limit.unwrap_or(100), q.offset.unwrap_or(0));
    let items = a
        .list_realm_consents(&c, &r, q.client_id.as_deref().filter(|s| !s.is_empty()), limit, offset)
        .await
        .map_err(err(&rid))?;
    Ok(ok(
        StatusCode::OK,
        json!({"items": items, "limit": limit.clamp(1, 500), "offset": offset.max(0)}),
    ))
}

async fn realm_consent_revoke(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path((r, id)): P2,
) -> Res {
    a.revoke_realm_consent(&c, &ci, &r, &id).await.map_err(err(&rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn provider_list(
    State(a): State<AuthService>,
    Client(_, rid): Client,
    Caller(c): Caller,
    Path(r): Path<String>,
) -> Res {
    Ok(ok(
        StatusCode::OK,
        a.list_realm_providers(&c, &r).await.map_err(err(&rid))?,
    ))
}

async fn provider_get(
    State(a): State<AuthService>,
    Client(_, rid): Client,
    Caller(c): Caller,
    Path((r, id)): P2,
) -> Res {
    Ok(ok(
        StatusCode::OK,
        a.get_realm_provider(&c, &r, &id).await.map_err(err(&rid))?,
    ))
}

async fn provider_put(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path((r, id)): P2,
    Body(b): Body<serde_json::Value>,
) -> Res {
    Ok(ok(
        StatusCode::OK,
        a.put_realm_provider(&c, &ci, &r, &id, b).await.map_err(err(&rid))?,
    ))
}

async fn provider_delete(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path((r, id)): P2,
) -> Res {
    a.delete_realm_provider(&c, &ci, &r, &id).await.map_err(err(&rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn sms_get(
    State(a): State<AuthService>,
    Client(_, rid): Client,
    Caller(c): Caller,
    Path(r): Path<String>,
) -> Res {
    Ok(ok(StatusCode::OK, a.get_realm_sms(&c, &r).await.map_err(err(&rid))?))
}

async fn sms_put(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(r): Path<String>,
    Body(b): Body<serde_json::Value>,
) -> Res {
    Ok(ok(
        StatusCode::OK,
        a.put_realm_sms(&c, &ci, &r, b).await.map_err(err(&rid))?,
    ))
}

async fn sms_delete(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Caller(c): Caller,
    Path(r): Path<String>,
) -> Res {
    a.delete_realm_sms(&c, &ci, &r).await.map_err(err(&rid))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
