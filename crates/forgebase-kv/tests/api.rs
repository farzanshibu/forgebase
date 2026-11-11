//! REST API tests: authorisation, tenant isolation, every data type over HTTP,
//! TTL with a controlled clock, errors, pub/sub over SSE.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use axum::http::{Method, StatusCode};
use common::Harness;
use forgebase_data::{AccessContext, Action, Authorizer, Decision, Principal};
use forgebase_kv::KvConfig;
use http_body_util::BodyExt;
use serde_json::json;

// ---- authorisation ---------------------------------------------------------------------

#[tokio::test]
async fn anonymous_is_denied_everywhere() {
    let h = Harness::new().await;
    h.admin(
        Method::PUT,
        "/api/v1/kv/app/strings/k",
        Some(json!({"value": "secret"})),
    )
    .await
    .expect(StatusCode::OK);
    let calls: Vec<(Method, &str, Option<serde_json::Value>)> = vec![
        (Method::GET, "/api/v1/kv/_info", None),
        (Method::GET, "/api/v1/kv/_namespaces", None),
        (Method::DELETE, "/api/v1/kv/app", None),
        (Method::GET, "/api/v1/kv/app/keys", None),
        (Method::GET, "/api/v1/kv/app/keys/k", None),
        (Method::DELETE, "/api/v1/kv/app/keys/k", None),
        (Method::PUT, "/api/v1/kv/app/keys/k/ttl", Some(json!({"ttl_ms": 5}))),
        (Method::DELETE, "/api/v1/kv/app/keys/k/ttl", None),
        (Method::PUT, "/api/v1/kv/app/strings/k", Some(json!({"value": "x"}))),
        (Method::POST, "/api/v1/kv/app/strings/k/incr", Some(json!({}))),
        (Method::POST, "/api/v1/kv/app/mget", Some(json!({"keys": ["k"]}))),
        (
            Method::POST,
            "/api/v1/kv/app/mset",
            Some(json!({"entries": [{"key": "k", "value": "v"}]})),
        ),
        (
            Method::POST,
            "/api/v1/kv/app/hashes/h/set",
            Some(json!({"fields": {"f": "v"}})),
        ),
        (
            Method::POST,
            "/api/v1/kv/app/lists/l/push",
            Some(json!({"side": "left", "values": ["a"]})),
        ),
        (
            Method::POST,
            "/api/v1/kv/app/sets/s/add",
            Some(json!({"members": ["a"]})),
        ),
        (
            Method::POST,
            "/api/v1/kv/app/zsets/z/add",
            Some(json!({"members": [{"member": "a", "score": 1}]})),
        ),
        (
            Method::POST,
            "/api/v1/kv/app/streams/x/add",
            Some(json!({"fields": {"a": "1"}})),
        ),
        (Method::GET, "/api/v1/kv/app/streams/x/range", None),
        (
            Method::POST,
            "/api/v1/kv/app/exec",
            Some(json!({"command": ["GET", "k"]})),
        ),
        (
            Method::POST,
            "/api/v1/kv/app/batch",
            Some(json!({"commands": [["GET", "k"]]})),
        ),
        (Method::POST, "/api/v1/kv/app/publish/c", Some(json!({"message": "hi"}))),
        (Method::GET, "/api/v1/kv/app/subscribe/c", None),
    ];
    for (m, uri, body) in calls {
        let r = h.send(m.clone(), uri, &[], body).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{m} {uri}");
        assert_eq!(r.code(), "unauthenticated", "{m} {uri}");
        assert!(
            !String::from_utf8_lossy(&r.bytes).contains("secret"),
            "no data in the denial"
        );
    }
    // nothing was changed or created by the denied calls
    assert_eq!(h.exec("app", &["DBSIZE"]).await, json!(1));
}

#[tokio::test]
async fn authenticated_users_are_denied_by_the_default_authorizer() {
    let h = Harness::new().await;
    h.put("/api/v1/kv/app/strings/k", json!({"value": "secret"}))
        .await
        .expect(StatusCode::OK);
    for (m, uri, body) in [
        (Method::GET, "/api/v1/kv/app/keys/k", None),
        (Method::PUT, "/api/v1/kv/app/strings/k", Some(json!({"value": "x"}))),
        (
            Method::POST,
            "/api/v1/kv/app/exec",
            Some(json!({"command": ["GET", "k"]})),
        ),
        (Method::DELETE, "/api/v1/kv/app", None),
    ] {
        let r = h.send(m.clone(), uri, &[("x-test-user", "user:7")], body).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{m} {uri}");
        assert_eq!(r.code(), "forbidden");
    }
}

/// Lets users read, nothing else: proves the right action is asked per command.
#[derive(Debug)]
struct UsersReadOnly;

impl Authorizer for UsersReadOnly {
    fn authorize(
        &self,
        ctx: &AccessContext,
        action: Action,
        _collection: Option<&forgebase_schema::Collection>,
    ) -> Decision {
        match (&ctx.principal, action) {
            (Principal::System | Principal::Admin { .. }, _) => Decision::Allow,
            (Principal::User { .. }, Action::KvRead) => Decision::Allow,
            _ => Decision::Deny,
        }
    }
}

/// A user without a tenant reaches shared namespaces only (FB-138).
#[tokio::test]
async fn each_command_asks_for_the_action_it_needs() {
    let h = Harness::with(Arc::new(UsersReadOnly), KvConfig::default()).await;
    h.put("/api/v1/kv/shared.app/strings/k", json!({"value": "v"}))
        .await
        .expect(StatusCode::OK);
    let user = [("x-test-user", "user:7")];
    // read is allowed
    let r = h
        .send(Method::GET, "/api/v1/kv/shared.app/keys/k", &user, None)
        .await
        .expect(StatusCode::OK);
    assert_eq!(r.json()["value"], "v");
    h.send(
        Method::POST,
        "/api/v1/kv/shared.app/exec",
        &user,
        Some(json!({"command": ["GET", "k"]})),
    )
    .await
    .expect(StatusCode::OK);
    h.send(Method::GET, "/api/v1/kv/shared.app/keys", &user, None)
        .await
        .expect(StatusCode::OK);
    h.send(Method::GET, "/api/v1/kv/_namespaces", &user, None)
        .await
        .expect(StatusCode::OK);
    // write is not, however it is spelled
    for body in [
        json!({"command": ["SET", "k", "x"]}),
        json!({"command": ["set", "k", "x"]}),
        json!({"command": ["DEL", "k"]}),
        json!({"command": ["PUBLISH", "c", "x"]}),
        json!({"command": ["FLUSHDB"]}),
    ] {
        let r = h
            .send(Method::POST, "/api/v1/kv/shared.app/exec", &user, Some(body.clone()))
            .await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{body}");
    }
    // a batch mixing a read with a write is refused as a whole, nothing runs
    let r = h
        .send(
            Method::POST,
            "/api/v1/kv/shared.app/batch",
            &user,
            Some(json!({"commands": [["GET", "k"], ["SET", "k", "evil"]]})),
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(h.exec("shared.app", &["GET", "k"]).await, json!("v"));
    // admin-only: stats and flushing
    h.send(Method::GET, "/api/v1/kv/_info", &user, None)
        .await
        .expect(StatusCode::FORBIDDEN);
    h.send(Method::DELETE, "/api/v1/kv/shared.app", &user, None)
        .await
        .expect(StatusCode::FORBIDDEN);
    h.send(
        Method::PUT,
        "/api/v1/kv/shared.app/strings/k",
        &user,
        Some(json!({"value": "x"})),
    )
    .await
    .expect(StatusCode::FORBIDDEN);
    // an SSE stream never ends: check the status only, do not read the body
    let sse = h
        .raw(Method::GET, "/api/v1/kv/shared.app/subscribe/c", &user, None)
        .await;
    assert_eq!(sse.status(), StatusCode::OK);
}

/// An authorizer that answers `Filter` (row-level) cannot be honoured by KV: denied.
#[derive(Debug)]
struct Filtering;

impl Authorizer for Filtering {
    fn authorize(&self, _: &AccessContext, _: Action, _: Option<&forgebase_schema::Collection>) -> Decision {
        Decision::Filter(forgebase_data::Predicate {
            filter: forgebase_query::Expr::Const(true),
            params: Default::default(),
        })
    }
}

#[tokio::test]
async fn a_row_filter_decision_fails_closed() {
    let h = Harness::with(Arc::new(Filtering), KvConfig::default()).await;
    let r = h.get("/api/v1/kv/app/keys/k").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
}

// ---- tenant isolation -------------------------------------------------------------------

async fn as_tenant(
    h: &Harness,
    tenant: &str,
    method: Method,
    uri: &str,
    body: Option<serde_json::Value>,
) -> common::Resp {
    h.send(
        method,
        uri,
        &[("x-test-user", "admin"), ("x-test-tenant", tenant)],
        body,
    )
    .await
}

#[tokio::test]
async fn tenants_cannot_see_or_touch_each_others_keys() {
    let h = Harness::new().await;
    let put = |t: &'static str, v: &'static str| {
        as_tenant(
            &h,
            t,
            Method::PUT,
            "/api/v1/kv/shared/strings/k",
            Some(json!({"value": v})),
        )
    };
    put("acme", "acme-secret").await.expect(StatusCode::OK);
    put("globex", "globex-secret").await.expect(StatusCode::OK);
    // the default tenant (no tenant in the context) is a third, separate space
    h.put("/api/v1/kv/shared/strings/k", json!({"value": "default-secret"}))
        .await
        .expect(StatusCode::OK);

    for (tenant, expect) in [("acme", "acme-secret"), ("globex", "globex-secret")] {
        let r = as_tenant(&h, tenant, Method::GET, "/api/v1/kv/shared/keys/k", None)
            .await
            .expect(StatusCode::OK);
        assert_eq!(r.json()["value"], expect);
    }
    assert_eq!(
        h.get("/api/v1/kv/shared/keys/k").await.json()["value"],
        "default-secret"
    );

    // listings only show the caller's own keys and namespaces
    as_tenant(
        &h,
        "acme",
        Method::PUT,
        "/api/v1/kv/acme-only/strings/a",
        Some(json!({"value": "1"})),
    )
    .await
    .expect(StatusCode::OK);
    let ns = as_tenant(&h, "globex", Method::GET, "/api/v1/kv/_namespaces", None)
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(ns["namespaces"], json!([{"name": "shared", "keys": 1}]));
    let ns = as_tenant(&h, "acme", Method::GET, "/api/v1/kv/_namespaces", None)
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(ns["namespaces"].as_array().unwrap().len(), 2);
    let keys = as_tenant(&h, "globex", Method::GET, "/api/v1/kv/acme-only/keys", None)
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(
        keys["keys"],
        json!([]),
        "a namespace name from another tenant shows nothing"
    );
    as_tenant(&h, "globex", Method::GET, "/api/v1/kv/acme-only/keys/a", None)
        .await
        .expect(StatusCode::NOT_FOUND);

    // deleting, flushing and counters stay inside the tenant
    as_tenant(&h, "acme", Method::DELETE, "/api/v1/kv/shared/keys/k", None)
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        as_tenant(&h, "globex", Method::GET, "/api/v1/kv/shared/keys/k", None)
            .await
            .json()["value"],
        "globex-secret"
    );
    as_tenant(&h, "globex", Method::DELETE, "/api/v1/kv/shared", None)
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        h.get("/api/v1/kv/shared/keys/k").await.json()["value"],
        "default-secret"
    );
    let post = |t: &'static str| {
        as_tenant(
            &h,
            t,
            Method::POST,
            "/api/v1/kv/ctr/strings/n/incr",
            Some(json!({"by": 5})),
        )
    };
    assert_eq!(post("acme").await.json()["value"], 5);
    assert_eq!(post("acme").await.json()["value"], 10);
    assert_eq!(post("globex").await.json()["value"], 5);

    // tenant ids that are prefixes of each other do not mix
    as_tenant(
        &h,
        "ac",
        Method::PUT,
        "/api/v1/kv/shared/strings/k",
        Some(json!({"value": "ac"})),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(
        as_tenant(&h, "acme", Method::GET, "/api/v1/kv/shared/keys/k", None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    // exec, batch and the generic key scan are scoped the same way
    let r = as_tenant(
        &h,
        "acme",
        Method::POST,
        "/api/v1/kv/ctr/exec",
        Some(json!({"command": ["KEYS", "*"]})),
    )
    .await;
    assert_eq!(r.json()["result"], json!(["n"]));
    let r = as_tenant(
        &h,
        "initech",
        Method::POST,
        "/api/v1/kv/ctr/exec",
        Some(json!({"command": ["KEYS", "*"]})),
    )
    .await;
    assert_eq!(r.json()["result"], json!([]));
}

#[tokio::test]
async fn pubsub_is_scoped_by_tenant_and_namespace() {
    let h = Harness::new().await;
    let sub = |tenant: &'static str, ns: &'static str| {
        let h = &h;
        async move {
            h.raw(
                Method::GET,
                &format!("/api/v1/kv/{ns}/subscribe/news"),
                &[("x-test-user", "admin"), ("x-test-tenant", tenant)],
                None,
            )
            .await
        }
    };
    let acme = sub("acme", "chat").await;
    assert_eq!(acme.status(), StatusCode::OK);
    assert_eq!(acme.headers()["content-type"], "text/event-stream");
    let globex = sub("globex", "chat").await;
    let acme_other_ns = sub("acme", "other").await;
    let mut acme = acme.into_body();
    let mut globex = globex.into_body();
    let mut acme_other_ns = acme_other_ns.into_body();

    let r = as_tenant(
        &h,
        "acme",
        Method::POST,
        "/api/v1/kv/chat/publish/news",
        Some(json!({"message": "hello acme"})),
    )
    .await
    .expect(StatusCode::OK);
    assert_eq!(r.json()["receivers"], 1);

    let event = next_event(&mut acme).await;
    assert!(event.contains("event: message"), "{event}");
    let data = event.lines().find_map(|l| l.strip_prefix("data: ")).unwrap();
    let data: serde_json::Value = serde_json::from_str(data).unwrap();
    assert_eq!(data, json!({"channel": "news", "message": "hello acme"}));
    for other in [&mut globex, &mut acme_other_ns] {
        let got = tokio::time::timeout(Duration::from_millis(150), other.frame()).await;
        assert!(got.is_err(), "nothing must arrive in another tenant or namespace");
    }
    // publishing without subscribers reaches nobody
    let r = as_tenant(
        &h,
        "initech",
        Method::POST,
        "/api/v1/kv/chat/publish/news",
        Some(json!({"message": "x"})),
    )
    .await;
    assert_eq!(r.json()["receivers"], 0);
    // binary messages travel as base64
    h.post("/api/v1/kv/chat/publish/bin", json!({"message": {"base64": "AP8="}}))
        .await
        .expect(StatusCode::OK);
}

async fn next_event(body: &mut axum::body::Body) -> String {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(2), body.frame())
            .await
            .expect("an event within 2s")
            .expect("stream open")
            .expect("frame");
        if let Ok(data) = frame.into_data() {
            let text = String::from_utf8(data.to_vec()).unwrap();
            if text.contains("event:") {
                return text;
            }
        }
    }
}

#[tokio::test]
async fn pubsub_delivers_in_order_to_every_subscriber() {
    let h = Harness::new().await;
    let a = h
        .raw(
            Method::GET,
            "/api/v1/kv/q/subscribe/c",
            &[("x-test-user", "admin")],
            None,
        )
        .await;
    let b = h
        .raw(
            Method::GET,
            "/api/v1/kv/q/subscribe/c",
            &[("x-test-user", "admin")],
            None,
        )
        .await;
    let (mut a, mut b) = (a.into_body(), b.into_body());
    for i in 0..3 {
        let r = h
            .post("/api/v1/kv/q/publish/c", json!({"message": format!("m{i}")}))
            .await;
        assert_eq!(r.json()["receivers"], 2);
    }
    for body in [&mut a, &mut b] {
        for i in 0..3 {
            let ev = next_event(body).await;
            assert!(ev.contains(&format!("m{i}")), "{ev}");
        }
    }
    // dropping the subscribers frees the channel
    drop(a);
    drop(b);
    let r = h.post("/api/v1/kv/q/publish/c", json!({"message": "late"})).await;
    assert_eq!(r.json()["receivers"], 0);
}

// ---- data types over REST ---------------------------------------------------------------

#[tokio::test]
async fn strings_counters_and_ttl_over_rest() {
    let h = Harness::new().await;
    let r = h
        .put(
            "/api/v1/kv/app/strings/greeting",
            json!({"value": "héllo", "ttl_ms": 5000}),
        )
        .await;
    assert_eq!(r.expect(StatusCode::OK).json(), json!({"stored": true}));
    let r = h
        .get("/api/v1/kv/app/keys/greeting")
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(r["type"], "string");
    assert_eq!(r["value"], "héllo");
    assert_eq!(r["ttl_ms"], 5000);
    assert_eq!(r["size"], 6, "bytes, not characters");
    // nx / xx
    let r = h
        .put("/api/v1/kv/app/strings/greeting", json!({"value": "x", "nx": true}))
        .await;
    assert_eq!(r.expect(StatusCode::OK).json(), json!({"stored": false}));
    let r = h
        .put("/api/v1/kv/app/strings/nope", json!({"value": "x", "xx": true}))
        .await;
    assert_eq!(r.expect(StatusCode::OK).json(), json!({"stored": false}));
    // the TTL runs on the controlled clock
    h.advance(4_999);
    assert_eq!(h.get("/api/v1/kv/app/keys/greeting").await.json()["ttl_ms"], 1);
    h.advance(1);
    h.get("/api/v1/kv/app/keys/greeting")
        .await
        .expect(StatusCode::NOT_FOUND);
    // keepttl
    h.put("/api/v1/kv/app/strings/a", json!({"value": "1", "ttl_ms": 10_000}))
        .await;
    h.put("/api/v1/kv/app/strings/a", json!({"value": "2", "keepttl": true}))
        .await;
    assert_eq!(h.get("/api/v1/kv/app/keys/a").await.json()["ttl_ms"], 10_000);
    h.put("/api/v1/kv/app/strings/a", json!({"value": "3"})).await;
    assert!(h.get("/api/v1/kv/app/keys/a").await.json()["ttl_ms"].is_null());
    // set and clear a TTL
    h.put("/api/v1/kv/app/keys/a/ttl", json!({"ttl_ms": 250}))
        .await
        .expect(StatusCode::OK);
    assert_eq!(h.get("/api/v1/kv/app/keys/a").await.json()["ttl_ms"], 250);
    assert_eq!(
        h.delete("/api/v1/kv/app/keys/a/ttl")
            .await
            .expect(StatusCode::OK)
            .json(),
        json!({"persisted": true})
    );
    assert_eq!(
        h.delete("/api/v1/kv/app/keys/a/ttl").await.json(),
        json!({"persisted": false})
    );
    h.put("/api/v1/kv/app/keys/missing/ttl", json!({"ttl_ms": 5}))
        .await
        .expect(StatusCode::NOT_FOUND);
    h.delete("/api/v1/kv/app/keys/missing/ttl")
        .await
        .expect(StatusCode::NOT_FOUND);
    h.put("/api/v1/kv/app/keys/a/ttl", json!({"ttl_ms": 0}))
        .await
        .expect(StatusCode::BAD_REQUEST);
    // counters
    let n = |by: serde_json::Value| h.post("/api/v1/kv/app/strings/hits/incr", by);
    assert_eq!(n(json!({})).await.json()["value"], 1);
    assert_eq!(n(json!({"by": 10})).await.json()["value"], 11);
    assert_eq!(n(json!({"by": -20})).await.json()["value"], -9);
    h.put("/api/v1/kv/app/strings/word", json!({"value": "abc"})).await;
    assert_eq!(
        h.post("/api/v1/kv/app/strings/word/incr", json!({})).await.status,
        StatusCode::BAD_REQUEST
    );
    // mset / mget
    h.post(
        "/api/v1/kv/app/mset",
        json!({"entries": [{"key": "x", "value": "1"}, {"key": "y", "value": "2"}]}),
    )
    .await
    .expect(StatusCode::OK);
    let r = h
        .post("/api/v1/kv/app/mget", json!({"keys": ["x", "nope", "y"]}))
        .await
        .expect(StatusCode::OK);
    assert_eq!(r.json()["values"], json!(["1", null, "2"]));
    // delete
    assert_eq!(h.delete("/api/v1/kv/app/keys/x").await.json(), json!({"deleted": true}));
    assert_eq!(
        h.delete("/api/v1/kv/app/keys/x").await.json(),
        json!({"deleted": false})
    );
}

#[tokio::test]
async fn binary_values_round_trip_as_base64() {
    let h = Harness::new().await;
    h.put("/api/v1/kv/app/strings/bin", json!({"value": {"base64": "AP+A"}}))
        .await
        .expect(StatusCode::OK);
    let r = h.get("/api/v1/kv/app/keys/bin").await.json();
    assert_eq!(r["value"], json!({"base64": "AP+A"}));
    assert_eq!(r["size"], 3);
    h.put("/api/v1/kv/app/strings/bad", json!({"value": {"base64": "***"}}))
        .await
        .expect(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn hashes_over_rest() {
    let h = Harness::new().await;
    let r = h
        .post(
            "/api/v1/kv/app/hashes/user:1/set",
            json!({"fields": {"name": "Ada", "age": "36"}}),
        )
        .await;
    assert_eq!(r.expect(StatusCode::OK).json(), json!({"added": 2}));
    assert_eq!(
        h.post("/api/v1/kv/app/hashes/user:1/incr", json!({"field": "age", "by": 2}))
            .await
            .json()["value"],
        38
    );
    let r = h.get("/api/v1/kv/app/keys/user:1").await.expect(StatusCode::OK).json();
    assert_eq!(r["type"], "hash");
    assert_eq!(r["value"], json!({"age": "38", "name": "Ada"}));
    assert_eq!(r["size"], 2);
    let r = h
        .post(
            "/api/v1/kv/app/hashes/user:1/delete",
            json!({"fields": ["age", "nope"]}),
        )
        .await;
    assert_eq!(r.json(), json!({"removed": 1}));
    h.post("/api/v1/kv/app/hashes/user:1/delete", json!({"fields": ["name"]}))
        .await;
    h.get("/api/v1/kv/app/keys/user:1").await.expect(StatusCode::NOT_FOUND);
    h.post("/api/v1/kv/app/hashes/h/set", json!({"fields": {}}))
        .await
        .expect(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn lists_over_rest() {
    let h = Harness::new().await;
    let push = |side: &'static str, vals: serde_json::Value| {
        h.post("/api/v1/kv/app/lists/jobs/push", json!({"side": side, "values": vals}))
    };
    assert_eq!(push("right", json!(["a", "b"])).await.json()["length"], 2);
    assert_eq!(push("left", json!(["z"])).await.json()["length"], 3);
    let r = h.get("/api/v1/kv/app/keys/jobs").await.json();
    assert_eq!(r["type"], "list");
    assert_eq!(r["value"], json!(["z", "a", "b"]));
    let r = h.get("/api/v1/kv/app/keys/jobs?limit=2").await.json();
    assert_eq!(r["value"], json!(["z", "a"]));
    assert_eq!(r["truncated"], true);
    assert_eq!(r["size"], 3);
    let pop = |side: &'static str, count: serde_json::Value| {
        h.post("/api/v1/kv/app/lists/jobs/pop", json!({"side": side, "count": count}))
    };
    assert_eq!(pop("left", json!(1)).await.json()["values"], json!(["z"]));
    assert_eq!(pop("right", json!(5)).await.json()["values"], json!(["b", "a"]));
    assert_eq!(pop("right", json!(1)).await.json()["values"], json!([]));
    h.post("/api/v1/kv/app/lists/jobs/push", json!({"side": "up", "values": ["x"]}))
        .await
        .expect(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sets_and_sorted_sets_over_rest() {
    let h = Harness::new().await;
    let r = h
        .post(
            "/api/v1/kv/app/sets/tags/add",
            json!({"members": ["rust", "kv", "rust"]}),
        )
        .await;
    assert_eq!(r.expect(StatusCode::OK).json(), json!({"added": 2}));
    let r = h.get("/api/v1/kv/app/keys/tags").await.json();
    assert_eq!(r["type"], "set");
    assert_eq!(r["value"], json!(["kv", "rust"]));
    assert_eq!(
        h.post("/api/v1/kv/app/sets/tags/remove", json!({"members": ["kv", "x"]}))
            .await
            .json(),
        json!({"removed": 1})
    );

    let r = h
        .post(
            "/api/v1/kv/app/zsets/board/add",
            json!({"members": [{"member": "bob", "score": 20.5}, {"member": "amy", "score": 30}, {"member": "cy", "score": 10}]}),
        )
        .await;
    assert_eq!(r.expect(StatusCode::OK).json(), json!({"added": 3}));
    let r = h.get("/api/v1/kv/app/keys/board").await.json();
    assert_eq!(r["type"], "zset");
    assert_eq!(
        r["value"],
        json!([{"member": "cy", "score": 10.0}, {"member": "bob", "score": 20.5}, {"member": "amy", "score": 30.0}])
    );
    assert_eq!(h.exec("app", &["ZREVRANGE", "board", "0", "0"]).await, json!(["amy"]));
    assert_eq!(
        h.post("/api/v1/kv/app/zsets/board/remove", json!({"members": ["cy"]}))
            .await
            .json(),
        json!({"removed": 1})
    );
    h.post(
        "/api/v1/kv/app/zsets/board/add",
        json!({"members": [{"member": "x", "score": "inf"}]}),
    )
    .await
    .expect(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn streams_over_rest() {
    let h = Harness::new().await;
    let add = |id: serde_json::Value, v: &'static str| {
        h.post(
            "/api/v1/kv/app/streams/events/add",
            json!({"id": id, "fields": {"kind": v}}),
        )
    };
    let first = add(json!(null), "login").await.expect(StatusCode::CREATED).json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(first, "1000000-0");
    let second = add(json!(null), "click").await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(second, "1000000-1");
    add(json!("5-5"), "old").await.expect(StatusCode::BAD_REQUEST);
    let r = h
        .get("/api/v1/kv/app/streams/events/range")
        .await
        .expect(StatusCode::OK)
        .json();
    assert_eq!(
        r["entries"],
        json!([
            {"id": "1000000-0", "fields": {"kind": "login"}},
            {"id": "1000000-1", "fields": {"kind": "click"}},
        ])
    );
    let r = h
        .get("/api/v1/kv/app/streams/events/range?rev=true&count=1")
        .await
        .json();
    assert_eq!(r["entries"][0]["id"], "1000000-1");
    let r = h.get("/api/v1/kv/app/streams/events/read?after=1000000-0").await.json();
    assert_eq!(r["entries"].as_array().unwrap().len(), 1);
    let r = h.get("/api/v1/kv/app/streams/events/read?after=1000000-1").await.json();
    assert_eq!(r["entries"], json!([]));
    let r = h.get("/api/v1/kv/app/keys/events").await.json();
    assert_eq!(r["type"], "stream");
    assert_eq!(r["size"], 2);
    h.post(
        "/api/v1/kv/app/streams/events/add",
        json!({"maxlen": 1, "fields": {"a": "b"}}),
    )
    .await
    .expect(StatusCode::CREATED);
    assert_eq!(h.get("/api/v1/kv/app/keys/events").await.json()["size"], 1);
}

#[tokio::test]
async fn scan_pages_through_keys() {
    let h = Harness::new().await;
    for i in 0..23 {
        h.put(&format!("/api/v1/kv/app/strings/item:{i:02}"), json!({"value": "v"}))
            .await;
    }
    h.post("/api/v1/kv/app/hashes/other/set", json!({"fields": {"a": "1"}}))
        .await;
    let mut seen = Vec::new();
    let mut cursor = "0".to_owned();
    let mut pages = 0;
    loop {
        let r = h
            .get(&format!("/api/v1/kv/app/keys?cursor={cursor}&count=10&match=item:*"))
            .await
            .expect(StatusCode::OK)
            .json();
        seen.extend(
            r["keys"]
                .as_array()
                .unwrap()
                .iter()
                .map(|k| k.as_str().unwrap().to_owned()),
        );
        cursor = r["cursor"].as_str().unwrap().to_owned();
        pages += 1;
        if cursor == "0" {
            break;
        }
        assert!(pages < 20, "the cursor must end");
    }
    assert_eq!(seen.len(), 23);
    assert!(pages >= 3);
    let r = h.get("/api/v1/kv/app/keys?type=hash").await.json();
    assert_eq!(r["keys"], json!(["other"]));
    h.get("/api/v1/kv/app/keys?cursor=garbage")
        .await
        .expect(StatusCode::BAD_REQUEST);
    h.get("/api/v1/kv/app/keys?type=nope")
        .await
        .expect(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn exec_and_batch_run_any_command() {
    let h = Harness::new().await;
    assert_eq!(h.exec("app", &["SET", "k", "1"]).await, json!("OK"));
    assert_eq!(h.exec("app", &["INCRBY", "k", "41"]).await, json!(42));
    assert_eq!(h.exec("app", &["get", "nope"]).await, json!(null));
    assert_eq!(h.exec("app", &["ZADD", "z", "1", "a", "2", "b"]).await, json!(2));
    assert_eq!(
        h.exec("app", &["ZRANGE", "z", "0", "-1", "WITHSCORES"]).await,
        json!(["a", "1", "b", "2"])
    );
    let r = h
        .post(
            "/api/v1/kv/app/batch",
            json!({"commands": [["INCR", "n"], ["INCR", "n"], ["LPUSH", "k", "x"], ["GET", "n"], ["BOGUS"]]}),
        )
        .await
        .expect(StatusCode::OK)
        .json();
    let results = r["results"].as_array().unwrap();
    assert_eq!(results[0], json!({"result": 1}));
    assert_eq!(results[1], json!({"result": 2}));
    assert_eq!(
        results[2]["error"]["code"], "kv_wrong_type",
        "one failing command does not stop the rest"
    );
    assert_eq!(results[3], json!({"result": "2"}));
    assert_eq!(results[4]["error"]["code"], "kv_unknown_command");
    h.post("/api/v1/kv/app/batch", json!({"commands": []}))
        .await
        .expect(StatusCode::BAD_REQUEST);
    h.post("/api/v1/kv/app/exec", json!({"command": []}))
        .await
        .expect(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn errors_use_the_envelope_and_the_right_status() {
    let h = Harness::new().await;
    h.put("/api/v1/kv/app/strings/s", json!({"value": "v"})).await;
    let r = h
        .post("/api/v1/kv/app/lists/s/push", json!({"side": "left", "values": ["x"]}))
        .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(r.code(), "kv_wrong_type");
    assert!(r.json()["error"]["request_id"].is_string());
    let r = h.post("/api/v1/kv/app/exec", json!({"command": ["NOPE"]})).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "kv_unknown_command")
    );
    let r = h.post("/api/v1/kv/app/exec", json!({"command": ["GET"]})).await;
    assert_eq!(
        (r.status, r.code().as_str()),
        (StatusCode::BAD_REQUEST, "kv_wrong_arity")
    );
    h.get("/api/v1/kv/app/keys/nothing").await.expect(StatusCode::NOT_FOUND);
    // malformed JSON
    let r = h
        .send(
            Method::POST,
            "/api/v1/kv/app/exec",
            &[("x-test-user", "admin"), ("content-type", "application/json")],
            None,
        )
        .await;
    assert!(r.status.is_client_error());
}

#[tokio::test]
async fn namespace_and_key_names_are_validated() {
    let h = Harness::new().await;
    for ns in ["_hidden", ".dot", "has%20space", "a%2Fb", "-dash"] {
        let r = h
            .put(&format!("/api/v1/kv/{ns}/strings/k"), json!({"value": "v"}))
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "{ns}");
        assert_eq!(r.code(), "kv_invalid_namespace", "{ns}");
    }
    let long = "a".repeat(65);
    h.put(&format!("/api/v1/kv/{long}/strings/k"), json!({"value": "v"}))
        .await
        .expect(StatusCode::BAD_REQUEST);
    h.put("/api/v1/kv/ok.ns-1_x:y/strings/k", json!({"value": "v"}))
        .await
        .expect(StatusCode::OK);
    // keys with spaces, slashes and unicode via percent-encoding
    for key in ["a%20b", "a%2Fb", "%E2%9C%93"] {
        h.put(&format!("/api/v1/kv/app/strings/{key}"), json!({"value": "v"}))
            .await
            .expect(StatusCode::OK);
        h.get(&format!("/api/v1/kv/app/keys/{key}"))
            .await
            .expect(StatusCode::OK);
    }
    let r = h.get("/api/v1/kv/app/keys").await.json();
    assert_eq!(r["keys"], json!(["a b", "a/b", "✓"]));
    let long = "k".repeat(513);
    let r = h
        .put(&format!("/api/v1/kv/app/strings/{long}"), json!({"value": "v"}))
        .await;
    assert_eq!(r.code(), "kv_invalid_key");
}

#[tokio::test]
async fn flush_namespace_and_stats() {
    let h = Harness::with(
        Arc::new(forgebase_data::DefaultAuthorizer),
        KvConfig {
            max_memory_bytes: 1 << 20,
            ..KvConfig::default()
        },
    )
    .await;
    for i in 0..5 {
        h.put(&format!("/api/v1/kv/a/strings/k{i}"), json!({"value": "v"}))
            .await;
    }
    h.put("/api/v1/kv/b/strings/k", json!({"value": "v"})).await;
    h.get("/api/v1/kv/a/keys/k0").await;
    h.get("/api/v1/kv/a/keys/missing").await;
    let s = h.get("/api/v1/kv/_info").await.expect(StatusCode::OK).json();
    assert_eq!(s["keys"], 6);
    assert_eq!(s["max_memory_bytes"], 1 << 20);
    assert_eq!(s["eviction_policy"], "allkeys-lru");
    assert_eq!(s["persistent"], true);
    assert!(s["used_memory_bytes"].as_u64().unwrap() > 0);
    assert!(s["hits"].as_u64().unwrap() >= 1);
    assert!(s["misses"].as_u64().unwrap() >= 1);
    assert_eq!(s["unsaved_keys"], 6);
    h.kv.flush().await.unwrap();
    assert_eq!(h.get("/api/v1/kv/_info").await.json()["unsaved_keys"], 0);
    assert_eq!(
        h.delete("/api/v1/kv/a").await.expect(StatusCode::OK).json(),
        json!({"deleted": 5})
    );
    let ns = h.get("/api/v1/kv/_namespaces").await.json();
    assert_eq!(ns["namespaces"], json!([{"name": "b", "keys": 1}]));
}

#[tokio::test]
async fn lru_eviction_through_the_api() {
    let h = Harness::with(
        Arc::new(forgebase_data::DefaultAuthorizer),
        KvConfig {
            max_memory_bytes: 2_000,
            ..KvConfig::default()
        },
    )
    .await;
    let value = "v".repeat(300);
    for i in 0..10 {
        h.put(&format!("/api/v1/kv/cache/strings/k{i}"), json!({"value": value}))
            .await
            .expect(StatusCode::OK);
    }
    let s = h.get("/api/v1/kv/_info").await.json();
    assert!(s["used_memory_bytes"].as_u64().unwrap() <= 2_000, "{s}");
    assert!(s["evicted_keys"].as_u64().unwrap() >= 5, "{s}");
    // the newest key survived, the oldest did not
    h.get("/api/v1/kv/cache/keys/k9").await.expect(StatusCode::OK);
    h.get("/api/v1/kv/cache/keys/k0").await.expect(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn large_bodies_are_refused() {
    let h = Harness::new().await;
    let huge = "x".repeat(5 << 20);
    let r = h.put("/api/v1/kv/app/strings/k", json!({"value": huge})).await;
    assert_eq!(r.status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn list_remove_list_set_and_stream_delete_over_rest() {
    let h = Harness::new().await;
    h.post(
        "/api/v1/kv/app/lists/l/push",
        json!({"side": "right", "values": ["a", "b", "a", "c", "a"]}),
    )
    .await;
    // LSET, negative index counts from the tail
    let r = h
        .post("/api/v1/kv/app/lists/l/set", json!({"index": -1, "value": "z"}))
        .await
        .expect(StatusCode::OK);
    assert_eq!(r.json()["ok"], true);
    h.post("/api/v1/kv/app/lists/l/set", json!({"index": 9, "value": "q"}))
        .await
        .expect(StatusCode::BAD_REQUEST);
    // LREM: first match from the head, then all the rest
    let r = h
        .post("/api/v1/kv/app/lists/l/remove", json!({"value": "a", "count": 1}))
        .await
        .expect(StatusCode::OK);
    assert_eq!(r.json()["removed"], 1);
    let r = h.post("/api/v1/kv/app/lists/l/remove", json!({"value": "a"})).await;
    assert_eq!(r.json()["removed"], 1);
    assert_eq!(
        h.get("/api/v1/kv/app/keys/l").await.json()["value"],
        json!(["b", "c", "z"])
    );

    // XDEL
    let id1 = h
        .post("/api/v1/kv/app/streams/s/add", json!({"fields": {"k": "1"}}))
        .await
        .json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    h.post("/api/v1/kv/app/streams/s/add", json!({"fields": {"k": "2"}}))
        .await;
    let r = h
        .post("/api/v1/kv/app/streams/s/delete", json!({"ids": [id1, "1-1"]}))
        .await
        .expect(StatusCode::OK);
    assert_eq!(r.json()["removed"], 1);
    let r = h.get("/api/v1/kv/app/streams/s/range").await.json();
    assert_eq!(r["entries"].as_array().unwrap().len(), 1);
    h.post("/api/v1/kv/app/streams/s/delete", json!({"ids": []}))
        .await
        .expect(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn channel_list_shows_live_subscribed_channels_per_tenant_and_namespace() {
    let h = Harness::new().await;
    let sub = |tenant: &'static str, ns: &'static str, ch: &'static str| {
        let h = &h;
        async move {
            h.raw(
                Method::GET,
                &format!("/api/v1/kv/{ns}/subscribe/{ch}"),
                &[("x-test-user", "admin"), ("x-test-tenant", tenant)],
                None,
            )
            .await
        }
    };
    let none = as_tenant(&h, "acme", Method::GET, "/api/v1/kv/chat/channels", None).await;
    assert_eq!(none.json()["channels"], json!([]));
    let a1 = sub("acme", "chat", "news").await;
    let a2 = sub("acme", "chat", "news").await;
    let a3 = sub("acme", "chat", "alerts").await;
    let g = sub("globex", "chat", "secret").await;
    let r = as_tenant(&h, "acme", Method::GET, "/api/v1/kv/chat/channels", None)
        .await
        .expect(StatusCode::OK);
    assert_eq!(
        r.json()["channels"],
        json!([{"name": "alerts", "subscribers": 1}, {"name": "news", "subscribers": 2}])
    );
    let r = as_tenant(&h, "acme", Method::GET, "/api/v1/kv/other/channels", None).await;
    assert_eq!(r.json()["channels"], json!([]));
    let r = as_tenant(&h, "globex", Method::GET, "/api/v1/kv/chat/channels", None).await;
    assert_eq!(r.json()["channels"], json!([{"name": "secret", "subscribers": 1}]));
    drop((a1, a2, a3, g));
}
