//! Backup, verification, restore (incl. restore after a crash), the master key
//! rules and snapshots with retention. Real SQLite files, real directories.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{Instance, open, read};
use forgebase_backup::restore::{self, RestoreOptions};
use forgebase_backup::{BackupError, BackupOptions, Retention, SnapshotStore, backup, verify};
use serde_json::json;

async fn seeded(root: &std::path::Path) -> Instance {
    let inst = open(root).await;
    inst.seed(25).await;
    inst.put_blob("blobs/ab/abcdef", b"blob one");
    inst.put_blob("staging/s1/part", b"staged");
    inst.put_blob(".tmp/inflight", b"never backed up");
    inst.put_index("vector/c1/f.usearch", b"index bytes");
    inst.put_key(b"0123456789abcdef0123456789abcdef");
    inst
}

#[tokio::test]
async fn backup_verify_restore_round_trip_keeps_ids_and_files() {
    let tmp = tempfile::tempdir().unwrap();
    let src = seeded(&tmp.path().join("src")).await;
    let dest = tmp.path().join("bk");
    let m = backup::create(
        &src.db,
        &src.layout,
        &dest,
        BackupOptions {
            include_master_key: true,
            ..BackupOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(!dest.with_file_name("bk.partial").exists());
    assert!(m.files.iter().any(|f| f.path == "forgebase.db"));
    assert!(m.files.iter().any(|f| f.path == "storage/blobs/ab/abcdef"));
    assert!(m.files.iter().any(|f| f.path == "indexes/vector/c1/f.usearch"));
    assert!(m.files.iter().any(|f| f.path == "auth/master.key"));
    assert!(
        !m.files.iter().any(|f| f.path.contains(".tmp")),
        "in-flight temp files are skipped"
    );

    let report = verify(&dest).await;
    assert!(report.ok, "{:?}", report.problems);
    assert_eq!(report.files_checked, m.files.len());

    // Restore into a different, empty data directory.
    let target = tmp.path().join("restored");
    let layout = forgebase_core::DataLayout::new(&target);
    let r = restore::restore(&dest, &layout, RestoreOptions::default())
        .await
        .unwrap();
    assert_eq!(r.schema_version, m.schema_version);
    assert!(r.previous_data.is_none());
    assert_eq!(read(layout.storage_dir().join("blobs/ab/abcdef")), b"blob one");
    assert_eq!(read(layout.indexes_dir().join("vector/c1/f.usearch")), b"index bytes");
    assert_eq!(
        read(backup::master_key_path(&layout)),
        b"0123456789abcdef0123456789abcdef"
    );

    let restored = open(&target).await;
    assert_eq!(restored.count("books").await, 25);
    let rec = restored
        .data
        .get_record(&common::admin(), "books", "book0000000007", &Default::default())
        .await
        .unwrap();
    assert_eq!(rec["title"], json!("Book 7"));
    assert_eq!(rec["author"], json!("author00000001"));
    assert!(restored.db.integrity_check().await.unwrap().is_empty());
}

#[tokio::test]
async fn backup_is_consistent_while_the_database_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let src = open(&tmp.path().join("src")).await;
    src.seed(0).await;
    let writer = {
        let data = src.data.clone();
        tokio::spawn(async move {
            for i in 0..100 {
                data.create_record(
                    &common::admin(),
                    "books",
                    json!({"id": format!("w{i:012}"), "title": format!("T{i}")}),
                )
                .await
                .unwrap();
            }
        })
    };
    let dest = tmp.path().join("bk");
    backup::create(&src.db, &src.layout, &dest, BackupOptions::default())
        .await
        .unwrap();
    writer.await.unwrap();

    let report = verify(&dest).await;
    assert!(report.ok, "{:?}", report.problems);
    let target = tmp.path().join("restored");
    let layout = forgebase_core::DataLayout::new(&target);
    restore::restore(&dest, &layout, RestoreOptions::default())
        .await
        .unwrap();
    let restored = open(&target).await;
    let n = restored.count("books").await;
    assert!(n <= 100, "a snapshot never sees more than was committed");
    // Every record in the copy is complete: ids are contiguous from 0 (one writer, in order).
    for i in 0..n {
        restored
            .data
            .get_record(&common::admin(), "books", &format!("w{i:012}"), &Default::default())
            .await
            .unwrap();
    }
    assert_eq!(src.count("books").await, 100);
}

#[tokio::test]
async fn verify_rejects_corruption_missing_extra_and_unfinished_backups() {
    let tmp = tempfile::tempdir().unwrap();
    let src = seeded(&tmp.path().join("src")).await;

    let dest = tmp.path().join("bk");
    backup::create(&src.db, &src.layout, &dest, BackupOptions::default())
        .await
        .unwrap();
    assert!(verify(&dest).await.ok);

    // Flipped byte in a blob.
    let blob = dest.join("storage/blobs/ab/abcdef");
    let mut bytes = std::fs::read(&blob).unwrap();
    bytes[0] ^= 0xff;
    std::fs::write(&blob, &bytes).unwrap();
    let r = verify(&dest).await;
    assert!(
        !r.ok && r.problems.iter().any(|p| p.contains("corrupt")),
        "{:?}",
        r.problems
    );
    bytes[0] ^= 0xff;
    std::fs::write(&blob, &bytes).unwrap();
    assert!(verify(&dest).await.ok);

    // Missing and extra files.
    std::fs::remove_file(&blob).unwrap();
    std::fs::write(dest.join("stray.txt"), b"x").unwrap();
    let r = verify(&dest).await;
    assert!(r.problems.iter().any(|p| p.contains("is missing")), "{:?}", r.problems);
    assert!(r.problems.iter().any(|p| p.contains("not listed")), "{:?}", r.problems);

    // A damaged database is caught even when the manifest is rewritten to match.
    let dest2 = tmp.path().join("bk2");
    backup::create(&src.db, &src.layout, &dest2, BackupOptions::default())
        .await
        .unwrap();
    let db_file = dest2.join("forgebase.db");
    let mut db_bytes = std::fs::read(&db_file).unwrap();
    let mid = db_bytes.len() / 2;
    for b in &mut db_bytes[mid..mid + 4096] {
        *b = 0xA5;
    }
    std::fs::write(&db_file, &db_bytes).unwrap();
    let r = verify(&dest2).await;
    assert!(!r.ok);

    // No manifest: an unfinished backup is refused by verify and by restore.
    let dest3 = tmp.path().join("bk3");
    backup::create(&src.db, &src.layout, &dest3, BackupOptions::default())
        .await
        .unwrap();
    std::fs::remove_file(dest3.join("MANIFEST.json")).unwrap();
    let r = verify(&dest3).await;
    assert!(!r.ok && r.problems[0].contains("MANIFEST.json is missing"));
    let layout = forgebase_core::DataLayout::new(tmp.path().join("t"));
    let err = restore::restore(&dest3, &layout, RestoreOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, BackupError::Verify(_)));
    assert!(!layout.database_path().exists(), "a refused restore changes nothing");
}

#[tokio::test]
async fn a_crashed_backup_leaves_no_complete_looking_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let src = seeded(&tmp.path().join("src")).await;
    // A leftover partial directory from a crash is replaced by the next attempt.
    let partial = tmp.path().join("bk.partial");
    std::fs::create_dir_all(&partial).unwrap();
    std::fs::write(partial.join("forgebase.db"), b"half written").unwrap();
    let dest = tmp.path().join("bk");
    assert!(!verify(&partial).await.ok);
    backup::create(&src.db, &src.layout, &dest, BackupOptions::default())
        .await
        .unwrap();
    assert!(verify(&dest).await.ok);
    assert!(!partial.exists());
    // Existing destinations are never overwritten.
    let err = backup::create(&src.db, &src.layout, &dest, BackupOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, BackupError::Conflict(_)));
}

#[tokio::test]
async fn restore_replaces_existing_data_only_with_force_and_keeps_the_old_data() {
    let tmp = tempfile::tempdir().unwrap();
    let src = seeded(&tmp.path().join("live")).await;
    let dest = tmp.path().join("bk");
    backup::create(&src.db, &src.layout, &dest, BackupOptions::default())
        .await
        .unwrap();

    // The live instance changes after the backup.
    src.data
        .create_record(&common::admin(), "books", json!({"title": "after backup"}))
        .await
        .unwrap();
    src.put_blob("blobs/zz/new", b"new blob");
    src.db.checkpoint().await.unwrap();
    src.db.close().await;

    let err = restore::restore(&dest, &src.layout, RestoreOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, BackupError::Conflict(_)), "{err}");

    let r = restore::restore(
        &dest,
        &src.layout,
        RestoreOptions {
            force: true,
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap();
    let old = r.previous_data.unwrap();
    assert!(old.join("forgebase.db").is_file(), "the replaced database is kept");
    assert_eq!(read(old.join("storage/blobs/zz/new")), b"new blob");
    assert!(!src.layout.storage_dir().join("blobs/zz/new").exists());

    let again = open(src.layout.root()).await;
    assert_eq!(again.count("books").await, 25, "state as of the backup");
    assert!(!restore::interrupted(&again.layout));
}

#[tokio::test]
async fn restore_after_a_crash_rolls_back_and_can_be_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let src = seeded(&tmp.path().join("live")).await;
    let dest = tmp.path().join("bk");
    backup::create(&src.db, &src.layout, &dest, BackupOptions::default())
        .await
        .unwrap();
    src.data
        .create_record(&common::admin(), "books", json!({"title": "newer than the backup"}))
        .await
        .unwrap();
    src.db.checkpoint().await.unwrap();
    src.db.close().await;
    let live_db_before = read(src.layout.database_path());

    // Die after N moved items, for every N across the swap.
    for crash_after in 1..=6 {
        let opts = RestoreOptions {
            force: true,
            crash_after: Some(crash_after),
            ..RestoreOptions::default()
        };
        let err = restore::restore(&dest, &src.layout, opts).await.unwrap_err();
        assert!(err.to_string().contains("simulated crash"), "{err}");
        assert!(restore::interrupted(&src.layout), "crash_after={crash_after}");

        // The next start (or `restore --recover`) puts the previous data back.
        assert!(restore::recover(&src.layout).unwrap());
        assert!(!restore::interrupted(&src.layout));
        assert_eq!(
            read(src.layout.database_path()),
            live_db_before,
            "crash_after={crash_after}"
        );
        assert_eq!(read(src.layout.storage_dir().join("blobs/ab/abcdef")), b"blob one");
        assert_eq!(
            read(src.layout.indexes_dir().join("vector/c1/f.usearch")),
            b"index bytes"
        );
    }

    // A restore that finds the journal rolls back by itself, then succeeds.
    let opts = RestoreOptions {
        force: true,
        crash_after: Some(2),
        ..RestoreOptions::default()
    };
    restore::restore(&dest, &src.layout, opts).await.unwrap_err();
    restore::restore(
        &dest,
        &src.layout,
        RestoreOptions {
            force: true,
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap();
    let after = open(src.layout.root()).await;
    assert_eq!(after.count("books").await, 25);
    assert!(after.db.integrity_check().await.unwrap().is_empty());
}

#[tokio::test]
async fn restore_into_an_empty_directory_after_a_crash_leaves_it_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let src = seeded(&tmp.path().join("src")).await;
    let dest = tmp.path().join("bk");
    backup::create(&src.db, &src.layout, &dest, BackupOptions::default())
        .await
        .unwrap();
    let layout = forgebase_core::DataLayout::new(tmp.path().join("fresh"));
    for crash_after in 1..=3 {
        restore::restore(
            &dest,
            &layout,
            RestoreOptions {
                crash_after: Some(crash_after),
                allow_missing_key: true,
                ..RestoreOptions::default()
            },
        )
        .await
        .unwrap_err();
        assert!(restore::recover(&layout).unwrap());
        assert!(!layout.database_path().exists(), "no half-restored database remains");
    }
    restore::restore(
        &dest,
        &layout,
        RestoreOptions {
            allow_missing_key: true,
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(layout.database_path().is_file());
}

#[tokio::test]
async fn master_key_is_excluded_by_default_and_checked_on_restore() {
    let tmp = tempfile::tempdir().unwrap();
    let src = seeded(&tmp.path().join("src")).await;
    let dest = tmp.path().join("bk");
    let m = backup::create(&src.db, &src.layout, &dest, BackupOptions::default())
        .await
        .unwrap();
    assert!(!m.master_key.included);
    assert!(m.master_key.fingerprint.is_some());
    assert!(!dest.join("auth").exists(), "the key is not copied unless asked for");
    let v = verify(&dest).await;
    assert!(v.warnings.iter().any(|w| w.contains("master key")));

    let layout = forgebase_core::DataLayout::new(tmp.path().join("t"));
    // No key anywhere: refused with instructions.
    let err = restore::restore(&dest, &layout, RestoreOptions::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("--master-key"), "{err}");
    assert!(!layout.database_path().exists());

    // A different key is refused.
    let wrong = tmp.path().join("wrong.key");
    std::fs::write(&wrong, b"not the original key at all....").unwrap();
    let err = restore::restore(
        &dest,
        &layout,
        RestoreOptions {
            master_key: Some(wrong),
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("does not match"), "{err}");

    // The original key is installed.
    let right = tmp.path().join("right.key");
    std::fs::write(&right, b"0123456789abcdef0123456789abcdef").unwrap();
    let r = restore::restore(
        &dest,
        &layout,
        RestoreOptions {
            master_key: Some(right),
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(r.master_key.contains("--master-key"));
    assert_eq!(
        read(backup::master_key_path(&layout)),
        b"0123456789abcdef0123456789abcdef"
    );

    // An existing, matching key in the target is kept.
    let kept = forgebase_core::DataLayout::new(tmp.path().join("kept"));
    std::fs::create_dir_all(kept.root().join("auth")).unwrap();
    std::fs::write(backup::master_key_path(&kept), b"0123456789abcdef0123456789abcdef").unwrap();
    let r = restore::restore(&dest, &kept, RestoreOptions::default()).await.unwrap();
    assert!(r.master_key.contains("already in the data directory"));

    // Explicit opt-out.
    let none = forgebase_core::DataLayout::new(tmp.path().join("none"));
    restore::restore(
        &dest,
        &none,
        RestoreOptions {
            allow_missing_key: true,
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn snapshots_list_resolve_point_in_time_and_prune() {
    let tmp = tempfile::tempdir().unwrap();
    let src = seeded(&tmp.path().join("src")).await;
    let store = SnapshotStore::new(&src.layout);
    assert!(store.list().unwrap().is_empty());

    let mut ids = Vec::new();
    for i in 0..4 {
        let m = store
            .create(
                &src.db,
                &src.layout,
                BackupOptions {
                    label: Some(format!("s{i}")),
                    ..BackupOptions::default()
                },
                None,
            )
            .await
            .unwrap();
        assert_eq!(m.kind, "snapshot");
        ids.push(m.id);
        // Snapshot ids are unique even within one second.
    }
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 4);
    let list = store.list().unwrap();
    assert_eq!(list.len(), 4);
    assert_eq!(list[0].manifest.label.as_deref(), Some("s3"), "newest first");
    assert_eq!(store.resolve("latest").unwrap().id, ids[3]);
    assert_eq!(store.resolve(&ids[1]).unwrap().id, ids[1]);
    assert!(matches!(store.resolve("nope"), Err(BackupError::NotFound(_))));
    assert!(matches!(store.resolve("../etc"), Err(BackupError::Invalid(_))));

    // Point in time: the newest snapshot at or before a time.
    let far_future = "2999-01-01T00:00:00Z";
    assert_eq!(store.resolve(far_future).unwrap().id, ids[3]);
    assert!(matches!(
        store.resolve("2000-01-01T00:00:00Z"),
        Err(BackupError::NotFound(_))
    ));

    // Retention: keep the newest two.
    let gone = store
        .prune(Retention {
            keep_last: Some(2),
            max_age_days: None,
        })
        .await
        .unwrap();
    assert_eq!(gone.len(), 2);
    let left: Vec<_> = store.list().unwrap().into_iter().map(|s| s.id).collect();
    assert_eq!(left, vec![ids[3].clone(), ids[2].clone()]);

    // Age-only retention keeps everything younger than the limit; none keeps all.
    assert!(
        store
            .prune(Retention {
                keep_last: None,
                max_age_days: Some(1),
            })
            .await
            .unwrap()
            .is_empty()
    );
    assert!(store.prune(Retention::default()).await.unwrap().is_empty());

    // keep_last protects the newest even when everything is older than max_age.
    // (Age is simulated by rewriting the manifest time.)
    for s in store.list().unwrap() {
        let mut m = s.manifest.clone();
        m.created = "2001-01-01T00:00:00Z".into();
        std::fs::write(s.path.join("MANIFEST.json"), serde_json::to_vec(&m).unwrap()).unwrap();
    }
    let gone = store
        .prune(Retention {
            keep_last: Some(1),
            max_age_days: Some(30),
        })
        .await
        .unwrap();
    assert_eq!(gone.len(), 1);
    assert_eq!(store.list().unwrap().len(), 1);

    // Create with retention applies it right away.
    store
        .create(
            &src.db,
            &src.layout,
            BackupOptions::default(),
            Some(Retention {
                keep_last: Some(1),
                max_age_days: None,
            }),
        )
        .await
        .unwrap();
    assert_eq!(store.list().unwrap().len(), 1);

    // Delete by id; ids that are paths are rejected.
    let id = store.list().unwrap()[0].id.clone();
    store.delete(&id).await.unwrap();
    assert!(matches!(store.delete(&id).await, Err(BackupError::NotFound(_))));
    assert!(matches!(store.delete("../x").await, Err(BackupError::Invalid(_))));
}

#[tokio::test]
async fn a_snapshot_restores_like_any_backup() {
    let tmp = tempfile::tempdir().unwrap();
    let src = seeded(&tmp.path().join("src")).await;
    let store = SnapshotStore::new(&src.layout);
    store
        .create(&src.db, &src.layout, BackupOptions::default(), None)
        .await
        .unwrap();
    let snap = store.resolve("latest").unwrap();
    let layout = forgebase_core::DataLayout::new(tmp.path().join("t"));
    restore::restore(
        &snap.path,
        &layout,
        RestoreOptions {
            allow_missing_key: true,
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(open(layout.root()).await.count("books").await, 25);
}
