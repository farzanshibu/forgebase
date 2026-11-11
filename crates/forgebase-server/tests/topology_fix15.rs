//! FIX-15: three replica read-path bugs FB-178 found (ADR-0029, "Addendum:
//! FB-178 measurements" and "FIX-15").
//!
//! PostgreSQL only (`FORGEBASE_TEST_BACKEND=postgres` with an engine in
//! `FORGEBASE_PG_ENGINE_DIR`), each on a real local replica added through
//! `POST /api/system/database/nodes`:
//!
//! 1. a replica added (and removed) while the server runs is used (and
//!    dropped) by the data core at once, with no restart;
//! 2. on a sharded server, a record write on control answers
//!    `X-ForgeBase-Commit`, before and after a restart;
//! 3. an anonymous sign-in right after a signup finds the user while the
//!    replica has not replayed it (identity reads run on the primary).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forgebase_db::{ReadIntent, Statement};
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

/// An [`Env`] around a server started again on the same directory.
async fn restart(env: Env, cfg: &forgebase_core::Config) -> Env {
    let Env { server, auth, dir, .. } = env;
    drop(auth);
    server.shutdown().await.unwrap();
    let server = forgebase_server::start(cfg).await.unwrap();
    let mut acfg = forgebase_auth::AuthConfig::for_data_dir(&cfg.data_dir);
    acfg.argon2 = forgebase_auth::Argon2Config::insecure_fast_for_tests();
    let auth = forgebase_auth::AuthService::open_with_data(
        Arc::new(server.database().clone()),
        Arc::new(forgebase_data::DefaultAuthorizer),
        &cfg.data_dir,
        acfg,
        None,
    )
    .await
    .unwrap();
    let addr = server.local_addr();
    Env {
        server,
        auth,
        addr,
        dir,
    }
}

/// Runs `sql` with the engine's `psql` on the local node whose data
/// directory is `node`, as the engine's superuser over its socket.
fn psql(data_dir: &Path, node: &Path, sql: &str) {
    let pid = std::fs::read_to_string(node.join("postmaster.pid")).expect("the node runs");
    let mut l = pid.lines();
    let port = l.nth(3).unwrap().trim().to_owned();
    let sock = l.next().unwrap().trim().to_owned();
    let engine = forgebase_db::shard_testing::engine_dir().expect("an engine");
    let secrets = forgebase_pgengine::RoleSecrets::load(&data_dir.join("postgres").join("roles.secret"))
        .unwrap()
        .expect("roles.secret");
    let password = secrets.password(forgebase_pgengine::Role::Super).unwrap();
    let out = std::process::Command::new(engine.join("bin").join("psql"))
        .env("PGPASSWORD", password)
        .args(["-X", "-A", "-t", "-q", "-v", "ON_ERROR_STOP=1", "-w"])
        .args(["-U", "forgebase_super", "-d", "postgres", "-h", &sock, "-p", &port, "-c", sql])
        .output()
        .unwrap();
    assert!(out.status.success(), "psql: {}", String::from_utf8_lossy(&out.stderr));
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

#[tokio::test]
async fn control_set_writes_on_a_sharded_server_return_the_commit_token() {
    if !postgres() {
        return;
    }
    let env = Env::new().await;
    let admin = env.admin("fix15-sharded@example.com").await;
    add_replica(&env, &admin, "r1").await;
    let r = env
        .as_(
            &admin,
            "POST",
            &format!("{BASE}/shards"),
            &[],
            Some(json!({"name": "s1", "node": {"name": "s1p", "kind": "local"}})),
        )
        .await;
    assert_eq!(r.status, 202, "{}", r.text());
    let op = r.json()["operation"]["id"].as_str().unwrap().to_owned();
    let done = wait_op(&env, &admin, &op, Duration::from_secs(300)).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert!(env.server.router().is_sharded());

    // A shared collection lives on control.
    create_collection(&env, &admin, "settings").await;
    let token = write(&env, &admin, "settings", "a").await.expect("X-ForgeBase-Commit");
    assert!(token.starts_with("control/"), "{token}");

    // And after a restart.
    let cfg = security_common::config(env.dir.path());
    let env = restart(env, &cfg).await;
    let r = env
        .req(
            "POST",
            "/api/auth/admins/login",
            &[],
            Some(json!({"email": "fix15-sharded@example.com", "password": ADMIN_PASSWORD})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let admin = r.json()["access_token"].as_str().unwrap().to_owned();
    assert!(env.server.router().is_sharded());
    wait_in_rotation(&env, "r1").await;
    let token = write(&env, &admin, "settings", "b").await.expect("X-ForgeBase-Commit after a restart");
    assert!(token.starts_with("control/"), "{token}");
    env.shutdown().await;
}

#[tokio::test]
async fn a_sign_in_right_after_a_signup_finds_the_user_on_a_lagging_replica() {
    if !postgres() {
        return;
    }
    // Keep the paused replica in rotation: it must be the one asked.
    let env = Env::with(|c| {
        c.database.topology.replicas.max_lag_ms = 600_000;
        c.database.topology.replicas.max_lag_bytes = 1 << 40;
    })
    .await;
    let admin = env.admin("fix15-auth@example.com").await;
    add_replica(&env, &admin, "r1").await;
    let cfg = security_common::config(env.dir.path());
    let node = cfg.data_dir.join("postgres").join("nodes").join("r1").join("data");
    psql(&cfg.data_dir, &node, "SELECT pg_wal_replay_pause()");

    let email = "fix15-new-user@example.com";
    env.auth
        .signup(&forgebase_auth::ClientInfo::default(), None, email, USER_PASSWORD, None)
        .await
        .unwrap();
    // The replica has not replayed the signup...
    let replicas = env.server.router().control().replicas().cloned().expect("r1");
    let rows = replicas
        .fetch(
            &Statement::raw(format!(
                "SELECT CAST(pg_is_in_recovery() AS INT), (SELECT count(*) FROM users WHERE email = '{email}')"
            )),
            &ReadIntent::replica("fix15 test"),
        )
        .await
        .unwrap();
    assert_eq!(
        (rows.rows[0][0].as_i64(), rows.rows[0][1].as_i64()),
        (Some(1), Some(0)),
        "r1 is behind the signup"
    );
    // ...and an anonymous sign-in still finds the user at once.
    let r = env
        .req(
            "POST",
            "/api/auth/login",
            &[],
            Some(json!({"email": email, "password": USER_PASSWORD})),
        )
        .await;
    assert_eq!(r.status, 200, "login: {}", r.text());
    assert_eq!(r.json()["user"]["email"], email, "{}", r.text());
    let token = r.json()["access_token"].as_str().unwrap().to_owned();
    let me = env.as_(&token, "GET", "/api/auth/me", &[], None).await;
    assert_eq!(me.status, 200, "me: {}", me.text());
    psql(&cfg.data_dir, &node, "SELECT pg_wal_replay_resume()");
    env.shutdown().await;
}
