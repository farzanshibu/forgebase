//! FB-173: two-phase commit across database sets, the journal and recovery
//! (ADR-0029 §3.7). Each test runs on real, separate PostgreSQL clusters
//! (`forgebase_db::shard_testing`) and skips without an engine
//! (`FORGEBASE_TEST_PG_ENGINE` or `FORGEBASE_PG_ENGINE_DIR`).

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use forgebase_core::ErrorCode;
use forgebase_db::shard_testing::TestCluster;
use forgebase_db::twophase::{
    self, InDoubt, NodeWork, Participant, SameStatements, TwoPhase, TwoPhaseError, node_gid,
};
use forgebase_db::{Backend, BoxFuture, Database, DbError, Statement, TxMode, Value, WriteTx};
use sqlx::{AssertSqlSafe, Connection};

/// Control and `shards` shard clusters, each with a migrated database.
async fn cluster(shards: usize, extra: &[(&str, &str)]) -> Option<(Vec<TestCluster>, Vec<Participant>)> {
    let mut clusters = Vec::new();
    let mut nodes = Vec::new();
    for k in 0..=shards {
        let name = if k == 0 { "control".to_owned() } else { format!("s{k}") };
        let c = TestCluster::start(&name, if k == shards { extra } else { &[] }).await?;
        let db = c.database("forgebase").await;
        nodes.push(Participant::new(name, u16::try_from(k).unwrap(), db));
        clusters.push(c);
    }
    Some((clusters, nodes))
}

async fn count(db: &Database, sql: &str) -> i64 {
    db.fetch_all(&Statement::new(sql, vec![]))
        .await
        .unwrap()
        .scalar()
        .and_then(Value::as_i64)
        .unwrap_or(-1)
}

async fn table_exists(db: &Database, name: &str) -> bool {
    count(db, &format!("SELECT count(*) FROM pg_catalog.pg_tables WHERE tablename = '{name}'")).await == 1
}

async fn journal_state(control: &Database, gid: &str) -> Option<String> {
    control
        .fetch_all(&Statement::new(
            "SELECT state FROM _forgebase_xact_journal WHERE gid = ?1",
            vec![gid.into()],
        ))
        .await
        .unwrap()
        .scalar()
        .and_then(|v| v.as_str().map(str::to_owned))
}

/// Prepares `sql` on `cluster`'s database outside any coordinator, as a
/// coordinator that crashed would have left it.
async fn prepare_raw(c: &TestCluster, sql: &str, node_gid: &str) {
    let mut conn = sqlx::PgConnection::connect(&c.url_of("forgebase")).await.unwrap();
    sqlx::raw_sql(AssertSqlSafe(format!("BEGIN; {sql}; PREPARE TRANSACTION '{node_gid}'")))
        .execute(&mut conn)
        .await
        .unwrap();
    let _ = conn.close().await;
}

/// Creates a table and writes the node's number into it.
struct CreateAndInsert {
    fail_on: Option<u16>,
}

impl NodeWork<DbError> for CreateAndInsert {
    fn run<'a>(&'a self, node: &'a Participant, tx: &'a mut dyn WriteTx) -> BoxFuture<'a, Result<(), DbError>> {
        Box::pin(async move {
            tx.execute(&Statement::raw("CREATE TABLE public.t173 (k bigint NOT NULL)"))
                .await?;
            tx.execute(&Statement::new(
                "INSERT INTO public.t173 (k) VALUES (?1)",
                vec![i64::from(node.number).into()],
            ))
            .await?;
            if self.fail_on == Some(node.number) {
                tx.execute(&Statement::raw("SELECT no_such_column FROM public.t173"))
                    .await?;
            }
            Ok(())
        })
    }
}

#[tokio::test]
async fn a_schema_change_commits_on_every_node() {
    let Some((_c, nodes)) = cluster(2, &[]).await else {
        return;
    };
    let control = nodes[0].db.clone();
    let out = twophase::run::<DbError>(
        &control,
        &nodes,
        "test create",
        TxMode::SchemaChange,
        &CreateAndInsert { fail_on: None },
    )
    .await
    .expect("commits")
    .expect("two-phase with three nodes");
    assert_eq!(out.nodes, vec!["control", "s1", "s2"]);
    assert!(out.pending.is_empty());
    for n in &nodes {
        assert!(table_exists(&n.db, "t173").await, "{} has the table", n.set_id);
        assert_eq!(
            count(&n.db, "SELECT k FROM public.t173").await,
            i64::from(n.number)
        );
        assert!(n.db.prepared_transactions().await.unwrap().is_empty());
    }
    assert_eq!(journal_state(&control, &out.gid).await.as_deref(), Some("committed"));
    // Readers see the new table at once (schema generation bumped).
    for n in &nodes {
        assert_eq!(count(&n.db, "SELECT count(*) FROM public.t173").await, 1);
    }
}

#[tokio::test]
async fn a_failure_before_the_decision_rolls_back_every_part() {
    let Some((_c, nodes)) = cluster(2, &[]).await else {
        return;
    };
    let control = nodes[0].db.clone();
    let err = twophase::run::<DbError>(
        &control,
        &nodes,
        "test failure",
        TxMode::SchemaChange,
        &CreateAndInsert { fail_on: Some(2) },
    )
    .await
    .expect_err("s2 fails");
    assert_eq!(err.code(), "invalid_statement");
    for n in &nodes {
        assert!(!table_exists(&n.db, "t173").await, "{} rolled back", n.set_id);
        assert!(n.db.prepared_transactions().await.unwrap().is_empty());
    }
    assert_eq!(count(&control, "SELECT count(*) FROM _forgebase_xact_journal").await, 0);
}

#[tokio::test]
async fn abort_and_drop_roll_back_prepared_parts() {
    let Some((_c, nodes)) = cluster(1, &[]).await else {
        return;
    };
    let control = nodes[0].db.clone();
    let mut two = TwoPhase::new(&control, "test abort");
    for n in &nodes {
        let mut tx = n.db.begin(TxMode::Normal).await.unwrap();
        tx.execute(&Statement::raw("CREATE TABLE public.t173 (k bigint)")).await.unwrap();
        two.prepare(n, tx).await.unwrap();
    }
    assert_eq!(nodes[1].db.prepared_transactions().await.unwrap().len(), 1);
    assert!(two.abort().await.is_empty());
    for n in &nodes {
        assert!(n.db.prepared_transactions().await.unwrap().is_empty());
        assert!(!table_exists(&n.db, "t173").await);
    }
    // Dropped undecided (a cancelled request): rolled back in the background.
    let mut two = TwoPhase::new(&control, "test drop");
    let tx = nodes[1].db.begin(TxMode::Normal).await.unwrap();
    two.prepare(&nodes[1], tx).await.unwrap();
    drop(two);
    let mut left = 1;
    for _ in 0..50 {
        left = nodes[1].db.prepared_transactions().await.unwrap().len();
        if left == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(left, 0);
}

#[tokio::test]
async fn recovery_rolls_back_parts_without_a_decision_after_a_crash() {
    let Some((c, nodes)) = cluster(1, &[]).await else {
        return;
    };
    let control = nodes[0].db.clone();
    let gid = twophase::new_gid();
    prepare_raw(&c[0], "CREATE TABLE public.t173 (k bigint)", &node_gid(&gid, 0)).await;
    prepare_raw(&c[1], "CREATE TABLE public.t173 (k bigint)", &node_gid(&gid, 1)).await;
    // The shard crashes; its prepared part survives the restart.
    c[1].crash();
    c[1].restart().await;
    let shard = c[1].open("forgebase").await;
    let nodes = vec![nodes[0].clone(), Participant::new("s1", 1, shard.clone())];
    assert_eq!(shard.prepared_transactions().await.unwrap().len(), 1);
    // Younger than the grace period: left alone.
    let r = twophase::recover(&control, &nodes, Duration::from_secs(3600)).await.unwrap();
    assert_eq!(r.skipped.len(), 2, "{r:?}");
    assert!(r.rolled_back.is_empty());
    let doubt = twophase::in_doubt(&control, &nodes, Duration::ZERO).await.unwrap();
    assert!(
        doubt
            .iter()
            .all(|d| matches!(d, InDoubt::Prepared { journal: None, .. })),
        "{doubt:?}"
    );
    assert_eq!(doubt.len(), 2);
    // Past the grace period: no journal row, so roll back.
    let r = twophase::recover(&control, &nodes, Duration::ZERO).await.unwrap();
    assert_eq!(r.rolled_back.len(), 2, "{r:?}");
    for db in [&control, &shard] {
        assert!(db.prepared_transactions().await.unwrap().is_empty());
        assert!(!table_exists(db, "t173").await);
    }
    assert!(twophase::in_doubt(&control, &nodes, Duration::ZERO).await.unwrap().is_empty());
}

#[tokio::test]
async fn recovery_commits_decided_parts_after_a_node_crash() {
    let Some((c, nodes)) = cluster(2, &[]).await else {
        return;
    };
    let control = nodes[0].db.clone();
    let gid = twophase::new_gid();
    for (k, cl) in c.iter().enumerate() {
        prepare_raw(cl, "CREATE TABLE public.t173 (k bigint)", &node_gid(&gid, u16::try_from(k).unwrap())).await;
    }
    // The decision was written, control and s1 committed, then s2 crashed.
    let mut tx = control.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_xact_journal (gid, nodes, purpose) VALUES (?1, CAST(?2 AS jsonb), 'test') \
         ON CONFLICT (gid) DO NOTHING",
        vec![gid.clone().into(), r#"["control","s1","s2"]"#.into()],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(control.commit_prepared(&node_gid(&gid, 0)).await.unwrap());
    assert!(nodes[1].db.commit_prepared(&node_gid(&gid, 1)).await.unwrap());
    c[2].crash();
    // While s2 is down, recovery cannot finish: the row stays open.
    let r = twophase::recover(&control, &nodes, Duration::ZERO).await.unwrap();
    assert_eq!(r.unreachable.len(), 1, "{r:?}");
    assert!(r.closed.is_empty());
    assert_eq!(journal_state(&control, &gid).await.as_deref(), Some("committing"));
    c[2].restart().await;
    let s2 = c[2].open("forgebase").await;
    let nodes = vec![nodes[0].clone(), nodes[1].clone(), Participant::new("s2", 2, s2.clone())];
    let r = twophase::recover(&control, &nodes, Duration::ZERO).await.unwrap();
    assert_eq!(r.committed, vec![node_gid(&gid, 2)], "{r:?}");
    assert_eq!(r.closed, vec![gid.clone()]);
    assert!(table_exists(&s2, "t173").await);
    assert_eq!(journal_state(&control, &gid).await.as_deref(), Some("committed"));
    // Nothing left; a second run is a no-op.
    assert!(twophase::recover(&control, &nodes, Duration::ZERO).await.unwrap().is_empty());
}

#[tokio::test]
async fn recovery_skips_coordinators_in_flight_and_doctor_reports_stuck_rows() {
    let Some((_c, nodes)) = cluster(1, &[]).await else {
        return;
    };
    let control = nodes[0].db.clone();
    let mut two = TwoPhase::new(&control, "in flight");
    let tx = nodes[1].db.begin(TxMode::Normal).await.unwrap();
    two.prepare(&nodes[1], tx).await.unwrap();
    let r = twophase::recover(&control, &nodes, Duration::ZERO).await.unwrap();
    assert_eq!(r.skipped, vec![node_gid(two.gid(), 1)]);
    let gid = two.gid().to_owned();
    let done = two.commit().await.unwrap();
    assert_eq!(done.gid, gid);
    assert_eq!(journal_state(&control, &gid).await.as_deref(), Some("committed"));
    // A decided row that names an unreachable set for a long time.
    let mut tx = control.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_xact_journal (gid, nodes, purpose, created_at) \
         VALUES ('fb_00000000000000000000000000000001', '[\"control\",\"gone\"]', 'old', '2020-01-01T00:00:00Z')",
        vec![],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let doubt = twophase::in_doubt(&control, &nodes, Duration::ZERO).await.unwrap();
    assert!(
        matches!(doubt.as_slice(), [InDoubt::Stuck { purpose, .. }] if purpose == "old"),
        "{doubt:?}"
    );
    // Recovery cannot close it (set "gone" is not reachable).
    let r = twophase::recover(&control, &nodes, Duration::ZERO).await.unwrap();
    assert!(r.closed.is_empty(), "{r:?}");
}

#[tokio::test]
async fn nodes_without_prepared_transactions_are_refused() {
    let Some((_c, nodes)) = cluster(1, &[("max_prepared_transactions", "0")]).await else {
        return;
    };
    assert_eq!(nodes[1].db.max_prepared_transactions().await.unwrap(), 0);
    let err = twophase::check_supported(&nodes[1]).await.expect_err("refused");
    assert!(matches!(err, TwoPhaseError::Unsupported(ref s) if s == "s1"));
    assert_eq!(err.code(), "sharding_unsupported");
    let work = SameStatements(vec![Statement::raw("CREATE TABLE public.t173 (k bigint)")]);
    let err = twophase::run::<DbError>(&nodes[0].db, &nodes, "refused", TxMode::Normal, &work)
        .await
        .expect_err("refused");
    assert_eq!(err.code(), "database_unsupported");
    assert!(!table_exists(&nodes[0].db, "t173").await);
    // PREPARE on such a node says what to change.
    let mut tx = nodes[1].db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw("CREATE TABLE public.t173 (k bigint)")).await.unwrap();
    let err = tx.prepare(node_gid(&twophase::new_gid(), 1)).await.expect_err("disabled");
    assert!(err.to_string().contains("max_prepared_transactions"), "{err}");
}
