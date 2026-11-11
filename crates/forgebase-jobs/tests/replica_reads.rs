//! Replica reads in the job service (ADR-0029 §2.5, FB-169): job list, get,
//! events, attempts and statistics read a lagging standby when the request
//! allows it. Claims, heartbeats, completions, results, checkpoints,
//! schedules and every write read the primary.
//!
//! Needs a PostgreSQL engine (`FORGEBASE_PG_ENGINE_DIR` or
//! `FORGEBASE_TEST_PG_ENGINE`); without one each test prints a skip.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::routing::testing::RoutedCluster;
use forgebase_db::routing::{Consistency, ReadAfter, RequestScope, scope};
use forgebase_jobs::{ClaimRequest, JobError, JobFilter, JobService, JobStatus, ManualClock, NewJob};
use serde_json::json;

struct Env {
    rc: RoutedCluster,
    jobs: JobService,
    ctx: AccessContext,
    j1: String,
}

/// `j1` queued and replayed on the standby; then replay is paused, `j1` is
/// claimed (running, one attempt) and `j2` is enqueued: the standby still
/// shows `j1` queued and has no `j2`.
async fn env() -> Option<Env> {
    let Some(rc) = RoutedCluster::start().await else {
        eprintln!("skipped: no PostgreSQL engine configured");
        return None;
    };
    let ctx = AccessContext::system();
    let data = DataService::new(rc.backend(), Arc::new(DefaultAuthorizer)).await.unwrap();
    let jobs = JobService::with_clock(data, Arc::new(ManualClock::from_system()));
    let (j1, _) = jobs.enqueue(&ctx, NewJob::new("k", json!({"n": 1}))).await.unwrap();
    rc.sync().await;
    rc.lag().await;
    let claimed = jobs
        .claim(
            &ctx,
            ClaimRequest {
                worker_id: "w1".into(),
                max: Some(1),
                ..ClaimRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1, "the claim found j1 on the primary");
    jobs.enqueue(&ctx, NewJob::new("k", json!({"n": 2}))).await.unwrap();
    Some(Env {
        rc,
        jobs,
        ctx,
        j1: j1.id,
    })
}

#[tokio::test]
async fn job_reads_use_a_lagging_replica_when_allowed() {
    let Some(e) = env().await else { return };
    let (jobs, ctx, rc) = (&e.jobs, &e.ctx, &e.rc);

    // list: one job on the standby, and still queued there.
    let before = rc.replica_reads();
    let page = jobs.list(ctx, JobFilter::default()).await.unwrap();
    assert_eq!(page.total, 1, "the standby has j1 only");
    assert_eq!(page.items[0].status, JobStatus::Queued);
    assert!(rc.replica_reads() > before);

    // get
    let before = rc.replica_reads();
    assert_eq!(jobs.get(ctx, &e.j1).await.unwrap().status, JobStatus::Queued);
    assert!(rc.replica_reads() > before);

    // events and attempts: the claim is not replayed yet
    let before = rc.replica_reads();
    let events = jobs.events(ctx, &e.j1).await.unwrap();
    assert!(events.iter().all(|ev| ev.event != "claimed"), "{events:?}");
    assert!(jobs.attempts(ctx, &e.j1).await.unwrap().is_empty());
    assert!(rc.replica_reads() >= before + 2);

    // stats
    let before = rc.replica_reads();
    let stats = jobs.stats(ctx).await.unwrap();
    assert_eq!(stats.totals["queued"], 1);
    assert_eq!(stats.totals["running"], 0);
    assert!(rc.replica_reads() > before);
    assert_eq!(rc.fallbacks(), 0);
}

#[tokio::test]
async fn claims_completions_and_work_in_flight_read_the_primary() {
    let Some(e) = env().await else { return };
    let (jobs, ctx, rc) = (&e.jobs, &e.ctx, &e.rc);
    let before = rc.replica_reads();

    // Another claim sees j2 (primary only) and the lease of j1 is intact.
    let claimed = jobs
        .claim(
            ctx,
            ClaimRequest {
                worker_id: "w2".into(),
                max: Some(5),
                ..ClaimRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1, "w2 claims j2, which the standby lacks");
    let lease = &claimed[0];
    jobs.heartbeat(ctx, &lease.job.id, &lease.lease_id).await.unwrap();
    jobs.complete(ctx, &lease.job.id, &lease.lease_id, Some(json!({"ok": true})))
        .await
        .unwrap();
    // The result and the checkpoint of work in flight are primary reads.
    assert_eq!(
        jobs.result(ctx, &lease.job.id).await.unwrap(),
        Some(json!({"ok": true}))
    );
    jobs.checkpoint(ctx, &lease.job.id).await.unwrap();
    // Management writes.
    jobs.cancel(ctx, &e.j1).await.unwrap();
    assert_eq!(rc.replica_reads(), before, "claims, acks and writes read the primary");
    assert!(matches!(
        jobs.get(ctx, "does-not-exist").await,
        Err(JobError::NotFound(_))
    ));
}

#[tokio::test]
async fn strong_reads_and_a_requests_own_writes_see_the_primary() {
    let Some(e) = env().await else { return };
    let (jobs, ctx, rc) = (&e.jobs, &e.ctx, &e.rc);

    let strong = RequestScope::new(Consistency::Strong, ReadAfter::new());
    let before = rc.replica_reads();
    let page = scope::scoped(strong, jobs.list(ctx, JobFilter::default()))
        .await
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(rc.replica_reads(), before);

    let own = RequestScope::new(Consistency::Session, ReadAfter::new());
    let got = scope::scoped(own, async {
        let (j, _) = jobs.enqueue(ctx, NewJob::new("k", json!({"n": 3}))).await.unwrap();
        jobs.get(ctx, &j.id).await
    })
    .await;
    assert!(got.is_ok(), "read-your-writes");
}
