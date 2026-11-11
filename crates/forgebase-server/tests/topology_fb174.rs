//! FB-174 through `/api/system/database/*` on a managed engine: a shard on
//! this server (local primary), a tenant moved to it and back by a
//! rebalance and a drain, the typed confirmations, the audit log, the
//! removal keeping the shard's data until it is deleted. SQLite: every
//! move route answers `400 postgres_required`. (MFA step-up for moves and
//! rebalances: `topology_fb175.rs`.)

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use std::time::{Duration, Instant};

use forgebase_data::AccessContext;
use forgebase_db::{Backend, Statement, Value};
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

async fn audit(env: &Env, admin: &str, event: &str) -> usize {
    let r = env
        .as_(admin, "GET", &format!("/api/auth/audit?event={event}"), &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    r.json()["items"].as_array().map_or(0, Vec::len)
}

async fn count(db: &dyn Backend, sql: &str) -> i64 {
    db.fetch_all(&Statement::raw(sql))
        .await
        .unwrap()
        .scalar()
        .and_then(Value::as_i64)
        .unwrap_or(-1)
}

#[tokio::test]
async fn sqlite_answers_postgres_required_for_moves_and_rebalancing() {
    if postgres() {
        return;
    }
    let env = Env::new().await;
    let admin = env.admin("fb174-sqlite@example.com").await;
    for (p, b) in [
        (
            format!("{BASE}/placements/t1/move"),
            json!({"to": "s1", "confirm": "t1"}),
        ),
        (format!("{BASE}/rebalance/plan"), json!({})),
        (format!("{BASE}/rebalance"), json!({"plan_id": "p", "confirm": "p"})),
    ] {
        let r = env.as_(&admin, "POST", &p, &[], Some(b)).await;
        assert_eq!(
            (r.status, r.code()),
            (400, "postgres_required".to_owned()),
            "{p}: {}",
            r.text()
        );
    }
    let r = env.as_(&admin, "GET", &format!("{BASE}/retired"), &[], None).await;
    assert_eq!((r.status, r.code()), (400, "postgres_required".to_owned()));
    env.shutdown().await;
}

#[tokio::test]
async fn a_local_shard_takes_a_tenant_by_move_rebalance_and_drain_through_the_api() {
    if !postgres() {
        return;
    }
    let env = Env::with(|c| {
        // Cleanups run as soon as the moves switch.
        c.database.topology.sharding.move_source_retention_hours = 0;
        // The shared test machine can be slow; this test is about the API
        // (the engine tests keep the 10 s default).
        c.database.topology.sharding.move_freeze_timeout_ms = 60_000;
    })
    .await;
    let admin = env.admin("fb174-ops@example.com").await;
    let (user, _) = env.user("fb174-user@example.com").await;
    let r = env
        .as_(&user, "POST", &format!("{BASE}/rebalance/plan"), &[], Some(json!({})))
        .await;
    assert_eq!(r.status, 403, "administrators only");

    // A shard on this server.
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
    let done = wait_op(&env, &admin, &op, Duration::from_secs(240)).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    let router = env.server.router().clone();
    let s1 = router
        .topology()
        .sets
        .into_iter()
        .find(|s| s.name == "s1")
        .expect("set s1");
    assert!(router.set(&s1.id).is_some(), "the router opened the local shard");
    let node = router.topology().primary_of(&s1.id).cloned().unwrap();
    assert_eq!(node.kind, forgebase_db::topology::NodeKind::Local);

    // A tenant with records on control.
    let data = env.server.realtime().data().clone();
    data.create_collection(
        &AccessContext::system(),
        serde_json::from_value(json!({"name": "notes", "options": {"tenant_scoped": true},
            "fields": [{"name": "body", "type": "text"}]}))
        .unwrap(),
    )
    .await
    .unwrap();
    router
        .place("t1", "control", forgebase_db::topology::PlacementState::Active)
        .await
        .unwrap();
    let t1 = AccessContext::admin("root").with_tenant("t1");
    for i in 0..30 {
        data.create_record(&t1, "notes", json!({ "body": format!("n{i}") }))
            .await
            .unwrap();
    }

    // Move: the typed tenant id, then 202 and the operation.
    let r = env
        .as_(
            &admin,
            "POST",
            &format!("{BASE}/placements/t1/move"),
            &[],
            Some(json!({"to": s1.id, "confirm": "t2"})),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (400, "confirm_mismatch".to_owned()),
        "{}",
        r.text()
    );
    let r = env
        .as_(
            &admin,
            "POST",
            &format!("{BASE}/placements/t1/move"),
            &[],
            Some(json!({"to": "no-such-set", "confirm": "t1"})),
        )
        .await;
    assert_eq!(
        (r.status, r.code()),
        (404, "shard_not_found".to_owned()),
        "{}",
        r.text()
    );
    let r = env
        .as_(
            &admin,
            "POST",
            &format!("{BASE}/placements/t1/move"),
            &[],
            Some(json!({"to": s1.id, "confirm": "t1"})),
        )
        .await;
    assert_eq!(r.status, 202, "{}", r.text());
    assert_eq!(r.json()["operation"]["kind"], "database.tenant.move");
    let op = r.json()["operation"]["id"].as_str().unwrap().to_owned();
    let done = wait_op(&env, &admin, &op, Duration::from_secs(180)).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert!(done["result"]["freeze_ms"].as_u64().is_some(), "{done}");
    eprintln!("FREEZE api move freeze_ms={}", done["result"]["freeze_ms"]);
    assert_eq!(router.placement("t1").unwrap().set_id, s1.id);
    let shard = router.set(&s1.id).unwrap();
    assert_eq!(
        count(&shard, "SELECT count(*) FROM notes WHERE _tenant = 't1'").await,
        30
    );
    data.create_record(&t1, "notes", json!({"body": "on the shard"}))
        .await
        .unwrap();
    assert_eq!(
        count(&shard, "SELECT count(*) FROM notes WHERE _tenant = 't1'").await,
        31
    );
    let r = env
        .as_(&admin, "GET", &format!("{BASE}/placements?set={}", s1.id), &[], None)
        .await;
    assert_eq!(r.status, 200);
    assert!(r.text().contains("\"t1\""), "{}", r.text());
    assert!(
        audit(&env, &admin, "db.topology.tenant.move").await >= 1,
        "the move is audited"
    );
    assert!(
        audit(&env, &admin, "db.topology.refused").await >= 2,
        "the refusals are audited"
    );

    // Rebalance: a plan (t1 alone on s1, control empty of tenants: one move back
    // would not narrow anything with one tenant, so ask for t2 too).
    router
        .place("t2", &s1.id, forgebase_db::topology::PlacementState::Active)
        .await
        .unwrap();
    let r = env
        .as_(
            &admin,
            "POST",
            &format!("{BASE}/rebalance/plan"),
            &[],
            Some(json!({"max_moves": 0})),
        )
        .await;
    assert_eq!((r.status, r.code()), (400, "invalid_request".to_owned()));
    let r = env
        .as_(&admin, "POST", &format!("{BASE}/rebalance/plan"), &[], Some(json!({})))
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let plan = r.json();
    let plan_id = plan["id"].as_str().unwrap().to_owned();
    assert!(plan["expires_at"].as_str().is_some_and(|s| !s.is_empty()), "{plan}");
    assert!(plan["before"].as_array().unwrap().len() >= 2, "{plan}");
    let r = env
        .as_(
            &admin,
            "POST",
            &format!("{BASE}/rebalance"),
            &[],
            Some(json!({"plan_id": plan_id, "confirm": "nope"})),
        )
        .await;
    assert_eq!((r.status, r.code()), (400, "confirm_mismatch".to_owned()));
    let r = env
        .as_(
            &admin,
            "POST",
            &format!("{BASE}/rebalance"),
            &[],
            Some(json!({"plan_id": plan_id, "confirm": plan_id})),
        )
        .await;
    assert_eq!(r.status, 202, "{}", r.text());
    assert_eq!(r.json()["operation"]["kind"], "database.rebalance");
    let op = r.json()["operation"]["id"].as_str().unwrap().to_owned();
    let done = wait_op(&env, &admin, &op, Duration::from_secs(180)).await;
    assert_eq!(done["status"], "succeeded", "{done}");
    assert!(audit(&env, &admin, "db.topology.rebalance").await >= 1);
    assert!(audit(&env, &admin, "db.topology.rebalance.plan").await >= 1);

    // Drain: every tenant off s1, then removal; the data stays on disk.
    let r = env
        .as_(
            &admin,
            "PATCH",
            &format!("{BASE}/shards/{}", s1.id),
            &[],
            Some(json!({"state": "draining"})),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = env
        .as_(&admin, "GET", &format!("{BASE}/operations?active=1"), &[], None)
        .await;
    let drain = r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["kind"] == "database.shard.drain")
        .cloned();
    if let Some(d) = drain {
        let done = wait_op(&env, &admin, d["id"].as_str().unwrap(), Duration::from_secs(240)).await;
        assert_eq!(done["status"], "succeeded", "{done}");
    }
    let deadline = Instant::now() + Duration::from_secs(120);
    while router.placements().iter().any(|p| p.set_id == s1.id) {
        assert!(
            Instant::now() < deadline,
            "s1 still holds tenants: {:?}",
            router.placements()
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    // Cleanups (retention 0) let the shard go.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let r = env
            .as_(
                &admin,
                "DELETE",
                &format!("{BASE}/shards/{}?confirm=s1", s1.id),
                &[],
                None,
            )
            .await;
        if r.status == 200 {
            break;
        }
        assert!(
            r.code() == "operation_in_progress" && Instant::now() < deadline,
            "{} {}",
            r.status,
            r.text()
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(router.set(&s1.id).is_none());
    assert_eq!(router.placement("t1").unwrap().set_id, "control");
    assert_eq!(
        count(env.server.database(), "SELECT count(*) FROM notes WHERE _tenant = 't1'").await,
        31
    );
    let r = env.as_(&admin, "GET", &format!("{BASE}/retired"), &[], None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let items = r.json()["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0]["name"], "s1p");
    let id = items[0]["id"].as_str().unwrap().to_owned();
    let r = env
        .as_(
            &admin,
            "DELETE",
            &format!("{BASE}/retired/{id}?confirm=nope"),
            &[],
            None,
        )
        .await;
    assert_eq!((r.status, r.code()), (400, "confirm_mismatch".to_owned()));
    let r = env
        .as_(&admin, "DELETE", &format!("{BASE}/retired/{id}?confirm=s1p"), &[], None)
        .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = env.as_(&admin, "GET", &format!("{BASE}/retired"), &[], None).await;
    assert!(r.json()["items"].as_array().unwrap().is_empty());
    assert!(audit(&env, &admin, "db.topology.retired.delete").await >= 1);
    env.shutdown().await;
}
