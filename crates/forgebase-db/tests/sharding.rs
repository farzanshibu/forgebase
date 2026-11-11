//! FB-170: topology tables, placement registry, sequence ranges and the set
//! router (ADR-0029 §3.1–§3.5, §4.2).
//!
//! Tests that need one database run on the backend `FORGEBASE_TEST_BACKEND`
//! selects; the registry is checked on SQLite always and on PostgreSQL when
//! an engine is configured; multi-set tests use two PostgreSQL test
//! databases (one per set) and skip without an engine.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forgebase_db::placement;
use forgebase_db::router::{PlacementMode, PlacementPolicy, RouterOptions, SetConnector};
use forgebase_db::testing::{self, TestDb};
use forgebase_db::topology::{
    self, CONTROL_SET_ID, DbNode, DbSet, NewNode, NewSet, NodeKind, NodeOrigin, NodeRole, NodeState, PlacementState,
    SetKind, SetState,
};
use forgebase_db::{
    Backend, BackendKind, BoxFuture, Database, DbError, DbRouter, RouterError, Statement, TenantRef, TxMode, Value,
};

async fn scalar(db: &dyn Backend, sql: &str, params: Vec<Value>) -> Value {
    db.fetch_all(&Statement::new(sql, params))
        .await
        .unwrap()
        .scalar()
        .cloned()
        .unwrap_or(Value::Null)
}

async fn placement_rows(db: &dyn Backend) -> i64 {
    scalar(db, "SELECT count(*) FROM _forgebase_tenant_placements", vec![])
        .await
        .as_i64()
        .unwrap()
}

fn assert_complete(schema: &placement::SchemaTables, backend: &str) {
    assert!(
        schema.len() > 140,
        "{backend}: only {} system tables read",
        schema.len()
    );
    for t in [
        "_forgebase_db_sets",
        "_forgebase_tenant_placements",
        "_forgebase_migrations",
    ] {
        assert!(schema.contains_key(t), "{backend}: {t} missing");
    }
    let problems = placement::problems(schema);
    assert!(
        problems.is_empty(),
        "{backend}: placement registry problems:\n{}",
        problems.join("\n")
    );
}

#[tokio::test]
async fn every_system_table_is_classified_on_sqlite() {
    let db = testing::test_backend_of(BackendKind::Sqlite).await;
    let schema = placement::system_tables(&db).await.unwrap();
    assert_complete(&schema, "sqlite");
    for t in placement::SQLITE_ONLY_TABLES {
        assert!(!schema.contains_key(*t), "{t} is left out");
    }
}

#[tokio::test]
async fn every_system_table_is_classified_on_postgres() {
    let Some(db) = testing::postgres_test_db().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let pg = placement::system_tables(&db).await.unwrap();
    assert_complete(&pg, "postgres");
    // Both schemas hold the same system tables (FTS5 shadow tables aside).
    let sqlite = testing::test_backend_of(BackendKind::Sqlite).await;
    let lite = placement::system_tables(&sqlite).await.unwrap();
    assert_eq!(
        pg.keys().collect::<Vec<_>>(),
        lite.keys().collect::<Vec<_>>(),
        "system tables differ between the backends"
    );
}

#[tokio::test]
async fn migration_seeds_the_control_set() {
    let db = testing::test_backend().await;
    let t = topology::load(&*db).await.unwrap();
    assert_eq!(t.epoch, 0);
    assert!(t.nodes.is_empty());
    assert_eq!(t.sets.len(), 1);
    let control = &t.sets[0];
    assert_eq!(control.id, CONTROL_SET_ID);
    assert_eq!(
        (control.kind, control.number, control.state),
        (SetKind::Control, 0, SetState::Active)
    );
    assert!(control.hosts_tenants && control.weight == 1);
    assert!(!control.created_at.is_empty());
    assert!(!t.is_sharded());
    // Only one control set can exist.
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    let err = tx
        .execute(&Statement::raw(
            "INSERT INTO _forgebase_db_sets (id, name, kind, number) VALUES ('c2', 'c2', 'control', 1)",
        ))
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::Constraint { .. }), "{err:?}");
}

/// One set: the router hands out the one database and writes nothing.
async fn assert_behaves_as_one_set(router: &DbRouter, db: &TestDb) {
    assert!(!router.is_sharded());
    assert_eq!(router.sets().len(), 1);
    let control = router.control();
    assert!(control.is_control() && control.number() == 0);
    assert_eq!(control.location(), db.location());
    for tenant in [TenantRef::Instance, TenantRef::from("env1"), TenantRef::from("")] {
        let h = router.for_tenant(tenant).await.unwrap();
        assert_eq!(h.id(), CONTROL_SET_ID);
        assert_eq!(router.try_for_tenant(tenant).unwrap().id(), CONTROL_SET_ID);
    }
    // Writes through a handle land in the one database.
    let h = router.for_tenant(TenantRef::from("env1")).await.unwrap();
    h.meta_set("fb170_probe", "1").await.unwrap();
    assert_eq!(db.meta_get("fb170_probe").await.unwrap().as_deref(), Some("1"));
    let rows = h
        .fetch_all(&Statement::new(
            "SELECT value FROM _forgebase_meta WHERE key = ?1",
            vec!["fb170_probe".into()],
        ))
        .await
        .unwrap();
    assert_eq!(rows.scalar().and_then(Value::as_str), Some("1"));
    // New tenants stay on control without a placement row.
    let placed = router.place_new_tenant("env1", Some("p1"), None).await.unwrap();
    assert_eq!(placed.id(), CONTROL_SET_ID);
    assert_eq!(
        router
            .place_new_tenant("env2", None, Some(CONTROL_SET_ID))
            .await
            .unwrap()
            .id(),
        CONTROL_SET_ID
    );
    assert!(matches!(
        router.place_new_tenant("env3", None, Some("elsewhere")).await,
        Err(RouterError::UnknownSet(_))
    ));
    assert_eq!(placement_rows(&**db).await, 0);
    assert!(router.placement("env1").is_none());
}

#[tokio::test]
async fn single_router_is_todays_database() {
    let db = testing::test_backend().await;
    let router = DbRouter::single(db.database().clone());
    assert_behaves_as_one_set(&router, &db).await;
    assert!(matches!(
        router.place("env1", CONTROL_SET_ID, PlacementState::Active).await,
        Err(RouterError::SingleSet)
    ));
    router.reload().await.unwrap();
    assert_eq!(router.epoch(), 0);
}

#[tokio::test]
async fn opened_router_on_one_set_behaves_the_same() {
    let db = testing::test_backend().await;
    let file = db.data_dir().join(topology::TOPOLOGY_FILE);
    let router = DbRouter::open(
        db.database().clone(),
        RouterOptions {
            topology_file: Some(file.clone()),
            ..RouterOptions::default()
        },
    )
    .await
    .unwrap();
    assert_behaves_as_one_set(&router, &db).await;
    assert_eq!(router.epoch(), 0);
    let written = topology::read_topology_file(&file).unwrap().unwrap();
    assert_eq!(written.topology.sets.len(), 1);
    assert_eq!(written.topology.epoch, 0);
    // Without a path nothing is written next to the database.
    let plain = DbRouter::open(db.database().clone(), RouterOptions::default())
        .await
        .unwrap();
    assert!(plain.topology_file().is_none());

    match db.kind() {
        BackendKind::Sqlite => {
            assert!(topology::apply_sequence_range(&db, 0).await.unwrap().is_empty());
            assert!(matches!(
                topology::apply_sequence_range(&db, 1).await,
                Err(DbError::Unsupported(_))
            ));
            let new = NewSet {
                name: "shard-1".into(),
                weight: 1,
            };
            assert!(matches!(
                topology::insert_set(&db, &new).await,
                Err(DbError::Unsupported(_))
            ));
        }
        BackendKind::Postgres => {}
    }
}

#[tokio::test]
async fn sequence_ranges_confine_identity_values() {
    let Some(shard) = testing::postgres_test_db().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let insert = "INSERT INTO _forgebase_billing_audit (at, actor, action, object, object_id) \
                  VALUES (1, 'a', 'b', 'c', 'd') RETURNING seq";
    async fn next_seq(db: &Database, sql: &str) -> i64 {
        let mut tx = db.begin(TxMode::Normal).await.unwrap();
        let rows = tx.fetch_all(&Statement::raw(sql)).await.unwrap();
        tx.commit().await.unwrap();
        rows.scalar().and_then(Value::as_i64).unwrap()
    }
    // A fresh set 3: every sequence restarts in [3·2⁴⁰ + 1, 4·2⁴⁰ − 1].
    let sequences = scalar(
        &*shard,
        "SELECT count(*) FROM pg_sequences WHERE schemaname = 'forgebase'",
        vec![],
    )
    .await
    .as_i64()
    .unwrap();
    let changed = topology::apply_sequence_range(&shard, 3).await.unwrap();
    assert!(
        sequences >= 18 && changed.len() as i64 == sequences,
        "{sequences}: {changed:?}"
    );
    assert!(changed.iter().all(|n| n.starts_with("forgebase.")), "{changed:?}");
    let range = topology::sequence_range(3);
    let id = next_seq(&shard, insert).await;
    assert_eq!(id, range.min);
    assert_eq!(topology::set_number_of(id), 3);
    assert!(
        topology::apply_sequence_range(&shard, 3).await.unwrap().is_empty(),
        "idempotent"
    );
    let max: i64 = scalar(
        &*shard,
        "SELECT max_value FROM pg_sequences WHERE schemaname = 'forgebase' AND sequencename = '_forgebase_billing_audit_seq_seq'",
        vec![],
    )
    .await
    .as_i64()
    .unwrap();
    assert_eq!(max, range.max);

    // Control (0): existing ids continue; only the bounds change.
    let control = testing::postgres_test_db().await.unwrap();
    let first = next_seq(&control, insert).await;
    assert!(!topology::apply_sequence_range(&control, 0).await.unwrap().is_empty());
    assert_eq!(next_seq(&control, insert).await, first + 1);
    assert!(topology::apply_sequence_range(&control, 0).await.unwrap().is_empty());
    assert!(matches!(
        topology::apply_sequence_range(&control, topology::MAX_SET_NUMBER + 1).await,
        Err(DbError::Statement(_))
    ));
}

/// Opens shards from a map of node name to test database.
#[derive(Debug, Default)]
struct MapConnector(HashMap<String, Database>);

impl SetConnector for MapConnector {
    fn connect<'a>(&'a self, _set: &'a DbSet, primary: &'a DbNode) -> BoxFuture<'a, Result<Database, DbError>> {
        Box::pin(async move {
            self.0
                .get(&primary.name)
                .cloned()
                .ok_or_else(|| DbError::Unavailable(format!("no database for node {}", primary.name)))
        })
    }
}

async fn add_shard(control: &Database, name: &str) -> (DbSet, DbNode) {
    let set = topology::insert_set(
        control,
        &NewSet {
            name: name.into(),
            weight: 1,
        },
    )
    .await
    .unwrap();
    let node = topology::insert_node(
        control,
        &NewNode {
            set_id: set.id.clone(),
            name: format!("{name}-primary"),
            kind: NodeKind::External,
            role: NodeRole::Primary,
            url_sealed: Some("sealed-url".into()),
            state: NodeState::Ready,
            weight: 1,
            origin: NodeOrigin::Api,
        },
    )
    .await
    .unwrap();
    (set, node)
}

async fn exec(db: &Database, sql: &str, params: Vec<Value>) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(sql, params)).await.unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn router_routes_tenants_to_their_sets() {
    let Some(control) = testing::postgres_test_db().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let shard_db = testing::postgres_test_db().await.unwrap();
    let (set, node) = add_shard(&control, "shard-1").await;
    assert_eq!(
        (set.number, set.kind, set.state),
        (1, SetKind::Shard, SetState::Provisioning)
    );
    assert!(
        !topology::apply_sequence_range(&shard_db, set.number)
            .await
            .unwrap()
            .is_empty()
    );

    let connector = Arc::new(MapConnector(HashMap::from([(
        node.name.clone(),
        shard_db.database().clone(),
    )])));
    let file = control.data_dir().join(topology::TOPOLOGY_FILE);
    let options = RouterOptions {
        connector: Some(connector.clone()),
        topology_file: Some(file.clone()),
        ..RouterOptions::default()
    };

    // A provisioning set is not routable: still one set, and no connector needed.
    let router = DbRouter::open(control.database().clone(), RouterOptions::default())
        .await
        .unwrap();
    assert!(!router.is_sharded());
    assert_eq!(router.epoch(), 2, "insert_set and insert_node bump the epoch");

    topology::update_set(&*control, &set.id, Some(SetState::Active), None, None)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(router.reload().await, Err(RouterError::NoConnector)));
    let router = DbRouter::open(control.database().clone(), options.clone())
        .await
        .unwrap();
    assert!(router.is_sharded());
    assert_eq!(router.epoch(), 3);
    let sets = router.sets();
    assert_eq!(sets.iter().map(|s| s.number()).collect::<Vec<_>>(), vec![0, 1]);
    assert_eq!(router.set_by_number(1).unwrap().id(), set.id);
    assert_eq!(router.set(&set.id).unwrap().location(), shard_db.location());

    // Explicit placement: env-a on the shard.
    let p = router.place("env-a", &set.id, PlacementState::Active).await.unwrap();
    assert_eq!(
        (p.epoch, p.state, p.set_id.as_str()),
        (1, PlacementState::Active, set.id.as_str())
    );
    assert!(p.moved_at.is_none());
    assert_eq!(router.epoch(), 4);
    let again = router.place("env-a", &set.id, PlacementState::Active).await.unwrap();
    assert_eq!(again.epoch, 1, "an unchanged placement keeps its epoch");
    assert_eq!(router.epoch(), 4);
    let a = router.for_tenant(TenantRef::from("env-a")).await.unwrap();
    assert_eq!(a.number(), 1);
    a.meta_set("fb170_tenant_write", "a").await.unwrap();
    assert_eq!(
        shard_db.meta_get("fb170_tenant_write").await.unwrap().as_deref(),
        Some("a")
    );
    assert_eq!(control.meta_get("fb170_tenant_write").await.unwrap(), None);
    assert_eq!(router.for_tenant(TenantRef::from("env-b")).await.unwrap().number(), 0);
    assert_eq!(router.for_tenant(TenantRef::Instance).await.unwrap().number(), 0);
    assert!(matches!(
        router.place("env-x", "nope", PlacementState::Active).await,
        Err(RouterError::UnknownSet(_))
    ));

    // Least loaded: equal bytes, control has fewer placement rows.
    assert_eq!(router.place_new_tenant("env-c", None, None).await.unwrap().number(), 0);
    assert_eq!(placement_rows(&*control).await, 2, "sharded: new tenants get a row");
    // Bytes decide once the statistics are in.
    assert!(
        topology::record_placement_stats(&*control, "env-c", 10, 1_000_000)
            .await
            .unwrap()
    );
    router.reload().await.unwrap();
    assert_eq!(router.placement("env-c").unwrap().est_bytes, 1_000_000);
    assert_eq!(router.place_new_tenant("env-d", None, None).await.unwrap().number(), 1);
    // Idempotent for a placed tenant.
    assert_eq!(
        router
            .place_new_tenant("env-d", None, Some(CONTROL_SET_ID))
            .await
            .unwrap()
            .number(),
        1
    );
    // An administrator's choice wins.
    assert_eq!(
        router
            .place_new_tenant("env-e", None, Some(CONTROL_SET_ID))
            .await
            .unwrap()
            .number(),
        0
    );
    assert!(matches!(
        router.place_new_tenant("env-f", None, Some("nope")).await,
        Err(RouterError::UnknownSet(_))
    ));

    // Project affinity: a new environment joins its project's set, even when
    // that set is the more loaded one.
    let now = 1_700_000_000_i64;
    exec(
        &control,
        "INSERT INTO _forgebase_projects (id, realm_id, owner_user, slug, name, created_at) VALUES ('p1', 'default', 'u1', 'p1', 'P1', ?1)",
        vec![now.into()],
    )
    .await;
    for (env, at) in [("p1-prod", now), ("p1-dev", now + 1)] {
        exec(
            &control,
            "INSERT INTO _forgebase_environments (id, project_id, name, created_at) VALUES (?1, 'p1', ?1, ?2)",
            vec![env.into(), at.into()],
        )
        .await;
    }
    router.place("p1-prod", &set.id, PlacementState::Active).await.unwrap();
    assert!(
        topology::record_placement_stats(&*control, "p1-prod", 1, 50_000_000)
            .await
            .unwrap()
    );
    router.reload().await.unwrap();
    assert_eq!(
        router
            .place_new_tenant("p1-dev", Some("p1"), None)
            .await
            .unwrap()
            .number(),
        1
    );
    let control_mode = DbRouter::open(
        control.database().clone(),
        RouterOptions {
            policy: PlacementPolicy {
                mode: PlacementMode::Control,
                project_affinity: true,
            },
            ..options.clone()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        control_mode
            .place_new_tenant("env-g", None, None)
            .await
            .unwrap()
            .number(),
        0
    );

    // Draining: no new tenants, existing ones still served.
    topology::update_set(&*control, &set.id, Some(SetState::Draining), None, None)
        .await
        .unwrap();
    router.reload().await.unwrap();
    assert!(matches!(
        router.place_new_tenant("env-h", None, Some(&set.id)).await,
        Err(RouterError::SetClosed(_))
    ));
    assert_eq!(router.place_new_tenant("env-h", None, None).await.unwrap().number(), 0);
    assert_eq!(router.for_tenant(TenantRef::from("env-a")).await.unwrap().number(), 1);
    topology::update_set(&*control, &set.id, Some(SetState::Active), None, None)
        .await
        .unwrap();
    router.reload().await.unwrap();

    // Frozen: requests wait for the placement to change, then go on.
    let frozen = router.place("env-a", &set.id, PlacementState::Frozen).await.unwrap();
    assert_eq!(frozen.epoch, 2);
    assert!(matches!(
        router.try_for_tenant(TenantRef::from("env-a")),
        Err(RouterError::TenantMoving(_))
    ));
    let flipper = {
        let router = router.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(150)).await;
            router
                .place("env-a", CONTROL_SET_ID, PlacementState::Cleanup)
                .await
                .unwrap()
        })
    };
    let started = Instant::now();
    let after = router.for_tenant(TenantRef::from("env-a")).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert_eq!(after.number(), 0, "the request runs where the flip sent it");
    let moved = flipper.await.unwrap();
    assert_eq!((moved.epoch, moved.state), (3, PlacementState::Cleanup));
    assert!(moved.moved_at.is_some());
    // A frozen tenant past the timeout is refused with tenant_moving.
    let impatient = DbRouter::open(
        control.database().clone(),
        RouterOptions {
            freeze_timeout: Duration::from_millis(100),
            ..options.clone()
        },
    )
    .await
    .unwrap();
    impatient
        .place("env-e", CONTROL_SET_ID, PlacementState::Frozen)
        .await
        .unwrap();
    assert!(matches!(
        impatient.for_tenant(TenantRef::from("env-e")).await,
        Err(RouterError::TenantMoving(_))
    ));
    impatient
        .place("env-e", CONTROL_SET_ID, PlacementState::Active)
        .await
        .unwrap();

    // The cache never goes back to an older epoch.
    let mut stale = router.placement("env-a").unwrap();
    stale.epoch = 1;
    stale.set_id = set.id.clone();
    assert!(!router.apply_placement(stale));
    assert_eq!(router.for_tenant(TenantRef::from("env-a")).await.unwrap().number(), 0);
    router.reload().await.unwrap();

    // A fresh router (a restart) sees the same placements and topology.
    let restarted = DbRouter::open(control.database().clone(), options.clone())
        .await
        .unwrap();
    assert_eq!(restarted.placements(), router.placements());
    assert_eq!(restarted.epoch(), router.epoch());
    assert_eq!(
        restarted.for_tenant(TenantRef::from("env-d")).await.unwrap().number(),
        1
    );

    // topology.json follows every change.
    let written = topology::read_topology_file(&file).unwrap().unwrap();
    assert_eq!(written.topology.epoch, router.epoch());
    assert_eq!(written.topology, router.topology());
    assert_eq!(written.topology.sets.len(), 2);
    assert_eq!(written.topology.nodes, vec![node.clone()]);
    assert_eq!(written.topology.primary_of(&set.id).unwrap().name, node.name);

    // Notifications carry the epoch.
    let mut rx = router.subscribe();
    router.place("env-z", &set.id, PlacementState::Active).await.unwrap();
    rx.changed().await.unwrap();
    assert_eq!(*rx.borrow(), router.epoch());
}
