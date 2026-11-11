//! Shard primaries on this host (ADR-0029 §3.9, FB-174): created next to
//! the primary with its roles, restarted at server start, retired with
//! their data kept, deleted on request. Skips without
//! `FORGEBASE_PG_ENGINE_DIR`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use forgebase_pgengine::shard::{LocalShards, SHARD_MARKER};
use forgebase_pgengine::{Engine, EngineConfig, EngineSource, Layout, Role};
use sqlx::{Connection, PgConnection};

fn engine_dir() -> Option<PathBuf> {
    match std::env::var_os("FORGEBASE_PG_ENGINE_DIR").filter(|v| !v.is_empty()) {
        Some(d) => Some(PathBuf::from(d)),
        None => {
            eprintln!("skipped: FORGEBASE_PG_ENGINE_DIR is not set");
            None
        }
    }
}

fn tempdir() -> tempfile::TempDir {
    let base = std::env::var_os("FORGEBASE_PGENGINE_TEST_TMP")
        .or_else(|| std::env::var_os("FORGEBASE_TEST_PG_TMP"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&base).expect("test tmp base");
    tempfile::Builder::new().prefix("sh").tempdir_in(base).expect("tempdir")
}

fn config(dir: &Path) -> EngineConfig {
    EngineConfig {
        source: EngineSource::Dir(dir.to_path_buf()),
        socket_port: 18_430,
        unsafe_fast: true,
        ready_timeout: Duration::from_secs(60),
        stop_timeout: Duration::from_secs(30),
        ..EngineConfig::default()
    }
}

async fn setting(url: &str, name: &str) -> String {
    let mut c = PgConnection::connect(url).await.unwrap();
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT current_setting('{name}')")))
        .fetch_one(&mut c)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_local_shard_starts_with_the_primarys_roles_and_is_retired_with_its_data_kept() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let cfg = config(&engine);
    let primary = Engine::prepare(&Layout::primary(tmp.path()), &cfg).await.unwrap();
    let shards = LocalShards::new(tmp.path(), &cfg);

    let s1 = shards.add("shard-1", "set-1").await.unwrap();
    assert!(s1.tcp_addr().is_none(), "shards never listen on TCP");
    // The server reaches it with the primary's server role and password.
    assert_eq!(
        s1.url(Role::Server).expose().contains("forgebase_server"),
        primary.url(Role::Server).expose().contains("forgebase_server")
    );
    let url = s1.url(Role::Server).expose().to_owned();
    assert_eq!(setting(&url, "max_prepared_transactions").await, "16");
    assert_eq!(setting(&url, "wal_level").await, "logical");
    {
        let mut c = PgConnection::connect(s1.url(Role::Super).expose()).await.unwrap();
        sqlx::raw_sql("CREATE TABLE forgebase.fb174_keep (n int); INSERT INTO forgebase.fb174_keep VALUES (7)")
            .execute(&mut c)
            .await
            .unwrap();
    }
    assert!(shards.add("shard-1", "set-1").await.is_err(), "already running");
    assert_eq!(shards.on_disk(), vec![("shard-1".to_owned(), "set-1".to_owned())]);
    // Another set may not take the directory.
    shards.stop("shard-1").await.unwrap();
    assert!(shards.add("shard-1", "set-2").await.is_err());

    // Server start: shard nodes on disk come back with their data.
    let started = shards.start_existing().await;
    assert_eq!(started.len(), 1);
    assert!(started[0].2.is_ok(), "{:?}", started[0].2);
    let s1 = shards.get("shard-1").await.unwrap();
    let mut c = PgConnection::connect(s1.url(Role::Super).expose()).await.unwrap();
    let n: i32 = sqlx::query_scalar("SELECT n FROM forgebase.fb174_keep")
        .fetch_one(&mut c)
        .await
        .unwrap();
    assert_eq!(n, 7);
    drop(c);
    // A replica's directory is not a shard.
    std::fs::create_dir_all(tmp.path().join("postgres/nodes/rep-1/data")).unwrap();
    assert!(shards.start("rep-1").await.is_err());
    assert!(!tmp.path().join("postgres/nodes/rep-1").join(SHARD_MARKER).exists());

    // Remove: stopped, data kept under retired/, then deleted on request.
    let retired = shards.retire("shard-1").await.unwrap();
    assert!(shards.get("shard-1").await.is_none());
    assert!(retired.path.starts_with(tmp.path().join("postgres/retired")));
    assert!(retired.path.join("data/PG_VERSION").is_file());
    assert!(retired.bytes > 1_000_000);
    assert_eq!(shards.retired().len(), 1);
    assert!(shards.on_disk().is_empty());
    shards.delete_retired(&retired.id).unwrap();
    assert!(!retired.path.exists());
    shards.stop_all().await;
    primary.stop().await.unwrap();
}
