//! FB-175: the database topology API on a real server.
//!
//! PostgreSQL (`FORGEBASE_TEST_BACKEND=postgres` with an engine in
//! `FORGEBASE_PG_ENGINE_DIR`): a local replica added through
//! `POST /api/system/database/nodes` is cloned by the `database.node.add`
//! job, opened by the router and serves replica reads; one operation per
//! set; removal with the typed name; promote through the API, a restart
//! onto the new primary (`topology.json`) with every commit kept, and the
//! old primary rejoined with `…/rebuild`; audit entries; MFA step-up.
//! SQLite: `postgres_required`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use std::time::{Duration, Instant};

use std::sync::Arc;

use forgebase_db::{Backend, ReadIntent, Statement};
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

async fn topology(env: &Env, admin: &str) -> Json {
    let r = env.as_(admin, "GET", &format!("{BASE}/topology"), &[], None).await;
    assert_eq!(r.status, 200, "topology: {}", r.text());
    r.json()
}

fn node<'a>(topo: &'a Json, name: &str) -> Option<&'a Json> {
    topo["nodes"].as_array().unwrap().iter().find(|n| n["name"] == name)
}

async fn audit(env: &Env, admin: &str, event: &str) -> usize {
    let r = env
        .as_(admin, "GET", &format!("/api/auth/audit?event={event}"), &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["items"].as_array().map_or(0, Vec::len)
}

/// An [`Env`] around a server started again on the same directory.
async fn adopt(server: forgebase_server::RunningServer, dir: tempfile::TempDir, cfg: &forgebase_core::Config) -> Env {
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

async fn sql(env: &Env, stmt: &str) {
    let mut tx = env.server.database().begin(forgebase_db::TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(stmt)).await.unwrap();
    tx.commit().await.unwrap();
}

async fn meta(db: &forgebase_db::Database, key: &str) -> Option<String> {
    db.meta_get(key).await.unwrap()
}

#[tokio::test]
async fn sqlite_shows_one_server_and_answers_postgres_required() {
    if postgres() {
        return;
    }
    let env = Env::new().await;
    let admin = env.admin("fb175-sqlite@example.com").await;
    let t = topology(&env, &admin).await;
    assert_eq!(t["backend"], "sqlite");
    let r = env
        .as_(
            admin.as_str(),
            "POST",
            &format!("{BASE}/nodes"),
            &[],
            Some(json!({"name": "r1", "kind": "local"})),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (400, "postgres_required".to_owned()),
        "{}",
        r.text()
    );
    let (user, _) = env.user("fb175-sqlite-user@example.com").await;
    let r = env.as_(&user, "GET", &format!("{BASE}/topology"), &[], None).await;
    assert_eq!(r.status, 403);
    env.shutdown().await;
}

#[tokio::test]
async fn a_local_replica_added_through_the_api_serves_reads_is_promoted_and_the_old_primary_rejoins() {
    if !postgres() {
        return;
    }
    let env = Env::new().await;
    let admin = env.admin("fb175-ops@example.com").await;
    let db = env.server.database().clone();
    db.meta_set("fb175_before_replica", "1").await.unwrap();

    // Join tokens need the primary on TCP ([database.postgres] listen).
    let r = env
        .as_(&admin, "POST", &format!("{BASE}/join-tokens"), &[], Some(json!({})))
        .await;
    assert_eq!(
        (r.status, r.code()),
        (400, "node_unsupported".to_owned()),
        "{}",
        r.text()
    );

    // Add r1: a job clones and starts it; the router opens it.
    let r = env
        .as_(
            &admin,
            "POST",
            &format!("{BASE}/nodes"),
            &[],
            Some(json!({"name": "r1", "kind": "local"})),
        )
        .await;
    assert_eq!(r.status, 202, "{}", r.text());
    let op = r.json()["operation"].clone();
    assert_eq!(op["kind"], "database.node.add");
    let done = wait_op(&env, &admin, op["id"].as_str().unwrap(), Duration::from_secs(180)).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert_eq!(done["progress"]["percent"], 100.0, "{done}");
    let t = topology(&env, &admin).await;
    let r1 = node(&t, "r1").expect("r1 listed").clone();
    let r1_id = r1["id"].as_str().unwrap().to_owned();
    assert_eq!(
        (r1["kind"].as_str(), r1["role"].as_str()),
        (Some("local"), Some("replica"))
    );
    assert!(!t.to_string().contains("url_sealed"), "no URLs in the topology");

    // Reads route to it once the health monitor has it in rotation.
    let start = Instant::now();
    let replicas = loop {
        if let Some(rs) = env.server.router().control().replicas().cloned()
            && rs.status().iter().any(|s| s.name == "r1" && s.in_rotation)
        {
            break rs;
        }
        assert!(start.elapsed() < Duration::from_secs(90), "r1 never entered rotation");
        tokio::time::sleep(Duration::from_millis(300)).await;
    };
    let rows = replicas
        .fetch(
            &Statement::raw("SELECT CAST(pg_is_in_recovery() AS INT), (SELECT value FROM _forgebase_meta WHERE key = 'fb175_before_replica')"),
            &ReadIntent::replica("fb175 test"),
        )
        .await
        .unwrap();
    assert_eq!(rows.rows[0][0].as_i64(), Some(1), "the read ran on the standby");
    assert_eq!(rows.rows[0][1].as_str(), Some("1"));
    assert!(replicas.status().iter().any(|s| s.name == "r1" && s.reads > 0));
    let t = topology(&env, &admin).await;
    assert_eq!(node(&t, "r1").unwrap()["in_rotation"], true, "{t}");
    let r = env
        .as_(&admin, "GET", &format!("{BASE}/nodes/{r1_id}"), &[], None)
        .await;
    assert_eq!(r.status, 200);
    assert!(r.json()["lag_history"].is_array());

    // One topology change per set: r2's clone holds control.
    let r = env
        .as_(
            &admin,
            "POST",
            &format!("{BASE}/nodes"),
            &[],
            Some(json!({"name": "r2", "kind": "local"})),
        )
        .await;
    assert_eq!(r.status, 202, "{}", r.text());
    let r2_op = r.json()["operation"]["id"].as_str().unwrap().to_owned();
    let r = env
        .as_(&admin, "POST", &format!("{BASE}/nodes/{r1_id}/rebuild"), &[], None)
        .await;
    assert_eq!(
        (r.status, r.code()),
        (409, "operation_in_progress".to_owned()),
        "{}",
        r.text()
    );
    let done = wait_op(&env, &admin, &r2_op, Duration::from_secs(180)).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    let r2_id = node(&topology(&env, &admin).await, "r2").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = env
        .as_(&admin, "DELETE", &format!("{BASE}/nodes/{r2_id}?confirm=r1"), &[], None)
        .await;
    assert_eq!(
        (r.status, r.code()),
        (400, "confirm_mismatch".to_owned()),
        "{}",
        r.text()
    );
    let r = env
        .as_(&admin, "DELETE", &format!("{BASE}/nodes/{r2_id}?confirm=r2"), &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(node(&topology(&env, &admin).await, "r2").is_none());
    let slots = db
        .fetch_all(&Statement::raw("SELECT slot_name FROM pg_replication_slots ORDER BY 1"))
        .await
        .unwrap();
    let slots: Vec<&str> = slots.rows.iter().filter_map(|r| r[0].as_str()).collect();
    assert_eq!(slots, ["fbr_r1"], "r2's slot is dropped");

    // Promote r1: the check first, then the operation.
    let r = env
        .as_(&admin, "GET", &format!("{BASE}/nodes/{r1_id}/promote-check"), &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["primary_reachable"], true);
    let r = env
        .as_(
            &admin,
            "POST",
            &format!("{BASE}/nodes/{r1_id}/promote"),
            &[],
            Some(json!({"confirm": "R1"})),
        )
        .await;
    assert_eq!((r.status, r.code()), (400, "confirm_mismatch".to_owned()));
    db.meta_set("fb175_before_promote", "2").await.unwrap();
    let r = env
        .as_(
            &admin,
            "POST",
            &format!("{BASE}/nodes/{r1_id}/promote"),
            &[],
            Some(json!({"confirm": "r1"})),
        )
        .await;
    assert_eq!(r.status, 202, "{}", r.text());
    let promote_op = r.json()["operation"]["id"].as_str().unwrap().to_owned();
    let cfg = security_common::config(env.dir.path());
    let file = cfg.data_dir.join(forgebase_db::topology::TOPOLOGY_FILE);
    let start = Instant::now();
    loop {
        if let Ok(Some(f)) = forgebase_db::topology::read_topology_file(&file)
            && f.topology
                .nodes
                .iter()
                .any(|n| n.name == "r1" && n.role == forgebase_db::topology::NodeRole::Primary)
        {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "r1 never became primary in topology.json"
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    // Restart: the server comes up on r1 (topology.json) with every commit.
    let Env { server, auth, dir, .. } = env;
    drop(auth);
    drop(replicas);
    drop(db);
    server.shutdown().await.unwrap();
    let server = forgebase_server::start(&cfg).await.unwrap();
    let env = adopt(server, dir, &cfg).await;
    let db = env.server.database().clone();
    assert!(
        env.server
            .engine()
            .unwrap()
            .layout()
            .pgdata()
            .ends_with("postgres/nodes/r1/data"),
        "control's primary is r1 now"
    );
    assert_eq!(meta(&db, "fb175_before_replica").await.as_deref(), Some("1"));
    assert_eq!(
        meta(&db, "fb175_before_promote").await.as_deref(),
        Some("2"),
        "no commit lost"
    );
    db.meta_set("fb175_after_promote", "3").await.unwrap();
    let t = topology(&env, &admin).await;
    let r1 = node(&t, "r1").unwrap();
    assert_eq!(
        (r1["role"].as_str(), r1["state"].as_str()),
        (Some("primary"), Some("ready")),
        "{t}"
    );
    let old = node(&t, "primary").expect("the old primary is a node now").clone();
    assert_eq!(old["state"], "needs_rebuild", "{t}");
    assert!(t["restart_required"].is_null());
    // The promote finishes on this start (its checkpoint was replicated).
    let done = wait_op(&env, &admin, &promote_op, Duration::from_secs(180)).await;
    assert_eq!(done["status"], "succeeded", "{done}");

    // Rejoin the old primary (pg_rewind, or a fresh clone on engines without it).
    let old_id = old["id"].as_str().unwrap().to_owned();
    let r = env
        .as_(&admin, "POST", &format!("{BASE}/nodes/{old_id}/rebuild"), &[], None)
        .await;
    assert_eq!(r.status, 202, "{}", r.text());
    let done = wait_op(
        &env,
        &admin,
        r.json()["operation"]["id"].as_str().unwrap(),
        Duration::from_secs(180),
    )
    .await;
    assert_eq!(done["status"], "succeeded", "{done}");
    let t = topology(&env, &admin).await;
    let old = node(&t, "primary").unwrap();
    assert!(
        matches!(old["state"].as_str(), Some("catching_up" | "streaming")),
        "{t}"
    );

    // Every change is in the audit log.
    for e in [
        "db.topology.node.add",
        "db.topology.node.remove",
        "db.topology.node.promote",
        "db.topology.refused",
    ] {
        assert!(audit(&env, &admin, e).await >= 1, "{e} audited");
    }
    env.shutdown().await;
}

#[tokio::test]
async fn removing_and_promoting_need_a_recent_second_factor() {
    if !postgres() {
        return;
    }
    let env = Env::new().await;
    let email = "fb175-mfa@example.com";
    let admin = env.admin(email).await;
    // Without MFA the step-up is not asked for (an unknown node: 404).
    let r = env
        .as_(&admin, "DELETE", &format!("{BASE}/nodes/n-nope?confirm=x"), &[], None)
        .await;
    assert_eq!((r.status, r.code()), (404, "node_not_found".to_owned()), "{}", r.text());

    // Turn TOTP on.
    let r = env
        .as_(&admin, "POST", "/api/auth/mfa/totp/setup", &[], Some(json!({})))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let secret = forgebase_auth::crypto::base32_decode(r.json()["secret"].as_str().unwrap()).unwrap();
    let step = |d: u64| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        forgebase_auth::crypto::hotp(&secret, now / 30 + d)
    };
    let r = env
        .as_(
            &admin,
            "POST",
            "/api/auth/mfa/totp/confirm",
            &[],
            Some(json!({"code": step(0)})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());

    // Confirming proves the factor: this recent session may run them now.
    let r = env
        .as_(&admin, "DELETE", &format!("{BASE}/nodes/n-nope?confirm=x"), &[], None)
        .await;
    assert_eq!((r.status, r.code()), (404, "node_not_found".to_owned()), "{}", r.text());
    // A session that did not pass the second factor cannot remove or promote.
    sql(&env, "UPDATE _forgebase_auth_sessions SET mfa = 0").await;
    for (m, p, b) in [
        ("DELETE", format!("{BASE}/nodes/n-nope?confirm=x"), None),
        (
            "POST",
            format!("{BASE}/nodes/n-nope/promote"),
            Some(json!({"confirm": "x"})),
        ),
        ("DELETE", format!("{BASE}/shards/s-nope?confirm=x"), None),
        (
            "POST",
            format!("{BASE}/placements/t1/move"),
            Some(json!({"to": "s2", "confirm": "t1"})),
        ),
        (
            "POST",
            format!("{BASE}/rebalance"),
            Some(json!({"plan_id": "p", "confirm": "p"})),
        ),
    ] {
        let r = env.as_(&admin, m, &p, &[], b).await;
        assert_eq!(
            (r.status, r.code()),
            (403, "mfa_step_up_required".to_owned()),
            "{m} {p}: {}",
            r.text()
        );
    }
    // A sign-in that passed the second factor can.
    let r = env
        .req(
            "POST",
            "/api/auth/admins/login",
            &[],
            Some(json!({"email": email, "password": ADMIN_PASSWORD})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let mfa_token = r.json()["mfa_token"].as_str().expect("mfa required").to_owned();
    let r = env
        .req(
            "POST",
            "/api/auth/mfa/verify",
            &[],
            Some(json!({"mfa_token": mfa_token, "code": step(1)})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let fresh = r.json()["access_token"].as_str().unwrap().to_owned();
    let r = env
        .as_(&fresh, "DELETE", &format!("{BASE}/nodes/n-nope?confirm=x"), &[], None)
        .await;
    assert_eq!((r.status, r.code()), (404, "node_not_found".to_owned()), "{}", r.text());
    // Ten minutes later it has to step up again.
    sql(
        &env,
        "UPDATE _forgebase_auth_sessions SET created_at = created_at - 3600",
    )
    .await;
    let r = env
        .as_(&fresh, "DELETE", &format!("{BASE}/nodes/n-nope?confirm=x"), &[], None)
        .await;
    assert_eq!(
        (r.status, r.code()),
        (403, "mfa_step_up_required".to_owned()),
        "{}",
        r.text()
    );
    assert!(audit(&env, &fresh, "db.topology.refused").await >= 1);
    env.shutdown().await;
}
