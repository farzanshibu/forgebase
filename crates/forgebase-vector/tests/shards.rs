//! Shard routing of the vector service (ADR-0029 §3.5, FB-172).
//!
//! * One set (SQLite and PostgreSQL): a service with
//!   `attach_router(DbRouter::single(db))` (or `open_routed`) behaves as
//!   without a router.
//! * Several sets (PostgreSQL, `forgebase_db::shard_testing::Sharded`;
//!   skipped without an engine): a tenant's collections, items and values
//!   land on its set only and reads stay there; the shared `''` scope is on
//!   control; each set has its own storage engine (control on pgvector when
//!   the engine has it, the shard converted to portable); the in-process
//!   indexes are loaded from every set after a restart and warmed up per
//!   set; a tenant being moved keeps being served but refuses schema
//!   changes (`409 tenant_moving`), a frozen one answers `tenant_moving`
//!   after the freeze timeout; conversions skip the partial copy of a tenant
//!   moving in and the stale copy of one moved out, and wait for a frozen
//!   tenant; after a move's flip the tenant's collections follow it to the
//!   new set (its vectors converted to that set's engine).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use forgebase_core::config::ExtensionMode;
use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::{DataService, DefaultAuthorizer};
use forgebase_db::shard_testing::Sharded;
use forgebase_db::topology::PlacementState;
use forgebase_db::{Backend, Database, DbRouter, RouterError, RouterOptions, Statement, TxMode, Value, shards};
use forgebase_vector::model::*;
use forgebase_vector::{VectorConfig, VectorError, VectorService};
use serde_json::json;

async fn scalar(db: &Database, sql: &str) -> i64 {
    db.fetch_all(&Statement::raw(sql))
        .await
        .unwrap()
        .scalar()
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

async fn texts(db: &Database, sql: &str) -> Vec<String> {
    db.fetch_all(&Statement::raw(sql))
        .await
        .unwrap()
        .rows
        .iter()
        .map(|r| r[0].as_str().unwrap_or_default().to_owned())
        .collect()
}

async fn pin(db: &Database) -> Option<String> {
    texts(db, "SELECT value FROM _forgebase_meta WHERE key = 'ext.vector.engine'")
        .await
        .into_iter()
        .next()
}

/// Id of `tenant`'s collection `name` on `db`.
async fn cid(db: &Database, tenant: &str, name: &str) -> i64 {
    scalar(
        db,
        &format!("SELECT id FROM _forgebase_vector_collections WHERE tenant = '{tenant}' AND name = '{name}'"),
    )
    .await
}

/// Values of collection `cid` on `db` whose vector is in `column`.
async fn in_column(db: &Database, cid: i64, column: &str) -> i64 {
    scalar(
        db,
        &format!("SELECT COUNT(*) FROM _forgebase_vector_values WHERE collection_id = {cid} AND {column} IS NOT NULL"),
    )
    .await
}

fn moving(e: &VectorError) -> bool {
    matches!(e, VectorError::Router(RouterError::TenantMoving(_)))
        && e.code() == "tenant_moving"
        && e.kind() == ErrorKind::Conflict
}

/// Copies `tenant`'s vector rows from `from` to `to` with their ids, as a
/// move's copy does (FB-174): rows of the source's column as they are.
async fn copy_tenant(from: &Database, to: &Database, tenant: &str) {
    let colls = format!("SELECT id FROM _forgebase_vector_collections WHERE tenant = '{tenant}'");
    for (table, filter) in [
        ("_forgebase_vector_collections", format!("tenant = '{tenant}'")),
        ("_forgebase_vector_fields", format!("collection_id IN ({colls})")),
        ("_forgebase_vector_items", format!("collection_id IN ({colls})")),
        ("_forgebase_vector_values", format!("collection_id IN ({colls})")),
    ] {
        let rows = texts(
            from,
            &format!("SELECT CAST(row_to_json(t) AS text) FROM {table} t WHERE {filter}"),
        )
        .await;
        let mut tx = to.begin(TxMode::Normal).await.unwrap();
        for r in rows {
            tx.execute(&Statement::new(
                format!(
                    "INSERT INTO {table} SELECT * FROM json_populate_record(CAST(NULL AS {table}), CAST(?1 AS json))"
                ),
                vec![Value::Text(r)],
            ))
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
    }
}

async fn routed(control: &Database, dir: &Path, router: &DbRouter) -> VectorService {
    let data = DataService::new(Arc::new(control.clone()), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    let svc = VectorService::open(data, layout(dir).indexes_dir(), VectorConfig::default())
        .await
        .unwrap();
    svc.attach_router(router.clone()).await.unwrap();
    svc
}

fn v3(id: &str, v: [f32; 3]) -> UpsertItem {
    item(id, "v", &v, json!({"id": id}))
}

async fn found(svc: &VectorService, ctx: &forgebase_data::AccessContext, q: [f32; 3], k: u32) -> Vec<String> {
    let mut got = ids(&svc.search(ctx, "docs", query("v", &q, k)).await.unwrap());
    got.sort();
    got
}

#[tokio::test]
async fn one_set_router_changes_nothing() {
    let h = harness().await;
    h.svc.attach_router(DbRouter::single(h.db.clone())).await.unwrap();
    let again = h.svc.attach_router(DbRouter::single(h.db.clone())).await.unwrap_err();
    assert_eq!(again.code(), "router_attached");

    let a = tenant("t1");
    create(&h.svc, &a, "docs", vec![field("v", 3, Metric::Cosine)], None).await;
    create(&h.svc, &admin(), "shared", vec![field("v", 3, Metric::L2)], None).await;
    h.svc
        .upsert(&a, "docs", vec![v3("x", [1.0, 0.0, 0.0]), v3("y", [0.0, 1.0, 0.0])])
        .await
        .unwrap();
    let r = h.svc.search(&a, "docs", query("v", &[1.0, 0.1, 0.0], 2)).await.unwrap();
    assert_eq!(ids(&r), ["x", "y"]);
    assert_eq!(h.svc.get_item(&a, "docs", "y").await.unwrap().metadata["id"], "y");
    assert!(matches!(
        h.svc.get_collection(&tenant("t2"), "docs").await,
        Err(VectorError::CollectionNotFound(_))
    ));
    let names: Vec<String> = h
        .svc
        .list_collections(&admin())
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["shared"]);
    assert_eq!(h.svc.delete_items(&a, "docs", vec!["x".into()]).await.unwrap(), 1);
    assert_eq!(h.svc.stats(&a, "docs").await.unwrap().items, 1);
    h.svc.add_field(&a, "docs", field("w", 2, Metric::Dot)).await.unwrap();
    h.svc.delete_field(&a, "docs", "w").await.unwrap();
    let engine = h.svc.storage_engine().await;
    let engines = h.svc.storage_engines().await;
    assert_eq!(engines.len(), 1);
    assert_eq!(engines["control"], engine);
    if h.db.kind() == forgebase_db::BackendKind::Postgres {
        let report = h.svc.convert(&admin(), engine).await.unwrap();
        assert!(report.sets.is_empty(), "one set: no per-set entries");
        assert_eq!(report.vectors, 0);
    }
    assert_eq!(h.svc.tenant_stats(&a).await.unwrap().collections, 1);

    // A restart with the router from the start.
    h.svc.shutdown().await;
    let data = DataService::new(Arc::new(h.db.clone()), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    let svc = VectorService::open_routed(
        data,
        DbRouter::single(h.db.clone()),
        layout(h.dir.path()).indexes_dir(),
        VectorConfig::default(),
    )
    .await
    .unwrap();
    wait_ready(&svc, &a, "docs").await;
    let r = svc.search(&a, "docs", query("v", &[0.0, 1.0, 0.0], 5)).await.unwrap();
    assert_eq!(ids(&r), ["y"]);
    svc.delete_collection(&a, "docs").await.unwrap();
    assert!(svc.list_collections(&a).await.unwrap().is_empty());
    svc.shutdown().await;
}

#[tokio::test]
async fn sets_hold_their_tenants_vectors() {
    let Some(mut sh) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    // Both sets get the server's extension setup; the shard is converted
    // to the portable engine below, so the two sets differ (when the engine
    // has pgvector).
    setup_extensions(&sh.control, ExtensionMode::Auto).await;
    let (set, s1) = sh.provision("s1").await;
    shards::activate_shard(
        &sh.router,
        &set,
        &s1,
        shards::ShardSetup {
            backups_dir: None,
            replay: None,
            control_conninfo: None,
        },
    )
    .await
    .unwrap();
    setup_extensions(&s1, ExtensionMode::Auto).await;
    let control = sh.control.clone();
    let router = DbRouter::open(
        control.clone(),
        RouterOptions {
            connector: Some(sh.connector.clone()),
            freeze_timeout: Duration::from_millis(300),
            ..RouterOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(router.is_sharded());
    // tenant-a has no placement row: control.
    router.place("tenant-b", &set.id, PlacementState::Active).await.unwrap();
    router.place("tenant-c", &set.id, PlacementState::Active).await.unwrap();
    let pgvector = pgvector_installed(&control).await;
    eprintln!("shards: pgvector on control: {pgvector}");
    let dir = tempfile::tempdir().unwrap();
    layout(dir.path()).ensure().unwrap();
    let svc = routed(&control, dir.path(), &router).await;

    // --- One engine per set: the shard alone is converted to portable.
    let report = svc
        .convert_set(&admin(), Some(&set.id), VectorEngine::Portable)
        .await
        .unwrap();
    assert_eq!(report.sets.len(), 1);
    assert_eq!(report.sets[0].set, set.id);
    let engines = svc.storage_engines().await;
    let control_engine = if pgvector {
        VectorEngine::Pgvector
    } else {
        VectorEngine::Portable
    };
    assert_eq!(engines["control"], control_engine);
    assert_eq!(engines[&set.id], VectorEngine::Portable);
    assert_eq!(svc.storage_engine().await, control_engine);
    assert_eq!(pin(&s1).await.as_deref(), Some("portable"));
    if pgvector {
        assert_eq!(pin(&control).await.as_deref(), Some("extension"));
    }

    // --- Rows land on the tenant's set; the shared scope on control.
    let (a, b, c) = (tenant("tenant-a"), tenant("tenant-b"), tenant("tenant-c"));
    for ctx in [&a, &b, &c] {
        create(&svc, ctx, "docs", vec![field("v", 3, Metric::Cosine)], None).await;
    }
    create(&svc, &admin(), "shared", vec![field("v", 3, Metric::Cosine)], None).await;
    svc.upsert(&a, "docs", vec![v3("a1", [1.0, 0.0, 0.0]), v3("a2", [0.0, 1.0, 0.0])])
        .await
        .unwrap();
    svc.upsert(&b, "docs", vec![v3("b1", [1.0, 0.0, 0.0]), v3("b2", [0.0, 0.0, 1.0])])
        .await
        .unwrap();
    svc.upsert(&c, "docs", vec![v3("c1", [1.0, 0.0, 0.0]), v3("c2", [0.0, 1.0, 1.0])])
        .await
        .unwrap();
    svc.upsert(&admin(), "shared", vec![v3("s1", [1.0, 1.0, 0.0])])
        .await
        .unwrap();
    let tenants = "SELECT tenant FROM _forgebase_vector_collections ORDER BY tenant";
    assert_eq!(texts(&control, tenants).await, ["", "tenant-a"]);
    assert_eq!(texts(&s1, tenants).await, ["tenant-b", "tenant-c"]);
    let items = "SELECT id FROM _forgebase_vector_items ORDER BY id";
    assert_eq!(texts(&control, items).await, ["a1", "a2", "s1"]);
    assert_eq!(texts(&s1, items).await, ["b1", "b2", "c1", "c2"]);
    let (a_cid, b_cid, c_cid) = (
        cid(&control, "tenant-a", "docs").await,
        cid(&s1, "tenant-b", "docs").await,
        cid(&s1, "tenant-c", "docs").await,
    );
    let shared_cid = cid(&control, "", "shared").await;
    assert!(
        b_cid >= 1 << 40 && c_cid >= 1 << 40,
        "the shard allocates in its own range"
    );
    assert!(a_cid < 1 << 40);

    // --- Reads stay in the tenant's set.
    assert_eq!(found(&svc, &a, [1.0, 0.1, 0.0], 5).await, ["a1", "a2"]);
    assert_eq!(found(&svc, &b, [1.0, 0.1, 0.0], 5).await, ["b1", "b2"]);
    assert!(matches!(
        svc.get_item(&a, "docs", "b1").await,
        Err(VectorError::ItemNotFound(_))
    ));
    assert_eq!(svc.get_item(&b, "docs", "b1").await.unwrap().id, "b1");
    assert_eq!(svc.get_collection(&b, "docs").await.unwrap().items, 2);
    let listed: Vec<String> = svc
        .list_collections(&admin())
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(listed, ["shared"]);
    let hybrid = svc
        .hybrid(
            &b,
            "docs",
            HybridRequest {
                vector: Some(vec![0.0, 0.0, 1.0]),
                ..serde_json::from_value(json!({})).unwrap()
            },
        )
        .await
        .unwrap();
    assert_eq!(hybrid.hits[0].id, "b2");
    assert_eq!(svc.tenant_stats(&b).await.unwrap().collections, 1);

    // --- Each set stores in its own engine.
    assert_eq!(in_column(&s1, b_cid, "vector").await, 2);
    assert_eq!(svc.stats(&b, "docs").await.unwrap().indexes[0].engine, "usearch");
    if pgvector {
        assert_eq!(in_column(&control, a_cid, "vec").await, 2);
        assert_eq!(in_column(&s1, b_cid, "vec").await, 0);
        assert_eq!(svc.stats(&a, "docs").await.unwrap().indexes[0].engine, "pgvector");
        assert!(
            scalar(
                &control,
                &format!("SELECT COUNT(*) FROM pg_indexes WHERE indexname LIKE 'fbv\\_{a_cid}\\_%'")
            )
            .await
                == 1
        );
        assert_eq!(
            scalar(&s1, "SELECT COUNT(*) FROM pg_indexes WHERE indexname LIKE 'fbv\\_%'").await,
            0
        );
    }

    // --- A tenant being moved: still served, no schema changes.
    router.place("tenant-c", &set.id, PlacementState::Moving).await.unwrap();
    let new = |name: &str| NewCollection {
        name: name.into(),
        description: String::new(),
        fields: vec![field("v", 3, Metric::Cosine)],
        exact_threshold: None,
        source_collection: None,
    };
    assert!(moving(&svc.create_collection(&c, new("more")).await.unwrap_err()));
    assert!(moving(
        &svc.add_field(&c, "docs", field("w", 2, Metric::L2)).await.unwrap_err()
    ));
    assert!(moving(&svc.delete_field(&c, "docs", "v").await.unwrap_err()));
    assert!(moving(&svc.delete_collection(&c, "docs").await.unwrap_err()));
    svc.upsert(&c, "docs", vec![v3("c3", [0.0, 0.0, 1.0])]).await.unwrap();
    assert_eq!(found(&svc, &c, [1.0, 0.0, 0.0], 5).await, ["c1", "c2", "c3"]);
    // Another tenant is not held up.
    svc.create_collection(&b, new("more")).await.unwrap();
    svc.delete_collection(&b, "more").await.unwrap();

    // --- Restart: every set's in-process indexes come back from their
    // files; the warm-up runs per set.
    svc.shutdown().await;
    let svc = routed(&control, dir.path(), &router).await;
    svc.start_background();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let st = svc.stats(&b, "docs").await.unwrap();
        if st.indexes[0].state == IndexState::Ready {
            assert_eq!(st.indexes[0].indexed_vectors, 2);
            assert!(
                st.indexes[0].last_rebuild.is_none(),
                "loaded from its file, not rebuilt"
            );
            break;
        }
        assert!(Instant::now() < deadline, "the warm-up did not load the shard's index");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    wait_ready(&svc, &c, "docs").await;
    assert!(svc.stats(&c, "docs").await.unwrap().indexes[0].last_rebuild.is_none());
    // tenant-c is read from its source (three items), not from the copy.
    assert_eq!(svc.get_collection(&c, "docs").await.unwrap().items, 3);
    assert_eq!(found(&svc, &b, [0.0, 0.0, 1.0], 1).await, ["b2"]);
    // tenant-c is copied to control meanwhile (control is the move's target).
    copy_tenant(&s1, &control, "tenant-c").await;

    // --- A frozen tenant waits, then 409; its set is not converted meanwhile.
    router.place("tenant-b", &set.id, PlacementState::Frozen).await.unwrap();
    let t = Instant::now();
    let e = svc
        .search(&b, "docs", query("v", &[1.0, 0.0, 0.0], 1))
        .await
        .unwrap_err();
    assert!(moving(&e), "{e:?}");
    assert!(t.elapsed() >= Duration::from_millis(250), "waited for the freeze");
    assert!(moving(
        &svc.convert_set(&admin(), Some(&set.id), VectorEngine::Portable)
            .await
            .unwrap_err()
    ));
    router.place("tenant-b", &set.id, PlacementState::Active).await.unwrap();
    assert_eq!(found(&svc, &b, [1.0, 0.0, 0.0], 1).await, ["b1"]);

    // --- A move's flip (tenant-a: control -> s1): the collections follow.
    copy_tenant(&control, &s1, "tenant-a").await;
    router
        .place("tenant-a", &set.id, PlacementState::Cleanup)
        .await
        .unwrap();
    assert_eq!(found(&svc, &a, [1.0, 0.1, 0.0], 5).await, ["a1", "a2"]);
    svc.upsert(&a, "docs", vec![v3("a3", [0.0, 0.0, 1.0])]).await.unwrap();
    let count = |cid: i64| format!("SELECT COUNT(*) FROM _forgebase_vector_items WHERE collection_id = {cid}");
    assert_eq!(scalar(&s1, &count(a_cid)).await, 3, "writes go to the new set");
    assert_eq!(scalar(&control, &count(a_cid)).await, 2, "the stale copy is left alone");
    assert_eq!(
        in_column(&s1, a_cid, "vector").await,
        3,
        "converted to the shard's engine"
    );
    if pgvector {
        assert_eq!(in_column(&s1, a_cid, "vec").await, 0);
    }
    assert_eq!(svc.stats(&a, "docs").await.unwrap().indexes[0].engine, "usearch");
    assert_eq!(found(&svc, &a, [0.0, 0.0, 1.0], 1).await, ["a3"]);

    // --- Conversions skip the partial copy of a tenant moving in (c) and
    // the stale copy of a tenant moved out (a).
    if pgvector {
        let r = svc
            .convert_set(&admin(), Some("control"), VectorEngine::Portable)
            .await
            .unwrap();
        assert_eq!(r.sets[0].vectors, 1, "only the shared collection's vector moved");
        assert_eq!(in_column(&control, shared_cid, "vector").await, 1);
        assert_eq!(in_column(&control, a_cid, "vec").await, 2, "stale copy untouched");
        let r = svc
            .convert_set(&admin(), Some("control"), VectorEngine::Pgvector)
            .await
            .unwrap();
        assert_eq!(r.sets[0].vectors, 1);
        assert_eq!(in_column(&control, shared_cid, "vec").await, 1);
        assert_eq!(in_column(&control, c_cid, "vector").await, 3, "partial copy untouched");
        assert_eq!(svc.storage_engines().await["control"], VectorEngine::Pgvector);
    }
    let shared = svc
        .search(&admin(), "shared", query("v", &[1.0, 1.0, 0.0], 1))
        .await
        .unwrap();
    assert_eq!(ids(&shared), ["s1"]);

    // --- Opened with the router from the start, control holds only what
    // it owns (tenant-a is read from s1) and its start-up reconcile skips
    // the partial copy of tenant-c.
    svc.shutdown().await;
    let data = DataService::new(Arc::new(control.clone()), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    let svc = VectorService::open_routed(
        data,
        router.clone(),
        layout(dir.path()).indexes_dir(),
        VectorConfig::default(),
    )
    .await
    .unwrap();
    if pgvector {
        assert_eq!(
            in_column(&control, c_cid, "vector").await,
            3,
            "partial copy untouched at start-up"
        );
        assert_eq!(in_column(&control, c_cid, "vec").await, 0);
    }
    assert_eq!(svc.get_collection(&a, "docs").await.unwrap().items, 3);
    assert_eq!(svc.get_collection(&c, "docs").await.unwrap().items, 3);
    assert_eq!(found(&svc, &b, [1.0, 0.0, 0.0], 5).await, ["b1", "b2"]);
    assert_eq!(svc.storage_engines().await.len(), 2);
    svc.shutdown().await;
}
