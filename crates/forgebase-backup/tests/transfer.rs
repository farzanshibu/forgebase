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
    let rep = transfer::export_bundle(&a.data, &dir, ExportOptions::default())
        .await
        .unwrap();
    let names: Vec<_> = rep.bundle.collections.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["authors", "books"], "relation targets come first");
    assert_eq!(rep.bundle.collections[1].records, 30);
    assert!(dir.join("collections/books.ndjson").is_file());
    assert!(dir.join("collections/books.schema.json").is_file());

    // Into an empty instance.
    let b = open(&tmp.path().join("b")).await;
    let imp = transfer::import_bundle(&b.data, &dir, ImportOptions::default())
        .await
        .unwrap();
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
    assert_eq!(
        rec["author"],
        json!("author00000001"),
        "record ids and relations are preserved"
    );
}

#[tokio::test]
async fn conflicts_fail_by_default_and_skip_on_request() {
    let tmp = tempfile::tempdir().unwrap();
    let a = open(&tmp.path().join("a")).await;
    a.seed(10).await;
    let dir = tmp.path().join("bundle");
    transfer::export_bundle(&a.data, &dir, ExportOptions::default())
        .await
        .unwrap();

    // Importing back into the source collides on the collection.
    let err = transfer::import_bundle(&a.data, &dir, ImportOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, BackupError::Conflict(_)), "{err}");

    // Skip mode merges: one new record in the target stays, existing ids are skipped.
    let b = open(&tmp.path().join("b")).await;
    transfer::import_bundle(&b.data, &dir, ImportOptions::default())
        .await
        .unwrap();
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
    transfer::import_bundle(&c.data, &dir, ImportOptions::default())
        .await
        .unwrap();
    c.data
        .delete_record(&admin(), "books", "book0000000003", None)
        .await
        .unwrap();
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
    transfer::export_bundle(&a.data, &dir, ExportOptions::default())
        .await
        .unwrap();

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
    transfer::export_bundle(&a.data, &dir, ExportOptions::default())
        .await
        .unwrap();

    let b = open(&tmp.path().join("b")).await;
    let nd = dir.join("collections/books.ndjson");
    let mut text = std::fs::read_to_string(&nd).unwrap();
    text.push_str("{\"id\":\"tampered\"}\n");
    std::fs::write(&nd, text).unwrap();
    let err = transfer::import_bundle(&b.data, &dir, ImportOptions::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("corrupt"), "{err}");
    assert!(
        b.data.get_collection(&admin(), "authors").await.is_err(),
        "nothing was created"
    );

    let err = transfer::import_bundle(&b.data, tmp.path(), ImportOptions::default())
        .await
        .unwrap_err();
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

#[tokio::test]
async fn single_collection_files_in_json_ndjson_and_csv() {
    use forgebase_backup::transfer::Format;
    let tmp = tempfile::tempdir().unwrap();
    let a = open(&tmp.path().join("a")).await;
    a.seed(12).await;
    a.data
        .create_record(
            &admin(),
            "books",
            json!({"id": "tricky00000001", "title": "Comma, \"quote\"\nnewline"}),
        )
        .await
        .unwrap();

    let csv = tmp.path().join("books.csv");
    assert_eq!(
        transfer::export_records(&a.data, "books", &csv, Format::Csv)
            .await
            .unwrap(),
        13
    );
    let text = std::fs::read_to_string(&csv).unwrap();
    assert!(text.starts_with("id,created,updated,title,author\n"), "{text}");
    assert!(text.contains("\"Comma, \"\"quote\"\"\nnewline\""), "{text}");

    let json_file = tmp.path().join("books.json");
    transfer::export_records(&a.data, "books", &json_file, Format::Json)
        .await
        .unwrap();
    let arr: Vec<serde_json::Value> = serde_json::from_str(&std::fs::read_to_string(&json_file).unwrap()).unwrap();
    assert_eq!(arr.len(), 13);
    let nd = tmp.path().join("books.ndjson");
    transfer::export_records(&a.data, "books", &nd, Format::Ndjson)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&nd).unwrap().lines().count(), 13);
    // Existing files are never overwritten; unknown collections are errors.
    assert!(matches!(
        transfer::export_records(&a.data, "books", &nd, Format::Ndjson).await,
        Err(BackupError::Conflict(_))
    ));
    assert!(
        transfer::export_records(&a.data, "nope", &tmp.path().join("n"), Format::Json)
            .await
            .is_err()
    );

    // Both JSON flavours import into an existing collection, ids preserved.
    for (file, format) in [(&json_file, Format::Json), (&nd, Format::Ndjson)] {
        let b = open(&tmp.path().join(format!("b-{file:?}").replace(['/', '"', ' '], "_"))).await;
        let bundle = tmp
            .path()
            .join(format!("bundle-{}", file.file_name().unwrap().to_string_lossy()));
        transfer::export_bundle(&a.data, &bundle, ExportOptions::default())
            .await
            .unwrap();
        transfer::import_bundle(
            &b.data,
            &bundle,
            ImportOptions {
                collections: Some(vec!["authors".into(), "books".into()]),
                ..ImportOptions::default()
            },
        )
        .await
        .unwrap();
        let (n, skipped) = transfer::import_records_file(&b.data, "books", file, format, OnConflict::Skip)
            .await
            .unwrap();
        assert_eq!((n, skipped), (0, 13));
    }
    assert!(matches!(
        transfer::import_records_file(&a.data, "books", &csv, Format::Csv, OnConflict::Skip).await,
        Err(BackupError::Invalid(_))
    ));
}

/// A conflict is found before anything is written, and a record that fails
/// halfway removes the collections the run created: an import is all or
/// nothing for the collections it makes.
#[tokio::test]
async fn a_conflict_or_a_failed_record_leaves_nothing_imported() {
    use forgebase_data::schema::Collection;

    let tmp = tempfile::tempdir().unwrap();
    let a = open(&tmp.path().join("a")).await;
    a.seed(5).await;
    let dir = tmp.path().join("bundle");
    transfer::export_bundle(&a.data, &dir, ExportOptions::default())
        .await
        .unwrap();

    // The target has `books` but not `authors` (which the bundle lists first).
    let b = open(&tmp.path().join("b")).await;
    b.data
        .create_collection(&admin(), Collection::base("books"))
        .await
        .unwrap();
    let err = transfer::import_bundle(&b.data, &dir, ImportOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, BackupError::Conflict(_)), "{err}");
    assert!(err.to_string().contains("nothing was imported"), "{err}");
    assert!(
        b.data.get_collection(&admin(), "authors").await.is_err(),
        "authors was not created before the conflict was found"
    );

    // A record that fails after `authors` and `books` were made.
    let books = dir.join("collections/books.ndjson");
    let mut text = std::fs::read_to_string(&books).unwrap();
    text.push_str("{\"id\":\"bad00000000001\",\"title\":12345}\n");
    std::fs::write(&books, text).unwrap();
    let meta = dir.join("bundle.json");
    let mut bundle: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&meta).unwrap()).unwrap();
    for c in bundle["collections"].as_array_mut().unwrap() {
        c.as_object_mut().unwrap().remove("sha256");
    }
    std::fs::write(&meta, serde_json::to_string(&bundle).unwrap()).unwrap();
    let c = open(&tmp.path().join("c")).await;
    let err = transfer::import_bundle(&c.data, &dir, ImportOptions::default())
        .await
        .unwrap_err();
    for name in ["authors", "books"] {
        assert!(
            c.data.get_collection(&admin(), name).await.is_err(),
            "{name} was removed again after: {err}"
        );
    }
}
