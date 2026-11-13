//! The portable fuzzy engine against `pg_trgm` (ADR-0034).
//!
//! * a fixture corpus: every engine (SQLite portable, PostgreSQL portable
//!   with `trigram = "off"`, PostgreSQL `pg_trgm`) returns exactly the
//!   records and order `pg_trgm`'s model gives (similarity = shared /
//!   union of padded-word trigrams, threshold 0.3, best first, then id);
//! * the trigram sets themselves: SQLite's trigger-maintained table and
//!   PostgreSQL's `forgebase._forgebase_trgm` equal the Rust model, and
//!   with `pg_trgm` installed, `show_trgm`;
//! * the SQLite index's life cycle: table rebuilds, renamed fields,
//!   dropped collections.
//!
//! Run with `FORGEBASE_TEST_BACKEND=sqlite` and `=postgres`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod common;

use std::collections::BTreeSet;

use common::{admin, is_postgres, service, test_db};
use forgebase_core::config::{ExtensionMode, PostgresExtensions};
use forgebase_data::db::testing::selected_backend;
use forgebase_data::db::{Backend, BackendKind, Database, ExtensionSetup, Feature, Statement, Value};
use forgebase_data::fuzzy::TrigramEngine;
use forgebase_data::query::trigram::{similarity, trigrams};
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
    db.setup_extensions(&ExtensionSetup {
        modes: PostgresExtensions {
            vector: ExtensionMode::Off,
            graph: ExtensionMode::Off,
            queue: ExtensionMode::Off,
            cron: ExtensionMode::Off,
            trigram: mode,
        },
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

/// Names with near-duplicates, typos, punctuation, short words, repeats,
/// an empty value and a missing one.
const CORPUS: &[&str] = &[
    "John Smith",
    "Jon Smith",
    "Jonathan Smythe",
    "Joan Smithers",
    "Smith & Wesson",
    "Katherine Johnson",
    "Catherine Johnston",
    "Kathryn Jonson",
    "McDonald",
    "MacDonald's",
    "O'Brien",
    "OBrien",
    "Anna-Maria Lopez",
    "Anna Maria Lopes",
    "Ana Mari",
    "laptop",
    "Laptop Pro 15",
    "lap-top",
    "laptops and tablets",
    "A",
    "a b c",
    "x",
    "word words wordy",
    "aaa aaa",
    "The Quick Brown Fox",
    "quick brown",
    "fox",
    "brown fox jumps",
    "1234 5678",
    "12345",
    "R2-D2",
    "C3PO",
    "  padded  ",
    "",
];

const QUERIES: &[&str] = &[
    "john smith",
    "smyth",
    "katherine",
    "mcdonald",
    "obrien",
    "anna maria",
    "laptop",
    "lap top",
    "quick fox",
    "a",
    "1234",
    "r2d2",
    "word",
    "zzzz",
    "!!!",
];

struct Corpus {
    /// (record id, name) for every record that has a name.
    rows: Vec<(String, Option<String>)>,
}

async fn corpus(svc: &DataService) -> Corpus {
    let ctx = admin();
    svc.create_collection(
        &ctx,
        Collection::base("people")
            .with_field(Field::new("name", text()))
            .with_field(Field::new("other", text())),
    )
    .await
    .unwrap();
    let mut rows = Vec::new();
    for name in CORPUS {
        let r = svc
            .create_record(&ctx, "people", json!({"name": name, "other": "o"}))
            .await
            .unwrap();
        rows.push((r["id"].as_str().unwrap().to_owned(), Some((*name).to_owned())));
    }
    let r = svc
        .create_record(&ctx, "people", json!({"other": "no name"}))
        .await
        .unwrap();
    rows.push((r["id"].as_str().unwrap().to_owned(), None));
    Corpus { rows }
}

/// What `pg_trgm` answers: rows whose similarity reaches `threshold`, best
/// first, then by id. PostgreSQL computes similarities in `real`; SQLite in
/// double precision.
fn expected(c: &Corpus, query: &str, threshold: f64, single: bool) -> Vec<String> {
    let q = trigrams(query);
    let mut hits: Vec<(f64, &str)> = c
        .rows
        .iter()
        .filter_map(|(id, name)| {
            let sim = similarity(&trigrams(name.as_deref()?), &q);
            let sim = if single { f64::from(sim as f32) } else { sim };
            (sim >= threshold).then_some((sim, id.as_str()))
        })
        .collect();
    hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(b.1)));
    hits.into_iter().map(|(_, id)| id.to_owned()).collect()
}

async fn fuzzy_ids(svc: &DataService, query: &str, threshold: Option<f64>) -> Vec<String> {
    let r = svc
        .list_records(
            &admin(),
            "people",
            &ListQuery {
                fuzzy: Some(query.into()),
                fuzzy_threshold: threshold,
                per_page: Some(500),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(r.total_items, Some(r.items.len() as u64));
    r.items.iter().map(|i| i["id"].as_str().unwrap().to_owned()).collect()
}

async fn check_corpus(svc: &DataService, c: &Corpus) {
    let engine = svc.trigram_engine();
    let single = engine != TrigramEngine::SqlitePortable;
    for q in QUERIES {
        if q.trim().is_empty() {
            continue;
        }
        for t in [None, Some(0.1), Some(0.5), Some(0.0), Some(1.0)] {
            let got = fuzzy_ids(svc, q, t).await;
            let want = expected(c, q, t.unwrap_or(0.3), single);
            assert_eq!(got, want, "{engine:?}: fuzzy {q:?} threshold {t:?}");
        }
    }
}

async fn strings(db: &Database, sql: &str, param: &str) -> BTreeSet<String> {
    let rows = db.fetch_all(&Statement::new(sql, vec![param.into()])).await.unwrap();
    rows.rows
        .iter()
        .filter_map(|r| r.first().and_then(Value::as_str).map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn every_engine_returns_pg_trgm_matches_and_order() {
    let modes: Vec<Option<ExtensionMode>> = match selected_backend() {
        BackendKind::Postgres => vec![Some(ExtensionMode::Off), Some(ExtensionMode::Auto)],
        _ => vec![None],
    };
    for mode in modes {
        let db = test_db().await;
        if let Some(m) = mode {
            with_trigram(&db, m).await;
            if m == ExtensionMode::Auto && !db.feature_active(Feature::Trigram) {
                eprintln!("skipped (auto): the engine has no pg_trgm");
                continue;
            }
        }
        let svc = service(&db).await;
        let c = corpus(&svc).await;
        svc.create_index(&admin(), trigram("people_name_trgm", "people", &["name"]))
            .await
            .unwrap();
        check_corpus(&svc, &c).await;

        // Values written after the index was built are indexed alike
        // (SQLite: by the triggers).
        let ctx = admin();
        let late = svc
            .create_record(&ctx, "people", json!({"name": "Jon Smithson"}))
            .await
            .unwrap();
        let mut c = c;
        c.rows
            .push((late["id"].as_str().unwrap().to_owned(), Some("Jon Smithson".into())));
        check_corpus(&svc, &c).await;
    }
}

/// On PostgreSQL with `pg_trgm` installed, switching to the portable
/// engine (`trigram = "off"`) answers exactly as `pg_trgm` did, on the same
/// data: the differential test of the two engines.
#[tokio::test]
async fn portable_and_pg_trgm_agree_on_postgres() {
    if selected_backend() != BackendKind::Postgres {
        eprintln!("skipped: needs FORGEBASE_TEST_BACKEND=postgres");
        return;
    }
    let db = test_db().await;
    with_trigram(&db, ExtensionMode::Auto).await;
    if !db.feature_active(Feature::Trigram) {
        eprintln!("skipped: the engine has no pg_trgm");
        return;
    }
    let svc = service(&db).await;
    let c = corpus(&svc).await;
    svc.create_index(&admin(), trigram("people_name_trgm", "people", &["name"]))
        .await
        .unwrap();
    let mut with_ext = Vec::new();
    for q in QUERIES {
        for t in [None, Some(0.1), Some(0.6)] {
            with_ext.push(fuzzy_ids(&svc, q, t).await);
        }
    }
    // The trigram sets: pg_trgm's show_trgm, the portable SQL function and
    // the Rust model agree on every corpus string.
    for (_, name) in &c.rows {
        let Some(name) = name else { continue };
        let pg = strings(&db, "SELECT unnest(forgebase.show_trgm(?1))", name).await;
        let portable = strings(&db, "SELECT unnest(forgebase._forgebase_trgm(?1))", name).await;
        let model: BTreeSet<String> = trigrams(name).into_iter().collect();
        assert_eq!(pg, portable, "{name:?}");
        assert_eq!(portable, model, "{name:?}");
    }
    // Same database, extension off: the portable engine.
    with_trigram(&db, ExtensionMode::Off).await;
    assert_eq!(svc.trigram_engine(), TrigramEngine::PgPortable);
    let mut portable = Vec::new();
    for q in QUERIES {
        for t in [None, Some(0.1), Some(0.6)] {
            portable.push(fuzzy_ids(&svc, q, t).await);
        }
    }
    assert_eq!(with_ext, portable);
}

/// SQLite: the trigger-maintained trigram table holds exactly the Rust
/// model's trigrams, Unicode included (case folding of Latin, Greek and
/// Cyrillic, punctuation as separators).
#[tokio::test]
async fn sqlite_trigram_rows_equal_the_model() {
    let db = test_db().await;
    let svc = service(&db).await;
    let ctx = admin();
    svc.create_collection(&ctx, Collection::base("words").with_field(Field::new("w", text())))
        .await
        .unwrap();
    let samples = [
        "ÉCOLE Normale Supérieure",
        "école normale",
        "Straße – Weg",
        "ΣΟΦΙΑ σοφία",
        "Москва, Россия",
        "日本語 テキスト",
        "“quoted” — dashes… and\u{a0}nbsp",
        "ＦＵＬＬ ｗｉｄｔｈ",
        "emoji 😀 face",
        "tab\tand\nnewline",
        "UPPER lower MiXeD 123",
    ];
    let mut ids = Vec::new();
    for s in samples {
        let r = svc.create_record(&ctx, "words", json!({"w": s})).await.unwrap();
        ids.push(r["id"].as_str().unwrap().to_owned());
    }
    svc.create_index(&ctx, trigram("words_trgm", "words", &["w"]))
        .await
        .unwrap();
    let more = "Ünïcödé after the index";
    let r = svc.create_record(&ctx, "words", json!({"w": more})).await.unwrap();
    ids.push(r["id"].as_str().unwrap().to_owned());

    let sql = if is_postgres(&db) {
        "SELECT unnest(forgebase._forgebase_trgm(w)) FROM words WHERE id = ?1"
    } else {
        "SELECT s.tri FROM _forgebase_trgm_words_trgm s JOIN words t ON t.rowid = s.rid WHERE t.id = ?1"
    };
    for (id, s) in ids.iter().zip(samples.iter().chain([&more])) {
        let got = strings(&db, sql, id).await;
        let model: BTreeSet<String> = trigrams(s).into_iter().collect();
        if is_postgres(&db) {
            // PostgreSQL classifies with the server's locale (the test
            // engine's is C: non-ASCII characters separate words), so only
            // ASCII samples must equal the model there.
            if s.is_ascii() {
                assert_eq!(got, model, "{s:?}");
            }
        } else {
            assert_eq!(got, model, "{s:?}");
        }
    }
    if !is_postgres(&db) {
        // Case folding beyond ASCII: "école" finds "ÉCOLE …".
        let r = svc
            .list_records(
                &ctx,
                "words",
                &ListQuery {
                    fuzzy: Some("école normale".into()),
                    ..ListQuery::default()
                },
            )
            .await
            .unwrap();
        let found: Vec<&str> = r.items.iter().map(|i| i["w"].as_str().unwrap()).collect();
        assert_eq!(found[0], "école normale");
        assert!(found.contains(&"ÉCOLE Normale Supérieure"), "{found:?}");
        let r = svc
            .list_records(
                &ctx,
                "words",
                &ListQuery {
                    fuzzy: Some("СОФИЯ москва".into()),
                    ..ListQuery::default()
                },
            )
            .await
            .unwrap();
        let found: Vec<&str> = r.items.iter().map(|i| i["w"].as_str().unwrap()).collect();
        assert!(found.contains(&"Москва, Россия"), "{found:?}");
    }
}

/// The trigram index survives what changes its table: SQLite rebuilds the
/// table when a field is dropped (triggers re-created, rows re-indexed),
/// renamed fields carry the index along, and dropping the collection drops
/// the trigram table.
#[tokio::test]
async fn trigram_indexes_follow_schema_changes() {
    let db = test_db().await;
    let svc = service(&db).await;
    let ctx = admin();
    let coll = svc
        .create_collection(
            &ctx,
            Collection::base("pets")
                .with_field(Field::new("name", text()))
                .with_field(Field::new("extra", text())),
        )
        .await
        .unwrap();
    for n in ["Rex", "Rexy", "Fluffy", "Fluff"] {
        svc.create_record(&ctx, "pets", json!({"name": n, "extra": "x"}))
            .await
            .unwrap();
    }
    svc.create_index(&ctx, trigram("pets_trgm", "pets", &["name"]))
        .await
        .unwrap();
    let names = |r: forgebase_data::ListResult, key: &str| -> Vec<String> {
        r.items.iter().map(|i| i[key].as_str().unwrap().to_owned()).collect()
    };
    let fuzzy = |t: &str| ListQuery {
        fuzzy: Some(t.into()),
        ..ListQuery::default()
    };

    // Drop a field: SQLite rebuilds the table.
    let mut changed = coll.clone();
    changed.fields.retain(|f| f.name != "extra");
    let coll = svc.update_collection(&ctx, "pets", changed).await.unwrap();
    assert_eq!(
        names(svc.list_records(&ctx, "pets", &fuzzy("fluffy")).await.unwrap(), "name"),
        ["Fluffy", "Fluff"]
    );
    svc.create_record(&ctx, "pets", json!({"name": "Fluffster"}))
        .await
        .unwrap();
    let r = names(
        svc.list_records(&ctx, "pets", &fuzzy("fluffster")).await.unwrap(),
        "name",
    );
    assert_eq!(r[0], "Fluffster", "{r:?}");

    // Rename the indexed field.
    let mut renamed = coll.clone();
    for f in &mut renamed.fields {
        if f.name == "name" {
            f.name = "title".into();
        }
    }
    svc.update_collection(&ctx, "pets", renamed).await.unwrap();
    svc.create_record(&ctx, "pets", json!({"title": "Rexford"}))
        .await
        .unwrap();
    let r = names(
        svc.list_records(&ctx, "pets", &fuzzy("rexford")).await.unwrap(),
        "title",
    );
    assert_eq!(r[0], "Rexford", "{r:?}");
    assert!(r.contains(&"Rex".to_owned()), "{r:?}");

    // Dropping the collection drops the trigram table.
    svc.delete_collection(&ctx, "pets").await.unwrap();
    let sql = if is_postgres(&db) {
        "SELECT count(*) FROM pg_indexes WHERE indexname = 'pets_trgm'"
    } else {
        "SELECT count(*) FROM sqlite_master WHERE name LIKE '\\_forgebase\\_trgm\\_pets%' ESCAPE '\\'"
    };
    let left = db.fetch_all(&Statement::new(sql, vec![])).await.unwrap();
    assert_eq!(left.scalar().cloned(), Some(Value::Integer(0)));
}
