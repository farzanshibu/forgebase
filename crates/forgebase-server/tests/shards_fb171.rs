//! FB-171: the server on a sharded topology (ADR-0029 §3.5). The server
//! builds the router from the control database, opens the shard through
//! its sealed URL ([`forgebase_server::shards::NodeConnector`]), migrates
//! every set, and runs the per-set loops: realtime dispatchers, the
//! after-hook cursor per set, two-phase recovery and the cross-set
//! `ON DELETE` fan-out job. Real, separate PostgreSQL clusters
//! (`forgebase_db::shard_testing`); the sharded test skips without an
//! engine. The one-set test runs on the default backend.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::time::Duration;

use forgebase_core::Config;
use forgebase_core::config::{DatabaseBackend, DatabaseUrl, PostgresMode};
use forgebase_data::AccessContext;
use forgebase_db::{Backend, Database, Statement, Value};
use serde_json::json;

fn base_config(dir: &Path) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg
}

async fn scalar(db: &Database, sql: &str) -> i64 {
    db.fetch_all(&Statement::new(sql, vec![]))
        .await
        .unwrap()
        .scalar()
        .and_then(Value::as_i64)
        .unwrap_or(-1)
}

#[tokio::test]
async fn one_set_server_routes_everything_to_control() {
    if std::env::var("FORGEBASE_TEST_BACKEND").as_deref() == Ok("postgres") {
        eprintln!("skipped: the one-set case runs on SQLite (PostgreSQL: the sharded test)");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = base_config(dir.path());
    cfg.database.backend = DatabaseBackend::Sqlite;
    let server = forgebase_server::start(&cfg).await.unwrap();
    assert!(!server.router().is_sharded());
    assert_eq!(server.router().sets().len(), 1);
    assert_eq!(server.realtime().heads().keys().copied().collect::<Vec<_>>(), vec![0]);
    server.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn sharded_server_opens_every_set_and_runs_per_set_loops() {
    use forgebase_db::shard_testing::Sharded;
    use forgebase_db::shards::{self, ShardSetup};
    use forgebase_db::topology::PlacementState;

    let Some(mut s) = Sharded::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let (set, shard) = s.provision("s1").await;
    shards::activate_shard(
        &s.router,
        &set,
        &shard,
        ShardSetup {
            backups_dir: None,
            replay: None,
            control_conninfo: None,
        },
    )
    .await
    .unwrap();
    // The node's URL, sealed with the data directory's master key.
    let dir = tempfile::Builder::new()
        .prefix("fb171-")
        .tempdir_in(std::env::var_os("FORGEBASE_TEST_PG_TMP").unwrap_or_else(|| std::env::temp_dir().into()))
        .unwrap();
    let key = forgebase_auth::AuthConfig::for_data_dir(dir.path().join("data")).master_key_path;
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    let keyring = forgebase_auth::crypto::Keyring::load_or_create(&key).unwrap();
    let node = s.router.topology().nodes_of(&set.id).next().unwrap().clone();
    let sealed = forgebase_server::shards::seal_node_url(&keyring, &node.id, &s.clusters[1].url_of("forgebase")).unwrap();
    let mut tx = s.control.begin(forgebase_db::TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(
        "UPDATE _forgebase_db_nodes SET url_sealed = ?1 WHERE id = ?2",
        vec![sealed.into(), node.id.clone().into()],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let mut cfg = base_config(&dir.path().join("data"));
    cfg.data_dir = dir.path().join("data");
    cfg.database.backend = DatabaseBackend::Postgres;
    cfg.database.postgres.mode = PostgresMode::External;
    cfg.database.postgres.url = Some(DatabaseUrl::new(s.clusters[0].url_of("forgebase")));
    let server = forgebase_server::start(&cfg).await.unwrap();
    let router = server.router().clone();
    assert!(router.is_sharded(), "{:?}", router.sets());
    assert!(router.set(&set.id).is_some());
    assert!(dir.path().join("data").join("topology.json").exists());
    // One realtime dispatcher per set.
    let heads = server.realtime().heads();
    assert!(heads.contains_key(&0) && heads.contains_key(&set.number), "{heads:?}");
    // The after-hook cursor is per set (functions are on by default).
    assert_eq!(
        scalar(&shard, "SELECT count(*) FROM _forgebase_function_cursors WHERE name = 'after_hooks'").await,
        1
    );

    // Records of a tenant on s1 go to s1; its realtime head moves there.
    let data = server.realtime().data().clone();
    let sys = AccessContext::system();
    for def in [
        json!({"id": "users_c", "name": "users", "fields": [{"name": "name", "type": "text"}]}),
        json!({"name": "posts", "options": {"tenant_scoped": true}, "fields": [
            {"name": "title", "type": "text"},
            {"name": "author", "type": "relation", "collection_id": "users_c"}]}),
    ] {
        data.create_collection(&sys, serde_json::from_value(def).unwrap())
            .await
            .unwrap();
    }
    router.place("t1", &set.id, PlacementState::Active).await.unwrap();
    let admin = AccessContext::admin("root");
    let u = data.create_record(&admin, "users", json!({"name": "ann"})).await.unwrap();
    let uid = u["id"].as_str().unwrap().to_owned();
    data.create_record(&admin.clone().with_tenant("t1"), "posts", json!({"title": "x", "author": uid}))
        .await
        .unwrap();
    assert_eq!(scalar(&shard, "SELECT count(*) FROM posts").await, 1);
    let mut moved = false;
    for _ in 0..100 {
        if server.realtime().heads().get(&set.number).copied().unwrap_or(0) > 0 {
            moved = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(moved, "s1's dispatcher did not publish: {:?}", server.realtime().heads());

    // Deleting the shared user runs ON DELETE on s1 through the job.
    data.delete_record(&admin, "users", &uid, None).await.unwrap();
    let mut cleared = false;
    for _ in 0..200 {
        if scalar(&shard, "SELECT count(*) FROM posts WHERE author IS NULL").await == 1 {
            cleared = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(cleared, "the relation fan-out job did not run on s1");
    assert!(
        scalar(
            &s.control,
            "SELECT count(*) FROM _forgebase_jobs WHERE kind = 'data.relation_deletes' AND status = 'succeeded'"
        )
        .await
            >= 1
    );
    server.shutdown().await.unwrap();
}
