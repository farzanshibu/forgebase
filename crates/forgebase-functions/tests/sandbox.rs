//! The sandbox: fuel, memory and wall-clock limits, concurrency, capability
//! checks at deploy and at run time, versions and rollback, sealed secrets.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::{Duration, Instant};

use common::{Wat, admin, constant, harness, out_json, req, wat_b64};
use forgebase_core::ErrorCode;
use forgebase_data::db::{Backend, Statement, Value};
use forgebase_functions::{ExecError, FunctionError, LimitsInput, Trigger};
use serde_json::json;

const SPIN: &str = "(loop $l (br $l)) (i32.const 0)";

fn exec_err(e: FunctionError) -> ExecError {
    match e {
        FunctionError::Execution(x) => x,
        other => panic!("expected an execution error, got {other:?}"),
    }
}

#[tokio::test]
async fn fuel_exhaustion_stops_an_infinite_loop() {
    let h = harness().await;
    let w = Wat::new(&[]);
    let mut r = req("spin", &w.wasm(SPIN), &[]);
    r.limits = LimitsInput {
        fuel: Some(2_000_000),
        timeout_ms: Some(30_000),
        ..LimitsInput::default()
    };
    h.deploy(r).await;
    let started = Instant::now();
    let err = h
        .svc
        .invoke("spin", Trigger::Direct, &json!({}), None)
        .await
        .unwrap_err();
    assert_eq!(exec_err(err), ExecError::FuelExhausted(2_000_000));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "fuel, not the clock, stopped it"
    );
    let logs = h.svc.logs(&admin(), "spin", 10).await.unwrap();
    assert_eq!(logs[0].status, "fuel_exhausted");
    assert!(logs[0].fuel_used >= 1_900_000, "{}", logs[0].fuel_used);
}

#[tokio::test]
async fn memory_growth_beyond_the_cap_traps() {
    let h = harness().await;
    // Grows by one page (64 KiB) until memory.grow fails or the host refuses.
    let mut w = Wat::new(&[]);
    w.pages = 6;
    let body = "(loop $l (br_if $l (i32.ne (memory.grow (i32.const 1)) (i32.const -1)))) (i32.const 0)";
    let mut r = req("hog", &w.wasm(body), &[]);
    r.limits = LimitsInput {
        memory_bytes: Some(1024 * 1024),
        ..LimitsInput::default()
    };
    h.deploy(r).await;
    let err = h
        .svc
        .invoke("hog", Trigger::Direct, &json!({}), None)
        .await
        .unwrap_err();
    assert_eq!(exec_err(err), ExecError::MemoryLimit(1024 * 1024));
    assert_eq!(h.svc.logs(&admin(), "hog", 1).await.unwrap()[0].status, "memory_limit");

    // An initial memory above the limit is refused at deploy.
    let mut w = Wat::new(&[]);
    w.pages = 64; // 4 MiB
    let mut r = req("big", &w.wasm("(i32.const 0)"), &[]);
    r.limits.memory_bytes = Some(1024 * 1024);
    let err = h.svc.deploy(&admin(), r).await.unwrap_err();
    assert_eq!(err.code(), "memory_limit_too_small");
}

#[tokio::test]
async fn wall_clock_timeout_interrupts_a_busy_guest() {
    let h = harness().await;
    let w = Wat::new(&[]);
    let mut r = req("slow", &w.wasm(SPIN), &[]);
    r.limits = LimitsInput {
        fuel: Some(20_000_000_000),
        timeout_ms: Some(150),
        ..LimitsInput::default()
    };
    h.deploy(r).await;
    let started = Instant::now();
    let err = h
        .svc
        .invoke("slow", Trigger::Direct, &json!({}), None)
        .await
        .unwrap_err();
    let took = started.elapsed();
    assert_eq!(exec_err(err), ExecError::Timeout(150));
    assert!(took >= Duration::from_millis(140), "{took:?}");
    assert!(took < Duration::from_secs(5), "{took:?}");
    assert_eq!(h.svc.logs(&admin(), "slow", 1).await.unwrap()[0].status, "timeout");
}

#[tokio::test]
async fn concurrency_limit_rejects_extra_executions() {
    let h = harness().await;
    let w = Wat::new(&[]);
    let mut r = req("busy", &w.wasm(SPIN), &[]);
    r.limits = LimitsInput {
        fuel: Some(20_000_000_000),
        timeout_ms: Some(1_000),
        max_concurrency: Some(1),
        ..LimitsInput::default()
    };
    h.deploy(r).await;
    let svc = h.svc.clone();
    let first = tokio::spawn(async move { svc.invoke("busy", Trigger::Direct, &json!({}), None).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let second = h
        .svc
        .invoke("busy", Trigger::Direct, &json!({}), None)
        .await
        .unwrap_err();
    assert_eq!(second.code(), "function_busy");
    assert!(matches!(
        first.await.unwrap(),
        Err(FunctionError::Execution(ExecError::Timeout(_)))
    ));
}

#[tokio::test]
async fn traps_and_failure_status_are_reported() {
    let h = harness().await;
    let w = Wat::new(&[]);
    h.deploy(req("boom", &w.wasm("unreachable"), &[])).await;
    let err = exec_err(
        h.svc
            .invoke("boom", Trigger::Direct, &json!({}), None)
            .await
            .unwrap_err(),
    );
    assert!(
        matches!(err, ExecError::Trap(ref m) if m.contains("unreachable")),
        "{err:?}"
    );

    let mut w = Wat::new(&[]);
    let s = w.s("bad input");
    h.deploy(req("fails", &w.wasm(&format!("(call $emit {s}) (i32.const 3)")), &[]))
        .await;
    let err = exec_err(
        h.svc
            .invoke("fails", Trigger::Direct, &json!({}), None)
            .await
            .unwrap_err(),
    );
    assert_eq!(
        err,
        ExecError::Failed {
            status: 3,
            message: Some("bad input".into())
        }
    );
}

#[tokio::test]
async fn deploy_checks_imports_against_granted_capabilities() {
    let h = harness().await;
    let w = Wat::new(&["db_create"]);
    let err = h
        .svc
        .deploy(&admin(), req("w", &w.wasm("(i32.const 0)"), &["db.read"]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "capability_not_granted", "{err}");

    // No WASI, no foreign imports.
    let wasi = wat_b64(
        r#"(module (import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1) (func (export "handle") (result i32) (i32.const 0)))"#,
    );
    let err = h.svc.deploy(&admin(), req("wasi", &wasi, &[])).await.unwrap_err();
    assert_eq!(err.code(), "invalid_module", "{err}");

    // Required exports and a valid binary.
    let no_handle = wat_b64(r#"(module (memory (export "memory") 1))"#);
    assert_eq!(
        h.svc
            .deploy(&admin(), req("x", &no_handle, &[]))
            .await
            .unwrap_err()
            .code(),
        "invalid_module"
    );
    assert_eq!(
        h.svc
            .deploy(&admin(), req("x", "AGFzbQEAAAA=", &[]))
            .await
            .unwrap_err()
            .code(),
        "invalid_module"
    );
    assert_eq!(
        h.svc.deploy(&admin(), req("x", "AAAA", &[])).await.unwrap_err().code(),
        "invalid_module"
    );
    assert_eq!(
        h.svc
            .deploy(&admin(), req("x", &constant("{}"), &["root"]))
            .await
            .unwrap_err()
            .code(),
        "invalid_capability"
    );
    assert_eq!(
        h.svc
            .deploy(&admin(), req("Bad Name", &constant("{}"), &[]))
            .await
            .unwrap_err()
            .code(),
        "invalid_function_name"
    );
    // Not an administrator.
    let user = forgebase_data::AccessContext::user("users", "u1");
    assert_eq!(
        h.svc
            .deploy(&user, req("x", &constant("{}"), &[]))
            .await
            .unwrap_err()
            .code(),
        "forbidden"
    );
    assert_eq!(
        h.svc
            .deploy(
                &forgebase_data::AccessContext::anonymous(),
                req("x", &constant("{}"), &[])
            )
            .await
            .unwrap_err()
            .code(),
        "unauthenticated"
    );
}

#[tokio::test]
async fn versions_and_rollback_restore_module_and_configuration() {
    let h = harness().await;
    let d1 = h.deploy(req("greet", &constant(r#"{"v":1}"#), &[])).await;
    assert_eq!((d1.version.version, d1.function.active_version), (1, 1));
    let mut r2 = req("greet", &constant(r#"{"v":2}"#), &["log"]);
    r2.env.insert("MODE".into(), "two".into());
    let d2 = h.deploy(r2).await;
    assert_eq!((d2.version.version, d2.function.active_version), (2, 2));
    let run = |h: &common::H| {
        let svc = h.svc.clone();
        async move { out_json(&svc.invoke("greet", Trigger::Direct, &json!({}), None).await.unwrap()) }
    };
    assert_eq!(run(&h).await, json!({"v": 2}));

    // Rollback to the previous version: module and configuration.
    let f = h.svc.rollback(&admin(), "greet", None).await.unwrap();
    assert_eq!((f.active_version, f.latest_version), (1, 2));
    assert_eq!(run(&h).await, json!({"v": 1}));
    let versions = h.svc.versions(&admin(), "greet").await.unwrap();
    assert_eq!(versions.len(), 2);
    assert!(versions[1].active && !versions[0].active);
    assert!(versions[1].config.capabilities.is_empty());
    assert_eq!(versions[0].config.env.get("MODE").map(String::as_str), Some("two"));

    // Forward again by number; errors for missing and for "no previous".
    h.svc.rollback(&admin(), "greet", Some(2)).await.unwrap();
    assert_eq!(run(&h).await, json!({"v": 2}));
    assert_eq!(
        h.svc.rollback(&admin(), "greet", Some(9)).await.unwrap_err().code(),
        "function_version_not_found"
    );
    h.svc.rollback(&admin(), "greet", Some(1)).await.unwrap();
    assert_eq!(
        h.svc.rollback(&admin(), "greet", None).await.unwrap_err().code(),
        "no_previous_version"
    );

    // A deploy that does not activate keeps the active version.
    let mut r3 = req("greet", &constant(r#"{"v":3}"#), &[]);
    r3.activate = false;
    let d3 = h.deploy(r3).await;
    assert_eq!(
        (d3.version.version, d3.function.active_version, d3.version.active),
        (3, 1, false)
    );
    assert_eq!(run(&h).await, json!({"v": 1}));

    // Survives a reload from the database (persisted).
    h.svc.reload().await.unwrap();
    assert_eq!(h.svc.get(&admin(), "greet").unwrap().active_version, 1);

    h.svc.delete(&admin(), "greet").await.unwrap();
    assert_eq!(
        h.svc
            .invoke("greet", Trigger::Direct, &json!({}), None)
            .await
            .unwrap_err()
            .code(),
        "function_not_found"
    );
}

#[tokio::test]
async fn secrets_are_sealed_at_rest_and_injected_only_with_the_capability() {
    let h = harness().await;
    let mut w = Wat::new(&["secret_get", "env_get"]);
    let name = w.s("API_TOKEN");
    let env = w.s("REGION");
    let body = format!(
        "(local.set $r (call $secret_get {name}))
         (drop (call $result_read (i32.const {res}) (local.get $r)))
         (local.set $q (call $env_get {env}))
         (drop (call $result_read (i32.const {aux}) (local.get $q)))
         (local.set $p (call $copy (i32.const {out}) (i32.const {res}) (local.get $r)))
         (local.set $p (call $copy (local.get $p) (i32.const {aux}) (local.get $q)))
         (call $emit (i32.const {out}) (i32.sub (local.get $p) (i32.const {out})))
         (i32.const 0)",
        res = common::RES,
        aux = common::AUX,
        out = common::OUT
    );
    let mut r = req("tok", &w.wasm(&body), &["secrets", "env"]);
    r.env.insert("REGION".into(), "-eu".into());
    h.deploy(r).await;
    h.svc
        .put_secret(&admin(), "tok", "API_TOKEN", "s3cr3t-value")
        .await
        .unwrap();
    let out = h.svc.invoke("tok", Trigger::Direct, &json!({}), None).await.unwrap();
    assert_eq!(out.output.as_deref(), Some(b"s3cr3t-value-eu".as_slice()));

    // At rest: sealed, not the plaintext; the API lists names only.
    let rows =
        h.db.fetch_all(&Statement::new(
            "SELECT sealed FROM _forgebase_function_secrets",
            vec![],
        ))
        .await
        .unwrap();
    let Value::Blob(sealed) = &rows.rows[0][0] else {
        panic!("sealed is a blob")
    };
    assert!(!sealed.windows(12).any(|w| w == b"s3cr3t-value"));
    let listed = serde_json::to_string(&h.svc.secrets(&admin(), "tok").await.unwrap()).unwrap();
    assert!(listed.contains("API_TOKEN") && !listed.contains("s3cr3t"));

    // A version without the capability cannot import secret_get at all, and
    // a module that only checks at run time gets capability_denied.
    let w = Wat::new(&["secret_get"]);
    let err = h
        .svc
        .deploy(&admin(), req("tok", &w.wasm("(i32.const 0)"), &[]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "capability_not_granted");

    h.svc.delete_secret(&admin(), "tok", "API_TOKEN").await.unwrap();
    assert_eq!(
        h.svc
            .delete_secret(&admin(), "tok", "API_TOKEN")
            .await
            .unwrap_err()
            .code(),
        "secret_not_found"
    );
}

#[tokio::test]
async fn database_access_goes_through_the_authorizer_as_the_function() {
    let h = harness().await;
    h.collection(json!({"name": "notes", "schema_mode": "flexible", "fields": [{"name": "title", "type": "text"}]}))
        .await;
    let mut w = Wat::new(&["db_create"]);
    let args = w.s(r#"{"collection":"notes","data":{"title":"from a function"}}"#);
    let body = format!(
        "(local.set $r (call $db_create {args}))
         (if (i32.lt_s (local.get $r) (i32.const 0)) (then (local.set $r (i32.sub (i32.const 0) (local.get $r)))))
         (drop (call $result_read (i32.const {res}) (local.get $r)))
         (call $emit (i32.const {res}) (local.get $r))
         (i32.const 0)",
        res = common::RES
    );
    h.deploy(req("writer", &w.wasm(&body), &["db.write"])).await;
    // Not granted yet: the policy engine denies the function's identity.
    let out = out_json(&h.svc.invoke("writer", Trigger::Direct, &json!({}), None).await.unwrap());
    assert_eq!(out["code"], "forbidden", "{out}");
    assert_eq!(
        h.data
            .list_records(&admin(), "notes", &Default::default())
            .await
            .unwrap()
            .items
            .len(),
        0
    );

    h.grant("writer", "data:notes:write").await;
    h.grant("writer", "data:notes:read").await;
    let out = out_json(&h.svc.invoke("writer", Trigger::Direct, &json!({}), None).await.unwrap());
    assert_eq!(out["title"], "from a function", "{out}");
    let id = out["id"].as_str().unwrap();
    let stored = h
        .data
        .get_record(&admin(), "notes", id, &Default::default())
        .await
        .unwrap();
    assert_eq!(stored["title"], "from a function");
    // The write is attributed to the function's identity in the outbox.
    let ev = forgebase_realtime::outbox::read_after(h.data.backend().as_ref(), 0, 10)
        .await
        .unwrap();
    assert_eq!(ev.last().unwrap().actor, "service:function.writer");
}
