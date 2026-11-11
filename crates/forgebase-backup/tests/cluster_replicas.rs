//! FB-179: a backup taken from a replica, and what a restore does to the
//! replicas: marked `needs_rebuild`, then rebuilt (managed). Real bundled
//! engine, a real local replica (FB-168), real `pg_basebackup` against the
//! standby's socket.
//!
//! Runs with `FORGEBASE_TEST_BACKEND=postgres` and an engine in
//! `FORGEBASE_PG_ENGINE_DIR` (or `FORGEBASE_TEST_PG_ENGINE`); skips otherwise.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::Duration;

use common::{PgInstance, engine_config, open_pg, open_pg_with, pg_engine_dir, short_tempdir};
use forgebase_backup::manifest::Method;
use forgebase_backup::topology::{
    self, ClusterBackupOptions, ClusterRestoreOptions, CompleteOptions, NodeSource, RebuildOutcome, ReplicaRebuilder,
    SetSource,
};
use forgebase_backup::{BackupError, BackupOptions, Password, PgSource, RestoreOptions, VerifyOptions};
use forgebase_core::{BackupSource, DataLayout};
use forgebase_db::topology::{self as dbtopo, DbNode, DbSet, NodeKind, NodeRole, NodeState};
use forgebase_db::{DbRouter, RouterOptions};
use forgebase_pgengine::replication::service::{self, AddLocalReplica};
use forgebase_pgengine::standby::{self, LocalReplicas};
use forgebase_pgengine::{EngineHandle, Layout, Role, RoleSecrets};
use sqlx::{Connection, PgConnection};

/// The replicator over the standby's own socket (ADR-0029 §2.9).
fn replica_source(handle: &EngineHandle, root: &Path) -> PgSource {
    let layout = Layout::primary(root);
    let secrets = RoleSecrets::load(&layout.secrets_file()).unwrap().unwrap();
    PgSource::managed(
        Some(handle.engine().root.clone()),
        handle.version().text.clone(),
        handle.socket_dir().unwrap(),
        handle.port(),
        Role::Replicator.name(),
        Password::new(secrets.password(Role::Replicator).unwrap()),
        layout.postgres_dir(),
    )
}

async fn sql(h: &EngineHandle, statement: &str) {
    let mut c = PgConnection::connect(h.url(Role::Super).expose()).await.unwrap();
    sqlx::raw_sql(sqlx::AssertSqlSafe(statement.to_owned()))
        .execute(&mut c)
        .await
        .unwrap();
    let _ = c.close().await;
}

async fn count(h: &EngineHandle) -> i64 {
    let mut c = PgConnection::connect(h.url(Role::Super).expose()).await.unwrap();
    sqlx::query_scalar("SELECT count(*) FROM forgebase.fb179_load")
        .fetch_one(&mut c)
        .await
        .unwrap()
}

async fn in_recovery(h: &EngineHandle) -> bool {
    let mut c = PgConnection::connect(h.url(Role::Super).expose()).await.unwrap();
    sqlx::query_scalar("SELECT pg_is_in_recovery()")
        .fetch_one(&mut c)
        .await
        .unwrap()
}

/// What the server's replica service does for a rebuild, and what state the
/// node was in when it was asked.
struct Rebuilder<'a> {
    pg: &'a PgInstance,
    replicas: &'a LocalReplicas,
    seen: Mutex<Vec<(String, NodeState)>>,
}

impl ReplicaRebuilder for Rebuilder<'_> {
    fn rebuild<'a>(
        &'a self,
        node: &'a DbNode,
    ) -> Pin<Box<dyn Future<Output = Result<RebuildOutcome, BackupError>> + Send + 'a>> {
        Box::pin(async move {
            self.seen.lock().unwrap().push((node.name.clone(), node.state));
            if node.kind != NodeKind::Local {
                return Ok(RebuildOutcome::Manual(format!("{} is not local", node.name)));
            }
            service::rebuild_replica(&self.pg.inst.db, self.replicas, Some(&self.pg.engine), &node.id)
                .await
                .map(|_| RebuildOutcome::Rebuilt)
                .map_err(|e| BackupError::Internal(e.to_string()))
        })
    }
}

#[tokio::test]
async fn a_replica_backup_restores_as_a_primary_and_the_replicas_are_rebuilt() {
    let Some(engine_dir) = pg_engine_dir() else { return };
    let tmp = short_tempdir();
    let root = tmp.path().join("d");
    let layout = DataLayout::new(&root);
    let cfg = engine_config(&engine_dir);

    // A primary with a replica streaming from it, and rows on both.
    let pg = open_pg(&engine_dir, &root).await;
    let tools = PgSource::tools_only(pg.src.engine_root.clone(), pg.src.version.clone());
    pg.inst.seed(25).await;
    sql(&pg.engine, "CREATE TABLE forgebase.fb179_load (id int)").await;
    sql(
        &pg.engine,
        "INSERT INTO forgebase.fb179_load SELECT g FROM generate_series(1, 10) g",
    )
    .await;
    let replicas = LocalReplicas::new(&root, &cfg);
    let added = service::add_local_replica(
        &pg.inst.db,
        &replicas,
        &pg.engine,
        &AddLocalReplica {
            set_id: "control".into(),
            name: "r1".into(),
            weight: 1,
        },
    )
    .await
    .unwrap();
    let r1 = replicas.get("r1").await.unwrap();
    standby::wait_streaming(&r1, Duration::from_secs(60)).await.unwrap();
    let lsn = standby::facts(&pg.engine).await.unwrap().lsn.unwrap();
    standby::wait_replayed(&r1, lsn, Duration::from_secs(60)).await.unwrap();
    assert_eq!(count(&r1).await, 10);

    let node_sets = |replicas: Vec<NodeSource>| {
        vec![SetSource {
            set: DbSet::control(),
            db: pg.inst.db.clone(),
            primary: NodeSource {
                name: "primary".into(),
                pg: pg.src.clone(),
                data_dir: Some("postgres/data".into()),
            },
            replicas,
        }]
    };
    let on_replica = vec![NodeSource {
        name: "r1".into(),
        pg: replica_source(&r1, &root),
        data_dir: None,
    }];

    // --- `source = replica`: copied from r1, the manifest says so.
    let dest = tmp.path().join("bk-replica");
    let m = topology::create_cluster(
        &pg.inst.db,
        &layout,
        &dest,
        ClusterBackupOptions {
            base: BackupOptions {
                include_master_key: true,
                ..BackupOptions::default()
            },
            source: BackupSource::Replica,
            sets: node_sets(on_replica.clone()),
        },
    )
    .await
    .unwrap();
    assert_eq!(m.source, BackupSource::Replica);
    let e = &m.sets[0];
    assert_eq!(
        (e.node.as_str(), e.role, e.method),
        ("r1", NodeRole::Replica, Method::Basebackup)
    );
    assert_eq!(
        e.data_dir.as_deref(),
        Some("postgres/data"),
        "restored where the primary lives"
    );
    assert!(e.timeline.is_some() && e.start_lsn.is_some() && e.end_lsn.is_some());
    assert!(e.fallback.is_none() && m.warnings.is_empty(), "{:?}", m.warnings);
    assert!(
        dest.join("control/postgres/standby.signal").is_file(),
        "the files really came from the standby"
    );
    // The replica's own history is what the backup holds: same system identifier as the primary.
    let vopts = VerifyOptions {
        pg: Some(tools.clone()),
        tmp: Some(layout.tmp_dir()),
    };
    assert!(topology::verify_cluster(&dest, &vopts).await.ok);

    // --- `source = replica` without a healthy replica: the primary is used and the manifest says why.
    let dest_fb = tmp.path().join("bk-fallback");
    let fb = topology::create_cluster(
        &pg.inst.db,
        &layout,
        &dest_fb,
        ClusterBackupOptions {
            base: BackupOptions {
                include_master_key: true,
                ..BackupOptions::default()
            },
            source: BackupSource::Replica,
            sets: node_sets(Vec::new()),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        (fb.sets[0].role, fb.sets[0].node.as_str()),
        (NodeRole::Primary, "primary")
    );
    assert!(
        fb.sets[0]
            .fallback
            .as_deref()
            .is_some_and(|f| f.contains("no healthy replica"))
    );
    assert_eq!(fb.warnings.len(), 1);
    assert!(!dest_fb.join("control/postgres/standby.signal").exists());

    // Rows written after the backup must not survive the restore.
    sql(
        &pg.engine,
        "INSERT INTO forgebase.fb179_load SELECT g FROM generate_series(11, 15) g",
    )
    .await;
    assert_eq!(count(&pg.engine).await, 15);

    // --- restore: refused while any cluster of the directory runs ...
    let restore_opts = |force: bool| ClusterRestoreOptions {
        base: RestoreOptions {
            force,
            pg: Some(tools.clone()),
            ..Default::default()
        },
        ..Default::default()
    };
    let err = topology::restore_cluster(&dest, &layout, restore_opts(true))
        .await
        .unwrap_err();
    assert!(
        matches!(err, BackupError::Conflict(ref m) if m.contains("still running")),
        "{err}"
    );
    // ... and the single-set restore still refuses a directory with replicas.
    assert!(
        forgebase_backup::restore::restore(&dest.join("control"), &layout, RestoreOptions::default())
            .await
            .is_err()
    );

    // --- ... stopped, it replaces the primary's data and moves the replica aside.
    replicas.stop_all().await;
    pg.close().await;
    let report = topology::restore_cluster(&dest, &layout, restore_opts(true))
        .await
        .unwrap();
    assert_eq!(report.sets.len(), 1);
    assert_eq!(report.moved_aside, ["postgres/nodes/r1/data"]);
    assert_eq!(report.replicas.len(), 1);
    assert_eq!(
        (report.replicas[0].node.as_str(), report.replicas[0].kind),
        ("r1", NodeKind::Local)
    );
    let prev = report.previous_data.clone().expect("the old data is kept");
    assert!(prev.join("postgres/nodes/r1/data/PG_VERSION").is_file());
    assert!(
        !root.join("postgres/nodes/r1/data").exists(),
        "the stale replica is gone"
    );
    assert!(
        !root.join("postgres/data/standby.signal").exists(),
        "a replica copy starts as a primary"
    );
    let file = dbtopo::read_topology_file(&root.join(dbtopo::TOPOLOGY_FILE))
        .unwrap()
        .unwrap();
    let r1_row = file.topology.nodes.iter().find(|n| n.name == "r1").unwrap();
    assert_eq!(r1_row.state, NodeState::NeedsRebuild, "topology.json already says so");
    assert!(topology::read_pending(&layout).unwrap().is_some());

    // --- the restored primary starts as a primary with the rows of the backup ...
    let pg2 = open_pg_with(&engine_dir, &root, false).await;
    assert!(
        !in_recovery(&pg2.engine).await,
        "restored from a standby, started as a primary"
    );
    assert_eq!(count(&pg2.engine).await, 10, "the row written after the backup is gone");
    assert_eq!(pg2.inst.count("books").await, 25);

    // --- ... and completing the restore marks the replica needs_rebuild, then rebuilds it.
    let replicas2 = LocalReplicas::new(&root, &cfg);
    let rb = Rebuilder {
        pg: &pg2,
        replicas: &replicas2,
        seen: Mutex::new(Vec::new()),
    };
    let router = DbRouter::open(pg2.inst.db.clone(), RouterOptions::default())
        .await
        .unwrap();
    let done = topology::complete_restore(
        &pg2.inst.db,
        &layout,
        CompleteOptions {
            router: Some(&router),
            backups_dir: Some(&layout.backups_dir()),
            rebuilder: Some(&rb),
        },
    )
    .await
    .unwrap();
    assert!(done.pending && done.is_complete(), "{done:?}");
    assert_eq!(done.marked, ["r1"]);
    assert_eq!(done.rebuilt, ["r1"]);
    assert_eq!(done.up_to_date, ["control"]);
    assert_eq!(
        *rb.seen.lock().unwrap(),
        [("r1".to_owned(), NodeState::NeedsRebuild)],
        "the replica was needs_rebuild when its rebuild was asked for"
    );
    assert!(topology::read_pending(&layout).unwrap().is_none());
    let node = dbtopo::load(&pg2.inst.db).await.unwrap();
    let row = node.nodes.iter().find(|n| n.id == added.id).unwrap();
    assert_ne!(row.state, NodeState::NeedsRebuild, "rebuilt: {:?}", row.state);
    // The rebuilt replica streams from the restored primary and holds its rows.
    let r1 = replicas2.get("r1").await.expect("r1 runs again");
    standby::wait_streaming(&r1, Duration::from_secs(60)).await.unwrap();
    let lsn = standby::facts(&pg2.engine).await.unwrap().lsn.unwrap();
    standby::wait_replayed(&r1, lsn, Duration::from_secs(60)).await.unwrap();
    assert!(in_recovery(&r1).await);
    assert_eq!(count(&r1).await, 10);
    // Calling it again has nothing left to do.
    assert!(
        !topology::complete_restore(&pg2.inst.db, &layout, CompleteOptions::default())
            .await
            .unwrap()
            .pending
    );

    drop(router);
    replicas2.stop_all().await;
    pg2.close().await;
}
