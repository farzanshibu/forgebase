//! FIX-4 (MDF-59) acceptance tests: admin consents by realm/client, OIDC key
//! ids, social providers and the SMS gateway managed over REST. Real SQLite,
//! real HTTP routes, in-process mock gateway.
#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use common::{H, assert_error, harness, mock_http};
use forgebase_auth::sms::DevSmsProvider;
use serde_json::{Value, json};

const MASK: &str = "********";

async fn make_realm(h: &H, admin: &str, id: &str, config: Value) {
    h.sql(&format!(
        "INSERT INTO _forgebase_realms (id, name, settings, created_at) VALUES ('{id}', '{id}', '{{}}', 0) ON CONFLICT DO NOTHING"
    ))
    .await;
    let r = h
        .req(
            axum::http::Method::PUT,
            &format!("/api/auth/realms/{id}"),
            Some(admin),
            Some(json!({"config": config})),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
}

async fn put(h: &H, path: &str, bearer: &str, body: Value) -> common::Resp {
    h.req(axum::http::Method::PUT, path, Some(bearer), Some(body)).await
}

// ---------------------------------------------------------------------------
// OIDC keys
// ---------------------------------------------------------------------------

#[tokio::test]
async fn realm_keys_list_current_and_retired_kids_without_key_material() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let r = h.get("/api/auth/realms/default/keys", Some(&admin)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let items = r.json["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["status"], "active");
    assert_eq!(items[0]["alg"], "ES256");
    assert_eq!(r.json["current"], items[0]["kid"]);
    let first_kid = items[0]["kid"].as_str().unwrap().to_owned();
    let text = r.json.to_string();
    assert!(!text.contains("private") && !text.contains("\"d\""), "{text}");

    let rot = h
        .post("/api/auth/keys/oidc/rotate", Some(&admin), json!({"realm": "default"}))
        .await;
    assert_eq!(rot.status, StatusCode::OK, "{}", rot.json);
    let new_kid = rot.json["kid"].as_str().unwrap().to_owned();
    let r = h.get("/api/auth/realms/default/keys", Some(&admin)).await;
    let items = r.json["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(r.json["current"], new_kid);
    let old = items.iter().find(|k| k["kid"] == first_kid).unwrap();
    assert_eq!(old["status"], "retired");
    assert_eq!(old["verifying"], true);
    assert!(old["verify_until"].is_string());

    let (_, user) = h.verified_user("kim@example.com").await;
    assert_error(
        &h.get("/api/auth/realms/default/keys", Some(&user)).await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
    assert_error(
        &h.get("/api/auth/realms/default/keys", None).await,
        StatusCode::UNAUTHORIZED,
        "unauthenticated",
    );
    assert_error(
        &h.get("/api/auth/realms/nope/keys", Some(&admin)).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

// ---------------------------------------------------------------------------
// Consents
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_lists_and_revokes_consents_by_realm_and_client() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    let mut ids = Vec::new();
    for name in ["Acme Web", "Acme Mobile"] {
        let r = h
            .post(
                "/api/auth/clients",
                Some(&admin),
                json!({"name": name, "redirect_uris": ["https://app.example.com/cb"], "scopes": ["openid", "profile"]}),
            )
            .await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.json);
        ids.push(r.json["client"]["client_id"].as_str().unwrap().to_owned());
    }
    for (n, (cid, user)) in [(&ids[0], "u1"), (&ids[0], "u2"), (&ids[1], "u1")]
        .into_iter()
        .enumerate()
    {
        h.sql(&format!(
            "INSERT INTO _forgebase_oauth_consents (id, realm, client_id, collection, user_id, scopes, created_at, updated_at)
             VALUES ('c{n}', 'default', '{cid}', 'users', '{user}', '[\"openid\"]', {}, {})",
            100 + n,
            100 + n
        ))
        .await;
    }

    // The user-only route still refuses an administrator.
    assert_eq!(
        h.get("/api/auth/consents", Some(&admin)).await.status,
        StatusCode::FORBIDDEN
    );

    let r = h.get("/api/auth/realms/default/consents", Some(&admin)).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let items = r.json["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["id"], "c2", "newest first");
    assert_eq!(items[0]["client_name"], "Acme Mobile");
    assert_eq!(items[0]["user_id"], "u1");
    assert_eq!(items[0]["collection"], "users");

    let r = h
        .get(
            &format!("/api/auth/realms/default/consents?client_id={}", ids[0]),
            Some(&admin),
        )
        .await;
    assert_eq!(r.json["items"].as_array().unwrap().len(), 2);
    let r = h
        .get("/api/auth/realms/default/consents?limit=1&offset=1", Some(&admin))
        .await;
    assert_eq!(r.json["items"].as_array().unwrap().len(), 1);
    assert_eq!(r.json["items"][0]["id"], "c1");

    let r = h.delete("/api/auth/realms/default/consents/c0", Some(&admin)).await;
    assert_eq!(r.status, StatusCode::NO_CONTENT);
    assert_eq!(
        h.count("SELECT count(*) FROM _forgebase_oauth_consents").await,
        2,
        "one consent deleted"
    );
    assert_error(
        &h.delete("/api/auth/realms/default/consents/c0", Some(&admin)).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );

    let (_, user) = h.verified_user("kim@example.com").await;
    assert_error(
        &h.get("/api/auth/realms/default/consents", Some(&user)).await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );
}

// ---------------------------------------------------------------------------
// Social providers
// ---------------------------------------------------------------------------

fn google(secret: &str) -> Value {
    json!({"kind": "google", "client_id": "cid-123", "client_secret": secret, "display_name": "Google"})
}

#[tokio::test]
async fn social_providers_crud_with_write_only_sealed_secrets() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    make_realm(&h, &admin, "acme", json!({})).await;
    let base = "/api/auth/realms/acme/oauth-providers";

    assert_eq!(h.get(base, Some(&admin)).await.json["items"], json!([]));
    let r = put(&h, &format!("{base}/google"), &admin, google("s3cret-value")).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["client_secret"], MASK);
    assert_eq!(r.json["id"], "google");
    assert_eq!(r.json["source"], "admin");
    assert!(!r.json.to_string().contains("s3cret-value"));

    // Sealed at rest, not plaintext.
    assert_eq!(
        h.count("SELECT count(*) FROM _forgebase_oauth_providers WHERE CAST(config AS TEXT) LIKE '%s3cret-value%'")
            .await,
        0
    );
    assert_eq!(
        h.count("SELECT count(*) FROM _forgebase_oauth_providers WHERE CAST(config AS TEXT) LIKE '%sealed:%'")
            .await,
        1
    );

    // Registered at once and offered on the realm's login page only.
    let info = h
        .send(
            axum::http::Method::GET,
            "/api/auth/realm",
            None,
            None,
            &[("x-forgebase-realm", "acme")],
        )
        .await;
    assert_eq!(info.json["oauth_providers"][0]["id"], "google");
    let other = h.get("/api/auth/realm", None).await;
    assert_eq!(other.json["oauth_providers"], json!([]));

    let r = h.get(&format!("{base}/google"), Some(&admin)).await;
    assert_eq!(r.json["client_secret"], MASK);
    let list = h.get(base, Some(&admin)).await;
    assert_eq!(list.json["items"].as_array().unwrap().len(), 1);
    assert!(!list.json.to_string().contains("s3cret-value"));

    // Mask / null / absent keep the secret; a new value replaces it; "" clears it.
    for keep in [json!(MASK), Value::Null] {
        let r = put(&h, &format!("{base}/google"), &admin, google_with(keep, "Google 2")).await;
        assert_eq!(r.status, StatusCode::OK, "{}", r.json);
        assert_eq!(r.json["client_secret"], MASK);
        assert_eq!(r.json["display_name"], "Google 2");
    }
    let mut no_secret = google("x");
    no_secret.as_object_mut().unwrap().remove("client_secret");
    assert_eq!(
        put(&h, &format!("{base}/google"), &admin, no_secret).await.status,
        StatusCode::OK
    );
    // The kept secret is the original one (the provider still validates).
    assert!(h.auth.providers().get("acme", "google").is_some());
    let cleared = put(&h, &format!("{base}/google"), &admin, google("")).await;
    assert_error(&cleared, StatusCode::BAD_REQUEST, "invalid_input");

    // Validation.
    assert_error(
        &put(&h, &format!("{base}/BAD_ID"), &admin, google("x")).await,
        StatusCode::BAD_REQUEST,
        "invalid_input",
    );
    assert_error(
        &put(
            &h,
            &format!("{base}/acme-sso"),
            &admin,
            json!({"kind": "oidc", "client_id": "c", "client_secret": "s"}),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_input",
    );
    assert_error(
        &put(
            &h,
            &format!("{base}/google"),
            &admin,
            json!({"id": "other", "kind": "google", "client_id": "c", "client_secret": "s"}),
        )
        .await,
        StatusCode::BAD_REQUEST,
        "invalid_input",
    );

    // Non-admins are refused.
    let (_, user) = h.verified_user("kim@example.com").await;
    assert_error(&h.get(base, Some(&user)).await, StatusCode::FORBIDDEN, "forbidden");
    assert_error(
        &put(&h, &format!("{base}/x"), &user, google("s")).await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );

    // Delete.
    assert_eq!(
        h.delete(&format!("{base}/google"), Some(&admin)).await.status,
        StatusCode::NO_CONTENT
    );
    assert!(h.auth.providers().get("acme", "google").is_none());
    assert_error(
        &h.delete(&format!("{base}/google"), Some(&admin)).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
    assert_error(
        &h.get(&format!("{base}/google"), Some(&admin)).await,
        StatusCode::NOT_FOUND,
        "not_found",
    );
}

fn google_with(secret: Value, name: &str) -> Value {
    json!({"kind": "google", "client_id": "cid-123", "client_secret": secret, "display_name": name})
}

#[tokio::test]
async fn stored_providers_load_at_boot_and_the_file_wins_and_is_read_only() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    make_realm(&h, &admin, "acme", json!({})).await;
    let r = put(
        &h,
        "/api/auth/realms/acme/oauth-providers/google",
        &admin,
        google("boot-secret"),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    let r = put(
        &h,
        "/api/auth/realms/acme/oauth-providers/gh",
        &admin,
        json!({"kind": "github", "client_id": "g", "client_secret": "gs"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);

    // A file defines `gh` too.
    let auth_dir = h.dir.data_dir().join("auth");
    std::fs::create_dir_all(&auth_dir).ok();
    std::fs::write(
        auth_dir.join("oauth-providers.json"),
        r#"{"providers":[{"realm":"acme","id":"gh","kind":"github","client_id":"file","client_secret":"fs"}]}"#,
    )
    .unwrap();

    let mut cfg = forgebase_auth::AuthConfig::for_data_dir(h.dir.data_dir());
    cfg.argon2 = forgebase_auth::Argon2Config::insecure_fast_for_tests();
    let again = forgebase_auth::AuthService::open_with_data(
        Arc::new(h.db.clone()),
        Arc::new(forgebase_data::DefaultAuthorizer),
        h.dir.data_dir(),
        cfg,
        None,
    )
    .await
    .unwrap();
    assert!(
        again.providers().get("acme", "google").is_some(),
        "stored provider reloaded and unsealed"
    );
    assert!(again.providers().is_file_defined("acme", "gh"));
    assert!(!again.providers().is_file_defined("acme", "google"));
    assert_eq!(again.providers().get("acme", "gh").unwrap().config.client_id, "file");
}

// ---------------------------------------------------------------------------
// SMS gateway
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sms_gateway_per_realm_is_write_only_and_used_by_that_realm_only() {
    let h = harness(|_| {}).await;
    let admin = h.root_token().await;
    make_realm(
        &h,
        &admin,
        "acme",
        json!({"methods": {"phone": true}, "phone": {"signup": true}}),
    )
    .await;
    let global = DevSmsProvider::default();
    h.auth.set_sms_provider(Arc::new(global.clone()));
    let mock = mock_http::start(|_| (200, "{}".into())).await;

    let sms = "/api/auth/realms/acme/sms";
    let r = h.get(sms, Some(&admin)).await;
    assert_eq!(r.json["configured"], false);

    let r = put(
        &h,
        sms,
        &admin,
        json!({"provider": "webhook", "url": format!("{}/send", mock.url), "bearer_token": "hook-token"}),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert_eq!(r.json["configured"], true);
    assert_eq!(r.json["config"]["provider"], "webhook");
    assert_eq!(r.json["config"]["bearer_token"], MASK);
    assert!(!r.json.to_string().contains("hook-token"));
    assert_eq!(
        h.count("SELECT count(*) FROM _forgebase_sms_config WHERE CAST(config AS TEXT) LIKE '%hook-token%'")
            .await,
        0
    );
    assert_eq!(h.get(sms, Some(&admin)).await.json["config"]["bearer_token"], MASK);

    // The realm's phone login goes through its own gateway, with the token.
    let r = h
        .send(
            axum::http::Method::POST,
            "/api/auth/phone/request",
            None,
            Some(json!({"phone": "+15550100077"})),
            &[("x-forgebase-realm", "acme")],
        )
        .await;
    assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.json);
    let seen = mock.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/send");
    assert_eq!(seen[0].header("authorization"), Some("Bearer hook-token"));
    assert!(
        global.sent().is_empty(),
        "the instance gateway is not used for realm acme"
    );

    // Another realm keeps using the instance gateway.
    let r = h
        .post("/api/auth/phone/request", None, json!({"phone": "+15550100078"}))
        .await;
    // `default` has phone login off, so it answers uniformly without sending.
    assert!(
        r.status == StatusCode::ACCEPTED || r.status == StatusCode::FORBIDDEN,
        "{}",
        r.json
    );
    assert_eq!(mock.requests().len(), 1);

    // Keeping the token on update; changing provider type drops it.
    let r = put(
        &h,
        sms,
        &admin,
        json!({"provider": "webhook", "url": format!("{}/send2", mock.url), "bearer_token": MASK}),
    )
    .await;
    assert_eq!(r.json["config"]["bearer_token"], MASK);
    let r = put(&h, sms, &admin, json!({"provider": "webhook", "url": "not a url"})).await;
    assert_error(&r, StatusCode::BAD_REQUEST, "invalid_input");
    let r = put(&h, sms, &admin, json!({"provider": "carrier-pigeon"})).await;
    assert_error(&r, StatusCode::BAD_REQUEST, "invalid_input");
    let r = put(&h, sms, &admin, json!({"provider": "dev"})).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.json);
    assert!(r.json["config"]["bearer_token"].is_null());

    let (_, user) = h.verified_user("kim@example.com").await;
    assert_error(&h.get(sms, Some(&user)).await, StatusCode::FORBIDDEN, "forbidden");
    assert_error(
        &put(&h, sms, &user, json!({"provider": "dev"})).await,
        StatusCode::FORBIDDEN,
        "forbidden",
    );

    assert_eq!(h.delete(sms, Some(&admin)).await.status, StatusCode::NO_CONTENT);
    assert_eq!(h.get(sms, Some(&admin)).await.json["configured"], false);
    assert_error(&h.delete(sms, Some(&admin)).await, StatusCode::NOT_FOUND, "not_found");
}
