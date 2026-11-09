//! Realtime acceptance tests (FB-40): SPEC §27 #16 and #17, replay without
//! gaps or duplicates, authorisation at subscribe time and per event, revoked
//! sessions, backpressure, limits, heartbeats, presence and channels.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashSet;
use std::time::Duration;

use common::{Got, Rule, Stack, admin, enc, fast_config};
use forgebase_realtime::RealtimeConfig;
use forgebase_realtime::protocol::close;
use serde_json::{Value as Json, json};

async fn products(s: &Stack) {
    s.collection(json!({"name": "products", "fields": [
        {"name": "name", "type": "text", "required": true},
        {"name": "price", "type": "number"},
        {"name": "owner", "type": "text"},
        {"name": "secret", "type": "text", "hidden": true},
    ]}))
    .await;
}

fn id_of(v: &Json) -> String {
    v["id"].as_str().unwrap().to_owned()
}

/// SPEC #16 + #17 over WebSocket: subscribe, then every committed create,
/// update and delete arrives once, in order, with the SPEC event shape.
#[tokio::test]
async fn spec16_17_websocket_receives_committed_changes() {
    let s = Stack::start(fast_config()).await;
    products(&s).await;
    s.rules.set("products", Rule::Allow);
    let (_uid, token, _) = s.user("ann@example.com").await;
    let mut ws = s.ws(Some(&token)).await;
    assert_eq!(
        ws.welcome["actor"].as_str().unwrap().split('/').next(),
        Some("user:users")
    );

    let (replayed, ok) = ws.subscribe(json!({"id": "p", "collection": "products"})).await;
    assert_eq!(ok["type"], "subscribed", "{ok}");
    assert!(replayed.is_empty());

    let rec = s
        .create("products", json!({"name": "lamp", "price": 12, "secret": "s3"}))
        .await;
    let e = ws.next_json().await;
    assert_eq!(e["type"], "record.created", "{e}");
    assert_eq!(e["id"], "p");
    assert_eq!(e["collection"], "products");
    assert_eq!(e["record_id"], rec["id"]);
    assert!(e["tenant_id"].is_null());
    assert!(e["timestamp"].as_str().unwrap().ends_with('Z'));
    assert_eq!(e["data"]["name"], "lamp");
    assert_eq!(e["data"]["price"], 12.0);
    assert!(e["data"].get("secret").is_none(), "hidden field leaked: {e}");
    let c1 = e["cursor"].as_i64().unwrap();

    // The change is committed and persisted when the event arrives.
    let stored = s
        .data
        .get_record(&admin(), "products", &id_of(&rec), &Default::default())
        .await
        .unwrap();
    assert_eq!(stored["name"], "lamp");

    s.data
        .update_record(
            &admin(),
            "products",
            &id_of(&rec),
            json!({"price": 15}),
            Default::default(),
        )
        .await
        .unwrap();
    let e = ws.next_json().await;
    assert_eq!(e["type"], "record.updated");
    assert_eq!(e["data"]["price"], 15.0);
    assert_eq!(e["data"]["_version"], 2);
    assert!(e["cursor"].as_i64().unwrap() > c1);

    s.data
        .delete_record(&admin(), "products", &id_of(&rec), None)
        .await
        .unwrap();
    let e = ws.next_json().await;
    assert_eq!(e["type"], "record.deleted");
    assert_eq!(e["record_id"], rec["id"]);
    assert_eq!(e["data"]["name"], "lamp", "deletes carry the last visible state");

    // A failed (rolled back) write produces no event: the outbox row is in
    // the same transaction. The batch's second record is invalid.
    let before = s.outbox_len().await;
    let err = s
        .data
        .create_records(&admin(), "products", vec![json!({"name": "ok"}), json!({"price": 1})])
        .await;
    assert!(err.is_err());
    assert_eq!(s.outbox_len().await, before, "rolled-back changes leave no outbox rows");
    s.create("products", json!({"name": "after"})).await;
    let e = ws.next_json().await;
    assert_eq!(e["data"]["name"], "after", "the rolled-back record was never announced");

    // A record subscription only sees its record.
    let other = s.create("products", json!({"name": "other"})).await;
    let _ = ws.next_json().await; // collection subscription sees it
    let (_, ok) = ws
        .subscribe(json!({"id": "one", "collection": "products", "record": id_of(&other)}))
        .await;
    assert_eq!(ok["type"], "subscribed", "{ok}");
    s.data
        .update_record(
            &admin(),
            "products",
            &id_of(&other),
            json!({"price": 1}),
            Default::default(),
        )
        .await
        .unwrap();
    let mut subs = HashSet::new();
    for _ in 0..2 {
        let e = ws.next_json().await;
        assert_eq!(e["record_id"], other["id"]);
        subs.insert(e["id"].as_str().unwrap().to_owned());
    }
    assert_eq!(subs, HashSet::from(["p".to_owned(), "one".to_owned()]));
    s.create("products", json!({"name": "third"})).await;
    let e = ws.next_json().await;
    assert_eq!(
        (e["id"].as_str(), e["data"]["name"].as_str()),
        (Some("p"), Some("third"))
    );
    ws.quiet(Duration::from_millis(300)).await;
}

/// SPEC #16 + #17 over SSE, with `id:` = cursor.
#[tokio::test]
async fn spec16_17_sse_receives_committed_changes() {
    let s = Stack::start(fast_config()).await;
    products(&s).await;
    s.rules.set("products", Rule::Allow);
    let (_uid, token, _) = s.user("ann@example.com").await;
    let subs = enc(&json!([{"id": "p", "collection": "products", "filter": "price > 10"}]).to_string());
    let mut sse = s
        .sse(Some(&token), &format!("?subscriptions={subs}"), None)
        .await
        .unwrap();
    assert_eq!(sse.expect("welcome").await.data["type"], "welcome");
    assert_eq!(sse.expect("subscribed").await.data["id"], "p");

    s.create("products", json!({"name": "cheap", "price": 5})).await;
    let rec = s.create("products", json!({"name": "dear", "price": 50})).await;
    let e = sse.expect("record.created").await;
    assert_eq!(e.data["record_id"], rec["id"], "the filter withheld the cheap one");
    assert_eq!(e.id.as_deref(), Some(e.data["cursor"].to_string().as_str()));
}

/// Replay after a disconnect: every event after the cursor exactly once, in
/// order, also while writes continue during the reconnect.
#[tokio::test]
async fn replay_after_disconnect_has_no_gaps_or_duplicates() {
    let s = Stack::start(fast_config()).await;
    products(&s).await;
    let token = s.admin_token().await;
    let mut ws = s.ws(Some(&token)).await;
    ws.subscribe(json!({"id": "p", "collection": "products"})).await;

    let mut created = Vec::new();
    let mut seen = Vec::new();
    for i in 0..5 {
        created.push(id_of(&s.create("products", json!({"name": format!("a{i}")})).await));
    }
    let mut cursor = 0;
    for _ in 0..5 {
        let e = ws.next_json().await;
        cursor = e["cursor"].as_i64().unwrap();
        seen.push(e["record_id"].as_str().unwrap().to_owned());
    }
    drop(ws);

    // Writes while nobody listens, and a writer that keeps going while the
    // client reconnects and replays.
    for i in 0..10 {
        created.push(id_of(&s.create("products", json!({"name": format!("b{i}")})).await));
    }
    let data = s.data.clone();
    let writer = tokio::spawn(async move {
        let mut ids = Vec::new();
        for i in 0..20 {
            let r = data
                .create_record(&admin(), "products", json!({"name": format!("c{i}")}))
                .await
                .unwrap();
            ids.push(r["id"].as_str().unwrap().to_owned());
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
        ids
    });
    let mut ws = s.ws(Some(&token)).await;
    let (replayed, ok) = ws
        .subscribe(json!({"id": "p", "collection": "products", "cursor": cursor}))
        .await;
    assert_eq!(ok["type"], "subscribed", "{ok}");
    assert_eq!(ok["replayed"].as_u64().unwrap() as usize, replayed.len());
    assert!(replayed.len() >= 10);
    let mut last = cursor;
    for e in &replayed {
        let c = e["cursor"].as_i64().unwrap();
        assert!(c > last, "cursors strictly increase");
        last = c;
        seen.push(e["record_id"].as_str().unwrap().to_owned());
    }
    created.extend(writer.await.unwrap());
    while seen.len() < created.len() {
        let e = ws.next_json().await;
        let c = e["cursor"].as_i64().unwrap();
        assert!(c > last, "no duplicate or out-of-order event: {c} after {last}");
        last = c;
        seen.push(e["record_id"].as_str().unwrap().to_owned());
    }
    assert_eq!(seen, created, "every change exactly once, in commit order");
    ws.quiet(Duration::from_millis(300)).await;
}

/// SSE replays from `Last-Event-ID` the way `EventSource` reconnects.
#[tokio::test]
async fn sse_replays_from_last_event_id() {
    let s = Stack::start(fast_config()).await;
    products(&s).await;
    let token = s.admin_token().await;
    let subs = enc(&json!([{"id": "p", "collection": "products"}]).to_string());
    let q = format!("?subscriptions={subs}");
    let mut sse = s.sse(Some(&token), &q, None).await.unwrap();
    sse.expect("welcome").await;
    sse.expect("subscribed").await;
    s.create("products", json!({"name": "x1"})).await;
    let last = sse.expect("record.created").await.id.unwrap();
    drop(sse);
    let mut missed = Vec::new();
    for i in 0..3 {
        missed.push(s.create("products", json!({"name": format!("y{i}")})).await["id"].clone());
    }
    let mut sse = s.sse(Some(&token), &q, Some(&last)).await.unwrap();
    sse.expect("welcome").await;
    for m in &missed {
        assert_eq!(&sse.expect("record.created").await.data["record_id"], m);
    }
    let ok = sse.expect("subscribed").await;
    assert_eq!(ok.data["replayed"], 3);
}

/// Subscriptions are authorised when they are made.
#[tokio::test]
async fn unauthorized_subscriptions_are_denied() {
    let s = Stack::start(fast_config()).await;
    products(&s).await;
    s.collection(json!({"name": "secrets", "fields": [{"name": "v", "type": "text"}]}))
        .await;
    s.rules.set("products", Rule::Allow);
    let (_uid, token, _) = s.user("ann@example.com").await;

    // Anonymous: nothing.
    let mut anon = s.ws(None).await;
    let (_, err) = anon.subscribe(json!({"id": "a", "collection": "products"})).await;
    assert_eq!(err["code"], "unauthenticated", "{err}");

    let mut ws = s.ws(Some(&token)).await;
    let (_, err) = ws.subscribe(json!({"id": "a", "collection": "secrets"})).await;
    assert_eq!(err["code"], "forbidden", "{err}");
    // A missing collection looks exactly like a forbidden one.
    let (_, err) = ws.subscribe(json!({"id": "a", "collection": "nope"})).await;
    assert_eq!(err["code"], "forbidden", "{err}");
    // Hidden fields cannot be probed with a filter.
    let (_, err) = ws
        .subscribe(json!({"id": "a", "collection": "products", "filter": "secret = 'x'"}))
        .await;
    assert_ne!(err["type"], "subscribed", "{err}");
    // Channels follow the channel rules.
    let (_, err) = ws.subscribe(json!({"id": "a", "channel": "private:x"})).await;
    assert_eq!(err["code"], "forbidden", "{err}");
    let (_, err) = ws.subscribe(json!({"id": "a"})).await;
    assert_eq!(err["code"], "invalid_subscription", "{err}");
    let (_, ok) = ws.subscribe(json!({"id": "a", "collection": "products"})).await;
    assert_eq!(ok["type"], "subscribed");

    // Nothing from the denied collection ever arrives.
    s.create("secrets", json!({"v": "top"})).await;
    s.create("products", json!({"name": "visible"})).await;
    let e = ws.next_json().await;
    assert_eq!(e["data"]["name"], "visible");

    // SSE: a denied subscription is an HTTP status.
    let subs = enc(&json!([{"id": "a", "collection": "secrets"}]).to_string());
    let (status, body) = s
        .sse(Some(&token), &format!("?subscriptions={subs}"), None)
        .await
        .err()
        .unwrap();
    assert_eq!(status, 403);
    assert_eq!(body["error"]["code"], "forbidden");
    let (status, _) = s
        .sse(None, &format!("?subscriptions={subs}"), None)
        .await
        .err()
        .unwrap();
    assert_eq!(status, 401);
    // An invalid query-string token is rejected before the upgrade.
    let (status, _) = s.ws_raw(None, "?access_token=nope").await.err().unwrap();
    assert_eq!(status, 401);
    // A valid query-string token works (browsers).
    let mut q = s.ws_raw(None, &format!("?access_token={token}")).await.unwrap();
    let w = q.next_json().await;
    assert!(w["actor"].as_str().unwrap().starts_with("user:users/"));
}

/// Every event is re-checked against the subscriber's current access: row
/// rules that change mid-stream, the owner predicate, hidden fields.
#[tokio::test]
async fn per_event_authorisation_never_leaks() {
    let s = Stack::start(fast_config()).await;
    products(&s).await;
    s.rules.set("products", Rule::Owner);
    let (ann, ann_token, _) = s.user("ann@example.com").await;
    let (bob, _bob_token, _) = s.user("bob@example.com").await;
    let admin_token = s.admin_token().await;

    let mut ws = s.ws(Some(&ann_token)).await;
    let (_, ok) = ws.subscribe(json!({"id": "p", "collection": "products"})).await;
    assert_eq!(ok["type"], "subscribed", "{ok}");
    let mut adm = s.ws(Some(&admin_token)).await;
    adm.subscribe(json!({"id": "p", "collection": "products"})).await;

    // Owner rule: Bob's record is withheld, Ann's delivered.
    s.create("products", json!({"name": "bobs", "owner": bob, "secret": "b"}))
        .await;
    let mine = s
        .create("products", json!({"name": "anns", "owner": ann, "secret": "a"}))
        .await;
    let e = ws.next_json().await;
    assert_eq!(e["data"]["name"], "anns");
    assert!(e["data"].get("secret").is_none(), "hidden field leaked: {e}");
    // The administrator sees both, hidden fields included.
    assert_eq!(adm.next_json().await["data"]["secret"], "b");
    assert_eq!(adm.next_json().await["data"]["secret"], "a");

    // Ann's record moves to Bob: the update is not visible to Ann.
    s.data
        .update_record(
            &admin(),
            "products",
            &id_of(&mine),
            json!({"owner": bob}),
            Default::default(),
        )
        .await
        .unwrap();
    // Rule changes to deny while subscribed: even Ann's own records stop.
    // (Delivery checks the rule in force when the event is sent, so wait
    // until this one has been checked before changing the rule back.)
    s.until("move withheld", |st| st.events_withheld >= 2).await;
    s.rules.set("products", Rule::Deny);
    s.create("products", json!({"name": "anns2", "owner": ann})).await;
    s.until("deny withheld", |st| st.events_withheld >= 3).await;
    // Rule changes back: delivery resumes with the next event.
    s.rules.set("products", Rule::Owner);
    s.create("products", json!({"name": "anns3", "owner": ann})).await;
    let e = ws.next_json().await;
    assert_eq!(e["data"]["name"], "anns3", "nothing withheld leaked in between: {e}");
    ws.quiet(Duration::from_millis(300)).await;
    assert!(s.rt.stats().events_withheld >= 3);

    // Replay applies the *current* rule too.
    s.rules.set("products", Rule::Allow);
    let (replayed, ok) = ws
        .subscribe(json!({"id": "r", "collection": "products", "cursor": 0}))
        .await;
    assert_eq!(ok["type"], "subscribed");
    assert_eq!(replayed.len(), 5, "with Allow every stored event replays: {replayed:?}");
    s.rules.set("products", Rule::Owner);
    let (replayed, _) = ws
        .subscribe(json!({"id": "r2", "collection": "products", "cursor": 0}))
        .await;
    let names: Vec<_> = replayed.iter().map(|e| e["data"]["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["anns", "anns2", "anns3"], "{replayed:?}");
}

/// A revoked session closes its sockets with 4001; SSE streams end with a
/// close event.
#[tokio::test]
async fn revoked_session_disconnects() {
    let s = Stack::start(fast_config()).await;
    products(&s).await;
    s.rules.set("products", Rule::Allow);
    let (_uid, token, _sid) = s.user("ann@example.com").await;
    let mut ws = s.ws(Some(&token)).await;
    ws.subscribe(json!({"id": "p", "collection": "products"})).await;
    let subs = enc(&json!([{"id": "p", "collection": "products"}]).to_string());
    let mut sse = s
        .sse(Some(&token), &format!("?subscriptions={subs}"), None)
        .await
        .unwrap();
    sse.expect("welcome").await;
    sse.expect("subscribed").await;

    let caller = s.auth.authenticate_access_token(&token).await.unwrap();
    s.auth.logout(&caller, &common::ci()).await.unwrap();

    assert_eq!(ws.close_code().await, Some(close::UNAUTHENTICATED));
    let e = sse.expect("close").await;
    assert_eq!(e.data["code"], close::UNAUTHENTICATED);
    assert!(sse.next().await.is_none(), "the stream ends");
    s.until("revocations", |st| st.revoked >= 2).await;
    s.until("connections closed", |st| st.connections == 0).await;
    // The token no longer opens a connection.
    let (status, _) = s.ws_raw(Some(&token), "").await.err().unwrap();
    assert_eq!(status, 401);
}

/// A client that stops reading is closed as a slow consumer (4008) instead
/// of buffering without bound; a client that reads is unaffected.
#[tokio::test]
async fn slow_consumer_is_dropped_with_4008() {
    let cfg = RealtimeConfig {
        outbound_queue: 4,
        send_timeout: Duration::from_millis(300),
        ..fast_config()
    };
    let s = Stack::start(cfg).await;
    s.collection(json!({"name": "blobs", "fields": [{"name": "body", "type": "text"}]}))
        .await;
    let token = s.admin_token().await;
    let mut slow = s.ws_small_buffer(&token).await;
    slow.subscribe(json!({"id": "b", "collection": "blobs"})).await;
    let mut fast = s.ws(Some(&token)).await;
    fast.subscribe(json!({"id": "b", "collection": "blobs"})).await;
    const N: usize = 200;
    let fast_reader = tokio::spawn(async move {
        let mut n = 0;
        while n < N {
            match fast.next_within(Duration::from_secs(30)).await {
                Some(Got::Json(_)) => n += 1,
                Some(Got::Close(c, r)) => panic!("fast reader closed after {n}: {c:?} {r}"),
                None => panic!("fast reader stalled after {n}"),
            }
        }
        n
    });
    let body = "x".repeat(16 * 1024);
    let batch: Vec<Json> = (0..N).map(|_| json!({"body": body})).collect();
    s.data.create_records(&admin(), "blobs", batch).await.unwrap();
    s.until("a slow consumer", |st| st.slow_consumers >= 1).await;
    assert_eq!(slow.close_code().await, Some(close::SLOW_CONSUMER));
    assert_eq!(fast_reader.await.unwrap(), N, "a reading client gets every event");
    assert_eq!(s.rt.stats().slow_consumers, 1);
}

/// Per-principal connection limit, per-connection subscription limit,
/// message size limit, publish rate limit.
#[tokio::test]
async fn limits_are_enforced() {
    let cfg = RealtimeConfig {
        max_connections_per_principal: 2,
        max_subscriptions_per_connection: 2,
        max_message_bytes: 1024,
        publish_per_second: 3,
        ..fast_config()
    };
    let s = Stack::start(cfg).await;
    products(&s).await;
    s.rules.set("products", Rule::Allow);
    let (_uid, token, _) = s.user("ann@example.com").await;
    let mut a = s.ws(Some(&token)).await;
    let _b = s.ws(Some(&token)).await;
    let (status, body) = s.ws_raw(Some(&token), "").await.err().unwrap();
    assert_eq!(status, 429, "{body}");
    // Another principal is not affected.
    let (_, other, _) = s.user("bob@example.com").await;
    let _c = s.ws(Some(&other)).await;
    assert!(s.rt.stats().rejected >= 1);

    a.subscribe(json!({"id": "1", "collection": "products"})).await;
    a.subscribe(json!({"id": "2", "channel": "public:x"})).await;
    let (_, err) = a.subscribe(json!({"id": "3", "collection": "products"})).await;
    assert_eq!(err["code"], "too_many_subscriptions");
    let (_, err) = a.subscribe(json!({"id": "1", "collection": "products"})).await;
    assert_eq!(err["code"], "duplicate_subscription");
    a.send(json!({"type": "unsubscribe", "id": "1", "ref": 7})).await;
    let r = a.next_json().await;
    assert_eq!((r["type"].as_str(), r["ref"].as_i64()), (Some("unsubscribed"), Some(7)));

    // Publish rate limit (burst 3).
    let mut limited = 0;
    for i in 0..6 {
        a.send(json!({"type": "publish", "channel": "public:x", "data": i, "ref": i}))
            .await;
    }
    let mut acks = 0;
    while acks + limited < 6 {
        let m = a.next_json().await;
        match m["type"].as_str() {
            Some("ack") => acks += 1,
            Some("error") => {
                assert_eq!(m["code"], "rate_limited");
                limited += 1;
            }
            Some("message") => {}
            other => panic!("unexpected {other:?}"),
        }
    }
    assert!(limited >= 2, "rate limited {limited} of 6");

    // A message above the limit closes with 1009.
    a.send(json!({"type": "ping", "pad": "x".repeat(4096)})).await;
    assert_eq!(a.close_code().await, Some(close::MESSAGE_TOO_BIG));
    // The slot is released: a new connection fits again.
    s.until("slot released", |st| st.connections == 2).await;
    let _d = s.ws(Some(&token)).await;
}

/// Server pings keep idle connections alive only while the client answers;
/// a silent client is closed with 4002. App-level ping gets a pong.
#[tokio::test]
async fn heartbeats_and_idle_timeout() {
    let cfg = RealtimeConfig {
        heartbeat_interval: Duration::from_millis(100),
        idle_timeout: Duration::from_millis(400),
        ..fast_config()
    };
    let s = Stack::start(cfg).await;
    let token = s.admin_token().await;
    let mut ws = s.ws(Some(&token)).await;
    ws.send(json!({"type": "ping", "ref": "r1"})).await;
    let p = ws.next_json().await;
    assert_eq!((p["type"].as_str(), p["ref"].as_str()), (Some("pong"), Some("r1")));
    // Reading answers pings automatically: still open after several periods.
    ws.quiet(Duration::from_millis(700)).await;
    // Stop reading (so no pongs go back): closed for inactivity.
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(ws.close_code().await, Some(close::HEARTBEAT_TIMEOUT));
}

/// Channels: broadcast to subscribers, presence join/leave/state, REST
/// publish and presence, disconnect leaves.
#[tokio::test]
async fn channels_and_presence() {
    let s = Stack::start(fast_config()).await;
    let (_a, ta, _) = s.user("ann@example.com").await;
    let (_b, tb, _) = s.user("bob@example.com").await;
    let mut ann = s.ws(Some(&ta)).await;
    let mut bob = s.ws(Some(&tb)).await;
    ann.subscribe(json!({"id": "room", "channel": "public:lobby"})).await;
    bob.subscribe(json!({"id": "r", "channel": "public:lobby"})).await;

    ann.send(json!({"type": "presence.join", "channel": "public:lobby", "meta": {"name": "Ann"}, "ref": 1}))
        .await;
    let mut state = None;
    for _ in 0..2 {
        let m = ann.next_json().await;
        if m["type"] == "presence.state" {
            state = Some(m);
        } else {
            assert_eq!(m["type"], "presence.join");
        }
    }
    let state = state.unwrap();
    assert_eq!(state["members"].as_array().unwrap().len(), 1);
    assert_eq!(state["members"][0]["meta"]["name"], "Ann");
    let j = bob.next_json().await;
    assert_eq!(
        (j["type"].as_str(), j["id"].as_str()),
        (Some("presence.join"), Some("r"))
    );
    assert_eq!(j["member"]["meta"]["name"], "Ann");

    // REST presence and publish (as the administrator through the service).
    let members = s.rt.presence(&admin(), "public:lobby").unwrap();
    assert_eq!(members.len(), 1);
    s.rt.publish(&admin(), "public:lobby", json!({"text": "hi"}))
        .await
        .unwrap();
    for ws in [&mut ann, &mut bob] {
        let m = ws.next_json().await;
        assert_eq!(m["type"], "message");
        assert_eq!(m["data"]["text"], "hi");
        assert_eq!(m["from"], "admin:root");
    }
    // A user publishes over the socket.
    bob.send(json!({"type": "publish", "channel": "public:lobby", "data": "yo", "ref": "x"}))
        .await;
    let m = ann.next_json().await;
    assert_eq!(m["data"], "yo");
    assert!(m["from"].as_str().unwrap().starts_with("user:users/"));
    // Publishing to a forbidden channel fails; nobody receives it.
    bob.send(json!({"type": "publish", "channel": "private:x", "data": 1}))
        .await;
    let mut got_err = false;
    for _ in 0..3 {
        let m = bob.next_json().await;
        if m["type"] == "error" {
            assert_eq!(m["code"], "forbidden");
            got_err = true;
            break;
        }
    }
    assert!(got_err);
    // Anonymous REST-level checks.
    assert!(
        s.rt.presence(&forgebase_data::AccessContext::anonymous(), "public:lobby")
            .is_err()
    );

    drop(ann);
    let l = loop {
        let m = bob.next_json().await;
        if m["type"] == "presence.leave" {
            break m;
        }
    };
    assert_eq!(l["member"]["meta"]["name"], "Ann");
    assert!(s.rt.presence(&admin(), "public:lobby").unwrap().is_empty());
}

/// An expired cursor gets a `reset` instead of a silent gap.
#[tokio::test]
async fn expired_cursor_gets_reset() {
    let cfg = RealtimeConfig {
        max_retained_events: 5,
        prune_interval: Duration::from_millis(100),
        ..fast_config()
    };
    let s = Stack::start(cfg).await;
    products(&s).await;
    for i in 0..20 {
        s.create("products", json!({"name": format!("n{i}")})).await;
    }
    for _ in 0..100 {
        if s.outbox_len().await <= 6 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(s.outbox_len().await <= 6);
    let token = s.admin_token().await;
    let mut ws = s.ws(Some(&token)).await;
    let (events, ok) = ws
        .subscribe(json!({"id": "p", "collection": "products", "cursor": 1}))
        .await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["type"], "reset");
    assert_eq!(events[0]["reason"], "cursor_expired");
    assert_eq!(ok["reset"], "cursor_expired");
    assert_eq!(ok["cursor"], 20);
    // A cursor from the future is refused the same way.
    let (events, _) = ws
        .subscribe(json!({"id": "q", "collection": "products", "cursor": 999}))
        .await;
    assert_eq!(events[0]["reason"], "cursor_invalid");
    // Live delivery continues.
    s.create("products", json!({"name": "live"})).await;
    for _ in 0..2 {
        assert_eq!(ws.next_json().await["data"]["name"], "live");
    }
}

/// Shutdown closes every connection with 1001.
#[tokio::test]
async fn shutdown_closes_connections() {
    let s = Stack::start(fast_config()).await;
    let token = s.admin_token().await;
    let mut ws = s.ws(Some(&token)).await;
    s.rt.shutdown();
    assert_eq!(ws.close_code().await, Some(close::GOING_AWAY));
}
