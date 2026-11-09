//! Export and import of collections and configuration: ids preserved,
//! relation order, conflicts, corruption.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{admin, open};
use forgebase_backup::BackupError;
use forgebase_backup::transfer::{self, ExportOptions, ImportOptions, OnConflict};
use serde_json::json;

#[tokio::test]
async fn export_import_preserves_ids_and_orders_relations() {
    let tmp = tempfile::tempdir().unwrap();
    let a = open(&tmp.path().join("a")).await;
    let (authors_id, books_id) = a.seed(30).await;

    let dir = tmp.path().join("bundle");
    let rep = transfer::export_bundle(&a.data, &dir, ExportOptions::default()).await.unwrap();
    let names: Vec<_> = rep.bundle.collections.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["authors", "books"], "relation targets come first");
    assert_eq!(rep.bundle.collections[1].records, 30);
    assert!(dir.join("collections/books.ndjson").is_file());
    assert!(dir.join("collections/books.schema.json").is_file());

    // Into an empty instance.
    let b = open(&tmp.path().join("b")).await;
    let imp = transfer::import_bundle(&b.data, &dir, ImportOptions::default()).await.unwrap();
    assert!(imp.collections.iter().all(|c| c.created));
    assert_eq!(b.count("books").await, 30);
    let coll = b.data.get_collection(&admin(), "books").await.unwrap();
    assert_eq!(coll.id, books_id, "collection ids are preserved");
    assert_eq!(b.data.get_collection(&admin(), "authors").await.unwrap().id, authors_id);
    let rec = b
        .data
        .get_record(&admin(), "books", "book0000000012", &Default::default())
        .await
        .unwrap();
    assert_eq!(rec["title"], json!("Book 12"));
    assert_eq!(rec["author"], json!("author00000001"), "record ids and relations are preserved");
}

#[tokio::test]
async fn conflicts_fail_by_default_and_skip_on_request() {
    let tmp = tempfile::tempdir().unwrap();
    let a = open(&tmp.path().join("a")).await;
    a.seed(10).await;
    let dir = tmp.path().join("bundle");
    transfer::export_bundle(&a.data, &dir, ExportOptions::default()).await.unwrap();

    // Importing back into the source collides on the collection.
    let err = transfer::import_bundle(&a.data, &dir, ImportOptions::default()).await.unwrap_err();
    assert!(matches!(err, BackupError::Conflict(_)), "{err}");

    // Skip mode merges: one new record in the target stays, existing ids are skipped.
    let b = open(&tmp.path().join("b")).await;
    transfer::import_bundle(&b.data, &dir, ImportOptions::default()).await.unwrap();
    b.data
        .create_record(&admin(), "books", json!({"id": "extra000000001", "title": "only in b"}))
        .await
        .unwrap();
    let imp = transfer::import_bundle(
        &b.data,
        &dir,
        ImportOptions {
            on_conflict: OnConflict::Skip,
            ..ImportOptions::default()
        },
    )
    .await
    .unwrap();
    let books = imp.collections.iter().find(|c| c.name == "books").unwrap();
    assert_eq!((books.records, books.skipped, books.created), (0, 10, false));
    assert_eq!(b.count("books").await, 11);

    // Merge into a target missing some records.
    let c = open(&tmp.path().join("c")).await;
    transfer::import_bundle(&c.data, &dir, ImportOptions::default()).await.unwrap();
    c.data.delete_record(&admin(), "books", "book0000000003", None).await.unwrap();
    let imp = transfer::import_bundle(
        &c.data,
        &dir,
        ImportOptions {
            on_conflict: OnConflict::Skip,
            ..ImportOptions::default()
        },
    )
    .await
    .unwrap();
    let books = imp.collections.iter().find(|c| c.name == "books").unwrap();
    assert_eq!((books.records, books.skipped), (1, 9));
    assert_eq!(c.count("books").await, 10);
}

#[tokio::test]
async fn dry_run_changes_nothing_and_subsets_work() {
    let tmp = tempfile::tempdir().unwrap();
    let a = open(&tmp.path().join("a")).await;
    a.seed(5).await;
    let dir = tmp.path().join("bundle");
    transfer::export_bundle(&a.data, &dir, ExportOptions::default()).await.unwrap();

    let b = open(&tmp.path().join("b")).await;
    let rep = transfer::import_bundle(
        &b.data,
        &dir,
        ImportOptions {
            dry_run: true,
            ..ImportOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(rep.dry_run);
    assert_eq!(rep.collections.len(), 2);
    assert!(b.data.get_collection(&admin(), "authors").await.is_err());

    let only = tmp.path().join("only-authors");
    let rep = transfer::export_bundle(
        &a.data,
        &only,
        ExportOptions {
            collections: Some(vec!["authors".into()]),
            ..ExportOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(rep.bundle.collections.len(), 1);
    assert!(matches!(
        transfer::export_bundle(
            &a.data,
            &tmp.path().join("x"),
            ExportOptions {
                collections: Some(vec!["nope".into()]),
                ..ExportOptions::default()
            }
        )
        .await,
        Err(BackupError::NotFound(_))
    ));
    // The destination is never overwritten.
    assert!(matches!(
        transfer::export_bundle(&a.data, &only, ExportOptions::default()).await,
        Err(BackupError::Conflict(_))
    ));
}

#[tokio::test]
async fn corrupt_or_foreign_bundles_are_refused_before_anything_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let a = open(&tmp.path().join("a")).await;
    a.seed(5).await;
    let dir = tmp.path().join("bundle");
    transfer::export_bundle(&a.data, &dir, ExportOptions::default()).await.unwrap();

    let b = open(&tmp.path().join("b")).await;
    let nd = dir.join("collections/books.ndjson");
    let mut text = std::fs::read_to_string(&nd).unwrap();
    text.push_str("{\"id\":\"tampered\"}\n");
    std::fs::write(&nd, text).unwrap();
    let err = transfer::import_bundle(&b.data, &dir, ImportOptions::default()).await.unwrap_err();
    assert!(err.to_string().contains("corrupt"), "{err}");
    assert!(b.data.get_collection(&admin(), "authors").await.is_err(), "nothing was created");

    let err = transfer::import_bundle(&b.data, tmp.path(), ImportOptions::default()).await.unwrap_err();
    assert!(matches!(err, BackupError::Format(_)));
}

#[tokio::test]
async fn config_round_trips_through_toml_and_is_validated() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = forgebase_core::Config::defaults(tmp.path());
    cfg.server.port = 9191;
    cfg.database.read_pool_size = 3;
    cfg.queue.max_message_bytes = 12345;
    let toml = transfer::config_to_toml(&cfg).unwrap();
    assert!(!toml.contains("data_dir"));
    let file = tmp.path().join("forgebase.toml");
    std::fs::write(&file, &toml).unwrap();
    let back = transfer::validate_config_file(&file).unwrap();
    assert_eq!(back.server.port, 9191);
    assert_eq!(back.database.read_pool_size, 3);
    assert_eq!(back.queue.max_message_bytes, 12345);
    assert_eq!(back.storage, cfg.storage);
    assert_eq!(back.resp, cfg.resp);
    assert_eq!(back.billing, cfg.billing);

    // Unknown keys and bad values are errors, as at server startup.
    std::fs::write(&file, "[server]\nport = 70000\n").unwrap();
    assert!(transfer::validate_config_file(&file).is_err());
    std::fs::write(&file, "[nope]\nx = 1\n").unwrap();
    assert!(transfer::validate_config_file(&file).is_err());
}

#[tokio::test]
async fn export_bundle_can_carry_the_config() {
    let tmp = tempfile::tempdir().unwrap();
    let a = open(&tmp.path().join("a")).await;
    a.seed(1).await;
    let toml = transfer::config_to_toml(&forgebase_core::Config::defaults(tmp.path())).unwrap();
    let dir = tmp.path().join("bundle");
    let rep = transfer::export_bundle(
        &a.data,
        &dir,
        ExportOptions {
            config_toml: Some(toml),
            ..ExportOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(rep.bundle.config);
    assert!(dir.join("config.toml").is_file());
}
