//! Server functions through the real server lifecycle (SPEC §27 #18 on a
//! live server): the migration applies, the API is mounted and documented,
//! an HTTP function answers over TCP, the `function.invoke` job runs a
//! function in the worker pool, and functions survive a restart.
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use base64::Engine as _;
use forgebase_core::Config;
use forgebase_data::AccessContext;
use forgebase_functions::{DeployRequest, InvokeAccess, LimitsInput};
use forgebase_jobs::{JobStatus, NewJob};
use serde_json::{Value as Json, json};
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

/// A module that logs and answers `{"status":200,"body":{"hello":"world"}}`.
fn hello() -> String {
    let out = r#"{"status":200,"body":{"hello":"world"}}"#;
    let esc: String = out.bytes().map(|b| format!("\\{b:02x}")).collect();
    let wat = format!(
        r#"(module
  (import "forgebase" "output" (func $output (param i32 i32) (result i32)))
  (import "forgebase" "log" (func $log (param i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 16) "{esc}")
  (data (i32.const 256) "hi")
  (func (export "handle") (result i32)
    (drop (call $log (i32.const 1) (i32.const 256) (i32.const 2)))
    (drop (call $output (i32.const 16) (i32.const {len})))
    (i32.const 0)))"#,
        len = out.len()
    );
    base64::engine::general_purpose::STANDARD.encode(wat::parse_str(&wat).unwrap())
}

#[tokio::test]
async fn functions_run_on_the_live_server_from_http_and_jobs_and_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let server = forgebase_server::start(&cfg).await.unwrap();
    let addr = server.local_addr();

    let (s, doc) = http(addr, "GET", "/api/openapi.json", "").await;
    assert_eq!(s, 200);
    assert!(doc.contains("/api/v1/functions/{name}/invoke"));
    assert_eq!(http(addr, "GET", "/api/v1/functions", "").await.0, 401, "anonymous");
    assert_eq!(
        http(addr, "POST", "/api/v1/functions", r#"{"name":"x","wasm":""}"#)
            .await
            .0,
        401
    );

    let admin = AccessContext::admin("root");
    let d = server
        .functions()
        .deploy(
            &admin,
            DeployRequest {
                name: "hello".into(),
                wasm: hello(),
                description: Some("greets".into()),
                access: Some(InvokeAccess::Public),
                capabilities: vec!["log".into()],
                fetch_hosts: Vec::new(),
                limits: LimitsInput::default(),
                env: Default::default(),
                hooks: Vec::new(),
                activate: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(d.version.version, 1);

    let (s, body) = http(addr, "POST", "/api/v1/functions/hello/invoke", "{}").await;
    assert_eq!(s, 200, "{body}");
    assert_eq!(serde_json::from_str::<Json>(&body).unwrap(), json!({"hello": "world"}));

    // A background job runs the function in the server's worker pool.
    let sys = AccessContext::system();
    let (job, _) = server
        .jobs()
        .enqueue(&sys, NewJob::new("function.invoke", json!({"function": "hello"})))
        .await
        .unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let done = loop {
        let j = server.jobs().get(&sys, &job.id).await.unwrap();
        if j.status == JobStatus::Succeeded || j.status == JobStatus::Dead {
            break j;
        }
        assert!(tokio::time::Instant::now() < deadline, "job did not finish");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(done.status, JobStatus::Succeeded, "{:?}", done.result);
    assert_eq!(done.result.unwrap()["body"]["hello"], "world");
    let logs = server.functions().logs(&admin, "hello", 10).await.unwrap();
    assert!(logs.iter().any(|l| l.trigger == "job" && l.status == "ok"));
    assert!(logs.iter().any(|l| l.trigger == "http" && l.logs[0].message == "hi"));
    server.shutdown().await.unwrap();

    // Restart on the same data directory: the function is still deployed.
    let server = forgebase_server::start(&cfg).await.unwrap();
    let (s, body) = http(server.local_addr(), "GET", "/api/v1/functions/hello/invoke", "").await;
    assert_eq!(s, 200, "{body}");
    assert_eq!(server.functions().get(&admin, "hello").unwrap().active_version, 1);
    server.shutdown().await.unwrap();
}
