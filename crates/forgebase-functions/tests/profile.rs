//! Function profiling (ADR-0036 §3): a JavaScript function's host calls,
//! cold start, fuel, memory and timeline, in the aggregates and in the
//! recorded executions.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{admin, harness};
use forgebase_functions::{DeployRequest, InvocationQuery, Trigger};
use serde_json::json;

const SRC: &str = r#"import { serve } from "forgebase";
serve(async (req, ctx) => {
  await ctx.db.from("items").create({ title: "a" });
  await ctx.db.from("items").list();
  await ctx.kv.set("k", "v");
  await ctx.kv.get("k");
  return Response.json({ ok: true });
});
"#;

async fn deployed() -> common::H {
    let h = harness().await;
    h.collection(json!({"name": "items", "fields": [{"name": "title", "type": "text"}]}))
        .await;
    let req: DeployRequest = serde_json::from_value(json!({
        "name": "prof",
        "source": SRC,
        "access": "public",
        "capabilities": ["db.read", "db.write", "kv", "log"],
    }))
    .unwrap();
    h.svc.deploy(&admin(), req).await.unwrap();
    h.grant("prof", "data:items:create").await;
    h.grant("prof", "data:items:list").await;
    h
}

#[tokio::test]
async fn host_calls_cold_starts_fuel_and_memory_are_profiled() {
    let h = deployed().await;
    // A deploy leaves the script cached: forget it, as a restart would.
    h.svc.evict_caches();
    for _ in 0..2 {
        h.svc
            .invoke("prof", Trigger::Test, &json!({}), Some("req-1".into()))
            .await
            .unwrap();
    }
    let live = h.svc.profiler().peek();
    assert_eq!(live.functions.len(), 1);
    let a = &live.functions[0];
    assert_eq!(
        (a.function.as_str(), a.invocations, a.errors, a.timeouts),
        ("prof", 2, 0, 0)
    );
    assert_eq!(a.cold_starts, 1, "only the first execution loads the script");
    assert_eq!(a.host["db.create"].count, 2);
    assert_eq!(a.host["db.list"].count, 2);
    assert_eq!(a.host["kv"].count, 4);
    assert_eq!(a.host_calls, 8);
    assert!(a.host.values().all(|h| h.errors == 0), "{:?}", a.host);
    assert!(
        a.fuel_total > 0 && a.fuel_max > 0 && a.fuel_max <= a.fuel_limit,
        "{a:?}"
    );
    assert!(a.memory_max > 0 && a.memory_max <= a.memory_limit, "{a:?}");
    assert_eq!(a.sketch.count(), 2);

    // The recorded executions carry the same numbers, per invocation.
    let rows = h
        .svc
        .invocations(InvocationQuery {
            function: Some("prof".into()),
            limit: 10,
            ..InvocationQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    let (newest, oldest) = (&rows[0], &rows[1]);
    assert_eq!(newest.log.status, "ok");
    assert_eq!(newest.log.request_id.as_deref(), Some("req-1"));
    assert_eq!(newest.log.trigger, "test");
    let (n, o) = (newest.profile.as_ref().unwrap(), oldest.profile.as_ref().unwrap());
    assert_eq!(n["cold_start"], false);
    assert_eq!(o["cold_start"], true);
    assert!(o["prepare_us"].as_u64().unwrap() > 0, "{o}");
    assert_eq!(n["host_calls"], 4);
    assert_eq!(n["host"]["kv"]["count"], 2);
    let ops: Vec<&str> = n["timeline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["op"].as_str().unwrap())
        .collect();
    assert_eq!(ops, ["db.create", "db.list", "kv", "kv"]);
    let t = n["timeline"].as_array().unwrap();
    assert!(
        t.windows(2).all(|w| w[0]["at_us"].as_u64() <= w[1]["at_us"].as_u64()),
        "ordered in time"
    );
    assert!(
        n["fuel_used"].as_u64().unwrap() > 0 && n["fuel_limit"].as_u64().unwrap() > n["fuel_used"].as_u64().unwrap()
    );
    assert!(n["memory_peak_bytes"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn failures_and_timeouts_are_counted_with_their_status() {
    let h = deployed().await;
    let boom: DeployRequest = serde_json::from_value(json!({
        "name": "boom",
        "source": "import { serve } from \"forgebase\";\nserve(async () => { throw new Error(\"x\"); });\n",
        "access": "public",
        "capabilities": ["log"],
    }))
    .unwrap();
    h.svc.deploy(&admin(), boom).await.unwrap();
    h.svc.profiler().drain();
    assert!(h.svc.invoke("boom", Trigger::Test, &json!({}), None).await.is_err());
    let d = h.svc.profiler().drain();
    let a = d.functions.iter().find(|a| a.function == "boom").unwrap();
    assert_eq!((a.invocations, a.errors, a.timeouts), (1, 1, 0));
    let rows = h
        .svc
        .invocations(InvocationQuery {
            function: Some("boom".into()),
            only_errors: true,
            limit: 5,
            ..InvocationQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].log.status, "ok");
    assert_eq!(rows[0].profile.as_ref().unwrap()["host_calls"], 0);
}

#[tokio::test]
async fn profiling_can_be_switched_off() {
    let h = deployed().await;
    h.svc.profiler().set_enabled(false);
    h.svc.invoke("prof", Trigger::Test, &json!({}), None).await.unwrap();
    assert!(h.svc.profiler().peek().functions.is_empty());
    let rows = h
        .svc
        .invocations(InvocationQuery {
            function: Some("prof".into()),
            limit: 5,
            ..InvocationQuery::default()
        })
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert!(rows[0].profile.is_none(), "no profile while it is off");
    h.svc.profiler().set_enabled(true);
}
