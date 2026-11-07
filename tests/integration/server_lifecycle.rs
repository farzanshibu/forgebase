//! In-process server lifecycle: start on a random port, check the Phase 1
//! endpoints, write through the database layer, restart on the same data
//! directory and read the row back.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::{file_len, get_json, http};
use forgebase_core::{Config, DataLayout};

fn config(dir: &std::path::Path) -> Config {
    let mut c = Config::defaults(dir);
    c.server.port = 0;
    c.server.shutdown_timeout_secs = 5;
    c
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_write_restart_and_read_back() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let layout = DataLayout::new(&cfg.data_dir);

    // First start on a fresh directory creates and migrates everything.
    let server = forgebase_server::start(&cfg).await.unwrap();
    assert_eq!(
        server.startup_migrations().applied.len(),
        forgebase_db::migrations::SYSTEM_MIGRATIONS.len()
    );
    let addr = server.local_addr().to_string();
    assert_ne!(server.local_addr().port(), 0, "random port was bound");

    let (s, health) = get_json(&addr, "/health");
    assert_eq!((s, health), (200, serde_json::json!({"status": "ok"})));
    let (s, ready) = get_json(&addr, "/health/ready");
    assert_eq!(s, 200);
    assert_eq!(
        ready,
        serde_json::json!({"status": "ready", "checks": {"database": "ok", "storage": "ok"}})
    );
    let (s, info) = get_json(&addr, "/api/system/info");
    assert_eq!(s, 200);
    assert_eq!(
        info["database"]["schema_version"],
        forgebase_db::migrations::Migrator::system().latest_version()
    );
    assert_eq!(info["database"]["journal_mode"], "wal");
    assert_eq!(info["data_dir"], cfg.data_dir.display().to_string());
    let (s, _, metrics) = http("GET", &addr, "/metrics").unwrap();
    assert_eq!(s, 200);
    assert!(metrics.contains("forgebase_http_requests_total"));

    // Write through the database layer while serving.
    let instance = server.instance().clone();
    server
        .database()
        .meta_set("phase1.persistence", "still here")
        .await
        .unwrap();

    // A second server on the same data directory is refused.
    let err = forgebase_server::start(&cfg).await.unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&err), "data_dir_locked", "{err}");

    // Graceful shutdown releases the port, checkpoints the WAL and the lock.
    server.shutdown().await.unwrap();
    assert!(http("GET", &addr, "/health").is_err(), "listener closed");
    assert_eq!(
        file_len(&layout.database_wal_path()),
        0,
        "WAL checkpointed into the main file"
    );

    // Restart: nothing to migrate, same instance, row still there.
    let server = forgebase_server::start(&cfg).await.unwrap();
    assert!(
        server.startup_migrations().applied.is_empty(),
        "migrations are idempotent across restarts"
    );
    assert_eq!(server.instance(), &instance);
    assert_eq!(
        server
            .database()
            .meta_get("phase1.persistence")
            .await
            .unwrap()
            .as_deref(),
        Some("still here")
    );
    let addr = server.local_addr().to_string();
    let (s, info) = get_json(&addr, "/api/system/info");
    assert_eq!(s, 200);
    assert_eq!(
        info["database"]["schema_version"],
        forgebase_db::migrations::Migrator::system().latest_version()
    );
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn port_conflict_is_reported_and_releases_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let mut cfg = config(tmp.path());
    cfg.server.port = blocker.local_addr().unwrap().port();
    let err = forgebase_server::start(&cfg).await.unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&err), "port_in_use", "{err}");

    // The failed start must not leave the data directory locked.
    drop(blocker);
    cfg.server.port = 0;
    let server = forgebase_server::start(&cfg).await.unwrap();
    server.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_requests_while_writing() {
    let tmp = tempfile::tempdir().unwrap();
    let server = forgebase_server::start(&config(tmp.path())).await.unwrap();
    let addr = server.local_addr().to_string();
    let db = server.database().clone();
    let writer = tokio::spawn(async move {
        for i in 0..100 {
            db.meta_set(&format!("load-{i}"), "x").await.unwrap();
        }
    });
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let addr = addr.clone();
            tokio::task::spawn_blocking(move || {
                for _ in 0..25 {
                    let (s, info) = get_json(&addr, "/api/system/info");
                    assert_eq!(s, 200);
                    assert_eq!(info["database"]["journal_mode"], "wal");
                }
            })
        })
        .collect();
    writer.await.unwrap();
    for r in readers {
        r.await.unwrap();
    }
    assert_eq!(server.database().meta_all().await.unwrap().len(), 100 + 2);
    tokio::time::timeout(Duration::from_secs(10), server.shutdown())
        .await
        .expect("shutdown finished")
        .unwrap();
}

/// The Phase 2 data API over a real socket: documented by OpenAPI, and denied
/// to anonymous callers until the auth layer (FB-20) is mounted.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn data_api_is_documented_and_denies_anonymous_requests() {
    let tmp = tempfile::tempdir().unwrap();
    let server = forgebase_server::start(&config(tmp.path())).await.unwrap();
    let addr = server.local_addr().to_string();

    let (s, doc) = get_json(&addr, "/api/openapi.json");
    assert_eq!(s, 200);
    assert!(doc["paths"]["/api/v1/collections/{collection}/records"]["get"].is_object());

    for (method, path) in [
        ("GET", "/api/v1/collections"),
        ("GET", "/api/v1/collections/anything/records"),
        ("GET", "/api/v1/sql/tables"),
        ("GET", "/api/v1/indexes"),
    ] {
        let (status, _, body) = http(method, &addr, path).unwrap();
        let j: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(status, 401, "{method} {path}: {body}");
        assert_eq!(j["error"]["code"], "unauthenticated");
    }
    server.shutdown().await.unwrap();
}
