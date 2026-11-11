//! Managed replicas (FB-168) against a real PostgreSQL: clone with
//! `pg_basebackup -R`, stream, promote under write load without losing a
//! commit, re-follow, rejoin with `pg_rewind` (or a fresh clone when the
//! engine has none), and the topology service on control. They skip unless
//! `FORGEBASE_PG_ENGINE_DIR` names a PostgreSQL 17+ installation. Every
//! cluster lives in a temporary directory and is stopped before the test
//! ends (and by `Drop` if it panics).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use forgebase_db::topology::{self, NodeKind as TopoKind, NodeRole, NodeState};
use forgebase_db::{Database, DbOptions, DbTarget};
use forgebase_pgengine::replication::service::{
    self, AddLocalReplica, AgentState, CommandKind, Heartbeat, JoinRequest, JoinTokenRequest, PoolSwitch,
    PromoteRequest, ReplicationSource, ServiceError,
};
use forgebase_pgengine::replication::{self, EngineIdentity, Lsn};
use forgebase_pgengine::standby::{self, CloneOptions, LocalReplicas, OldPrimary, RejoinHow, SslMode, Upstream};
use forgebase_pgengine::{Engine, EngineConfig, EngineHandle, EngineSource, Layout, NodeKind, Role};
use sqlx::{Connection, PgConnection};

fn engine_dir() -> Option<PathBuf> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
    match std::env::var_os("FORGEBASE_PG_ENGINE_DIR").filter(|v| !v.is_empty()) {
        Some(d) => Some(PathBuf::from(d)),
        None => {
            eprintln!("skipped: FORGEBASE_PG_ENGINE_DIR is not set");
            None
        }
    }
}

fn tempdir() -> tempfile::TempDir {
    let base = std::env::var_os("FORGEBASE_PGENGINE_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&base).expect("test tmp base");
    tempfile::Builder::new().prefix("pr").tempdir_in(base).expect("tempdir")
}

fn config(dir: &Path, port: u16) -> EngineConfig {
    let mut cfg = EngineConfig {
        source: EngineSource::Dir(dir.to_path_buf()),
        socket_port: port,
        unsafe_fast: true,
        ready_timeout: Duration::from_secs(60),
        stop_timeout: Duration::from_secs(30),
        ..EngineConfig::default()
    };
    // Acknowledged means flushed on the primary, even in the fast test mode.
    cfg.settings.insert("synchronous_commit".into(), "on".into());
    cfg
}

async fn conn(h: &EngineHandle, role: Role) -> PgConnection {
    PgConnection::connect(h.url(role).expose())
        .await
        .unwrap_or_else(|e| panic!("connect as {}: {e}", role.name()))
}

async fn count(h: &EngineHandle) -> i64 {
    let mut c = conn(h, Role::Super).await;
    sqlx::query_scalar("SELECT count(*) FROM forgebase.fb168_load")
        .fetch_one(&mut c)
        .await
        .expect("count")
}

async fn current_lsn(h: &EngineHandle) -> Lsn {
    standby::facts(h).await.expect("facts").lsn.expect("lsn")
}

fn has_rewind(engine: &Path) -> bool {
    engine.join("bin").join("pg_rewind").is_file()
}

/// Inserts rows one transaction at a time until told to stop or a write
/// fails (the fence); returns every id whose COMMIT was acknowledged.
fn writer(h: &EngineHandle, stop: Arc<AtomicBool>) -> tokio::task::JoinHandle<Vec<i64>> {
    let url = h.url(Role::Server);
    tokio::spawn(async move {
        let mut acked = Vec::new();
        let Ok(mut c) = PgConnection::connect(url.expose()).await else {
            return acked;
        };
        let mut id = 0i64;
        while !stop.load(Ordering::SeqCst) {
            id += 1;
            let r = sqlx::query("INSERT INTO forgebase.fb168_load (id, v) VALUES ($1, repeat('x', 200))")
                .bind(id)
                .execute(&mut c)
                .await;
            match r {
                Ok(_) => acked.push(id),
                Err(_) => break,
            }
        }
        acked
    })
}

#[tokio::test]
async fn local_replica_is_promoted_under_write_load_without_losing_a_commit() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let cfg = config(&engine, 18_681);
    let primary = Engine::prepare(&Layout::primary(tmp.path()), &cfg).await.expect("primary");
    {
        let mut c = conn(&primary, Role::Server).await;
        sqlx::query("CREATE TABLE forgebase.fb168_load (id bigint PRIMARY KEY, v text NOT NULL)")
            .execute(&mut c)
            .await
            .expect("table");
        let repl: bool = sqlx::query_scalar("SELECT rolreplication FROM pg_roles WHERE rolname = 'forgebase_replicator'")
            .fetch_one(&mut c)
            .await
            .expect("replicator");
        assert!(repl, "forgebase_replicator has REPLICATION");
        let keep: String = sqlx::query_scalar("SHOW max_slot_wal_keep_size")
            .fetch_one(&mut c)
            .await
            .expect("setting");
        assert_eq!(keep, "8GB");
    }

    let replicas = LocalReplicas::new(tmp.path(), &cfg);
    let r1 = replicas.add(&primary, "r1").await.expect("r1");
    let r2 = replicas.add(&primary, "r2").await.expect("r2");
    for r in [&r1, &r2] {
        standby::wait_streaming(r, Duration::from_secs(30)).await.expect("streaming");
    }
    // The follow settings carry no password; the slots exist and are used.
    let auto = std::fs::read_to_string(replicas.layout("r1").unwrap().pgdata().join("postgresql.auto.conf")).unwrap();
    assert!(auto.contains("primary_slot_name = 'fbr_r1'"), "{auto}");
    assert!(!auto.contains("password"), "{auto}");
    {
        let mut c = conn(&primary, Role::Super).await;
        let slots = replication::slots(&mut c).await.expect("slots");
        let names: Vec<&str> = slots.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["fbr_r1", "fbr_r2"]);
        assert!(slots.iter().all(|s| s.active && !s.needs_rebuild()), "{slots:?}");
        let senders = replication::senders(&mut c).await.expect("senders");
        assert!(senders.iter().any(|s| s.application_name == "r1"), "{senders:?}");
    }

    // Write load, then promote r1 while the writer is still writing.
    let stop = Arc::new(AtomicBool::new(false));
    let load = writer(&primary, stop.clone());
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let report = standby::promote_local(&replicas, "r1", OldPrimary::Running(&primary), false, Duration::from_secs(60))
        .await
        .expect("promote");
    stop.store(true, Ordering::SeqCst);
    let acked = load.await.expect("writer");
    assert!(acked.len() > 20, "the writer committed under load: {}", acked.len());
    assert!(report.clean, "{report:?}");
    assert!(report.replayed > report.old_primary_end, "{report:?}");
    assert_eq!(report.refollowed, ["r2"]);
    assert!(report.timeline.unwrap_or(0) >= 2, "{report:?}");

    // No acknowledged commit is lost.
    let new = replicas.take("r1").await.expect("promoted handle");
    {
        let mut c = conn(&new, Role::Super).await;
        let present: i64 = sqlx::query_scalar("SELECT count(*) FROM forgebase.fb168_load WHERE id = ANY($1)")
            .bind(&acked)
            .fetch_one(&mut c)
            .await
            .expect("present");
        assert_eq!(present, i64::try_from(acked.len()).unwrap(), "every acknowledged commit survived");
        let f = replication::facts(&mut c).await.expect("facts");
        assert!(!f.in_recovery);
    }
    // The new primary takes writes and r2 follows it on the new timeline.
    {
        let mut c = conn(&new, Role::Server).await;
        sqlx::query("INSERT INTO forgebase.fb168_load (id, v) VALUES (-1, 'after promote')")
            .execute(&mut c)
            .await
            .expect("write on the new primary");
    }
    let r2 = replicas.get("r2").await.expect("r2");
    standby::wait_replayed(&r2, current_lsn(&new).await, Duration::from_secs(30))
        .await
        .expect("r2 follows the new primary");
    assert_eq!(count(&r2).await, count(&new).await);

    // The old primary rejoins as a standby: pg_rewind when the engine has
    // it, a fresh clone otherwise (the bundled engine ships none yet).
    let old_layout = Layout::primary(tmp.path()).with_kind(NodeKind::Standby);
    let up = Upstream::local(&new, standby::OLD_PRIMARY).expect("upstream");
    {
        let mut c = conn(&new, Role::Super).await;
        replication::ensure_slot(&mut c, &up.slot).await.expect("slot");
    }
    let how = standby::rejoin(
        new.engine(),
        &old_layout,
        &up,
        Some(&up.clone().as_super(&new)),
        CloneOptions { no_sync: true },
    )
    .await
    .expect("rejoin");
    let expected = if has_rewind(&engine) { RejoinHow::Rewound } else { RejoinHow::Recloned };
    assert_eq!(how, expected);
    let old = Engine::prepare(&old_layout, &replicas.config().clone()).await.expect("old primary as standby");
    standby::wait_streaming(&old, Duration::from_secs(30)).await.expect("old primary streams");
    standby::wait_replayed(&old, current_lsn(&new).await, Duration::from_secs(30))
        .await
        .expect("old primary catches up");
    assert_eq!(count(&old).await, count(&new).await);
    assert!(standby::facts(&old).await.expect("facts").in_recovery);

    old.stop().await.expect("stop");
    replicas.stop_all().await;
    new.stop().await.expect("stop");
    primary.stop().await.expect("stop");
}

#[tokio::test]
async fn unclean_stop_needs_accept_data_loss() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let cfg = config(&engine, 18_682);
    let primary = Engine::prepare(&Layout::primary(tmp.path()), &cfg).await.expect("primary");
    let replicas = LocalReplicas::new(tmp.path(), &cfg);
    let r1 = replicas.add(&primary, "r1").await.expect("r1");
    standby::wait_streaming(&r1, Duration::from_secs(30)).await.expect("streaming");
    primary.stop_immediate().await.expect("crash-like stop");
    let layout = Layout::primary(tmp.path());
    let err = standby::promote_local(
        &replicas,
        "r1",
        OldPrimary::Stopped {
            layout: &layout,
            engine: primary.engine(),
        },
        false,
        Duration::from_secs(10),
    )
    .await
    .expect_err("refused");
    assert!(err.to_string().contains("accept_data_loss"), "{err}");
    assert!(standby::facts(&r1).await.expect("facts").in_recovery, "still a standby");
    let report = standby::promote_local(
        &replicas,
        "r1",
        OldPrimary::Stopped {
            layout: &layout,
            engine: primary.engine(),
        },
        true,
        Duration::from_secs(10),
    )
    .await
    .expect("promote with accept_data_loss");
    assert!(!report.clean);
    replicas.stop_all().await;
}

/// The server's side of a promote in a test: the writer pool is the test's,
/// so `fence` does nothing; `repoint` opens control on the new primary.
struct TestPools;

impl PoolSwitch for TestPools {
    fn fence<'a>(&'a self, _set_id: &'a str) -> forgebase_db::BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn repoint<'a>(
        &'a self,
        _set_id: &'a str,
        _new_primary: &'a topology::DbNode,
        handle: Arc<EngineHandle>,
    ) -> forgebase_db::BoxFuture<'a, service::ServiceResult<Database>> {
        Box::pin(async move { Ok(open_db(&handle).await) })
    }
}

async fn open_db(h: &EngineHandle) -> Database {
    Database::open(DbOptions {
        target: DbTarget::postgres(forgebase_db::SecretString::new(h.url(Role::Server).expose()), true),
        synchronous: forgebase_core::Synchronous::Full,
        busy_timeout: Duration::from_secs(5),
        read_pool_size: 2,
        create_if_missing: false,
        read_only: false,
    })
    .await
    .expect("open control")
}

#[tokio::test]
async fn service_adds_promotes_and_rejoins_local_replicas() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let cfg = config(&engine, 18_683);
    let primary = Engine::prepare(&Layout::primary(tmp.path()), &cfg).await.expect("primary");
    let control = open_db(&primary).await;
    control.migrate(None).await.expect("migrate");
    let replicas = LocalReplicas::new(tmp.path(), &cfg);

    let r1 = service::add_local_replica(
        &control,
        &replicas,
        &primary,
        &AddLocalReplica {
            set_id: "control".into(),
            name: "r1".into(),
            weight: 1,
        },
    )
    .await
    .expect("add r1");
    assert_eq!(r1.kind, TopoKind::Local);
    assert_eq!(r1.state, NodeState::CatchingUp);
    assert_eq!(r1.engine_version, Some(EngineIdentity::of(primary.engine()).label()));
    assert!(r1.system_identifier.is_some());
    let dup = service::add_local_replica(
        &control,
        &replicas,
        &primary,
        &AddLocalReplica {
            set_id: "control".into(),
            name: "r1".into(),
            weight: 1,
        },
    )
    .await
    .expect_err("duplicate");
    assert_eq!(dup.code(), "node_exists");
    let json = serde_json::to_string(&service::nodes(&control, None).await.expect("nodes")).expect("json");
    assert!(!json.contains("url_sealed"), "{json}");

    let wrong = service::promote_local(
        &control,
        &replicas,
        OldPrimary::Running(&primary),
        &r1.id,
        &PromoteRequest {
            confirm: "R1".into(),
            accept_data_loss: false,
        },
        &TestPools,
        None,
    )
    .await
    .expect_err("confirmation");
    assert!(matches!(wrong, ServiceError::ConfirmMismatch(_)), "{wrong}");

    let check = service::promote_check(&replicas, Some(&primary), "r1").await.expect("check");
    assert!(check.primary_reachable && check.replayed.is_some());

    let topo_file = tmp.path().join("topology.json");
    control.close().await;
    let (out, new) = service::promote_local(
        &open_db(&primary).await,
        &replicas,
        OldPrimary::Running(&primary),
        &r1.id,
        &PromoteRequest {
            confirm: "r1".into(),
            accept_data_loss: false,
        },
        &TestPools,
        Some(&topo_file),
    )
    .await
    .expect("promote");
    assert!(out.report.clean);
    let control = open_db(&new).await;
    let topo = topology::load(&control).await.expect("topology");
    assert!(topo.epoch >= out.epoch);
    let p = topo.primary_of("control").expect("primary row");
    assert_eq!((p.name.as_str(), p.state), ("r1", NodeState::Ready));
    let old = topo.nodes.iter().find(|n| n.id == out.old_primary).expect("old primary row");
    assert_eq!((old.name.as_str(), old.role, old.state), ("primary", NodeRole::Replica, NodeState::NeedsRebuild));
    let file = topology::read_topology_file(&topo_file).expect("read").expect("file");
    assert_eq!(file.topology.epoch, topo.epoch);

    let (view, how) = service::rejoin_local(&control, &replicas, &new, &out.old_primary)
        .await
        .expect("rejoin");
    assert_eq!(view.state, NodeState::CatchingUp);
    let expected = if has_rewind(&engine) { RejoinHow::Rewound } else { RejoinHow::Recloned };
    assert_eq!(how, expected);
    let rejoined = replicas.get("primary").await.expect("running");
    standby::wait_streaming(&rejoined, Duration::from_secs(30)).await.expect("streams");

    // Slot health: nothing lost yet.
    assert!(service::mark_lost_slots(&control, "control", &new).await.expect("slots").is_empty());

    service::remove_replica(&control, &replicas, Some(&new), &out.old_primary, "primary")
        .await
        .expect("remove");
    assert!(!tmp.path().join("postgres/data").exists() || replicas.get("primary").await.is_none());
    let names: Vec<String> = service::nodes(&control, None)
        .await
        .expect("nodes")
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert_eq!(names, ["r1"]);
    control.close().await;
    replicas.stop_all().await;
    new.stop().await.expect("stop");
}

#[tokio::test]
async fn join_tokens_agents_commands_and_heartbeats() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let mut cfg = config(&engine, 18_684);
    cfg.listen = Some("127.0.0.1:18684".parse().unwrap());
    let primary = Engine::prepare(&Layout::primary(tmp.path()), &cfg).await.expect("primary");
    let control = open_db(&primary).await;
    control.migrate(None).await.expect("migrate");
    let source = ReplicationSource::from_handle(&primary, None, SslMode::Disable).expect("source");

    let tok = service::issue_join_token(
        &control,
        &JoinTokenRequest {
            set_id: "control".into(),
            role: NodeRole::Replica,
        },
        Some("admin-1"),
        "http://127.0.0.1:8090/",
    )
    .await
    .expect("token");
    assert!(tok.command.starts_with("forgebase node join http://127.0.0.1:8090 --token fbj_"), "{}", tok.command);
    let mut req = JoinRequest {
        token: tok.token.clone(),
        name: "far-1".into(),
        engine: EngineIdentity {
            minor: source.engine.minor + 1,
            version: format!("{}.{}", source.engine.major, source.engine.minor + 1),
            ..source.engine.clone()
        },
        listen: Some("127.0.0.1:18685".into()),
    };
    let e = service::redeem_join(&control, &source, &req, None).await.expect_err("engine mismatch");
    assert_eq!(e.code(), "engine_mismatch");
    req.engine = source.engine.clone();
    let grant = service::redeem_join(&control, &source, &req, Some("127.0.0.1".parse().unwrap()))
        .await
        .expect("join after fixing the engine (token kept)");
    assert_eq!(grant.upstream.slot, "fbr_far_1");
    assert_eq!(grant.upstream.user, "forgebase_replicator");
    let again = service::redeem_join(&control, &source, &req, None).await.expect_err("one time");
    assert_eq!(again.code(), "join_token_invalid");

    // The agent clones over TCP with what the grant says.
    let layout = Layout::node(tmp.path().join("agent"), "far-1", NodeKind::Standby).unwrap();
    standby::clone_from(primary.engine(), &layout, &grant.upstream.upstream("far-1"), CloneOptions { no_sync: true })
        .await
        .expect("clone over TCP");
    assert!(layout.pgdata().join("standby.signal").is_file());

    // Commands and heartbeats.
    let bad = service::poll_commands(&control, &grant.node_id, "fba_wrong", Duration::ZERO, false)
        .await
        .expect_err("bad key");
    assert_eq!(bad.status(), 401);
    let empty = service::poll_commands(&control, &grant.node_id, &grant.agent_key, Duration::from_millis(300), false)
        .await
        .expect("poll");
    assert!(empty.commands.is_empty());
    let cmd = service::queue_command(&control, &grant.node_id, CommandKind::Follow, serde_json::json!({"x": 1}), None)
        .await
        .expect("queue");
    let got = service::poll_commands(&control, &grant.node_id, &grant.agent_key, Duration::from_secs(5), false)
        .await
        .expect("poll");
    assert_eq!(got.commands.len(), 1);
    assert_eq!(got.commands[0].id, cmd.id);
    let reply = service::heartbeat(
        &control,
        &grant.node_id,
        &grant.agent_key,
        &Heartbeat {
            state: AgentState::Streaming,
            replay_lsn: Some(Lsn(42)),
            disk_free: Some(1 << 30),
            engine: source.engine.clone(),
            detail: None,
            results: vec![service::CommandResult {
                id: cmd.id.clone(),
                ok: true,
                message: "followed".into(),
            }],
        },
    )
    .await
    .expect("heartbeat");
    assert_eq!(reply.node_state, NodeState::CatchingUp);
    assert!(!reply.commands_pending);
    let done = service::wait_command(&control, &cmd.id, Duration::from_secs(2)).await.expect("done");
    assert_eq!((done.state.as_str(), done.result.as_deref()), ("done", Some("followed")));
    let v = service::node_view(&control, &grant.node_id).await.expect("view");
    let agent = v.agent.expect("agent");
    assert_eq!(agent.state, AgentState::Streaming);
    assert_eq!(agent.replay_lsn, Some(Lsn(42)));
    let (addr, listen) = service::agent_addresses(&control, &grant.node_id).await.expect("addresses");
    assert_eq!((addr.as_deref(), listen.as_deref()), (Some("127.0.0.1"), Some("127.0.0.1:18685")));

    control.close().await;
    primary.stop().await.expect("stop");
}
