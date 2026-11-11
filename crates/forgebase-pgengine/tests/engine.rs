//! End-to-end tests against a real PostgreSQL. They skip (pass with a note)
//! unless `FORGEBASE_PG_ENGINE_DIR` names a PostgreSQL 17+ installation.
//! Clusters live under `FORGEBASE_PGENGINE_TEST_TMP` (default: the system
//! temp dir) and every test stops its engines.

use std::path::{Path, PathBuf};
use std::time::Duration;

use forgebase_pgengine::{
    Engine, EngineConfig, EngineError, EngineHandle, EngineOrigin, EngineSource, EngineState, ExtensionMode, Layout,
    NodeKind, RestartPolicy, Role, archive,
};
use sqlx::{Connection, PgConnection, Row};

fn engine_dir() -> Option<PathBuf> {
    // `RUST_LOG=forgebase=debug` shows the engine's log and each step.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
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
    tempfile::Builder::new().prefix("pe").tempdir_in(base).expect("tempdir")
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

async fn connect(h: &EngineHandle, role: Role) -> PgConnection {
    PgConnection::connect(h.url(role).expose())
        .await
        .unwrap_or_else(|e| panic!("connect as {}: {e}", role.name()))
}

async fn one(conn: &mut PgConnection, sql: &'static str) -> String {
    sqlx::query(sql)
        .fetch_one(&mut *conn)
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .try_get::<String, _>(0)
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
}

async fn wait_state(h: &EngineHandle, want: EngineState, within: Duration) {
    let mut rx = h.watch_health();
    let ok = tokio::time::timeout(within, async {
        loop {
            if rx.borrow_and_update().state == want {
                return;
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    })
    .await;
    assert!(ok.is_ok(), "engine did not reach {want:?}: {:?}", h.health());
    assert_eq!(h.health().state, want);
}

#[tokio::test]
async fn primary_is_initialised_configured_and_restartable() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let layout = Layout::primary(tmp.path());
    let cfg = config(&engine, 18_201);
    let h = Engine::prepare(&layout, &cfg).await.expect("prepare");
    assert_eq!(h.health().state, EngineState::Ready);
    assert!(h.version().major >= 17);
    assert_eq!(h.socket_dir(), Some(layout.run_dir().as_path()));

    // Files and modes (ADR-0028 §4.1).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&layout.secrets_file()), 0o600);
        assert_eq!(mode(&layout.pgdata()), 0o700);
        assert_eq!(mode(&layout.run_dir()), 0o700);
    }
    let conf = std::fs::read_to_string(layout.conf_file()).expect("conf");
    assert!(conf.contains("listen_addresses = ''"), "{conf}");
    let secrets = std::fs::read_to_string(layout.secrets_file()).expect("secrets");
    assert!(!format!("{:?}", h.url(Role::Server)).contains("forgebase_server:"));

    // Settings of ADR-0028 §4.3 and ADR-0029 §1.3.
    let mut sup = connect(&h, Role::Super).await;
    for (name, want) in [
        ("listen_addresses", ""),
        ("wal_level", "logical"),
        ("max_wal_senders", "16"),
        ("max_replication_slots", "16"),
        ("max_prepared_transactions", "16"),
        ("max_worker_processes", "16"),
        ("max_connections", "64"),
        ("shared_buffers", "128MB"),
        ("data_checksums", "on"),
        ("password_encryption", "scram-sha-256"),
        ("TimeZone", "UTC"),
    ] {
        let got: String = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SHOW {name}")))
            .fetch_one(&mut sup)
            .await
            .expect("show");
        assert_eq!(got, want, "{name}");
    }
    let preload: String = one(&mut sup, "SHOW shared_preload_libraries").await;
    assert_eq!(preload, h.preloaded().join(","));
    if !h.has_cron() {
        assert!(h.warnings().iter().any(|w| w.contains("pg_cron")), "{:?}", h.warnings());
    }
    // Builtin C.UTF-8 locale on the forgebase database.
    let (provider, locale): (String, Option<String>) =
        sqlx::query_as("SELECT datlocprovider::text, datlocale FROM pg_database WHERE datname = 'forgebase'")
            .fetch_one(&mut sup)
            .await
            .expect("locale");
    assert_eq!(provider, "b");
    assert_eq!(locale.as_deref(), Some("C.UTF-8"));
    // Roles (rls-coverage §3.2).
    let rows: Vec<(String, bool, bool, bool)> = sqlx::query_as(
        "SELECT rolname::text, rolcanlogin, rolsuper, rolbypassrls FROM pg_roles
         WHERE rolname LIKE 'forgebase%' ORDER BY rolname",
    )
    .fetch_all(&mut sup)
    .await
    .expect("roles");
    let names: Vec<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(
        names,
        [
            "forgebase_login",
            "forgebase_login_write",
            "forgebase_owner",
            "forgebase_reader",
            "forgebase_rls",
            "forgebase_server",
            "forgebase_sql_admin",
            "forgebase_super"
        ]
    );
    for (name, login, superuser, bypass) in &rows {
        let want = match name.as_str() {
            "forgebase_super" => (true, true, true),
            "forgebase_server" | "forgebase_reader" => (true, false, true),
            "forgebase_owner" | "forgebase_sql_admin" => (false, false, true),
            "forgebase_rls" | "forgebase_login" | "forgebase_login_write" => (false, false, false),
            _ => unreachable!(),
        };
        assert_eq!((*login, *superuser, *bypass), want, "{name}");
    }
    let _ = sup.close().await;

    // The server role: search_path, schema owner, writes land in `forgebase`.
    let mut server = connect(&h, Role::Server).await;
    assert_eq!(one(&mut server, "SHOW search_path").await, "forgebase, public");
    assert_eq!(one(&mut server, "SELECT current_user::text").await, "forgebase_server");
    sqlx::raw_sql("CREATE TABLE _forgebase_probe (id bigint PRIMARY KEY); INSERT INTO _forgebase_probe VALUES (1);")
        .execute(&mut server)
        .await
        .expect("server writes");
    let schema: String = one(
        &mut server,
        "SELECT table_schema::text FROM information_schema.tables WHERE table_name = '_forgebase_probe'",
    )
    .await;
    assert_eq!(schema, "forgebase");
    sqlx::raw_sql("SET ROLE forgebase_owner; CREATE TABLE public.c_posts (id bigint); RESET ROLE;")
        .execute(&mut server)
        .await
        .expect("server can act as the owner");
    let _ = server.close().await;

    // The reader: same search_path, read-only, can read what the server made.
    let mut reader = connect(&h, Role::Reader).await;
    assert_eq!(one(&mut reader, "SHOW search_path").await, "forgebase, public");
    assert_eq!(one(&mut reader, "SHOW default_transaction_read_only").await, "on");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM _forgebase_probe")
        .fetch_one(&mut reader)
        .await
        .expect("reader reads server tables");
    assert_eq!(n, 1);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM c_posts")
        .fetch_one(&mut reader)
        .await
        .expect("reader reads owner tables");
    assert_eq!(n, 0);
    let err = sqlx::raw_sql("INSERT INTO _forgebase_probe VALUES (2)")
        .execute(&mut reader)
        .await
        .expect_err("reader cannot write");
    assert!(err.to_string().contains("read-only"), "{err}");
    let _ = reader.close().await;

    // SCRAM everywhere: a wrong password is refused.
    let bad = h
        .url(Role::Server)
        .expose()
        .replacen("forgebase_server:", "forgebase_server:x", 1);
    assert!(PgConnection::connect(&bad).await.is_err());

    // Stop: fast shutdown, no postmaster.pid, state Stopped; stop is idempotent.
    let pid = h.pid().expect("pid");
    h.stop().await.expect("stop");
    assert_eq!(h.health().state, EngineState::Stopped);
    assert!(!layout.pgdata().join("postmaster.pid").exists());
    h.stop().await.expect("stop again");
    #[cfg(unix)]
    assert!(!Path::new(&format!("/proc/{pid}")).exists() || cfg!(not(target_os = "linux")));
    drop(h);

    // Second start: existing cluster, same passwords, data kept.
    let h = Engine::prepare(&layout, &cfg).await.expect("second prepare");
    assert_eq!(
        std::fs::read_to_string(layout.secrets_file()).expect("secrets"),
        secrets
    );
    let mut server = connect(&h, Role::Server).await;
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM _forgebase_probe")
        .fetch_one(&mut server)
        .await
        .expect("data survives a restart");
    assert_eq!(n, 1);
    let _ = server.close().await;
    h.stop().await.expect("stop");
}

#[tokio::test]
async fn forgebase_db_opens_the_managed_engine() {
    use forgebase_db::{Backend, BackendKind, Database, DbOptions, DbTarget, Statement, TxMode, Value};
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let layout = Layout::primary(tmp.path());
    let h = Engine::prepare(&layout, &config(&engine, 18_202))
        .await
        .expect("prepare");
    let opts = |url: &str, read_only: bool| DbOptions {
        target: DbTarget::postgres(forgebase_db::SecretString::new(url), true),
        synchronous: forgebase_core::Synchronous::Full,
        busy_timeout: Duration::from_secs(5),
        read_pool_size: 2,
        create_if_missing: false,
        read_only,
    };
    let db = Database::open(opts(h.url(Role::Server).expose(), false))
        .await
        .expect("open as server");
    assert_eq!(db.kind(), BackendKind::Postgres);
    assert!(
        !db.location()
            .contains(h.url(Role::Server).expose().split(':').nth(2).unwrap_or("x"))
    );
    let mut tx = db.begin(TxMode::SchemaChange).await.expect("begin");
    tx.execute(&Statement::new(
        "CREATE TABLE _forgebase_fb132 (k text PRIMARY KEY, v bigint)",
        vec![],
    ))
    .await
    .expect("create");
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_fb132 (k, v) VALUES (?1, ?2)",
        vec![Value::Text("a".into()), Value::Integer(7)],
    ))
    .await
    .expect("insert");
    tx.commit().await.expect("commit");
    db.close().await;

    let ro = Database::open(opts(h.url(Role::Reader).expose(), true))
        .await
        .expect("open as reader");
    let rows = ro
        .fetch_all(&Statement::new(
            "SELECT v FROM _forgebase_fb132 WHERE k = ?1",
            vec![Value::Text("a".into())],
        ))
        .await
        .expect("read");
    assert_eq!(rows.rows, vec![vec![Value::Integer(7)]]);
    ro.close().await;
    h.stop().await.expect("stop");
}

#[cfg(unix)]
#[tokio::test]
async fn crashed_postmaster_is_restarted_with_backoff() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let layout = Layout::primary(tmp.path());
    let cfg = EngineConfig {
        restart: RestartPolicy {
            initial: Duration::from_millis(200),
            max: Duration::from_secs(2),
            max_failures: 20,
            window: Duration::from_secs(600),
        },
        ..config(&engine, 18_203)
    };
    let h = Engine::prepare(&layout, &cfg).await.expect("prepare");
    let first = h.pid().expect("pid");
    let mut rx = h.watch_health();
    rx.borrow_and_update();
    h.crash_for_test().await;
    tokio::time::timeout(Duration::from_secs(10), async {
        while rx.borrow_and_update().state != EngineState::Restarting {
            rx.changed().await.expect("health");
        }
    })
    .await
    .expect("restarting");
    wait_state(&h, EngineState::Ready, Duration::from_secs(60)).await;
    let health = h.health();
    assert_eq!(health.restarts, 1);
    assert_ne!(health.pid, Some(first));
    // The last failure: the crash itself, or a restart attempt that found the
    // old backends still holding shared memory.
    assert!(health.last_error.is_some());
    let mut server = connect(&h, Role::Server).await;
    assert_eq!(one(&mut server, "SELECT current_user::text").await, "forgebase_server");
    let _ = server.close().await;
    h.stop().await.expect("stop");
}

#[cfg(unix)]
#[tokio::test]
async fn postmaster_left_by_a_killed_server_is_stopped_first() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let layout = Layout::primary(tmp.path());
    let cfg = config(&engine, 18_204);
    Engine::prepare(&layout, &cfg)
        .await
        .expect("prepare")
        .stop()
        .await
        .expect("stop");

    // A postmaster that no ForgeBase supervises, as after `kill -9` of the
    // server: started through a shell that exits, so it is reparented to
    // init (a child of this test would linger as a zombie after it stops).
    let status = tokio::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(r#""$0" -D "$1" </dev/null >/dev/null 2>&1 &"#)
        .arg(engine.join("bin/postgres"))
        .arg(layout.pgdata())
        .status()
        .await
        .expect("orphan postmaster");
    assert!(status.success());
    let socket = layout.run_dir().join(".s.PGSQL.18204");
    tokio::time::timeout(Duration::from_secs(30), async {
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("orphan listens");
    let pidfile = std::fs::read_to_string(layout.pgdata().join("postmaster.pid")).expect("pid file");
    let orphan_pid: i32 = pidfile.lines().next().and_then(|l| l.parse().ok()).expect("pid");
    let orphan_alive =
        || rustix::process::Pid::from_raw(orphan_pid).is_some_and(|p| rustix::process::test_kill_process(p).is_ok());
    assert!(orphan_alive());

    let h = Engine::prepare(&layout, &cfg).await.expect("prepare over a leftover");
    assert_ne!(h.pid(), u32::try_from(orphan_pid).ok());
    assert!(!orphan_alive(), "the leftover postmaster was stopped");
    let mut server = connect(&h, Role::Server).await;
    assert_eq!(one(&mut server, "SELECT current_user::text").await, "forgebase_server");
    let _ = server.close().await;
    h.stop().await.expect("stop");
}

#[tokio::test]
async fn engine_from_an_archive_is_extracted_once_and_old_minors_are_removed() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let archive_path = tmp.path().join("engine.tar.zst");
    let file = std::fs::File::create(&archive_path).expect("create archive");
    let version = forgebase_pgengine::EngineDir::open(&engine, EngineOrigin::Dir)
        .await
        .expect("engine")
        .version;
    archive::pack_dir(
        &engine,
        &["bin", "lib", "share"],
        &archive::PackInfo {
            postgres: version.text.clone(),
            build: "t1".into(),
            target: "test".into(),
            ..archive::PackInfo::default()
        },
        file,
        1,
    )
    .expect("pack");

    let data = tmp.path().join("data");
    let layout = Layout::primary(&data);
    // An older minor of the same major, and an engine of another major.
    for name in [
        format!("{}.0-r1-000000000000", version.major),
        "16.9-r1-111111111111".to_owned(),
    ] {
        let d = layout.engines_dir().join(name);
        std::fs::create_dir_all(&d).expect("old engine");
        std::fs::write(d.join(archive::MANIFEST_NAME), "{}").expect("manifest");
    }
    let cfg = EngineConfig {
        source: EngineSource::Archive(archive_path.clone()),
        ..config(&engine, 18_205)
    };
    let h = Engine::prepare(&layout, &cfg).await.expect("prepare from archive");
    let root = h.engine().root.clone();
    assert!(root.starts_with(layout.engines_dir()));
    let name = root.file_name().expect("name").to_string_lossy().into_owned();
    assert!(name.starts_with(&format!("{}-t1-", version.text)), "{name}");
    assert!(matches!(h.engine().origin, EngineOrigin::Extracted { fresh: true, .. }));
    archive::verify_extracted(&root).expect("verify");
    assert!(
        !layout
            .engines_dir()
            .join(format!("{}.0-r1-000000000000", version.major))
            .exists()
    );
    assert!(layout.engines_dir().join("16.9-r1-111111111111").exists());
    let mut server = connect(&h, Role::Server).await;
    assert_eq!(one(&mut server, "SELECT current_user::text").await, "forgebase_server");
    let _ = server.close().await;
    h.stop().await.expect("stop");
    drop(h);

    let h = Engine::prepare(&layout, &cfg).await.expect("prepare again");
    assert_eq!(h.engine().root, root);
    assert!(matches!(
        h.engine().origin,
        EngineOrigin::Extracted { fresh: false, .. }
    ));
    h.stop().await.expect("stop");
}

#[tokio::test]
async fn cron_modes_and_bad_settings() {
    let Some(engine) = engine_dir() else { return };
    let probe = forgebase_pgengine::EngineDir::open(&engine, EngineOrigin::Dir)
        .await
        .expect("engine");
    let has_cron = probe.has_module("pg_cron");
    let tmp = tempdir();
    let layout = Layout::primary(tmp.path());

    let on = EngineConfig {
        cron: ExtensionMode::On,
        ..config(&engine, 18_206)
    };
    if !has_cron {
        let err = Engine::prepare(&layout, &on).await.expect_err("cron required");
        assert!(matches!(err, EngineError::Config(_)), "{err}");
        assert!(err.to_string().contains("pg_cron"), "{err}");
        assert!(!layout.pgdata().exists(), "refused before initdb");
    }

    let off = EngineConfig {
        cron: ExtensionMode::Off,
        ..config(&engine, 18_206)
    };
    let h = Engine::prepare(&layout, &off).await.expect("cron off");
    assert!(!h.has_cron());
    assert!(h.warnings().iter().all(|w| !w.contains("pg_cron")));
    assert!(
        !std::fs::read_to_string(layout.conf_file())
            .expect("conf")
            .contains("cron.")
    );
    h.stop().await.expect("stop");
    drop(h);

    // A deny-listed setting is refused before anything starts.
    let mut denied = config(&engine, 18_206);
    denied.settings.insert("archive_command".into(), "touch /tmp/x".into());
    let err = Engine::prepare(&layout, &denied).await.expect_err("denied");
    assert!(err.to_string().contains("managed by ForgeBase"), "{err}");

    // A value PostgreSQL rejects: the start fails with the engine's own words.
    let mut bad = config(&engine, 18_206);
    bad.settings.insert("shared_buffers".into(), "lots".into());
    let err = Engine::prepare(&layout, &bad).await.expect_err("bad value");
    let text = err.to_string();
    assert!(matches!(err, EngineError::Start { .. }), "{text}");
    assert!(text.contains("shared_buffers"), "{text}");
    assert!(!layout.pgdata().join("postmaster.pid").exists());
}

#[tokio::test]
async fn several_clusters_run_side_by_side_and_a_standby_starts() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let primary = Layout::primary(tmp.path());
    let shard = Layout::node(tmp.path(), "shard-1", NodeKind::Primary).expect("node");
    let replica = Layout::node(tmp.path(), "replica-1", NodeKind::Standby).expect("node");
    let cfg = config(&engine, 18_207);
    let p = Engine::prepare(&primary, &cfg).await.expect("primary");
    let s = Engine::prepare(&shard, &cfg).await.expect("shard");
    assert_ne!(p.socket_dir(), s.socket_dir());
    assert!(shard.pgdata().join("PG_VERSION").exists());
    for h in [&p, &s] {
        let mut c = connect(h, Role::Server).await;
        assert_eq!(one(&mut c, "SHOW search_path").await, "forgebase, public");
        let _ = c.close().await;
    }

    // A standby needs provisioning first (FB-168 does it with pg_basebackup).
    let err = Engine::prepare(&replica, &cfg)
        .await
        .expect_err("unprovisioned standby");
    assert!(err.to_string().contains("pg_basebackup"), "{err}");
    let secrets = forgebase_pgengine::RoleSecrets::load(&primary.secrets_file())
        .expect("load")
        .expect("secrets");
    std::fs::create_dir_all(replica.node_dir()).expect("node dir");
    let out = tokio::process::Command::new(engine.join("bin/pg_basebackup"))
        .arg("-D")
        .arg(replica.pgdata())
        .args(["-X", "stream", "-R", "--checkpoint=fast", "-h"])
        .arg(p.socket_dir().expect("socket"))
        .args(["-p", &p.port().to_string(), "-U", "forgebase_super"])
        .env("PGPASSWORD", secrets.password(Role::Super).expect("pw"))
        .output()
        .await
        .expect("pg_basebackup");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let r = Engine::prepare(&replica, &cfg).await.expect("standby");
    let mut c = connect(&r, Role::Reader).await;
    let in_recovery: bool = sqlx::query_scalar("SELECT pg_is_in_recovery()")
        .fetch_one(&mut c)
        .await
        .expect("recovery");
    assert!(in_recovery);
    assert_eq!(one(&mut c, "SHOW hot_standby_feedback").await, "on");
    let _ = c.close().await;
    for h in [r, s, p] {
        h.stop().await.expect("stop");
    }
}

/// Runs only in a build with the `bundled-postgres` feature (the archive
/// named by `FORGEBASE_PG_ENGINE_ARCHIVE` at build time).
#[tokio::test]
async fn embedded_engine_is_extracted_and_runs() {
    if !forgebase_pgengine::HAS_EMBEDDED {
        eprintln!("skipped: built without the bundled-postgres feature");
        return;
    }
    let tmp = tempdir();
    let layout = Layout::primary(tmp.path());
    let cfg = EngineConfig {
        source: EngineSource::Embedded,
        socket_port: 18_208,
        unsafe_fast: true,
        ready_timeout: Duration::from_secs(60),
        ..EngineConfig::default()
    };
    let h = Engine::prepare(&layout, &cfg)
        .await
        .expect("prepare from the embedded archive");
    assert!(h.engine().root.starts_with(layout.engines_dir()));
    assert!(matches!(h.engine().origin, EngineOrigin::Extracted { fresh: true, .. }));
    let mut server = connect(&h, Role::Server).await;
    assert_eq!(one(&mut server, "SHOW search_path").await, "forgebase, public");
    let _ = server.close().await;
    h.stop().await.expect("stop");
}
