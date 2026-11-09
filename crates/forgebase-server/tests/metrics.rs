//! `GET /metrics` through the real server: the process, database, job,
//! storage, vector, key-value and queue series the dashboard overview reads
//! (FIX-6). Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;

use forgebase_core::Config;
use forgebase_server::start;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn get(addr: SocketAddr, path: &str) -> (u16, String, String) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!("GET {path} HTTP/1.1\r\nHost: forgebase\r\nConnection: close\r\n\r\n");
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
    (status, head.to_owned(), body.to_owned())
}

#[tokio::test]
async fn metrics_report_process_database_and_subsystem_series() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::defaults(tmp.path());
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    let server = start(&cfg).await.unwrap();
    let addr = server.local_addr();

    // One ordinary request first, so the HTTP series exist too.
    let (status, ..) = get(addr, "/healthz").await;
    assert!(status == 200 || status == 404, "{status}");
    let (status, head, text) = get(addr, "/metrics").await;
    assert_eq!(status, 200, "{text}");
    assert!(
        head.to_ascii_lowercase().contains("text/plain; version=0.0.4"),
        "{head}"
    );

    for name in [
        "process_resident_memory_bytes",
        "process_cpu_seconds_total",
        "forgebase_db_query_duration_seconds_bucket{mode=\"read\"",
        "forgebase_db_query_duration_seconds_bucket{mode=\"write\"",
        "forgebase_db_query_duration_seconds_count",
        "forgebase_db_pool_connections_in_use{pool=\"writer\"}",
        "forgebase_db_pool_connections_in_use{pool=\"reader\"}",
        "forgebase_db_pool_connections_idle{pool=\"writer\"}",
        "forgebase_db_pool_connections_idle{pool=\"reader\"}",
        "forgebase_jobs{status=\"queued\"}",
        "forgebase_jobs{status=\"running\"}",
        "forgebase_jobs{status=\"succeeded\"}",
        "forgebase_jobs{status=\"dead\"}",
        "forgebase_jobs{status=\"cancelled\"}",
        "forgebase_storage_bytes{kind=\"logical\"}",
        "forgebase_storage_bytes{kind=\"stored\"}",
        "forgebase_storage_objects{kind=\"current\"}",
        "forgebase_storage_objects{kind=\"versions\"}",
        "forgebase_vector_indexes{state=\"ready\"}",
        "forgebase_vector_indexes{state=\"building\"}",
        "forgebase_vector_indexed_vectors",
        "forgebase_vector_index_memory_bytes",
        "forgebase_kv_keys",
        "forgebase_kv_memory_bytes",
        "forgebase_queue_deliveries{state=\"ready\"}",
        "forgebase_queue_deliveries{state=\"delayed\"}",
        "forgebase_queue_deliveries{state=\"in_flight\"}",
        "forgebase_queue_messages",
        // Existing names are unchanged.
        "forgebase_http_requests_total",
        "forgebase_http_request_duration_seconds_bucket",
        "forgebase_db_size_bytes",
        "forgebase_uptime_seconds",
    ] {
        assert!(text.contains(name), "missing {name} in:\n{text}");
    }
    assert!(text.contains("# TYPE process_cpu_seconds_total counter"), "{text}");
    assert!(
        text.contains("# TYPE forgebase_db_query_duration_seconds histogram"),
        "{text}"
    );

    // The writer pool has one connection: in use + idle never exceeds it.
    let writer_in_use = value(&text, "forgebase_db_pool_connections_in_use{pool=\"writer\"}");
    let writer_idle = value(&text, "forgebase_db_pool_connections_idle{pool=\"writer\"}");
    assert!(writer_in_use + writer_idle <= 1.0, "{writer_in_use} + {writer_idle}");
    assert!(value(&text, "process_resident_memory_bytes") > 0.0);

    server.shutdown().await.unwrap();
}

fn value(text: &str, series: &str) -> f64 {
    text.lines()
        .find_map(|l| l.strip_prefix(series).and_then(|r| r.trim().parse().ok()))
        .unwrap_or_else(|| panic!("no series {series}"))
}
