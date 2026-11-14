//! FB-151: a database built by the crates' own services, converted SQLite →
//! PostgreSQL → SQLite, with equal per-table counts and SHA-256 at every
//! step, the services working on each result, and the refusals.
//!
//! Needs a PostgreSQL engine (`FORGEBASE_PG_ENGINE_DIR`,
//! `FORGEBASE_TEST_PG_ENGINE` or `FORGEBASE_TEST_PG_URL`); skips without.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use forgebase_convert::{Conn, ConvertError, ConvertReport, Endpoint, convert};
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer, IndexDefinition, IndexKind, ListQuery};
use forgebase_db::{Backend, Database, DbOptions, DbTarget, SecretString, Statement, TxMode, Value};
use forgebase_schema::{Collection, CollectionType, Field};
use serde_json::{Value as Json, json};

fn field(v: Json) -> Field {
    serde_json::from_value(v).unwrap()
}

async fn sqlite_db(dir: &Path) -> (Database, PathBuf) {
    let path = dir.join("forgebase.db");
    let opts = DbOptions::from_config(&forgebase_core::Config::defaults(dir).database, &path).create_if_missing(true);
    let db = Database::open(opts).await.unwrap();
    db.migrate(None).await.unwrap();
    (db, path)
}

fn pg_url(t: &forgebase_db::testing::TestDb) -> SecretString {
    match t.options().target {
        DbTarget::Postgres { url, .. } => url,
        DbTarget::Sqlite { .. } => unreachable!(),
    }
}

async fn data(db: &Database) -> DataService {
    DataService::new(Arc::new(db.clone()), Arc::new(DefaultAuthorizer))
        .await
        .unwrap()
}

async fn exec(db: &Database, sql: &str, params: Vec<Value>) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(sql, params)).await.unwrap();
    tx.commit().await.unwrap();
}

/// Collections of every shape, records, indexes (manual, unique, partial,
/// full text, trigram), a view, a relation cycle, graph, jobs and a schedule, a
/// queue with a delivery owed, vectors with text, and raw rows of the
/// types the services do not reach (bytea, double precision).
async fn fixtures(db: &Database, dir: &Path) {
    let svc = data(db).await;
    let ctx = AccessContext::system();
    let authors = svc
        .create_collection(
            &ctx,
            Collection::base("authors")
                .with_field(field(json!({"name": "name", "type": "text", "required": true})))
                .with_field(field(json!({"name": "email", "type": "email", "unique": true})))
                .with_field(field(json!({"name": "active", "type": "bool"})))
                .with_field(field(json!({"name": "score", "type": "number"})))
                .with_field(field(json!({"name": "meta", "type": "json"}))),
        )
        .await
        .unwrap();
    let posts = svc
        .create_collection(
            &ctx,
            Collection::base("posts")
                .with_field(field(json!({"name": "title", "type": "text"})))
                .with_field(field(json!({"name": "body", "type": "text"})))
                .with_field(field(
                    json!({"name": "author", "type": "relation", "collection_id": authors.id}),
                ))
                .with_field(field(json!({"name": "tags", "type": "json"}))),
        )
        .await
        .unwrap();
    // A relation cycle: authors.favourite -> posts, posts.author -> authors.
    let mut a2 = svc.get_collection(&ctx, "authors").await.unwrap();
    a2.fields.push(field(
        json!({"name": "favourite", "type": "relation", "collection_id": posts.id}),
    ));
    svc.update_collection(&ctx, "authors", a2).await.unwrap();
    let mut ids = Vec::new();
    for i in 0..30 {
        let r = svc
            .create_record(
                &ctx,
                "authors",
                json!({"name": format!("Ünïcode \"author\"\t{i}\nline"), "email": format!("a{i}@example.com"),
                       "active": i % 2 == 0, "score": f64::from(i) / 3.0,
                       "meta": {"z": i, "a": [1, 2.5, "x"], "nested": {"k": null}}}),
            )
            .await
            .unwrap();
        ids.push(r["id"].as_str().unwrap().to_owned());
    }
    for i in 0..60 {
        svc.create_record(
            &ctx,
            "posts",
            json!({"title": format!("post {i} café"), "body": "hello world, l'été \\ backslash",
                   "author": ids[i % ids.len()], "tags": ["a", i]}),
        )
        .await
        .unwrap();
    }
    svc.create_index(
        &ctx,
        IndexDefinition {
            name: "idx_posts_title".into(),
            ..IndexDefinition::btree("posts", &["title"])
        },
    )
    .await
    .unwrap();
    svc.create_index(
        &ctx,
        IndexDefinition {
            name: "fts_posts".into(),
            kind: IndexKind::Fulltext,
            ..IndexDefinition::btree("posts", &["title", "body"])
        },
    )
    .await
    .unwrap();
    // ADR-0034: SQLite keeps it as a trigram table, PostgreSQL as a GIN index.
    svc.create_index(
        &ctx,
        IndexDefinition {
            name: "tri_posts_title".into(),
            kind: IndexKind::Trigram,
            ..IndexDefinition::btree("posts", &["title"])
        },
    )
    .await
    .unwrap();
    svc.create_collection(
        &ctx,
        Collection {
            kind: CollectionType::View,
            view_query: Some("SELECT id, title FROM posts".into()),
            ..Collection::base("post_titles")
        },
    )
    .await
    .unwrap();

    let graph = forgebase_graph::GraphService::new(svc.clone());
    let n1 = graph
        .create_node(
            &ctx,
            serde_json::from_value(json!({"label": "person", "properties": {"n": 1}})).unwrap(),
        )
        .await
        .unwrap();
    let n2 = graph
        .create_node(
            &ctx,
            serde_json::from_value(json!({"label": "person", "properties": {"n": 2}})).unwrap(),
        )
        .await
        .unwrap();
    graph
        .create_edge(
            &ctx,
            serde_json::from_value(json!({"type": "knows", "source": n1.id, "target": n2.id})).unwrap(),
        )
        .await
        .unwrap();

    let jobs = forgebase_jobs::JobService::new(svc.clone());
    for i in 0..5 {
        jobs.enqueue(
            &ctx,
            serde_json::from_value(json!({"kind": "fixture.work", "payload": {"i": i}})).unwrap(),
        )
        .await
        .unwrap();
    }
    jobs.create_schedule(
        &ctx,
        serde_json::from_value(json!({"name": "nightly", "cron": "0 3 * * *", "kind": "fixture.work"})).unwrap(),
    )
    .await
    .unwrap();

    let queue = forgebase_queue::QueueService::new(
        svc.clone(),
        forgebase_auth::crypto::Keyring::from_master(&[7u8; 32]),
        forgebase_queue::QueueSettings::default(),
    );
    queue
        .create_topic(&ctx, serde_json::from_value(json!({"name": "orders"})).unwrap())
        .await
        .unwrap();
    queue
        .create_subscription(
            &ctx,
            serde_json::from_value(json!({"name": "billing", "topic": "orders"})).unwrap(),
        )
        .await
        .unwrap();
    queue
        .publish(
            &ctx,
            "orders",
            vec![
                serde_json::from_value(json!({"data": {"order": 1}})).unwrap(),
                serde_json::from_value(json!({"data": {"order": 2}})).unwrap(),
            ],
        )
        .await
        .unwrap();
    // One message leased (a delivery row), one still owed.
    let leased = queue
        .pull(
            &ctx,
            "billing",
            serde_json::from_value(json!({"max_messages": 1})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(leased.len(), 1);

    let vectors = forgebase_vector::VectorService::open(
        svc.clone(),
        dir.join("indexes"),
        forgebase_vector::VectorConfig::default(),
    )
    .await
    .unwrap();
    vectors
        .create_collection(
            &ctx,
            serde_json::from_value(
                json!({"name": "docs", "fields": [{"name": "emb", "dimension": 3, "metric": "cosine"}]}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    vectors
        .upsert(
            &ctx,
            "docs",
            (0..10)
                .map(|i| {
                    serde_json::from_value(
                        json!({"id": format!("d{i}"), "vectors": {"emb": [1.0, f64::from(i), 0.5]},
                        "metadata": {"i": i}, "text": format!("document number {i}")}),
                    )
                    .unwrap()
                })
                .collect(),
        )
        .await
        .unwrap();
    vectors.shutdown().await;

    exec(
        db,
        "INSERT INTO _forgebase_auth_keys (kid, alg, secret_enc, status, created_at) VALUES (?1, 'HS256', ?2, 'retired', 1)",
        vec!["fixture-key-1".into(), Value::Blob(vec![0, 1, 2, 0x5c, 0x0a, 255])],
    )
    .await;
}

/// The services read the converted database as they wrote the original.
async fn check_services(db: &Database, dir: &Path) {
    let svc = data(db).await;
    let ctx = AccessContext::system();
    let posts = svc
        .list_records(
            &ctx,
            "posts",
            &ListQuery {
                search: Some("café".into()),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(posts.total_items, Some(60), "full-text search over the recreated index");
    let fuzzy = svc
        .list_records(
            &ctx,
            "posts",
            &ListQuery {
                fuzzy: Some("post 7".into()),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        fuzzy.items.first().map(|r| r["title"].clone()),
        Some(json!("post 7 café")),
        "fuzzy search over the rebuilt trigram index"
    );
    let a = svc
        .list_records(
            &ctx,
            "authors",
            &ListQuery {
                filter: Some("email = 'a7@example.com'".into()),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(a.items.len(), 1);
    assert_eq!(a.items[0]["meta"]["a"], json!([1, 2.5, "x"]));
    assert_eq!(a.items[0]["name"], json!("Ünïcode \"author\"\t7\nline"));
    let view = svc
        .list_records(&ctx, "post_titles", &ListQuery::default())
        .await
        .unwrap();
    assert_eq!(view.total_items, Some(60));
    // A unique index still holds.
    let dup = svc
        .create_record(&ctx, "authors", json!({"name": "x", "email": "a1@example.com"}))
        .await;
    assert!(dup.is_err(), "unique email holds after the conversion");
    // Identity sequences continue past the copied rows.
    svc.create_record(&ctx, "authors", json!({"name": "new", "email": "new@example.com"}))
        .await
        .unwrap();

    let queue = forgebase_queue::QueueService::new(
        svc.clone(),
        forgebase_auth::crypto::Keyring::from_master(&[7u8; 32]),
        forgebase_queue::QueueSettings::default(),
    );
    let got = queue
        .pull(
            &ctx,
            "billing",
            serde_json::from_value(json!({"max_messages": 10})).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        got.len(),
        1,
        "the owed message is delivered and the leased one stays leased"
    );

    let vectors = forgebase_vector::VectorService::open(
        svc.clone(),
        dir.join("indexes"),
        forgebase_vector::VectorConfig::default(),
    )
    .await
    .unwrap();
    let res = vectors
        .search(
            &ctx,
            "docs",
            serde_json::from_value(json!({"field": "emb", "vector": [1.0, 3.0, 0.5], "k": 1})).unwrap(),
        )
        .await
        .unwrap();
    let res = serde_json::to_value(&res).unwrap();
    assert!(res.to_string().contains("\"d3\""), "{res}");
    vectors.shutdown().await;

    let jobs = forgebase_jobs::JobService::new(svc);
    let n = db
        .fetch_all(&Statement::raw("SELECT count(*) FROM _forgebase_jobs"))
        .await
        .unwrap();
    assert_eq!(n.scalar().and_then(Value::as_i64), Some(5));
    jobs.enqueue(&ctx, serde_json::from_value(json!({"kind": "fixture.work"})).unwrap())
        .await
        .unwrap();
}

fn same_tables(a: &ConvertReport, b: &ConvertReport) {
    assert_eq!(a.tables.len(), b.tables.len());
    for (x, y) in a.tables.iter().zip(&b.tables) {
        assert_eq!(x, y, "table {} differs between the conversions", x.table);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn round_trip_sqlite_postgres_sqlite_keeps_every_table() {
    let Some(pg) = forgebase_db::testing::postgres_test_db().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let (src_dir, back_dir) = (tmp.path().join("a"), tmp.path().join("b"));
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&back_dir).unwrap();
    let (src, src_path) = sqlite_db(&src_dir).await;
    fixtures(&src, &src_dir).await;
    // A raw-SQL index the conversion must report, not recreate.
    exec(
        &src,
        "CREATE INDEX raw_posts_body ON posts (substr(body, 1, 3))",
        vec![],
    )
    .await;

    let url = pg_url(&pg);
    let lines = std::sync::Mutex::new(Vec::<String>::new());
    let progress = |l: &str| lines.lock().unwrap().push(l.to_owned());
    let to_pg = convert(
        Endpoint {
            db: &src,
            conn: &Conn::Sqlite(src_path.clone()),
        },
        Endpoint {
            db: &pg,
            conn: &Conn::Postgres(url.clone()),
        },
        &progress,
    )
    .await
    .unwrap();
    assert_eq!(to_pg.from, "sqlite");
    assert!(to_pg.collections >= 3);
    assert_eq!(
        to_pg.indexes, 3,
        "the manual b-tree, the full-text and the trigram index"
    );
    assert_eq!(to_pg.rebuilt.len(), 1, "{:?}", to_pg.rebuilt);
    assert_eq!(
        (to_pg.rebuilt[0].name.as_str(), to_pg.rebuilt[0].rows),
        ("tri_posts_title", 60)
    );
    assert!(to_pg.total_rows() > 100);
    assert_eq!(to_pg.derived_rows, 10, "vector text rebuilt for full text");
    assert!(
        to_pg.manual.iter().any(|m| m.name == "raw_posts_body"),
        "{:?}",
        to_pg.manual
    );
    let nonempty: Vec<&str> = to_pg
        .tables
        .iter()
        .filter(|t| t.rows > 0)
        .map(|t| t.table.as_str())
        .collect();
    for t in [
        "_forgebase_collections",
        "_forgebase_indexes",
        "_forgebase_schema_history",
        "_forgebase_graph_nodes",
        "_forgebase_jobs",
        "_forgebase_job_schedules",
        "_forgebase_queue_deliveries",
        "_forgebase_vector_values",
        "_forgebase_auth_keys",
        "authors",
        "posts",
    ] {
        assert!(nonempty.contains(&t), "{t} was copied with rows: {nonempty:?}");
    }
    assert!(lines.lock().unwrap().iter().any(|l| l == "verifying"));

    // A non-empty target is refused.
    let again = convert(
        Endpoint {
            db: &src,
            conn: &Conn::Sqlite(src_path.clone()),
        },
        Endpoint {
            db: &pg,
            conn: &Conn::Postgres(url.clone()),
        },
        &|_| {},
    )
    .await
    .unwrap_err();
    assert!(matches!(again, ConvertError::Precondition(_)), "{again}");

    // Back to SQLite: the same rows and hashes.
    let (back, back_path) = sqlite_db(&back_dir).await;
    let to_sqlite = convert(
        Endpoint {
            db: &pg,
            conn: &Conn::Postgres(url.clone()),
        },
        Endpoint {
            db: &back,
            conn: &Conn::Sqlite(back_path.clone()),
        },
        &|_| {},
    )
    .await
    .unwrap();
    same_tables(&to_pg, &to_sqlite);
    assert_eq!(to_sqlite.derived_rows, 10);
    assert_eq!(to_sqlite.rebuilt.len(), 1, "{:?}", to_sqlite.rebuilt);
    assert_eq!(
        to_sqlite.rebuilt[0].trigrams,
        Some(side_rows(&src, "tri_posts_title").await),
        "the trigram table holds what the source's held"
    );
    assert!(
        !to_sqlite.manual.iter().any(|m| m.name.contains("tri_posts_title")),
        "{:?}",
        to_sqlite.manual
    );
    assert!(
        !to_pg.manual.iter().any(|m| m.name.contains("tri_posts_title")),
        "{:?}",
        to_pg.manual
    );

    // The suite of services runs on both results (after the hashes, since
    // it writes).
    check_services(&pg, &src_dir.join("pg")).await;
    check_services(&back, &back_dir).await;
    src.close().await;
    back.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn to_sqlite_refuses_postgres_only_objects() {
    let Some(pg) = forgebase_db::testing::postgres_test_db().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let svc = data(&pg).await;
    let ctx = AccessContext::system();
    svc.create_collection(
        &ctx,
        Collection::base("things")
            .with_field(field(json!({"name": "n", "type": "number"})))
            .with_field(field(json!({"name": "t", "type": "text"}))),
    )
    .await
    .unwrap();
    // A trigram index (pg_trgm where the engine has it): not refused since
    // the ADR-0034 addendum, SQLite builds it again.
    pg.setup_extensions(&forgebase_db::ExtensionSetup::default())
        .await
        .unwrap();
    svc.create_index(
        &ctx,
        IndexDefinition {
            name: "things_tri".into(),
            kind: IndexKind::Trigram,
            ..IndexDefinition::btree("things", &["t"])
        },
    )
    .await
    .unwrap();
    let mut tx = pg.begin(TxMode::Normal).await.unwrap();
    for sql in [
        "CREATE FUNCTION public.fb151_double(x integer) RETURNS integer LANGUAGE plpgsql AS $$ BEGIN RETURN x * 2; END $$",
        "CREATE VIEW public.raw_view AS SELECT 1 AS one",
        "CREATE TYPE public.mood AS ENUM ('ok', 'sad')",
        "ALTER TABLE public.things ENABLE ROW LEVEL SECURITY",
        "CREATE POLICY hand_made ON public.things USING (true)",
    ] {
        tx.execute(&Statement::raw(sql)).await.unwrap();
    }
    tx.commit().await.unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (back, back_path) = sqlite_db(tmp.path()).await;
    let err = convert(
        Endpoint {
            db: &pg,
            conn: &Conn::Postgres(pg_url(&pg)),
        },
        Endpoint {
            db: &back,
            conn: &Conn::Sqlite(back_path),
        },
        &|_| {},
    )
    .await
    .unwrap_err();
    let ConvertError::Refused(problems) = &err else {
        panic!("{err}");
    };
    let text = problems.join("\n");
    for needle in ["fb151_double", "raw_view", "mood", "hand_made"] {
        assert!(text.contains(needle), "{needle} is listed:\n{text}");
    }
    assert!(!text.contains("trigram") && !text.contains("things_tri"), "{text}");
    // Nothing was made on the target.
    let n = back
        .fetch_all(&Statement::raw("SELECT count(*) FROM _forgebase_collections"))
        .await
        .unwrap();
    assert_eq!(n.scalar().and_then(Value::as_i64), Some(0));
    back.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn to_postgres_refuses_a_stale_or_inconsistent_source() {
    let Some(pg) = forgebase_db::testing::postgres_test_db().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let (src, src_path) = sqlite_db(tmp.path()).await;
    let svc = data(&src).await;
    let ctx = AccessContext::system();
    let a = svc
        .create_collection(
            &ctx,
            Collection::base("a").with_field(field(json!({"name": "t", "type": "text"}))),
        )
        .await
        .unwrap();
    svc.create_collection(
        &ctx,
        Collection::base("b").with_field(field(json!({"name": "a", "type": "relation", "collection_id": a.id}))),
    )
    .await
    .unwrap();
    // A dangling relation, as only a broken file can hold.
    let ok = svc.create_record(&ctx, "a", json!({"t": "x"})).await.unwrap();
    svc.create_record(&ctx, "b", json!({"a": ok["id"]})).await.unwrap();
    {
        use sqlx::ConnectOptions as _;
        let mut c = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&src_path)
            .foreign_keys(false)
            .connect()
            .await
            .unwrap();
        sqlx::query("UPDATE b SET a = 'missing'").execute(&mut c).await.unwrap();
        sqlx::Connection::close(c).await.unwrap();
    }
    let err = convert(
        Endpoint {
            db: &src,
            conn: &Conn::Sqlite(src_path.clone()),
        },
        Endpoint {
            db: &pg,
            conn: &Conn::Postgres(pg_url(&pg)),
        },
        &|_| {},
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("foreign keys"), "{err}");
    src.close().await;
}

async fn installed(db: &Database, ext: &str) -> bool {
    db.fetch_all(&Statement::new(
        "SELECT count(*) FROM pg_extension WHERE extname = ?1",
        vec![ext.into()],
    ))
    .await
    .unwrap()
    .scalar()
    .and_then(Value::as_i64)
    .unwrap_or(0)
        > 0
}

/// A project born on PostgreSQL with the server's extension setup (FB-161):
/// vectors in pgvector and deliveries in pgmq where the engine has them.
/// `--to sqlite` is refused while the data sits in an extension, and goes
/// through once `prepare_portable` has moved it to portable rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn to_sqlite_brings_extension_data_to_portable_rows() {
    let Some(pg) = forgebase_db::testing::postgres_test_db().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    pg.setup_extensions(&forgebase_db::ExtensionSetup::default())
        .await
        .unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (src_dir, back_dir) = (tmp.path().join("pg"), tmp.path().join("back"));
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::create_dir_all(&back_dir).unwrap();
    fixtures(&pg, &src_dir).await;
    let (pgvector, pgmq) = (installed(&pg, "vector").await, installed(&pg, "pgmq").await);
    eprintln!("pgvector {pgvector}, pgmq {pgmq}");

    let (back, back_path) = sqlite_db(&back_dir).await;
    let source = Endpoint {
        db: &pg,
        conn: &Conn::Postgres(pg_url(&pg)),
    };
    let target = Endpoint {
        db: &back,
        conn: &Conn::Sqlite(back_path.clone()),
    };
    if pgvector || pgmq {
        let err = convert(source, target, &|_| {}).await.unwrap_err();
        let ConvertError::Refused(problems) = &err else {
            panic!("{err}");
        };
        let text = problems.join("\n");
        if pgvector {
            assert!(text.contains("vector.convert"), "{text}");
        }
        if pgmq {
            assert!(text.contains("queue.convert"), "{text}");
        }
    }

    let prepared = forgebase_convert::prepare_portable(&pg, &src_dir.join("indexes"))
        .await
        .unwrap();
    assert_eq!(prepared.vectors.is_some(), pgvector, "{prepared:?}");
    assert_eq!(prepared.deliveries.is_some(), pgmq, "{prepared:?}");

    let report = convert(source, target, &|_| {}).await.unwrap();
    assert_eq!(report.from, "postgres");
    assert_eq!(report.derived_rows, 10);
    let meta = back
        .fetch_all(&Statement::raw(
            "SELECT count(*) FROM _forgebase_meta WHERE key LIKE 'ext.%'",
        ))
        .await
        .unwrap();
    assert_eq!(
        meta.scalar().and_then(Value::as_i64),
        Some(0),
        "extension pins are not copied"
    );
    check_services(&back, &back_dir).await;
    back.close().await;
}

/// A PostgreSQL project whose server has run with Apache AGE (the default
/// `auto` mode of the bundled engine) holds the graph accelerator's derived
/// tables, with an AGE `graphid` column. They are rebuilt where AGE runs,
/// so `--to sqlite` neither refuses them nor copies them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn to_sqlite_skips_the_derived_age_tables() {
    let Some(pg) = forgebase_db::testing::postgres_test_db().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    // AGE must be loaded in ForgeBase's sessions, as the bundled engine
    // sets for its roles; a fresh pool picks the setting up.
    exec(
        &pg,
        "DO $fb$ BEGIN EXECUTE format('ALTER DATABASE %I SET session_preload_libraries = %L', \
         current_database(), '$libdir/plugins/age'); END $fb$",
        vec![],
    )
    .await;
    let db = Database::open(pg.options()).await.unwrap();
    db.setup_extensions(&forgebase_db::ExtensionSetup::default())
        .await
        .unwrap();
    if !installed(&db, "age").await {
        eprintln!("skipped: the engine has no Apache AGE");
        db.close().await;
        return;
    }
    let derived = db
        .fetch_all(&Statement::raw(
            "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = 'forgebase' AND c.relname IN ('_forgebase_graph_age_ids', '_forgebase_graph_age_state')",
        ))
        .await
        .unwrap();
    assert_eq!(derived.scalar().and_then(Value::as_i64), Some(2), "the graph DDL ran");

    let tmp = tempfile::tempdir().unwrap();
    let (back, back_path) = sqlite_db(tmp.path()).await;
    let report = convert(
        Endpoint {
            db: &db,
            conn: &Conn::Postgres(pg_url(&pg)),
        },
        Endpoint {
            db: &back,
            conn: &Conn::Sqlite(back_path),
        },
        &|_| {},
    )
    .await
    .unwrap();
    assert_eq!(report.from, "postgres");
    let copied = back
        .fetch_all(&Statement::raw(
            "SELECT count(*) FROM sqlite_master WHERE name LIKE '%graph_age%'",
        ))
        .await
        .unwrap();
    assert_eq!(
        copied.scalar().and_then(Value::as_i64),
        Some(0),
        "AGE tables are not copied"
    );
    back.close().await;
    db.close().await;
}

/// Rows of the SQLite trigram table of index `index`.
async fn side_rows(db: &Database, index: &str) -> u64 {
    let table = forgebase_data::fuzzy::sqlite_objects(index).table;
    let n = db
        .fetch_all(&Statement::raw(format!("SELECT count(*) FROM \"{table}\"")))
        .await
        .unwrap();
    u64::try_from(n.scalar().and_then(Value::as_i64).unwrap()).unwrap()
}

const FIRST: [&str; 12] = [
    "Jon", "John", "Jonathan", "Joan", "Maria", "Mario", "Marianne", "Ann", "Anna", "Annette", "Peter", "Petra",
];
const LAST: [&str; 8] = ["Smith", "Smyth", "Garcia", "Garza", "Brown", "Browne", "Lee", "Leigh"];
const CITY: [&str; 7] = ["London", "Londonderry", "Lyon", "Boston", "Austin", "Berlin", "Bern"];

/// `people` (name, city) with a two-key trigram index and 98 records:
/// ASCII names (the test engine's `C` locale makes pg_trgm treat other
/// characters as separators, ADR-0034 §1.1), one without a city and one
/// with an empty name.
async fn people(db: &Database) {
    let svc = data(db).await;
    let ctx = AccessContext::system();
    svc.create_collection(
        &ctx,
        Collection::base("people")
            .with_field(field(json!({"name": "name", "type": "text"})))
            .with_field(field(json!({"name": "city", "type": "text"}))),
    )
    .await
    .unwrap();
    for i in 0..96 {
        svc.create_record(
            &ctx,
            "people",
            json!({"name": format!("{} {}", FIRST[i % 12], LAST[(i / 12) % 8]), "city": CITY[i % 7]}),
        )
        .await
        .unwrap();
    }
    svc.create_record(&ctx, "people", json!({"name": "Jon Smithers"}))
        .await
        .unwrap();
    svc.create_record(&ctx, "people", json!({"name": "", "city": "London"}))
        .await
        .unwrap();
    svc.create_index(
        &ctx,
        IndexDefinition {
            name: "people_fuzzy".into(),
            kind: IndexKind::Trigram,
            ..IndexDefinition::btree("people", &["name", "city"])
        },
    )
    .await
    .unwrap();
}

/// Fuzzy queries over both keys, each key alone and an explicit threshold.
fn people_queries() -> Vec<ListQuery> {
    let q = |text: &str, fields: &[&str], threshold: Option<f64>| ListQuery {
        fuzzy: Some(text.into()),
        fuzzy_fields: fields.iter().map(|f| (*f).to_owned()).collect(),
        fuzzy_threshold: threshold,
        per_page: Some(500),
        ..ListQuery::default()
    };
    vec![
        q("jon smith", &[], None),
        q("jonh", &["name"], None),
        q("londn", &["city"], None),
        q("mari garcia", &["name"], Some(0.2)),
        q("bern", &[], None),
        q("anette browne", &["name", "city"], Some(0.2)),
    ]
}

/// Per query: the ids, names and cities found, best match first.
async fn fuzzy_answers(db: &Database) -> Vec<Vec<String>> {
    let svc = data(db).await;
    let ctx = AccessContext::system();
    let mut out: Vec<Vec<String>> = Vec::new();
    for q in people_queries() {
        let r = svc.list_records(&ctx, "people", &q).await.unwrap();
        out.push(
            r.items
                .iter()
                .map(|i| format!("{} {} {}", i["id"], i["name"], i["city"]))
                .collect(),
        );
    }
    assert!(
        out.iter().all(|a| !a.is_empty()),
        "every query finds something: {out:?}"
    );
    out
}

/// Fuzzy answers follow inserts, updates and deletes after a conversion.
async fn trigram_maintained(db: &Database) {
    let svc = data(db).await;
    let ctx = AccessContext::system();
    let find = async |text: &str| -> Vec<String> {
        svc.list_records(
            &ctx,
            "people",
            &ListQuery {
                fuzzy: Some(text.into()),
                per_page: Some(500),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap()
        .items
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect()
    };
    let new = svc
        .create_record(&ctx, "people", json!({"name": "Zebulon Quixote", "city": "Zanzibar"}))
        .await
        .unwrap();
    let new_id = new["id"].as_str().unwrap().to_owned();
    assert_eq!(
        find("zebulon quixote").await.first(),
        Some(&new_id),
        "an insert is indexed"
    );
    let jon = find("jon smith").await;
    let moved = jon.first().unwrap().clone();
    svc.update_record(
        &ctx,
        "people",
        &moved,
        json!({"name": "Xavier Yellowstone"}),
        forgebase_data::UpdateOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        find("xavier yellowstone").await.first(),
        Some(&moved),
        "an update is indexed"
    );
    assert!(!find("jon smith").await.contains(&moved), "the old value is gone");
    svc.delete_record(&ctx, "people", &new_id, None).await.unwrap();
    assert!(!find("zebulon quixote").await.contains(&new_id), "a delete is indexed");
}

/// The trigram feature of a fresh PostgreSQL database in `mode`, every
/// other accelerator off; whether pg_trgm serves it.
async fn trigram_mode(db: &Database, mode: forgebase_core::config::ExtensionMode) -> bool {
    use forgebase_core::config::{ExtensionMode, PostgresExtensions};
    db.setup_extensions(&forgebase_db::ExtensionSetup {
        modes: PostgresExtensions {
            trigram: mode,
            vector: ExtensionMode::Off,
            graph: ExtensionMode::Off,
            queue: ExtensionMode::Off,
            cron: ExtensionMode::Off,
        },
        ..forgebase_db::ExtensionSetup::default()
    })
    .await
    .unwrap();
    db.feature_active(forgebase_db::Feature::Trigram)
}

/// ADR-0034 addendum: a SQLite trigram index goes to PostgreSQL (pg_trgm
/// where the target uses it, else the portable GIN index) and back to a
/// SQLite trigram table; every fuzzy query answers the same records in the
/// same order at each step, and the index follows later writes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn trigram_indexes_convert_both_ways_with_the_same_answers() {
    use forgebase_core::config::ExtensionMode;
    for mode in [ExtensionMode::Auto, ExtensionMode::Off] {
        let Some(pg) = forgebase_db::testing::postgres_test_db().await else {
            eprintln!("skipped: no PostgreSQL engine configured");
            return;
        };
        let pg_trgm = trigram_mode(&pg, mode).await;
        let engine = if pg_trgm { "pg_trgm" } else { "portable" };
        eprintln!("trigram = {mode:?}: {engine}");
        let tmp = tempfile::tempdir().unwrap();
        let (src_dir, back_dir) = (tmp.path().join("a"), tmp.path().join("b"));
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&back_dir).unwrap();
        let (src, src_path) = sqlite_db(&src_dir).await;
        people(&src).await;
        let before = fuzzy_answers(&src).await;
        let src_trigrams = side_rows(&src, "people_fuzzy").await;
        assert!(src_trigrams > 0);

        let lines = std::sync::Mutex::new(Vec::<String>::new());
        let progress = |l: &str| lines.lock().unwrap().push(l.to_owned());
        let to_pg = convert(
            Endpoint {
                db: &src,
                conn: &Conn::Sqlite(src_path.clone()),
            },
            Endpoint {
                db: &pg,
                conn: &Conn::Postgres(pg_url(&pg)),
            },
            &progress,
        )
        .await
        .unwrap();
        assert_eq!(to_pg.rebuilt.len(), 1, "{:?}", to_pg.rebuilt);
        let b = &to_pg.rebuilt[0];
        assert_eq!(
            (
                b.kind.as_str(),
                b.name.as_str(),
                b.collection.as_str(),
                b.engine.as_str()
            ),
            ("trigram", "people_fuzzy", "people", engine)
        );
        assert_eq!((b.rows, b.trigrams), (98, None));
        assert!(
            lines
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("built trigram index people_fuzzy on people")),
            "{:?}",
            lines.lock().unwrap()
        );
        assert!(
            !to_pg
                .tables
                .iter()
                .any(|t| t.table.starts_with("_forgebase_trgm_people")),
            "the trigram table is not copied"
        );
        assert!(to_pg.manual.is_empty(), "{:?}", to_pg.manual);
        let def = pg
            .fetch_all(&Statement::raw(
                "SELECT indexdef FROM pg_indexes WHERE schemaname = 'public' AND indexname = 'people_fuzzy'",
            ))
            .await
            .unwrap();
        let def = def.scalar().and_then(Value::as_str).unwrap().to_owned();
        let marker = if pg_trgm { "gin_trgm_ops" } else { "_forgebase_trgm(" };
        assert!(def.contains(marker), "{def}");
        let def_row = pg
            .fetch_all(&Statement::raw(
                "SELECT definition FROM _forgebase_indexes WHERE name = 'people_fuzzy'",
            ))
            .await
            .unwrap();
        let def_row: Json = serde_json::from_str(def_row.scalar().and_then(Value::as_str).unwrap()).unwrap();
        assert_eq!(def_row["kind"], "trigram", "the definition carried over");
        assert_eq!(fuzzy_answers(&pg).await, before, "PostgreSQL answers as SQLite did");

        let (back, back_path) = sqlite_db(&back_dir).await;
        let to_sqlite = convert(
            Endpoint {
                db: &pg,
                conn: &Conn::Postgres(pg_url(&pg)),
            },
            Endpoint {
                db: &back,
                conn: &Conn::Sqlite(back_path.clone()),
            },
            &|_| {},
        )
        .await
        .unwrap();
        same_tables(&to_pg, &to_sqlite);
        assert_eq!(to_sqlite.rebuilt.len(), 1, "{:?}", to_sqlite.rebuilt);
        let b = &to_sqlite.rebuilt[0];
        assert_eq!((b.engine.as_str(), b.rows), ("portable", 98));
        assert_eq!(b.trigrams, Some(src_trigrams), "the same trigram rows as the source");
        assert_eq!(side_rows(&back, "people_fuzzy").await, src_trigrams);
        assert!(to_sqlite.manual.is_empty(), "{:?}", to_sqlite.manual);
        let triggers = back
            .fetch_all(&Statement::raw(
                "SELECT count(*) FROM sqlite_master WHERE type = 'trigger' AND name LIKE '\\_forgebase\\_trgm\\_people\\_fuzzy\\_a_' ESCAPE '\\'",
            ))
            .await
            .unwrap();
        assert_eq!(
            triggers.scalar().and_then(Value::as_i64),
            Some(3),
            "the triggers are back"
        );
        assert_eq!(fuzzy_answers(&back).await, before, "SQLite answers as before");

        // Writes after the conversions (the hashes above are compared first).
        trigram_maintained(&pg).await;
        trigram_maintained(&back).await;
        src.close().await;
        back.close().await;
    }
}

/// A copy that fails after the trigram index was taken out leaves the
/// target as step 2 made it (the index is back, no rows) and the source
/// as it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_copy_keeps_the_trigram_index_on_the_target() {
    let Some(pg) = forgebase_db::testing::postgres_test_db().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let tmp = tempfile::tempdir().unwrap();
    let (src, src_path) = sqlite_db(tmp.path()).await;
    people(&src).await;
    // PostgreSQL text cannot hold NUL: the copy of `people` fails.
    exec(
        &src,
        "UPDATE people SET city = 'Lon' || char(0) || 'don' WHERE name = 'Jon Smithers'",
        vec![],
    )
    .await;
    let src_trigrams = side_rows(&src, "people_fuzzy").await;
    let err = convert(
        Endpoint {
            db: &src,
            conn: &Conn::Sqlite(src_path.clone()),
        },
        Endpoint {
            db: &pg,
            conn: &Conn::Postgres(pg_url(&pg)),
        },
        &|_| {},
    )
    .await
    .unwrap_err();
    assert!(matches!(err, ConvertError::Copy { .. }), "{err}");
    let ix = pg
        .fetch_all(&Statement::raw(
            "SELECT count(*) FROM pg_indexes WHERE schemaname = 'public' AND indexname = 'people_fuzzy'",
        ))
        .await
        .unwrap();
    assert_eq!(ix.scalar().and_then(Value::as_i64), Some(1), "the index is back");
    let rows = pg
        .fetch_all(&Statement::raw("SELECT count(*) FROM public.people"))
        .await
        .unwrap();
    assert_eq!(rows.scalar().and_then(Value::as_i64), Some(0), "nothing was copied");
    assert_eq!(
        side_rows(&src, "people_fuzzy").await,
        src_trigrams,
        "the source is unchanged"
    );
    src.close().await;
}
