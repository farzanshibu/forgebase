//! The complete `GET /api/openapi.json` (every crate's fragment merged) has unique
//! operationIds, so generated clients get one function per operation.
//! Binds a local port (`127.0.0.1:0`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use forgebase_core::Config;
use forgebase_server::start;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn full_document_has_unique_operation_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::defaults(tmp.path());
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    let server = start(&cfg).await.unwrap();

    let mut s = tokio::net::TcpStream::connect(server.local_addr()).await.unwrap();
    s.write_all(b"GET /api/openapi.json HTTP/1.1\r\nHost: forgebase\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let body = out.split("\r\n\r\n").nth(1).unwrap();
    let doc: serde_json::Value = serde_json::from_str(body).unwrap_or_else(|e| panic!("{e}: {body}"));

    let mut seen = BTreeMap::new();
    let mut count = 0;
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            count += 1;
            let id = op["operationId"]
                .as_str()
                .unwrap_or_else(|| panic!("{method} {path} has no operationId"));
            if let Some(first) = seen.insert(id.to_owned(), format!("{method} {path}")) {
                panic!("duplicate operationId `{id}`: {first} and {method} {path}");
            }
        }
    }
    assert!(count > 200, "suspiciously small document: {count} operations");
    for p in [
        "/api/auth/login",
        "/api/auth/signup",
        "/api/v1/kv/{ns}/batch",
        "/api/v1/jobs",
    ] {
        assert!(doc["paths"][p].is_object(), "missing {p}");
    }
    server.shutdown().await.unwrap();
}

/// utoipa component names are global in the merged document: two different Rust
/// types sharing a schema name overwrite each other, and the generated SDK types
/// come out wrong. Every fragment that is merged into the served document must
/// agree on the definition of each component name it shares with another.
#[test]
fn schema_names_do_not_collide_across_fragments() {
    let fragments: Vec<(&str, utoipa::openapi::OpenApi)> = vec![
        ("api", forgebase_api::openapi::document()),
        ("auth", forgebase_auth::openapi()),
        ("graph", forgebase_graph::openapi()),
        ("realtime", forgebase_realtime::openapi()),
        ("kv", forgebase_kv::http::openapi()),
        ("billing", forgebase_billing::openapi()),
        ("credits", forgebase_credits::openapi()),
        ("entitlements", forgebase_entitlements::openapi()),
        ("functions", forgebase_functions::openapi()),
        ("customize", forgebase_customize::openapi()),
        ("jobs", forgebase_jobs::openapi()),
        ("vector", forgebase_vector::openapi()),
        ("storage", forgebase_storage::openapi()),
        ("queue", forgebase_queue::openapi()),
    ];
    let mut seen: BTreeMap<String, (&str, serde_json::Value)> = BTreeMap::new();
    let mut clashes = Vec::new();
    for (owner, doc) in &fragments {
        let Some(components) = &doc.components else { continue };
        for (name, schema) in &components.schemas {
            let value = serde_json::to_value(schema).unwrap();
            match seen.get(name) {
                Some((first, v)) if *v != value => {
                    clashes.push(format!("`{name}` is defined differently in `{first}` and `{owner}`"));
                }
                Some(_) => {}
                None => {
                    seen.insert(name.clone(), (owner, value));
                }
            }
        }
    }
    assert!(clashes.is_empty(), "schema name collisions:\n{}", clashes.join("\n"));
}
