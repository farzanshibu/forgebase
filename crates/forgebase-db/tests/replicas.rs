//! Read replicas and read-your-writes (ADR-0029 §2.4–§2.7, FB-167).
//!
//! The replica tests build a real streaming standby
//! (`forgebase_db::routing::testing::ReplicaCluster`) from the engine in
//! `FORGEBASE_TEST_PG_ENGINE` / `FORGEBASE_PG_ENGINE_DIR` and are skipped
//! without one. `routed_reads_without_replicas_use_the_primary` runs on the
//! backend `FORGEBASE_TEST_BACKEND` selects.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use forgebase_core::ErrorCode;
use forgebase_db::routing::scope::{self, RequestScope};
use forgebase_db::routing::testing::{ReplicaCluster, open_read_only};
use forgebase_db::routing::{
    Consistency, ReadAfter, ReadIntent, ReadRouting, ReplicaCheckError, ReplicaNode, ReplicaOptions, ReplicaSet,
    check_replica, primary_facts,
};
use forgebase_db::topology::{self, DbNode, DbSet, NewNode, NodeKind, NodeOrigin, NodeRole, NodeState};
use forgebase_db::{
    Backend, BoxFuture, Database, DbError, DbRouter, RouterOptions, SetConnector, SetHandle, Statement, TxMode, Value,
    testing,
};

/// Which server answered, and whether it sees row `id`.
const SERVED: &str = "SELECT CASE WHEN pg_is_in_recovery() THEN 1 ELSE 0 END AS standby, \
                      (SELECT count(*) FROM rt WHERE id = 1) AS n";

async fn cluster(settings: &[(&str, &str)]) -> Option<ReplicaCluster> {
    match ReplicaCluster::start(settings).await {
        None => {
            eprintln!("skipped: no PostgreSQL engine configured");
            None
        }
        Some(Ok(c)) => Some(c),
        Some(Err(e)) => panic!("replica cluster: {e}"),
    }
}

fn node(name: &str) -> DbNode {
    DbNode {
        id: format!("n-{name}"),
        set_id: "control".into(),
        name: name.into(),
        kind: NodeKind::External,
        role: NodeRole::Replica,
        url_sealed: None,
        state: NodeState::Ready,
        weight: 1,
        engine_version: None,
        system_identifier: None,
        timeline: None,
        origin: NodeOrigin::Api,
        created_at: String::new(),
        updated_at: String::new(),
    }
}

/// No background monitor: the tests sample with `refresh`.
fn manual(options: ReplicaOptions) -> ReplicaOptions {
    ReplicaOptions {
        health_interval: Duration::ZERO,
        ..options
    }
}

async fn exec(db: &dyn Backend, sql: &str) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(sql)).await.unwrap();
    tx.commit().await.unwrap();
}

/// A cluster with table `rt`, and a control handle routing over its standby.
async fn setup(settings: &[(&str, &str)], options: ReplicaOptions) -> Option<(ReplicaCluster, ReplicaSet, SetHandle)> {
    let c = cluster(settings).await?;
    let primary = c.open_primary().await.unwrap();
    exec(&primary, "CREATE TABLE rt (id bigint PRIMARY KEY, body text)").await;
    c.wait_caught_up().await.unwrap();
    let standby = c.open_standby().await.unwrap();
    let replicas = ReplicaSet::open(
        "control",
        primary.clone(),
        vec![ReplicaNode {
            node: node("r1"),
            db: standby,
        }],
        manual(options),
        None,
    )
    .await;
    let handle = SetHandle::new(DbSet::control(), primary).with_replicas(replicas.clone());
    Some((c, replicas, handle))
}

/// `(standby, rows with id 1)` of a routed read.
async fn served(handle: &SetHandle, intent: &ReadIntent) -> (i64, i64) {
    let rows = handle
        .fetch_routed(&Statement::new(SERVED, Vec::new()), intent)
        .await
        .unwrap();
    let row = &rows.rows[0];
    (row[0].as_i64().unwrap(), row[1].as_i64().unwrap())
}

fn replica() -> ReadIntent {
    ReadIntent::replica("test.read")
}

#[tokio::test]
async fn routed_reads_without_replicas_use_the_primary() {
    let db = testing::test_backend().await;
    let stmt = Statement::new("SELECT 1 AS one", Vec::new());
    let direct = db.database().fetch_all(&stmt).await.unwrap();
    let routed = db.database().fetch_routed(&stmt, &replica()).await.unwrap();
    assert_eq!(direct, routed);
    let handle = SetHandle::new(DbSet::control(), db.database().clone());
    assert!(handle.replicas().is_none());
    let scope = RequestScope::new(Consistency::Session, ReadAfter::new());
    scope::scoped(scope.clone(), async {
        assert_eq!(handle.fetch_routed(&stmt, &replica()).await.unwrap(), direct);
        exec(&handle, "CREATE TABLE IF NOT EXISTS rt_none (id integer)").await;
    })
    .await;
    // No replicas: commits record no token, so responses stay unchanged.
    assert!(scope.commits().is_empty());
    // A router over one set opens no replicas.
    let router = DbRouter::single(db.database().clone());
    assert!(router.control().replicas().is_none());
}

#[tokio::test]
async fn lagging_replicas_leave_rotation_and_return_after_three_good_samples() {
    let options = ReplicaOptions {
        max_lag_bytes: 256 * 1024,
        max_lag: Duration::from_secs(3_600),
        ..ReplicaOptions::default()
    };
    let Some((c, replicas, handle)) = setup(&[], options).await else {
        return;
    };
    assert_eq!(replicas.status()[0].state, NodeState::Streaming);
    assert_eq!(served(&handle, &replica()).await.0, 1, "a healthy replica serves");
    assert_eq!(served(&handle, &ReadIntent::primary("auth")).await.0, 0);

    c.pause_replay().await.unwrap();
    exec(
        &handle,
        "INSERT INTO rt SELECT g, repeat('x', 1000) FROM generate_series(1, 3000) g",
    )
    .await;
    replicas.refresh().await;
    let status = &replicas.status()[0];
    assert_eq!(status.state, NodeState::Lagging, "{status:?}");
    assert!(!status.in_rotation);
    assert!(status.lag_bytes > 256 * 1024);
    assert_eq!(
        served(&handle, &replica()).await.0,
        0,
        "a lagging replica is out of rotation"
    );

    c.resume_replay().await.unwrap();
    c.wait_caught_up().await.unwrap();
    for _ in 0..2 {
        replicas.refresh().await;
        assert_eq!(replicas.status()[0].state, NodeState::Lagging, "hysteresis");
        assert_eq!(served(&handle, &replica()).await.0, 0);
    }
    replicas.refresh().await;
    assert_eq!(replicas.status()[0].state, NodeState::Streaming);
    assert_eq!(served(&handle, &replica()).await, (1, 1));
}

#[tokio::test]
async fn read_after_tokens_wait_for_the_replica_then_read_the_primary() {
    let options = ReplicaOptions {
        lsn_wait: Duration::from_millis(200),
        ..ReplicaOptions::default()
    };
    let Some((c, replicas, handle)) = setup(&[], options).await else {
        return;
    };
    c.pause_replay().await.unwrap();

    // A write inside a request scope records its token, and the request's
    // own later reads see the write.
    let write = RequestScope::new(Consistency::Session, ReadAfter::new());
    let own = scope::scoped(write.clone(), async {
        exec(&handle, "INSERT INTO rt (id, body) VALUES (1, 'one')").await;
        served(&handle, &replica()).await
    })
    .await;
    assert_eq!(own, (0, 1), "the request's own write is read from the primary");
    let commits = write.commits();
    let lsn = commits.get("control").expect("a commit token for control");
    assert!(lsn >= replicas.status()[0].replay_lsn.unwrap());
    let header = commits.to_header().unwrap();
    assert!(header.starts_with("control/"), "{header}");

    // Without the token the (stale) replica serves.
    assert_eq!(served(&handle, &replica()).await, (1, 0));
    // A token for another set does not hold this one back.
    let other = ReadAfter::parse_header("s9/FF/0");
    assert_eq!(served(&handle, &replica().after_all(&other)).await, (1, 0));

    // With the token: wait up to lsn_wait, then the primary.
    let started = Instant::now();
    assert_eq!(served(&handle, &replica().after_all(&commits)).await, (0, 1));
    assert!(
        started.elapsed() >= Duration::from_millis(200),
        "{:?}",
        started.elapsed()
    );
    // The same through the request scope (X-ForgeBase-Read-After).
    let later = RequestScope::new(Consistency::Session, ReadAfter::parse_header(&header));
    assert_eq!(scope::scoped(later, served(&handle, &replica())).await, (0, 1));
    // Strong consistency reads the primary at once.
    let strong = RequestScope::new(Consistency::Strong, ReadAfter::new());
    let started = Instant::now();
    assert_eq!(scope::scoped(strong, served(&handle, &replica())).await, (0, 1));
    assert!(started.elapsed() < Duration::from_millis(200));
    assert_eq!(
        served(&handle, &replica().with_consistency(Consistency::Strong)).await,
        (0, 1)
    );

    // Once replayed, the replica serves the read that needs the write.
    c.resume_replay().await.unwrap();
    assert_eq!(served(&handle, &replica().after_all(&commits)).await, (1, 1));
    assert!(replicas.status()[0].replay_lsn.unwrap() >= lsn);
    assert_eq!(replicas.status()[0].fallbacks, 0, "waits are not failures");
}

#[tokio::test]
async fn read_your_writes_off_ignores_tokens() {
    let options = ReplicaOptions {
        read_your_writes: false,
        ..ReplicaOptions::default()
    };
    let Some((c, _replicas, handle)) = setup(&[], options).await else {
        return;
    };
    c.pause_replay().await.unwrap();
    let write = RequestScope::new(Consistency::Session, ReadAfter::new());
    scope::scoped(write.clone(), exec(&handle, "INSERT INTO rt (id) VALUES (1)")).await;
    assert_eq!(served(&handle, &replica().after_all(&write.commits())).await, (1, 0));
    c.resume_replay().await.unwrap();
}

#[tokio::test]
async fn a_recovery_conflict_is_retried_on_the_primary() {
    let settings = [("max_standby_streaming_delay", "0"), ("hot_standby_feedback", "off")];
    let Some((c, replicas, handle)) = setup(&settings, ReplicaOptions::default()).await else {
        return;
    };
    exec(&handle, "INSERT INTO rt (id) VALUES (1)").await;
    c.wait_caught_up().await.unwrap();
    // The replica read holds a lock on rt for 1.5 s; the primary's ALTER
    // TABLE replays an exclusive lock, which cancels it at once (delay 0).
    let slow = Statement::new(
        "SELECT CASE WHEN pg_is_in_recovery() THEN 1 ELSE 0 END AS standby, pg_sleep(1.5)::text AS slept, \
         (SELECT count(*) FROM rt) AS n",
        Vec::new(),
    );
    let read = async { handle.fetch_routed(&slow, &replica()).await };
    let alter = async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        exec(handle.database(), "ALTER TABLE rt ADD COLUMN extra integer").await;
    };
    let (rows, ()) = tokio::join!(read, alter);
    let rows = rows.expect("the read is retried on the primary, not failed");
    assert_eq!(rows.rows[0][0], Value::Integer(0), "served by the primary");
    assert_eq!(rows.rows[0][2], Value::Integer(1));
    let status = &replicas.status()[0];
    assert_eq!(status.fallbacks, 1, "{status:?}");
    assert_eq!(status.state, NodeState::Streaming, "a conflict is not an outage");
    assert_eq!(served(&handle, &replica()).await.0, 1);
}

#[tokio::test]
async fn replicas_of_another_cluster_are_refused() {
    let Some(mut c) = cluster(&[]).await else {
        return;
    };
    let primary = c.open_primary().await.unwrap();
    let standby = c.open_standby().await.unwrap();
    let facts = check_replica(&primary, &standby, "r1").await.unwrap();
    assert!(facts.in_recovery && facts.schema_readable);
    assert!(facts.version_num >= 160_000);
    let own = primary_facts(&primary).await.unwrap();
    assert!(!own.in_recovery);
    assert_eq!(facts.system_identifier, own.system_identifier);
    assert!(!own.system_identifier.is_empty());

    let err = check_replica(&primary, &primary, "self").await.unwrap_err();
    assert_eq!(err.code(), "node_mismatch");
    assert!(err.to_string().contains("not in recovery"), "{err}");

    let url = c.start_unrelated_standby().await.unwrap();
    let other = open_read_only(url).await.unwrap();
    let err = check_replica(&primary, &other, "other").await.unwrap_err();
    assert!(matches!(&err, ReplicaCheckError::Mismatch { .. }), "{err}");
    assert_eq!(err.code(), "node_mismatch");
    assert!(err.to_string().contains("system_identifier"), "{err}");
}

/// Opens replica nodes from a map of node name to URL.
#[derive(Debug, Default)]
struct UrlConnector(HashMap<String, String>);

impl SetConnector for UrlConnector {
    fn connect<'a>(&'a self, _set: &'a DbSet, primary: &'a DbNode) -> BoxFuture<'a, Result<Database, DbError>> {
        Box::pin(async move { Err(DbError::Unavailable(format!("no shard {}", primary.name))) })
    }

    fn connect_replica<'a>(&'a self, _set: &'a DbSet, node: &'a DbNode) -> BoxFuture<'a, Result<Database, DbError>> {
        Box::pin(async move {
            let url = self
                .0
                .get(&node.name)
                .cloned()
                .ok_or_else(|| DbError::Unavailable(format!("no replica {}", node.name)))?;
            open_read_only(url).await
        })
    }
}

#[tokio::test]
async fn the_router_routes_control_reads_to_checked_replicas() {
    let Some(mut c) = cluster(&[]).await else {
        return;
    };
    let control = c.open_primary().await.unwrap();
    let unrelated = c.start_unrelated_standby().await.unwrap();
    for name in ["good", "wrong", "missing"] {
        topology::insert_node(
            &control,
            &NewNode {
                set_id: "control".into(),
                name: name.into(),
                kind: NodeKind::External,
                role: NodeRole::Replica,
                url_sealed: None,
                state: NodeState::Ready,
                weight: 1,
                origin: NodeOrigin::Api,
            },
        )
        .await
        .unwrap();
    }
    c.wait_caught_up().await.unwrap();
    let connector = UrlConnector(HashMap::from([
        ("good".to_owned(), c.standby_url()),
        ("wrong".to_owned(), unrelated),
    ]));
    let options = RouterOptions {
        connector: Some(std::sync::Arc::new(connector)),
        replicas: manual(ReplicaOptions::default()),
        ..RouterOptions::default()
    };
    let router = DbRouter::open(control.clone(), options.clone()).await.unwrap();
    assert!(!router.is_sharded(), "replicas are not shards");
    let set = router.control();
    let replicas = set.replicas().expect("control has a replica");
    let names: Vec<String> = replicas.status().into_iter().map(|s| s.name).collect();
    assert_eq!(
        names,
        ["good"],
        "the other cluster's node and the unreachable one are left out"
    );
    let rows = set
        .fetch_routed(
            &Statement::new("SELECT CASE WHEN pg_is_in_recovery() THEN 1 ELSE 0 END", Vec::new()),
            &replica(),
        )
        .await
        .unwrap();
    assert_eq!(rows.scalar(), Some(&Value::Integer(1)));
    // A reload with the same nodes keeps the replica set (and its counters).
    router.reload().await.unwrap();
    assert_eq!(router.control().replicas().unwrap().status()[0].reads, 1);

    // The health transition (ready -> streaming) is recorded on control.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let nodes = topology::load(&control).await.unwrap().nodes;
        let good = nodes.iter().find(|n| n.name == "good").unwrap();
        if good.state == NodeState::Streaming {
            break;
        }
        assert!(Instant::now() < deadline, "state stayed {}", good.state);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // read_routing = primary opens no replicas.
    let primary_only = DbRouter::open(
        control.clone(),
        RouterOptions {
            replicas: ReplicaOptions {
                read_routing: ReadRouting::Primary,
                ..options.replicas
            },
            ..options
        },
    )
    .await
    .unwrap();
    assert!(primary_only.control().replicas().is_none());
}
