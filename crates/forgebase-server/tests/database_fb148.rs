//! FB-148: the backend of a data directory and the bundled PostgreSQL engine
//! around `Database::open` (ADR-0028 §4.3, §6.3).
//!
//! The PostgreSQL tests run with `FORGEBASE_TEST_BACKEND=postgres` and an
//! engine in `FORGEBASE_PG_ENGINE_DIR` (or a `bundled-postgres` build), and
//! skip otherwise. Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::Path;

use forgebase_core::config::{DatabaseBackend, PostgresMode};
use forgebase_core::{Config, DataLayout, ErrorCode};
use forgebase_db::{Backend, Statement, Value};
use forgebase_server::database::{self, Access, BackendMarker, Create};
use forgebase_server::{DataDirLock, start};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn postgres_wanted() -> bool {
    std::env::var("FORGEBASE_TEST_BACKEND").as_deref() == Ok("postgres")
        && (forgebase_pgengine::HAS_EMBEDDED
            || std::env::var_os(forgebase_pgengine::ENGINE_DIR_ENV).is_some_and(|v| !v.is_empty()))
}

fn config(dir: &Path, backend: DatabaseBackend) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg.database.backend = backend;
    cfg.database.postgres.mode = PostgresMode::Managed;
    // Tests on an external SSD: durability off, as forgebase_db::testing does.
    for (k, v) in [
        ("fsync", "off"),
        ("synchronous_commit", "off"),
        ("full_page_writes", "off"),
    ] {
        cfg.database.postgres.settings.insert(k.into(), v.into());
    }
    cfg
}

fn tempdir() -> tempfile::TempDir {
    // Short paths: the engine's Unix socket path is limited to 103 bytes.
    let base = std::env::var_os("FORGEBASE_TEST_PG_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("fb148-").tempdir_in(base).unwrap()
}

async fn get(addr: SocketAddr, path: &str) -> (u16, serde_json::Value) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: forgebase\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = out.split_once("\r\n\r\n").map_or("", |(_, b)| b);
    // Chunked bodies: the JSON object sits between the chunk-size lines.
    let json = body
        .find('{')
        .and_then(|i| body.rfind('}').map(|j| &body[i..=j]))
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn scalar(db: &forgebase_db::Database, sql: &str) -> String {
    let rows = db.fetch_all(&Statement::raw(sql)).await.unwrap();
    match rows.scalar() {
        Some(Value::Text(t)) => t.clone(),
        other => format!("{other:?}"),
    }
}

/// The marker is written on the first start and a configuration that
/// contradicts it is refused before anything is opened (both builds; SQLite).
#[tokio::test]
async fn the_backend_marker_is_written_and_a_mismatch_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path(), DatabaseBackend::Sqlite);
    let layout = DataLayout::new(&cfg.data_dir);
    let server = start(&cfg).await.unwrap();
    assert_eq!(server.backend(), BackendMarker::Sqlite);
    assert!(server.engine().is_none());
    server.shutdown().await.unwrap();
    assert_eq!(
        std::fs::read_to_string(database::marker_path(&layout)).unwrap(),
        "sqlite\n"
    );

    let pg = config(tmp.path(), DatabaseBackend::Postgres);
    let err = start(&pg).await.unwrap_err();
    assert_eq!(err.code(), "backend_mismatch", "{err}");
    assert!(err.to_string().contains("forgebase db convert --to postgres"), "{err}");
    assert!(
        !database::engine_layout(&layout).pgdata().exists(),
        "no cluster next to the SQLite project"
    );
    // The lock was released: SQLite starts again.
    let server = start(&cfg).await.unwrap();
    server.shutdown().await.unwrap();
}

/// The lock holder starts the engine; other commands attach to it through
/// the socket with their own login; closing the owner stops the engine; a
/// command with no server running starts and stops it itself.
#[tokio::test]
async fn the_lock_holder_runs_the_engine_and_others_attach() {
    if !postgres_wanted() {
        eprintln!("skipped: FORGEBASE_TEST_BACKEND=postgres with an engine is needed");
        return;
    }
    let tmp = tempdir();
    let cfg = config(tmp.path(), DatabaseBackend::Postgres);
    let layout = DataLayout::new(&cfg.data_dir);
    layout.ensure().unwrap();
    assert!(matches!(
        database::open_attached(&cfg, true, &layout, Access::Read).await,
        Err(forgebase_server::ServerError::NotInitialized(_))
    ));

    let lock = DataDirLock::acquire(&layout).unwrap();
    let owned = database::open_owned(&cfg, true, &layout, Create::Yes).await.unwrap();
    assert_eq!(owned.marker(), BackendMarker::PostgresManaged);
    assert_eq!(
        database::read_marker(&layout).unwrap(),
        Some(BackendMarker::PostgresManaged)
    );
    let engine_pid = owned.engine().expect("engine").health().pid.expect("pid");
    owned.db().migrate(Some(&layout.backups_dir())).await.unwrap();
    assert_eq!(
        scalar(owned.db(), "SELECT current_user::text").await,
        "forgebase_server"
    );
    let pm = database::postmaster(&layout).expect("postmaster.pid");
    assert_eq!(pm.pid, engine_pid);
    assert_eq!(pm.status, "ready");

    for (access, role) in [
        (Access::Read, "forgebase_reader"),
        (Access::Write, "forgebase_server"),
        (Access::Super, "forgebase_super"),
    ] {
        let other = database::open_attached(&cfg, true, &layout, access).await.unwrap();
        assert!(other.engine().is_none(), "attached, not started");
        assert_eq!(other.attached().expect("attached").pid, engine_pid);
        assert_eq!(scalar(other.db(), "SELECT current_user::text").await, role);
        other.close().await;
    }
    assert!(
        database::postmaster(&layout).is_some(),
        "closing an attached handle keeps the engine"
    );

    owned.close().await;
    drop(lock);
    assert!(
        database::postmaster(&layout).is_none(),
        "the owner's close stops the engine"
    );

    // No server: the command takes the lock and runs the engine itself.
    let alone = database::open_attached(&cfg, true, &layout, Access::Read)
        .await
        .unwrap();
    assert!(alone.engine().is_some());
    assert!(DataDirLock::acquire(&layout).is_err(), "held while the command runs");
    let version = scalar(alone.db(), "SELECT max(version)::text FROM _forgebase_migrations").await;
    assert!(version.parse::<i64>().unwrap() > 0, "{version}");
    alone.close().await;
    assert!(database::postmaster(&layout).is_none());
    DataDirLock::acquire(&layout).unwrap();

    // An explicit SQLite configuration is refused on this directory.
    let sqlite = config(tmp.path(), DatabaseBackend::Sqlite);
    let err = start(&sqlite).await.unwrap_err();
    assert_eq!(err.code(), "backend_mismatch", "{err}");
}

/// The whole server on the bundled engine: info, readiness, shutdown stops
/// the engine. Needs the wave-2 ports of every service (FB-143 to FB-147):
/// on `main` before them the data core still asks `sqlite_master`.
#[tokio::test]
#[ignore = "needs FB-143..FB-147 merged: the services' SQL on PostgreSQL"]
async fn the_server_runs_on_the_bundled_engine() {
    if !postgres_wanted() {
        return;
    }
    let tmp = tempdir();
    let cfg = config(tmp.path(), DatabaseBackend::Postgres);
    let layout = DataLayout::new(&cfg.data_dir);
    let server = start(&cfg).await.unwrap();
    assert_eq!(server.backend(), BackendMarker::PostgresManaged);
    let addr = server.local_addr();
    let (s, info) = get(addr, "/api/system/info").await;
    assert_eq!(s, 200, "{info}");
    assert_eq!(info["database"]["backend"], "postgres");
    assert_eq!(info["database"]["mode"], "managed");
    assert_eq!(info["database"]["engine"]["state"], "ready");
    assert_eq!(info["database"]["features"].as_object().map(|m| m.len()), Some(5));
    let (s, ready) = get(addr, "/health/ready").await;
    assert_eq!(s, 200, "{ready}");
    assert_eq!(ready["checks"]["database"], "ok");
    server.shutdown().await.unwrap();
    assert!(database::postmaster(&layout).is_none());
}
