//! Schedules on pg_cron (FB-165, ADR-0029 §1.7).
//!
//! Three groups:
//! - `forgebase.fire_schedule()` against the Rust tick (PostgreSQL; the
//!   plain test engine is enough);
//! - the fallback: a database without an active pg_cron keeps every schedule
//!   on the tick (every backend);
//! - the real thing on the bundled engine: pg_cron fires, the listener
//!   wakes the pool, expressions move between pg_cron and the tick, a
//!   restart fires a missed schedule once. These start a managed engine from
//!   `FORGEBASE_PG_ENGINE_DIR` and skip when it has no pg_cron.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use common::{harness, system};
use forgebase_core::config::PostgresExtensions;
use forgebase_data::{DataService, DefaultAuthorizer};
use forgebase_db::{
    Backend, BackendKind, Database, DbOptions, DbTarget, ExtensionSetup, Feature, SecretString, Statement, TxMode,
    Value,
};
use forgebase_jobs::{JobService, NewSchedule, SchedulePatch, attach_pgcron};
use forgebase_pgengine::{Engine, EngineConfig, EngineHandle, EngineSource, Layout, Role};
use serde_json::json;

async fn rows(db: &Database, sql: &str) -> Vec<Vec<Value>> {
    db.fetch_all(&Statement::new(sql, vec![]))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"))
        .rows
}

async fn count(db: &Database, sql: &str) -> i64 {
    match rows(db, sql).await.first().and_then(|r| r.first()) {
        Some(Value::Integer(n)) => *n,
        other => panic!("{sql}: {other:?}"),
    }
}

async fn write(db: &Database, sql: &str) {
    let mut tx = db.begin(TxMode::Normal).await.unwrap();
    tx.execute(&Statement::new(sql, vec![]))
        .await
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    tx.commit().await.unwrap();
}

fn new(name: &str, cron: &str) -> NewSchedule {
    NewSchedule {
        name: name.into(),
        cron: cron.into(),
        kind: "tick".into(),
        queue: Some("work".into()),
        payload: json!({"x": 1}),
        priority: Some(3),
        max_attempts: Some(4),
        ..NewSchedule::default()
    }
}

/// The job columns that must not depend on who fired the schedule.
const JOB_COLUMNS: &str = "j.queue, j.kind, j.payload::text, j.priority, j.status, j.max_attempts, j.backoff_base_ms, j.backoff_max_ms, j.visibility_timeout_ms, j.timeout_ms, j.waiting_for, j.attempts, j.tenant, j.context::text, j.schedule_id IS NOT NULL";

#[tokio::test]
async fn fire_schedule_inserts_the_job_the_tick_inserts() {
    let h = harness().await;
    if h.db.kind() != BackendKind::Postgres {
        return;
    }
    let a = h.svc.create_schedule(&system(), new("sql", "* * * * *")).await.unwrap();
    let b = h
        .svc
        .create_schedule(&system(), new("tick", "0-30/5 * * * * *"))
        .await
        .unwrap();
    assert_eq!((a.fired_by.as_str(), b.fired_by.as_str()), ("forgebase", "forgebase"));
    // A tenant with queue defaults: both paths read `<tenant>/<queue>`.
    write(&h.db, "UPDATE _forgebase_job_schedules SET tenant = 'acme'").await;
    write(
        &h.db,
        &format!(
            "INSERT INTO _forgebase_job_queues (name, default_timeout_ms, default_priority, created_at, updated_at) \
             VALUES ('acme/work', 7000, 9, {0}, {0})",
            h.svc.now()
        ),
    )
    .await;
    let mut listener = h.db.listen("forgebase_jobs").await.unwrap();

    write(&h.db, &format!("SELECT forgebase.fire_schedule('{}')", a.id)).await;
    write(
        &h.db,
        "UPDATE _forgebase_job_schedules SET next_run_at = 1 WHERE name = 'tick'",
    )
    .await;
    assert_eq!(h.svc.fire_due_schedules(&system()).await.unwrap(), 1);

    let sql = |name: &str| {
        format!(
            "SELECT {JOB_COLUMNS} FROM _forgebase_jobs j JOIN _forgebase_job_schedules s ON s.id = j.schedule_id \
             WHERE s.name = '{name}'"
        )
    };
    let (by_sql, by_tick) = (rows(&h.db, &sql("sql")).await, rows(&h.db, &sql("tick")).await);
    assert_eq!(by_sql.len(), 1);
    assert_eq!(by_sql, by_tick, "fire_schedule and the tick must insert the same job");
    // The queue's default timeout applied, the schedule's priority kept.
    assert_eq!(by_sql[0][3], Value::Integer(3));
    assert_eq!(by_sql[0][9], Value::Integer(7000));
    assert_eq!(by_sql[0][12], Value::Text("acme".into()));

    // Created by the schedule, with its event, key and bookkeeping.
    let meta = rows(
        &h.db,
        "SELECT j.created_by, j.idempotency_key, (SELECT count(*) FROM _forgebase_job_events e WHERE e.job_id = j.id), \
         (s.last_job_id = j.id)::int, (s.last_run_at IS NOT NULL)::int FROM _forgebase_jobs j \
         JOIN _forgebase_job_schedules s ON s.id = j.schedule_id WHERE s.name = 'sql'",
    )
    .await;
    assert_eq!(meta[0][0], Value::Text("schedule:sql".into()));
    let Value::Text(key) = &meta[0][1] else {
        panic!("{meta:?}")
    };
    assert!(key.starts_with(&format!("schedule:{}:", a.id)), "{key}");
    let due: i64 = key.rsplit(':').next().unwrap().parse().unwrap();
    assert_eq!(due % 60_000, 0, "a minute schedule is keyed to the start of its minute");
    assert_eq!(meta[0][2], Value::Integer(1));
    assert_eq!(
        (meta[0][3].clone(), meta[0][4].clone()),
        (Value::Integer(1), Value::Integer(1))
    );

    // NOTIFY forgebase_jobs carries the job id.
    let payload = tokio::time::timeout(Duration::from_secs(30), listener.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(payload.starts_with("job-"), "{payload}");

    // The same due time is one job, however often it is fired.
    write(&h.db, &format!("SELECT forgebase.fire_schedule('{}')", a.id)).await;
    write(&h.db, &format!("SELECT forgebase.fire_schedule('{}', {due})", a.id)).await;
    assert_eq!(
        count(
            &h.db,
            &format!("SELECT count(*) FROM _forgebase_jobs WHERE schedule_id = '{}'", a.id)
        )
        .await,
        1
    );
    // Another due time is another job; a disabled or unknown schedule fires nothing.
    write(
        &h.db,
        &format!("SELECT forgebase.fire_schedule('{}', {})", a.id, due + 60_000),
    )
    .await;
    assert_eq!(
        count(
            &h.db,
            &format!("SELECT count(*) FROM _forgebase_jobs WHERE schedule_id = '{}'", a.id)
        )
        .await,
        2
    );
    h.svc
        .update_schedule(
            &system(),
            &a.id,
            SchedulePatch {
                enabled: Some(false),
                ..SchedulePatch::default()
            },
        )
        .await
        .unwrap();
    write(
        &h.db,
        &format!("SELECT forgebase.fire_schedule('{}', {})", a.id, due + 120_000),
    )
    .await;
    write(&h.db, "SELECT forgebase.fire_schedule('sch-nope')").await;
    assert_eq!(count(&h.db, "SELECT count(*) FROM _forgebase_jobs").await, 3);
}

#[tokio::test]
async fn second_slots_are_aligned_to_the_step() {
    let h = harness().await;
    if h.db.kind() != BackendKind::Postgres {
        return;
    }
    let s = h
        .svc
        .create_schedule(&system(), new("secs", "*/10 * * * * *"))
        .await
        .unwrap();
    write(&h.db, &format!("SELECT forgebase.fire_schedule('{}')", s.id)).await;
    let keys = rows(&h.db, "SELECT idempotency_key FROM _forgebase_jobs").await;
    let Value::Text(key) = &keys[0][0] else {
        panic!("{keys:?}")
    };
    let due: i64 = key.rsplit(':').next().unwrap().parse().unwrap();
    assert_eq!(due % 10_000, 0, "{key}");
}

#[tokio::test]
async fn without_an_active_pg_cron_every_schedule_stays_on_the_tick() {
    let h = harness().await;
    // The harness never runs the extension setup: the feature is not active.
    assert!(!h.db.feature_active(Feature::Cron));
    assert!(attach_pgcron(&h.svc, &h.db).await.is_none());
    assert!(!h.svc.pgcron_active());
    let s = h
        .svc
        .create_schedule(&system(), new("plain", "* * * * * *"))
        .await
        .unwrap();
    assert_eq!(s.fired_by, "forgebase");
    h.clock.advance(Duration::from_millis(1500));
    assert_eq!(h.svc.fire_due_schedules(&system()).await.unwrap(), 1);
    let after = h.svc.schedule(&system(), &s.id).await.unwrap();
    assert_eq!(after.fired_by, "forgebase");
    assert!(after.last_run_at.is_some());
}

// ---------------------------------------------------------------- the engine

struct Cron {
    _dir: tempfile::TempDir,
    handle: EngineHandle,
    db: Database,
    svc: JobService,
}

fn engine_dir() -> Option<PathBuf> {
    match std::env::var_os("FORGEBASE_PG_ENGINE_DIR").filter(|v| !v.is_empty()) {
        Some(d) => Some(PathBuf::from(d)),
        None => {
            eprintln!("skipped: FORGEBASE_PG_ENGINE_DIR is not set");
            None
        }
    }
}

/// A managed engine with pg_cron preloaded, migrated and set up as the
/// server does it; `None` when there is no such engine.
async fn cron_engine(port: u16) -> Option<Cron> {
    let engine = engine_dir()?;
    let base = std::env::var_os("FORGEBASE_PGENGINE_TEST_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&base).unwrap();
    let dir = tempfile::Builder::new().prefix("pc").tempdir_in(base).unwrap();
    let handle = Engine::prepare(
        &Layout::primary(dir.path()),
        &EngineConfig {
            source: EngineSource::Dir(engine),
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
    let db = open_db(&handle).await;
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
    let data = DataService::new(Arc::new(db.clone()), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    let svc = JobService::new(data);
    Some(Cron {
        _dir: dir,
        handle,
        db,
        svc,
    })
}

async fn open_db(h: &EngineHandle) -> Database {
    Database::open(DbOptions {
        target: DbTarget::postgres(SecretString::new(h.url(Role::Server).expose()), true),
        synchronous: forgebase_core::Synchronous::Full,
        busy_timeout: Duration::from_secs(5),
        read_pool_size: 2,
        create_if_missing: false,
        read_only: false,
    })
    .await
    .unwrap()
}

async fn eventually<F, Fut>(what: &str, secs: u64, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < end {
        if f().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("timed out waiting for {what}");
}

fn jobs_of(id: &str) -> String {
    format!("SELECT count(*) FROM _forgebase_jobs WHERE schedule_id = '{id}'")
}

/// Jobs of a schedule whose due time is at or before `cutoff` (a catch-up's, not pg_cron's).
fn missed_jobs(id: &str, cutoff: i64) -> String {
    format!(
        "SELECT count(*) FROM _forgebase_jobs WHERE schedule_id = '{id}' \
         AND substring(idempotency_key from '[0-9]+$')::bigint <= {cutoff}"
    )
}

fn cron_jobs() -> &'static str {
    "SELECT count(*) FROM cron.job WHERE jobname LIKE 'fbsched\\_%'"
}

#[tokio::test]
async fn pg_cron_fires_wakes_the_pool_and_hands_over() {
    let Some(c) = cron_engine(18_461).await else { return };
    let sys = system();
    let handle = attach_pgcron(&c.svc, &c.db).await.expect("attached");
    assert!(c.svc.pgcron_active());

    // Drain wake-ups so far; the next one must come from NOTIFY.
    let _ = tokio::time::timeout(Duration::from_millis(100), c.svc.work_available()).await;
    let s = c.svc.create_schedule(&sys, new("secs", "*/2 * * * * *")).await.unwrap();
    assert_eq!(s.fired_by, "pg_cron");
    assert!(s.next_run_at.as_str() > "2000", "{}", s.next_run_at);
    let row = rows(
        &c.db,
        "SELECT jobname, schedule, command FROM cron.job WHERE jobname LIKE 'fbsched\\_%'",
    )
    .await;
    assert_eq!(
        row,
        vec![vec![
            Value::Text(format!("fbsched_{}", s.id)),
            Value::Text("2 seconds".into()),
            Value::Text(format!("SELECT forgebase.fire_schedule('{}')", s.id)),
        ]]
    );
    // The tick never fires it: the sentinel keeps it out of the due list.
    assert_eq!(c.svc.fire_due_schedules(&sys).await.unwrap(), 0);

    tokio::time::timeout(Duration::from_secs(20), c.svc.work_available())
        .await
        .expect("a NOTIFY wakes the pool");
    eventually("a job from pg_cron", 20, || async {
        count(&c.db, &jobs_of(&s.id)).await >= 1
    })
    .await;
    let keys = rows(
        &c.db,
        &format!(
            "SELECT idempotency_key, created_by, queue FROM _forgebase_jobs WHERE schedule_id = '{}'",
            s.id
        ),
    )
    .await;
    let Value::Text(key) = &keys[0][0] else {
        panic!("{keys:?}")
    };
    let due: i64 = key.rsplit(':').next().unwrap().parse().unwrap();
    assert_eq!(due % 2000, 0, "{key}");
    assert_eq!(keys[0][1], Value::Text("schedule:secs".into()));
    let fired = c.svc.schedule(&sys, &s.id).await.unwrap();
    assert!(fired.last_run_at.is_some() && fired.last_job_id.is_some());
    assert_eq!(fired.fired_by, "pg_cron");

    // An expression pg_cron cannot read the same way goes back to the tick.
    let moved = c
        .svc
        .update_schedule(
            &sys,
            &s.id,
            SchedulePatch {
                cron: Some("*/7 * * * * *".into()),
                ..SchedulePatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(moved.fired_by, "forgebase");
    assert_eq!(count(&c.db, cron_jobs()).await, 0);
    assert!(
        count(
            &c.db,
            &format!("SELECT next_run_at FROM _forgebase_job_schedules WHERE id = '{}'", s.id)
        )
        .await
            < 253_402_300_799_000
    );
    // ... and back.
    let back = c
        .svc
        .update_schedule(
            &sys,
            &s.id,
            SchedulePatch {
                cron: Some("0 9 * * MON-FRI".into()),
                ..SchedulePatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(back.fired_by, "pg_cron");
    assert_eq!(count(&c.db, cron_jobs()).await, 1);
    // Disabled: off pg_cron; enabled: on again; deleted: gone.
    let off = c
        .svc
        .update_schedule(
            &sys,
            &s.id,
            SchedulePatch {
                enabled: Some(false),
                ..SchedulePatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(off.fired_by, "forgebase");
    assert_eq!(count(&c.db, cron_jobs()).await, 0);
    let on = c
        .svc
        .update_schedule(
            &sys,
            &s.id,
            SchedulePatch {
                enabled: Some(true),
                ..SchedulePatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(on.fired_by, "pg_cron");
    c.svc.delete_schedule(&sys, &s.id).await.unwrap();
    assert_eq!(count(&c.db, cron_jobs()).await, 0);

    // The census FB-146 allows: pg_cron runs exactly the fire function.
    let s2 = c.svc.create_schedule(&sys, new("census", "* * * * *")).await.unwrap();
    assert_eq!(
        count(
            &c.db,
            "SELECT count(*) FROM cron.job WHERE command NOT LIKE 'SELECT forgebase.fire_schedule(%'"
        )
        .await,
        0
    );
    c.svc.delete_schedule(&sys, &s2.id).await.unwrap();
    drop(handle);
    c.db.close().await;
    c.handle.stop().await.unwrap();
}

#[tokio::test]
async fn a_restart_fires_a_missed_schedule_once_and_cleans_up_when_pg_cron_goes() {
    let Some(c) = cron_engine(18_462).await else { return };
    let sys = system();
    let handle = attach_pgcron(&c.svc, &c.db).await.expect("attached");
    // Every minute, and on the tick: one of each.
    let a = c.svc.create_schedule(&sys, new("minutely", "* * * * *")).await.unwrap();
    let b = c
        .svc
        .create_schedule(&sys, new("tick-only", "*/7 * * * * *"))
        .await
        .unwrap();
    assert_eq!((a.fired_by.as_str(), b.fired_by.as_str()), ("pg_cron", "forgebase"));
    drop(handle);

    // The server was down for ten minutes: nothing fired since.
    let ago = c.svc.now() - 600_000;
    write(
        &c.db,
        &format!(
            "UPDATE _forgebase_job_schedules SET created_at = {ago}, updated_at = {ago}, last_run_at = NULL WHERE id = '{}'",
            a.id
        ),
    )
    .await;
    // Take pg_cron's job away so only the catch-up can fire it this minute.
    write(&c.db, &format!("SELECT cron.unschedule('fbsched_{}')", a.id)).await;
    let handle = attach_pgcron(&c.svc, &c.db).await.expect("attached again");
    // Only the catch-up's job counts: pg_cron's own job is back at once, and on a
    // loaded machine a minute boundary can pass during this test and fire it.
    let missed = missed_jobs(&a.id, ago + 60_000);
    assert_eq!(
        count(&c.db, &missed).await,
        1,
        "the missed firings collapse into one job"
    );
    let key = rows(
        &c.db,
        &format!(
            "SELECT idempotency_key FROM _forgebase_jobs WHERE schedule_id = '{}' \
             AND substring(idempotency_key from '[0-9]+$')::bigint <= {}",
            a.id,
            ago + 60_000
        ),
    )
    .await;
    let Value::Text(key) = &key[0][0] else {
        panic!("{key:?}")
    };
    let due: i64 = key.rsplit(':').next().unwrap().parse().unwrap();
    assert!(due >= ago && due <= ago + 60_000, "the first missed minute: {key}");
    assert_eq!(count(&c.db, cron_jobs()).await, 1, "pg_cron's job is back");
    // A second start without a miss adds nothing.
    drop(handle);
    let handle = attach_pgcron(&c.svc, &c.db).await.expect("attached a third time");
    assert_eq!(count(&c.db, &missed).await, 1);
    drop(handle);

    // A server whose feature is not active hands everything back.
    let plain = Database::open(DbOptions {
        target: DbTarget::postgres(SecretString::new(c.handle.url(Role::Server).expose()), true),
        synchronous: forgebase_core::Synchronous::Full,
        busy_timeout: Duration::from_secs(5),
        read_pool_size: 1,
        create_if_missing: false,
        read_only: false,
    })
    .await
    .unwrap();
    assert!(!plain.feature_active(Feature::Cron), "no setup ran on this handle");
    assert!(attach_pgcron(&c.svc, &plain).await.is_none());
    assert_eq!(count(&c.db, cron_jobs()).await, 0);
    let back = c.svc.schedule(&sys, &a.id).await.unwrap();
    assert_eq!(back.fired_by, "forgebase");
    plain.close().await;
    c.db.close().await;
    c.handle.stop().await.unwrap();
}

/// The differential test's live half: the real pg_cron accepts every
/// expression the mapping produces and rejects the ones it must not map.
#[tokio::test]
async fn the_real_pg_cron_accepts_what_the_mapping_produces() {
    let Some(c) = cron_engine(18_463).await else { return };
    let mapped = [
        "* * * * *",
        "*/15 * * * *",
        "0 9 * * MON-FRI",
        "0 9 * * mon-fri",
        "30 2 1 JAN *",
        "0 0 * * 7",
        "0 0 13 * FRI",
        "1-5/2,10 */6 * * *",
        "0 0 *,5 * *",
        "0-59/20 */4 * jan-mar *",
        "*/10 * * * * *",
        "*/1 * * * * *",
        "*/30 * * * * *",
        "0 0 0 * * *",
    ];
    for expr in mapped {
        let text = forgebase_jobs::map_expression(expr).unwrap_or_else(|| panic!("{expr:?} should map"));
        let sql = format!("SELECT cron.schedule('fbdiff', '{text}', 'SELECT 1')");
        let mut tx = c.db.begin(TxMode::Normal).await.unwrap();
        tx.fetch_all(&Statement::new(sql, vec![]))
            .await
            .unwrap_or_else(|e| panic!("pg_cron refused {text:?} (from {expr:?}): {e}"));
        tx.commit().await.unwrap();
    }
    // What pg_cron refuses must not be mapped.
    for expr in ["5/10 * * * *", "0 0 1/2 * *", "? * * * *"] {
        assert!(forgebase_jobs::map_expression(expr).is_none(), "{expr:?}");
        let mut tx = c.db.begin(TxMode::Normal).await.unwrap();
        let r = tx
            .fetch_all(&Statement::new(
                format!("SELECT cron.schedule('fbdiff', '{expr}', 'SELECT 1')"),
                vec![],
            ))
            .await;
        let _ = tx.rollback().await;
        assert!(r.is_err(), "pg_cron should refuse {expr:?}");
    }
    c.db.close().await;
    c.handle.stop().await.unwrap();
}
