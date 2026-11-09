//! The vector engine through the real server lifecycle: mounted under
//! `/api/v1/vector`, indexes saved at shutdown and loaded at the next start,
//! rebuilt when the derived files are deleted (SPEC §27 #12, #21, #23).
//! These tests bind a local port (`127.0.0.1:0`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use forgebase_core::Config;
use forgebase_data::AccessContext;
use forgebase_server::{RunningServer, start};
use forgebase_vector::model::{FieldSpec, Metric, NewCollection, SearchRequest, UpsertItem};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn config(dir: &Path) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg
}

async fn http(addr: SocketAddr, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: forgebase\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, out.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned())
}

fn search(v: [f64; 3]) -> SearchRequest {
    serde_json::from_value(serde_json::json!({"vector": v, "k": 2})).unwrap()
}

async fn top(server: &RunningServer, v: [f64; 3]) -> Vec<String> {
    let r = server
        .vector()
        .search(&AccessContext::system(), "kb", search(v))
        .await
        .unwrap();
    r.hits.into_iter().map(|h| h.id).collect()
}

#[tokio::test]
async fn vectors_persist_and_indexes_reload_or_rebuild_across_restarts() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let sys = AccessContext::system();

    let server = start(&cfg).await.unwrap();
    let (status, body) = http(server.local_addr(), "GET", "/api/v1/vector/collections", "").await;
    assert_eq!(status, 401, "anonymous is denied: {body}");
    assert!(body.contains("unauthenticated"));
    server
        .vector()
        .create_collection(
            &sys,
            NewCollection {
                name: "kb".into(),
                description: String::new(),
                fields: vec![FieldSpec {
                    name: "emb".into(),
                    dimension: 3,
                    metric: Metric::L2,
                    quantization: None,
                    connectivity: None,
                    expansion_add: None,
                    expansion_search: None,
                }],
                exact_threshold: Some(0),
            },
        )
        .await
        .unwrap();
    let items = (0..200)
        .map(|i| UpsertItem {
            id: Some(format!("p{i}")),
            vectors: [("emb".to_owned(), vec![f64::from(i), 0.0, 0.0])].into(),
            metadata: None,
            text: None,
        })
        .collect();
    server.vector().upsert(&sys, "kb", items).await.unwrap();
    assert_eq!(top(&server, [41.2, 0.0, 0.0]).await, ["p41", "p42"]);
    server.shutdown().await.unwrap();

    let files = cfg.data_dir.join("indexes/vector");
    let count_files = || {
        std::fs::read_dir(&files)
            .unwrap()
            .flat_map(|d| std::fs::read_dir(d.unwrap().path()).unwrap())
            .count()
    };
    assert_eq!(count_files(), 2, "index file and manifest written at shutdown");

    // Restart: data is still there, the index is loaded rather than rebuilt.
    let server = start(&cfg).await.unwrap();
    assert!(
        server
            .vector()
            .wait_ready(&sys, "kb", Duration::from_secs(30))
            .await
            .unwrap()
    );
    let stats = server.vector().stats(&sys, "kb").await.unwrap();
    assert!(stats.indexes[0].last_rebuild.is_none());
    assert_eq!(stats.indexes[0].indexed_vectors, 200);
    assert_eq!(top(&server, [41.2, 0.0, 0.0]).await, ["p41", "p42"]);
    server.shutdown().await.unwrap();

    // Derived files deleted: rebuilt from SQLite on start, same answers.
    std::fs::remove_dir_all(&files).unwrap();
    let server = start(&cfg).await.unwrap();
    assert!(
        server
            .vector()
            .wait_ready(&sys, "kb", Duration::from_secs(30))
            .await
            .unwrap()
    );
    let stats = server.vector().stats(&sys, "kb").await.unwrap();
    assert_eq!(stats.indexes[0].last_rebuild.as_ref().unwrap().reason, "missing");
    assert_eq!(top(&server, [41.2, 0.0, 0.0]).await, ["p41", "p42"]);
    let item = server.vector().get_item(&sys, "kb", "p7").await.unwrap();
    assert_eq!(item.vectors["emb"], vec![7.0, 0.0, 0.0]);
    server.shutdown().await.unwrap();
}
