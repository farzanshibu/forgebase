//! FB-141 (rls-coverage G2, G16): queue RLS over HTTP against the real
//! server. Each test failed before FB-141: record-change topics carried every
//! non-password field (hidden ones too) of every row and tenant to anyone
//! with `queue_consume`; topic and subscription names were global across
//! tenants; `engine:queue_<action>:<name>` resource keys were ignored (only
//! the plain key counted); and nobody owned a subscription.
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use std::time::Duration;

use security_common::{Env, Resp};
use serde_json::{Value as Json, json};

/// A user's organization with one project; returns the production tenant.
async fn tenant_of(env: &Env, token: &str, slug: &str) -> String {
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
    p.json()["environments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "production")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// JSON request as `token`, optionally in `tenant`.
async fn call(env: &Env, token: &str, tenant: Option<&str>, method: &str, path: &str, body: Option<Json>) -> Resp {
    let t: Vec<(&str, &str)> = tenant.map(|t| ("X-ForgeBase-Tenant", t)).into_iter().collect();
    env.as_(token, method, path, &t, body).await
}

async fn grant(env: &Env, admin: &str, principal: &str, permission: &str) {
    let r = call(
        env,
        admin,
        None,
        "POST",
        "/api/v1/overrides",
        Some(json!({"principal": principal, "permission": permission, "effect": "allow"})),
    )
    .await;
    assert!(r.status < 300, "override {permission}: {}", r.text());
}

async fn topic(env: &Env, token: &str, tenant: Option<&str>, body: Json) -> Resp {
    call(env, token, tenant, "POST", "/api/v1/queues/topics", Some(body)).await
}

async fn subscribe(env: &Env, token: &str, tenant: Option<&str>, name: &str, topic: &str) -> Resp {
    call(
        env,
        token,
        tenant,
        "POST",
        "/api/v1/queues/subscriptions",
        Some(json!({"name": name, "topic": topic, "start": "earliest"})),
    )
    .await
}

async fn publish(env: &Env, token: &str, tenant: Option<&str>, topic: &str, data: Json) -> Resp {
    call(
        env,
        token,
        tenant,
        "POST",
        &format!("/api/v1/queues/topics/{topic}/publish"),
        Some(json!({"messages": [{"data": data}]})),
    )
    .await
}

async fn pull(env: &Env, token: &str, tenant: Option<&str>, sub: &str, wait_ms: i64) -> Resp {
    call(
        env,
        token,
        tenant,
        "POST",
        &format!("/api/v1/queues/subscriptions/{sub}/pull"),
        Some(json!({"max_messages": 100, "wait_ms": wait_ms})),
    )
    .await
}

fn messages(r: &Resp) -> Vec<Json> {
    r.json()["messages"].as_array().cloned().unwrap_or_default()
}

/// Everything `sub` delivers within a few seconds (acknowledged as it comes).
async fn drain(env: &Env, token: &str, tenant: Option<&str>, sub: &str, expect: usize) -> Vec<Json> {
    let mut out = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        let r = pull(env, token, tenant, sub, 300).await;
        assert_eq!(r.status, 200, "pull {sub}: {}", r.text());
        let got = messages(&r);
        if !got.is_empty() {
            let ids: Vec<Json> = got.iter().map(|m| m["ack_id"].clone()).collect();
            let a = call(
                env,
                token,
                tenant,
                "POST",
                &format!("/api/v1/queues/subscriptions/{sub}/ack"),
                Some(json!({"ack_ids": ids})),
            )
            .await;
            assert_eq!(a.status, 200, "ack: {}", a.text());
            out.extend(got);
        } else if out.len() >= expect {
            break;
        }
    }
    out
}

fn id_of(pk: &str) -> String {
    pk.rsplit('/').next().unwrap().to_owned()
}

async fn collection(env: &Env, admin: &str, def: Json, rules: Json) {
    let name = def["name"].as_str().unwrap().to_owned();
    let r = call(env, admin, None, "POST", "/api/v1/collections", Some(def)).await;
    assert_eq!(r.status, 201, "collection: {}", r.text());
    let r = call(
        env,
        admin,
        None,
        "PUT",
        &format!("/api/v1/policies/{name}"),
        Some(rules),
    )
    .await;
    assert_eq!(r.status, 200, "rules: {}", r.text());
}

async fn record(env: &Env, token: &str, tenant: Option<&str>, coll: &str, data: Json) -> String {
    let r = call(
        env,
        token,
        tenant,
        "POST",
        &format!("/api/v1/collections/{coll}/records"),
        Some(data),
    )
    .await;
    assert_eq!(r.status, 201, "record: {}", r.text());
    r.json()["id"].as_str().unwrap().to_owned()
}

/// Waits until topic `name` (instance-wide) retains `n` messages.
async fn wait_relayed(env: &Env, admin: &str, name: &str, n: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let r = call(
            env,
            admin,
            None,
            "GET",
            &format!("/api/v1/queues/topics/{name}/messages"),
            None,
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
        if r.json()["items"].as_array().unwrap().len() >= n {
            return;
        }
        assert!(tokio::time::Instant::now() < deadline, "not relayed: {}", r.text());
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// ---------------------------------------------------------------------------
// G2: record-change messages are the owner's view, decided at delivery
// ---------------------------------------------------------------------------

#[tokio::test]
async fn g2_record_messages_carry_the_subscription_owners_view() {
    let env = Env::new().await;
    let admin = env.admin("fb141-admin@example.com").await;
    let (alice, alice_pk) = env.user("alice@example.com").await;
    let (bob, bob_pk) = env.user("bob@example.com").await;
    let own = "owner = @request.auth.id";
    collection(
        &env,
        &admin,
        json!({"name": "notes", "fields": [
            {"name": "owner", "type": "text"},
            {"name": "body", "type": "text"},
            {"name": "secret", "type": "text", "hidden": true},
        ]}),
        json!({"list_rule": own, "view_rule": own}),
    )
    .await;
    let r = topic(
        &env,
        &admin,
        None,
        json!({"name": "note-events", "source": {"collection": "notes", "ops": []}}),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text());
    // Alice may run queue subscriptions (the pre-FB-141 plain keys).
    for p in ["engine:queue_admin", "engine:queue_read", "engine:queue_consume"] {
        grant(&env, &admin, &alice_pk, p).await;
    }
    let r = subscribe(&env, &alice, None, "alice-notes", "note-events").await;
    assert_eq!(r.status, 201, "{}", r.text());
    assert_eq!(r.json()["created_by"], json!(alice_pk));

    record(
        &env,
        &admin,
        None,
        "notes",
        json!({"owner": id_of(&alice_pk), "body": "mine", "secret": "s-alice"}),
    )
    .await;
    record(
        &env,
        &admin,
        None,
        "notes",
        json!({"owner": id_of(&bob_pk), "body": "bob's", "secret": "s-bob"}),
    )
    .await;
    wait_relayed(&env, &admin, "note-events", 2).await;

    // Alice's subscription delivers only what alice may see, masked.
    let got = drain(&env, &alice, None, "alice-notes", 1).await;
    assert_eq!(got.len(), 1, "{got:?}");
    let rec = &got[0]["message"]["data"]["record"];
    assert_eq!(rec["body"], "mine", "{got:?}");
    assert!(rec.get("secret").is_none(), "a hidden field leaked: {got:?}");
    let text = serde_json::to_string(&got).unwrap();
    assert!(!text.contains("s-bob") && !text.contains("bob's"), "{text}");

    // Browsing the topic is the reader's view too.
    let r = call(
        &env,
        &alice,
        None,
        "GET",
        "/api/v1/queues/topics/note-events/messages",
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let items = r.json()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1, "{}", r.text());
    assert!(
        !r.text().contains("s-alice") && !r.text().contains("s-bob"),
        "{}",
        r.text()
    );
    // The administrator's browse still shows everything.
    let r = call(
        &env,
        &admin,
        None,
        "GET",
        "/api/v1/queues/topics/note-events/messages",
        None,
    )
    .await;
    assert!(r.text().contains("s-bob"), "{}", r.text());

    // The rule is the one in force at delivery, not at relay.
    record(
        &env,
        &admin,
        None,
        "notes",
        json!({"owner": id_of(&alice_pk), "body": "later", "secret": "s3"}),
    )
    .await;
    wait_relayed(&env, &admin, "note-events", 3).await;
    let r = call(
        &env,
        &admin,
        None,
        "PUT",
        "/api/v1/policies/notes",
        Some(json!({"list_rule": "owner = 'nobody'", "view_rule": "owner = 'nobody'"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = pull(&env, &alice, None, "alice-notes", 500).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(messages(&r).is_empty(), "withheld under the current rule: {}", r.text());

    // Bob may consume a queue but not alice's subscription: she owns it.
    grant(&env, &admin, &bob_pk, "engine:queue_consume:bob-*").await;
    let r = pull(&env, &bob, None, "alice-notes", 0).await;
    assert_eq!(r.status, 403, "{}", r.text());
    env.shutdown().await;
}

#[tokio::test]
async fn g2_tenant_records_reach_only_their_tenants_subscriptions() {
    let env = Env::new().await;
    let admin = env.admin("fb141-admin2@example.com").await;
    let (alice, _) = env.user("alice2@example.com").await;
    let (bob, _) = env.user("bob2@example.com").await;
    let ta = tenant_of(&env, &alice, "alpha").await;
    let tb = tenant_of(&env, &bob, "bravo").await;
    let open = "@request.auth.id != ''";
    let r = call(
        &env,
        &admin,
        None,
        "POST",
        "/api/v1/collections",
        Some(json!({"name": "orders", "options": {"tenant_scoped": true}, "fields": [
            {"name": "item", "type": "text"},
            {"name": "cost", "type": "number", "hidden": true},
        ]})),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let r = call(
        &env,
        &admin,
        None,
        "PUT",
        "/api/v1/policies/orders",
        Some(json!({"list_rule": open, "view_rule": open, "create_rule": open})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = topic(
        &env,
        &admin,
        None,
        json!({"name": "order-events", "source": {"collection": "orders", "ops": ["create"]}}),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text());

    // The same subscription name in two tenants, each owned by its tenant's owner.
    let r = subscribe(&env, &alice, Some(&ta), "mine", "order-events").await;
    assert_eq!(r.status, 201, "{}", r.text());
    assert_eq!(r.json()["tenant"], json!(ta));
    let r = subscribe(&env, &bob, Some(&tb), "mine", "order-events").await;
    assert_eq!(r.status, 201, "bob's own subscription: {}", r.text());

    record(
        &env,
        &alice,
        Some(&ta),
        "orders",
        json!({"item": "alpha-item", "cost": 7}),
    )
    .await;
    record(
        &env,
        &bob,
        Some(&tb),
        "orders",
        json!({"item": "bravo-item", "cost": 9}),
    )
    .await;
    wait_relayed(&env, &admin, "order-events", 2).await;

    let a = drain(&env, &alice, Some(&ta), "mine", 1).await;
    let b = drain(&env, &bob, Some(&tb), "mine", 1).await;
    assert_eq!(a.len(), 1, "{a:?}");
    assert_eq!(b.len(), 1, "{b:?}");
    assert_eq!(a[0]["message"]["data"]["record"]["item"], "alpha-item");
    assert_eq!(b[0]["message"]["data"]["record"]["item"], "bravo-item");
    for m in [&a[0], &b[0]] {
        assert!(m["message"]["data"]["record"].get("cost").is_none(), "{m}");
    }
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// G16: tenant-scoped topics and subscriptions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn g16_topic_and_subscription_names_are_per_tenant_and_tenants_are_isolated() {
    let env = Env::new().await;
    let admin = env.admin("fb141-admin3@example.com").await;
    let (alice, _) = env.user("alice3@example.com").await;
    let (bob, _) = env.user("bob3@example.com").await;
    let ta = tenant_of(&env, &alice, "alpha").await;
    let tb = tenant_of(&env, &bob, "bravo").await;

    let r = topic(&env, &alice, Some(&ta), json!({"name": "jobs"})).await;
    assert_eq!(r.status, 201, "{}", r.text());
    assert_eq!(r.json()["tenant"], json!(ta));
    assert_eq!(r.json()["name"], "jobs");
    let r = topic(&env, &bob, Some(&tb), json!({"name": "jobs"})).await;
    assert_eq!(r.status, 201, "bob's own jobs topic: {}", r.text());
    // An instance-wide name is not taken over by a tenant.
    assert_eq!(topic(&env, &admin, None, json!({"name": "shared"})).await.status, 201);
    assert_eq!(
        topic(&env, &alice, Some(&ta), json!({"name": "shared"})).await.status,
        409
    );

    assert_eq!(subscribe(&env, &alice, Some(&ta), "w", "jobs").await.status, 201);
    assert_eq!(subscribe(&env, &bob, Some(&tb), "w", "jobs").await.status, 201);
    let r = publish(&env, &alice, Some(&ta), "jobs", json!({"secret": "alpha"})).await;
    assert_eq!(r.status, 200, "{}", r.text());

    // Bob's "w" and "jobs" are other resources.
    let r = pull(&env, &bob, Some(&tb), "w", 300).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(messages(&r).is_empty(), "{}", r.text());
    let r = call(
        &env,
        &bob,
        Some(&tb),
        "GET",
        "/api/v1/queues/topics/jobs/messages",
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.json()["items"].as_array().unwrap().is_empty(), "{}", r.text());
    let got = drain(&env, &alice, Some(&ta), "w", 1).await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0]["message"]["topic"], "jobs", "{got:?}");

    // The stored name of alice's topic is not a name anyone can use.
    let stored = format!("{ta}%2Fjobs");
    for (who, tenant) in [(&bob, Some(tb.as_str())), (&admin, None)] {
        let r = call(
            &env,
            who,
            tenant,
            "GET",
            &format!("/api/v1/queues/topics/{stored}"),
            None,
        )
        .await;
        assert!(r.status == 403 || r.status == 404, "{tenant:?}: {}", r.text());
        let r = publish(&env, who, tenant, &stored, json!(1)).await;
        assert!(r.status == 403 || r.status == 404, "{tenant:?}: {}", r.text());
    }
    // Without a tenant there is no "jobs" topic at all.
    let r = call(&env, &admin, None, "GET", "/api/v1/queues/topics/jobs", None).await;
    assert_eq!(r.status, 404, "{}", r.text());

    // Listings: the tenant's own plus the instance-wide ones.
    let r = call(&env, &alice, Some(&ta), "GET", "/api/v1/queues/topics", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let items = r.json()["items"].as_array().unwrap().clone();
    assert!(
        items.iter().any(|t| t["name"] == "jobs" && t["tenant"] == json!(ta)),
        "{}",
        r.text()
    );
    assert!(items.iter().any(|t| t["name"] == "shared" && t["tenant"].is_null()));
    assert!(!r.text().contains(&tb), "{}", r.text());
    let r = call(&env, &alice, Some(&ta), "GET", "/api/v1/queues/subscriptions", None).await;
    let subs = r.json()["items"].as_array().unwrap().clone();
    assert_eq!(subs.len(), 1, "{}", r.text());
    assert_eq!(subs[0]["topic"], "jobs");
    // A tenant may subscribe to an instance-wide topic.
    assert_eq!(subscribe(&env, &alice, Some(&ta), "w2", "shared").await.status, 201);
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// G16: per-topic and per-subscription permissions (FB-134 resource keys)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn g16_resource_keys_grant_one_topic_or_subscription() {
    let env = Env::new().await;
    let admin = env.admin("fb141-admin4@example.com").await;
    let (carol, carol_pk) = env.user("carol@example.com").await;
    for t in ["orders", "payroll"] {
        assert_eq!(topic(&env, &admin, None, json!({"name": t})).await.status, 201);
        assert_eq!(
            subscribe(&env, &admin, None, &format!("{t}-worker"), t).await.status,
            201
        );
    }
    grant(&env, &admin, &carol_pk, "engine:queue_publish:orders").await;
    grant(&env, &admin, &carol_pk, "engine:queue_consume:orders-worker").await;

    let r = publish(&env, &carol, None, "orders", json!({"n": 1})).await;
    assert_eq!(r.status, 200, "engine:queue_publish:orders: {}", r.text());
    let r = publish(&env, &carol, None, "payroll", json!({"n": 1})).await;
    assert_eq!(r.status, 403, "{}", r.text());
    let got = drain(&env, &carol, None, "orders-worker", 1).await;
    assert_eq!(got.len(), 1, "{got:?}");
    let r = pull(&env, &carol, None, "payroll-worker", 0).await;
    assert_eq!(r.status, 403, "{}", r.text());
    // A resource key grants that resource only, never the listing of others.
    let r = call(&env, &carol, None, "GET", "/api/v1/queues/topics", None).await;
    assert_eq!(r.status, 403, "{}", r.text());
    env.shutdown().await;
}

#[tokio::test]
async fn g16_a_subscriptions_creator_owns_it() {
    let env = Env::new().await;
    let admin = env.admin("fb141-admin5@example.com").await;
    let (dave, dave_pk) = env.user("dave@example.com").await;
    let (erin, _) = env.user("erin@example.com").await;
    for t in ["events", "payroll"] {
        assert_eq!(topic(&env, &admin, None, json!({"name": t})).await.status, 201);
    }
    assert_eq!(subscribe(&env, &admin, None, "ops", "events").await.status, 201);
    grant(&env, &admin, &dave_pk, "engine:queue_admin:dave.*").await;
    grant(&env, &admin, &dave_pk, "engine:queue_read:events").await;

    let r = subscribe(&env, &dave, None, "dave.inbox", "events").await;
    assert_eq!(r.status, 201, "{}", r.text());
    // A subscription only to a topic the creator may read, under a name it may administer.
    assert_eq!(subscribe(&env, &dave, None, "dave.pay", "payroll").await.status, 403);
    assert_eq!(subscribe(&env, &dave, None, "ops2", "events").await.status, 403);

    assert_eq!(publish(&env, &admin, None, "events", json!({"e": 1})).await.status, 200);
    // The owner consumes without a consume grant; others may not.
    let got = drain(&env, &dave, None, "dave.inbox", 1).await;
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(pull(&env, &erin, None, "dave.inbox", 0).await.status, 403);
    assert_eq!(pull(&env, &dave, None, "ops", 0).await.status, 403);
    let r = call(&env, &dave, None, "GET", "/api/v1/queues/subscriptions", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let names: Vec<Json> = r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("dave.inbox")], "{}", r.text());
    let r = call(
        &env,
        &dave,
        None,
        "DELETE",
        "/api/v1/queues/subscriptions/dave.inbox",
        None,
    )
    .await;
    assert_eq!(r.status, 204, "{}", r.text());
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// Edge functions publish as the caller (ctx.queues.publish), with the same checks
// ---------------------------------------------------------------------------

const PUBLISHER: &str = r#"import { serve } from "forgebase";

serve(async (req: Request, ctx) => {
  const topic = new URL(req.url).searchParams.get("topic") ?? "";
  try {
    const r = await ctx.queues.publish(topic, { via: "function" });
    return Response.json({ ok: r.duplicate === false });
  } catch (e: any) {
    return Response.json({ err: e.code });
  }
});
"#;

#[tokio::test]
async fn g16_function_publishes_get_the_callers_checks_and_tenant() {
    let env = Env::new().await;
    let admin = env.admin("fb141-admin6@example.com").await;
    let (carol, carol_pk) = env.user("carol6@example.com").await;
    let (alice, _) = env.user("alice6@example.com").await;
    let ta = tenant_of(&env, &alice, "alpha").await;
    let r = call(
        &env,
        &admin,
        None,
        "POST",
        "/api/v1/functions",
        Some(
            json!({"name": "pub", "source": PUBLISHER, "capabilities": ["queues.publish"],
                    "access": "authenticated"}),
        ),
    )
    .await;
    assert!(r.status < 300, "deploy: {}", r.text());
    for t in ["orders", "payroll"] {
        assert_eq!(topic(&env, &admin, None, json!({"name": t})).await.status, 201);
    }
    assert_eq!(
        topic(&env, &alice, Some(&ta), json!({"name": "inbox"})).await.status,
        201
    );
    grant(&env, &admin, &carol_pk, "engine:queue_publish:orders").await;

    let invoke = |token: String, tenant: Option<String>, topic: &'static str| {
        let env = &env;
        async move {
            let r = call(
                env,
                &token,
                tenant.as_deref(),
                "GET",
                &format!("/api/v1/functions/pub/invoke?topic={topic}"),
                None,
            )
            .await;
            assert_eq!(r.status, 200, "invoke: {}", r.text());
            r.json()
        }
    };
    assert_eq!(invoke(carol.clone(), None, "orders").await, json!({"ok": true}));
    assert_eq!(
        invoke(carol.clone(), None, "payroll").await,
        json!({"err": "forbidden"})
    );
    // In alice's tenant the function publishes to her tenant's "inbox"; without
    // the tenant there is no such topic for her.
    assert_eq!(
        invoke(alice.clone(), Some(ta.clone()), "inbox").await,
        json!({"ok": true})
    );
    assert!(
        invoke(alice.clone(), None, "inbox").await.get("err").is_some(),
        "no inbox outside alice's tenant"
    );
    let r = call(
        &env,
        &alice,
        Some(&ta),
        "GET",
        "/api/v1/queues/topics/inbox/messages",
        None,
    )
    .await;
    let items = r.json()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1, "{}", r.text());
    assert_eq!(items[0]["data"], json!({"via": "function"}));
    let r = call(&env, &admin, None, "GET", "/api/v1/queues/topics/orders/messages", None).await;
    let items = r.json()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1, "only carol's publish is instance-wide: {}", r.text());
    assert_eq!(items[0]["publisher"], json!(carol_pk));
    env.shutdown().await;
}
