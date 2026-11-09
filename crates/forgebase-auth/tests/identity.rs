//! FB-25 acceptance tests (ADR-0015): realm-scoped identity, phone + SMS,
//! LDAP, Cloudflare Turnstile, account lifecycle. Real SQLite, real HTTP
//! routes, in-process mock gateways (SMS provider, siteverify, LDAP server).
#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use std::sync::Arc;

use axum::http::{Method, StatusCode};
use common::mock_http;
use common::{H, PW, Resp, assert_error, harness};
use forgebase_auth::sms::{DevSmsProvider, SmsProvider};
use serde_json::{Value, json};

const ACME_PW: &str = "acme horse battery";

fn realm_h(r: &str) -> [(&'static str, &str); 1] {
    [("x-forgebase-realm", r)]
}

/// Creates realm `id` (as the policy API would) and gives it an identity.
async fn realm(h: &H, admin: &str, id: &str, config: Value) -> Resp {
    h.sql(&format!(
        "INSERT OR IGNORE INTO _forgebase_realms (id, name, settings, created_at) VALUES ('{id}', '{id}', '{{}}', 0)"
    ))
    .await;
    put_realm(h, admin, id, json!({"config": config})).await
}

async fn put_realm(h: &H, admin: &str, id: &str, body: Value) -> Resp {
    h.send(
        Method::PUT,
        &format!("/api/auth/realms/{id}"),
        Some(admin),
        Some(body),
        &[],
    )
    .await
}

async fn post_in(h: &H, realm: &str, path: &str, bearer: Option<&str>, body: Value) -> Resp {
    h.send(Method::POST, path, bearer, Some(body), &realm_h(realm)).await
}

fn dev_sms(h: &H) -> DevSmsProvider {
    let sms = DevSmsProvider::default();
    h.auth.set_sms_provider(Arc::new(sms.clone()));
    sms
}

fn code_for(sms: &DevSmsProvider, phone: &str) -> String {
    sms.last(phone).unwrap().secret
}

// ---------------------------------------------------------------------------
// Realms
// ---------------------------------------------------------------------------

#[tokio::test]
async fn same_email_in_two_realms_stays_isolated() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let r = realm(&h, &admin, "acme", json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["collection"], "acme_users");

    let a = h
        .post(
            "/api/auth/signup",
            None,
            json!({"email": "alice@example.com", "password": PW}),
        )
        .await;
    assert_eq!(a.status, StatusCode::CREATED, "{}", a.json);
    let b = post_in(
        &h,
        "acme",
        "/api/auth/signup",
        None,
        json!({"email": "alice@example.com", "password": ACME_PW}),
    )
    .await;
    assert_eq!(b.status, StatusCode::CREATED, "{}", b.json);
    assert_eq!(a.json["user"]["collection"], "users");
    assert_eq!(b.json["user"]["collection"], "acme_users");
    assert_ne!(a.json["user"]["id"], b.json["user"]["id"]);

    // Each realm checks its own password.
    let r = post_in(
        &h,
        "acme",
        "/api/auth/login",
        None,
        json!({"email": "alice@example.com", "password": PW}),
    )
    .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    let r = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "alice@example.com", "password": ACME_PW}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    let r = post_in(
        &h,
        "acme",
        "/api/auth/login",
        None,
        json!({"email": "alice@example.com", "password": ACME_PW}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["user"]["id"], b.json["user"]["id"]);
    let token = r.json["access_token"].as_str().unwrap().to_owned();
    let me = h.get("/api/auth/me", Some(&token)).await;
    assert_eq!(
        me.json["user"]["collection"]
            .as_str()
            .or(me.json["collection"].as_str()),
        Some("acme_users"),
        "{}",
        me.json
    );

    // Duplicates are per realm; another realm's collection is not reachable.
    let r = post_in(
        &h,
        "acme",
        "/api/auth/signup",
        None,
        json!({"email": "alice@example.com", "password": ACME_PW}),
    )
    .await;
    assert_error(&r, StatusCode::CONFLICT, "email_taken");
    let r = post_in(
        &h,
        "acme",
        "/api/auth/login",
        None,
        json!({"collection": "users", "email": "alice@example.com", "password": PW}),
    )
    .await;
    assert_error(&r, StatusCode::NOT_FOUND, "not_found");
    let r = post_in(
        &h,
        "nope",
        "/api/auth/login",
        None,
        json!({"email": "a@b.co", "password": PW}),
    )
    .await;
    assert_error(&r, StatusCode::NOT_FOUND, "not_found");
    let r = post_in(
        &h,
        "Bad Realm!",
        "/api/auth/login",
        None,
        json!({"email": "a@b.co", "password": PW}),
    )
    .await;
    assert_error(&r, StatusCode::NOT_FOUND, "not_found");

    // The directory knows each identity's home realm.
    assert_eq!(h.auth.realms().realm_of_collection("acme_users"), "acme");
    assert_eq!(h.auth.realms().realm_of_collection("users"), "default");
    // Audit rows carry the realm.
    assert!(
        h.count("SELECT COUNT(*) FROM _forgebase_auth_audit WHERE realm = 'acme' AND event = 'signup'")
            .await
            >= 1
    );
}

#[tokio::test]
async fn realm_configuration_is_admin_only_and_secrets_are_write_only() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let (_, user) = h.verified_user("u@example.com").await;
    h.sql("INSERT INTO _forgebase_realms (id, name, settings, created_at) VALUES ('acme', 'Acme', '{}', 0)")
        .await;
    let cfg = json!({"config": {"turnstile": {"enabled": true, "site_key": "site-1", "secret": "ts-secret-value"}}});
    assert_error(
        &put_realm(&h, &user, "acme", cfg.clone()).await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    let r = put_realm(&h, &admin, "acme", cfg).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["config"]["turnstile"]["secret"], "********");
    let stored = h
        .count("SELECT COUNT(*) FROM _forgebase_realm_identity WHERE realm_id = 'acme' AND config LIKE '%ts-secret-value%'")
        .await;
    assert_eq!(stored, 0, "secret stored sealed");
    // Sending the mask back keeps the secret.
    let r = put_realm(
        &h,
        &admin,
        "acme",
        json!({"config": {"turnstile": {"enabled": true, "site_key": "site-2", "secret": "********"}}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let ident = h.auth.realms().get("acme").unwrap();
    let ts = ident.config.turnstile.as_ref().unwrap();
    assert_eq!(
        (ts.site_key.as_str(), ts.secret.get()),
        ("site-2", Some("ts-secret-value"))
    );
    // Public view: site key, never the secret.
    let r = h
        .send(Method::GET, "/api/auth/realm", None, None, &realm_h("acme"))
        .await;
    assert_eq!(r.json["turnstile"]["site_key"], "site-2");
    assert!(!r.json.to_string().contains("ts-secret"));
    let r = h.get("/api/auth/realms", Some(&admin)).await;
    assert_eq!(r.json["items"].as_array().unwrap().len(), 2, "{}", r.json);

    // The user pool cannot move; unknown realms; default's users stay put.
    assert_error(
        &put_realm(&h, &admin, "acme", json!({"collection": "other_users"})).await,
        StatusCode::CONFLICT,
        "conflict",
    );
    assert_error(
        &put_realm(&h, &admin, "ghost", json!({})).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    h.sql("INSERT INTO _forgebase_realms (id, name, settings, created_at) VALUES ('beta', 'Beta', '{}', 0)")
        .await;
    assert_error(
        &put_realm(&h, &admin, "beta", json!({"collection": "users"})).await,
        StatusCode::CONFLICT,
        "conflict",
    );
    // Validation: an enabled Turnstile needs a secret; LDAP needs settings.
    assert_error(
        &put_realm(
            &h,
            &admin,
            "beta",
            json!({"config": {"turnstile": {"enabled": true, "site_key": "x"}}}),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_input",
    );
    assert_error(
        &put_realm(&h, &admin, "beta", json!({"config": {"methods": {"ldap": true}}})).await,
        StatusCode::BAD_REQUEST,
        "invalid_input",
    );
}

#[tokio::test]
async fn disabled_methods_and_signup_are_refused() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    h.verified_user("u@example.com").await;
    let r = put_realm(
        &h,
        &admin,
        "default",
        json!({"config": {"signup": false, "methods": {"password": false, "email_otp": false}}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let r = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "u@example.com", "password": PW}),
        )
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
    assert!(r.json["error"]["message"].as_str().unwrap().contains("method_disabled"));
    let r = h
        .post("/api/auth/otp/request", None, json!({"email": "u@example.com"}))
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
    let r = h
        .post(
            "/api/auth/signup",
            None,
            json!({"email": "n@example.com", "password": PW}),
        )
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
    // Magic links still work (enabled).
    let r = h
        .post("/api/auth/magic-link/request", None, json!({"email": "u@example.com"}))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.json);
}

#[tokio::test]
async fn service_accounts_belong_to_the_realm_they_are_created_in() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    realm(&h, &admin, "acme", json!({})).await;
    let r = h
        .send(
            Method::POST,
            "/api/auth/service-accounts",
            Some(&admin),
            Some(json!({"name": "acme-bot"})),
            &realm_h("acme"),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    assert_eq!(r.json["realm"], "acme");
    let id = r.json["id"].as_str().unwrap();
    assert_eq!(h.auth.realms().realm_of_service_account(id), "acme");
    let r = h
        .send(
            Method::POST,
            "/api/auth/service-accounts",
            Some(&admin),
            Some(json!({"name": "ghost-bot"})),
            &realm_h("ghost"),
        )
        .await;
    assert_error(&r, StatusCode::NOT_FOUND, "not_found");
}

// ---------------------------------------------------------------------------
// Phone + SMS
// ---------------------------------------------------------------------------

async fn enable_phone(h: &H, admin: &str, realm_id: &str, signup: bool) {
    let cfg = json!({"methods": {"phone": true}, "phone": {"signup": signup}});
    let r = if realm_id == "default" {
        put_realm(h, admin, "default", json!({"config": cfg})).await
    } else {
        realm(h, admin, realm_id, cfg).await
    };
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

async fn phone_login(h: &H, realm_id: &str, sms: &DevSmsProvider, phone: &str) -> Resp {
    let r = post_in(h, realm_id, "/api/auth/phone/request", None, json!({"phone": phone})).await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.json);
    let code = code_for(sms, &forgebase_auth::phone::normalize_phone(phone).unwrap());
    post_in(
        h,
        realm_id,
        "/api/auth/phone/verify",
        None,
        json!({"phone": phone, "code": code}),
    )
    .await
}

#[tokio::test]
async fn phone_login_signs_up_then_signs_in_the_same_account() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let sms = dev_sms(&h);
    // Off by default.
    let r = h
        .post("/api/auth/phone/request", None, json!({"phone": "+15550100001"}))
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
    enable_phone(&h, &admin, "default", true).await;

    let r = phone_login(&h, "default", &sms, "+1 555 010 0001").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let uid = r.json["user"]["id"].as_str().unwrap().to_owned();
    assert!(r.json["user"]["email"].as_str().unwrap().ends_with("@phone.invalid"));
    assert!(
        !r.json["user"]["email"].as_str().unwrap().contains("5550100001"),
        "number not in the placeholder"
    );
    let token = r.json["access_token"].as_str().unwrap().to_owned();
    let p = h.get("/api/auth/phone", Some(&token)).await;
    assert_eq!(p.json["phone"], "+15550100001");
    let s = h.get("/api/auth/sessions", Some(&token)).await;
    assert_eq!(s.json["items"][0]["auth_method"], "sms", "{}", s.json);

    let r = phone_login(&h, "default", &sms, "+15550100001").await;
    assert_eq!(r.json["user"]["id"], uid.as_str(), "second sign-in, same account");
    // The text carries the code, the dev provider kept it.
    assert!(
        sms.last("+15550100001")
            .unwrap()
            .text
            .contains(&sms.last("+15550100001").unwrap().secret)
    );
}

#[tokio::test]
async fn wrong_codes_burn_and_unknown_numbers_get_nothing_without_signup() {
    // Lockout off (it would answer 429 first), so the burn itself is tested.
    let h = harness(|c| c.lockout_threshold = 1000).await;
    let admin = h.root_token().await;
    let sms = dev_sms(&h);
    enable_phone(&h, &admin, "default", false).await;
    // Unknown number, signup off: same answer, no SMS.
    let r = h
        .post("/api/auth/phone/request", None, json!({"phone": "+15550100002"}))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    assert!(sms.sent().is_empty());
    assert_error(
        &h.post("/api/auth/phone/request", None, json!({"phone": "555-0100"}))
            .await,
        StatusCode::BAD_REQUEST,
        "invalid_input",
    );

    // A known number (added by its owner) gets codes.
    let (_, token) = h.verified_user("p@example.com").await;
    let r = h
        .post("/api/auth/phone/add", Some(&token), json!({"phone": "+15550100003"}))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.json);
    let code = code_for(&sms, "+15550100003");
    let r = h
        .post(
            "/api/auth/phone/confirm",
            Some(&token),
            json!({"phone": "+15550100003", "code": code}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);

    h.post("/api/auth/phone/request", None, json!({"phone": "+15550100003"}))
        .await;
    let good = code_for(&sms, "+15550100003");
    let wrong = if good == "000000" { "111111" } else { "000000" };
    for _ in 0..5 {
        let r = h
            .post(
                "/api/auth/phone/verify",
                None,
                json!({"phone": "+15550100003", "code": wrong}),
            )
            .await;
        assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    }
    let r = h
        .post(
            "/api/auth/phone/verify",
            None,
            json!({"phone": "+15550100003", "code": good}),
        )
        .await;
    assert_eq!(
        r.status,
        StatusCode::UNAUTHORIZED,
        "burnt after 5 wrong guesses: {}",
        r.json
    );
}

#[tokio::test]
async fn phone_numbers_are_per_realm_and_unique_within_one() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let sms = dev_sms(&h);
    enable_phone(&h, &admin, "default", true).await;
    enable_phone(&h, &admin, "acme", true).await;
    let a = phone_login(&h, "default", &sms, "+15550100009").await;
    let b = phone_login(&h, "acme", &sms, "+15550100009").await;
    assert_eq!(
        (a.status, b.status),
        (StatusCode::OK, StatusCode::OK),
        "{} {}",
        a.json,
        b.json
    );
    assert_ne!(a.json["user"]["id"], b.json["user"]["id"]);
    assert_eq!(b.json["user"]["collection"], "acme_users");

    // Another user of the default realm cannot take the number.
    let (_, other) = h.verified_user("o@example.com").await;
    h.post("/api/auth/phone/add", Some(&other), json!({"phone": "+15550100009"}))
        .await;
    let code = code_for(&sms, "+15550100009");
    let r = h
        .post(
            "/api/auth/phone/confirm",
            Some(&other),
            json!({"phone": "+15550100009", "code": code}),
        )
        .await;
    assert_error(&r, StatusCode::CONFLICT, "conflict");
    // A code for one account does not confirm for another.
    h.post("/api/auth/phone/add", Some(&other), json!({"phone": "+15550100010"}))
        .await;
    let code = code_for(&sms, "+15550100010");
    let (_, third) = h.verified_user("t@example.com").await;
    let r = h
        .post(
            "/api/auth/phone/confirm",
            Some(&third),
            json!({"phone": "+15550100010", "code": code}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    // The phone-only account cannot drop its only way in; a real email can.
    let tok_a = a.json["access_token"].as_str().unwrap();
    assert_error(
        &h.delete("/api/auth/phone", Some(tok_a)).await,
        StatusCode::CONFLICT,
        "conflict",
    );
    let r = h
        .post(
            "/api/auth/phone/confirm",
            Some(&other),
            json!({"phone": "+15550100010", "code": code}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        h.delete("/api/auth/phone", Some(&other)).await.status,
        StatusCode::NO_CONTENT
    );
}

// ---------------------------------------------------------------------------
// SMS gateways
// ---------------------------------------------------------------------------

#[tokio::test]
async fn twilio_and_webhook_gateways_use_the_http_client() {
    let h = harness(|_| {}).await;
    let mock = mock_http::start(|r| {
        if r.path.contains("fail") {
            (500, "{}".into())
        } else {
            (201, r#"{"sid":"SM1"}"#.into())
        }
    })
    .await;
    let sms = forgebase_auth::sms::Sms {
        to: "+15550100004".into(),
        text: "Your code is 123456".into(),
        kind: forgebase_auth::sms::SmsKind::LoginCode,
        secret: "123456".into(),
    };
    let twilio = forgebase_auth::sms::TwilioSms::new(
        "AC123",
        "tok",
        Some("+15550000000".into()),
        None,
        Some(mock.url.clone()),
        h.auth.http_client(),
    )
    .unwrap();
    twilio.send(&sms).await.unwrap();
    let hook = forgebase_auth::sms::WebhookSms::new(
        format!("{}/send", mock.url),
        Some("hook-token".into()),
        h.auth.http_client(),
    );
    hook.send(&sms).await.unwrap();
    let bad = forgebase_auth::sms::WebhookSms::new(format!("{}/fail", mock.url), None, h.auth.http_client());
    assert!(bad.send(&sms).await.is_err());

    let seen = mock.requests();
    assert_eq!(seen[0].path, "/2010-04-01/Accounts/AC123/Messages.json");
    assert_eq!(seen[0].header("authorization"), Some("Basic QUMxMjM6dG9r"));
    assert_eq!(seen[0].form("To").as_deref(), Some("+15550100004"));
    assert_eq!(seen[0].form("From").as_deref(), Some("+15550000000"));
    assert_eq!(seen[0].form("Body").as_deref(), Some("Your code is 123456"));
    assert_eq!(seen[1].header("authorization"), Some("Bearer hook-token"));
    let body: Value = serde_json::from_str(&seen[1].body).unwrap();
    assert_eq!(
        (body["to"].as_str(), body["kind"].as_str()),
        (Some("+15550100004"), Some("login_code"))
    );
}

// ---------------------------------------------------------------------------
// Turnstile
// ---------------------------------------------------------------------------

async fn siteverify(action: Option<&'static str>) -> mock_http::MockHttp {
    mock_http::start(move |r| {
        let token = r.form("response").unwrap_or_default();
        let ok = token == "good" && r.form("secret").as_deref() == Some("ts-secret");
        let hostname = if token == "elsewhere" { "evil.example" } else { "app.example" };
        if token == "down" {
            return (500, "oops".into());
        }
        (
            200,
            json!({"success": ok || token == "elsewhere", "hostname": hostname, "action": action.unwrap_or(""), "error-codes": []})
                .to_string(),
        )
    })
    .await
}

async fn enable_turnstile(h: &H, admin: &str, url: &str, actions: Value) {
    let r = put_realm(
        h,
        admin,
        "default",
        json!({"config": {"turnstile": {"enabled": true, "site_key": "site", "secret": "ts-secret",
            "verify_url": format!("{url}/turnstile/v0/siteverify"), "hostnames": ["app.example"], "actions": actions}}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

fn ts(token: &str) -> [(&'static str, &str); 1] {
    [("cf-turnstile-response", token)]
}

#[tokio::test]
async fn turnstile_protects_signup_login_reset_and_otp_when_enabled() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    // Off by default: no token needed.
    h.verified_user("u@example.com").await;
    let mock = siteverify(None).await;
    enable_turnstile(
        &h,
        &admin,
        &mock.url,
        json!(["signup", "login", "password_reset", "otp"]),
    )
    .await;

    let body = json!({"email": "n@example.com", "password": PW});
    let r = h
        .send(Method::POST, "/api/auth/signup", None, Some(body.clone()), &[])
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "turnstile_failed");
    let r = h
        .send(Method::POST, "/api/auth/signup", None, Some(body.clone()), &ts("bad"))
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "turnstile_failed");
    let r = h
        .send(Method::POST, "/api/auth/signup", None, Some(body), &ts("good"))
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    let seen = mock.requests().pop().unwrap();
    assert_eq!(seen.path, "/turnstile/v0/siteverify");
    assert_eq!(seen.form("remoteip").as_deref(), Some("10.0.0.1"));
    assert!(seen.form("idempotency_key").is_some());

    let login = json!({"email": "u@example.com", "password": PW});
    assert_error(
        &h.send(Method::POST, "/api/auth/login", None, Some(login.clone()), &[])
            .await,
        StatusCode::FORBIDDEN,
        "turnstile_failed",
    );
    let r = h
        .send(
            Method::POST,
            "/api/auth/login",
            None,
            Some(login),
            &[("x-turnstile-token", "good")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    for path in [
        "/api/auth/password-reset/request",
        "/api/auth/otp/request",
        "/api/auth/magic-link/request",
    ] {
        let b = json!({"email": "u@example.com"});
        let r = h.send(Method::POST, path, None, Some(b.clone()), &[]).await;
        assert_error(&r, StatusCode::FORBIDDEN, "turnstile_failed");
        let r = h.send(Method::POST, path, None, Some(b), &ts("good")).await;
        assert_eq!(r.status, StatusCode::ACCEPTED, "{path}: {}", r.json);
    }
    // Not protected: verification mails, refresh.
    let r = h
        .post(
            "/api/auth/verify-email/request",
            None,
            json!({"email": "u@example.com"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED);
    // Failures are audited.
    assert!(
        h.count("SELECT COUNT(*) FROM _forgebase_auth_audit WHERE event = 'turnstile' AND outcome = 'failure'")
            .await
            >= 4
    );
}

#[tokio::test]
async fn turnstile_fails_closed_and_checks_hostname_action_and_scope() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    h.verified_user("u@example.com").await;
    let mock = siteverify(Some("checkout")).await;
    enable_turnstile(&h, &admin, &mock.url, json!(["login"])).await;
    let login = json!({"email": "u@example.com", "password": PW});
    // The widget named another action.
    assert_error(
        &h.send(Method::POST, "/api/auth/login", None, Some(login.clone()), &ts("good"))
            .await,
        StatusCode::FORBIDDEN,
        "turnstile_failed",
    );
    let mock = siteverify(None).await;
    enable_turnstile(&h, &admin, &mock.url, json!(["login"])).await;
    // A token solved on another site.
    assert_error(
        &h.send(
            Method::POST,
            "/api/auth/login",
            None,
            Some(login.clone()),
            &ts("elsewhere"),
        )
        .await,
        StatusCode::FORBIDDEN,
        "turnstile_failed",
    );
    // siteverify down: refused, not waved through.
    let r = h
        .send(Method::POST, "/api/auth/login", None, Some(login.clone()), &ts("down"))
        .await;
    assert_error(&r, StatusCode::BAD_GATEWAY, "provider_error");
    // Only `login` is protected here.
    let r = h
        .post("/api/auth/otp/request", None, json!({"email": "u@example.com"}))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.json);
    // Disabled again: no token needed.
    put_realm(&h, &admin, "default", json!({"config": {}})).await;
    let r = h.send(Method::POST, "/api/auth/login", None, Some(login), &[]).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

#[tokio::test]
async fn turnstile_also_guards_sms_codes() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let sms = dev_sms(&h);
    let mock = siteverify(None).await;
    let r = put_realm(
        &h,
        &admin,
        "default",
        json!({"config": {"methods": {"phone": true}, "turnstile": {"enabled": true, "site_key": "s", "secret": "ts-secret",
            "verify_url": format!("{}/v", mock.url), "actions": ["otp"]}}}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let b = json!({"phone": "+15550100005"});
    let r = h
        .send(Method::POST, "/api/auth/phone/request", None, Some(b.clone()), &[])
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "turnstile_failed");
    assert!(sms.sent().is_empty());
    let r = h
        .send(Method::POST, "/api/auth/phone/request", None, Some(b), &ts("good"))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.json);
    assert_eq!(sms.sent().len(), 1);
}

// ---------------------------------------------------------------------------
// LDAP
// ---------------------------------------------------------------------------

use common::ldap::{Directory, Entry, Tls};

const SVC_DN: &str = "cn=svc,dc=example,dc=com";

fn ldap_cfg(port: u16, extra: Value) -> Value {
    let mut l = json!({
        "url": format!("ldap://127.0.0.1:{port}"),
        "insecure_plaintext": true,
        "bind_dn": SVC_DN,
        "bind_password": "svc-pw",
        "base_dn": "ou=people,dc=example,dc=com",
        "user_filter": "(&(objectClass=person)(uid={username}))",
        "id_attribute": "entryUUID",
    });
    if let (Some(o), Some(e)) = (l.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    json!({"config": {"methods": {"ldap": true}, "ldap": l}})
}

fn directory() -> Directory {
    Directory {
        entries: vec![
            Entry::person("alice", "alice-pw", Some("Alice@Corp.Example")),
            Entry::person("bob", "bob-pw", Some("bob@corp.example")),
            Entry::person("eve", "eve-pw", None),
        ],
        service: Some((SVC_DN.into(), "svc-pw".into())),
    }
}

async fn ldap_login(h: &H, user: &str, pw: &str) -> Resp {
    h.post("/api/auth/ldap/login", None, json!({"username": user, "password": pw}))
        .await
}

#[tokio::test]
async fn ldap_login_binds_searches_and_links_one_account() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let ldap = common::ldap::start(directory(), Tls::None).await;
    let r = put_realm(&h, &admin, "default", ldap_cfg(ldap.port, json!({}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["config"]["ldap"]["bind_password"], "********");

    let r = ldap_login(&h, "alice", "alice-pw").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["user"]["email"], "alice@corp.example");
    assert_eq!(r.json["user"]["verified"], true, "directory email trusted");
    let uid = r.json["user"]["id"].as_str().unwrap().to_owned();
    let log = ldap.log();
    assert_eq!(log[0], format!("bind {SVC_DN}"));
    assert!(log[1].starts_with("search And(["), "{log:?}");
    assert_eq!(log[2], "bind uid=alice,ou=people,dc=example,dc=com");
    let token = r.json["access_token"].as_str().unwrap().to_owned();
    let ids = h.get("/api/auth/oauth/identities", Some(&token)).await;
    assert_eq!(ids.json["identities"][0]["provider"], "ldap", "{}", ids.json);

    let r = ldap_login(&h, "ALICE", "alice-pw").await;
    assert_eq!(r.json["user"]["id"], uid.as_str(), "same directory account, same user");
    // No email attribute: a placeholder address.
    let r = ldap_login(&h, "eve", "eve-pw").await;
    assert!(
        r.json["user"]["email"].as_str().unwrap().ends_with("@ldap.invalid"),
        "{}",
        r.json
    );
}

#[tokio::test]
async fn ldap_refuses_wrong_passwords_unknown_users_injection_and_empty_passwords() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let ldap = common::ldap::start(directory(), Tls::None).await;
    put_realm(&h, &admin, "default", ldap_cfg(ldap.port, json!({}))).await;
    assert_error(
        &ldap_login(&h, "alice", "nope").await,
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
    );
    assert_error(
        &ldap_login(&h, "mallory", "x").await,
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
    );
    // A filter metacharacter is a literal, not a wildcard.
    assert_error(
        &ldap_login(&h, "*", "alice-pw").await,
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
    );
    assert!(
        ldap.log().iter().any(|l| l.contains(r#"Equal("uid", [42])"#)),
        "escaped: {:?}",
        ldap.log()
    );
    assert_error(
        &ldap_login(&h, "a*)(uid=*", "alice-pw").await,
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
    );
    // Empty password: refused before talking to the directory (an LDAP
    // unauthenticated bind would succeed).
    let before = ldap.log().len();
    assert_error(
        &ldap_login(&h, "alice", "").await,
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
    );
    assert_eq!(ldap.log().len(), before);
    assert!(
        h.count("SELECT COUNT(*) FROM _forgebase_auth_audit WHERE event = 'login' AND detail LIKE '%\"ldap\"%'")
            .await
            >= 4
    );
}

#[tokio::test]
async fn ldap_conflicts_ambiguity_outages_and_unsafe_configs() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    // A local account already owns bob's email (verified), no auto-link.
    let r = h
        .post(
            "/api/auth/signup",
            None,
            json!({"email": "bob@corp.example", "password": PW}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED);
    let ldap = common::ldap::start(directory(), Tls::None).await;
    put_realm(&h, &admin, "default", ldap_cfg(ldap.port, json!({}))).await;
    assert_error(&ldap_login(&h, "bob", "bob-pw").await, StatusCode::CONFLICT, "conflict");
    // A filter matching several entries is refused.
    put_realm(
        &h,
        &admin,
        "default",
        ldap_cfg(
            ldap.port,
            json!({"user_filter": "(|(uid={username})(objectClass=person))"}),
        ),
    )
    .await;
    assert_error(
        &ldap_login(&h, "alice", "alice-pw").await,
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
    );
    // Directory down.
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    put_realm(&h, &admin, "default", ldap_cfg(closed, json!({}))).await;
    assert_error(
        &ldap_login(&h, "alice", "alice-pw").await,
        StatusCode::BAD_GATEWAY,
        "provider_error",
    );
    // Clear-text ldap:// needs an explicit opt-in.
    let r = put_realm(
        &h,
        &admin,
        "default",
        ldap_cfg(ldap.port, json!({"insecure_plaintext": false})),
    )
    .await;
    assert_error(&r, StatusCode::BAD_REQUEST, "invalid_input");
    // Wrong service password: the directory is "unavailable", not the user wrong.
    put_realm(
        &h,
        &admin,
        "default",
        ldap_cfg(ldap.port, json!({"bind_password": "bad"})),
    )
    .await;
    assert_error(
        &ldap_login(&h, "alice", "alice-pw").await,
        StatusCode::BAD_GATEWAY,
        "provider_error",
    );
}

#[cfg(feature = "https")]
fn tls_acceptor() -> tokio_rustls::TlsAcceptor {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(include_bytes!("fixtures/tls-server.pem"))
        .map(|c| c.unwrap())
        .collect();
    let key = PrivateKeyDer::from_pem_slice(include_bytes!("fixtures/tls-server.key")).unwrap();
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(cfg))
}

#[cfg(feature = "https")]
#[tokio::test]
async fn ldap_over_starttls_and_ldaps_verifies_the_certificate() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let ca = include_str!("fixtures/tls-ca.pem");
    let ldap = common::ldap::start(directory(), Tls::StartTls(tls_acceptor())).await;
    let cfg = |extra: Value| {
        let mut c = ldap_cfg(ldap.port, extra);
        c["config"]["ldap"]["url"] = json!(format!("ldap://localhost:{}", ldap.port));
        c["config"]["ldap"]["insecure_plaintext"] = json!(false);
        c["config"]["ldap"]["start_tls"] = json!(true);
        c
    };
    let r = put_realm(&h, &admin, "default", cfg(json!({"ca_pem": ca}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let r = ldap_login(&h, "alice", "alice-pw").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(ldap.log()[0], "starttls");
    // Without the CA the certificate is not trusted.
    put_realm(&h, &admin, "default", cfg(json!({"ca_pem": null}))).await;
    assert_error(
        &ldap_login(&h, "alice", "alice-pw").await,
        StatusCode::BAD_GATEWAY,
        "provider_error",
    );

    let ldaps = common::ldap::start(directory(), Tls::Ldaps(tls_acceptor())).await;
    let mut c = ldap_cfg(ldaps.port, json!({"ca_pem": ca}));
    c["config"]["ldap"]["url"] = json!(format!("ldaps://127.0.0.1:{}", ldaps.port));
    c["config"]["ldap"]["insecure_plaintext"] = json!(false);
    let r = put_realm(&h, &admin, "default", c).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let r = ldap_login(&h, "bob", "bob-pw").await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

// ---------------------------------------------------------------------------
// Account lifecycle
// ---------------------------------------------------------------------------

async fn pat(h: &H, token: &str) -> String {
    let r = h
        .post(
            "/api/auth/tokens",
            Some(token),
            json!({"name": "t", "scopes": ["auth:read"]}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
    r.json["key"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn suspension_blocks_every_way_in_until_lifted() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let (uid, token) = h.verified_user("s@example.com").await;
    let login = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "s@example.com", "password": PW}),
        )
        .await;
    let refresh = login.json["refresh_token"].as_str().unwrap().to_owned();
    let pat = pat(&h, &token).await;
    assert_eq!(h.get("/api/auth/me", Some(&pat)).await.status, StatusCode::OK);

    let r = h
        .post(
            &format!("/api/auth/users/users/{uid}/suspend"),
            Some(&token),
            json!({"reason": "x"}),
        )
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
    let r = h
        .post(
            &format!("/api/auth/users/users/{uid}/suspend"),
            Some(&admin),
            json!({"reason": "abuse"}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        (r.json["status"].as_str(), r.json["reason"].as_str()),
        (Some("suspended"), Some("abuse"))
    );

    assert_eq!(
        h.get("/api/auth/me", Some(&token)).await.status,
        StatusCode::UNAUTHORIZED,
        "sessions revoked"
    );
    let r = h
        .post("/api/auth/refresh", None, json!({"refresh_token": refresh}))
        .await;
    assert!(
        r.status == StatusCode::FORBIDDEN || r.status == StatusCode::UNAUTHORIZED,
        "{}",
        r.json
    );
    let r = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "s@example.com", "password": PW}),
        )
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "account_suspended");
    assert_error(
        &h.get("/api/auth/me", Some(&pat)).await,
        StatusCode::FORBIDDEN,
        "account_suspended",
    );
    // The policy engine sees the same suspension.
    assert_eq!(
        h.count(&format!(
            "SELECT COUNT(*) FROM _forgebase_suspensions WHERE principal = 'user:users/{uid}'"
        ))
        .await,
        1
    );

    let r = h
        .post(
            &format!("/api/auth/users/users/{uid}/unsuspend"),
            Some(&admin),
            json!({}),
        )
        .await;
    assert_eq!(r.json["status"], "active", "{}", r.json);
    let r = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "s@example.com", "password": PW}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        h.get("/api/auth/me", Some(&pat)).await.status,
        StatusCode::OK,
        "tokens work again"
    );
    // A limited suspension that already passed does not block.
    h.sql(&format!(
        "INSERT INTO _forgebase_suspensions (principal, reason, suspended_by, suspended_at, until_at) VALUES ('user:users/{uid}', '', 'x', 0, 1)"
    ))
    .await;
    let r = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "s@example.com", "password": PW}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

#[tokio::test]
async fn deactivation_needs_proof_and_an_admin_to_undo() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let (uid, token) = h.verified_user("d@example.com").await;
    let r = h
        .post(
            "/api/auth/account/deactivate",
            Some(&token),
            json!({"current_password": "wrong password"}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    let r = h
        .post(
            "/api/auth/account/deactivate",
            Some(&token),
            json!({"current_password": PW}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(
        h.get("/api/auth/me", Some(&token)).await.status,
        StatusCode::UNAUTHORIZED
    );
    let r = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "d@example.com", "password": PW}),
        )
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "account_deactivated");
    let r = h
        .get(&format!("/api/auth/users/users/{uid}/status"), Some(&admin))
        .await;
    assert_eq!(r.json["status"], "deactivated");
    let r = h
        .post(
            &format!("/api/auth/users/users/{uid}/reactivate"),
            Some(&admin),
            json!({}),
        )
        .await;
    assert_eq!(r.json["status"], "active", "{}", r.json);
    let token = h.password_login("d@example.com").await;
    // Administrators deactivate too; reactivating an active account is a conflict.
    let r = h
        .post(
            &format!("/api/auth/users/users/{uid}/deactivate"),
            Some(&admin),
            json!({"reason": "left"}),
        )
        .await;
    assert_eq!(r.json["reason"], "left", "{}", r.json);
    assert_eq!(
        h.get("/api/auth/me", Some(&token)).await.status,
        StatusCode::UNAUTHORIZED
    );
    h.post(
        &format!("/api/auth/users/users/{uid}/reactivate"),
        Some(&admin),
        json!({}),
    )
    .await;
    let r = h
        .post(
            &format!("/api/auth/users/users/{uid}/reactivate"),
            Some(&admin),
            json!({}),
        )
        .await;
    assert_error(&r, StatusCode::CONFLICT, "conflict");
}

#[tokio::test]
async fn self_delete_removes_the_record_and_every_auth_row() {
    let h = harness(|_| {}).await;
    let (uid, token) = h.verified_user("del@example.com").await;
    let _pat = pat(&h, &token).await;
    // A recent sign-in stands in for the password.
    let r = h.post("/api/auth/account/delete", Some(&token), json!({})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["deleted"], true);
    let r = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "del@example.com", "password": PW}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    for (table, col) in [
        ("_forgebase_auth_sessions", "subject_id"),
        ("_forgebase_auth_api_keys", "owner_id"),
        ("_forgebase_auth_one_time", "subject_id"),
        ("users", "id"),
    ] {
        assert_eq!(
            h.count(&format!("SELECT COUNT(*) FROM {table} WHERE {col} = '{uid}'"))
                .await,
            0,
            "{table}"
        );
    }
    let pii = h
        .count(&format!(
            "SELECT COUNT(*) FROM _forgebase_auth_audit WHERE subject_id = '{uid}' AND (ip IS NOT NULL OR user_agent IS NOT NULL)"
        ))
        .await;
    assert_eq!(pii, 0, "audit trail keeps events, not IPs");
    assert!(
        h.count(&format!(
            "SELECT COUNT(*) FROM _forgebase_auth_audit WHERE subject_id = '{uid}' AND event = 'account.deleted'"
        ))
        .await
            == 1
    );
    // The address is free again.
    let r = h
        .post(
            "/api/auth/signup",
            None,
            json!({"email": "del@example.com", "password": PW}),
        )
        .await;
    assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
}

#[tokio::test]
async fn stale_sessions_must_prove_identity_again() {
    let h = harness(|_| {}).await;
    let (_, token) = h.verified_user("old@example.com").await;
    h.sql("UPDATE _forgebase_auth_sessions SET created_at = created_at - 3600")
        .await;
    let r = h.post("/api/auth/account/delete", Some(&token), json!({})).await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
    assert!(
        r.json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("reauthentication_required")
    );
    let r = h
        .post(
            "/api/auth/account/delete",
            Some(&token),
            json!({"current_password": PW, "anonymise": true}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["anonymised"], true);
}

#[tokio::test]
async fn anonymise_keeps_the_id_and_removes_personal_data() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let (uid, token) = h.verified_user("anon@example.com").await;
    let _pat = pat(&h, &token).await;
    let r = h
        .post(
            &format!("/api/auth/users/users/{uid}/anonymise"),
            Some(&token),
            json!({}),
        )
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
    let r = h
        .post(
            &format!("/api/auth/users/users/{uid}/anonymise"),
            Some(&admin),
            json!({}),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let anonymised = h
        .count(&format!(
            "SELECT COUNT(*) FROM users WHERE id = '{uid}' AND email LIKE 'anonymised.%@anonymised.invalid' AND verified = 0"
        ))
        .await;
    assert_eq!(anonymised, 1, "id kept, email replaced");
    assert_eq!(
        h.count(&format!(
            "SELECT COUNT(*) FROM _forgebase_auth_api_keys WHERE owner_id = '{uid}'"
        ))
        .await,
        0
    );
    assert_eq!(
        h.get("/api/auth/me", Some(&token)).await.status,
        StatusCode::UNAUTHORIZED
    );
    let r = h
        .post(
            "/api/auth/login",
            None,
            json!({"email": "anon@example.com", "password": PW}),
        )
        .await;
    assert_error(&r, StatusCode::UNAUTHORIZED, "invalid_credentials");
    let r = h
        .get(&format!("/api/auth/users/users/{uid}/status"), Some(&admin))
        .await;
    assert_eq!(r.json["status"], "anonymised");
    let r = h
        .post(
            &format!("/api/auth/users/users/{uid}/deactivate"),
            Some(&admin),
            json!({}),
        )
        .await;
    assert_error(&r, StatusCode::CONFLICT, "conflict");
    // Admin delete of another user.
    let (other, _) = h.verified_user("gone@example.com").await;
    let r = h
        .post(
            &format!("/api/auth/users/users/{other}/delete"),
            Some(&admin),
            json!({}),
        )
        .await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    let r = h
        .get(&format!("/api/auth/users/users/{other}/status"), Some(&admin))
        .await;
    assert_error(&r, StatusCode::NOT_FOUND, "not_found");
}

#[tokio::test]
async fn export_returns_the_account_data() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let (uid, token) = h.verified_user("exp@example.com").await;
    let r = h.get("/api/auth/account/export", Some(&token)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["format"], "forgebase.account-export.v1");
    assert_eq!(r.json["realm"], "default");
    assert_eq!(r.json["user"]["email"], "exp@example.com");
    assert!(r.json["user"].get("password").is_none() && r.json["user"].get("token_key").is_none());
    assert!(!r.json["sessions"].as_array().unwrap().is_empty());
    assert!(
        r.json["audit"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "signup")
    );
    assert_eq!(r.json["status"]["status"], "active");
    let pat = pat(&h, &token).await;
    assert_error(
        &h.get("/api/auth/account/export", Some(&pat)).await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    let r = h
        .get(&format!("/api/auth/users/users/{uid}/export"), Some(&admin))
        .await;
    assert_eq!(
        r.json["personal_access_tokens"].as_array().unwrap().len(),
        1,
        "{}",
        r.json
    );
    assert_error(
        &h.get(&format!("/api/auth/users/users/{uid}/export"), Some(&token))
            .await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    assert!(
        h.count("SELECT COUNT(*) FROM _forgebase_auth_audit WHERE event = 'account.exported'")
            .await
            >= 2
    );
}

// ---------------------------------------------------------------------------
// Impersonation (ADR-0014): no changes to the target's ways in or account
// ---------------------------------------------------------------------------

fn assert_impersonation_refused(r: &Resp, what: &str) {
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{what}: {}", r.json);
    assert!(
        r.json["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("impersonated_session"),
        "{what}: {}",
        r.json
    );
}

#[tokio::test]
async fn impersonated_sessions_cannot_change_ways_in_or_take_the_account() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let _sms = dev_sms(&h);
    enable_phone(&h, &admin, "default", true).await;
    let (uid, own) = h.verified_user("target@example.com").await;
    let g = h
        .post(
            "/api/auth/impersonate",
            Some(&admin),
            json!({"principal": format!("user:users/{uid}"), "reason": "support ticket 42"}),
        )
        .await;
    assert_eq!(g.status, StatusCode::OK, "{}", g.json);
    let tok = g.json["access_token"].as_str().unwrap().to_owned();
    // The impersonation works for reading.
    assert_eq!(h.get("/api/auth/me", Some(&tok)).await.status, StatusCode::OK);

    let cases: Vec<(&str, Resp)> = vec![
        (
            "oauth link",
            h.post("/api/auth/oauth/google/link", Some(&tok), json!({})).await,
        ),
        (
            "oauth unlink",
            h.delete("/api/auth/oauth/identities/some-id", Some(&tok)).await,
        ),
        (
            "passkey register",
            h.post("/api/auth/webauthn/register/options", Some(&tok), json!({}))
                .await,
        ),
        (
            "passkey delete",
            h.delete("/api/auth/webauthn/credentials/some-id", Some(&tok)).await,
        ),
        (
            "phone add",
            h.post("/api/auth/phone/add", Some(&tok), json!({"phone": "+15550100077"}))
                .await,
        ),
        (
            "phone confirm",
            h.post(
                "/api/auth/phone/confirm",
                Some(&tok),
                json!({"phone": "+15550100077", "code": "123456"}),
            )
            .await,
        ),
        ("phone remove", h.delete("/api/auth/phone", Some(&tok)).await),
        (
            "deactivate",
            h.post("/api/auth/account/deactivate", Some(&tok), json!({})).await,
        ),
        (
            "delete",
            h.post("/api/auth/account/delete", Some(&tok), json!({"current_password": PW}))
                .await,
        ),
        (
            "anonymise",
            h.post("/api/auth/account/delete", Some(&tok), json!({"anonymise": true}))
                .await,
        ),
        ("export", h.get("/api/auth/account/export", Some(&tok)).await),
    ];
    for (what, r) in &cases {
        assert_impersonation_refused(r, what);
    }
    // Nothing happened to the account; its own session still can.
    let r = h
        .get(&format!("/api/auth/users/users/{uid}/status"), Some(&admin))
        .await;
    assert_eq!(r.json["status"], "active");
    assert_eq!(
        h.get("/api/auth/account/export", Some(&own)).await.status,
        StatusCode::OK
    );
    let r = h
        .post("/api/auth/phone/add", Some(&own), json!({"phone": "+15550100077"}))
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.json);
    // Administrator lifecycle routes are not reachable with the user token.
    let r = h
        .post(&format!("/api/auth/users/users/{uid}/anonymise"), Some(&tok), json!({}))
        .await;
    assert_error(&r, StatusCode::FORBIDDEN, "forbidden");
}
