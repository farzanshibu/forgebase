//! The extension layer (ADR-0029 §1.1, FB-161): each feature `auto`, `on`
//! and `off`, the stored engine of stateful features, detection without
//! changes. PostgreSQL tests run with `FORGEBASE_TEST_BACKEND=postgres` and
//! adapt to the engine: FB-130's engine has pgvector, pgmq and pg_trgm
//! (pg_cron is not preloaded by the test engine, AGE is not built yet);
//! Homebrew's has only pg_trgm.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use forgebase_core::ErrorCode;
use forgebase_core::config::{ExtensionMode, PostgresExtensions};
use forgebase_db::testing::{TestDb, postgres_test_db, selected_backend, test_backend_of};
use forgebase_db::{
    Backend, BackendKind, Database, ExtensionError, ExtensionSetup, Feature, FeatureState, Statement, Value,
};

async fn pg() -> Option<TestDb> {
    if selected_backend() != BackendKind::Postgres {
        return None;
    }
    postgres_test_db().await
}

fn setup(modes: PostgresExtensions) -> ExtensionSetup {
    ExtensionSetup {
        modes,
        ..ExtensionSetup::default()
    }
}

fn all(mode: ExtensionMode) -> PostgresExtensions {
    PostgresExtensions {
        vector: mode,
        graph: mode,
        queue: mode,
        cron: mode,
        trigram: mode,
    }
}

async fn scalar(db: &Database, sql: &str, params: Vec<Value>) -> Value {
    db.fetch_all(&Statement::new(sql, params))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .scalar()
        .cloned()
        .unwrap_or(Value::Null)
}

async fn available(db: &Database, ext: &str) -> bool {
    scalar(
        db,
        "SELECT count(*) FROM pg_available_extensions WHERE name = ?1",
        vec![ext.into()],
    )
    .await
        == Value::Integer(1)
}

async fn installed(db: &Database, ext: &str) -> Option<String> {
    match scalar(
        db,
        "SELECT n.nspname::text FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace \
         WHERE e.extname = ?1",
        vec![ext.into()],
    )
    .await
    {
        Value::Text(s) => Some(s),
        _ => None,
    }
}

async fn meta(db: &Database, key: &str) -> Option<String> {
    db.meta_get(key).await.unwrap()
}

#[tokio::test]
async fn auto_uses_what_the_server_offers_and_falls_back_for_the_rest() {
    let Some(db) = pg().await else { return };
    let has_vector = available(&db, "vector").await;
    let has_pgmq = available(&db, "pgmq").await;
    assert!(
        available(&db, "pg_trgm").await,
        "pg_trgm is contrib: every engine has it"
    );
    assert!(db.extensions().is_none(), "nothing before the setup");

    let report = db
        .setup_extensions(&setup(PostgresExtensions::default()))
        .await
        .unwrap();
    assert_eq!(report.features.len(), 5);
    let order: Vec<Feature> = report.features.iter().map(|s| s.feature).collect();
    assert_eq!(order, Feature::ALL);

    // pg_trgm: created in the forgebase schema, usable by the server.
    let trgm = report.get(Feature::Trigram).unwrap();
    assert_eq!(trgm.state, FeatureState::Active, "{trgm:?}");
    assert!(trgm.engine.starts_with("pg_trgm "), "{}", trgm.engine);
    assert_eq!(trgm.schema.as_deref(), Some("forgebase"));
    assert_eq!(installed(&db, "pg_trgm").await.as_deref(), Some("forgebase"));
    assert_eq!(
        scalar(&db, "SELECT similarity('forgebase', 'forgebass') > 0.5", vec![]).await,
        Value::Integer(1)
    );

    // pgvector: the versioned DDL adds the vector column; the data engine is recorded.
    let vector = report.get(Feature::Vector).unwrap();
    if has_vector {
        assert_eq!(vector.state, FeatureState::Active, "{vector:?}");
        assert!(vector.engine.starts_with("pgvector "), "{}", vector.engine);
        assert_eq!(vector.schema_version, 1);
        assert_eq!(meta(&db, "ext.vector.schema").await.as_deref(), Some("1"));
        assert_eq!(meta(&db, "ext.vector.engine").await.as_deref(), Some("extension"));
        assert_eq!(
            scalar(
                &db,
                "SELECT format_type(atttypid, atttypmod) || ' ' || (NOT attnotnull)::text FROM pg_attribute \
                 WHERE attrelid = '_forgebase_vector_values'::regclass AND attname = ?1",
                vec!["vec".into()],
            )
            .await,
            Value::Text("vector true".into())
        );
        assert_eq!(
            scalar(
                &db,
                "SELECT attnotnull::int FROM pg_attribute WHERE attrelid = '_forgebase_vector_values'::regclass \
                 AND attname = 'vector'",
                vec![],
            )
            .await,
            Value::Integer(0),
            "the portable column is nullable on accelerated databases"
        );
        assert!(db.feature_active(Feature::Vector));
    } else {
        assert_eq!(vector.state, FeatureState::Portable);
        assert!(vector.reason.as_deref().unwrap().contains("pg_available_extensions"));
        assert_eq!(meta(&db, "ext.vector.engine").await.as_deref(), Some("portable"));
        assert!(!db.feature_active(Feature::Vector));
    }

    // pgmq: created by the server's own role, which can make queues.
    let queue = report.get(Feature::Queue).unwrap();
    assert_eq!(queue.state == FeatureState::Active, has_pgmq, "{queue:?}");
    if has_pgmq {
        assert_eq!(queue.schema.as_deref(), Some("pgmq"));
        let mut tx = db.begin(forgebase_db::TxMode::Normal).await.unwrap();
        tx.execute(&Statement::new("SELECT pgmq.create('fb161_probe')", vec![]))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            scalar(&db, "SELECT count(*) FROM pgmq.list_queues()", vec![]).await,
            Value::Integer(1)
        );
    }

    // pg_cron is not preloaded by the test engine, AGE is not in it.
    let cron = report.get(Feature::Cron).unwrap();
    assert_eq!(cron.state, FeatureState::Portable);
    assert!(
        cron.reason.as_deref().unwrap().contains("shared_preload_libraries")
            || cron.reason.as_deref().unwrap().contains("pg_available_extensions"),
        "{cron:?}"
    );
    let graph = report.get(Feature::Graph).unwrap();
    assert_eq!(
        (graph.state, graph.engine.as_str()),
        (FeatureState::Portable, "portable")
    );
    assert!(installed(&db, "pg_cron").await.is_none());
    assert!(installed(&db, "age").await.is_none());

    // Kept on the handle, shown by info().
    let info = db.info().await.unwrap();
    assert_eq!(info.features, report.features);
    assert_eq!(db.extensions().as_deref(), Some(&report));

    // A restart sets up the same way and applies no DDL twice.
    let again = db
        .setup_extensions(&setup(PostgresExtensions::default()))
        .await
        .unwrap();
    assert_eq!(again, report);
}

#[tokio::test]
async fn on_refuses_the_start_without_the_extension() {
    let Some(db) = pg().await else { return };
    let modes = PostgresExtensions {
        graph: ExtensionMode::On,
        cron: ExtensionMode::On,
        trigram: ExtensionMode::On,
        ..PostgresExtensions::default()
    };
    let err = db.setup_extensions(&setup(modes)).await.unwrap_err();
    assert_eq!(err.code(), "extension_unavailable");
    let ExtensionError::Refused(problems) = &err else {
        panic!("{err}")
    };
    // Both missing ones in one message; trigram (present) is not a problem.
    assert_eq!(problems.len(), 2, "{problems:?}");
    assert!(problems.iter().any(|p| p.starts_with("graph = \"on\"")), "{problems:?}");
    assert!(problems.iter().any(|p| p.starts_with("cron = \"on\"")), "{problems:?}");
    assert!(db.extensions().is_none(), "a refused setup is not kept");

    // `on` with the extension present works like auto.
    let modes = PostgresExtensions {
        trigram: ExtensionMode::On,
        ..PostgresExtensions::default()
    };
    let report = db.setup_extensions(&setup(modes)).await.unwrap();
    assert!(report.is_active(Feature::Trigram));
}

#[tokio::test]
async fn off_keeps_every_feature_portable_and_creates_nothing() {
    let Some(db) = pg().await else { return };
    let has_vector = available(&db, "vector").await;
    let report = db.setup_extensions(&setup(all(ExtensionMode::Off))).await.unwrap();
    for s in &report.features {
        assert_eq!((s.state, s.engine.as_str()), (FeatureState::Off, "portable"), "{s:?}");
        assert_eq!(s.mode, ExtensionMode::Off);
        assert!(s.reason.is_none());
    }
    for ext in ["vector", "pgmq", "pg_trgm", "pg_cron", "age"] {
        assert!(installed(&db, ext).await.is_none(), "{ext} was created");
    }
    // Stateful features remember that their data is portable…
    assert_eq!(meta(&db, "ext.vector.engine").await.as_deref(), Some("portable"));
    assert_eq!(meta(&db, "ext.queue.engine").await.as_deref(), Some("portable"));
    assert_eq!(meta(&db, "ext.trigram.engine").await, None);
    assert_eq!(meta(&db, "ext.vector.schema").await, None);

    // …so an extension that is there later changes nothing in auto,
    let report = db
        .setup_extensions(&setup(PostgresExtensions::default()))
        .await
        .unwrap();
    let vector = report.get(Feature::Vector).unwrap();
    assert_eq!(vector.state, FeatureState::Portable);
    if has_vector {
        assert!(
            vector.reason.as_deref().unwrap().contains("vector.convert"),
            "{vector:?}"
        );
        assert!(installed(&db, "vector").await.is_none());
    }
    // while the derived-state features follow their mode.
    assert!(report.is_active(Feature::Trigram));

    // `on` contradicts the stored engine.
    let err = db
        .setup_extensions(&setup(PostgresExtensions {
            vector: ExtensionMode::On,
            ..PostgresExtensions::default()
        }))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("vector = \"on\", but this database stores"),
        "{err}"
    );
}

#[tokio::test]
async fn data_kept_in_an_extension_cannot_be_switched_off() {
    let Some(db) = pg().await else { return };
    if !available(&db, "vector").await {
        eprintln!("skipped: the test engine has no pgvector");
        return;
    }
    db.setup_extensions(&setup(PostgresExtensions::default()))
        .await
        .unwrap();
    assert_eq!(meta(&db, "ext.vector.engine").await.as_deref(), Some("extension"));
    let err = db
        .setup_extensions(&setup(PostgresExtensions {
            vector: ExtensionMode::Off,
            ..PostgresExtensions::default()
        }))
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("vector = \"off\", but this database keeps its vector data in vector"),
        "{text}"
    );
    assert!(text.contains("vector.convert"), "{text}");
}

#[tokio::test]
async fn detection_changes_nothing() {
    let Some(db) = pg().await else { return };
    let has_vector = available(&db, "vector").await;
    let report = db.detect_extensions(&PostgresExtensions::default()).await.unwrap();
    assert_eq!(report.features.len(), 5);
    let trgm = report.get(Feature::Trigram).unwrap();
    assert_eq!(trgm.state, FeatureState::Portable);
    assert!(
        trgm.reason.as_deref().unwrap().contains("not installed yet"),
        "{trgm:?}"
    );
    assert!(trgm.available.is_some());
    assert!(installed(&db, "pg_trgm").await.is_none());
    if has_vector {
        assert!(installed(&db, "vector").await.is_none());
    }
    assert_eq!(meta(&db, "ext.vector.engine").await, None);
    assert!(db.extensions().is_none(), "detection is not kept");
    assert!(db.info().await.unwrap().features.is_empty());
}

#[tokio::test]
async fn sqlite_has_no_extensions() {
    let db = test_backend_of(BackendKind::Sqlite).await;
    let report = db.setup_extensions(&setup(all(ExtensionMode::On))).await.unwrap();
    assert!(report.features.is_empty());
    assert!(
        db.detect_extensions(&all(ExtensionMode::On))
            .await
            .unwrap()
            .features
            .is_empty()
    );
    assert!(db.extensions().is_none());
    assert!(!db.feature_active(Feature::Vector));
    assert!(db.info().await.unwrap().features.is_empty());
}
