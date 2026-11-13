//! Full-text query and trigram search (ADR-0029 §1.8, FB-166; ADR-0034):
//! `search` reads words as text (`plainto_tsquery`), the `trigram` index
//! kind and the `fuzzy` list parameters on every engine: SQLite (portable),
//! PostgreSQL with `trigram = "off"` (portable) and PostgreSQL with
//! `pg_trgm` (`auto`, when the engine has it).
//!
//! Run with `FORGEBASE_TEST_BACKEND=sqlite` and `=postgres`; the
//! PostgreSQL run covers both of its engines.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use std::sync::Arc;

use common::{admin, is_postgres, service, service_with, test_db};
use forgebase_core::ErrorCode;
use forgebase_core::config::{ExtensionMode, PostgresExtensions};
use forgebase_data::db::testing::{TestDb, selected_backend};
use forgebase_data::db::{BackendKind, Database, ExtensionSetup, Feature, Statement, Value};
use forgebase_data::fuzzy::TrigramEngine;
use forgebase_data::schema::{Collection, Field, FieldKind};
use forgebase_data::{AccessContext, Action, Authorizer, DataService, Decision, IndexDefinition, IndexKind, ListQuery};
use serde_json::json;

fn text() -> FieldKind {
    FieldKind::Text {
        min_length: None,
        max_length: None,
    }
}

async fn with_trigram(db: &Database, mode: ExtensionMode) {
    let modes = PostgresExtensions {
        vector: ExtensionMode::Off,
        graph: ExtensionMode::Off,
        queue: ExtensionMode::Off,
        cron: ExtensionMode::Off,
        trigram: mode,
    };
    db.setup_extensions(&ExtensionSetup {
        modes,
        ..ExtensionSetup::default()
    })
    .await
    .unwrap();
}

/// The `trigram` modes to run a test with: none on SQLite (one engine),
/// `off` (portable) and `auto` (`pg_trgm`) on PostgreSQL.
fn modes() -> Vec<Option<ExtensionMode>> {
    match selected_backend() {
        BackendKind::Postgres => vec![Some(ExtensionMode::Off), Some(ExtensionMode::Auto)],
        _ => vec![None],
    }
}

/// A database and service with `trigram` in `mode`, or `None` (skip) when
/// `auto` finds no `pg_trgm`.
async fn engine_db(
    mode: Option<ExtensionMode>,
    authorizer: Option<Arc<dyn Authorizer>>,
) -> Option<(TestDb, DataService)> {
    let db = test_db().await;
    if let Some(m) = mode {
        with_trigram(&db, m).await;
        if m == ExtensionMode::Auto && !db.feature_active(Feature::Trigram) {
            eprintln!("skipped (auto): the engine has no pg_trgm");
            return None;
        }
    }
    let svc = match authorizer {
        Some(a) => service_with(&db, a, None).await,
        None => service(&db).await,
    };
    Some((db, svc))
}

fn trigram(name: &str, collection: &str, keys: &[&str]) -> IndexDefinition {
    IndexDefinition {
        name: name.into(),
        kind: IndexKind::Trigram,
        ..IndexDefinition::btree(collection, keys)
    }
}

async fn products(svc: &DataService) {
    let ctx = admin();
    svc.create_collection(
        &ctx,
        Collection::base("products")
            .with_field(Field::new("name", text()))
            .with_field(Field::new("brand", text()))
            .with_field(Field::new("note", text())),
    )
    .await
    .unwrap();
    for (name, brand) in [
        ("Laptop", "Acme"),
        ("Laptop Pro", "Acme"),
        ("Lap top stand", "Desk Co"),
        ("Keyboard", "Acme Labs"),
        ("Laptoop sleeve", "Bags Inc"),
        ("Monitor", "Dell"),
    ] {
        svc.create_record(&ctx, "products", json!({"name": name, "brand": brand, "note": "n"}))
            .await
            .unwrap();
    }
}

fn names(r: &forgebase_data::ListResult) -> Vec<String> {
    r.items.iter().map(|i| i["name"].as_str().unwrap().to_owned()).collect()
}

fn fuzzy(text: &str) -> ListQuery {
    ListQuery {
        fuzzy: Some(text.into()),
        ..ListQuery::default()
    }
}

/// Where `pg_trgm` is not in use the portable engine answers: trigram
/// indexes are created and `fuzzy` finds records; `~` keeps working.
#[tokio::test]
async fn the_portable_engine_answers_where_pg_trgm_is_not_in_use() {
    let db = test_db().await;
    if is_postgres(&db) {
        with_trigram(&db, ExtensionMode::Off).await;
    }
    assert!(!db.feature_active(Feature::Trigram));
    let svc = service(&db).await;
    assert_ne!(svc.trigram_engine(), TrigramEngine::PgTrgm);
    let ctx = admin();
    products(&svc).await;

    let info = svc
        .create_index(&ctx, trigram("products_name_trgm", "products", &["name"]))
        .await
        .unwrap();
    assert_eq!(info.kind, IndexKind::Trigram);
    if is_postgres(&db) {
        let sql = info.sql.as_deref().unwrap_or_default();
        assert!(
            sql.contains("_forgebase_trgm") && !sql.contains("gin_trgm_ops"),
            "{info:?}"
        );
    } else {
        // SQLite: the trigram table is the index; its size is reported.
        assert!(info.stats.size_bytes.unwrap_or(0) > 0, "{info:?}");
    }
    let r = svc.list_records(&ctx, "products", &fuzzy("laptop")).await.unwrap();
    assert_eq!(names(&r)[0], "Laptop", "{:?}", names(&r));
    // `~` keeps working, unindexed.
    let found = svc
        .list_records(
            &ctx,
            "products",
            &ListQuery {
                filter: Some("name ~ 'lap'".into()),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(found.total_items, Some(4));
    // A blank `fuzzy` is no fuzzy search.
    let all = svc.list_records(&ctx, "products", &fuzzy("  ")).await.unwrap();
    assert_eq!(all.total_items, Some(6));
}

#[tokio::test]
async fn full_text_search_reads_words_not_operators() {
    let db = test_db().await;
    let svc = service(&db).await;
    let ctx = admin();
    products(&svc).await;
    svc.create_index(
        &ctx,
        IndexDefinition {
            name: "products_fts".into(),
            kind: IndexKind::Fulltext,
            ..IndexDefinition::btree("products", &["name", "brand"])
        },
    )
    .await
    .unwrap();
    for (q, n) in [
        ("laptop", 2),
        ("laptop pro", 1),
        // No operators: `-pro`, `or` and quotes are text (words are ANDed).
        ("laptop -pro", 1),
        ("laptop OR keyboard", 0),
        ("\"laptop pro\"", 1),
        ("acme laptop", 2),
    ] {
        let r = svc
            .list_records(
                &ctx,
                "products",
                &ListQuery {
                    search: Some(q.into()),
                    ..ListQuery::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(r.total_items, Some(n), "{q}");
    }
}

#[tokio::test]
async fn fuzzy_search_ranks_by_similarity_on_every_engine() {
    for mode in modes() {
        let Some((db, svc)) = engine_db(mode, None).await else {
            continue;
        };
        let engine = svc.trigram_engine();
        let ctx = admin();
        products(&svc).await;

        // Without a trigram index there is nothing to compare.
        let err = svc.list_records(&ctx, "products", &fuzzy("laptop")).await.unwrap_err();
        assert_eq!(err.code(), "no_trigram_index");

        let info = svc
            .create_index(&ctx, trigram("products_name_trgm", "products", &["name"]))
            .await
            .unwrap();
        assert_eq!(info.kind, IndexKind::Trigram);
        let sql = info.sql.clone().unwrap_or_default();
        match engine {
            TrigramEngine::PgTrgm => assert!(sql.contains("gin_trgm_ops"), "{info:?}"),
            TrigramEngine::PgPortable => assert!(sql.contains("_forgebase_trgm("), "{info:?}"),
            TrigramEngine::SqlitePortable => assert!(info.sql.is_none(), "{info:?}"),
        }
        assert!(
            svc.list_indexes(&ctx, Some("products"))
                .await
                .unwrap()
                .iter()
                .any(|i| { i.name == "products_name_trgm" && i.kind == IndexKind::Trigram })
        );
        // Trigram indexes are neither unique nor partial.
        let mut bad = trigram("products_bad", "products", &["name"]);
        bad.unique = true;
        assert_eq!(svc.create_index(&ctx, bad).await.unwrap_err().code(), "invalid_index");

        // Best match first; a typo still finds the word.
        let r = svc.list_records(&ctx, "products", &fuzzy("laptop")).await.unwrap();
        assert_eq!(names(&r)[0], "Laptop", "{engine:?}: {:?}", names(&r));
        assert!(names(&r).contains(&"Laptop Pro".to_owned()), "{:?}", names(&r));
        assert!(!names(&r).contains(&"Monitor".to_owned()));
        assert_eq!(r.total_items, Some(r.items.len() as u64));
        let typo = svc.list_records(&ctx, "products", &fuzzy("laptpo")).await.unwrap();
        assert!(names(&typo).contains(&"Laptop".to_owned()), "{:?}", names(&typo));

        // An explicit `sort` wins over the ranking.
        let sorted = svc
            .list_records(
                &ctx,
                "products",
                &ListQuery {
                    sort: Some("-name".into()),
                    ..fuzzy("laptop")
                },
            )
            .await
            .unwrap();
        let mut expect = names(&sorted);
        expect.sort_by(|a, b| b.cmp(a));
        assert_eq!(names(&sorted), expect);

        // Threshold: stricter keeps the close ones, looser lets more in, and
        // out-of-range values are refused.
        let strict = svc
            .list_records(
                &ctx,
                "products",
                &ListQuery {
                    fuzzy_threshold: Some(0.9),
                    ..fuzzy("laptop")
                },
            )
            .await
            .unwrap();
        assert_eq!(names(&strict), vec!["Laptop"]);
        let loose = svc
            .list_records(
                &ctx,
                "products",
                &ListQuery {
                    fuzzy_threshold: Some(0.0),
                    ..fuzzy("laptop")
                },
            )
            .await
            .unwrap();
        assert!(
            loose.items.len() > r.items.len(),
            "{:?} vs {:?}",
            names(&loose),
            names(&r)
        );
        // Threshold 0 matches every row with a value, as `similarity >= 0`.
        assert_eq!(loose.total_items, Some(6));
        let err = svc
            .list_records(
                &ctx,
                "products",
                &ListQuery {
                    fuzzy_threshold: Some(1.5),
                    ..fuzzy("laptop")
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), "invalid_fuzzy_threshold");

        // `fuzzy_fields` must be indexed fields.
        let err = svc
            .list_records(
                &ctx,
                "products",
                &ListQuery {
                    fuzzy_fields: vec!["brand".into()],
                    ..fuzzy("acme")
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), "no_trigram_index");
        svc.create_index(&ctx, trigram("products_brand_trgm", "products", &["brand"]))
            .await
            .unwrap();
        let by_brand = svc
            .list_records(
                &ctx,
                "products",
                &ListQuery {
                    fuzzy_fields: vec!["brand".into()],
                    ..fuzzy("acme")
                },
            )
            .await
            .unwrap();
        assert!(
            names(&by_brand).contains(&"Keyboard".to_owned()),
            "{:?}",
            names(&by_brand)
        );
        // Both fields: the better of the two decides.
        let both = svc.list_records(&ctx, "products", &fuzzy("acme")).await.unwrap();
        assert!(names(&both).contains(&"Laptop".to_owned()));

        // Writes keep the index current (SQLite: the triggers).
        let id = svc
            .create_record(&ctx, "products", json!({"name": "Laptop Air", "brand": "Zed"}))
            .await
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let r = svc.list_records(&ctx, "products", &fuzzy("laptop air")).await.unwrap();
        assert_eq!(names(&r)[0], "Laptop Air", "{:?}", names(&r));
        svc.update_record(
            &ctx,
            "products",
            &id,
            json!({"name": "Tablet"}),
            forgebase_data::UpdateOptions::default(),
        )
        .await
        .unwrap();
        let r = svc.list_records(&ctx, "products", &fuzzy("laptop air")).await.unwrap();
        assert!(!names(&r).contains(&"Tablet".to_owned()), "{:?}", names(&r));
        let r = svc.list_records(&ctx, "products", &fuzzy("tablet")).await.unwrap();
        assert_eq!(names(&r), ["Tablet"]);
        svc.delete_record(&ctx, "products", &id, None).await.unwrap();
        let r = svc.list_records(&ctx, "products", &fuzzy("tablet")).await.unwrap();
        assert!(r.items.is_empty(), "{:?}", names(&r));

        // Cursor pagination continues the ranking without repeats or gaps.
        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let page = svc
                .list_records(
                    &ctx,
                    "products",
                    &ListQuery {
                        fuzzy_threshold: Some(0.05),
                        per_page: Some(2),
                        cursor: cursor.clone(),
                        ..fuzzy("laptop")
                    },
                )
                .await
                .unwrap();
            seen.extend(names(&page));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        let mut dedup = seen.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(dedup.len(), seen.len(), "{seen:?}");
        let all = svc
            .list_records(
                &ctx,
                "products",
                &ListQuery {
                    fuzzy_threshold: Some(0.05),
                    ..fuzzy("laptop")
                },
            )
            .await
            .unwrap();
        assert_eq!(seen, names(&all));

        // Dropping the index leaves nothing to compare (and, on SQLite, no
        // trigram table).
        svc.drop_index(&ctx, "products_name_trgm").await.unwrap();
        svc.drop_index(&ctx, "products_brand_trgm").await.unwrap();
        let err = svc.list_records(&ctx, "products", &fuzzy("laptop")).await.unwrap_err();
        assert_eq!(err.code(), "no_trigram_index");
        if !is_postgres(&db) {
            let left = scalar(
                &db,
                "SELECT count(*) FROM sqlite_master WHERE name LIKE '\\_forgebase\\_trgm\\_products%' ESCAPE '\\'",
            )
            .await;
            assert_eq!(left, Value::Integer(0));
        }
    }
}

/// The plan test: with a trigram index, `fuzzy` (and with `pg_trgm`, `~`)
/// are index scans.
#[tokio::test]
async fn contains_and_fuzzy_use_the_trigram_index() {
    for mode in modes() {
        let Some((db, svc)) = engine_db(mode, None).await else {
            continue;
        };
        let engine = svc.trigram_engine();
        let ctx = admin();
        svc.create_collection(&ctx, Collection::base("docs").with_field(Field::new("title", text())))
            .await
            .unwrap();
        // Enough varied rows that an index scan beats a table scan.
        for i in 0..3000 {
            svc.create_record(
                &ctx,
                "docs",
                json!({"title": format!("{:x}{:x} quark {i}", i * 7919 % 100_003, i * 104_729 % 99_991)}),
            )
            .await
            .unwrap();
        }
        svc.create_record(&ctx, "docs", json!({"title": "needle in a haystack"}))
            .await
            .unwrap();
        svc.create_index(&ctx, trigram("docs_title_trgm", "docs", &["title"]))
            .await
            .unwrap();
        svc.analyze(&ctx, Some("docs")).await.unwrap();

        if engine == TrigramEngine::PgTrgm {
            let contains = svc
                .explain_list(
                    &ctx,
                    "docs",
                    &ListQuery {
                        filter: Some("title ~ 'haystack'".into()),
                        ..ListQuery::default()
                    },
                )
                .await
                .unwrap();
            assert!(
                contains.indexes_used.contains(&"docs_title_trgm".to_owned()),
                "~ must use the trigram index: {contains:?}"
            );
        }
        let plan = svc.explain_list(&ctx, "docs", &fuzzy("haystak")).await.unwrap();
        match engine {
            TrigramEngine::SqlitePortable => {
                // Candidates come from the trigram table's primary key
                // (tri, k, rid); scores from its (rid, k, tri) index.
                let text: Vec<&str> = plan.steps.iter().map(|s| s.detail.as_str()).collect();
                assert!(
                    text.contains(&"SEARCH s USING PRIMARY KEY (tri=? AND k=?)")
                        && text.contains(&"SEARCH t USING INTEGER PRIMARY KEY (rowid=?)")
                        && text
                            .iter()
                            .any(|d| d.contains("_forgebase_trgm_docs_title_trgm_rid (rid=? AND k=?)"))
                        && !text.iter().any(|d| d.starts_with("SCAN t")),
                    "fuzzy must search the trigram table: {text:?}"
                );
            }
            _ => assert!(
                plan.indexes_used.contains(&"docs_title_trgm".to_owned()),
                "{engine:?}: fuzzy must use the trigram index: {plan:?}"
            ),
        }
        // "haystak" alone shares 6 of 23 trigrams with the title (0.26, below
        // the threshold, as pg_trgm computes it); the longer text matches.
        let miss = svc.list_records(&ctx, "docs", &fuzzy("haystak")).await.unwrap();
        assert!(miss.items.is_empty(), "{:?}", miss.items);
        let hit = svc
            .list_records(&ctx, "docs", &fuzzy("needle in a haystak"))
            .await
            .unwrap();
        assert_eq!(hit.items[0]["title"], "needle in a haystack");
        let hit = svc
            .list_records(
                &ctx,
                "docs",
                &ListQuery {
                    filter: Some("title ~ 'haystack'".into()),
                    ..ListQuery::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(hit.total_items, Some(1));
        if is_postgres(&db) {
            let value = scalar(
                &db,
                "SELECT count(*) FROM pg_indexes WHERE indexname = 'docs_title_trgm'",
            )
            .await;
            assert_eq!(value, Value::Integer(1));
        }
    }
}

async fn scalar(db: &Database, sql: &str) -> Value {
    use forgebase_data::db::Backend;
    db.fetch_all(&Statement::new(sql, vec![]))
        .await
        .unwrap()
        .scalar()
        .cloned()
        .unwrap_or(Value::Null)
}

/// Users may list; field `secret` is hidden from them.
#[derive(Debug)]
struct ListOnly;

impl Authorizer for ListOnly {
    fn authorize(&self, ctx: &AccessContext, action: Action, _c: Option<&Collection>) -> Decision {
        match (&ctx.principal, action) {
            (forgebase_data::Principal::Admin { .. }, _) => Decision::Allow,
            (forgebase_data::Principal::User { .. }, Action::List) => Decision::Allow,
            _ => Decision::Deny,
        }
    }
}

/// Masked fields are refused for `fuzzy` as for `search`, indexed or not.
#[tokio::test]
async fn fuzzy_refuses_masked_fields() {
    for mode in modes() {
        let Some((_db, svc)) = engine_db(mode, Some(Arc::new(ListOnly))).await else {
            continue;
        };
        let ctx = admin();
        svc.create_collection(
            &ctx,
            Collection::base("notes")
                .with_field(Field::new("title", text()))
                .with_field(Field {
                    hidden: true,
                    ..Field::new("secret", text())
                }),
        )
        .await
        .unwrap();
        svc.create_record(&ctx, "notes", json!({"title": "alpha", "secret": "hunter2"}))
            .await
            .unwrap();
        svc.create_index(&ctx, trigram("notes_trgm", "notes", &["title", "secret"]))
            .await
            .unwrap();
        let user = AccessContext::user("users", "u1");
        // A masked field, or all indexed fields (which include one): refused.
        for fields in [vec!["secret".to_owned()], vec![]] {
            let err = svc
                .list_records(
                    &user,
                    "notes",
                    &ListQuery {
                        fuzzy_fields: fields.clone(),
                        ..fuzzy("hunter2")
                    },
                )
                .await;
            assert_eq!(err.unwrap_err().code(), "no_trigram_index");
        }
        let ok = svc
            .list_records(
                &user,
                "notes",
                &ListQuery {
                    fuzzy_fields: vec!["title".into()],
                    ..fuzzy("alpha")
                },
            )
            .await
            .unwrap();
        assert_eq!(ok.items.len(), 1);
        assert!(ok.items[0].get("secret").is_none());
    }
}
