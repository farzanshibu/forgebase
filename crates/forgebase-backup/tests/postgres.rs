//! FB-150: backups, verification, restore, snapshots and pre-migration
//! snapshots on the bundled PostgreSQL engine, and logical backups of an
//! external server. Real clusters, real `pg_basebackup`.
//!
//! Runs with `FORGEBASE_TEST_BACKEND=postgres` and an engine in
//! `FORGEBASE_PG_ENGINE_DIR` (or `FORGEBASE_TEST_PG_ENGINE`); skips otherwise.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{open_pg, open_pg_with, pg_engine_dir, read, short_tempdir};
use forgebase_backup::manifest::{FileKind, Method};
use forgebase_backup::restore::{self, RestoreOptions};
use forgebase_backup::{BackupError, BackupOptions, PgSource, Retention, SnapshotStore, VerifyOptions, backup};
use forgebase_core::DataLayout;
use forgebase_pgengine::{Role, RoleSecrets};
use serde_json::json;

fn tools(src: &PgSource) -> PgSource {
    PgSource::tools_only(src.engine_root.clone(), src.version.clone())
}

fn verify_opts(src: &PgSource, layout: &DataLayout) -> VerifyOptions {
    VerifyOptions {
        pg: Some(tools(src)),
        tmp: Some(layout.tmp_dir()),
    }
}

async fn seeded(engine: &std::path::Path, root: &std::path::Path) -> common::PgInstance {
    let pg = open_pg(engine, root).await;
    pg.inst.seed(25).await;
    pg.inst.put_blob("blobs/ab/abcdef", b"blob one");
    pg.inst.put_index("vector/c1/f.usearch", b"index bytes");
    pg.inst.put_key(b"0123456789abcdef0123456789abcdef");
    pg
}

async fn add_books(inst: &common::Instance, from: usize, to: usize) {
    for i in from..to {
        inst.data
            .create_record(
                &common::admin(),
                "books",
                json!({"id": format!("book{i:010}"), "title": format!("Book {i}"), "author": "author00000001"}),
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn physical_backup_verifies_and_restores_into_an_empty_directory() {
    let Some(engine) = pg_engine_dir() else { return };
    let tmp = short_tempdir();
    let src = seeded(&engine, &tmp.path().join("src")).await;
    let dest = tmp.path().join("bk");
    let m = backup::create(
        &src.inst.db,
        &src.inst.layout,
        &dest,
        BackupOptions {
            include_master_key: true,
            ..src.opts()
        },
    )
    .await
    .unwrap();

    // Manifest: engine, format, files.
    assert_eq!(m.format, 2);
    let e = m.engine_info();
    assert_eq!(
        (e.kind.as_str(), e.mode.as_deref(), e.method),
        ("postgres", Some("managed"), Method::Basebackup)
    );
    assert_eq!(e.major, src.src.major());
    assert!(
        m.files
            .iter()
            .any(|f| f.path == "postgres/PG_VERSION" && f.kind == FileKind::Postgres)
    );
    assert!(m.files.iter().any(|f| f.path == "postgres/backup_manifest"));
    assert!(m.files.iter().any(|f| f.path == "roles.secret"));
    assert!(m.files.iter().any(|f| f.path == "storage/blobs/ab/abcdef"));
    assert!(
        !m.files.iter().any(|f| f.path.starts_with("postgres/engine")),
        "the extracted engine is never backed up"
    );
    assert!(m.schema_version > 0);
    assert!(!dest.with_file_name("bk.partial").exists());
    assert!(
        !src.inst
            .layout
            .tmp_dir()
            .read_dir()
            .is_ok_and(|mut d| { d.any(|e| e.unwrap().file_name().to_string_lossy().starts_with(".pgpass")) })
    );

    // Verify: with the engine's tools pg_verifybackup runs; without, it says so.
    let r = forgebase_backup::verify_with(&dest, &verify_opts(&src.src, &src.inst.layout)).await;
    assert!(r.ok, "{:?}", r.problems);
    assert!(!r.warnings.iter().any(|w| w.contains("pg_verifybackup was not run")));
    let r = forgebase_backup::verify(&dest).await;
    assert!(r.ok);
    assert!(
        r.warnings.iter().any(|w| w.contains("pg_verifybackup was not run")),
        "{:?}",
        r.warnings
    );

    // The source keeps working while and after it was backed up.
    add_books(&src.inst, 25, 27).await;
    assert_eq!(src.inst.count("books").await, 27);

    // Restore into another, empty data directory and open it.
    let target = tmp.path().join("restored");
    let layout = DataLayout::new(&target);
    let rep = restore::restore(
        &dest,
        &layout,
        RestoreOptions {
            pg: Some(tools(&src.src)),
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(rep.previous_data.is_none());
    assert_eq!(rep.engine, format!("postgres {} (managed)", src.src.version));
    assert_eq!(read(layout.storage_dir().join("blobs/ab/abcdef")), b"blob one");
    assert_eq!(
        read(forgebase_backup::backup::master_key_path(&layout)),
        b"0123456789abcdef0123456789abcdef"
    );
    for dir in ["pg_wal/archive_status", "pg_tblspc", "pg_replslot"] {
        assert!(target.join("postgres/data").join(dir).is_dir(), "{dir} is restored");
    }
    // roles.secret of the backup unlocks the restored cluster.
    assert!(
        RoleSecrets::load(&target.join("postgres/roles.secret"))
            .unwrap()
            .is_some()
    );

    let restored = open_pg(&engine, &target).await;
    assert_eq!(
        restored.inst.count("books").await,
        25,
        "the backup holds the state at backup time"
    );
    let rec = restored
        .inst
        .data
        .get_record(&common::admin(), "books", "book0000000007", &Default::default())
        .await
        .unwrap();
    assert_eq!(rec["title"], json!("Book 7"));
    restored.close().await;
    src.close().await;
}

#[tokio::test]
async fn damage_is_caught_by_the_hashes_and_by_pg_verifybackup() {
    let Some(engine) = pg_engine_dir() else { return };
    let tmp = short_tempdir();
    let src = seeded(&engine, &tmp.path().join("src")).await;
    let dest = tmp.path().join("bk");
    let m = backup::create(&src.inst.db, &src.inst.layout, &dest, src.opts())
        .await
        .unwrap();
    let opts = verify_opts(&src.src, &src.inst.layout);

    // The biggest relation file of the copy.
    let victim = m
        .files
        .iter()
        .filter(|f| f.path.starts_with("postgres/base/"))
        .max_by_key(|f| f.size)
        .unwrap()
        .clone();
    let path = dest.join(&victim.path);
    let original = std::fs::read(&path).unwrap();
    let mut bad = original.clone();
    bad[original.len() / 2] ^= 0xFF;
    std::fs::write(&path, &bad).unwrap();
    let r = forgebase_backup::verify_with(&dest, &opts).await;
    assert!(!r.ok);
    assert!(
        r.problems
            .iter()
            .any(|p| p.contains("corrupt") && p.contains(&victim.path)),
        "{:?}",
        r.problems
    );

    // Hide the damage from our own manifest: pg_verifybackup still sees it.
    let (sha, size) = {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(&bad);
        (
            d.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            bad.len() as u64,
        )
    };
    let mpath = dest.join("MANIFEST.json");
    let mut doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&mpath).unwrap()).unwrap();
    for f in doc["files"].as_array_mut().unwrap() {
        if f["path"] == victim.path.as_str() {
            f["sha256"] = json!(sha);
            f["size"] = json!(size);
        }
    }
    std::fs::write(&mpath, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();
    let r = forgebase_backup::verify_with(&dest, &opts).await;
    assert!(!r.ok);
    assert!(
        r.problems.iter().any(|p| p.contains("pg_verifybackup failed")),
        "{:?}",
        r.problems
    );
    // Without the tools the same backup passes: that is why `restore` and `doctor` pass them.
    assert!(forgebase_backup::verify(&dest).await.ok);

    // Restore refuses it.
    let layout = DataLayout::new(tmp.path().join("t"));
    let err = restore::restore(
        &dest,
        &layout,
        RestoreOptions {
            pg: Some(tools(&src.src)),
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, BackupError::Verify(_)), "{err}");
    assert!(!layout.root().join("postgres/data").exists(), "nothing was installed");
    src.close().await;
}

#[tokio::test]
async fn restore_swaps_the_cluster_and_an_interrupted_restore_rolls_back() {
    let Some(engine) = pg_engine_dir() else { return };
    let tmp = short_tempdir();
    let root = tmp.path().join("src");
    let src = seeded(&engine, &root).await;
    let dest = tmp.path().join("bk");
    backup::create(&src.inst.db, &src.inst.layout, &dest, src.opts())
        .await
        .unwrap();
    add_books(&src.inst, 25, 30).await;
    let (layout, pg) = (src.inst.layout.clone(), tools(&src.src));
    let live_secret = std::fs::read(root.join("postgres/roles.secret")).unwrap();
    src.close().await;

    let opts = |crash_after, force| RestoreOptions {
        force,
        pg: Some(pg.clone()),
        crash_after,
        ..RestoreOptions::default()
    };
    // Without --force an existing cluster is not touched.
    let err = restore::restore(&dest, &layout, opts(None, false)).await.unwrap_err();
    assert!(matches!(err, BackupError::Conflict(_)), "{err}");

    // A crash in the middle: the journal stays, `recover` puts the old cluster back.
    restore::restore(&dest, &layout, opts(Some(2), true)).await.unwrap_err();
    assert!(restore::interrupted(&layout));
    assert!(restore::recover(&layout).unwrap());
    assert!(!restore::interrupted(&layout));
    assert_eq!(std::fs::read(root.join("postgres/roles.secret")).unwrap(), live_secret);
    let again = open_pg(&engine, &root).await;
    assert_eq!(again.inst.count("books").await, 30, "rolled back to the live cluster");
    again.close().await;

    // The real restore: the old cluster is kept aside.
    let rep = restore::restore(&dest, &layout, opts(None, true)).await.unwrap();
    let aside = rep.previous_data.expect("the replaced cluster is kept");
    assert!(aside.join("postgres/data/PG_VERSION").is_file());
    assert!(aside.join("postgres/roles.secret").is_file());
    let after = open_pg(&engine, &root).await;
    assert_eq!(after.inst.count("books").await, 25);
    after.close().await;
}

#[tokio::test]
async fn restore_refuses_what_it_cannot_take() {
    let Some(engine) = pg_engine_dir() else { return };
    let tmp = short_tempdir();
    let src = seeded(&engine, &tmp.path().join("src")).await;
    let dest = tmp.path().join("bk");
    backup::create(&src.inst.db, &src.inst.layout, &dest, src.opts())
        .await
        .unwrap();

    // The cluster still runs: a restore over it is refused.
    let err = restore::restore(
        &dest,
        &src.inst.layout,
        RestoreOptions {
            force: true,
            pg: Some(tools(&src.src)),
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("still running"), "{err}");

    // Another major.
    let other = PgSource::tools_only(src.src.engine_root.clone(), "17.2");
    let err = restore::restore(
        &dest,
        &DataLayout::new(tmp.path().join("t1")),
        RestoreOptions {
            pg: Some(other),
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("same major"), "{err}");

    // A SQLite data directory: the backend does not change by restore.
    let sqlite_root = tmp.path().join("lite");
    let lite = common::open(&sqlite_root).await;
    lite.db.close().await;
    let err = restore::restore(
        &dest,
        &lite.layout,
        RestoreOptions {
            force: true,
            pg: Some(tools(&src.src)),
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("db convert"), "{err}");

    // More than one data set (replicas, shards): FB-179.
    let multi = DataLayout::new(tmp.path().join("multi"));
    multi.ensure().unwrap();
    std::fs::create_dir_all(multi.root().join("postgres/nodes/replica1")).unwrap();
    let err = restore::restore(
        &dest,
        &multi,
        RestoreOptions {
            pg: Some(tools(&src.src)),
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string().contains("more than one PostgreSQL cluster") && err.to_string().contains("backup --cluster"),
        "{err}"
    );
    std::fs::create_dir_all(src.root.join("postgres/nodes/shard1")).unwrap();
    let err = backup::create(&src.inst.db, &src.inst.layout, &tmp.path().join("bk2"), src.opts())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("backup --cluster"), "{err}");
    std::fs::remove_dir_all(src.root.join("postgres/nodes")).unwrap();

    // A PostgreSQL database without the engine's tools cannot be backed up.
    let err = backup::create(
        &src.inst.db,
        &src.inst.layout,
        &tmp.path().join("bk3"),
        BackupOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("PgSource"), "{err}");
    src.close().await;
}

#[tokio::test]
async fn snapshots_and_pre_migration_snapshots() {
    let Some(engine) = pg_engine_dir() else { return };
    let tmp = short_tempdir();
    let src = seeded(&engine, &tmp.path().join("src")).await;
    let store = SnapshotStore::new(&src.inst.layout);

    let a = store
        .create(&src.inst.db, &src.inst.layout, src.opts(), None)
        .await
        .unwrap();
    let b = store
        .create(
            &src.inst.db,
            &src.inst.layout,
            src.opts(),
            Some(Retention {
                keep_last: Some(1),
                max_age_days: None,
            }),
        )
        .await
        .unwrap();
    assert_eq!(a.kind, "snapshot");
    let all = store.list().unwrap();
    assert_eq!(all.len(), 1, "retention keeps the newest");
    assert_eq!(all[0].id, b.id);
    assert_eq!(store.resolve("latest").unwrap().id, b.id);
    assert!(
        forgebase_backup::verify_with(&all[0].path, &verify_opts(&src.src, &src.inst.layout))
            .await
            .ok
    );

    // The pre-migration hook: make the newest migration pending again, so
    // `migrate` has work on a database that holds data. The snapshot is taken
    // first, whatever the migration then does.
    let latest = src.inst.db.schema_version().await.unwrap();
    sqlx_exec(
        &src,
        &format!("DELETE FROM forgebase._forgebase_migrations WHERE version = {latest}"),
    )
    .await;
    let _ = src.inst.db.migrate(Some(&src.inst.layout.backups_dir())).await;
    let dir = std::fs::read_dir(src.inst.layout.backups_dir())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(&format!("pre-migrate-v{}-", latest - 1))
        });
    // The previous version number depends on numbering gaps; accept any pre-migrate dir.
    let dir = dir.or_else(|| {
        std::fs::read_dir(src.inst.layout.backups_dir())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.file_name().unwrap().to_string_lossy().starts_with("pre-migrate-v"))
    });
    let dir = dir.expect("the migration took a pre-migration snapshot");
    let m = forgebase_backup::Manifest::read(&dir).unwrap();
    assert!(m.database_only);
    assert_eq!(m.kind, "pre-migration");
    assert!(m.is_physical_postgres());
    assert!(!m.files.iter().any(|f| f.path.starts_with("storage/")));
    assert!(
        forgebase_backup::verify_with(&dir, &verify_opts(&src.src, &src.inst.layout))
            .await
            .ok
    );

    // Restoring it replaces the database and leaves storage and the key alone.
    add_books(&src.inst, 25, 28).await;
    src.inst.put_blob("blobs/new/after", b"written after the snapshot");
    let (layout, pg, root) = (src.inst.layout.clone(), tools(&src.src), src.root.clone());
    src.close().await;
    let rep = restore::restore(
        &dir,
        &layout,
        RestoreOptions {
            force: true,
            pg: Some(pg),
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(rep.master_key.contains("database-only"));
    assert_eq!(
        read(layout.storage_dir().join("blobs/new/after")),
        b"written after the snapshot"
    );
    assert_eq!(read(layout.storage_dir().join("blobs/ab/abcdef")), b"blob one");
    assert_eq!(
        read(forgebase_backup::backup::master_key_path(&layout)),
        b"0123456789abcdef0123456789abcdef"
    );
    // (The snapshot was taken mid-hack: its migration row is missing, so do not migrate.)
    let back = open_pg_with(&engine, &root, false).await;
    assert_eq!(back.inst.count("books").await, 25);
    back.close().await;
}

async fn sqlx_exec(pg: &common::PgInstance, sql: &str) {
    use sqlx::Connection;
    let mut conn = sqlx::PgConnection::connect(pg.engine.url(Role::Super).expose())
        .await
        .unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_owned()))
        .execute(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();
}

#[tokio::test]
async fn an_external_server_gets_a_logical_backup_that_restores_in_place() {
    let Some(engine) = pg_engine_dir() else { return };
    let tmp = short_tempdir();
    let host = open_pg(&engine, &tmp.path().join("host")).await;
    // "External" here: a second database of the same server, reached by URL.
    sqlx_exec(&host, "CREATE DATABASE fbext OWNER forgebase_owner").await;
    let url = host.engine.url_for(Role::Super, "fbext");
    let src = PgSource::external(Some(engine.clone()), host.src.version.clone(), url.expose());

    let root = tmp.path().join("ext");
    let layout = DataLayout::new(&root);
    layout.ensure().unwrap();
    let cfg = forgebase_core::Config::defaults(&root);
    let db = forgebase_db::Database::open(forgebase_db::DbOptions::from_config_target(
        &cfg.database,
        forgebase_db::DbTarget::postgres(url.expose(), false),
    ))
    .await
    .unwrap();
    db.migrate(Some(&layout.backups_dir())).await.unwrap();
    let data = forgebase_data::DataService::new(
        std::sync::Arc::new(db.clone()),
        std::sync::Arc::new(forgebase_data::DefaultAuthorizer),
    )
    .await
    .unwrap();
    let inst = common::Instance {
        layout: layout.clone(),
        db: db.clone(),
        data,
    };
    inst.seed(10).await;
    inst.put_blob("blobs/ab/abcdef", b"blob one");

    let dest = tmp.path().join("bk");
    let m = backup::create(
        &db,
        &layout,
        &dest,
        BackupOptions {
            pg: Some(src.clone()),
            ..BackupOptions::default()
        },
    )
    .await
    .unwrap();
    let e = m.engine_info();
    assert_eq!((e.mode.as_deref(), e.method), (Some("external"), Method::Dump));
    assert!(
        m.files
            .iter()
            .any(|f| f.path == "postgres.dump" && f.kind == FileKind::Dump)
    );
    assert!(m.files.iter().any(|f| f.path == "storage/blobs/ab/abcdef"));
    let r = forgebase_backup::verify_with(&dest, &verify_opts(&src, &layout)).await;
    assert!(r.ok, "{:?}", r.problems);
    assert!(!r.warnings.iter().any(|w| w.contains("not run")), "{:?}", r.warnings);

    // Change the data, then restore the dump in place (needs --force).
    add_books(&inst, 10, 14).await;
    assert_eq!(inst.count("books").await, 14);
    db.close().await;
    let opts = |force| RestoreOptions {
        force,
        pg: Some(src.clone()),
        ..RestoreOptions::default()
    };
    let err = restore::restore(&dest, &layout, opts(false)).await.unwrap_err();
    assert!(matches!(err, BackupError::Conflict(_)), "{err}");
    restore::restore(&dest, &layout, opts(true)).await.unwrap();

    let db = forgebase_db::Database::open(forgebase_db::DbOptions::from_config_target(
        &cfg.database,
        forgebase_db::DbTarget::postgres(url.expose(), false),
    ))
    .await
    .unwrap();
    let data = forgebase_data::DataService::new(
        std::sync::Arc::new(db.clone()),
        std::sync::Arc::new(forgebase_data::DefaultAuthorizer),
    )
    .await
    .unwrap();
    let back = common::Instance {
        layout,
        db: db.clone(),
        data,
    };
    assert_eq!(back.count("books").await, 10);
    db.close().await;
    host.close().await;
}
