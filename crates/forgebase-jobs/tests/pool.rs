//! The in-process worker pool: execution, retries, dead-lettering, priority,
//! concurrency, cron, cancellation, graceful shutdown and crash recovery.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{Harness, Store, claim_one, harness, service, system, wait_for};
use forgebase_data::DefaultAuthorizer;
use forgebase_jobs::{
    HandlerError, Job, JobFilter, JobRegistry, JobService, JobStatus, NewJob, NewSchedule, PoolConfig, start_pool,
};
use serde_json::json;
use tokio::time::sleep;

fn fast() -> PoolConfig {
    PoolConfig {
        concurrency: 4,
        poll_interval: Duration::from_millis(20),
        maintenance_interval: Duration::from_millis(20),
        shutdown_grace: Duration::from_secs(5),
        ..PoolConfig::default()
    }
}

async fn status_of(svc: &JobService, id: &str) -> Job {
    svc.get(&system(), id).await.unwrap()
}

async fn wait_status(svc: &JobService, id: &str, status: JobStatus) -> Job {
    wait_for(Duration::from_secs(30), || async {
        let j = status_of(svc, id).await;
        (j.status == status).then_some(j)
    })
    .await
}

/// Like `wait_status`, but each poll also moves the harness clock forward, so
/// backoff delays elapse in virtual time instead of real time.
async fn wait_status_advancing(h: &Harness, id: &str, status: JobStatus) -> Job {
    wait_for(Duration::from_secs(30), || async {
        h.clock.advance(Duration::from_millis(100));
        let j = status_of(&h.svc, id).await;
        (j.status == status).then_some(j)
    })
    .await
}

#[tokio::test]
async fn pool_runs_a_job_and_stores_the_result() {
    let h = harness().await;
    let registry = JobRegistry::new().register("echo", |ctx: forgebase_jobs::JobContext| async move {
        Ok(json!({"echo": ctx.payload().clone(), "attempt": ctx.attempt()}))
    });
    let pool = start_pool(h.svc.clone(), registry, fast());
    let j = h
        .svc
        .enqueue(&system(), NewJob::new("echo", json!({"hello": "world"})))
        .await
        .unwrap()
        .0;
    let done = wait_status(&h.svc, &j.id, JobStatus::Succeeded).await;
    assert_eq!(done.result, Some(json!({"echo": {"hello": "world"}, "attempt": 1})));
    assert!(done.worker_id.unwrap().starts_with("inproc-"));
    pool.shutdown().await;
}

#[tokio::test]
async fn handler_errors_retry_with_backoff_then_succeed() {
    let h = harness().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let registry = JobRegistry::new().register("flaky", move |_ctx: forgebase_jobs::JobContext| {
        let n = c.fetch_add(1, Ordering::SeqCst) + 1;
        async move {
            if n < 3 {
                Err(HandlerError::retry(format!("transient {n}")))
            } else {
                Ok(json!(n))
            }
        }
    });
    let pool = start_pool(h.svc.clone(), registry, fast());
    let j = h
        .svc
        .enqueue(
            &system(),
            NewJob::new("flaky", json!(null)).retries(5, Duration::from_millis(30), Duration::from_millis(200)),
        )
        .await
        .unwrap()
        .0;
    let done = wait_status_advancing(&h, &j.id, JobStatus::Succeeded).await;
    assert_eq!((done.attempts, calls.load(Ordering::SeqCst)), (3, 3));
    let events: Vec<_> = h
        .svc
        .events(&system(), &j.id)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(events.iter().filter(|e| *e == "retry_scheduled").count(), 2);
    pool.shutdown().await;
}

#[tokio::test]
async fn dead_letter_queue_after_max_attempts_and_for_fatal_and_panicking_handlers() {
    let h = harness().await;
    let always = Arc::new(AtomicUsize::new(0));
    let fatal = Arc::new(AtomicUsize::new(0));
    let (a, f) = (always.clone(), fatal.clone());
    let registry = JobRegistry::new()
        .register("always_fails", move |_c: forgebase_jobs::JobContext| {
            a.fetch_add(1, Ordering::SeqCst);
            async { Err::<serde_json::Value, _>(HandlerError::retry("boom")) }
        })
        .register("fatal", move |_c: forgebase_jobs::JobContext| {
            f.fetch_add(1, Ordering::SeqCst);
            async { Err::<serde_json::Value, _>(HandlerError::fatal("cannot be fixed by retrying")) }
        })
        .register("panics", |_c: forgebase_jobs::JobContext| async {
            panic!("handler bug");
            #[allow(unreachable_code)]
            Ok(json!(null))
        });
    let pool = start_pool(h.svc.clone(), registry, fast());
    let retries = |n| (n, Duration::from_millis(10), Duration::from_millis(20));
    let (n, b, m) = retries(3);
    let j1 = h
        .svc
        .enqueue(&system(), NewJob::new("always_fails", json!(null)).retries(n, b, m))
        .await
        .unwrap()
        .0;
    let j2 = h
        .svc
        .enqueue(&system(), NewJob::new("fatal", json!(null)))
        .await
        .unwrap()
        .0;
    let j3 = h
        .svc
        .enqueue(&system(), NewJob::new("panics", json!(null)).retries(2, b, m))
        .await
        .unwrap()
        .0;

    let d1 = wait_status_advancing(&h, &j1.id, JobStatus::Dead).await;
    assert_eq!((d1.attempts, d1.last_error.as_deref()), (3, Some("boom")));
    assert_eq!(always.load(Ordering::SeqCst), 3, "ran exactly max_attempts times");
    let d2 = wait_status_advancing(&h, &j2.id, JobStatus::Dead).await;
    assert_eq!((d2.attempts, fatal.load(Ordering::SeqCst)), (1, 1));
    let d3 = wait_status_advancing(&h, &j3.id, JobStatus::Dead).await;
    assert_eq!((d3.attempts, d3.last_error.as_deref()), (2, Some("handler panicked")));

    let dlq = h
        .svc
        .list(
            &system(),
            JobFilter {
                status: Some(JobStatus::Dead),
                ..JobFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(dlq.total, 3);
    // The pool is still healthy after the panic.
    let ok = h
        .svc
        .enqueue(&system(), NewJob::new("fatal", json!(null)))
        .await
        .unwrap()
        .0;
    wait_status(&h.svc, &ok.id, JobStatus::Dead).await;
    pool.shutdown().await;
}

#[tokio::test]
async fn priority_order_and_concurrency_limit() {
    let h = harness().await;
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let (running, peak) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
    let (o, r, p) = (order.clone(), running.clone(), peak.clone());
    let registry = JobRegistry::new().register("work", move |ctx: forgebase_jobs::JobContext| {
        let (o, r, p) = (o.clone(), r.clone(), p.clone());
        async move {
            o.lock()
                .unwrap()
                .push(ctx.payload()["name"].as_str().unwrap().to_owned());
            let now = r.fetch_add(1, Ordering::SeqCst) + 1;
            p.fetch_max(now, Ordering::SeqCst);
            sleep(Duration::from_millis(150)).await;
            r.fetch_sub(1, Ordering::SeqCst);
            Ok(json!(null))
        }
    });
    // Enqueue before the pool starts, so the order is decided by priority alone.
    let mut ids = Vec::new();
    for (name, prio) in [
        ("low", 0),
        ("urgent", 100),
        ("normal", 10),
        ("low2", 0),
        ("normal2", 10),
        ("low3", 0),
    ] {
        ids.push(
            h.svc
                .enqueue(&system(), NewJob::new("work", json!({"name": name})).priority(prio))
                .await
                .unwrap()
                .0
                .id,
        );
    }
    let pool = start_pool(
        h.svc.clone(),
        registry,
        PoolConfig {
            concurrency: 2,
            ..fast()
        },
    );
    for id in &ids {
        wait_status(&h.svc, id, JobStatus::Succeeded).await;
    }
    assert_eq!(
        peak.load(Ordering::SeqCst),
        2,
        "never more than `concurrency`, and it was used"
    );
    let order = order.lock().unwrap().clone();
    assert_eq!(
        &order[..2],
        ["urgent", "normal"],
        "highest priorities start first: {order:?}"
    );
    assert!(order.iter().position(|n| n == "normal2").unwrap() < order.iter().position(|n| n == "low").unwrap());
    pool.shutdown().await;
}

#[tokio::test]
async fn cron_schedule_fires_jobs_through_the_pool() {
    let h = harness().await;
    let runs = Arc::new(AtomicUsize::new(0));
    let r = runs.clone();
    let registry = JobRegistry::new().register("tick", move |_c: forgebase_jobs::JobContext| {
        r.fetch_add(1, Ordering::SeqCst);
        async { Ok(json!("tock")) }
    });
    let pool = start_pool(h.svc.clone(), registry, fast());
    let s = h
        .svc
        .create_schedule(
            &system(),
            NewSchedule {
                name: "every-second".into(),
                cron: "* * * * * *".into(),
                kind: "tick".into(),
                ..NewSchedule::default()
            },
        )
        .await
        .unwrap();
    wait_for(Duration::from_secs(30), || async {
        h.clock.advance(Duration::from_millis(500));
        (runs.load(Ordering::SeqCst) >= 3).then_some(())
    })
    .await;
    pool.shutdown().await;

    let jobs = h
        .svc
        .list(
            &system(),
            JobFilter {
                kind: Some("tick".into()),
                limit: Some(200),
                ..JobFilter::default()
            },
        )
        .await
        .unwrap();
    assert!(
        jobs.items
            .iter()
            .all(|j| j.schedule_id.as_deref() == Some(s.id.as_str()))
    );
    let keys: std::collections::HashSet<_> = jobs.items.iter().map(|j| j.idempotency_key.clone().unwrap()).collect();
    assert_eq!(
        keys.len(),
        jobs.items.len(),
        "one job per firing, never two for the same due time"
    );
    assert!(jobs.items.iter().filter(|j| j.status == JobStatus::Succeeded).count() >= 3);
}

#[tokio::test]
async fn unregistered_kinds_are_left_for_external_workers() {
    let h = harness().await;
    let registry = JobRegistry::new().register("mine", |_c: forgebase_jobs::JobContext| async { Ok(json!(1)) });
    let pool = start_pool(h.svc.clone(), registry, fast());
    let theirs = h
        .svc
        .enqueue(&system(), NewJob::new("external_only", json!(null)))
        .await
        .unwrap()
        .0;
    let mine = h
        .svc
        .enqueue(&system(), NewJob::new("mine", json!(null)))
        .await
        .unwrap()
        .0;
    wait_status(&h.svc, &mine.id, JobStatus::Succeeded).await;
    sleep(Duration::from_millis(200)).await;
    assert_eq!(status_of(&h.svc, &theirs.id).await.status, JobStatus::Queued);
    // An external worker picks it up.
    let c = claim_one(&h.svc, "external-1").await.unwrap();
    assert_eq!(c.job.id, theirs.id);
    pool.shutdown().await;
}

#[tokio::test]
async fn cancelling_a_running_job_tells_the_handler_to_stop() {
    let h = harness().await;
    let started = Arc::new(AtomicUsize::new(0));
    let stopped = Arc::new(AtomicUsize::new(0));
    let (st, sp) = (started.clone(), stopped.clone());
    let registry = JobRegistry::new().register("long", move |ctx: forgebase_jobs::JobContext| {
        let (st, sp) = (st.clone(), sp.clone());
        async move {
            st.fetch_add(1, Ordering::SeqCst);
            tokio::select! {
                () = ctx.cancelled() => {
                    sp.fetch_add(1, Ordering::SeqCst);
                    Err(HandlerError::retry("stopped on request"))
                }
                () = sleep(Duration::from_secs(60)) => Ok(json!("finished")),
            }
        }
    });
    let pool = start_pool(h.svc.clone(), registry, fast());
    let j = h
        .svc
        .enqueue(
            &system(),
            NewJob::new("long", json!(null)).visibility_timeout(Duration::from_millis(150)),
        )
        .await
        .unwrap()
        .0;
    wait_for(Duration::from_secs(30), || async {
        (started.load(Ordering::SeqCst) == 1).then_some(())
    })
    .await;
    h.svc.cancel(&system(), &j.id).await.unwrap();
    let end = wait_status(&h.svc, &j.id, JobStatus::Cancelled).await;
    assert_eq!(stopped.load(Ordering::SeqCst), 1);
    assert_eq!(started.load(Ordering::SeqCst), 1, "a cancelled job is not retried");
    assert_eq!(end.last_error.as_deref(), Some("stopped on request"));
    pool.shutdown().await;
}

#[tokio::test]
async fn graceful_shutdown_lets_short_jobs_finish() {
    let h = harness().await;
    let registry = JobRegistry::new().register("short", |_c: forgebase_jobs::JobContext| async {
        sleep(Duration::from_millis(300)).await;
        Ok(json!("done"))
    });
    let pool = start_pool(
        h.svc.clone(),
        registry,
        PoolConfig {
            shutdown_grace: Duration::from_secs(5),
            ..fast()
        },
    );
    let j = h
        .svc
        .enqueue(&system(), NewJob::new("short", json!(null)))
        .await
        .unwrap()
        .0;
    wait_status(&h.svc, &j.id, JobStatus::Running).await;
    pool.shutdown().await;
    assert_eq!(status_of(&h.svc, &j.id).await.status, JobStatus::Succeeded);
}

#[tokio::test]
async fn graceful_shutdown_releases_jobs_that_outlive_the_grace_period() {
    let h = harness().await;
    let dropped = Arc::new(AtomicUsize::new(0));
    struct OnDrop(Arc<AtomicUsize>);
    impl Drop for OnDrop {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    let d = dropped.clone();
    let registry = JobRegistry::new().register("forever", move |_c: forgebase_jobs::JobContext| {
        let guard = OnDrop(d.clone());
        async move {
            let _guard = guard;
            sleep(Duration::from_secs(3600)).await;
            Ok(json!(null))
        }
    });
    let pool = start_pool(
        h.svc.clone(),
        registry,
        PoolConfig {
            shutdown_grace: Duration::from_millis(100),
            ..fast()
        },
    );
    let j = h
        .svc
        .enqueue(&system(), NewJob::new("forever", json!(null)))
        .await
        .unwrap()
        .0;
    wait_status(&h.svc, &j.id, JobStatus::Running).await;
    pool.shutdown().await;

    let after = status_of(&h.svc, &j.id).await;
    assert_eq!(after.status, JobStatus::Queued, "released, not lost");
    assert_eq!(after.attempts, 0, "a shutdown is not a failed attempt");
    assert_eq!(dropped.load(Ordering::SeqCst), 1, "the handler was stopped");
    let events: Vec<_> = h
        .svc
        .events(&system(), &j.id)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(events, ["enqueued", "claimed", "released"]);
}

#[tokio::test]
async fn a_crashed_pool_loses_nothing_and_the_job_completes_exactly_once() {
    let dir = Store::new().await;
    let db = dir.open().await;
    let (_data, svc, _clock) = service(&db, Arc::new(DefaultAuthorizer)).await;

    // Process 1 starts the job and "dies" in the middle of it.
    let started = Arc::new(AtomicUsize::new(0));
    let finished_by_first = Arc::new(AtomicUsize::new(0));
    let (s, f) = (started.clone(), finished_by_first.clone());
    let first = JobRegistry::new().register("once", move |_c: forgebase_jobs::JobContext| {
        let (s, f) = (s.clone(), f.clone());
        async move {
            s.fetch_add(1, Ordering::SeqCst);
            sleep(Duration::from_secs(3600)).await;
            f.fetch_add(1, Ordering::SeqCst);
            Ok(json!("from the first process"))
        }
    });
    let pool1 = start_pool(svc.clone(), first, fast());
    let j = svc
        .enqueue(
            &system(),
            NewJob::new("once", json!({"order": 7})).visibility_timeout(Duration::from_millis(400)),
        )
        .await
        .unwrap()
        .0;
    wait_for(Duration::from_secs(30), || async {
        (started.load(Ordering::SeqCst) == 1).then_some(())
    })
    .await;
    pool1.crash().await; // no release, no heartbeat, nothing: like kill -9
    assert_eq!(
        status_of(&svc, &j.id).await.status,
        JobStatus::Running,
        "the dead worker still 'holds' the job"
    );

    // The database is reopened by a fresh process; the job is still there.
    db.close().await;
    let db = dir.open().await;
    let (_data, svc, clock) = service(&db, Arc::new(DefaultAuthorizer)).await;
    assert_eq!(status_of(&svc, &j.id).await.status, JobStatus::Running);
    // The dead worker's lease runs out (virtual time: no waiting, no load sensitivity).
    clock.advance(Duration::from_secs(10));

    let completed = Arc::new(AtomicUsize::new(0));
    let c = completed.clone();
    let second = JobRegistry::new().register("once", move |ctx: forgebase_jobs::JobContext| {
        c.fetch_add(1, Ordering::SeqCst);
        async move { Ok(json!({"order": ctx.payload()["order"], "attempt": ctx.attempt()})) }
    });
    let pool2 = start_pool(svc.clone(), second, fast());
    let done = wait_status(&svc, &j.id, JobStatus::Succeeded).await;
    sleep(Duration::from_millis(500)).await; // would a second run show up?
    pool2.shutdown().await;

    assert_eq!(done.result, Some(json!({"order": 7, "attempt": 2})));
    assert_eq!(completed.load(Ordering::SeqCst), 1, "ran to completion exactly once");
    assert_eq!(finished_by_first.load(Ordering::SeqCst), 0);
    let events: Vec<_> = svc
        .events(&system(), &j.id)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(events, ["enqueued", "claimed", "lease_expired", "claimed", "succeeded"]);
    db.close().await;
}
