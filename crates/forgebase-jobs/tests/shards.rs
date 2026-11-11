//! Jobs on database sets (FB-172, ADR-0029 §3.5).
//!
//! - one set (SQLite and PostgreSQL): a router changes nothing;
//! - several sets (PostgreSQL, separate clusters from
//!   `forgebase_db::shard_testing`): jobs, schedules, workflows and queue
//!   settings land on the tenant's set; lookups by id find them from any
//!   caller; unbound claims go round-robin; workers stay on control; the
//!   pool runs one claim loop per set; maintenance and claims skip tenants
//!   that move;
//! - pg_cron per set (managed engines with pg_cron): each set's schedules
//!   fire on that set, and a moving tenant's leave pg_cron.
//!
//! The multi-set tests skip without a PostgreSQL engine.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{claim_req, harness, system, wait_for};
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::shard_testing::Sharded;
use forgebase_db::topology::PlacementState;
use forgebase_db::{Backend, Database, DbRouter, RouterOptions, Statement, TxMode, Value, shards};
use forgebase_jobs::{
    JobRegistry, JobService, JobStatus, ManualClock, NewJob, NewSchedule, PoolConfig, QueueSettings, start_pool,
};
use serde_json::json;

fn as_tenant(t: &str) -> AccessContext {
    AccessContext::admin("ops").with_tenant(t)
}

async fn count(db: &Database, sql: &str) -> i64 {
    db.fetch_all(&Statement::raw(sql))
        .await
        .unwrap()
        .rows
        .first()
        .and_then(|r| r.first())
        .and_then(Value::as_i64)
        .unwrap_or(-1)
}

async fn exec(db: &Database, sql: &str) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::raw(sql)).await.unwrap();
    tx.commit().await.unwrap();
}

fn job(kind: &str) -> NewJob {
    NewJob::new(kind, json!({"n": 1}))
}

#[tokio::test]
async fn one_set_with_a_router_behaves_as_before() {
    let h = harness().await;
    h.svc.attach_router(DbRouter::single(h.db.clone()));
    let (a, created) = h.svc.enqueue(&as_tenant("ta"), job("work")).await.unwrap();
    assert!(created);
    let (b, _) = h.svc.enqueue(&system(), job("work")).await.unwrap();
    assert_eq!(h.svc.get(&system(), &a.id).await.unwrap().tenant.as_deref(), Some("ta"));
    assert_eq!(h.svc.list(&system(), Default::default()).await.unwrap().total, 2);
    assert_eq!(h.svc.stats(&system()).await.unwrap().totals["queued"], 2);
    let claimed = h.svc.claim(&system(), claim_req("w1", 5)).await.unwrap();
    assert_eq!(claimed.len(), 2);
    for c in &claimed {
        h.svc.complete(&system(), &c.job.id, &c.lease_id, None).await.unwrap();
    }
    assert_eq!(h.svc.get(&system(), &b.id).await.unwrap().status, JobStatus::Succeeded);
    assert_eq!(h.svc.workers(&system()).await.unwrap().len(), 1);

    // Schedules fire as before.
    let s = h
        .svc
        .create_schedule(
            &as_tenant("ta"),
            NewSchedule {
                name: "every-minute".into(),
                cron: "* * * * *".into(),
                kind: "work".into(),
                payload: json!({}),
                ..NewSchedule::default()
            },
        )
        .await
        .unwrap();
    h.clock.advance(Duration::from_secs(61));
    assert_eq!(h.svc.fire_due_schedules(&system()).await.unwrap(), 1);
    assert_eq!(h.svc.schedule(&system(), &s.id).await.unwrap().tenant.as_deref(), Some("ta"));

    // The pool over a one-set router runs jobs.
    let registry =
        JobRegistry::new().register("ping", |_ctx: forgebase_jobs::JobContext| async { Ok(json!("pong")) });
    let pool = start_pool(
        h.svc.clone(),
        registry,
        PoolConfig {
            poll_interval: Duration::from_millis(20),
            maintenance_interval: Duration::from_secs(3600),
            ..PoolConfig::default()
        },
    );
    let (p, _) = h.svc.enqueue(&system(), job("ping")).await.unwrap();
    let svc = h.svc.clone();
    wait_for(Duration::from_secs(20), || {
        let (svc, id) = (svc.clone(), p.id.clone());
        async move { (svc.get(&system(), &id).await.unwrap().status == JobStatus::Succeeded).then_some(()) }
    })
    .await;
    pool.shutdown().await;
}

/// Control plus one shard `s1`, with a router whose freeze wait is short.
async fn two_sets() -> Option<(Sharded, Database, String, DbRouter)> {
    let mut sh = Sharded::start().await?;
    let (set, db) = sh.provision("s1").await;
    shards::activate_shard(
        &sh.router,
        &set,
        &db,
        shards::ShardSetup {
            backups_dir: None,
            replay: None,
            control_conninfo: None,
        },
    )
    .await
    .unwrap();
    let router = DbRouter::open(
        sh.control.clone(),
        RouterOptions {
            connector: Some(sh.connector.clone()),
            freeze_timeout: Duration::from_millis(300),
            ..RouterOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(router.is_sharded());
    Some((sh, db, set.id, router))
}

#[tokio::test]
async fn jobs_live_on_the_tenant_set() {
    let Some((sh, s1, s1_id, router)) = two_sets().await else {
        eprintln!("skipped: no PostgreSQL engine");
        return;
    };
    let control = sh.control.clone();
    router.place("tb", &s1_id, PlacementState::Active).await.unwrap();
    let data = DataService::new(Arc::new(control.clone()), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    let clock = ManualClock::from_system();
    let svc = JobService::with_clock(data, Arc::new(clock.clone()));
    svc.attach_router(router.clone());

    // Enqueue: the tenant's set (ta has no placement: control).
    let (a, _) = svc.enqueue(&as_tenant("ta"), job("work")).await.unwrap();
    let (b, _) = svc.enqueue(&as_tenant("tb"), job("work")).await.unwrap();
    let (i, _) = svc.enqueue(&system(), job("work")).await.unwrap();
    let on = |db: &Database, id: &str| {
        let (db, sql) = (db.clone(), format!("SELECT count(*) FROM _forgebase_jobs WHERE id = '{id}'"));
        async move { count(&db, &sql).await }
    };
    assert_eq!((on(&control, &a.id).await, on(&s1, &a.id).await), (1, 0));
    assert_eq!((on(&control, &b.id).await, on(&s1, &b.id).await), (0, 1));
    assert_eq!((on(&control, &i.id).await, on(&s1, &i.id).await), (1, 0));

    // Lookup by id from any caller, on a fresh service (nothing remembered).
    let data2 = DataService::new(Arc::new(control.clone()), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    let fresh = JobService::with_clock(data2, Arc::new(clock.clone()));
    fresh.attach_router(router.clone());
    assert_eq!(fresh.get(&system(), &b.id).await.unwrap().tenant.as_deref(), Some("tb"));
    assert_eq!(fresh.get(&as_tenant("ta"), &b.id).await.unwrap().id, b.id);
    assert_eq!(fresh.events(&system(), &b.id).await.unwrap()[0].event, "enqueued");

    // Lists are per set; stats without a tenant add up every set.
    assert_eq!(svc.list(&as_tenant("tb"), Default::default()).await.unwrap().total, 1);
    assert_eq!(svc.list(&system(), Default::default()).await.unwrap().total, 2);
    assert_eq!(svc.stats(&system()).await.unwrap().totals["queued"], 3);
    assert_eq!(svc.stats(&as_tenant("tb")).await.unwrap().totals["queued"], 1);

    // Unbound claims go round-robin over the sets; the worker stays on control.
    let first = svc.claim(&system(), claim_req("w1", 1)).await.unwrap();
    let second = svc.claim(&system(), claim_req("w1", 1)).await.unwrap();
    let ids: Vec<&str> = first.iter().chain(&second).map(|c| c.job.id.as_str()).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&b.id.as_str()), "{ids:?}: the shard's job is claimed within two rounds");
    let rest = svc.claim(&system(), claim_req("w1", 10)).await.unwrap();
    assert_eq!(rest.len(), 1);
    for c in first.iter().chain(&second).chain(&rest) {
        fresh.heartbeat(&system(), &c.job.id, &c.lease_id).await.unwrap();
        fresh.complete(&system(), &c.job.id, &c.lease_id, None).await.unwrap();
    }
    assert_eq!(svc.get(&system(), &b.id).await.unwrap().status, JobStatus::Succeeded);
    assert_eq!(count(&control, "SELECT count(*) FROM _forgebase_job_workers").await, 1);
    assert_eq!(count(&s1, "SELECT count(*) FROM _forgebase_job_workers").await, 0);

    // A tenant-bound worker claims on its tenant's set only.
    let (b2, _) = svc.enqueue(&as_tenant("tb"), job("work")).await.unwrap();
    let mine = svc.claim(&as_tenant("tb"), claim_req("w2", 10)).await.unwrap();
    assert_eq!(mine.iter().map(|c| c.job.id.clone()).collect::<Vec<_>>(), vec![b2.id.clone()]);
    svc.release(&as_tenant("tb"), &b2.id, &mine[0].lease_id).await.unwrap();

    // Queue settings: on the tenant's set, mirrored to control.
    svc.pause_queue(&as_tenant("tb"), "default").await.unwrap();
    let paused = "SELECT count(*) FROM _forgebase_job_queues WHERE name = 'tb/default' AND paused = 1";
    assert_eq!((count(&s1, paused).await, count(&control, paused).await), (1, 1));
    assert!(svc.claim(&system(), claim_req("w1", 10)).await.unwrap().is_empty());
    svc.resume_queue(&as_tenant("tb"), "default").await.unwrap();
    assert_eq!(count(&control, paused).await, 0);
    let back = svc.claim(&system(), claim_req("w1", 10)).await.unwrap();
    assert_eq!(back.len(), 1);
    svc.complete(&system(), &b2.id, &back[0].lease_id, None).await.unwrap();

    // Schedules: stored and fired on the tenant's set.
    let s = svc
        .create_schedule(
            &as_tenant("tb"),
            NewSchedule {
                name: "tick".into(),
                cron: "* * * * *".into(),
                kind: "work".into(),
                payload: json!({}),
                ..NewSchedule::default()
            },
        )
        .await
        .unwrap();
    let sched = format!("SELECT count(*) FROM _forgebase_job_schedules WHERE id = '{}'", s.id);
    assert_eq!((count(&s1, &sched).await, count(&control, &sched).await), (1, 0));
    assert_eq!(fresh.schedule(&system(), &s.id).await.unwrap().name, "tick");
    clock.advance(Duration::from_secs(61));
    assert_eq!(svc.fire_due_schedules(&system()).await.unwrap(), 1);
    let fired = format!("SELECT count(*) FROM _forgebase_jobs WHERE schedule_id = '{}'", s.id);
    assert_eq!(count(&s1, &fired).await, 1);
    svc.delete_schedule(&system(), &s.id).await.unwrap();
    let pending = svc.claim(&system(), claim_req("w1", 10)).await.unwrap();
    for c in &pending {
        svc.complete(&system(), &c.job.id, &c.lease_id, None).await.unwrap();
    }

    // A tenant moving away from s1 (its placement names control while its
    // rows are a copy on s1): s1 neither claims, reaps nor fires for it.
    router.place("tc", &s1_id, PlacementState::Active).await.unwrap();
    let (c1, _) = svc.enqueue(&as_tenant("tc"), job("work")).await.unwrap();
    let sc = svc
        .create_schedule(
            &as_tenant("tc"),
            NewSchedule {
                name: "tc-tick".into(),
                cron: "* * * * *".into(),
                kind: "work".into(),
                payload: json!({}),
                ..NewSchedule::default()
            },
        )
        .await
        .unwrap();
    router.place("tc", "control", PlacementState::Moving).await.unwrap();
    assert_eq!(forgebase_jobs::excluded_tenants(&router, &s1_id), vec!["tc".to_owned()]);
    assert!(forgebase_jobs::excluded_tenants(&router, "control").is_empty());
    let s1_view = svc.on_set(router.set(&s1_id).unwrap());
    assert!(s1_view.claim(&system(), claim_req("w1", 10)).await.unwrap().is_empty());
    clock.advance(Duration::from_secs(61));
    assert_eq!(s1_view.fire_due_schedules(&system()).await.unwrap(), 0);
    let tc_fired = format!("SELECT count(*) FROM _forgebase_jobs WHERE schedule_id = '{}'", sc.id);
    assert_eq!(count(&s1, &tc_fired).await, 0);
    assert_eq!(
        count(&s1, &format!("SELECT count(*) FROM _forgebase_jobs WHERE id = '{}' AND status = 'queued'", c1.id)).await,
        1
    );

    // A frozen tenant: calls wait for the freeze, then 409 tenant_moving.
    router.place("tb", &s1_id, PlacementState::Frozen).await.unwrap();
    let err = svc.enqueue(&as_tenant("tb"), job("work")).await.unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&err), "tenant_moving");
    assert_eq!(
        forgebase_core::ErrorCode::kind(&err),
        forgebase_core::ErrorKind::Conflict
    );
    router.place("tb", &s1_id, PlacementState::Active).await.unwrap();

    // The pool: one claim loop per set; the moving tenant's job waits.
    let registry =
        JobRegistry::new().register("ping", |_ctx: forgebase_jobs::JobContext| async { Ok(json!("pong")) });
    let pool = start_pool(
        svc.clone(),
        registry,
        PoolConfig {
            poll_interval: Duration::from_millis(20),
            maintenance_interval: Duration::from_secs(3600),
            ..PoolConfig::default()
        },
    );
    let (pa, _) = svc.enqueue(&as_tenant("ta"), job("ping")).await.unwrap();
    let (pb, _) = svc.enqueue(&as_tenant("tb"), job("ping")).await.unwrap();
    for id in [pa.id.clone(), pb.id.clone()] {
        let svc = svc.clone();
        wait_for(Duration::from_secs(30), move || {
            let (svc, id) = (svc.clone(), id.clone());
            async move { (svc.get(&system(), &id).await.unwrap().status == JobStatus::Succeeded).then_some(()) }
        })
        .await;
    }
    assert!(svc.get(&system(), &pb.id).await.unwrap().worker_id.is_some());
    pool.shutdown().await;
    assert_eq!(svc.get(&system(), &c1.id).await.unwrap().status, JobStatus::Queued);

    // Workflows: on the tenant's set, found by id.
    let wf = svc
        .create_workflow(
            &as_tenant("tb"),
            forgebase_jobs::NewWorkflow::new("flow", vec![forgebase_jobs::NewStep::new("one", "work", json!({}))]),
        )
        .await
        .unwrap();
    let wf_sql = format!("SELECT count(*) FROM _forgebase_job_workflows WHERE id = '{}'", wf.workflow.id);
    assert_eq!((count(&s1, &wf_sql).await, count(&control, &wf_sql).await), (1, 0));
    assert_eq!(fresh.workflow(&system(), &wf.workflow.id).await.unwrap().steps.len(), 1);

    // copy_queue_settings: control's copy of a tenant's settings reaches its set.
    svc.set_queue(
        &as_tenant("tb"),
        "bulk",
        QueueSettings {
            concurrency: Some(2),
            ..QueueSettings::default()
        },
    )
    .await
    .unwrap();
    exec(&s1, "DELETE FROM _forgebase_job_queues WHERE name = 'tb/bulk'").await;
    assert_eq!(svc.copy_queue_settings("tb").await.unwrap(), 2);
    assert_eq!(
        count(&s1, "SELECT count(*) FROM _forgebase_job_queues WHERE name = 'tb/bulk' AND concurrency = 2").await,
        1
    );
    drop(sh);
}

/// A managed engine with pg_cron preloaded, migrated and set up as the
/// server does it; `None` without such an engine.
async fn cron_node(port: u16) -> Option<(tempfile::TempDir, forgebase_pgengine::EngineHandle, Database)> {
    use forgebase_core::config::PostgresExtensions;
    use forgebase_db::{DbOptions, DbTarget, ExtensionSetup, Feature, SecretString};
    use forgebase_pgengine::{Engine, EngineConfig, EngineSource, Layout, Role};
    let engine = std::env::var_os("FORGEBASE_PG_ENGINE_DIR").filter(|v| !v.is_empty())?;
    let base = std::env::var_os("FORGEBASE_PGENGINE_TEST_TMP")
        .or_else(|| std::env::var_os("FORGEBASE_TEST_PG_TMP"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new().prefix("pc").tempdir_in(base).unwrap();
    let handle = Engine::prepare(
        &Layout::primary(dir.path()),
        &EngineConfig {
            source: EngineSource::Dir(engine.into()),
            socket_port: port,
            unsafe_fast: true,
            ready_timeout: Duration::from_secs(60),
            stop_timeout: Duration::from_secs(20),
            ..EngineConfig::default()
        },
    )
    .await
    .expect("prepare");
    if !handle.has_cron() {
        eprintln!("skipped: the engine has no pg_cron");
        return None;
    }
    let db = Database::open(DbOptions {
        target: DbTarget::postgres(SecretString::new(handle.url(Role::Server).expose()), true),
        synchronous: forgebase_core::Synchronous::Full,
        busy_timeout: Duration::from_secs(5),
        read_pool_size: 2,
        create_if_missing: false,
        read_only: false,
    })
    .await
    .unwrap();
    db.migrate(None).await.unwrap();
    let report = db
        .setup_extensions(&ExtensionSetup {
            modes: PostgresExtensions::default(),
            admin_url: Some(SecretString::new(handle.url(Role::Super).expose())),
            reader_role: Some(Role::Reader.name().to_owned()),
        })
        .await
        .unwrap();
    assert!(report.is_active(Feature::Cron), "{report:?}");
    Some((dir, handle, db))
}

#[tokio::test]
async fn pg_cron_fires_each_set_on_its_own_node() {
    use forgebase_db::shard_testing::MapConnector;
    use forgebase_db::topology::{self, NewNode, NewSet, NodeKind, NodeOrigin, NodeRole, NodeState, SetState};
    let Some((_da, _ha, control)) = cron_node(55871).await else {
        eprintln!("skipped: no engine with pg_cron");
        return;
    };
    let (_db_dir, _hb, s1) = cron_node(55872).await.expect("second engine");
    let set = topology::insert_set(
        &control,
        &NewSet {
            name: "s1".into(),
            weight: 1,
        },
    )
    .await
    .unwrap();
    topology::insert_node(
        &control,
        &NewNode {
            set_id: set.id.clone(),
            name: "s1-primary".into(),
            kind: NodeKind::External,
            role: NodeRole::Primary,
            url_sealed: Some("sealed".into()),
            state: NodeState::Ready,
            weight: 1,
            origin: NodeOrigin::Api,
        },
    )
    .await
    .unwrap();
    topology::apply_sequence_range(&s1, set.number).await.unwrap();
    topology::update_set(&control, &set.id, Some(SetState::Active), None, None)
        .await
        .unwrap();
    let connector = Arc::new(MapConnector::default());
    connector.insert("s1-primary", s1.clone());
    let router = DbRouter::open(
        control.clone(),
        RouterOptions {
            connector: Some(connector),
            ..RouterOptions::default()
        },
    )
    .await
    .unwrap();
    router.place("tb", &set.id, PlacementState::Active).await.unwrap();
    let data = DataService::new(Arc::new(control.clone()), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    let svc = JobService::new(data);
    svc.attach_router(router.clone());
    let handle = forgebase_jobs::attach_pgcron_sets(&svc, &control)
        .await
        .expect("pg_cron on both sets");
    let s = svc
        .create_schedule(
            &as_tenant("tb"),
            NewSchedule {
                name: "fast".into(),
                cron: "*/2 * * * * *".into(),
                kind: "work".into(),
                payload: json!({}),
                ..NewSchedule::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(s.fired_by, "pg_cron", "{s:?}");
    let cron_jobs = "SELECT count(*) FROM cron.job WHERE jobname LIKE 'fbsched\\_%'";
    assert_eq!((count(&s1, cron_jobs).await, count(&control, cron_jobs).await), (1, 0));
    let fired = format!("SELECT count(*) FROM _forgebase_jobs WHERE schedule_id = '{}'", s.id);
    let end = std::time::Instant::now() + Duration::from_secs(30);
    while count(&s1, &fired).await < 1 {
        assert!(std::time::Instant::now() < end, "pg_cron on the shard never fired");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(count(&control, &fired).await, 0);

    // The tenant moves away (its placement names control): the shard's
    // pg_cron no longer fires its schedule.
    router.place("tb", "control", PlacementState::Moving).await.unwrap();
    svc.sync_pgcron_now().await.unwrap();
    assert_eq!(count(&s1, cron_jobs).await, 0);
    drop(handle);
}
