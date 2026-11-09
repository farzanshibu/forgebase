//! The RESP listener through the real server lifecycle: off by default, on by
//! `[resp] enabled`, authenticated with a real API key, data surviving a
//! restart, bind errors, shutdown. These tests bind local ports (`127.0.0.1:0`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;

use forgebase_auth::{Argon2Config, AuthConfig, AuthService, Authenticated, ClientInfo, CredentialKind, Subject};
use forgebase_core::{Config, ErrorCode};
use forgebase_data::DefaultAuthorizer;
use forgebase_server::{RunningServer, start};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn config(dir: &Path, resp: bool) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg.resp.enabled = resp;
    cfg.resp.port = 0;
    cfg
}

/// Creates an administrator and an API key through a second auth service on
/// the same database and master key (the server does not expose its own).
async fn api_key(server: &RunningServer, dir: &Path, name: &str) -> String {
    let mut acfg = AuthConfig::for_data_dir(dir);
    acfg.argon2 = Argon2Config::insecure_fast_for_tests();
    let auth = AuthService::open_with_data(
        Arc::new(server.database().clone()),
        Arc::new(DefaultAuthorizer),
        dir,
        acfg,
        None,
    )
    .await
    .unwrap();
    let ci = ClientInfo::default();
    let admin = match auth
        .create_admin(&format!("{name}@example.com"), "a-long-admin-password-1", "test", &ci)
        .await
    {
        Ok(a) => a.id,
        Err(e) => panic!("{e}"),
    };
    let caller = Authenticated {
        subject: Subject::Admin { id: admin },
        credential: CredentialKind::AccessToken,
        credential_id: "t".into(),
        scopes: None,
        mfa: false,
        impersonator: None,
    };
    let sa = auth
        .create_service_account(&caller, &ci, name, "", "admin")
        .await
        .unwrap();
    auth.create_api_key(&caller, &ci, &sa.id, "resp", &["*".to_owned()], None)
        .await
        .unwrap()
        .key
}

async fn redis_at(addr: std::net::SocketAddr, key: &str) -> redis::aio::MultiplexedConnection {
    redis::Client::open(format!("redis://:{key}@{addr}/"))
        .unwrap()
        .get_multiplexed_async_connection()
        .await
        .unwrap()
}

#[tokio::test]
async fn the_resp_listener_is_off_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let server = start(&config(tmp.path(), false)).await.unwrap();
    assert_eq!(server.resp_addr(), None);
    server.shutdown().await.unwrap();
    assert!(!Config::defaults(tmp.path()).resp.enabled);
}

#[tokio::test]
async fn redis_clients_use_the_kv_engine_with_an_api_key_and_data_survives_a_restart() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path(), true);
    let server = start(&cfg).await.unwrap();
    let resp = server.resp_addr().expect("listener is enabled");
    let key = api_key(&server, &cfg.data_dir, "worker-a").await;

    // unauthenticated: refused
    let mut raw = tokio::net::TcpStream::connect(resp).await.unwrap();
    raw.write_all(b"*2\r\n$3\r\nGET\r\n$1\r\nk\r\n").await.unwrap();
    let mut buf = [0u8; 64];
    let n = raw.read(&mut buf).await.unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("-NOAUTH"));

    let mut c = redis_at(resp, &key).await;
    let _: () = redis::cmd("SET")
        .arg("greeting")
        .arg("hello")
        .query_async(&mut c)
        .await
        .unwrap();
    let _: i64 = redis::cmd("RPUSH")
        .arg("l")
        .arg("a")
        .arg("b")
        .query_async(&mut c)
        .await
        .unwrap();
    let got: String = redis::cmd("GET").arg("greeting").query_async(&mut c).await.unwrap();
    assert_eq!(got, "hello");
    drop(c);
    drop(raw);

    // shutdown flushes the KV snapshot; a new server on the same data dir serves it over RESP
    server.shutdown().await.unwrap();
    let server = start(&cfg).await.unwrap();
    let resp = server.resp_addr().unwrap();
    let mut c = redis_at(resp, &key).await;
    let got: String = redis::cmd("GET").arg("greeting").query_async(&mut c).await.unwrap();
    assert_eq!(got, "hello", "the value survived the restart");
    let list: Vec<String> = redis::cmd("LRANGE")
        .arg("l")
        .arg(0)
        .arg(-1)
        .query_async(&mut c)
        .await
        .unwrap();
    assert_eq!(list, ["a", "b"]);
    drop(c);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_closes_open_resp_connections() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path(), true);
    let server = start(&cfg).await.unwrap();
    let key = api_key(&server, &cfg.data_dir, "worker-b").await;
    let mut raw = tokio::net::TcpStream::connect(server.resp_addr().unwrap())
        .await
        .unwrap();
    raw.write_all(format!("*2\r\n$4\r\nAUTH\r\n${}\r\n{key}\r\n", key.len()).as_bytes())
        .await
        .unwrap();
    let mut buf = [0u8; 16];
    let n = raw.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"+OK\r\n");
    tokio::time::timeout(std::time::Duration::from_secs(10), server.shutdown())
        .await
        .expect("shutdown does not hang on an open RESP connection")
        .unwrap();
    assert_eq!(raw.read(&mut buf).await.unwrap_or(0), 0, "the connection was closed");
}

#[tokio::test]
async fn a_resp_port_that_is_taken_is_a_clear_bind_error_and_frees_the_data_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let taken = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut cfg = config(tmp.path(), true);
    cfg.resp.port = taken.local_addr().unwrap().port();
    let err = start(&cfg).await.expect_err("port in use");
    assert_eq!(err.code(), "port_in_use");
    assert!(err.to_string().contains("(resp)"), "{err}");
    // nothing is left running or locked
    cfg.resp.port = 0;
    let server = start(&cfg).await.unwrap();
    server.shutdown().await.unwrap();
}
