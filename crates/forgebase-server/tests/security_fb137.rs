//! FB-137 (rls-coverage G19): realm-scoped administrators, against the real
//! server. A user holding `identity:admin` through a realm binding
//! administers the identities of their own realm (users, sessions, keys and
//! service accounts, realm identity settings, audit, impersonation) and
//! nothing else; platform administrators and the first-run setup are
//! unchanged. Each `realm_admin_*` test failed before FB-137: every identity
//! admin API asked for a platform administrator, so a realm administrator
//! was refused everywhere (and nothing limited an administrator to a realm).
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use security_common::{Env, Resp, USER_PASSWORD};
use serde_json::{Value as Json, json};

const REALM: &str = "X-ForgeBase-Realm";

/// A realm with its own user pool `<realm>_users` (as the platform admin).
async fn realm(env: &Env, admin: &str, id: &str) {
    let r = env
        .as_(
            admin,
            "POST",
            "/api/v1/realms",
            &[],
            Some(json!({"slug": id, "name": id})),
        )
        .await;
    assert_eq!(r.status, 201, "realm: {}", r.text());
    let r = env
        .as_(admin, "PUT", &format!("/api/auth/realms/{id}"), &[], Some(json!({})))
        .await;
    assert_eq!(r.status, 200, "realm identity: {}", r.text());
}

/// A user of `realm`, signed in; returns (access token, user id).
async fn user(env: &Env, realm: &str, email: &str) -> (String, String) {
    let body = json!({"email": email, "password": USER_PASSWORD});
    let r = env
        .req("POST", "/api/auth/signup", &[(REALM, realm)], Some(body.clone()))
        .await;
    assert_eq!(r.status, 201, "signup: {}", r.text());
    let r = env.req("POST", "/api/auth/login", &[(REALM, realm)], Some(body)).await;
    assert_eq!(r.status, 200, "login: {}", r.text());
    let j = r.json();
    (
        j["access_token"].as_str().unwrap().to_owned(),
        j["user"]["id"].as_str().unwrap().to_owned(),
    )
}

/// Binds `principal` in `realm` to a realm role that grants `allow` (one
/// role per permission set).
async fn bind(env: &Env, admin: &str, realm: &str, principal: &str, allow: &[&str]) {
    let key = format!("r-{}", allow.join("-").replace([':', '*'], "_"));
    let r = env
        .as_(
            admin,
            "PUT",
            "/api/v1/roles",
            &[(REALM, realm)],
            Some(json!({"key": key, "allow": allow})),
        )
        .await;
    assert_eq!(r.status, 200, "role: {}", r.text());
    let r = env
        .as_(
            admin,
            "PUT",
            "/api/v1/realm-bindings",
            &[(REALM, realm)],
            Some(json!({"principal": principal, "roles": [key]})),
        )
        .await;
    assert_eq!(r.status, 200, "binding: {}", r.text());
}

/// Realms `acme` and `beta`, each with users, and `alice` an `acme` realm
/// administrator.
struct World {
    env: Env,
    root: String,
    alice: String,
    alice_id: String,
    bob: String,
    bob_id: String,
    carol: String,
    carol_id: String,
}

async fn world() -> World {
    let env = Env::new().await;
    let root = env.admin("root@example.com").await;
    realm(&env, &root, "acme").await;
    realm(&env, &root, "beta").await;
    let (alice, alice_id) = user(&env, "acme", "alice@example.com").await;
    let (bob, bob_id) = user(&env, "acme", "bob@example.com").await;
    let (carol, carol_id) = user(&env, "beta", "carol@example.com").await;
    bind(
        &env,
        &root,
        "acme",
        &format!("user:acme_users/{alice_id}"),
        &["identity:admin"],
    )
    .await;
    World {
        env,
        root,
        alice,
        alice_id,
        bob,
        bob_id,
        carol,
        carol_id,
    }
}

impl World {
    async fn call(&self, token: &str, method: &str, path: &str, body: Option<Json>) -> Resp {
        self.env.as_(token, method, path, &[], body).await
    }
}

fn items(r: &Resp) -> Vec<Json> {
    let j = r.json();
    j["items"]
        .as_array()
        .or_else(|| j.as_array())
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn realm_admin_manages_only_the_users_of_their_realm() {
    let w = world().await;
    let bob = format!("/api/auth/users/acme_users/{}", w.bob_id);
    let carol = format!("/api/auth/users/beta_users/{}", w.carol_id);

    // Reads: user detail, sessions, sign-in history, status, export.
    for suffix in ["", "/sessions", "/login-history", "/status", "/export"] {
        let r = w.call(&w.alice, "GET", &format!("{bob}{suffix}"), None).await;
        assert_eq!(r.status, 200, "alice GET acme user{suffix}: {}", r.text());
        let r = w.call(&w.alice, "GET", &format!("{carol}{suffix}"), None).await;
        assert_eq!(r.status, 403, "alice GET beta user{suffix}: {}", r.text());
        // A user of the realm without the binding stays refused.
        let r = w.call(&w.bob, "GET", &format!("{carol}{suffix}"), None).await;
        assert_eq!(r.status, 403, "bob GET user{suffix}: {}", r.text());
    }
    // An unknown user of another realm is refused, not reported missing.
    let r = w.call(&w.alice, "GET", "/api/auth/users/beta_users/nobody", None).await;
    assert_eq!(r.status, 403, "{}", r.text());

    // Writes: revoke sessions, suspend and unsuspend.
    let r = w
        .call(&w.alice, "POST", &format!("{carol}/sessions/revoke"), None)
        .await;
    assert_eq!(r.status, 403, "{}", r.text());
    let r = w.call(&w.carol, "GET", "/api/auth/me", None).await;
    assert_eq!(r.status, 200, "carol's session survives: {}", r.text());
    let r = w
        .call(
            &w.alice,
            "POST",
            &format!("{carol}/suspend"),
            Some(json!({"reason": "x"})),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.text());
    let r = w.call(&w.alice, "POST", &format!("{bob}/sessions/revoke"), None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = w.call(&w.bob, "GET", "/api/auth/me", None).await;
    assert_eq!(r.status, 401, "bob's session was revoked: {}", r.text());
    let r = w
        .call(
            &w.alice,
            "POST",
            &format!("{bob}/suspend"),
            Some(json!({"reason": "abuse"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = w
        .call(
            &w.alice,
            "POST",
            &format!("{bob}/unsuspend"),
            Some(json!({"reason": "ok"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());

    // Platform administrators are unchanged: every realm.
    for path in [&bob, &carol] {
        let r = w.call(&w.root, "GET", path, None).await;
        assert_eq!(r.status, 200, "root GET {path}: {}", r.text());
    }
    w.env.shutdown().await;
}

#[tokio::test]
async fn realm_admin_impersonates_only_in_their_realm() {
    let w = world().await;
    let imp = |p: &str| json!({"principal": p, "reason": "support ticket"});

    // A user of another realm, and a user without the binding: refused.
    let r = w
        .call(
            &w.alice,
            "POST",
            "/api/auth/impersonate",
            Some(imp(&format!("user:beta_users/{}", w.carol_id))),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.text());
    let r = w
        .call(
            &w.bob,
            "POST",
            "/api/auth/impersonate",
            Some(imp(&format!("user:acme_users/{}", w.alice_id))),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.text());

    // A user of their realm: allowed, recorded with the realm admin.
    let r = w
        .call(
            &w.alice,
            "POST",
            "/api/auth/impersonate",
            Some(imp(&format!("user:acme_users/{}", w.bob_id))),
        )
        .await;
    assert_eq!(r.status, 200, "alice impersonates bob: {}", r.text());
    let g = r.json();
    assert_eq!(g["impersonator"], format!("user:acme_users/{}", w.alice_id));
    let as_bob = g["access_token"].as_str().unwrap().to_owned();
    let bob_session = g["session_id"].as_str().unwrap().to_owned();
    let r = w.call(&as_bob, "GET", "/api/auth/me", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["impersonator"], g["impersonator"]);

    // Another realm administrator of the same realm cannot be impersonated.
    let (_dave, dave_id) = user(&w.env, "acme", "dave@example.com").await;
    bind(
        &w.env,
        &w.root,
        "acme",
        &format!("user:acme_users/{dave_id}"),
        &["identity:admin"],
    )
    .await;
    let r = w
        .call(
            &w.alice,
            "POST",
            "/api/auth/impersonate",
            Some(imp(&format!("user:acme_users/{dave_id}"))),
        )
        .await;
    assert_eq!(r.status, 403, "{}", r.text());
    assert!(r.text().contains("cannot_impersonate_admin"), "{}", r.text());

    // A platform admin impersonating a realm admin gets no admin powers.
    let r = w
        .call(
            &w.root,
            "POST",
            "/api/auth/impersonate",
            Some(imp(&format!("user:acme_users/{}", w.alice_id))),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let as_alice = r.json()["access_token"].as_str().unwrap().to_owned();
    let r = w
        .call(
            &as_alice,
            "GET",
            &format!("/api/auth/users/acme_users/{}", w.bob_id),
            None,
        )
        .await;
    assert_eq!(r.status, 403, "impersonated realm admin: {}", r.text());

    // Root impersonates carol (beta): alice neither sees nor ends it.
    let r = w
        .call(
            &w.root,
            "POST",
            "/api/auth/impersonate",
            Some(imp(&format!("user:beta_users/{}", w.carol_id))),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let carol_session = r.json()["session_id"].as_str().unwrap().to_owned();
    let r = w.call(&w.alice, "GET", "/api/auth/impersonations", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let seen: Vec<String> = items(&r)
        .iter()
        .map(|v| v["target"].as_str().unwrap().to_owned())
        .collect();
    assert!(seen.iter().all(|t| t.starts_with("user:acme_users/")), "{seen:?}");
    assert!(seen.contains(&format!("user:acme_users/{}", w.bob_id)), "{seen:?}");
    let r = w
        .call(
            &w.alice,
            "POST",
            &format!("/api/auth/impersonations/{carol_session}/end"),
            None,
        )
        .await;
    assert_eq!(r.status, 404, "{}", r.text());
    let r = w
        .call(
            &w.alice,
            "POST",
            &format!("/api/auth/impersonations/{bob_session}/end"),
            None,
        )
        .await;
    assert!(r.status == 200 || r.status == 204, "{}", r.text());
    let r = w.call(&w.root, "GET", "/api/auth/impersonations", None).await;
    assert!(items(&r).len() >= 3, "root sees every impersonation: {}", r.text());
    w.env.shutdown().await;
}

#[tokio::test]
async fn realm_admin_keys_service_accounts_are_realm_scoped() {
    let w = world().await;
    // Root: a beta service account with a key, and an admin service account.
    let beta_sa = w
        .env
        .as_(
            &w.root,
            "POST",
            "/api/auth/service-accounts",
            &[(REALM, "beta")],
            Some(json!({"name": "beta-bot", "role": "service"})),
        )
        .await;
    assert_eq!(beta_sa.status, 201, "{}", beta_sa.text());
    let beta_sa = beta_sa.json()["id"].as_str().unwrap().to_owned();
    let r = w
        .call(
            &w.root,
            "POST",
            &format!("/api/auth/service-accounts/{beta_sa}/keys"),
            Some(json!({"name": "k", "scopes": ["*"]})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let beta_key = r.json()["id"].as_str().unwrap().to_owned();
    let root_key = w.env.admin_key(&w.root, &["*"]).await;
    let r = w.call(&root_key, "GET", "/api/auth/me", None).await;
    let root_sa = r.json()["subject"]["id"].as_str().unwrap().to_owned();

    // Alice: a service account in her realm; no admin role, no other realm.
    let r = w
        .call(
            &w.alice,
            "POST",
            "/api/auth/service-accounts",
            Some(json!({"name": "acme-bot", "role": "service"})),
        )
        .await;
    assert_eq!(r.status, 201, "alice creates a service account: {}", r.text());
    assert_eq!(r.json()["realm"], "acme");
    let acme_sa = r.json()["id"].as_str().unwrap().to_owned();
    let r = w
        .call(
            &w.alice,
            "POST",
            "/api/auth/service-accounts",
            Some(json!({"name": "acme-root", "role": "admin"})),
        )
        .await;
    assert_eq!(r.status, 403, "admin service account: {}", r.text());
    let r = w
        .env
        .as_(
            &w.alice,
            "POST",
            "/api/auth/service-accounts",
            &[(REALM, "beta")],
            Some(json!({"name": "intruder", "role": "service"})),
        )
        .await;
    assert_eq!(r.status, 403, "service account in beta: {}", r.text());

    // Listing and reading: only acme's non-admin accounts.
    let r = w.call(&w.alice, "GET", "/api/auth/service-accounts", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let ids: Vec<Json> = items(&r).iter().map(|v| v["id"].clone()).collect();
    assert_eq!(ids, vec![json!(acme_sa)], "{}", r.text());
    for id in [&beta_sa, &root_sa] {
        let r = w
            .call(&w.alice, "GET", &format!("/api/auth/service-accounts/{id}"), None)
            .await;
        assert_eq!(r.status, 404, "{}", r.text());
        let r = w
            .call(
                &w.alice,
                "POST",
                &format!("/api/auth/service-accounts/{id}/keys"),
                Some(json!({"name": "stolen", "scopes": ["*"]})),
            )
            .await;
        assert_eq!(r.status, 404, "key for {id}: {}", r.text());
        let r = w
            .call(&w.alice, "DELETE", &format!("/api/auth/service-accounts/{id}"), None)
            .await;
        assert_eq!(r.status, 404, "delete {id}: {}", r.text());
    }

    // Keys: her account's key, and her realm users' PATs.
    let r = w
        .call(
            &w.alice,
            "POST",
            &format!("/api/auth/service-accounts/{acme_sa}/keys"),
            Some(json!({"name": "k", "scopes": ["*"]})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let acme_key = r.json()["id"].as_str().unwrap().to_owned();
    let r = w
        .call(
            &w.bob,
            "POST",
            "/api/auth/tokens",
            Some(json!({"name": "bob", "scopes": ["*"]})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let bob_pat = r.json()["id"].as_str().unwrap().to_owned();
    let r = w
        .call(
            &w.carol,
            "POST",
            "/api/auth/tokens",
            Some(json!({"name": "carol", "scopes": ["*"]})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let carol_pat = r.json()["id"].as_str().unwrap().to_owned();

    let r = w.call(&w.alice, "GET", "/api/auth/api-keys", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let mut keys: Vec<String> = items(&r).iter().map(|v| v["id"].as_str().unwrap().to_owned()).collect();
    keys.sort();
    let mut want = vec![acme_key.clone(), bob_pat.clone()];
    want.sort();
    assert_eq!(keys, want, "{}", r.text());
    let r = w
        .call(
            &w.alice,
            "GET",
            &format!("/api/auth/service-accounts/{beta_sa}/keys"),
            None,
        )
        .await;
    assert!(items(&r).is_empty(), "{}", r.text());

    for id in [&beta_key, &carol_pat] {
        let r = w
            .call(&w.alice, "DELETE", &format!("/api/auth/api-keys/{id}"), None)
            .await;
        assert_eq!(r.status, 404, "revoke {id}: {}", r.text());
    }
    for id in [&bob_pat, &acme_key] {
        let r = w
            .call(&w.alice, "DELETE", &format!("/api/auth/api-keys/{id}"), None)
            .await;
        assert_eq!(r.status, 204, "revoke {id}: {}", r.text());
    }
    let r = w
        .call(
            &w.alice,
            "DELETE",
            &format!("/api/auth/service-accounts/{acme_sa}"),
            None,
        )
        .await;
    assert_eq!(r.status, 204, "{}", r.text());

    // Platform administrators still list every key.
    let r = w.call(&w.root, "GET", "/api/auth/api-keys", None).await;
    let all: Vec<String> = items(&r).iter().map(|v| v["id"].as_str().unwrap().to_owned()).collect();
    assert!(all.contains(&beta_key) && all.contains(&carol_pat), "{}", r.text());
    w.env.shutdown().await;
}

#[tokio::test]
async fn realm_admin_realm_identity_and_audit_are_realm_scoped() {
    let w = world().await;

    // Realm identity settings: her realm only; setting a pool up is the platform's.
    let r = w.call(&w.alice, "GET", "/api/auth/realms", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let realms: Vec<Json> = items(&r).iter().map(|v| v["realm"].clone()).collect();
    assert_eq!(realms, vec![json!("acme")], "{}", r.text());
    for path in [
        "/api/auth/realms/acme",
        "/api/auth/realms/acme/consents",
        "/api/auth/realms/acme/oauth-providers",
        "/api/auth/realms/acme/sms",
        "/api/auth/realms/acme/keys",
    ] {
        let r = w.call(&w.alice, "GET", path, None).await;
        assert_eq!(r.status, 200, "alice GET {path}: {}", r.text());
        let beta = path.replace("/acme", "/beta");
        let r = w.call(&w.alice, "GET", &beta, None).await;
        assert_eq!(r.status, 403, "alice GET {beta}: {}", r.text());
        let r = w.call(&w.bob, "GET", path, None).await;
        assert_eq!(r.status, 403, "bob GET {path}: {}", r.text());
    }
    let r = w
        .call(
            &w.alice,
            "PUT",
            "/api/auth/realms/acme",
            Some(json!({"config": {"signup": false}})),
        )
        .await;
    assert_eq!(r.status, 200, "alice edits acme: {}", r.text());
    let r = w.call(&w.alice, "PUT", "/api/auth/realms/beta", Some(json!({}))).await;
    assert_eq!(r.status, 403, "{}", r.text());
    let r = w
        .call(
            &w.root,
            "POST",
            "/api/v1/realms",
            Some(json!({"slug": "gamma", "name": "gamma"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let r = w.call(&w.alice, "PUT", "/api/auth/realms/gamma", Some(json!({}))).await;
    assert_eq!(r.status, 403, "{}", r.text());

    // Audit: entries recorded for acme only.
    let r = w.call(&w.alice, "GET", "/api/auth/audit?limit=500", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let entries = items(&r);
    assert!(!entries.is_empty(), "{}", r.text());
    assert!(
        entries.iter().all(|e| e["collection"] != "beta_users"),
        "beta entries leak: {}",
        r.text()
    );
    assert!(
        entries
            .iter()
            .any(|e| e["collection"] == "acme_users" && e["subject_id"] == w.bob_id.as_str()),
        "{}",
        r.text()
    );
    let r = w.call(&w.root, "GET", "/api/auth/audit?limit=500", None).await;
    assert!(
        items(&r)
            .iter()
            .any(|e| e["collection"] == "beta_users" && e["subject_id"] == w.carol_id.as_str()),
        "root sees every realm: {}",
        r.text()
    );

    // What the caller administers, for the dashboard.
    let r = w.call(&w.alice, "GET", "/api/auth/me", None).await;
    assert_eq!(r.json()["admin_scope"], json!({"realm": "acme"}), "{}", r.text());
    let r = w.call(&w.root, "GET", "/api/auth/me", None).await;
    assert_eq!(r.json()["admin_scope"], json!("platform"), "{}", r.text());
    let r = w.call(&w.bob, "GET", "/api/auth/me", None).await;
    assert!(r.json().get("admin_scope").is_none(), "{}", r.text());

    // Platform-only APIs stay platform-only.
    for (method, path, body) in [
        ("GET", "/api/auth/admins", None),
        (
            "POST",
            "/api/auth/admins",
            Some(json!({"email": "x@example.com", "password": "long enough password 1"})),
        ),
        ("POST", "/api/auth/keys/rotate", Some(json!({}))),
        ("POST", "/api/auth/realms", Some(json!({"id": "evil", "name": "evil"}))),
        (
            "PUT",
            "/api/v1/realm-bindings",
            Some(json!({"principal": "user:acme_users/x", "roles": ["r-identity_admin"]})),
        ),
    ] {
        let r = w.call(&w.alice, method, path, body).await;
        assert_eq!(r.status, 403, "alice {method} {path}: {}", r.text());
    }
    w.env.shutdown().await;
}

#[tokio::test]
async fn realm_admin_binding_removal_and_other_keys_grant_nothing() {
    let w = world().await;
    let bob = format!("/api/auth/users/acme_users/{}", w.bob_id);
    // A realm role without identity:admin grants nothing.
    let (eve, eve_id) = user(&w.env, "acme", "eve@example.com").await;
    bind(
        &w.env,
        &w.root,
        "acme",
        &format!("user:acme_users/{eve_id}"),
        &["identity:read"],
    )
    .await;
    let r = w.call(&eve, "GET", &bob, None).await;
    assert_eq!(r.status, 403, "{}", r.text());
    // A binding in another realm is not one of her realms.
    bind(
        &w.env,
        &w.root,
        "beta",
        &format!("user:acme_users/{eve_id}"),
        &["identity:admin"],
    )
    .await;
    let r = w
        .call(&eve, "GET", &format!("/api/auth/users/beta_users/{}", w.carol_id), None)
        .await;
    assert_eq!(r.status, 403, "{}", r.text());
    // Alice's binding removed: refused again.
    let r = w.call(&w.alice, "GET", &bob, None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = w
        .env
        .as_(
            &w.root,
            "PUT",
            "/api/v1/realm-bindings",
            &[(REALM, "acme")],
            Some(json!({"principal": format!("user:acme_users/{}", w.alice_id), "roles": []})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = w.call(&w.alice, "GET", &bob, None).await;
    assert_eq!(r.status, 403, "{}", r.text());
    w.env.shutdown().await;
}

/// The first-run setup (FB-90) still creates a platform administrator.
#[tokio::test]
async fn first_run_setup_creates_a_platform_administrator() {
    let env = Env::new().await;
    let token = env.server.setup_token().expect("setup token").to_owned();
    let r = env
        .req(
            "POST",
            "/api/setup/admin",
            &[],
            Some(json!({"token": token, "email": "first@example.com", "password": "first admin password 1", "cookie": false})),
        )
        .await;
    assert!(r.status == 200 || r.status == 201, "setup: {}", r.text());
    let admin = r.json()["access_token"].as_str().unwrap().to_owned();
    let r = env.as_(&admin, "GET", "/api/auth/me", &[], None).await;
    assert_eq!(r.json()["admin_scope"], json!("platform"), "{}", r.text());
    realm(&env, &admin, "acme").await;
    let (_, id) = user(&env, "acme", "u@example.com").await;
    let r = env
        .as_(&admin, "GET", &format!("/api/auth/users/acme_users/{id}"), &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    env.shutdown().await;
}
