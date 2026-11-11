//! The bundled engine's extension setup (ADR-0029 §1.1–§1.3, FB-161): the
//! server creates the extensions as `forgebase_super` after the migrations,
//! the server and reader roles can use them, AGE is preloaded per role only
//! when the engine ships it. Skips unless `FORGEBASE_PG_ENGINE_DIR` names an
//! engine; with FB-130's engine every extension but AGE is present.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use forgebase_core::config::{ExtensionMode as Mode, PostgresExtensions};
use forgebase_db::{
    Backend, Database, DbOptions, DbTarget, ExtensionSetup, Feature, FeatureState, SecretString, Statement, TxMode,
    Value,
};
use forgebase_pgengine::{
    Engine, EngineConfig, EngineDir, EngineError, EngineHandle, EngineOrigin, EngineSource, ExtensionMode, Layout, Role,
};

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
    tempfile::Builder::new().prefix("px").tempdir_in(base).expect("tempdir")
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

async fn open(h: &EngineHandle, role: Role, read_only: bool) -> Database {
    Database::open(DbOptions {
        target: DbTarget::postgres(SecretString::new(h.url(role).expose()), true),
        synchronous: forgebase_core::Synchronous::Full,
        busy_timeout: Duration::from_secs(5),
        read_pool_size: 2,
        create_if_missing: false,
        read_only,
    })
    .await
    .unwrap_or_else(|e| panic!("open as {}: {e}", role.name()))
}

fn managed(h: &EngineHandle, modes: PostgresExtensions) -> ExtensionSetup {
    ExtensionSetup {
        modes,
        admin_url: Some(SecretString::new(h.url(Role::Super).expose())),
        reader_role: Some(Role::Reader.name().to_owned()),
    }
}

async fn scalar(db: &Database, sql: &str) -> Value {
    db.fetch_all(&Statement::new(sql, vec![]))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .scalar()
        .cloned()
        .unwrap_or(Value::Null)
}

async fn write(db: &Database, sql: &str) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(sql, vec![]))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn the_managed_engine_creates_grants_and_reports_its_extensions() {
    let Some(engine) = engine_dir() else { return };
    let probe = EngineDir::open(&engine, EngineOrigin::Dir).await.expect("engine");
    let has_vector = probe.has_module("vector");
    let has_pgmq = engine.join("share/extension/pgmq.control").is_file();
    let tmp = tempdir();
    let layout = Layout::primary(tmp.path());
    let h = Engine::prepare(&layout, &config(&engine, 18_231))
        .await
        .expect("prepare");
    let has_cron = h.has_cron();
    assert!(!h.has_age(), "no engine ships AGE before FB-160");
    assert!(
        h.warnings().iter().any(|w| w.contains("Apache AGE")),
        "{:?}",
        h.warnings()
    );

    let db = open(&h, Role::Server, false).await;
    db.migrate(None).await.expect("migrate");
    let report = db
        .setup_extensions(&managed(&h, PostgresExtensions::default()))
        .await
        .expect("setup");
    let state = |f: Feature| report.get(f).unwrap().state;
    assert_eq!(state(Feature::Vector) == FeatureState::Active, has_vector, "{report:?}");
    assert_eq!(state(Feature::Queue) == FeatureState::Active, has_pgmq, "{report:?}");
    assert_eq!(state(Feature::Cron) == FeatureState::Active, has_cron, "{report:?}");
    assert_eq!(state(Feature::Trigram), FeatureState::Active);
    let graph = report.get(Feature::Graph).unwrap();
    assert_eq!(graph.state, FeatureState::Portable);
    assert!(
        graph.reason.as_deref().unwrap().contains("no age extension"),
        "{graph:?}"
    );

    // Owners: superuser-only extensions belong to forgebase_super, the
    // others to the server role.
    let owner = |ext: &str| {
        format!(
            "SELECT r.rolname::text FROM pg_extension e JOIN pg_roles r ON r.oid = e.extowner WHERE e.extname = '{ext}'"
        )
    };
    assert_eq!(
        scalar(&db, &owner("pg_trgm")).await,
        Value::Text("forgebase_server".into())
    );
    if has_vector {
        assert_eq!(
            scalar(&db, &owner("vector")).await,
            Value::Text("forgebase_super".into())
        );
        assert_eq!(
            scalar(&db, "SELECT ('[1,2,3]'::vector <-> '[1,2,4]'::vector)::text").await,
            Value::Text("1".into())
        );
        assert!(report.get(Feature::Vector).unwrap().engine.starts_with("pgvector 0.8"));
    }
    if has_pgmq {
        assert_eq!(
            scalar(&db, &owner("pgmq")).await,
            Value::Text("forgebase_server".into())
        );
        write(&db, "SELECT pgmq.create('fb161')").await;
        write(&db, "SELECT pgmq.send('fb161', '{\"a\": 1}'::jsonb)").await;
    }
    if has_cron {
        assert_eq!(
            scalar(&db, &owner("pg_cron")).await,
            Value::Text("forgebase_super".into())
        );
        write(&db, "SELECT cron.schedule('fb161', '0 3 * * *', 'SELECT 1')").await;
        assert_eq!(
            scalar(&db, "SELECT username::text FROM cron.job WHERE jobname = 'fb161'").await,
            Value::Text("forgebase_server".into())
        );
        write(&db, "SELECT cron.unschedule('fb161')").await;
    }

    // The reader role sees what it needs.
    let reader = open(&h, Role::Reader, true).await;
    assert_eq!(
        scalar(&reader, "SELECT similarity('forgebase', 'forgebase')::text").await,
        Value::Text("1".into())
    );
    if has_pgmq {
        assert_eq!(
            scalar(&reader, "SELECT count(*) FROM pgmq.meta").await,
            Value::Integer(1)
        );
        assert_eq!(
            scalar(&reader, "SELECT count(*) FROM pgmq.q_fb161").await,
            Value::Integer(1)
        );
    }
    reader.close().await;

    // Without AGE, no role preloads it (a missing library would break logins).
    assert_eq!(
        scalar(
            &db,
            "SELECT count(*) FROM pg_db_role_setting s JOIN pg_roles r ON r.oid = s.setrole \
             WHERE r.rolname IN ('forgebase_server', 'forgebase_reader') \
             AND array_to_string(s.setconfig, ',') LIKE '%session_preload_libraries%'"
        )
        .await,
        Value::Integer(0)
    );

    // Info shows it; a restart keeps the same setup.
    assert_eq!(db.info().await.unwrap().features, report.features);
    db.close().await;
    h.stop().await.expect("stop");
    drop(h);
    let h = Engine::prepare(&layout, &config(&engine, 18_231))
        .await
        .expect("restart");
    let db = open(&h, Role::Server, false).await;
    db.migrate(None).await.expect("migrate again");
    let again = db
        .setup_extensions(&managed(&h, PostgresExtensions::default()))
        .await
        .expect("setup again");
    assert_eq!(again, report);

    // `on` for the missing one refuses; `off` everywhere is refused for the
    // data already kept in extensions.
    let err = db
        .setup_extensions(&managed(
            &h,
            PostgresExtensions {
                graph: Mode::On,
                ..PostgresExtensions::default()
            },
        ))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("graph = \"on\""), "{err}");
    if has_vector {
        let err = db
            .setup_extensions(&managed(
                &h,
                PostgresExtensions {
                    vector: Mode::Off,
                    ..PostgresExtensions::default()
                },
            ))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("vector.convert"), "{err}");
    }
    db.close().await;
    h.stop().await.expect("stop");
}

#[tokio::test]
async fn graph_on_refuses_an_engine_without_age_before_initdb() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let layout = Layout::primary(tmp.path());
    let cfg = EngineConfig {
        graph: ExtensionMode::On,
        ..config(&engine, 18_232)
    };
    let err = Engine::prepare(&layout, &cfg).await.expect_err("AGE required");
    assert!(matches!(err, EngineError::Config(_)), "{err}");
    assert!(err.to_string().contains("Apache AGE"), "{err}");
    assert!(!layout.pgdata().exists(), "refused before initdb");

    // `off`: no warning about AGE.
    let cfg = EngineConfig {
        graph: ExtensionMode::Off,
        ..config(&engine, 18_232)
    };
    let h = Engine::prepare(&layout, &cfg).await.expect("graph off");
    assert!(!h.has_age());
    assert!(h.warnings().iter().all(|w| !w.contains("Apache AGE")));
    h.stop().await.expect("stop");
}
