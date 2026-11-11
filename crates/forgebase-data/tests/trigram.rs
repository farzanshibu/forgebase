//! Full-text query and trigram search (ADR-0029 §1.8, FB-166): `search`
//! reads words as text (`plainto_tsquery`), the `trigram` index kind, the
//! `fuzzy` list parameters, and the answers where `pg_trgm` is not in use
//! (SQLite, or PostgreSQL with `trigram = "off"`).
//!
//! Run with `FORGEBASE_TEST_BACKEND=sqlite` (the unsupported answers) and
//! `=postgres` (everything; the engine needs pg_trgm, contrib in any
//! PostgreSQL build).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{admin, is_postgres, service, test_db};
use forgebase_core::ErrorCode;
use forgebase_core::config::{ExtensionMode, PostgresExtensions};
use forgebase_data::db::{Database, ExtensionSetup, Feature, Statement, Value};
use forgebase_data::schema::{Collection, Field, FieldKind};
use forgebase_data::{DataService, IndexDefinition, IndexKind, ListQuery};
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
    r.items
        .iter()
        .map(|i| i["name"].as_str().unwrap().to_owned())
        .collect()
}

fn fuzzy(text: &str) -> ListQuery {
    ListQuery {
        fuzzy: Some(text.into()),
        ..ListQuery::default()
    }
}

/// SQLite, and PostgreSQL with the extension off: the unsupported answers.
#[tokio::test]
async fn trigram_features_answer_400_where_pg_trgm_is_not_in_use() {
    let db = test_db().await;
    if is_postgres(&db) {
        with_trigram(&db, ExtensionMode::Off).await;
    }
    assert!(!db.feature_active(Feature::Trigram));
    let svc = service(&db).await;
    let ctx = admin();
    products(&svc).await;

    let err = svc
        .create_index(&ctx, trigram("products_name_trgm", "products", &["name"]))
        .await
        .unwrap_err();
    assert_eq!(err.code(), "index_kind_unsupported");
    let err = svc.list_records(&ctx, "products", &fuzzy("laptop")).await.unwrap_err();
    assert_eq!(err.code(), "fuzzy_unsupported");
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
        ("laptop -pro", 0),
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
async fn fuzzy_search_ranks_by_similarity_and_uses_the_trigram_index() {
    let db = test_db().await;
    if !is_postgres(&db) {
        eprintln!("skipped: needs PostgreSQL");
        return;
    }
    with_trigram(&db, ExtensionMode::Auto).await;
    if !db.feature_active(Feature::Trigram) {
        eprintln!("skipped: the engine has no pg_trgm");
        return;
    }
    let svc = service(&db).await;
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
    assert!(
        info.sql.as_deref().unwrap_or_default().contains("gin_trgm_ops"),
        "{info:?}"
    );
    assert!(svc.list_indexes(&ctx, Some("products")).await.unwrap().iter().any(|i| {
        i.name == "products_name_trgm" && i.kind == IndexKind::Trigram
    }));
    // Trigram indexes are neither unique nor partial.
    let mut bad = trigram("products_bad", "products", &["name"]);
    bad.unique = true;
    assert_eq!(svc.create_index(&ctx, bad).await.unwrap_err().code(), "invalid_index");

    // Best match first; a typo still finds the word.
    let r = svc.list_records(&ctx, "products", &fuzzy("laptop")).await.unwrap();
    assert_eq!(names(&r)[0], "Laptop", "{:?}", names(&r));
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
                fuzzy_threshold: Some(0.1),
                ..fuzzy("laptop")
            },
        )
        .await
        .unwrap();
    assert!(loose.items.len() > r.items.len(), "{:?} vs {:?}", names(&loose), names(&r));
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
    assert!(names(&by_brand).contains(&"Keyboard".to_owned()), "{:?}", names(&by_brand));
    // Both fields: the better of the two decides.
    let both = svc.list_records(&ctx, "products", &fuzzy("acme")).await.unwrap();
    assert!(names(&both).contains(&"Laptop".to_owned()));

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

    // Dropping the index leaves nothing to compare.
    svc.drop_index(&ctx, "products_name_trgm").await.unwrap();
    svc.drop_index(&ctx, "products_brand_trgm").await.unwrap();
    let err = svc.list_records(&ctx, "products", &fuzzy("laptop")).await.unwrap_err();
    assert_eq!(err.code(), "no_trigram_index");
}

/// The plan test: with a trigram index `~` and `%` are index scans.
#[tokio::test]
async fn contains_and_fuzzy_use_the_trigram_index() {
    let db = test_db().await;
    if !is_postgres(&db) {
        eprintln!("skipped: needs PostgreSQL");
        return;
    }
    with_trigram(&db, ExtensionMode::Auto).await;
    if !db.feature_active(Feature::Trigram) {
        eprintln!("skipped: the engine has no pg_trgm");
        return;
    }
    let svc = service(&db).await;
    let ctx = admin();
    svc.create_collection(
        &ctx,
        Collection::base("docs").with_field(Field::new("title", text())),
    )
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
    let fuzzy_plan = svc.explain_list(&ctx, "docs", &fuzzy("haystak needle")).await.unwrap();
    assert!(
        fuzzy_plan.indexes_used.contains(&"docs_title_trgm".to_owned()),
        "fuzzy must use the trigram index: {fuzzy_plan:?}"
    );
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
    let value = scalar(&db, "SELECT count(*) FROM pg_indexes WHERE indexname = 'docs_title_trgm'").await;
    assert_eq!(value, Value::Integer(1));
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
