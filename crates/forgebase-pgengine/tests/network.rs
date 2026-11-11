//! FB-153: what the engine exposes on the network. Human decision
//! 2026-11-11: no TCP port unless an operator sets `listen`, and TLS on every
//! TCP login when one is set. Skips unless `FORGEBASE_PG_ENGINE_DIR` names a
//! PostgreSQL 17+ installation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use forgebase_pgengine::{Engine, EngineConfig, EngineHandle, EngineSource, Layout, Role, RoleSecrets, TlsFiles};
use sqlx::postgres::{PgConnectOptions, PgSslMode};
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
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&base).expect("test tmp base");
    tempfile::Builder::new().prefix("pn").tempdir_in(base).expect("tempdir")
}

fn config(dir: &Path, port: u16) -> EngineConfig {
    EngineConfig {
        source: EngineSource::Dir(dir.to_path_buf()),
        socket_port: port,
        unsafe_fast: true,
        ready_timeout: Duration::from_secs(60),
        stop_timeout: Duration::from_secs(20),
        ..EngineConfig::default()
    }
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    l.local_addr().expect("addr").port()
}

fn password(layout: &Layout, role: Role) -> String {
    RoleSecrets::load(&layout.secrets_file())
        .expect("secrets")
        .expect("roles.secret")
        .password(role)
        .expect("password")
        .to_owned()
}

fn tcp(port: u16, user: &str, password: &str, ssl: PgSslMode) -> PgConnectOptions {
    PgConnectOptions::new()
        .host("127.0.0.1")
        .port(port)
        .username(user)
        .password(password)
        .database(forgebase_pgengine::DATABASE)
        .ssl_mode(ssl)
}

async fn show(h: &EngineHandle, name: &str) -> String {
    let mut c = PgConnection::connect(h.url(Role::Super).expose()).await.expect("socket login");
    let v: String = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SHOW {name}")))
        .fetch_one(&mut c)
        .await
        .expect("show");
    let _ = c.close().await;
    v
}

fn host_lines(pgdata: &Path) -> Vec<String> {
    std::fs::read_to_string(pgdata.join("pg_hba.conf"))
        .expect("pg_hba.conf")
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty() && !l.starts_with("local"))
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn the_default_configuration_opens_no_tcp_port() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let layout = Layout::primary(tmp.path());
    // A port that would be the TCP port if anything listened.
    let port = free_port();
    let h = Engine::prepare(&layout, &config(&engine, port)).await.expect("prepare");
    assert_eq!(h.tcp_addr(), None);
    assert_eq!(show(&h, "listen_addresses").await, "");
    assert_eq!(show(&h, "ssl").await, "off");
    assert!(
        host_lines(&layout.pgdata()).is_empty(),
        "pg_hba.conf has no TCP line: {:?}",
        host_lines(&layout.pgdata())
    );
    for addr in [format!("127.0.0.1:{port}"), format!("[::1]:{port}")] {
        assert!(
            tokio::net::TcpStream::connect(&addr).await.is_err(),
            "nothing accepts TCP on {addr}"
        );
    }
    assert!(!layout.node_dir().join("tls").exists(), "no certificate without listen");
    // The roles FB-153 adds are there.
    let mut c = PgConnection::connect(h.url(Role::Super).expose()).await.expect("login");
    let ok: bool = sqlx::query_scalar(
        "SELECT pg_has_role('forgebase_reader', 'forgebase_sql_admin', 'SET') \
         AND pg_has_role('forgebase_server', 'forgebase_sql_admin', 'SET') \
         AND pg_has_role('forgebase_server', 'forgebase_login_write', 'SET') \
         AND NOT pg_has_role('forgebase_server', 'forgebase_login', 'USAGE') \
         AND pg_has_role('forgebase_login_write', 'forgebase_login', 'USAGE')",
    )
    .fetch_one(&mut c)
    .await
    .expect("roles");
    assert!(ok, "editor and login role memberships");
    let _ = c.close().await;
    h.stop().await.expect("stop");
}

#[tokio::test]
async fn listen_is_an_explicit_opt_in_and_always_uses_tls() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let layout = Layout::primary(tmp.path());
    let port = free_port();
    let mut cfg = config(&engine, port);
    cfg.listen = Some(format!("127.0.0.1:{port}").parse().expect("addr"));
    let h = Engine::prepare(&layout, &cfg).await.expect("prepare with listen");
    assert_eq!(h.tcp_addr(), cfg.listen);
    assert_eq!(show(&h, "ssl").await, "on");
    let tls = layout.node_dir().join("tls");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(tls.join("server.key")).expect("key").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the generated key is private");
    }
    let cert = std::fs::read(tls.join("server.crt")).expect("generated certificate");

    // ForgeBase's roles over loopback: TLS only.
    let pw = password(&layout, Role::Server);
    let mut c = PgConnection::connect_with(&tcp(port, Role::Server.name(), &pw, PgSslMode::Require))
        .await
        .expect("TLS login over loopback");
    let ssl: bool = sqlx::query_scalar("SELECT ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()")
        .fetch_one(&mut c)
        .await
        .expect("pg_stat_ssl");
    assert!(ssl, "the session is encrypted");
    let _ = c.close().await;
    let plain = PgConnection::connect_with(&tcp(port, Role::Server.name(), &pw, PgSslMode::Disable)).await;
    assert!(plain.is_err(), "a TCP login without TLS is refused");

    // A named login (member of forgebase_login) over TCP, with TLS.
    let mut sup = PgConnection::connect(h.url(Role::Super).expose()).await.expect("login");
    sqlx::raw_sql("CREATE ROLE fbu_net LOGIN PASSWORD 'net-test-only' IN ROLE forgebase_login")
        .execute(&mut sup)
        .await
        .expect("create login role");
    let _ = sup.close().await;
    let mut c = PgConnection::connect_with(&tcp(port, "fbu_net", "net-test-only", PgSslMode::Require))
        .await
        .expect("named login over TLS");
    let _ = c.close().await;
    assert!(
        PgConnection::connect_with(&tcp(port, "fbu_net", "net-test-only", PgSslMode::Disable))
            .await
            .is_err()
    );
    h.stop().await.expect("stop");

    // A restart keeps the generated certificate; a configured one replaces it.
    let h = Engine::prepare(&layout, &cfg).await.expect("restart");
    assert_eq!(std::fs::read(tls.join("server.crt")).expect("cert"), cert);
    h.stop().await.expect("stop");
    let own = tmp.path().join("own");
    std::fs::create_dir_all(&own).expect("dir");
    for f in ["server.crt", "server.key"] {
        std::fs::copy(tls.join(f), own.join(f)).expect("copy");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(own.join("server.key"), std::fs::Permissions::from_mode(0o600)).expect("mode");
    }
    cfg.tls = Some(TlsFiles {
        cert: own.join("server.crt"),
        key: own.join("server.key"),
    });
    let h = Engine::prepare(&layout, &cfg).await.expect("configured certificate");
    assert_eq!(show(&h, "ssl_cert_file").await, own.join("server.crt").display().to_string());
    PgConnection::connect_with(&tcp(port, Role::Server.name(), &pw, PgSslMode::Require))
        .await
        .expect("TLS with the configured certificate");
    h.stop().await.expect("stop");
    cfg.tls = Some(TlsFiles {
        cert: own.join("missing.crt"),
        key: own.join("server.key"),
    });
    assert!(Engine::prepare(&layout, &cfg).await.is_err(), "a missing certificate refuses the start");
}
