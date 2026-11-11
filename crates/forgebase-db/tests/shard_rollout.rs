//! FB-173: rolling the schema out over database sets (ADR-0029 §3.7, §3.9,
//! §3.10): a new shard's activation (migrations, sequence ranges, the
//! `fb_control` subscription), migrations over every set with
//! `shard_migrating`, and the two-phase checks. Real, separate PostgreSQL
//! clusters; skipped without an engine.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use forgebase_core::ErrorCode;
use forgebase_db::migrations::multi::{MultiOptions, NodeOutcome};
use forgebase_db::shard_testing::Sharded;
use forgebase_db::shards::{self, ShardSetup};
use forgebase_db::topology::{self, PlacementState, SetState};
use forgebase_db::{Backend, Database, Statement, TenantRef, TxMode, Value};

async fn scalar(db: &Database, sql: &str) -> Value {
    db.fetch_all(&Statement::new(sql, vec![]))
        .await
        .unwrap()
        .scalar()
        .cloned()
        .unwrap_or(Value::Null)
}

async fn exec(db: &Database, sql: &str) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(sql, vec![])).await.unwrap();
    tx.commit().await.unwrap();
}

fn setup<'a>(conninfo: Option<&'a forgebase_db::SecretString>) -> ShardSetup<'a> {
    ShardSetup {
        backups_dir: None,
        replay: None,
        control_conninfo: conninfo,
    }
}

#[tokio::test]
async fn a_new_shard_is_migrated_confined_and_subscribed_to_control() {
    let Some(mut s) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let (set, db) = s.provision("s1").await;
    assert!(!s.router.is_sharded());
    let conninfo = s.control_conninfo();
    let active = shards::activate_shard(&s.router, &set, &db, setup(Some(&conninfo)))
        .await
        .unwrap();
    assert_eq!(active.state, SetState::Active);
    assert!(s.router.is_sharded());
    assert!(s.router.set(&set.id).is_some());
    // Migrated to head.
    assert_eq!(
        db.schema_version().await.unwrap(),
        forgebase_db::migrations::postgres::PgMigrator::system().latest_version()
    );
    // Sequence ranges: control below 2^40, the shard in [k·2^40 + 1, …].
    let k = i64::from(set.number);
    let min = scalar(
        &db,
        "SELECT min(min_value) FROM pg_catalog.pg_sequences WHERE schemaname = 'forgebase'",
    )
    .await;
    assert_eq!(min.as_i64(), Some((k << 40) + 1));
    let max = scalar(
        &s.control,
        "SELECT max(max_value) FROM pg_catalog.pg_sequences WHERE schemaname = 'forgebase'",
    )
    .await;
    assert_eq!(max.as_i64(), Some((1_i64 << 40) - 1));
    // control_replicated rows reach the shard.
    let tables = shards::ensure_control_publication(&s.control).await.unwrap();
    // FB-153 added `_forgebase_db_logins` to the control_replicated class.
    assert_eq!(tables, shards::control_replicated_tables());
    assert!(tables.iter().any(|t| t == "_forgebase_resource_permissions"));
    exec(
        &s.control,
        "INSERT INTO _forgebase_resource_permissions (id, realm_id, resource_type, resource_id, grantee, permission, effect, created_by, created_at) \
         VALUES ('acl1', 'default', 'record', 'posts/1', 'user:people/ann', 'read', 'allow', 'root', 1)",
    )
    .await;
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let n = scalar(
            &db,
            "SELECT count(*) FROM _forgebase_resource_permissions WHERE id = 'acl1'",
        )
        .await;
        if n.as_i64() == Some(1) {
            break;
        }
        assert!(Instant::now() < deadline, "the ACL row never reached the shard");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let sub = shards::control_subscription(&db, set.number)
        .await
        .unwrap()
        .expect("subscribed");
    assert!(sub.enabled);
    assert_eq!(sub.tables, 1);
    // Activating again changes nothing and does not fail.
    shards::activate_shard(&s.router, &set, &db, setup(Some(&conninfo)))
        .await
        .unwrap();
    // Unsubscribing drops the subscription and the slot on control.
    shards::unsubscribe_from_control(&db, &s.control, set.number)
        .await
        .unwrap();
    assert!(shards::control_subscription(&db, set.number).await.unwrap().is_none());
    let slots = scalar(&s.control, "SELECT count(*) FROM pg_catalog.pg_replication_slots").await;
    assert_eq!(slots.as_i64(), Some(0));
}

#[tokio::test]
async fn migrations_run_on_every_set_and_migrating_shards_answer_503() {
    let Some(mut s) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let (set, db) = s.provision("s1").await;
    shards::activate_shard(&s.router, &set, &db, setup(None)).await.unwrap();
    s.router.place("t1", &set.id, PlacementState::Active).await.unwrap();
    assert_eq!(
        s.router.try_for_tenant(TenantRef::new(Some("t1"))).unwrap().id(),
        set.id
    );

    // Put the shard below head: forget the last migration and undo it.
    let head = forgebase_db::migrations::postgres::PgMigrator::system().latest_version();
    exec(
        &db,
        &format!("DELETE FROM _forgebase_migrations WHERE version = {head}"),
    )
    .await;
    exec(&db, "DROP TABLE _forgebase_index_builds").await;
    // A set marked migrating refuses its tenants with 503 shard_migrating.
    topology::update_set(&s.control, &set.id, Some(SetState::Migrating), None, None)
        .await
        .unwrap();
    s.router.reload().await.unwrap();
    let err = s.router.try_for_tenant(TenantRef::new(Some("t1"))).unwrap_err();
    assert_eq!(err.code(), "shard_migrating");
    assert_eq!(err.kind(), forgebase_core::ErrorKind::Unavailable);
    let err = s.router.for_tenant(TenantRef::new(Some("t1"))).await.unwrap_err();
    assert_eq!(err.code(), "shard_migrating");
    // Control and tenants on control are not affected.
    assert!(
        s.router
            .try_for_tenant(TenantRef::new(Some("elsewhere")))
            .unwrap()
            .is_control()
    );

    let report = shards::migrate_sets(&s.router, None, MultiOptions::default()).await;
    assert!(report.is_ok(), "{report:?}");
    assert_eq!(report.control.outcome, NodeOutcome::UpToDate);
    assert!(
        matches!(&report.shards[0].outcome, NodeOutcome::Applied(a) if a.len() == 1 && a[0].version == head),
        "{report:?}"
    );
    assert_eq!(db.schema_version().await.unwrap(), head);
    assert_eq!(s.router.set(&set.id).unwrap().set().state, SetState::Active);
    assert_eq!(
        s.router.try_for_tenant(TenantRef::new(Some("t1"))).unwrap().id(),
        set.id
    );
    // A second run finds every set at head.
    let report = shards::migrate_sets(&s.router, None, MultiOptions::default()).await;
    assert!(report.shards.iter().all(|r| r.outcome == NodeOutcome::UpToDate));
}

#[tokio::test]
async fn activation_refuses_a_node_without_prepared_transactions() {
    let Some(mut s) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let (set, db) = s.provision_with("s1", &[("max_prepared_transactions", "0")]).await;
    let err = shards::activate_shard(&s.router, &set, &db, setup(None))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "sharding_unsupported");
    assert_eq!(err.kind(), forgebase_core::ErrorKind::InvalidInput);
    // Nothing happened: still provisioning, not migrated, not routed.
    let topo = topology::load(&s.control).await.unwrap();
    assert_eq!(topo.set(&set.id).unwrap().state, SetState::Provisioning);
    assert_eq!(db.schema_version().await.unwrap(), 0);
    assert!(!s.router.is_sharded());
}
