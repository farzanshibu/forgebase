//! FB-179: backup and restore of a sharded cluster (control + two shards,
//! rows on each): one backup per set under one directory, the combined
//! manifest with the placement epoch, a restore of every set, the schema
//! reconcile of a set restored below head, and the refusals. Real, separate
//! PostgreSQL clusters from `forgebase_db::shard_testing`; skipped without an
//! engine (`FORGEBASE_TEST_PG_ENGINE` / `FORGEBASE_PG_ENGINE_DIR`).
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use forgebase_backup::manifest::Method;
use forgebase_backup::topology::{
    self, ClusterBackupOptions, ClusterRestoreOptions, CompleteOptions, NodeSource, SetSource,
};
use forgebase_backup::{BackupError, BackupOptions, Password, PgSource, VerifyOptions};
use forgebase_core::{BackupSource, DataLayout};
use forgebase_db::backend::{Backend, Statement, TxMode, Value};
use forgebase_db::shard_testing::{self, MapConnector, Sharded};
use forgebase_db::shards::{self, ShardSetup};
use forgebase_db::topology as dbtopo;
use forgebase_db::topology::{NodeRole, NodeState, PlacementState, SetKind, SetState};
use forgebase_db::{Database, DbOptions, DbRouter, DbTarget, RouterOptions, SecretString};

const SOCKET_PORT: u16 = 5432;

fn short_tempdir(prefix: &str) -> tempfile::TempDir {
    let base = std::env::var_os("FORGEBASE_TEST_PG_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&base).unwrap();
    tempfile::Builder::new().prefix(prefix).tempdir_in(base).unwrap()
}

/// `18.3` from the engine's own `postgres --version`.
fn engine_version(engine: &Path) -> String {
    let out = Command::new(engine.join("bin/postgres")).arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.split_whitespace()
        .last()
        .map(|v| v.trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.').to_owned())
        .unwrap()
}

fn source_of(engine: &Path, socket_dir: &Path, cluster_dir: &Path) -> PgSource {
    PgSource::managed(
        Some(engine.to_path_buf()),
        engine_version(engine),
        socket_dir,
        SOCKET_PORT,
        "postgres",
        Password::new("unused-trust-auth"),
        cluster_dir,
    )
}

fn tools(engine: &Path) -> PgSource {
    PgSource::tools_only(Some(engine.to_path_buf()), engine_version(engine))
}

async fn exec(db: &Database, sql: &str) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(sql, vec![])).await.unwrap();
    tx.commit().await.unwrap();
}

async fn marker(db: &Database, key: &str) -> Option<String> {
    db.fetch_all(&Statement::new(
        "SELECT value FROM _forgebase_meta WHERE key = ?1",
        vec![key.into()],
    ))
    .await
    .unwrap()
    .scalar()
    .and_then(Value::as_str)
    .map(str::to_owned)
}

fn set_marker_sql(key: &str, value: &str) -> String {
    format!(
        "INSERT INTO _forgebase_meta (key, value) VALUES ('{key}', '{value}') \
         ON CONFLICT (key) DO UPDATE SET value = EXCLUDED.value"
    )
}

/// A restored cluster started from its data directory on a private socket.
struct Running {
    bin: PathBuf,
    data: PathBuf,
    sock: tempfile::TempDir,
}

impl Running {
    fn start(engine: &Path, data: &Path) -> Self {
        let sock = short_tempdir("rs-");
        let bin = engine.join("bin");
        let log = sock.path().join("postgres.log");
        let out = Command::new(bin.join("pg_ctl"))
            .args(["start", "-w", "-t", "90", "-D"])
            .arg(data)
            .arg("-l")
            .arg(&log)
            .arg("-o")
            .arg(format!(
                "-c unix_socket_directories={} -c listen_addresses= -c port={SOCKET_PORT}",
                sock.path().display()
            ))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "start {}: {}\n{}",
            data.display(),
            String::from_utf8_lossy(&out.stderr),
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        Self {
            bin,
            data: data.to_path_buf(),
            sock,
        }
    }

    async fn database(&self) -> Database {
        let url = format!(
            "postgres://postgres@localhost/forgebase?host={}&port={SOCKET_PORT}",
            self.sock.path().display()
        );
        Database::open(DbOptions {
            target: DbTarget::postgres(SecretString::new(url), false),
            synchronous: forgebase_core::Synchronous::Full,
            busy_timeout: Duration::from_millis(5000),
            read_pool_size: 2,
            create_if_missing: false,
            read_only: false,
        })
        .await
        .unwrap()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = Command::new(self.bin.join("pg_ctl"))
            .args(["stop", "-m", "fast", "-w", "-D"])
            .arg(&self.data)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn no_setup() -> ShardSetup<'static> {
    ShardSetup {
        backups_dir: None,
        replay: None,
        control_conninfo: None,
    }
}

/// Control, `alpha` (active, at head) and `beta` (active but never migrated:
/// it is below the schema head), a marker row on each, a tenant on each shard.
struct Cluster {
    s: Sharded,
    alpha: (dbtopo::DbSet, Database),
    beta: (dbtopo::DbSet, Database),
}

async fn cluster() -> Option<Cluster> {
    let mut s = Sharded::start().await?;
    let alpha = s.provision("alpha").await;
    shards::activate_shard(&s.router, &alpha.0, &alpha.1, no_setup())
        .await
        .unwrap();
    // `beta` is made active without its migrations: a set that is behind.
    let beta = s.provision("beta").await;
    dbtopo::update_set(&s.control, &beta.0.id, Some(SetState::Active), None, None)
        .await
        .unwrap();
    s.router.reload().await.unwrap();
    s.router.place("tenant-a", &alpha.0.id, PlacementState::Active).await.unwrap();
    s.router.place("tenant-b", &beta.0.id, PlacementState::Active).await.unwrap();
    exec(&s.control, &set_marker_sql("fb179", "control-before")).await;
    exec(&alpha.1, &set_marker_sql("fb179", "alpha-before")).await;
    // beta has no _forgebase_meta yet (never migrated): its rows live in a plain table.
    exec(&beta.1, "CREATE TABLE fb179_rows (v text)").await;
    exec(&beta.1, "INSERT INTO fb179_rows VALUES ('beta-before')").await;
    Some(Cluster { s, alpha, beta })
}

fn set_sources(c: &Cluster, engine: &Path, scratch: &Path) -> Vec<SetSource> {
    let topo_sets = [
        (dbtopo::DbSet::control(), c.s.control.clone(), 0usize, "postgres/data".to_owned(), "control".to_owned()),
        (
            c.alpha.0.clone(),
            c.alpha.1.clone(),
            1,
            "postgres/nodes/alpha-primary/data".to_owned(),
            "alpha-primary".to_owned(),
        ),
        (
            c.beta.0.clone(),
            c.beta.1.clone(),
            2,
            "postgres/nodes/beta-primary/data".to_owned(),
            "beta-primary".to_owned(),
        ),
    ];
    topo_sets
        .into_iter()
        .map(|(set, db, idx, data_dir, node)| SetSource {
            set,
            db,
            primary: NodeSource {
                name: node,
                pg: source_of(engine, c.s.clusters[idx].socket_dir(), scratch),
                data_dir: Some(data_dir),
            },
            replicas: Vec::new(),
        })
        .collect()
}

#[tokio::test]
async fn a_sharded_cluster_backs_up_one_set_each_and_restores_every_set() {
    let Some(engine) = shard_testing::engine_dir() else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let Some(c) = cluster().await else { return };
    let tmp = short_tempdir("cl-");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let src_layout = DataLayout::new(tmp.path().join("src"));
    src_layout.ensure().unwrap();
    let topo = dbtopo::load(&c.s.control).await.unwrap();
    assert!(topo.epoch > 0 && topo.is_sharded());

    // --- backup: one directory, one backup per set, the combined manifest.
    let dest = tmp.path().join("bk");
    let m = topology::create_cluster(
        &c.s.control,
        &src_layout,
        &dest,
        ClusterBackupOptions {
            base: BackupOptions {
                label: Some("fb179".into()),
                ..BackupOptions::default()
            },
            source: BackupSource::Primary,
            sets: set_sources(&c, &engine, &scratch),
        },
    )
    .await
    .unwrap();
    assert!(dest.join("CLUSTER.json").is_file());
    assert!(dest.join("control/MANIFEST.json").is_file());
    assert!(dest.join("sets/1/MANIFEST.json").is_file());
    assert!(dest.join("sets/2/MANIFEST.json").is_file());
    assert!(!dir_has(&dest, "partial"), "no partial directory is left behind");
    assert_eq!(m.format, topology::CLUSTER_FORMAT);
    assert_eq!(m.label.as_deref(), Some("fb179"));
    assert_eq!(m.source, BackupSource::Primary);
    assert_eq!(m.topology_epoch, topo.epoch, "the placement epoch is recorded");
    assert_eq!(m.topology.sets.len(), 3);
    assert_eq!(m.placements.total, 2);
    assert_eq!(m.placements.per_set.get(&c.alpha.0.id), Some(&1));
    assert_eq!(m.placements.per_set.get(&c.beta.0.id), Some(&1));
    assert!(m.placements.max_epoch >= 1);
    let names: Vec<(&str, u16, &str, SetKind)> = m
        .sets
        .iter()
        .map(|s| (s.name.as_str(), s.number, s.dir.as_str(), s.kind))
        .collect();
    assert_eq!(
        names,
        [
            ("control", 0, "control", SetKind::Control),
            ("alpha", 1, "sets/1", SetKind::Shard),
            ("beta", 2, "sets/2", SetKind::Shard),
        ]
    );
    for e in &m.sets {
        assert_eq!(e.method, Method::Basebackup);
        assert_eq!(e.role, NodeRole::Primary);
        assert_eq!(e.timeline, Some(1), "{}", e.name);
        assert!(e.start_lsn.is_some() && e.end_lsn.is_some(), "{}", e.name);
        assert!(e.fallback.is_none());
        assert!(e.files > 10 && e.bytes > 0);
        assert_eq!(e.manifest_sha256.len(), 64);
    }
    assert_eq!(m.sets[1].node, "alpha-primary");
    assert_eq!(m.sets[1].data_dir.as_deref(), Some("postgres/nodes/alpha-primary/data"));
    let head = forgebase_db::migrations::postgres::PgMigrator::system().latest_version();
    assert_eq!(m.sets[0].schema_version, head);
    assert_eq!(m.sets[1].schema_version, head);
    assert_eq!(m.sets[2].schema_version, 0, "beta was behind");
    // The shard backups hold the database only; control also keeps storage, indexes.
    let on_disk = forgebase_backup::Manifest::read(&dest.join("sets/1")).unwrap();
    assert!(on_disk.database_only && on_disk.is_physical_postgres());
    assert!(!forgebase_backup::Manifest::read(&dest.join("control")).unwrap().database_only);
    // The manifest is what is on disk.
    assert_eq!(topology::ClusterManifest::read(&dest).unwrap(), m);

    // --- verify.
    let vopts = VerifyOptions {
        pg: Some(tools(&engine)),
        tmp: Some(src_layout.tmp_dir()),
    };
    let v = topology::verify_cluster(&dest, &vopts).await;
    assert!(v.ok, "{:?}", v.problems);
    assert_eq!(v.sets.len(), 3);
    assert!(v.files_checked >= 30);

    // Rows written after the backup must not come back.
    exec(&c.s.control, &set_marker_sql("fb179", "control-after")).await;
    exec(&c.alpha.1, &set_marker_sql("fb179", "alpha-after")).await;
    exec(&c.beta.1, "INSERT INTO fb179_rows VALUES ('beta-after')").await;

    // --- restore into an empty directory: every set where it lived.
    let dst_layout = DataLayout::new(tmp.path().join("dst"));
    let report = topology::restore_cluster(
        &dest,
        &dst_layout,
        ClusterRestoreOptions {
            base: forgebase_backup::RestoreOptions {
                pg: Some(tools(&engine)),
                allow_missing_key: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(report.sets.len(), 3);
    assert_eq!(report.topology_epoch, topo.epoch);
    assert!(report.replicas.is_empty() && report.moved_aside.is_empty());
    let root = dst_layout.root();
    for d in [
        "postgres/data",
        "postgres/nodes/alpha-primary/data",
        "postgres/nodes/beta-primary/data",
    ] {
        assert!(root.join(d).join("PG_VERSION").is_file(), "{d}");
        assert!(!root.join(d).join("standby.signal").exists());
    }
    let file = dbtopo::read_topology_file(&root.join(dbtopo::TOPOLOGY_FILE)).unwrap().unwrap();
    assert_eq!(file.topology.epoch, topo.epoch);
    assert_eq!(file.topology.sets.len(), 3);
    let pending = topology::read_pending(&dst_layout).unwrap().unwrap();
    assert_eq!(pending.backup_id, "bk");
    assert_eq!(pending.sets.len(), 3);

    // Start the three restored clusters and read what each holds.
    let rc = Running::start(&engine, &root.join("postgres/data"));
    let ra = Running::start(&engine, &root.join("postgres/nodes/alpha-primary/data"));
    let rb = Running::start(&engine, &root.join("postgres/nodes/beta-primary/data"));
    let (dc, da, db_) = (rc.database().await, ra.database().await, rb.database().await);
    assert_eq!(marker(&dc, "fb179").await.as_deref(), Some("control-before"));
    assert_eq!(marker(&da, "fb179").await.as_deref(), Some("alpha-before"));
    let rows = db_
        .fetch_all(&Statement::new("SELECT v FROM fb179_rows ORDER BY v", vec![]))
        .await
        .unwrap();
    assert_eq!(rows.rows.len(), 1, "the row written after the backup is gone");
    assert_eq!(rows.rows[0][0].as_str(), Some("beta-before"));
    // The placements came back with control.
    let placements = dbtopo::load_placements(&dc).await.unwrap();
    assert_eq!(placements.len(), 2);

    // --- schema reconcile: beta is below head; control and alpha are not.
    let connector = std::sync::Arc::new(MapConnector::default());
    connector.insert("alpha-primary", da.clone());
    connector.insert("beta-primary", db_.clone());
    let router = DbRouter::open(
        dc.clone(),
        RouterOptions {
            connector: Some(connector),
            ..RouterOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(db_.schema_version().await.unwrap(), 0);
    let done = topology::complete_restore(
        &dc,
        &dst_layout,
        CompleteOptions {
            router: Some(&router),
            ..CompleteOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(done.pending && done.is_complete(), "{done:?}");
    assert_eq!(done.migrated.len(), 1, "{done:?}");
    assert_eq!(done.migrated[0].0, c.beta.0.id);
    assert!(done.migrated[0].1 >= 1);
    assert!(done.up_to_date.contains(&"control".to_owned()) && done.up_to_date.contains(&c.alpha.0.id));
    assert_eq!(db_.schema_version().await.unwrap(), head, "beta is at head after the reconcile");
    assert!(done.marked.is_empty(), "no replicas in this cluster");
    assert!(topology::read_pending(&dst_layout).unwrap().is_none(), "the pending file is gone");
    // Running it again finds nothing to do.
    let again = topology::complete_restore(&dc, &dst_layout, CompleteOptions::default()).await.unwrap();
    assert!(!again.pending);
    // The rows survived the migration.
    assert_eq!(
        db_.fetch_all(&Statement::new("SELECT count(*) FROM fb179_rows", vec![]))
            .await
            .unwrap()
            .scalar()
            .and_then(Value::as_i64),
        Some(1)
    );
    drop(router);
    for d in [dc, da, db_] {
        d.close().await;
    }
    drop((rc, ra, rb));
}

fn dir_has(dir: &Path, needle: &str) -> bool {
    std::fs::read_dir(dir.parent().unwrap())
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().contains(needle))
}

#[tokio::test]
async fn a_damaged_or_incomplete_cluster_backup_is_refused() {
    let Some(engine) = shard_testing::engine_dir() else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let Some(c) = cluster().await else { return };
    let tmp = short_tempdir("cd-");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let layout = DataLayout::new(tmp.path().join("src"));
    layout.ensure().unwrap();

    // A set of the topology without a way in: refused before anything is written.
    let mut sources = set_sources(&c, &engine, &scratch);
    let dropped = sources.pop().unwrap();
    let dest = tmp.path().join("bk-missing");
    let err = topology::create_cluster(
        &c.s.control,
        &layout,
        &dest,
        ClusterBackupOptions {
            base: BackupOptions::default(),
            source: BackupSource::Primary,
            sets: sources.clone(),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, BackupError::Invalid(ref m) if m.contains("beta")), "{err}");
    assert!(!dest.exists());
    // A data directory a restore cannot use is refused too.
    sources.push(SetSource {
        primary: NodeSource {
            data_dir: Some("../outside".into()),
            ..dropped.primary.clone()
        },
        ..dropped.clone()
    });
    let err = topology::create_cluster(
        &c.s.control,
        &layout,
        &tmp.path().join("bk-bad"),
        ClusterBackupOptions {
            base: BackupOptions::default(),
            source: BackupSource::Primary,
            sets: sources,
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, BackupError::Invalid(ref m) if m.contains("data directory")), "{err}");

    // A good backup, then damage.
    let dest = tmp.path().join("bk");
    topology::create_cluster(
        &c.s.control,
        &layout,
        &dest,
        ClusterBackupOptions {
            base: BackupOptions::default(),
            source: BackupSource::Primary,
            sets: set_sources(&c, &engine, &scratch),
        },
    )
    .await
    .unwrap();
    let vopts = VerifyOptions {
        pg: Some(tools(&engine)),
        tmp: Some(layout.tmp_dir()),
    };
    assert!(topology::verify_cluster(&dest, &vopts).await.ok);

    // A byte flipped in one shard's file names that shard.
    let victim = dest.join("sets/1/postgres/PG_VERSION");
    let original = std::fs::read(&victim).unwrap();
    std::fs::write(&victim, b"99\n").unwrap();
    let v = topology::verify_cluster(&dest, &vopts).await;
    assert!(!v.ok);
    assert!(v.problems.iter().any(|p| p.starts_with("alpha:")), "{:?}", v.problems);
    // Restore refuses it, and changes nothing.
    let target = DataLayout::new(tmp.path().join("dst"));
    let err = topology::restore_cluster(&dest, &target, ClusterRestoreOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, BackupError::Verify(_)), "{err}");
    assert!(!target.root().join("postgres").exists());
    std::fs::write(&victim, original).unwrap();

    // A swapped member manifest (another set's backup under this name) is caught.
    let a = dest.join("sets/1/MANIFEST.json");
    let b = dest.join("sets/2/MANIFEST.json");
    let (ta, tb) = (std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
    std::fs::write(&a, &tb).unwrap();
    std::fs::write(&b, &ta).unwrap();
    assert!(!topology::verify_cluster(&dest, &vopts).await.ok);
    std::fs::write(&a, ta).unwrap();
    std::fs::write(&b, tb).unwrap();

    // A stray directory, and a missing combined manifest.
    std::fs::create_dir(dest.join("sets/9")).unwrap();
    let v = topology::verify_cluster(&dest, &vopts).await;
    assert!(v.problems.iter().any(|p| p.contains("sets/9")), "{:?}", v.problems);
    std::fs::remove_dir(dest.join("sets/9")).unwrap();
    assert!(topology::verify_cluster(&dest, &vopts).await.ok);
    std::fs::rename(dest.join("CLUSTER.json"), dest.join("CLUSTER.json.gone")).unwrap();
    let v = topology::verify_cluster(&dest, &vopts).await;
    assert!(!v.ok && v.problems[0].contains("CLUSTER.json"), "{:?}", v.problems);
    std::fs::rename(dest.join("CLUSTER.json.gone"), dest.join("CLUSTER.json")).unwrap();

    // A restore onto a running cluster, and onto existing data without --force.
    let root = tmp.path().join("occupied");
    let occupied = DataLayout::new(&root);
    let first = topology::restore_cluster(
        &dest,
        &occupied,
        ClusterRestoreOptions {
            base: forgebase_backup::RestoreOptions {
                pg: Some(tools(&engine)),
                allow_missing_key: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(first.sets.len(), 3);
    let err = topology::restore_cluster(
        &dest,
        &occupied,
        ClusterRestoreOptions {
            base: forgebase_backup::RestoreOptions {
                pg: Some(tools(&engine)),
                allow_missing_key: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, BackupError::Conflict(ref m) if m.contains("--force")), "{err}");
    let running = Running::start(&engine, &root.join("postgres/nodes/alpha-primary/data"));
    let err = topology::restore_cluster(
        &dest,
        &occupied,
        ClusterRestoreOptions {
            base: forgebase_backup::RestoreOptions {
                force: true,
                pg: Some(tools(&engine)),
                allow_missing_key: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, BackupError::Conflict(ref m) if m.contains("still running")), "{err}");
    drop(running);
    // Stopped, with --force: the old data is kept aside.
    let forced = topology::restore_cluster(
        &dest,
        &occupied,
        ClusterRestoreOptions {
            base: forgebase_backup::RestoreOptions {
                force: true,
                pg: Some(tools(&engine)),
                allow_missing_key: true,
                ..Default::default()
            },
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let prev = forced.previous_data.expect("old data kept");
    assert!(prev.join("postgres/nodes/alpha-primary/data/PG_VERSION").is_file());
    assert!(prev.join("postgres/data/PG_VERSION").is_file());
}

#[tokio::test]
async fn an_interrupted_cluster_restore_is_rolled_back() {
    let Some(engine) = shard_testing::engine_dir() else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return;
    };
    let Some(c) = cluster().await else { return };
    let tmp = short_tempdir("ci-");
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let layout = DataLayout::new(tmp.path().join("src"));
    layout.ensure().unwrap();
    let dest = tmp.path().join("bk");
    topology::create_cluster(
        &c.s.control,
        &layout,
        &dest,
        ClusterBackupOptions {
            base: BackupOptions::default(),
            source: BackupSource::Primary,
            sets: set_sources(&c, &engine, &scratch),
        },
    )
    .await
    .unwrap();

    // A first restore gives the directory something to lose.
    let target = DataLayout::new(tmp.path().join("dst"));
    let opts = |force: bool, crash_after: Option<usize>| ClusterRestoreOptions {
        base: forgebase_backup::RestoreOptions {
            force,
            pg: Some(tools(&engine)),
            allow_missing_key: true,
            crash_after,
            ..Default::default()
        },
        ..Default::default()
    };
    topology::restore_cluster(&dest, &target, opts(false, None)).await.unwrap();
    let marker_file = target.root().join("postgres/nodes/beta-primary/data/PG_VERSION");
    std::fs::write(target.root().join("postgres/nodes/beta-primary/data/mine"), b"keep me").unwrap();
    assert!(marker_file.is_file());

    // A crash half-way through the swap leaves the journal; the next restore rolls it back.
    let err = topology::restore_cluster(&dest, &target, opts(true, Some(4))).await.unwrap_err();
    assert!(err.to_string().contains("simulated crash"), "{err}");
    assert!(forgebase_backup::restore::interrupted(&target));
    assert!(forgebase_backup::restore::recover(&target).unwrap());
    assert!(!forgebase_backup::restore::interrupted(&target));
    assert_eq!(
        std::fs::read(target.root().join("postgres/nodes/beta-primary/data/mine")).unwrap(),
        b"keep me",
        "the previous data is back"
    );
    for d in ["postgres/data", "postgres/nodes/alpha-primary/data", "postgres/nodes/beta-primary/data"] {
        assert!(target.root().join(d).join("PG_VERSION").is_file(), "{d}");
    }
    // And a restore that completes after that one works.
    let ok = topology::restore_cluster(&dest, &target, opts(true, None)).await.unwrap();
    assert_eq!(ok.sets.len(), 3);
    assert!(!target.root().join("postgres/nodes/beta-primary/data/mine").exists());
    let _ = NodeState::Ready;
}
