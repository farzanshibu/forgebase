//! FIX-15: three replica read-path bugs FB-178 found (ADR-0029, "Addendum:
//! FB-178 measurements" and "FIX-15").
//!
//! PostgreSQL only (`FORGEBASE_TEST_BACKEND=postgres` with an engine in
//! `FORGEBASE_PG_ENGINE_DIR`), each on a real local replica added through
//! `POST /api/system/database/nodes`:
//!
//! 1. a replica added (and removed) while the server runs is used (and
//!    dropped) by the data core at once, with no restart.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use std::time::{Duration, Instant};

use security_common::*;
use serde_json::{Value as Json, json};

const BASE: &str = "/api/system/database";

fn postgres() -> bool {
    matches!(
        std::env::var("FORGEBASE_TEST_BACKEND").ok().as_deref().map(str::trim),
        Some("postgres" | "postgresql" | "pg")
    )
}

async fn wait_op(env: &Env, admin: &str, id: &str, timeout: Duration) -> Json {
    let start = Instant::now();
    loop {
        let r = env
            .as_(admin, "GET", &format!("{BASE}/operations/{id}"), &[], None)
            .await;
        assert_eq!(r.status, 200, "operation: {}", r.text());
        let j = r.json();
        if matches!(j["status"].as_str(), Some("succeeded" | "dead" | "cancelled")) {
            return j;
        }
        assert!(start.elapsed() < timeout, "operation {id} still {}: {j}", j["status"]);
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// Adds local replica `name` through the API, waits for the clone and for
/// the router to have it in rotation; returns its node id.
async fn add_replica(env: &Env, admin: &str, name: &str) -> String {
    let r = env
        .as_(
            admin,
            "POST",
            &format!("{BASE}/nodes"),
            &[],
            Some(json!({"name": name, "kind": "local"})),
        )
        .await;
    assert_eq!(r.status, 202, "add {name}: {}", r.text());
    let op = r.json()["operation"]["id"].as_str().unwrap().to_owned();
    let done = wait_op(env, admin, &op, Duration::from_secs(240)).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    wait_in_rotation(env, name).await;
    let t = env.as_(admin, "GET", &format!("{BASE}/topology"), &[], None).await;
    t.json()["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["name"] == name)
        .expect("the replica is listed")["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn wait_in_rotation(env: &Env, name: &str) {
    let start = Instant::now();
    loop {
        if let Some(rs) = env.server.router().control().replicas().cloned()
            && rs.status().iter().any(|s| s.name == name && s.in_rotation)
        {
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(120), "{name} never entered rotation");
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

async fn create_collection(env: &Env, admin: &str, name: &str) {
    let r = env
        .as_(
            admin,
            "POST",
            "/api/v1/collections",
            &[],
            Some(json!({"name": name, "fields": [{"name": "k", "type": "text"}]})),
        )
        .await;
    assert_eq!(r.status, 201, "collection {name}: {}", r.text());
}

/// `POST /api/v1/collections/{c}/records` as `admin`: the commit header.
async fn write(env: &Env, admin: &str, collection: &str, k: &str) -> Option<String> {
    let r = env
        .as_(
            admin,
            "POST",
            &format!("/api/v1/collections/{collection}/records"),
            &[],
            Some(json!({"k": k})),
        )
        .await;
    assert_eq!(r.status, 201, "write: {}", r.text());
    r.header("x-forgebase-commit").map(str::to_owned)
}

fn replica_reads(env: &Env) -> u64 {
    env.server
        .router()
        .control()
        .replicas()
        .map_or(0, |rs| rs.status().iter().map(|s| s.reads).sum())
}

#[tokio::test]
async fn a_replica_added_or_removed_while_the_server_runs_is_used_by_the_data_core_at_once() {
    if !postgres() {
        return;
    }
    let env = Env::new().await;
    let admin = env.admin("fix15-live@example.com").await;
    create_collection(&env, &admin, "notes").await;
    assert_eq!(write(&env, &admin, "notes", "a").await, None, "no replica: no token");

    // Added through the API: the next write carries a token, no restart.
    let r1 = add_replica(&env, &admin, "r1").await;
    let token = write(&env, &admin, "notes", "b").await.expect("X-ForgeBase-Commit");
    assert!(token.starts_with("control/"), "{token}");

    // A reader with no recent write of its own is served by the replica.
    let reader = env.admin("fix15-reader@example.com").await;
    let before = replica_reads(&env);
    let start = Instant::now();
    while replica_reads(&env) == before {
        let r = env
            .as_(&reader, "GET", "/api/v1/collections/notes/records", &[], None)
            .await;
        assert_eq!(r.status, 200, "{}", r.text());
        assert!(start.elapsed() < Duration::from_secs(30), "record reads never reached r1");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Removed through the API: dropped at once, writes carry no token again.
    let r = env
        .as_(&admin, "DELETE", &format!("{BASE}/nodes/{r1}?confirm=r1"), &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(env.server.router().control().replicas().is_none());
    assert_eq!(write(&env, &admin, "notes", "c").await, None, "r1 is gone");
    let r = env
        .as_(&reader, "GET", "/api/v1/collections/notes/records", &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["items"].as_array().map_or(0, Vec::len), 3);
    env.shutdown().await;
}
