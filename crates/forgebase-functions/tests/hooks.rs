//! Record hooks: before-hooks reject writes (and fail closed), after-hooks
//! run for committed changes from the realtime outbox.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{READ_INPUT, Wat, admin, harness, req};
use forgebase_core::ErrorCode;
use forgebase_data::{AccessContext, UpdateOptions};
use forgebase_functions::{HookEvent, HookSpec, LimitsInput};
use serde_json::json;

/// A before-hook that rejects any write whose input contains `forbidden`.
fn guard_module() -> String {
    let mut w = Wat::new(&[]);
    let needle = w.s("forbidden");
    let deny = w.s(r#"{"allow":false,"reason":"forbidden words are not allowed"}"#);
    let allow = w.s(r#"{"allow":true}"#);
    w.wasm(&format!(
        "{READ_INPUT}
         (if (call $contains (i32.const 65536) (local.get $n) {needle})
           (then (call $emit {deny}))
           (else (call $emit {allow})))
         (i32.const 0)"
    ))
}

fn hook(collection: &str, events: &[HookEvent]) -> HookSpec {
    HookSpec {
        collection: collection.into(),
        events: events.to_vec(),
    }
}

#[tokio::test]
async fn before_hooks_reject_creates_updates_and_deletes() {
    let h = harness().await;
    h.collection(json!({"name": "posts", "fields": [{"name": "title", "type": "text"}]}))
        .await;
    let mut r = req("moderate", &guard_module(), &[]);
    r.hooks = vec![hook(
        "posts",
        &[HookEvent::BeforeCreate, HookEvent::BeforeUpdate, HookEvent::BeforeDelete],
    )];
    h.deploy(r).await;
    let user = admin();

    let ok = h.data.create_record(&user, "posts", json!({"title": "hello"})).await.unwrap();
    let id = ok["id"].as_str().unwrap().to_owned();

    let err = h
        .data
        .create_record(&user, "posts", json!({"title": "a forbidden word"}))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "hook_rejected", "{err}");
    assert!(err.to_string().contains("moderate: forbidden words"), "{err}");
    assert_eq!(
        h.data.list_records(&admin(), "posts", &Default::default()).await.unwrap().items.len(),
        1,
        "the rejected record was not written"
    );

    let err = h
        .data
        .update_record(&user, "posts", &id, json!({"title": "forbidden"}), UpdateOptions::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "hook_rejected");
    let rec = h.data.get_record(&admin(), "posts", &id, &Default::default()).await.unwrap();
    assert_eq!(rec["title"], "hello", "the update did not happen");

    // Delete: the input carries the id; reject deletes of one id.
    let mut w = Wat::new(&[]);
    let needle = w.s(&id);
    let deny = w.s(r#"{"allow":false,"reason":"this post is pinned"}"#);
    let body = format!(
        "{READ_INPUT}
         (if (call $contains (i32.const 65536) (local.get $n) {needle}) (then (call $emit {deny})))
         (i32.const 0)"
    );
    let mut r = req("pin", &w.wasm(&body), &[]);
    r.hooks = vec![hook("posts", &[HookEvent::BeforeDelete])];
    h.deploy(r).await;
    let err = h.data.delete_record(&user, "posts", &id, None).await.unwrap_err();
    assert_eq!(err.code(), "hook_rejected");
    assert!(err.to_string().contains("pinned"));

    // The system principal bypasses hooks (migrations, internal jobs).
    h.data
        .delete_record(&AccessContext::system(), "posts", &id, None)
        .await
        .unwrap();

    // Rejections are in the execution log.
    let logs = h.svc.logs(&admin(), "moderate", 20).await.unwrap();
    assert!(logs.iter().any(|l| l.status == "rejected" && l.trigger == "hook"));
}

#[tokio::test]
async fn a_failing_before_hook_fails_closed() {
    let h = harness().await;
    h.collection(json!({"name": "orders", "fields": [{"name": "n", "type": "number"}]}))
        .await;
    let w = Wat::new(&[]);
    let mut r = req("broken", &w.wasm("(loop $l (br $l)) (i32.const 0)"), &[]);
    r.hooks = vec![hook("orders", &[HookEvent::BeforeCreate])];
    r.limits = LimitsInput {
        fuel: Some(1_000_000),
        ..LimitsInput::default()
    };
    h.deploy(r).await;
    let err = h.data.create_record(&admin(), "orders", json!({"n": 1})).await.unwrap_err();
    assert_eq!(err.code(), "hook_failed", "{err}");
    assert!(err.to_string().contains("fuel"), "{err}");

    // Bad output also fails closed.
    let mut w = Wat::new(&[]);
    let s = w.s(r#"{"allow":"maybe"}"#);
    let mut r = req("broken", &w.wasm(&format!("(call $emit {s}) (i32.const 0)")), &[]);
    r.hooks = vec![hook("orders", &[HookEvent::BeforeCreate])];
    h.deploy(r).await;
    let err = h.data.create_record(&admin(), "orders", json!({"n": 1})).await.unwrap_err();
    assert_eq!(err.code(), "hook_failed");
    assert_eq!(
        h.data.list_records(&admin(), "orders", &Default::default()).await.unwrap().items.len(),
        0
    );
}

#[tokio::test]
async fn after_hooks_run_for_committed_changes_only() {
    let h = harness().await;
    h.collection(json!({"name": "posts", "fields": [{"name": "title", "type": "text"}]}))
        .await;
    h.collection(json!({"name": "audit", "schema_mode": "flexible", "fields": [{"name": "event", "type": "text"}]}))
        .await;
    // Writes its whole input as an audit document.
    let mut w = Wat::new(&["db_create"]);
    let pre = w.s(r#"{"collection":"audit","data":"#);
    let close = w.s("}");
    let body = format!(
        "{READ_INPUT}
         (local.set $p (call $copy (i32.const {args}) {pre}))
         (local.set $p (call $copy (local.get $p) (i32.const 65536) (local.get $n)))
         (local.set $p (call $copy (local.get $p) {close}))
         (local.set $r (call $db_create (i32.const {args}) (i32.sub (local.get $p) (i32.const {args}))))
         (if (i32.lt_s (local.get $r) (i32.const 0)) (then (return (i32.const 1))))
         (i32.const 0)",
        args = common::ARGS
    );
    let mut r = req("auditor", &w.wasm(&body), &["db.write"]);
    r.hooks = vec![hook("posts", &[HookEvent::AfterCreate, HookEvent::AfterDelete])];
    h.deploy(r).await;
    h.grant("auditor", "data:audit:*").await;

    // A rejected (rolled back) write produces no event.
    let mut w = Wat::new(&[]);
    let s = w.s(r#"{"allow":false}"#);
    let mut gate = req("gate", &w.wasm(&format!("(call $emit {s}) (i32.const 0)")), &[]);
    gate.hooks = vec![hook("posts", &[HookEvent::BeforeUpdate])];
    h.deploy(gate).await;

    let rec = h.data.create_record(&admin(), "posts", json!({"title": "one"})).await.unwrap();
    let id = rec["id"].as_str().unwrap().to_owned();
    assert!(
        h.data
            .update_record(&admin(), "posts", &id, json!({"title": "x"}), UpdateOptions::default())
            .await
            .is_err()
    );
    h.data.delete_record(&admin(), "posts", &id, None).await.unwrap();

    let runs = h.svc.run_after_hooks().await.unwrap();
    assert_eq!(runs, 2, "create and delete; the rejected update never committed");
    let audit = h.data.list_records(&admin(), "audit", &Default::default()).await.unwrap();
    let events: Vec<String> = audit
        .items
        .iter()
        .map(|r| r["event"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(events.len(), 2, "{audit:?}");
    assert!(events.contains(&"after_create".to_owned()) && events.contains(&"after_delete".to_owned()));
    let created = audit.items.iter().find(|r| r["event"] == "after_create").unwrap();
    assert_eq!(created["record"]["title"], "one", "{created:?}");
    assert_eq!(created["record_id"], id.as_str());

    // The cursor persists: nothing runs twice, and the auditor's own audit
    // writes did not trigger it (it hooks posts, not audit).
    assert_eq!(h.svc.run_after_hooks().await.unwrap(), 0);
    assert_eq!(
        h.data.list_records(&admin(), "audit", &Default::default()).await.unwrap().items.len(),
        2
    );
}

#[tokio::test]
async fn hook_recursion_is_bounded() {
    let h = harness().await;
    h.collection(json!({"name": "loops", "schema_mode": "flexible", "fields": [{"name": "x", "type": "text"}]}))
        .await;
    // A before-create hook on `loops` that itself creates a `loops` record.
    let mut w = Wat::new(&["db_create"]);
    let args = w.s(r#"{"collection":"loops","data":{"x":"again"}}"#);
    let allow = w.s(r#"{"allow":true}"#);
    let body = format!(
        "(local.set $r (call $db_create {args}))
         (if (i32.lt_s (local.get $r) (i32.const 0)) (then (return (i32.const 2))))
         (call $emit {allow})
         (i32.const 0)"
    );
    let mut r = req("again", &w.wasm(&body), &["db.write"]);
    r.hooks = vec![hook("loops", &[HookEvent::BeforeCreate])];
    h.deploy(r).await;
    h.grant("again", "data:loops:*").await;
    let err = h.data.create_record(&admin(), "loops", json!({"x": "start"})).await.unwrap_err();
    assert_eq!(err.code(), "hook_failed", "{err}");
    assert_eq!(
        h.data.list_records(&admin(), "loops", &Default::default()).await.unwrap().items.len(),
        0,
        "nothing committed: every nested write was rejected in turn"
    );
}
