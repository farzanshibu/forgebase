//! FB-171: realtime on a sharded cluster (ADR-0029 §3.3, §3.5): one
//! dispatcher and one pruner per set feeding one bus, replay from the
//! subscription's own set, `reset` with `tenant_moved` for a cursor from
//! another set, and privileged fan-in across sets (live only,
//! `cursor_unsupported_sharded`). Real, separate PostgreSQL clusters;
//! skipped without an engine (SQLite and one set: the other suites).

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::Request;
use axum::middleware::Next;
use axum::response::Response;
use common::Ws;
use forgebase_data::db::shard_testing::Sharded;
use forgebase_data::db::shards::{self, ShardSetup};
use forgebase_data::db::topology::{PlacementState, set_number_of};
use forgebase_data::db::{Backend, Database, Statement, Value};
use forgebase_data::schema::Collection;
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_realtime::RealtimeService;
use serde_json::{Value as Json, json};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// Test identity: an administrator, in the tenant of `x-test-tenant`.
async fn as_admin(mut req: Request, next: Next) -> Response {
    let mut ctx = AccessContext::admin("root");
    if let Some(t) = req.headers().get("x-test-tenant").and_then(|v| v.to_str().ok()) {
        ctx = ctx.with_tenant(t);
    }
    req.extensions_mut().insert(ctx);
    next.run(req).await
}

async fn ws(addr: SocketAddr, tenant: Option<&str>) -> Ws {
    let mut req = format!("ws://{addr}/api/v1/realtime/ws").into_client_request().unwrap();
    if let Some(t) = tenant {
        req.headers_mut().insert("x-test-tenant", t.parse().unwrap());
    }
    let (stream, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    let mut ws = Ws {
        stream,
        welcome: Json::Null,
    };
    ws.welcome = ws.next_json().await;
    assert_eq!(ws.welcome["type"], "welcome");
    ws
}

async fn outbox_len(db: &Database) -> i64 {
    db.fetch_all(&Statement::new(
        "SELECT count(*) FROM _forgebase_realtime_outbox",
        vec![],
    ))
    .await
    .unwrap()
    .scalar()
    .and_then(Value::as_i64)
    .unwrap()
}

/// The next record event (skipping other frames).
async fn next_event(w: &mut Ws) -> Json {
    loop {
        let m = w.next_json().await;
        if m["type"].as_str().is_some_and(|t| t.starts_with("record.")) {
            return m;
        }
    }
}

#[tokio::test]
async fn events_flow_per_set_with_replay_resets_and_fan_in() {
    let Some(mut s) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let backend: Arc<dyn Backend> = Arc::new(s.control.clone());
    let data = DataService::new(backend, Arc::new(DefaultAuthorizer)).await.unwrap();
    let sys = AccessContext::system();
    for def in [
        json!({"name": "notes", "options": {"tenant_scoped": true}, "fields": [{"name": "body", "type": "text"}]}),
        json!({"name": "news", "fields": [{"name": "body", "type": "text"}]}),
    ] {
        let c: Collection = serde_json::from_value(def).unwrap();
        data.create_collection(&sys, c).await.unwrap();
    }
    let (set, shard) = s.provision("s1").await;
    shards::activate_shard(
        &s.router,
        &set,
        &shard,
        ShardSetup {
            backups_dir: None,
            replay: Some(&data),
            control_conninfo: None,
        },
    )
    .await
    .unwrap();
    assert!(data.attach_shards(s.router.clone()));
    s.router.place("t1", &set.id, PlacementState::Active).await.unwrap();
    let mut cfg = common::fast_config();
    cfg.prune_interval = Duration::from_millis(200);
    cfg.max_retained_events = 4;
    let rt = RealtimeService::start(data.clone(), None, cfg, None).await.unwrap();
    let app = Router::new()
        .nest_service("/api/v1/realtime", forgebase_realtime::router(rt.clone()))
        .layer(axum::middleware::from_fn(as_admin));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap();
    });
    let t1 = AccessContext::admin("root").with_tenant("t1");
    let t0 = AccessContext::admin("root").with_tenant("t0");

    // A tenant subscription lives on the tenant's set: s1's dispatcher
    // delivers its events, with cursors in s1's range.
    let mut a = ws(addr, Some("t1")).await;
    let (_, sub) = a.subscribe(json!({"id": "n", "collection": "notes"})).await;
    assert_eq!(sub["type"], "subscribed", "{sub}");
    data.create_record(&t1, "notes", json!({"body": "one"})).await.unwrap();
    let e1 = next_event(&mut a).await;
    assert_eq!(e1["data"]["body"], "one");
    let c1 = e1["cursor"].as_i64().unwrap();
    assert_eq!(set_number_of(c1), set.number, "cursor {c1} is in s1's range");

    // Shared collections stay on control: their events come from control.
    let (_, sub) = a.subscribe(json!({"id": "w", "collection": "news"})).await;
    assert_eq!(sub["type"], "subscribed");
    data.create_record(&t1, "news", json!({"body": "shared"}))
        .await
        .unwrap();
    let e = next_event(&mut a).await;
    assert_eq!(e["id"], "w");
    assert_eq!(set_number_of(e["cursor"].as_i64().unwrap()), 0);

    // Replay from the subscription's set.
    data.create_record(&t1, "notes", json!({"body": "two"})).await.unwrap();
    assert_eq!(next_event(&mut a).await["data"]["body"], "two");
    let mut b = ws(addr, Some("t1")).await;
    let (replayed, sub) = b
        .subscribe(json!({"id": "n", "collection": "notes", "cursor": c1}))
        .await;
    assert_eq!(sub["replayed"], 1, "{sub}");
    assert_eq!(replayed.last().unwrap()["data"]["body"], "two");
    assert!(sub.get("reset").is_none());

    // A cursor from another set (the tenant's old set): reset, tenant_moved.
    let control_cursor = e["cursor"].as_i64().unwrap();
    let (frames, sub) = b
        .subscribe(json!({"id": "m", "collection": "notes", "cursor": control_cursor}))
        .await;
    assert_eq!(sub["reset"], "tenant_moved", "{sub}");
    assert!(
        frames
            .iter()
            .any(|f| f["type"] == "reset" && f["reason"] == "tenant_moved")
    );
    assert_eq!(set_number_of(sub["cursor"].as_i64().unwrap()), set.number);

    // Privileged fan-in without a tenant: live events of every set, no
    // replay.
    let mut c = ws(addr, None).await;
    let (_, sub) = c
        .subscribe(json!({"id": "all", "collection": "notes", "cursor": c1}))
        .await;
    assert_eq!(sub["type"], "subscribed", "{sub}");
    assert_eq!(sub["reset"], "cursor_unsupported_sharded");
    assert!(sub["cursor"].is_null());
    data.create_record(&t1, "notes", json!({"body": "s1 again"}))
        .await
        .unwrap();
    data.create_record(&t0, "notes", json!({"body": "on control"}))
        .await
        .unwrap();
    let mut seen = vec![next_event(&mut c).await, next_event(&mut c).await];
    seen.sort_by_key(|e| e["cursor"].as_i64().unwrap());
    assert_eq!(seen[0]["data"]["body"], "on control");
    assert_eq!(seen[0]["tenant_id"], "t0");
    assert_eq!(seen[1]["data"]["body"], "s1 again");
    assert_eq!(set_number_of(seen[1]["cursor"].as_i64().unwrap()), set.number);
    // The tenant subscription saw only its own tenant's event.
    let e = next_event(&mut a).await;
    assert_eq!(e["data"]["body"], "s1 again");
    a.quiet(Duration::from_millis(300)).await;

    // Each set prunes its own outbox.
    for i in 0..6 {
        data.create_record(&t1, "notes", json!({ "body": format!("p{i}") }))
            .await
            .unwrap();
    }
    let mut pruned = false;
    for _ in 0..50 {
        if outbox_len(&shard).await <= 4 {
            pruned = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(pruned, "s1's outbox was not pruned: {}", outbox_len(&shard).await);
    assert!(rt.heads().get(&set.number).copied().unwrap_or(0) > c1);
    rt.shutdown();
    server.abort();
}
