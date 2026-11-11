//! FB-136 (rls-coverage G17, G18): billing, credits and realtime scoping,
//! against the real server. Each test failed before FB-136: a realm-level
//! billing manager reached other realms' organizations and saw them in the
//! "all customers" lists, realtime channel names were one instance-wide
//! namespace (tenants met on the same name, anyone could publish on any
//! channel the rules allowed), channel rules did not exist, and an API key
//! sent in-band (`?access_token=`) lost its tenant binding.
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use std::time::Duration;

use security_common::{Env, bearer};
use serde_json::{Value as Json, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A user's organization with one project; returns (org id, production tenant).
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

/// A realm role bound to `principal` (as an administrator).
async fn realm_role(env: &Env, admin: &str, principal: &str, key: &str, allow: &[&str]) {
    let r = env
        .as_(
            admin,
            "PUT",
            "/api/v1/roles",
            &[],
            Some(json!({"key": key, "allow": allow, "deny": []})),
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

// ---------------------------------------------------------------------------
// G18: billing and credits stay in the caller's realm
// ---------------------------------------------------------------------------

/// `method path` as an administrator, expecting `status`.
async fn admin_ok(env: &Env, admin: &str, method: &str, path: &str, body: Json, status: u16) -> Json {
    let r = env.as_(admin, method, path, &[], Some(body)).await;
    assert_eq!(r.status, status, "{method} {path}: {}", r.text());
    r.json()
}

/// A customer and an open manual invoice for `subject`; returns the invoice id.
async fn billed(env: &Env, admin: &str, subject: &str, key: &str) -> String {
    admin_ok(
        env,
        admin,
        "PUT",
        "/api/v1/billing/customer",
        json!({"subject": subject, "email": format!("{key}@example.test"), "currency": "USD", "provider": "manual"}),
        200,
    )
    .await;
    let c = admin_ok(
        env,
        admin,
        "POST",
        "/api/v1/billing/orders",
        json!({"subject": subject, "items": [{"price": "gizmo-once", "quantity": 1}], "idempotency_key": key}),
        201,
    )
    .await;
    c["invoice"]["id"]
        .as_str()
        .or_else(|| c["order"]["invoice_id"].as_str())
        .unwrap_or_else(|| panic!("no invoice: {c}"))
        .to_owned()
}

#[tokio::test]
async fn g18_realm_billing_managers_stay_in_their_realm() {
    let env = Env::with(|c| c.billing.enabled = true).await;
    let admin = env.admin("fb136-g18@example.com").await;
    let (alice, _) = env.user("alice@example.com").await;
    let (mallory, mallory_pk) = env.user("mallory@example.com").await;
    let (alpha, _) = org(&env, &alice, "alpha").await;
    // Another realm with an organization of its own.
    admin_ok(&env, &admin, "POST", "/api/v1/realms", json!({"slug": "beta", "name": "Beta"}), 201).await;
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/v1/orgs",
            &[("X-ForgeBase-Realm", "beta")],
            Some(json!({"slug": "victim", "name": "Victim"})),
        )
        .await;
    assert_eq!(r.status, 201, "beta org: {}", r.text());
    assert_eq!(r.json()["realm_id"], "beta");
    let victim = r.json()["id"].as_str().unwrap().to_owned();
    // Mallory manages billing at the default realm's level.
    realm_role(&env, &admin, &mallory_pk, "billing_ops", &["billing:*"]).await;

    // A catalog, and one invoice per organization.
    admin_ok(&env, &admin, "PUT", "/api/v1/billing/providers/manual", json!({}), 200).await;
    admin_ok(
        &env,
        &admin,
        "PUT",
        "/api/v1/billing/products/gizmo",
        json!({"name": "Gizmo", "kind": "service"}),
        200,
    )
    .await;
    admin_ok(
        &env,
        &admin,
        "PUT",
        "/api/v1/billing/prices/gizmo-once",
        json!({"product": "gizmo", "type": "one_time", "currency": "USD", "unit_amount": 500}),
        200,
    )
    .await;
    let alpha_s = format!("org:{alpha}");
    let victim_s = format!("org:{victim}");
    let alpha_inv = billed(&env, &admin, &alpha_s, "alpha-1").await;
    let victim_inv = billed(&env, &admin, &victim_s, "victim-1").await;

    // Her own realm's organization: the realm-level grant applies.
    let r = env
        .as_(&mallory, "GET", &format!("/api/v1/billing/customer?subject={alpha_s}"), &[], None)
        .await;
    assert_eq!(r.status, 200, "same realm: {}", r.text());
    // Another realm's organization: no fallback to her realm-level grant.
    let r = env
        .as_(&mallory, "GET", &format!("/api/v1/billing/customer?subject={victim_s}"), &[], None)
        .await;
    assert_eq!(r.status, 403, "cross-realm customer read: {}", r.text());
    assert!(!r.text().contains("victim@example.test"), "{}", r.text());
    let r = env
        .as_(
            &mallory,
            "PUT",
            "/api/v1/billing/customer",
            &[],
            Some(json!({"subject": victim_s, "name": "pwned"})),
        )
        .await;
    assert_eq!(r.status, 403, "cross-realm customer write: {}", r.text());
    let r = env
        .as_(
            &mallory,
            "POST",
            "/api/v1/billing/orders",
            &[],
            Some(json!({"subject": victim_s, "items": [{"price": "gizmo-once", "quantity": 1}], "idempotency_key": "m1"})),
        )
        .await;
    assert_eq!(r.status, 403, "cross-realm order: {}", r.text());

    // "All customers" lists hold her realm's subjects only.
    for path in [
        "/api/v1/billing/customers",
        "/api/v1/billing/invoices",
        "/api/v1/billing/subscriptions",
        "/api/v1/billing/admin/orders",
        "/api/v1/billing/dunning",
    ] {
        let r = env.as_(&mallory, "GET", path, &[], None).await;
        assert_eq!(r.status, 200, "{path}: {}", r.text());
        assert!(!r.text().contains(&victim), "{path} lists another realm: {}", r.text());
        let r = env.as_(&admin, "GET", path, &[], None).await;
        assert_eq!(r.status, 200, "{path} (admin): {}", r.text());
    }
    for path in ["/api/v1/billing/customers", "/api/v1/billing/invoices", "/api/v1/billing/admin/orders"] {
        let r = env.as_(&mallory, "GET", path, &[], None).await;
        assert!(r.text().contains(&alpha), "{path} keeps her realm: {}", r.text());
        let r = env.as_(&admin, "GET", path, &[], None).await;
        assert!(r.text().contains(&victim), "{path}: administrators see every realm");
    }
    let r = env
        .as_(&mallory, "GET", "/api/v1/billing/admin/orders", &[], None)
        .await;
    assert_eq!(r.json()["total"], 1, "orders total counts her realm only: {}", r.text());

    // Operator actions on another realm's invoice: as unknown as a missing one.
    let r = env
        .as_(
            &mallory,
            "POST",
            &format!("/api/v1/billing/invoices/{victim_inv}/mark-paid"),
            &[],
            Some(json!({"reference": "pwned"})),
        )
        .await;
    assert_eq!(r.status, 404, "cross-realm mark-paid: {}", r.text());
    let r = env
        .as_(
            &mallory,
            "POST",
            &format!("/api/v1/billing/invoices/{victim_inv}/void"),
            &[],
            Some(json!({})),
        )
        .await;
    assert_eq!(r.status, 404, "cross-realm void: {}", r.text());
    let r = env
        .as_(&admin, "GET", &format!("/api/v1/billing/invoices/{victim_inv}"), &[], None)
        .await;
    assert_eq!(r.json()["status"], "open", "the victim's invoice is untouched: {}", r.text());
    let r = env
        .as_(
            &mallory,
            "POST",
            &format!("/api/v1/billing/invoices/{alpha_inv}/mark-paid"),
            &[],
            Some(json!({"reference": "wire 1"})),
        )
        .await;
    assert_eq!(r.status, 200, "her realm's invoice: {}", r.text());

    // Credits: the same realm rule for reads and minting.
    let grant = |subject: &str, key: &str| json!({"subject": subject, "amount": 50, "idempotency_key": key});
    let r = env
        .as_(&mallory, "POST", "/api/v1/credits/grants", &[], Some(grant(&victim_s, "g1")))
        .await;
    assert_eq!(r.status, 403, "cross-realm credit grant: {}", r.text());
    let r = env
        .as_(&mallory, "GET", &format!("/api/v1/credits/balance?subject={victim_s}"), &[], None)
        .await;
    assert_eq!(r.status, 403, "cross-realm balance: {}", r.text());
    let r = env
        .as_(&mallory, "POST", "/api/v1/credits/grants", &[], Some(grant(&alpha_s, "g2")))
        .await;
    assert_eq!(r.status, 201, "her realm's credit grant: {}", r.text());
    let r = env
        .as_(&admin, "GET", &format!("/api/v1/credits/balance?subject={victim_s}"), &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["balance"], 0, "nothing was minted for the victim: {}", r.text());
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// G17: channel namespaces, global channels, channel rules
// ---------------------------------------------------------------------------

/// Percent-encodes a query value.
fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// An open SSE stream: what the server sent so far.
#[derive(Debug)]
struct Sse {
    s: tokio::net::TcpStream,
    buf: String,
}

impl Sse {
    /// Reads until `needle` arrives (true) or `wait` passes (false).
    async fn wait_for(&mut self, needle: &str, wait: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + wait;
        let mut chunk = [0u8; 4096];
        while !self.buf.contains(needle) {
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            match tokio::time::timeout(left, self.s.read(&mut chunk)).await {
                Ok(Ok(n)) if n > 0 => self.buf.push_str(&String::from_utf8_lossy(&chunk[..n])),
                _ => return false,
            }
        }
        true
    }
}

/// Opens `GET /api/v1/realtime/sse` with `subscriptions`; returns the status
/// and, on 200, the stream after every subscription is confirmed (the body
/// otherwise).
async fn sse(
    env: &Env,
    token: Option<&str>,
    tenant: Option<&str>,
    query: &str,
    subscriptions: Json,
) -> (u16, Result<Sse, String>) {
    let mut s = tokio::net::TcpStream::connect(env.addr).await.unwrap();
    let mut req = format!(
        "GET /api/v1/realtime/sse?subscriptions={}{query} HTTP/1.1\r\nHost: forgebase\r\nAccept: text/event-stream\r\n",
        enc(&subscriptions.to_string())
    );
    if let Some(t) = token {
        req.push_str(&format!("Authorization: {}\r\n", bearer(t)));
    }
    if let Some(t) = tenant {
        req.push_str(&format!("X-ForgeBase-Tenant: {t}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut st = Sse { s, buf: String::new() };
    assert!(st.wait_for("\r\n\r\n", Duration::from_secs(5)).await, "no response headers");
    let status: u16 = st.buf.split(' ').nth(1).unwrap().parse().unwrap();
    if status != 200 {
        st.wait_for("\"message\"", Duration::from_secs(2)).await;
        return (status, Err(st.buf));
    }
    let n = subscriptions.as_array().map_or(0, Vec::len);
    for _ in 0..50 {
        if st.buf.matches("\"type\":\"subscribed\"").count() >= n {
            break;
        }
        st.wait_for("\"type\":\"subscribed\"", Duration::from_millis(100)).await;
    }
    assert!(
        st.buf.matches("\"type\":\"subscribed\"").count() >= n,
        "subscriptions not confirmed: {}",
        st.buf
    );
    (status, Ok(st))
}

fn channel(id: &str, name: &str) -> Json {
    json!({"id": id, "channel": name})
}

/// `POST /api/v1/realtime/channels/{channel}/messages` as `token` in `tenant`.
async fn publish(env: &Env, token: &str, tenant: Option<&str>, ch: &str, data: Json) -> security_common::Resp {
    let h: Vec<(&str, &str)> = tenant.map(|t| ("X-ForgeBase-Tenant", t)).into_iter().collect();
    env.as_(
        token,
        "POST",
        &format!("/api/v1/realtime/channels/{ch}/messages"),
        &h,
        Some(json!({"data": data})),
    )
    .await
}

const WAIT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn g17_tenants_using_the_same_channel_name_never_meet() {
    let env = Env::new().await;
    let admin = env.admin("fb136-g17@example.com").await;
    let (alice, _) = env.user("alice@example.com").await;
    let (bob, _) = env.user("bob@example.com").await;
    let (_, ta) = org(&env, &alice, "alpha").await;
    let (_, tb) = org(&env, &bob, "bravo").await;

    let (st, bob_rx) = sse(&env, Some(&bob), Some(&tb), "", json!([channel("c", "chat")])).await;
    assert_eq!(st, 200);
    let mut bob_rx = bob_rx.unwrap();
    // Alice publishes on "chat" in her tenant, then bob in his: bob sees
    // his own message only (messages arrive in publish order).
    assert_eq!(publish(&env, &alice, Some(&ta), "chat", json!({"secret": "alpha-only"})).await.status, 202);
    assert_eq!(publish(&env, &bob, Some(&tb), "chat", json!({"marker": "bravo-1"})).await.status, 202);
    assert!(bob_rx.wait_for("bravo-1", WAIT).await, "{}", bob_rx.buf);
    assert!(!bob_rx.buf.contains("alpha-only"), "another tenant's message: {}", bob_rx.buf);
    assert!(bob_rx.buf.contains("\"channel\":\"chat\""), "the name bob used: {}", bob_rx.buf);

    // The explicit tenant form names the same channel, for its tenant only.
    let alpha_chat = format!("t:{ta}:chat");
    let bravo_chat = format!("t:{tb}:chat");
    let (st, body) = sse(&env, Some(&bob), Some(&tb), "", json!([channel("x", &alpha_chat)])).await;
    assert_eq!(st, 403, "bob subscribes to alpha's channel: {body:?}");
    assert!(format!("{body:?}").contains("tenant_mismatch"), "{body:?}");
    let (st, _) = sse(&env, Some(&bob), None, "", json!([channel("x", &alpha_chat)])).await;
    assert_eq!(st, 403, "no tenant at all");
    let r = publish(&env, &bob, Some(&tb), &alpha_chat, json!({"forged": true})).await;
    assert_eq!(r.status, 403, "bob publishes into alpha's channel: {}", r.text());
    let r = env
        .as_(
            &bob,
            "GET",
            &format!("/api/v1/realtime/channels/{alpha_chat}/presence"),
            &[("X-ForgeBase-Tenant", &tb)],
            None,
        )
        .await;
    assert_eq!(r.status, 403, "presence of alpha's channel: {}", r.text());
    // Bob's own explicit name, and an administrator addressing his tenant.
    assert_eq!(publish(&env, &bob, Some(&tb), &bravo_chat, json!({"marker": "bravo-2"})).await.status, 202);
    assert_eq!(publish(&env, &admin, None, &bravo_chat, json!({"marker": "from-admin"})).await.status, 202);
    assert!(bob_rx.wait_for("from-admin", WAIT).await, "{}", bob_rx.buf);
    assert!(bob_rx.buf.contains("bravo-2"), "{}", bob_rx.buf);
    // A plain name without a tenant is not any tenant's channel.
    assert_eq!(publish(&env, &admin, None, "chat", json!({"secret": "plain"})).await.status, 202);
    assert_eq!(publish(&env, &bob, Some(&tb), "chat", json!({"marker": "bravo-3"})).await.status, 202);
    assert!(bob_rx.wait_for("bravo-3", WAIT).await, "{}", bob_rx.buf);
    assert!(!bob_rx.buf.contains("\"plain\""), "{}", bob_rx.buf);
    env.shutdown().await;
}

#[tokio::test]
async fn g17_global_channels_are_shared_and_only_privileged_callers_publish() {
    let env = Env::new().await;
    let admin = env.admin("fb136-global@example.com").await;
    let (alice, _) = env.user("alice@example.com").await;
    let (bob, _) = env.user("bob@example.com").await;
    let (_, ta) = org(&env, &alice, "alpha").await;
    let (_, tb) = org(&env, &bob, "bravo").await;
    let (st, rx) = sse(&env, Some(&bob), Some(&tb), "", json!([channel("g", "global:news")])).await;
    assert_eq!(st, 200);
    let mut rx = rx.unwrap();
    let r = publish(&env, &alice, Some(&ta), "global:news", json!({"forged": "by-alice"})).await;
    assert_eq!(r.status, 403, "a tenant user publishes to every tenant: {}", r.text());
    assert!(r.text().contains("global_channel"), "{}", r.text());
    let r = publish(&env, &alice, None, "global:news", json!({"forged": "by-alice"})).await;
    assert_eq!(r.status, 403, "{}", r.text());
    assert_eq!(publish(&env, &admin, None, "global:news", json!({"n": "announcement"})).await.status, 202);
    assert!(rx.wait_for("announcement", WAIT).await, "{}", rx.buf);
    assert!(!rx.buf.contains("by-alice"), "{}", rx.buf);
    env.shutdown().await;
}

#[tokio::test]
async fn g17_channel_rules_are_resource_rules_and_see_the_tenant() {
    let env = Env::new().await;
    let admin = env.admin("fb136-rules@example.com").await;
    let (alice, _) = env.user("alice@example.com").await;
    let (carol, carol_pk) = env.user("carol@example.com").await;
    let (alpha, ta) = org(&env, &alice, "alpha").await;
    // Carol is an ordinary member: no engine permissions.
    let r = env
        .as_(
            &alice,
            "PUT",
            &format!("/api/v1/orgs/{alpha}/members"),
            &[("X-ForgeBase-Org", &alpha)],
            Some(json!({"principal": carol_pk, "roles": ["member"]})),
        )
        .await;
    assert_eq!(r.status, 200, "member: {}", r.text());
    let (st, _) = sse(&env, Some(&carol), Some(&ta), "", json!([channel("l", "lobby")])).await;
    assert_eq!(st, 403, "no permission and no rule");

    // A channel rule: subscribe in a tenant only, publish never.
    let r = env
        .as_(
            &admin,
            "PUT",
            "/api/v1/policies/resources/realtime_channel/lobby",
            &[],
            Some(json!({"rules": {"view": "@request.tenant != ''", "create": null}})),
        )
        .await;
    assert_eq!(r.status, 200, "channel rules: {}", r.text());
    let (st, rx) = sse(&env, Some(&carol), Some(&ta), "", json!([channel("l", "lobby")])).await;
    assert_eq!(st, 200, "the rule admits a caller with a tenant: {:?}", rx.err());
    let (st, _) = sse(&env, Some(&carol), None, "", json!([channel("l", "lobby")])).await;
    assert_eq!(st, 403, "the rule refuses a caller without a tenant");
    let (st, _) = sse(&env, Some(&carol), Some(&ta), "", json!([channel("o", "other")])).await;
    assert_eq!(st, 403, "a channel without a rule");
    let r = publish(&env, &carol, Some(&ta), "lobby", json!({})).await;
    assert_eq!(r.status, 403, "publishing is locked: {}", r.text());
    // A permission key names the channel without its tenant prefix.
    let r = env
        .as_(
            &alice,
            "PUT",
            &format!("/api/v1/orgs/{alpha}/roles"),
            &[("X-ForgeBase-Org", &alpha)],
            Some(json!({"key": "lobby_writer", "allow": ["engine:channel_publish:lobby"], "deny": []})),
        )
        .await;
    assert_eq!(r.status, 200, "org role: {}", r.text());
    let r = env
        .as_(
            &alice,
            "PUT",
            &format!("/api/v1/orgs/{alpha}/members"),
            &[("X-ForgeBase-Org", &alpha)],
            Some(json!({"principal": carol_pk, "roles": ["member", "lobby_writer"]})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = publish(&env, &carol, Some(&ta), "lobby", json!({"hello": "lobby"})).await;
    assert_eq!(r.status, 202, "{}", r.text());
    let mut rx = rx.unwrap();
    assert!(rx.wait_for("\"hello\":\"lobby\"", WAIT).await, "{}", rx.buf);
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// FB-138 follow-up: in-band keys keep their tenant binding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn in_band_keys_keep_their_tenant_binding() {
    let env = Env::new().await;
    let admin = env.admin("fb136-inband@example.com").await;
    let (alice, _) = env.user("alice@example.com").await;
    let (bob, _) = env.user("bob@example.com").await;
    let (_, ta) = org(&env, &alice, "alpha").await;
    let (_, tb) = org(&env, &bob, "bravo").await;
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
    let r = env
        .as_(
            &alice,
            "POST",
            "/api/auth/tokens",
            &[],
            Some(json!({"name": "rt", "scopes": ["*"], "tenant": ta})),
        )
        .await;
    assert_eq!(r.status, 201, "bound PAT: {}", r.text());
    let pat = r.json()["key"].as_str().unwrap().to_owned();

    // The key travels in the query string (browsers' EventSource): the
    // connection runs in the key's tenant.
    let subs = json!([{"id": "n", "collection": "notes"}, channel("c", "chat")]);
    let (st, rx) = sse(&env, None, None, &format!("&access_token={}", enc(&pat)), subs).await;
    assert_eq!(st, 200, "{:?}", rx.as_ref().err());
    let mut rx = rx.unwrap();
    let r = env
        .as_(
            &alice,
            "POST",
            "/api/v1/collections/notes/records",
            &[("X-ForgeBase-Tenant", &ta)],
            Some(json!({"title": "alpha note"})),
        )
        .await;
    assert_eq!(r.status, 201, "{}", r.text());
    assert!(rx.wait_for("alpha note", WAIT).await, "the key's tenant's records: {}", rx.buf);
    // Channels: the key's tenant namespace, not another tenant's.
    assert_eq!(publish(&env, &bob, Some(&tb), "chat", json!({"secret": "bravo-only"})).await.status, 202);
    assert_eq!(publish(&env, &alice, Some(&ta), "chat", json!({"marker": "alpha-1"})).await.status, 202);
    assert!(rx.wait_for("alpha-1", WAIT).await, "{}", rx.buf);
    assert!(!rx.buf.contains("bravo-only"), "{}", rx.buf);
    env.shutdown().await;
}
