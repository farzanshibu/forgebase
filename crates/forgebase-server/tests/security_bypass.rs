//! FB-80 bypass suite (SPEC §27 #25: malicious inputs and permission bypass
//! attempts), run against the real server over TCP. Each test plays an
//! attacker and asserts the refusal *and* that nothing leaked or happened
//! (no record returned, no connection made, no file written). The threat
//! model these tests prove is docs/security.md §4.
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use forgebase_core::OpsAccess;
use security_common::{ADMIN_PASSWORD, Canary, Env, USER_PASSWORD, bearer, send};
use serde_json::{Value as Json, json};

const FWD: (&str, &str) = ("X-Forwarded-For", "203.0.113.9");

// ---------------------------------------------------------------------------
// Operational endpoints (/metrics, /api/system/*)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ops_endpoints_need_an_administrator_unless_called_directly_on_loopback() {
    let env = Env::new().await;
    let admin = env.admin("ops@example.com").await;
    let (user, _) = env.user("ops-user@example.com").await;
    let narrow = env.admin_key(&admin, &["collections:read"]).await;
    let system = env.admin_key(&admin, &["system:read"]).await;
    for path in ["/metrics", "/api/system/info"] {
        let r = env.req("GET", path, &[], None).await;
        assert_eq!(
            r.status, 200,
            "{path}: same-host scraper (default ops_access = loopback)"
        );
        // Through a reverse proxy, the socket peer is loopback but the
        // client is not: the forwarding header closes the loopback door.
        for h in ["X-Forwarded-For", "Forwarded", "X-Real-IP"] {
            let r = env.req("GET", path, &[(h, "203.0.113.9")], None).await;
            assert_eq!(r.status, 401, "{path} via proxy ({h}): {}", r.text());
            assert!(!r.text().contains(env.dir.path().to_str().unwrap()), "no data_dir leak");
        }
        let r = env.as_(&user, "GET", path, &[FWD], None).await;
        assert_eq!(r.status, 403, "{path}: a signed-in user is not an operator");
        let r = env.as_(&narrow, "GET", path, &[FWD], None).await;
        assert_eq!(r.status, 403, "{path}: an admin key without system scope");
        assert!(r.text().contains("insufficient_scope"), "{}", r.text());
        let r = env.as_(&system, "GET", path, &[FWD], None).await;
        assert_eq!(r.status, 200, "{path}: admin key with system:read");
        let r = env.as_(&admin, "GET", path, &[FWD], None).await;
        assert_eq!(r.status, 200, "{path}: administrator from anywhere");
    }
    let r = env.req("GET", "/health", &[FWD], None).await;
    assert_eq!(r.status, 200, "liveness stays public for load balancers");
    env.shutdown().await;

    let env = Env::with(|c| c.server.ops_access = OpsAccess::Admin).await;
    let r = env.req("GET", "/metrics", &[], None).await;
    assert_eq!(r.status, 401, "ops_access = admin: loopback alone is not enough");
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Tenancy: realm, organization, tenant
// ---------------------------------------------------------------------------

async fn org(env: &Env, token: &str, slug: &str) -> (String, String) {
    let r = env
        .as_(
            token,
            "POST",
            "/api/v1/orgs",
            &[],
            Some(json!({"slug": slug, "name": slug})),
        )
        .await;
    assert_eq!(r.status, 201, "org: {}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let p = env
        .as_(
            token,
            "POST",
            &format!("/api/v1/orgs/{id}/projects"),
            &[("X-ForgeBase-Org", &id)],
            Some(json!({"slug": "app", "name": "app"})),
        )
        .await;
    assert_eq!(p.status, 201, "project: {}", p.text());
    let tenant = p.json()["environments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "production")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    (id, tenant)
}

#[tokio::test]
async fn realm_org_and_tenant_boundaries_hold_over_http() {
    let env = Env::new().await;
    let admin = env.admin("root@example.com").await;
    let (alice, _) = env.user("alice@example.com").await;
    let (bob, _) = env.user("bob@example.com").await;

    // Realm: a default-realm user cannot act in another realm.
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/realms",
            &[],
            Some(json!({"slug": "acme", "name": "Acme"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let r = env
        .as_(
            &alice,
            "GET",
            "/api/v1/authz/me",
            &[("X-ForgeBase-Realm", "acme")],
            None,
        )
        .await;
    assert_eq!(r.status, 403, "cross-realm: {}", r.text());

    // Organizations and tenants.
    let (org_a, ta) = org(&env, &alice, "alpha").await;
    let (_org_b, tb) = org(&env, &bob, "bravo").await;
    let r = env.as_(&bob, "GET", &format!("/api/v1/orgs/{org_a}"), &[], None).await;
    assert!(matches!(r.status, 403 | 404), "bob reads alice's org: {}", r.text());
    let r = env
        .as_(&bob, "GET", "/api/v1/authz/me", &[("X-ForgeBase-Org", &org_a)], None)
        .await;
    assert_eq!(r.status, 403, "bob selects alice's org: {}", r.text());
    let r = env
        .as_(
            &bob,
            "PUT",
            &format!("/api/v1/orgs/{org_a}/members"),
            &[("X-ForgeBase-Org", &org_a)],
            Some(json!({"principal": "user:users/bob", "roles": ["owner"]})),
        )
        .await;
    assert_eq!(r.status, 403, "bob adds himself to alice's org");

    // A tenant-scoped collection open to any signed-in user.
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/collections",
            &[],
            Some(json!({"name": "notes", "options": {"tenant_scoped": true},
                        "fields": [{"name": "title", "type": "text"}]})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let open = "@request.auth.id != ''";
    let r = env
        .as_(
            &admin,
            "PUT",
            "/api/v1/policies/notes",
            &[],
            Some(json!({"list_rule": open, "view_rule": open, "create_rule": open,
                        "update_rule": open, "delete_rule": open})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let a_hdr = [("X-ForgeBase-Org", org_a.as_str()), ("X-ForgeBase-Tenant", ta.as_str())];
    let r = env
        .as_(
            &alice,
            "POST",
            "/api/v1/collections/notes/records",
            &a_hdr,
            Some(json!({"title": "alpha secret", "_tenant": tb})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let note = r.json()["id"].as_str().unwrap().to_owned();
    assert!(
        r.json().get("_tenant").is_none(),
        "the tenant column never reaches clients"
    );

    let rec = format!("/api/v1/collections/notes/records/{note}");
    let r = env.as_(&bob, "GET", &rec, &[("X-ForgeBase-Tenant", &tb)], None).await;
    assert_eq!(r.status, 404, "a known id of another tenant: {}", r.text());
    let r = env.as_(&bob, "GET", &rec, &[("X-ForgeBase-Tenant", &ta)], None).await;
    assert_eq!(r.status, 403, "bob selects alice's tenant: {}", r.text());
    let r = env.as_(&bob, "GET", &rec, &[], None).await;
    assert!(matches!(r.status, 400 | 403 | 404), "no tenant at all: {}", r.text());
    for (method, body) in [("PATCH", Some(json!({"title": "pwned"}))), ("DELETE", None)] {
        let r = env.as_(&bob, method, &rec, &[("X-ForgeBase-Tenant", &tb)], body).await;
        assert_eq!(r.status, 404, "{method} across tenants: {}", r.text());
    }
    let r = env
        .as_(
            &bob,
            "GET",
            "/api/v1/collections/notes/records?filter=title%20!%3D%20''",
            &[("X-ForgeBase-Tenant", &tb)],
            None,
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(!r.text().contains("alpha secret"), "bob's listing: {}", r.text());
    // The "_tenant" alice sent was ignored: bob's tenant has nothing.
    assert_eq!(r.json()["items"].as_array().map_or(0, Vec::len), 0);
    let r = env.req("GET", &rec, &[("X-ForgeBase-Tenant", &ta)], None).await;
    assert!(matches!(r.status, 401 | 403 | 404), "anonymous: {}", r.text());
    assert!(!r.text().contains("alpha secret"));
    let r = env.as_(&alice, "GET", &rec, &a_hdr, None).await;
    assert_eq!(r.status, 200, "the owner still reads it: {}", r.text());
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Policy: deny wins
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_deny_beats_every_allow() {
    let env = Env::new().await;
    let (alice, _) = env.user("owner@example.com").await;
    let (bob, bob_pk) = env.user("member@example.com").await;
    let (org_a, _) = org(&env, &alice, "reports").await;
    let oh = [("X-ForgeBase-Org", org_a.as_str())];
    for role in [
        json!({"key": "reporter", "allow": ["reports:*"]}),
        json!({"key": "no-export", "deny": ["reports:export"]}),
    ] {
        let r = env
            .as_(&alice, "PUT", &format!("/api/v1/orgs/{org_a}/roles"), &oh, Some(role))
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
    }
    let r = env
        .as_(
            &alice,
            "PUT",
            &format!("/api/v1/orgs/{org_a}/members"),
            &oh,
            Some(json!({"principal": bob_pk, "roles": ["reporter", "no-export"]})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let check = |perm: &'static str| {
        let env = &env;
        let bob = bob.clone();
        let org_a = org_a.clone();
        async move {
            env.as_(
                &bob,
                "POST",
                "/api/v1/authz/check",
                &[("X-ForgeBase-Org", &org_a)],
                Some(json!({"permission": perm, "org": org_a})),
            )
            .await
            .json()
        }
    };
    assert_eq!(check("reports:view").await["allowed"], true, "the allow applies");
    let denied = check("reports:export").await;
    assert_eq!(denied["allowed"], false, "the deny wins over reports:*: {denied}");
    // Order does not matter, and an owner's grant does not override it either.
    let r = env
        .as_(
            &alice,
            "PUT",
            &format!("/api/v1/orgs/{org_a}/members"),
            &oh,
            Some(json!({"principal": bob_pk, "roles": ["no-export", "reporter", "admin"]})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(check("reports:export").await["allowed"], false);
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Scope escalation with API keys and PATs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scoped_keys_and_pats_cannot_escalate() {
    let env = Env::new().await;
    let admin = env.admin("keys@example.com").await;
    let mut secret_coll = String::new();
    let mut secret_row = String::new();
    for name in ["secrets", "notes"] {
        let mut fields = vec![json!({"name": "title", "type": "text"})];
        if name == "notes" {
            fields.push(json!({"name": "ref", "type": "relation", "collection_id": secret_coll}));
        }
        let r = env
            .as_(
                &admin,
                "POST",
                "/api/v1/collections",
                &[],
                Some(json!({"name": name, "fields": fields})),
            )
            .await;
        assert_eq!(r.status, 201, "{}", r.text());
        if name == "secrets" {
            secret_coll = r.json()["id"].as_str().unwrap().to_owned();
        }
        let mut rec = json!({"title": format!("{name}-row")});
        if name == "notes" {
            rec["ref"] = json!(secret_row);
        }
        let r = env
            .as_(
                &admin,
                "POST",
                &format!("/api/v1/collections/{name}/records"),
                &[],
                Some(rec),
            )
            .await;
        assert_eq!(r.status, 201, "{}", r.text());
        if name == "secrets" {
            secret_row = r.json()["id"].as_str().unwrap().to_owned();
        }
    }
    // An admin service account narrowed to reading one collection.
    let key = env.admin_key(&admin, &["collections:read:notes"]).await;
    let r = env
        .as_(&key, "GET", "/api/v1/collections/notes/records", &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    // Expanding a relation must not read a collection outside the scope.
    let r = env
        .as_(&key, "GET", "/api/v1/collections/notes/records?expand=ref", &[], None)
        .await;
    assert!(
        !r.text().contains("secrets-row"),
        "expand crossed the scope: {}",
        r.text()
    );
    // A relation filter is an oracle on the target: it must not see it either.
    let r = env
        .as_(
            &key,
            "GET",
            "/api/v1/collections/notes/records?filter=ref.title%20%3D%20'secrets-row'",
            &[],
            None,
        )
        .await;
    assert!(
        r.status >= 400 || r.json()["items"].as_array().is_some_and(Vec::is_empty),
        "relation filter crossed the scope: {}",
        r.text()
    );
    for (method, path, body) in [
        ("GET", "/api/v1/collections/secrets/records", None),
        ("POST", "/api/v1/collections/notes/records", Some(json!({"title": "x"}))),
        ("POST", "/api/v1/collections/notes/records/query", Some(json!({}))),
        ("DELETE", "/api/v1/collections/notes", None),
        ("GET", "/api/auth/api-keys", None),
        (
            "POST",
            "/api/auth/admins",
            Some(json!({"email": "evil@example.com", "password": ADMIN_PASSWORD})),
        ),
        ("GET", "/api/v1/kv/_namespaces", None),
        ("GET", "/api/v1/storage/buckets", None),
        ("GET", "/api/v1/realtime/channels/x/presence", None),
    ] {
        let r = env.as_(&key, method, path, &[], body).await;
        assert_eq!(r.status, 403, "{method} {path}: {}", r.text());
        assert!(r.text().contains("insufficient_scope"), "{method} {path}: {}", r.text());
    }
    // The same key as X-API-Key behaves the same.
    let r = env
        .req(
            "GET",
            "/api/v1/collections/secrets/records",
            &[("X-API-Key", &key)],
            None,
        )
        .await;
    assert_eq!(r.status, 403);

    // PATs: a scoped PAT cannot mint a broader one.
    let (user, _) = env.user("pat@example.com").await;
    let mk = |token: String, scopes: Json| {
        let env = &env;
        async move {
            env.as_(
                &token,
                "POST",
                "/api/auth/tokens",
                &[],
                Some(json!({"name": "t", "scopes": scopes})),
            )
            .await
        }
    };
    let pat = mk(user.clone(), json!(["auth:write", "collections:read:notes"])).await;
    assert_eq!(pat.status, 201, "{}", pat.text());
    let pat = pat.json()["key"].as_str().unwrap().to_owned();
    for wider in [
        json!(["*"]),
        json!(["collections:read"]),
        json!(["auth:*"]),
        json!(["collections:*"]),
    ] {
        let r = mk(pat.clone(), wider.clone()).await;
        assert_eq!(r.status, 403, "PAT mints {wider}: {}", r.text());
    }
    let r = mk(pat.clone(), json!(["collections:read:notes"])).await;
    assert_eq!(r.status, 201, "a narrower PAT is fine: {}", r.text());
    let r = env
        .as_(&pat, "GET", "/api/v1/collections/secrets/records", &[], None)
        .await;
    assert_eq!(r.status, 403, "{}", r.text());
    // A PAT is never an admin credential, even for an endpoint it has scope for.
    let r = env.as_(&pat, "GET", "/api/auth/admins", &[], None).await;
    assert!(matches!(r.status, 403), "{}", r.text());
    // Invalid scope strings are refused, not widened.
    let r = mk(user, json!(["collections:readwrite", "../*"])).await;
    assert_eq!(r.status, 400, "{}", r.text());
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Administrative endpoints
// ---------------------------------------------------------------------------

#[tokio::test]
async fn administrative_endpoints_refuse_anonymous_and_ordinary_users() {
    let env = Env::with(|c| c.billing.enabled = true).await;
    let (user, _) = env.user("plain@example.com").await;
    // Real targets, so "not found" cannot pass for a refusal.
    let admin = env.admin("real-admin@example.com").await;
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/storage/buckets",
            &[],
            Some(json!({"name": "b"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let r = send(
        env.addr,
        "PUT",
        "/api/v1/storage/buckets/b/objects/k",
        &[("Authorization", &bearer(&admin)), ("Content-Type", "text/plain")],
        b"private",
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let cases: &[(&str, &str, Option<Json>)] = &[
        ("GET", "/api/auth/admins", None),
        (
            "POST",
            "/api/auth/admins",
            Some(json!({"email": "x@example.com", "password": ADMIN_PASSWORD})),
        ),
        ("GET", "/api/auth/api-keys", None),
        ("GET", "/api/auth/service-accounts", None),
        (
            "POST",
            "/api/auth/service-accounts",
            Some(json!({"name": "x", "role": "admin"})),
        ),
        ("GET", "/api/auth/audit", None),
        ("POST", "/api/auth/keys/rotate", Some(json!({}))),
        (
            "POST",
            "/api/auth/impersonate",
            Some(json!({"principal": "user:users/x", "reason": "test"})),
        ),
        ("GET", "/api/auth/users/users/x", None),
        ("PUT", "/api/auth/realms/default", Some(json!({}))),
        (
            "POST",
            "/api/v1/collections",
            Some(json!({"name": "pwn", "fields": []})),
        ),
        ("PUT", "/api/v1/policies/users", Some(json!({"list_rule": ""}))),
        ("POST", "/api/v1/realms", Some(json!({"slug": "evil", "name": "evil"}))),
        ("GET", "/api/v1/authz/audit", None),
        (
            "POST",
            "/api/v1/suspensions",
            Some(json!({"principal": "user:users/x"})),
        ),
        ("POST", "/api/v1/functions", Some(json!({"name": "f", "wasm": ""}))),
        ("POST", "/api/v1/storage/buckets", Some(json!({"name": "b"}))),
        ("POST", "/api/v1/storage/maintenance/reconcile", Some(json!({}))),
        ("POST", "/api/v1/queues/topics", Some(json!({"name": "t"}))),
        ("GET", "/api/v1/jobs", None),
        // FB-83 backups.
        ("GET", "/api/v1/system/backups", None),
        ("POST", "/api/v1/system/backups", Some(json!({}))),
        ("POST", "/api/v1/system/backups/prune", Some(json!({}))),
        ("GET", "/api/v1/system/backups/x", None),
        ("DELETE", "/api/v1/system/backups/x", None),
        ("POST", "/api/v1/system/backups/x/verify", Some(json!({}))),
        // FIX-4 (identity, billing, storage and KV admin endpoints).
        ("GET", "/api/auth/realms", None),
        ("POST", "/api/auth/realms", Some(json!({"id": "evil", "name": "evil"}))),
        ("GET", "/api/auth/realms/default/keys", None),
        ("GET", "/api/auth/realms/default/consents", None),
        ("DELETE", "/api/auth/realms/default/consents/x", None),
        ("GET", "/api/auth/realms/default/oauth-providers", None),
        ("GET", "/api/auth/realms/default/oauth-providers/x", None),
        (
            "PUT",
            "/api/auth/realms/default/oauth-providers/x",
            Some(json!({"kind": "github", "client_id": "c", "client_secret": "s"})),
        ),
        ("DELETE", "/api/auth/realms/default/oauth-providers/x", None),
        ("GET", "/api/auth/realms/default/sms", None),
        (
            "PUT",
            "/api/auth/realms/default/sms",
            Some(json!({"provider": "webhook", "url": "https://sms.example/x"})),
        ),
        ("DELETE", "/api/auth/realms/default/sms", None),
        ("GET", "/api/v1/billing/admin/orders", None),
        ("GET", "/api/v1/billing/admin/usage-meters", None),
        ("GET", "/api/v1/billing/dunning", None),
        ("POST", "/api/v1/billing/products/p/archive", Some(json!({}))),
        ("POST", "/api/v1/billing/prices/p/archive", Some(json!({}))),
        ("POST", "/api/v1/billing/coupons/c/archive", Some(json!({}))),
        ("POST", "/api/v1/billing/tax-rates/in/archive", Some(json!({}))),
        ("GET", "/api/v1/billing/webhook-events", None),
        (
            "PUT",
            "/api/v1/billing/providers/stripe",
            Some(json!({"api_key": "sk_test_x"})),
        ),
        ("GET", "/api/v1/storage/buckets/b/usage", None),
        ("GET", "/api/v1/storage/buckets/b/meta/k", None),
        ("GET", "/api/v1/kv/_info", None),
        ("GET", "/api/v1/kv/app/channels", None),
        (
            "POST",
            "/api/v1/kv/app/lists/l/set",
            Some(json!({"index": 0, "value": "x"})),
        ),
        ("POST", "/api/v1/kv/app/lists/l/remove", Some(json!({"value": "x"}))),
        ("POST", "/api/v1/kv/app/streams/s/delete", Some(json!({"ids": ["0-1"]}))),
    ];
    for (method, path, body) in cases {
        let r = env.req(method, path, &[], body.clone()).await;
        assert!(
            matches!(r.status, 401 | 403),
            "anonymous {method} {path}: {} {}",
            r.status,
            r.text()
        );
        let r = env.as_(&user, method, path, &[], body.clone()).await;
        assert!(
            matches!(r.status, 403 | 404),
            "user {method} {path}: {} {}",
            r.status,
            r.text()
        );
    }
    // Forged or foreign tokens are refused, never treated as anonymous.
    for token in [
        "eyJhbGciOiJub25lIn0.eyJzdWIiOiJhZG1pbiJ9.",
        "fbk_live_not_a_key",
        "x.y.z",
    ] {
        let r = env.as_(token, "GET", "/api/auth/me", &[], None).await;
        assert_eq!(r.status, 401, "{token}: {}", r.text());
        assert!(!r.raw().contains(token), "the token is not echoed");
    }
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// SSRF through every outbound client
// ---------------------------------------------------------------------------

fn fetch_module(url: &str) -> String {
    use base64::Engine as _;
    let args = format!(r#"{{"url":"{url}"}}"#);
    let esc: String = args.bytes().map(|b| format!("\\{b:02x}")).collect();
    let wat = format!(
        r#"(module
  (import "forgebase" "fetch" (func $fetch (param i32 i32) (result i32)))
  (import "forgebase" "result_read" (func $result_read (param i32 i32) (result i32)))
  (import "forgebase" "output" (func $output (param i32 i32) (result i32)))
  (memory (export "memory") 2)
  (data (i32.const 16) "{esc}")
  (func (export "handle") (result i32)
    (local $r i32)
    (local.set $r (call $fetch (i32.const 16) (i32.const {len})))
    (if (i32.lt_s (local.get $r) (i32.const 0)) (then (local.set $r (i32.sub (i32.const 0) (local.get $r)))))
    (drop (call $result_read (i32.const 4096) (local.get $r)))
    (drop (call $output (i32.const 4096) (local.get $r)))
    (i32.const 0)))"#,
        len = args.len()
    );
    base64::engine::general_purpose::STANDARD.encode(wat::parse_str(&wat).unwrap())
}

#[tokio::test]
async fn outbound_clients_never_reach_internal_addresses() {
    let canary = Canary::start().await;
    let p = canary.port;
    let env = Env::with(|c| c.queue.allow_http_push = true).await;
    let admin = env.admin("ssrf@example.com").await;

    // 1. Queue push webhooks: literal internal addresses are refused when
    //    the subscription is created ...
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/queues/topics",
            &[],
            Some(json!({"name": "events"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    for (i, url) in [
        format!("http://127.0.0.1:{p}/hook"),
        "https://169.254.169.254/latest/meta-data/".to_owned(),
        format!("https://[::ffff:127.0.0.1]:{p}/"),
        "https://10.1.2.3/".to_owned(),
        "https://[fd00::1]/".to_owned(),
        "https://user:pass@example.com/".to_owned(),
        "file:///etc/passwd".to_owned(),
        "gopher://127.0.0.1:6379/_FLUSHALL".to_owned(),
    ]
    .iter()
    .enumerate()
    {
        let r = env
            .as_(
                &admin,
                "POST",
                "/api/v1/queues/subscriptions",
                &[],
                Some(json!({"name": format!("bad{i}"), "topic": "events",
                            "push": {"kind": "webhook", "endpoint": url}})),
            )
            .await;
        assert_eq!(r.status, 400, "{url}: {}", r.text());
    }
    // ... and a name that resolves to loopback is refused at delivery.
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/queues/subscriptions",
            &[],
            Some(json!({"name": "rebind", "topic": "events",
                        "push": {"kind": "webhook", "endpoint": format!("http://localhost:{p}/hook")}})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/queues/topics/events/publish",
            &[],
            Some(json!({"messages": [{"data": {"n": 1}}]})),
        )
        .await;
    assert!(matches!(r.status, 200 | 201), "{}", r.text());

    // 2. Functions fetch: the live server's fetcher refuses the canary.
    for (name, url) in [
        ("f-ip", format!("http://127.0.0.1:{p}/")),
        ("f-name", format!("http://localhost:{p}/")),
        ("f-meta", "http://169.254.169.254/latest/meta-data/".to_owned()),
    ] {
        env.server
            .functions()
            .deploy(
                &forgebase_data::AccessContext::admin("root"),
                forgebase_functions::DeployRequest {
                    name: name.into(),
                    wasm: Some(fetch_module(&url)),
                    source: None,
                    language: None,
                    description: None,
                    access: Some(forgebase_functions::InvokeAccess::Public),
                    capabilities: vec!["fetch".into()],
                    fetch_hosts: Vec::new(),
                    limits: forgebase_functions::LimitsInput::default(),
                    env: Default::default(),
                    hooks: Vec::new(),
                    activate: true,
                },
            )
            .await
            .unwrap();
        let out: Json = env
            .server
            .functions()
            .invoke(name, forgebase_functions::Trigger::Direct, &json!({}), None)
            .await
            .unwrap()
            .json()
            .unwrap()
            .unwrap();
        assert_eq!(out["code"], "fetch_blocked", "{name} {url}: {out}");
    }

    // 3. OAuth / OIDC: a provider configured through the admin API (FIX-4)
    //    or the operator's provider file whose discovery URL or endpoints
    //    point at loopback or the metadata address is refused before
    //    connecting.
    for (id, cfg) in [
        (
            "api-issuer",
            json!({"kind": "oidc", "client_id": "c", "client_secret": "s",
                              "issuer": format!("https://localhost:{p}")}),
        ),
        (
            "api-token",
            json!({"kind": "github", "client_id": "c", "client_secret": "s",
                             "token_url": format!("http://127.0.0.1:{p}/token"),
                             "authorize_url": "https://github.com/login/oauth/authorize",
                             "userinfo_url": "http://169.254.169.254/latest/meta-data/"}),
        ),
    ] {
        let r = env
            .as_(
                &admin,
                "PUT",
                &format!("/api/auth/realms/default/oauth-providers/{id}"),
                &[],
                Some(cfg),
            )
            .await;
        assert!(r.status < 300, "{id}: {}", r.text());
    }
    let r = env
        .req(
            "POST",
            "/api/auth/oauth/api-issuer/start",
            &[],
            Some(json!({"redirect_to": "/"})),
        )
        .await;
    assert!(r.status >= 400, "{}", r.text());
    assert!(
        r.text().contains("refused") || r.status == 502,
        "{} {}",
        r.status,
        r.text()
    );
    // The callback is where a code is exchanged at the token URL.
    let start = env
        .req(
            "POST",
            "/api/auth/oauth/api-token/start",
            &[],
            Some(json!({"redirect_to": "/"})),
        )
        .await;
    assert_eq!(start.status, 200, "{}", start.text());
    let state = start.json()["state"].as_str().unwrap().to_owned();
    let r = env
        .req(
            "GET",
            &format!("/api/auth/oauth/api-token/callback?code=abc&state={state}"),
            &[],
            None,
        )
        .await;
    assert!(
        r.status >= 300,
        "token exchange against loopback: {} {}",
        r.status,
        r.text()
    );
    assert!(!r.text().contains("access_token"), "{}", r.text());
    let providers = env.dir.path().join("forgebase-data/auth/oauth-providers.json");
    std::fs::write(
        &providers,
        json!({"providers": [{"id": "evil", "kind": "oidc", "client_id": "c", "client_secret": "s",
                              "issuer": format!("https://localhost:{p}")}]})
        .to_string(),
    )
    .unwrap();
    let data_dir = env.dir.path().join("forgebase-data");
    let security_common::Env { server, dir: _keep, .. } = env;
    server.shutdown().await.unwrap();
    let mut cfg = security_common::config(data_dir.parent().unwrap());
    cfg.data_dir = data_dir;
    let server = forgebase_server::start(&cfg).await.unwrap();
    let r = send(
        server.local_addr(),
        "POST",
        "/api/auth/oauth/evil/start",
        &[],
        br#"{"redirect_to":"/"}"#,
    )
    .await;
    assert!(r.status >= 400, "{}", r.text());
    assert!(
        r.text().contains("refused") || r.status == 502,
        "{} {}",
        r.status,
        r.text()
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(canary.hits(), 0, "no outbound client connected to the internal canary");
    server.shutdown().await.unwrap();
}

// ---------------------------------------------------------------------------
// Storage: path traversal and upload validation
// ---------------------------------------------------------------------------

fn files_under(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p.display().to_string());
            }
        }
    }
    out
}

#[tokio::test]
async fn storage_keys_cannot_escape_and_uploads_are_validated() {
    let env = Env::new().await;
    let admin = env.admin("files@example.com").await;
    let auth = bearer(&admin);
    let a = [("Authorization", auth.as_str())];
    for body in [
        json!({"name": "files"}),
        json!({"name": "img", "max_object_bytes": 1000, "allowed_content_types": ["image/*"]}),
    ] {
        let r = env.req("POST", "/api/v1/storage/buckets", &a, Some(body)).await;
        assert_eq!(r.status, 201, "{}", r.text());
    }
    for name in ["..", "../x", "a/b", ".hidden", "UPPER", "x\\y"] {
        let r = env
            .req("POST", "/api/v1/storage/buckets", &a, Some(json!({"name": name})))
            .await;
        assert_eq!(r.status, 400, "bucket {name:?}: {}", r.text());
    }
    // Traversal keys are opaque names: stored inside the content store or
    // refused, never written next to it.
    let marker = b"traversal-marker-7f3a";
    for key in [
        "../../../../escape.txt",
        "..%2F..%2F..%2Fescape2.txt",
        "a/../../escape3.txt",
        "%2e%2e/%2e%2e/escape4.txt",
        "..\\..\\escape5.txt",
    ] {
        let path = format!("/api/v1/storage/buckets/files/objects/{key}");
        let r = send(env.addr, "PUT", &path, &[a[0], ("Content-Type", "text/plain")], marker).await;
        assert!(matches!(r.status, 201 | 400 | 404), "{key}: {} {}", r.status, r.text());
    }
    for f in files_under(env.dir.path()) {
        assert!(!f.contains("escape"), "a traversal key became a file: {f}");
    }
    let outside = env.dir.path().parent().unwrap();
    for n in ["escape.txt", "escape2.txt", "escape3.txt", "escape4.txt", "escape5.txt"] {
        assert!(!outside.join(n).exists());
    }
    let r = env
        .req(
            "GET",
            "/api/v1/storage/buckets/files/objects/..%2F..%2Fforgebase.db",
            &a,
            None,
        )
        .await;
    assert!(matches!(r.status, 400 | 404), "{}", r.status);
    let r = env.req("GET", "/admin/..%2F..%2FCargo.toml", &[], None).await;
    assert!(!r.text().contains("[package]"), "dashboard assets do not traverse");

    // Upload validation: allow-list, signature check, size limit.
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[0u8; 64]].concat();
    let put = |bucket: &'static str, key: &'static str, ct: &'static str, body: Vec<u8>| {
        let env = &env;
        let auth = auth.clone();
        async move {
            send(
                env.addr,
                "PUT",
                &format!("/api/v1/storage/buckets/{bucket}/objects/{key}"),
                &[("Authorization", &auth), ("Content-Type", ct)],
                &body,
            )
            .await
        }
    };
    let r = put("img", "ok.png", "image/png", png.clone()).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let r = put(
        "img",
        "evil.png",
        "image/png",
        b"<html><script>alert(document.cookie)</script>".to_vec(),
    )
    .await;
    assert_eq!(r.status, 400, "HTML under a PNG label: {}", r.text());
    assert_eq!(r.code(), "content_type_mismatch");
    let r = put("img", "evil.html", "text/html", b"<html></html>".to_vec()).await;
    assert_eq!(r.code(), "content_type_not_allowed", "{}", r.text());
    let r = put("img", "evil.svg", "image/svg+xml", b"<svg onload=alert(1)/>".to_vec()).await;
    assert_eq!(r.status, 201, "SVG is an image/* type the bucket allows: {}", r.text());
    let r = put("img", "big.png", "image/png", [png.clone(), vec![0u8; 2000]].concat()).await;
    assert_eq!(r.status, 413, "{}", r.text());
    let r = put("img", "pdf.png", "image/png", b"%PDF-1.7 ...".to_vec()).await;
    assert_eq!(r.code(), "content_type_mismatch", "a PDF is not a PNG");
    let r = put(
        "img",
        "noext.png",
        "application/octet-stream",
        b"<html></html>".to_vec(),
    )
    .await;
    assert_eq!(
        r.code(),
        "content_type_not_allowed",
        "no type guessed from a lying extension: {}",
        r.text()
    );
    assert!(
        env.req("GET", "/api/v1/storage/buckets/img/objects/evil.png", &a, None)
            .await
            .status
            == 404
    );
    // Active content is served inert: attachment, nosniff, sandbox.
    let r = put("files", "page.html", "text/html", b"<script>alert(1)</script>".to_vec()).await;
    assert_eq!(r.status, 201, "{}", r.text());
    for key in ["page.html", "../img/evil.svg"] {
        let path = if key.starts_with("..") {
            "/api/v1/storage/buckets/img/objects/evil.svg".to_owned()
        } else {
            format!("/api/v1/storage/buckets/files/objects/{key}")
        };
        let r = env.req("GET", &path, &a, None).await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
        assert!(
            r.header("content-security-policy")
                .unwrap_or_default()
                .contains("sandbox")
        );
        assert!(
            r.header("content-disposition")
                .unwrap_or_default()
                .starts_with("attachment")
        );
    }
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Brute force and rate limits
// ---------------------------------------------------------------------------

#[tokio::test]
async fn brute_force_is_locked_out_and_rate_limited_without_trusting_client_headers() {
    let env = Env::new().await;
    env.auth
        .signup(
            &forgebase_auth::ClientInfo::default(),
            None,
            "victim@example.com",
            USER_PASSWORD,
            None,
        )
        .await
        .unwrap();
    let login = |pw: &'static str, xff: String| {
        let env = &env;
        async move {
            env.req(
                "POST",
                "/api/auth/login",
                &[("X-Forwarded-For", &xff)],
                Some(json!({"email": "victim@example.com", "password": pw})),
            )
            .await
        }
    };
    let mut statuses = Vec::new();
    for i in 0..30 {
        // A fresh "client address" every time: not trusted by default.
        statuses.push(login("wrong-password-xx", format!("198.51.100.{i}")).await.status);
    }
    assert!(statuses[..5].iter().all(|s| *s == 401), "{statuses:?}");
    assert!(
        statuses[5..].iter().all(|s| matches!(s, 423 | 429)),
        "account locked after 5 failures: {statuses:?}"
    );
    let r = login(USER_PASSWORD, "192.0.2.1".into()).await;
    assert_eq!(r.status, 429, "the IP budget is spent: {}", r.text());
    assert!(r.header("retry-after").is_some());
    assert!(!r.text().contains("access_token"));
    // Unknown accounts cost the same budget (no free enumeration).
    let r = env
        .req(
            "POST",
            "/api/auth/login",
            &[],
            Some(json!({"email": "nobody@example.com", "password": "x"})),
        )
        .await;
    assert_eq!(r.status, 429);
    env.shutdown().await;
}

/// FB-81 finding: an attacker behind the administrator's address (NAT,
/// proxy) used up the shared per-IP budget and locked admin sign-in out.
#[tokio::test]
async fn an_attacker_on_the_admins_address_cannot_lock_the_admin_out() {
    let env = Env::new().await;
    let ci = forgebase_auth::ClientInfo::default();
    for email in ["boss@example.com", "other-admin@example.com"] {
        env.auth.create_admin(email, ADMIN_PASSWORD, "test", &ci).await.unwrap();
    }
    env.auth
        .signup(&ci, None, "colleague@example.com", USER_PASSWORD, None)
        .await
        .unwrap();
    let admin = |email: &'static str, pw: &'static str, cookie: Option<String>| {
        let env = &env;
        async move {
            let c = cookie.unwrap_or_default();
            let h: Vec<(&str, &str)> = if c.is_empty() {
                vec![]
            } else {
                vec![("Cookie", c.as_str())]
            };
            env.req(
                "POST",
                "/api/auth/admins/login",
                &h,
                Some(json!({"email": email, "password": pw})),
            )
            .await
        }
    };
    let user = |pw: &'static str| {
        let env = &env;
        async move {
            env.req(
                "POST",
                "/api/auth/login",
                &[],
                Some(json!({"email": "colleague@example.com", "password": pw})),
            )
            .await
            .status
        }
    };
    // Colleagues behind the same address sign in all day: successes do not
    // spend the per-IP budget.
    for _ in 0..40 {
        assert_eq!(user(USER_PASSWORD).await, 200);
    }
    // The administrator signs in once and gets a known-device cookie.
    let r = admin("boss@example.com", ADMIN_PASSWORD, None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let set = r
        .headers
        .iter()
        .find(|(k, v)| k == "set-cookie" && v.starts_with("fb_admin_device="))
        .map(|(_, v)| v.clone())
        .expect("known-device cookie");
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Strict"), "{set}");
    assert!(set.contains("Path=/api/auth/admins/login"), "{set}");
    let device = set.split(';').next().unwrap().to_owned();

    // 1. The attacker floods end-user sign-in: that budget is spent ...
    let mut last = 0;
    for _ in 0..31 {
        last = user("wrong-password-xx").await;
    }
    assert_eq!(last, 429);
    // ... but admin sign-in has its own.
    assert_eq!(admin("boss@example.com", ADMIN_PASSWORD, None).await.status, 200);

    // 2. The attacker fails on purpose with the administrator's email: the
    //    account locks for unknown browsers, not for the admin's own.
    for _ in 0..6 {
        admin("boss@example.com", "not-the-password-1", None).await;
    }
    let r = admin("boss@example.com", ADMIN_PASSWORD, None).await;
    assert!(matches!(r.status, 423 | 429), "locked without the device: {}", r.text());
    let r = admin("boss@example.com", ADMIN_PASSWORD, Some(device.clone())).await;
    assert_eq!(r.status, 200, "known device: {}", r.text());

    // 3. The attacker floods admin sign-in from the address.
    let mut last = 0;
    for _ in 0..31 {
        last = admin("other-admin@example.com", "nope-nope-nope-1", None).await.status;
    }
    assert_eq!(last, 429, "the per-IP admin budget still stops guessing");
    let r = admin("boss@example.com", ADMIN_PASSWORD, None).await;
    assert_eq!(r.status, 429, "unknown browser on the flooded address");
    let r = admin("boss@example.com", ADMIN_PASSWORD, Some(device.clone())).await;
    assert_eq!(r.status, 200, "known device on the flooded address: {}", r.text());

    // 4. Forged and foreign device tokens are worth nothing.
    for forged in [
        "fb_admin_device=1700000000.AAAA".to_owned(),
        device.replace("fb_admin_device=", "fb_admin_device=9"),
    ] {
        let r = admin("boss@example.com", ADMIN_PASSWORD, Some(forged.clone())).await;
        assert_eq!(r.status, 429, "{forged}: {}", r.text());
    }
    let r = admin("other-admin@example.com", ADMIN_PASSWORD, Some(device.clone())).await;
    assert_eq!(r.status, 429, "a device token is bound to its admin's email");

    // 5. A known device is not a guessing oracle: five failures spend it.
    for _ in 0..5 {
        let r = admin("boss@example.com", "still-not-it-123", Some(device.clone())).await;
        assert_eq!(r.status, 401, "{}", r.text());
    }
    let r = admin("boss@example.com", ADMIN_PASSWORD, Some(device)).await;
    assert_eq!(r.status, 429, "the device's own budget is spent");
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// CORS and CSRF defaults
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cors_is_closed_and_cookie_writes_need_the_csrf_token() {
    let env = Env::new().await;
    env.auth
        .create_admin(
            "cookie@example.com",
            ADMIN_PASSWORD,
            "test",
            &forgebase_auth::ClientInfo::default(),
        )
        .await
        .unwrap();
    let evil = ("Origin", "https://evil.example");
    let r = env.req("GET", "/api/v1/collections", &[evil], None).await;
    assert!(r.header("access-control-allow-origin").is_none(), "{:?}", r.headers);
    let r = env
        .req(
            "OPTIONS",
            "/api/v1/collections",
            &[
                evil,
                ("Access-Control-Request-Method", "POST"),
                ("Access-Control-Request-Headers", "authorization"),
            ],
            None,
        )
        .await;
    assert!(
        r.header("access-control-allow-origin").is_none(),
        "preflight: {:?}",
        r.headers
    );
    assert!(r.header("access-control-allow-credentials").is_none());

    let r = env
        .req(
            "POST",
            "/api/auth/admins/login",
            &[],
            Some(json!({"email": "cookie@example.com", "password": ADMIN_PASSWORD, "cookie": true})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(
        r.json().get("access_token").is_none_or(Json::is_null),
        "cookie mode keeps tokens out of JS"
    );
    let set = r.header("set-cookie").unwrap().to_owned();
    assert!(set.contains("HttpOnly"), "{set}");
    assert!(set.to_ascii_lowercase().contains("samesite="), "{set}");
    let cookie = set.split(';').next().unwrap().to_owned();
    let csrf = r.json()["csrf_token"].as_str().unwrap().to_owned();
    let body = json!({"name": "csrf_probe", "fields": []});
    let r = env
        .req(
            "POST",
            "/api/v1/collections",
            &[("Cookie", &cookie), evil],
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, 403, "cookie write without X-CSRF-Token: {}", r.text());
    assert!(r.text().contains("csrf_failed"));
    let r = env
        .req(
            "POST",
            "/api/v1/collections",
            &[("Cookie", &cookie), ("X-CSRF-Token", "0000")],
            Some(body.clone()),
        )
        .await;
    assert_eq!(r.status, 403, "wrong token");
    let r = env
        .req("GET", "/api/v1/collections/csrf_probe", &[("Cookie", &cookie)], None)
        .await;
    assert_eq!(r.status, 404, "the forged writes did nothing");
    let r = env
        .req(
            "POST",
            "/api/v1/collections",
            &[("Cookie", &cookie), ("X-CSRF-Token", &csrf)],
            Some(body),
        )
        .await;
    assert_eq!(r.status, 201, "with the token: {}", r.text());
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Secrets never in logs or API output
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Every log line of this test binary, at TRACE.
fn logs() -> &'static Capture {
    static LOGS: OnceLock<Capture> = OnceLock::new();
    LOGS.get_or_init(|| {
        let c = Capture::default();
        let sub = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(c.clone())
            .with_ansi(false)
            .finish();
        let _ = tracing::subscriber::set_global_default(sub);
        c
    })
}

#[tokio::test]
async fn secrets_never_reach_logs_or_api_output() {
    let logs = logs();
    let env = Env::new().await;
    let admin = env.admin("secret-admin@example.com").await;
    let (user, _) = env.user("secret-user@example.com").await;
    let key = env.admin_key(&admin, &["*"]).await;
    let mut secrets = vec![
        ADMIN_PASSWORD.to_owned(),
        USER_PASSWORD.to_owned(),
        admin.clone(),
        user.clone(),
        key.clone(),
    ];
    let mut outputs = Vec::new();

    // A failed sign-in with a password-like value, a refresh, a PAT.
    let wrong = "Wrong-But-Secret-Pa55word";
    secrets.push(wrong.to_owned());
    outputs.push(
        env.req(
            "POST",
            "/api/auth/login",
            &[],
            Some(json!({"email": "secret-user@example.com", "password": wrong})),
        )
        .await,
    );
    let r = env
        .req(
            "POST",
            "/api/auth/login",
            &[],
            Some(json!({"email": "secret-user@example.com", "password": USER_PASSWORD})),
        )
        .await;
    let refresh = r.json()["refresh_token"].as_str().unwrap().to_owned();
    secrets.push(refresh.clone());
    let r = env
        .req(
            "POST",
            "/api/auth/refresh",
            &[],
            Some(json!({"refresh_token": refresh})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    secrets.push(r.json()["refresh_token"].as_str().unwrap().to_owned());
    secrets.push(r.json()["access_token"].as_str().unwrap().to_owned());
    let r = env
        .as_(
            &user,
            "POST",
            "/api/auth/tokens",
            &[],
            Some(json!({"name": "ci", "scopes": ["collections:read"]})),
        )
        .await;
    let pat = r.json()["key"].as_str().unwrap().to_owned();
    secrets.push(pat.clone());

    // A webhook signing secret and a function secret.
    env.as_(&admin, "POST", "/api/v1/queues/topics", &[], Some(json!({"name": "t"})))
        .await;
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/queues/subscriptions",
            &[],
            Some(json!({"name": "s", "topic": "t",
                        "push": {"kind": "webhook", "endpoint": "https://hooks.example.com/x"}})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    if let Some(s) = r.json()["push_secret"].as_str() {
        secrets.push(s.to_owned());
    }
    let fsecret = "function-secret-value-93ad";
    secrets.push(fsecret.to_owned());
    env.server
        .functions()
        .deploy(
            &forgebase_data::AccessContext::admin("root"),
            forgebase_functions::DeployRequest {
                name: "sf".into(),
                wasm: Some(fetch_module("https://example.com/")),
                source: None,
                language: None,
                description: None,
                access: None,
                capabilities: vec!["fetch".into(), "env".into()],
                fetch_hosts: vec!["example.com".into()],
                limits: forgebase_functions::LimitsInput::default(),
                env: Default::default(),
                hooks: Vec::new(),
                activate: true,
            },
        )
        .await
        .unwrap();
    let r = env
        .as_(
            &admin,
            "PUT",
            "/api/v1/functions/sf/secrets/API_TOKEN",
            &[],
            Some(json!({"value": fsecret})),
        )
        .await;
    assert!(r.status < 300, "{}", r.text());

    // Realm SMS gateway and OAuth provider secrets set through the admin API.
    let (sms_token, oauth_secret) = ("sms-bearer-secret-5be1", "oauth-client-secret-77c0");
    secrets.push(sms_token.to_owned());
    secrets.push(oauth_secret.to_owned());
    let r = env
        .as_(
            &admin,
            "PUT",
            "/api/auth/realms/default/sms",
            &[],
            Some(json!({"provider": "webhook", "url": "https://sms.example.com/send", "bearer_token": sms_token})),
        )
        .await;
    assert!(r.status < 300, "{}", r.text());
    let r = env
        .as_(
            &admin,
            "PUT",
            "/api/auth/realms/default/oauth-providers/gh",
            &[],
            Some(json!({"kind": "github", "client_id": "cid", "client_secret": oauth_secret})),
        )
        .await;
    assert!(r.status < 300, "{}", r.text());
    outputs.push(r);

    // Everything a client can read back.
    for (token, path) in [
        (&admin, "/api/auth/api-keys"),
        (&admin, "/api/auth/service-accounts"),
        (&admin, "/api/auth/audit"),
        (&user, "/api/auth/tokens"),
        (&user, "/api/auth/sessions"),
        (&user, "/api/auth/me"),
        (&admin, "/api/v1/queues/subscriptions/s"),
        (&admin, "/api/v1/queues/subscriptions"),
        (&admin, "/api/v1/functions/sf/secrets"),
        (&admin, "/api/v1/functions/sf"),
        (&admin, "/api/v1/authz/audit"),
        (&admin, "/api/auth/realms/default"),
        (&admin, "/api/auth/realms/default/sms"),
        (&admin, "/api/auth/realms/default/oauth-providers"),
        (&admin, "/api/auth/realms/default/oauth-providers/gh"),
        (&admin, "/api/auth/realms/default/keys"),
        (&user, "/api/auth/oauth/providers"),
        (&admin, "/api/system/info"),
        (&admin, "/metrics"),
    ] {
        let r = env.as_(token, "GET", path, &[], None).await;
        assert!(r.status < 400, "{path}: {} {}", r.status, r.text());
        outputs.push(r);
    }
    // Bad credentials are not echoed in errors.
    let bogus = "fbk_bogus_secret_value_1234567890";
    outputs.push(env.as_(bogus, "GET", "/api/auth/me", &[], None).await);
    secrets.push(bogus.to_owned());
    outputs.push(
        env.req("GET", &format!("/api/v1/collections?token={pat}"), &[], None)
            .await,
    );

    for o in &outputs {
        assert!(!o.raw().contains("PRIVATE KEY"), "signing keys stay private");
    }
    env.shutdown().await;
    let log = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
    assert!(log.contains("forgebase started"), "the capture works");
    for s in &secrets {
        assert!(
            !log.contains(s.as_str()),
            "a secret reached the logs: {}…",
            &s[..s.len().min(12)]
        );
        // The user's own password and tokens are expected nowhere in output,
        // except the access token header the client sent (not echoed).
        for o in &outputs {
            assert!(
                !o.raw().contains(s.as_str()),
                "a secret reached a response: {}…",
                &s[..s.len().min(12)]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Malicious inputs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn malicious_inputs_are_refused_without_errors_or_leaks() {
    let env = Env::new().await;
    let admin = env.admin("inputs@example.com").await;
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/collections",
            &[],
            Some(json!({"name": "items", "fields": [{"name": "title", "type": "text"}]})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let (user, _) = env.user("inputs-user@example.com").await;
    let long = "a".repeat(40 * 1024);
    let probes: Vec<(String, String, Option<Json>)> = vec![
        (
            "GET".into(),
            "/api/v1/collections/items/records?filter=title%20%3D%20''%20OR%201%3D1%20--".into(),
            None,
        ),
        (
            "GET".into(),
            "/api/v1/collections/items/records?filter=1%3D1%3B%20DROP%20TABLE%20items".into(),
            None,
        ),
        (
            "GET".into(),
            "/api/v1/collections/items/records?sort=(SELECT%201)".into(),
            None,
        ),
        (
            "GET".into(),
            "/api/v1/collections/items/records?fields=*,sqlite_master.sql".into(),
            None,
        ),
        (
            "GET".into(),
            format!("/api/v1/collections/items/records?filter={long}"),
            None,
        ),
        ("GET".into(), "/api/v1/collections/sqlite_master/records".into(), None),
        (
            "GET".into(),
            "/api/v1/collections/_forgebase_auth_sessions/records".into(),
            None,
        ),
        ("GET".into(), "/api/v1/collections/items%27%3B--/records".into(), None),
        (
            "GET".into(),
            "/api/v1/collections/items/records/..%2F..%2F..%2Fetc%2Fpasswd".into(),
            None,
        ),
        (
            "POST".into(),
            "/api/v1/collections/items/records".into(),
            Some(json!({"title": {"$gt": ""}})),
        ),
        (
            "POST".into(),
            "/api/v1/collections/items/records".into(),
            Some(json!({"id": "x' OR '1'='1", "title": "t"})),
        ),
        (
            "POST".into(),
            "/api/v1/collections/items/records/query".into(),
            Some(json!({"filter": "title ~ ?1", "params": [";"]})),
        ),
    ];
    for (method, path, body) in probes {
        for token in [&admin, &user] {
            let r = env.as_(token, &method, &path, &[], body.clone()).await;
            assert!(r.status < 500, "{method} {path}: {} {}", r.status, r.text());
            // Echoing the attacker's own input is fine; server internals are not.
            let t = r.text().to_ascii_lowercase();
            let dir = env.dir.path().display().to_string().to_ascii_lowercase();
            for leak in [
                "panicked",
                ".rs:",
                "sqlite_error",
                "no such table",
                "no such column",
                "near \"",
                &dir,
            ] {
                assert!(!t.contains(leak), "{method} {path} leaks {leak:?}: {t}");
            }
        }
    }
    // Oversized and malformed bodies.
    let big = vec![b'['; 5 * 1024 * 1024];
    let auth = bearer(&admin);
    let r = send(
        env.addr,
        "POST",
        "/api/v1/collections/items/records",
        &[("Authorization", &auth)],
        &big,
    )
    .await;
    assert_eq!(r.status, 413, "{}", r.text());
    let deep = format!("{}{}", "[".repeat(100_000), "]".repeat(100_000));
    let r = send(
        env.addr,
        "POST",
        "/api/v1/collections/items/records",
        &[("Authorization", &auth)],
        deep.as_bytes(),
    )
    .await;
    assert!(matches!(r.status, 400 | 413), "deep nesting: {}", r.status);
    let r = send(env.addr, "POST", "/api/auth/login", &[], b"{\"email\":").await;
    assert_eq!(r.status, 400);
    // The server still answers.
    assert_eq!(env.req("GET", "/health", &[], None).await.status, 200);
    env.shutdown().await;
}
