//! FB-138 (rls-coverage G6, G7, G8): key-value and Redis-protocol tenancy
//! and namespace rules, over HTTP and RESP against the real server. Each test
//! failed before FB-138: key tenant bindings were ignored, RESP sessions and
//! callers without a tenant shared one `default` scope, namespaces had no
//! permissions or rules of their own, and `kv_admin` holders saw engine-wide
//! statistics with one global eviction.
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use security_common::Env;
use serde_json::{Value as Json, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A user's organization with one project; returns (org id, production tenant).
async fn org(env: &Env, token: &str, slug: &str) -> (String, String) {
    let r = env
        .as_(token, "POST", "/api/v1/orgs", &[], Some(json!({"slug": slug, "name": slug})))
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

/// A realm role bound to `principal` (as an administrator).
async fn realm_role(env: &Env, admin: &str, principal: &str, key: &str, allow: &[&str], deny: &[&str]) {
    let r = env
        .as_(
            admin,
            "PUT",
            "/api/v1/roles",
            &[],
            Some(json!({"key": key, "allow": allow, "deny": deny})),
        )
        .await;
    assert_eq!(r.status, 200, "role: {}", r.text());
    let r = env
        .as_(
            admin,
            "PUT",
            "/api/v1/realm-bindings",
            &[],
            Some(json!({"principal": principal, "roles": [key]})),
        )
        .await;
    assert_eq!(r.status, 200, "binding: {}", r.text());
}

/// `POST /api/auth/tokens` as `token`; returns the response.
async fn pat(env: &Env, token: &str, tenant: Option<&str>) -> security_common::Resp {
    let mut body = json!({"name": "fb138", "scopes": ["*"]});
    if let Some(t) = tenant {
        body["tenant"] = json!(t);
    }
    env.as_(token, "POST", "/api/auth/tokens", &[], Some(body)).await
}

async fn put(env: &Env, token: &str, tenant: Option<&str>, ns: &str, key: &str, value: &str) -> security_common::Resp {
    let h: Vec<(&str, &str)> = tenant.map(|t| ("X-ForgeBase-Tenant", t)).into_iter().collect();
    env.as_(
        token,
        "PUT",
        &format!("/api/v1/kv/{ns}/strings/{key}"),
        &h,
        Some(json!({"value": value})),
    )
    .await
}

async fn get(env: &Env, token: Option<&str>, tenant: Option<&str>, ns: &str, key: &str) -> security_common::Resp {
    let h: Vec<(&str, &str)> = tenant.map(|t| ("X-ForgeBase-Tenant", t)).into_iter().collect();
    let path = format!("/api/v1/kv/{ns}/keys/{key}");
    match token {
        Some(t) => env.as_(t, "GET", &path, &h, None).await,
        None => env.req("GET", &path, &h, None).await,
    }
}

// ---------------------------------------------------------------------------
// G6: tenant binding of keys, tenant_required, shared namespaces
// ---------------------------------------------------------------------------

#[tokio::test]
async fn g6_a_tenant_bound_key_acts_in_its_tenant_only() {
    let env = Env::new().await;
    let (alice, _) = env.user("alice@example.com").await;
    let (bob, bob_pk) = env.user("bob@example.com").await;
    let (org_a, ta) = org(&env, &alice, "alpha").await;
    let (_org_b, tb) = org(&env, &bob, "bravo").await;

    // Created: the owner must be able to use the tenant (policy step 2).
    let r = pat(&env, &alice, Some(&tb)).await;
    assert_eq!(r.status, 403, "binding to bob's tenant: {}", r.text());
    let r = pat(&env, &alice, Some("no-such-tenant")).await;
    assert_eq!(r.status, 403, "binding to an unknown tenant: {}", r.text());
    let r = pat(&env, &alice, Some(&ta)).await;
    assert_eq!(r.status, 201, "{}", r.text());
    assert_eq!(r.json()["tenant"], json!(ta));
    let key = r.json()["key"].as_str().unwrap().to_owned();

    // Used without a header: the request acts in the bound tenant.
    let r = put(&env, &key, None, "cache", "k", "from-the-key").await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = get(&env, Some(&alice), Some(&ta), "cache", "k").await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["value"], "from-the-key");
    // Another tenant is refused, even one the owner may use.
    let r = get(&env, Some(&key), Some(&tb), "cache", "k").await;
    assert_eq!(r.status, 403, "{}", r.text());
    assert!(r.text().contains("tenant_mismatch"), "{}", r.text());
    let r = env.as_(&key, "GET", "/api/auth/me", &[], None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["tenant"], json!(ta));

    // Used: a key whose owner left the tenant's organization stops working.
    let oh = [("X-ForgeBase-Org", org_a.as_str())];
    let r = env
        .as_(
            &alice,
            "PUT",
            &format!("/api/v1/orgs/{org_a}/members"),
            &oh,
            Some(json!({"principal": bob_pk, "roles": ["owner"]})),
        )
        .await;
    assert!(r.status < 300, "add bob: {}", r.text());
    let r = pat(&env, &bob, Some(&ta)).await;
    assert_eq!(r.status, 201, "bob, now a member: {}", r.text());
    let bob_key = r.json()["key"].as_str().unwrap().to_owned();
    assert_eq!(get(&env, Some(&bob_key), None, "cache", "k").await.status, 200);
    let r = env
        .as_(
            &alice,
            "POST",
            &format!("/api/v1/orgs/{org_a}/members/remove"),
            &oh,
            Some(json!({"principal": bob_pk})),
        )
        .await;
    assert!(r.status < 300, "remove bob: {}", r.text());
    let r = get(&env, Some(&bob_key), None, "cache", "k").await;
    assert_eq!(r.status, 401, "membership ended: {}", r.text());
    env.shutdown().await;
}

#[tokio::test]
async fn g6_callers_without_a_tenant_need_one_or_a_shared_namespace() {
    let env = Env::new().await;
    let admin = env.admin("fb138-admin@example.com").await;
    let (alice, alice_pk) = env.user("alice@example.com").await;
    let (bob, bob_pk) = env.user("bob@example.com").await;
    let (_, ta) = org(&env, &alice, "alpha").await;
    for pk in [&alice_pk, &bob_pk] {
        realm_role(&env, &admin, pk, "kv_rw", &["engine:kv_read", "engine:kv_write"], &[]).await;
    }

    // A tenant namespace without a tenant: refused, not a shared bucket.
    let r = put(&env, &alice, None, "cache", "k", "x").await;
    assert_eq!(r.status, 400, "{}", r.text());
    assert_eq!(r.code(), "tenant_required");
    assert_eq!(get(&env, Some(&bob), None, "cache", "k").await.code(), "tenant_required");

    // An explicit shared namespace is realm-wide: the same keys with or
    // without a tenant.
    assert_eq!(put(&env, &alice, None, "shared.cache", "k", "hello").await.status, 200);
    let r = get(&env, Some(&bob), None, "shared.cache", "k").await;
    assert_eq!(r.json()["value"], "hello", "{}", r.text());
    let r = get(&env, Some(&alice), Some(&ta), "shared.cache", "k").await;
    assert_eq!(r.json()["value"], "hello", "{}", r.text());
    assert_eq!(get(&env, Some(&alice), Some(&ta), "cache", "k").await.status, 404);
    let r = env.as_(&alice, "GET", "/api/v1/kv/_namespaces", &[], None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let names: Vec<String> = r.json()["namespaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, vec!["shared.cache".to_owned()]);

    // Privileged callers keep the legacy shared scope.
    assert_eq!(put(&env, &admin, None, "cache", "k", "admin").await.status, 200);
    assert_eq!(get(&env, Some(&admin), None, "cache", "k").await.json()["value"], "admin");
    env.shutdown().await;
}

/// One RESP command over a raw connection; returns the reply's first line
/// (and the payload line of a bulk reply).
async fn resp(c: &mut tokio::net::TcpStream, args: &[&str]) -> String {
    let mut out = format!("*{}\r\n", args.len());
    for a in args {
        out.push_str(&format!("${}\r\n{a}\r\n", a.len()));
    }
    c.write_all(out.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        c.read_exact(&mut byte).await.unwrap();
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n") {
            let line = String::from_utf8_lossy(&buf).trim_end().to_owned();
            if line.starts_with('$') && line != "$-1" {
                buf.clear();
                continue;
            }
            return line;
        }
    }
}

#[tokio::test]
async fn g6_resp_sessions_run_in_the_key_tenant() {
    let env = Env::with(|c| {
        c.resp.enabled = true;
        c.resp.port = 0;
    })
    .await;
    let admin = env.admin("fb138-resp@example.com").await;
    let (alice, alice_pk) = env.user("alice@example.com").await;
    let (_, ta) = org(&env, &alice, "alpha").await;
    realm_role(&env, &admin, &alice_pk, "kv_rw", &["engine:kv_read", "engine:kv_write"], &[]).await;
    let bound = pat(&env, &alice, Some(&ta)).await.json()["key"].as_str().unwrap().to_owned();
    let unbound = pat(&env, &alice, None).await.json()["key"].as_str().unwrap().to_owned();
    let addr = env.server.resp_addr().expect("listener is enabled");

    let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
    assert_eq!(resp(&mut c, &["AUTH", &bound]).await, "+OK");
    assert_eq!(resp(&mut c, &["SET", "k", "over-resp"]).await, "+OK");
    // The key landed in the tenant, where HTTP callers of the tenant see it.
    let r = get(&env, Some(&alice), Some(&ta), "default", "k").await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["value"], "over-resp");

    // An unbound key: tenant namespaces need a tenant; shared ones work.
    let mut u = tokio::net::TcpStream::connect(addr).await.unwrap();
    assert_eq!(resp(&mut u, &["AUTH", &unbound]).await, "+OK");
    let e = resp(&mut u, &["SET", "k", "v"]).await;
    assert!(e.starts_with("-NOPERM tenant_required"), "{e}");
    assert_eq!(resp(&mut u, &["SELECT", "shared.cache"]).await, "+OK");
    assert_eq!(resp(&mut u, &["SET", "k", "v"]).await, "+OK");
    assert_eq!(resp(&mut u, &["GET", "k"]).await, "v");
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// G7: namespace permissions and rules (FB-134 framework)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn g7_namespace_permissions_and_rules_apply_to_kv() {
    let env = Env::new().await;
    let admin = env.admin("fb138-g7@example.com").await;
    let (alice, _) = env.user("alice@example.com").await;
    let (bob, bob_pk) = env.user("bob@example.com").await;
    let (carol, carol_pk) = env.user("carol@example.com").await;
    let (_, ta) = org(&env, &alice, "alpha").await;
    realm_role(
        &env,
        &admin,
        &bob_pk,
        "kv_public",
        &["engine:kv_read:shared.public", "engine:kv_write:shared.public"],
        &[],
    )
    .await;
    realm_role(
        &env,
        &admin,
        &carol_pk,
        "kv_all_but_secrets",
        &["engine:kv_read", "engine:kv_write"],
        &["engine:kv_read:shared.secret*"],
    )
    .await;
    for ns in ["shared.public", "shared.secret1", "shared.open", "shared.other"] {
        assert_eq!(put(&env, &admin, None, ns, "k", ns).await.status, 200);
    }

    // A resource key grants its namespace only.
    let r = get(&env, Some(&bob), None, "shared.public", "k").await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(put(&env, &bob, None, "shared.public", "k2", "v").await.status, 200);
    assert_eq!(get(&env, Some(&bob), None, "shared.other", "k").await.status, 403);
    // A denied resource key beats the plain key.
    assert_eq!(get(&env, Some(&carol), None, "shared.secret1", "k").await.status, 403);
    assert_eq!(get(&env, Some(&carol), None, "shared.public", "k").await.status, 200);

    // A namespace rule opens reads to everyone, writes stay closed.
    let r = env
        .as_(
            &admin,
            "PUT",
            "/api/v1/policies/resources/kv_namespace/shared.open",
            &[],
            Some(json!({"rules": {"read": ""}})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = get(&env, None, None, "shared.open", "k").await;
    assert_eq!(r.status, 200, "anonymous read through the rule: {}", r.text());
    let r = env
        .req(
            "PUT",
            "/api/v1/kv/shared.open/strings/k",
            &[],
            Some(json!({"value": "evil"})),
        )
        .await;
    assert_eq!(r.status, 401, "{}", r.text());
    // Rules are namespace-level: item fields are not part of the KV schema.
    let r = env
        .as_(
            &admin,
            "PUT",
            "/api/v1/policies/resources/kv_namespace/shared.x",
            &[],
            Some(json!({"rules": {"read": "key = 'x'"}})),
        )
        .await;
    assert_eq!(r.status, 400, "{}", r.text());

    // A function's private namespace is not reachable directly.
    let r = env
        .as_(&alice, "GET", "/api/v1/kv/fn.billing/keys", &[("X-ForgeBase-Tenant", &ta)], None)
        .await;
    assert_eq!(r.status, 403, "{}", r.text());
    let r = env
        .as_(&admin, "GET", "/api/v1/kv/fn.billing/keys", &[("X-ForgeBase-Tenant", &ta)], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// G8: per-tenant statistics and memory budget
// ---------------------------------------------------------------------------

#[tokio::test]
async fn g8_stats_are_per_tenant_and_the_tenant_budget_evicts_only_its_own_keys() {
    let env = Env::with(|c| c.kv.tenant_max_bytes = 4096).await;
    let (alice, _) = env.user("alice@example.com").await;
    let (bob, _) = env.user("bob@example.com").await;
    let (_, ta) = org(&env, &alice, "alpha").await;
    let (_, tb) = org(&env, &bob, "bravo").await;
    let big = "x".repeat(1000);
    for k in ["b0", "b1"] {
        assert_eq!(put(&env, &bob, Some(&tb), "cache", k, &big).await.status, 200);
    }
    for i in 0..10 {
        let r = put(&env, &alice, Some(&ta), "cache", &format!("a{i}"), &big).await;
        assert_eq!(r.status, 200, "{}", r.text());
    }

    let r = env
        .as_(&alice, "GET", "/api/v1/kv/_info", &[("X-ForgeBase-Tenant", &ta)], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let s: Json = r.json();
    assert_eq!(s["scope"], "tenant", "{s}");
    assert_eq!(s["tenant"], json!(ta));
    assert_eq!(s["max_memory_bytes"], 4096);
    assert!(s["used_memory_bytes"].as_u64().unwrap() <= 4096, "{s}");
    let keys = s["keys"].as_u64().unwrap();
    assert!((1..10).contains(&keys), "only alpha's keys, some evicted: {s}");
    assert!(s["evicted_keys"].as_u64().unwrap() >= 10 - keys, "{s}");

    // Alice's oldest keys went; bob's were never touched.
    assert_eq!(get(&env, Some(&alice), Some(&ta), "cache", "a0").await.status, 404);
    assert_eq!(get(&env, Some(&alice), Some(&ta), "cache", "a9").await.status, 200);
    for k in ["b0", "b1"] {
        assert_eq!(get(&env, Some(&bob), Some(&tb), "cache", k).await.status, 200, "{k}");
    }
    let r = env
        .as_(&bob, "GET", "/api/v1/kv/_info", &[("X-ForgeBase-Tenant", &tb)], None)
        .await;
    assert_eq!(r.json()["keys"], 2, "{}", r.text());
    assert_eq!(r.json()["evicted_keys"], 0, "{}", r.text());
    env.shutdown().await;
}
