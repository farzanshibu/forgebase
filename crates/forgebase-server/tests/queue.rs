//! Topics and queues through the real server lifecycle: mounted under
//! `/api/v1/queues`, documented in OpenAPI, messages survive a restart, and
//! `[queue] enabled = false` turns the API off. Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::Path;

use forgebase_core::Config;
use forgebase_data::AccessContext;
use forgebase_queue::{NewMessage, NewSubscription, NewTopic, PullRequest};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn config(dir: &Path) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg
}

async fn get(addr: SocketAddr, path: &str) -> (u16, String) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: forgebase\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = out.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned();
    (status, body)
}

#[tokio::test]
async fn queues_are_served_and_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let sys = AccessContext::system();
    let server = forgebase_server::start(&config(dir.path())).await.unwrap();
    let q = server.queue().expect("queue service on by default");
    q.create_topic(
        &sys,
        NewTopic {
            name: "events".into(),
            ..NewTopic::default()
        },
    )
    .await
    .unwrap();
    q.create_subscription(
        &sys,
        NewSubscription {
            name: "audit".into(),
            topic: "events".into(),
            ..NewSubscription::default()
        },
    )
    .await
    .unwrap();
    q.publish(&sys, "events", vec![NewMessage::new(json!({"kept": true}))])
        .await
        .unwrap();
    let addr = server.local_addr();
    let (status, _) = get(addr, "/api/v1/queues/topics").await;
    assert_eq!(status, 401, "mounted and authenticated");
    let (status, doc) = get(addr, "/api/openapi.json").await;
    assert_eq!(status, 200);
    assert!(doc.contains("/api/v1/queues/subscriptions/{sub}/pull"));
    assert!(doc.contains("queues_publish"));
    server.shutdown().await.unwrap();

    let server = forgebase_server::start(&config(dir.path())).await.unwrap();
    let got = server
        .queue()
        .unwrap()
        .pull(&sys, "audit", PullRequest::default())
        .await
        .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].message.data, json!({"kept": true}));
    server.shutdown().await.unwrap();

    let mut off = config(dir.path());
    off.queue.enabled = false;
    let server = forgebase_server::start(&off).await.unwrap();
    assert!(server.queue().is_none());
    let (status, _) = get(server.local_addr(), "/api/v1/queues/topics").await;
    assert_eq!(status, 404);
    server.shutdown().await.unwrap();
}
