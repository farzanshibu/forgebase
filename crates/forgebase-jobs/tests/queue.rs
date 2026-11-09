//! The job queue against a real SQLite database: enqueue, delay, priority,
//! idempotency, retries with backoff, dead-letter queue, leases, cancellation,
//! schedules and authorisation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{Only, claim_after, claim_one, claim_req, harness, harness_with, ms, system};
use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::{AccessContext, Action};
use forgebase_jobs::{ClaimRequest, JobFilter, JobStatus, NewJob, NewSchedule, NewWorker, SchedulePatch};
use serde_json::json;

fn job(kind: &str) -> NewJob {
    NewJob::new(kind, json!({"n": 1}))
}

#[tokio::test]
async fn enqueue_claim_complete_and_history() {
    let h = harness().await;
    let (j, created) = h
        .svc
        .enqueue(&system(), NewJob::new("send_email", json!({"to": "a@b.c"})))
        .await
        .unwrap();
    assert!(created);
    assert_eq!(j.status, JobStatus::Queued);
    assert_eq!(j.queue, "default");
    assert_eq!(j.payload, json!({"to": "a@b.c"}));
    assert_eq!(j.attempts, 0);

    let c = claim_one(&h.svc, "w1").await.expect("due job is claimed");
    assert_eq!(c.job.id, j.id);
    assert_eq!(c.job.status, JobStatus::Running);
    assert_eq!(c.job.attempts, 1);
    assert_eq!(c.job.worker_id.as_deref(), Some("w1"));
    assert!(c.job.visibility_until.is_some());
    assert!(
        claim_one(&h.svc, "w2").await.is_none(),
        "a running job is not handed out twice"
    );

    let done = h
        .svc
        .complete(&system(), &j.id, &c.lease_id, Some(json!({"sent": true})))
        .await
        .unwrap();
    assert_eq!(done.status, JobStatus::Succeeded);
    assert_eq!(done.result, Some(json!({"sent": true})));
    assert!(done.finished_at.is_some());
    assert!(done.visibility_until.is_none());

    let again = h.svc.complete(&system(), &j.id, &c.lease_id, None).await.unwrap_err();
    assert_eq!(again.code(), "lease_lost");
    assert_eq!(again.kind(), ErrorKind::Conflict);

    let events: Vec<_> = h
        .svc
        .events(&system(), &j.id)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(events, ["enqueued", "claimed", "succeeded"]);

    let missing = h.svc.get(&system(), "nope").await.unwrap_err();
    assert_eq!(missing.code(), "job_not_found");
}

#[tokio::test]
async fn higher_priority_first_then_oldest() {
    let h = harness().await;
    let low1 = h.svc.enqueue(&system(), job("k").priority(0)).await.unwrap().0;
    let high = h.svc.enqueue(&system(), job("k").priority(10)).await.unwrap().0;
    let mid = h.svc.enqueue(&system(), job("k").priority(5)).await.unwrap().0;
    let low2 = h.svc.enqueue(&system(), job("k").priority(0)).await.unwrap().0;
    let order: Vec<String> = h
        .svc
        .claim(&system(), claim_req("w", 4))
        .await
        .unwrap()
        .into_iter()
        .map(|c| c.job.id)
        .collect();
    assert_eq!(order, [high.id, mid.id, low1.id, low2.id]);
}

#[tokio::test]
async fn delayed_job_waits_for_its_time() {
    let h = harness().await;
    let j = h
        .svc
        .enqueue(&system(), job("k").delay(Duration::from_millis(1500)))
        .await
        .unwrap()
        .0;
    assert!(ms(&j.run_at) > ms(&j.created_at), "run_at is after creation");
    assert!(claim_one(&h.svc, "w").await.is_none());
    h.clock.advance(Duration::from_millis(150));
    assert!(claim_one(&h.svc, "w").await.is_none(), "still not due");
    let c = claim_after(&h, "w", Duration::from_secs(10))
        .await
        .expect("claimable once due");
    assert_eq!(c.job.id, j.id);

    // An absolute run_at works the same way.
    let at = forgebase_jobs::iso(h.svc.now() + 1200);
    let later = h
        .svc
        .enqueue(
            &system(),
            NewJob {
                run_at: Some(at),
                ..job("k")
            },
        )
        .await
        .unwrap()
        .0;
    assert!(claim_one(&h.svc, "w").await.is_none());
    assert_eq!(
        claim_after(&h, "w", Duration::from_secs(10)).await.unwrap().job.id,
        later.id
    );
}

#[tokio::test]
async fn idempotency_key_dedups_even_under_races() {
    let h = harness().await;
    let a = h
        .svc
        .enqueue(&system(), job("k").idempotency_key("order-1"))
        .await
        .unwrap();
    let b = h
        .svc
        .enqueue(&system(), job("k").idempotency_key("order-1"))
        .await
        .unwrap();
    assert!(a.1 && !b.1);
    assert_eq!(a.0.id, b.0.id);
    // Same key in another queue is a different job.
    let other = h
        .svc
        .enqueue(&system(), job("k").queue("other").idempotency_key("order-1"))
        .await
        .unwrap();
    assert!(other.1);
    assert_ne!(other.0.id, a.0.id);

    let mut tasks = Vec::new();
    for _ in 0..10 {
        let svc = h.svc.clone();
        tasks.push(tokio::spawn(async move {
            svc.enqueue(&system(), job("k").idempotency_key("race")).await.unwrap()
        }));
    }
    let mut created = 0;
    let mut ids = std::collections::HashSet::new();
    for t in tasks {
        let (j, c) = t.await.unwrap();
        created += usize::from(c);
        ids.insert(j.id);
    }
    assert_eq!(created, 1, "exactly one enqueue wins");
    assert_eq!(ids.len(), 1);
    let total = h
        .svc
        .list(
            &system(),
            JobFilter {
                kind: Some("k".into()),
                ..JobFilter::default()
            },
        )
        .await
        .unwrap()
        .total;
    assert_eq!(total, 3);
}

#[tokio::test]
async fn failures_back_off_exponentially_then_dead_letter() {
    let h = harness().await;
    let j = h
        .svc
        .enqueue(
            &system(),
            job("flaky").retries(3, Duration::from_millis(400), Duration::from_secs(10)),
        )
        .await
        .unwrap()
        .0;

    // Attempt 1 fails: retry after 400ms minus up to 25% jitter.
    let c1 = claim_one(&h.svc, "w").await.unwrap();
    let before = h.svc.now();
    let r1 = h
        .svc
        .fail(&system(), &j.id, &c1.lease_id, "boom 1", true)
        .await
        .unwrap();
    let after = h.svc.now();
    assert_eq!(r1.status, JobStatus::Queued);
    assert_eq!(r1.attempts, 1);
    assert_eq!(r1.last_error.as_deref(), Some("boom 1"));
    assert!(ms(&r1.run_at) - before >= 300, "delay >= 75% of 400ms");
    assert!(ms(&r1.run_at) - after <= 400, "delay <= 400ms");
    assert!(
        claim_one(&h.svc, "w").await.is_none(),
        "not claimable during the backoff"
    );

    // Attempt 2 fails: the delay doubles.
    let c2 = claim_after(&h, "w", Duration::from_secs(10)).await.unwrap();
    assert_eq!(c2.job.attempts, 2);
    let before = h.svc.now();
    let r2 = h
        .svc
        .fail(&system(), &j.id, &c2.lease_id, "boom 2", true)
        .await
        .unwrap();
    let after = h.svc.now();
    assert!(ms(&r2.run_at) - before >= 600, "delay >= 75% of 800ms");
    assert!(ms(&r2.run_at) - after <= 800, "delay <= 800ms");

    // Attempt 3 is the last: failing it dead-letters the job.
    let c3 = claim_after(&h, "w", Duration::from_secs(10)).await.unwrap();
    assert_eq!(c3.job.attempts, 3);
    let dead = h
        .svc
        .fail(&system(), &j.id, &c3.lease_id, "boom 3", true)
        .await
        .unwrap();
    assert_eq!(dead.status, JobStatus::Dead);
    assert_eq!(dead.last_error.as_deref(), Some("boom 3"));
    assert!(dead.finished_at.is_some());
    assert!(claim_one(&h.svc, "w").await.is_none());

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
    assert_eq!(dlq.total, 1);
    assert_eq!(dlq.items[0].id, j.id);

    let events: Vec<_> = h
        .svc
        .events(&system(), &j.id)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(
        events,
        [
            "enqueued",
            "claimed",
            "retry_scheduled",
            "claimed",
            "retry_scheduled",
            "claimed",
            "dead"
        ]
    );

    // From the dead-letter queue back into service with a fresh budget.
    let revived = h.svc.retry(&system(), &j.id).await.unwrap();
    assert_eq!((revived.status, revived.attempts), (JobStatus::Queued, 0));
    let c = claim_one(&h.svc, "w").await.unwrap();
    assert_eq!(c.job.attempts, 1);
    assert_eq!(
        h.svc
            .complete(&system(), &j.id, &c.lease_id, None)
            .await
            .unwrap()
            .status,
        JobStatus::Succeeded
    );
    assert_eq!(
        h.svc.retry(&system(), &j.id).await.unwrap_err().code(),
        "job_not_retryable"
    );
}

#[tokio::test]
async fn non_retryable_failure_goes_straight_to_the_dead_letter_queue() {
    let h = harness().await;
    let j = h.svc.enqueue(&system(), job("k")).await.unwrap().0;
    let c = claim_one(&h.svc, "w").await.unwrap();
    let dead = h
        .svc
        .fail(&system(), &j.id, &c.lease_id, "bad input", false)
        .await
        .unwrap();
    assert_eq!((dead.status, dead.attempts), (JobStatus::Dead, 1));
}

#[tokio::test]
async fn expired_lease_is_reclaimed_and_the_old_worker_is_fenced_off() {
    let h = harness().await;
    let j = h
        .svc
        .enqueue(&system(), job("k").visibility_timeout(Duration::from_millis(150)))
        .await
        .unwrap()
        .0;

    let a = claim_one(&h.svc, "worker-a").await.unwrap();
    assert!(claim_one(&h.svc, "worker-b").await.is_none(), "lease still valid");
    h.clock.advance(Duration::from_millis(900)); // worker-a "crashes": no heartbeat

    let b = claim_one(&h.svc, "worker-b")
        .await
        .expect("expired lease is re-claimed");
    assert_eq!(b.job.id, j.id);
    assert_eq!(b.job.attempts, 2);
    assert_ne!(a.lease_id, b.lease_id);

    // The late worker can neither heartbeat nor complete.
    assert_eq!(
        h.svc.heartbeat(&system(), &j.id, &a.lease_id).await.unwrap_err().code(),
        "lease_lost"
    );
    assert_eq!(
        h.svc
            .complete(&system(), &j.id, &a.lease_id, None)
            .await
            .unwrap_err()
            .code(),
        "lease_lost"
    );
    assert_eq!(
        h.svc
            .fail(&system(), &j.id, &a.lease_id, "late", true)
            .await
            .unwrap_err()
            .code(),
        "lease_lost"
    );
    // The current worker can, once.
    let done = h
        .svc
        .complete(&system(), &j.id, &b.lease_id, Some(json!("ok")))
        .await
        .unwrap();
    assert_eq!(done.status, JobStatus::Succeeded);
    assert_eq!(done.worker_id.as_deref(), Some("worker-b"));

    let events: Vec<_> = h
        .svc
        .events(&system(), &j.id)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(events, ["enqueued", "claimed", "lease_expired", "claimed", "succeeded"]);
}

#[tokio::test]
async fn reaper_requeues_idle_expired_leases_and_dead_letters_exhausted_ones() {
    let h = harness().await;
    let fresh = h
        .svc
        .enqueue(&system(), job("k").visibility_timeout(Duration::from_millis(600)))
        .await
        .unwrap()
        .0;
    let last = h
        .svc
        .enqueue(
            &system(),
            job("k")
                .retries(1, Duration::ZERO, Duration::ZERO)
                .visibility_timeout(Duration::from_millis(600)),
        )
        .await
        .unwrap()
        .0;
    let claimed = h.svc.claim(&system(), claim_req("w", 2)).await.unwrap();
    assert_eq!(claimed.len(), 2);
    assert_eq!(h.svc.reap_expired(&system()).await.unwrap(), 0, "leases are valid");
    h.clock.advance(Duration::from_millis(900));
    assert_eq!(h.svc.reap_expired(&system()).await.unwrap(), 2);
    assert_eq!(h.svc.get(&system(), &fresh.id).await.unwrap().status, JobStatus::Queued);
    let last = h.svc.get(&system(), &last.id).await.unwrap();
    assert_eq!(
        last.status,
        JobStatus::Dead,
        "its only attempt was used by the lost lease"
    );
    assert!(last.last_error.unwrap().contains("lease expired"));
}

#[tokio::test]
async fn heartbeats_keep_a_slow_job_leased() {
    let h = harness().await;
    let j = h
        .svc
        .enqueue(&system(), job("k").visibility_timeout(Duration::from_millis(2000)))
        .await
        .unwrap()
        .0;
    let c = claim_one(&h.svc, "slow").await.unwrap();
    for _ in 0..5 {
        h.clock.advance(Duration::from_millis(500));
        let hb = h.svc.heartbeat(&system(), &j.id, &c.lease_id).await.unwrap();
        assert!(!hb.cancel_requested);
        assert!(
            claim_one(&h.svc, "thief").await.is_none(),
            "the job stays leased while heartbeats arrive"
        );
    }
    assert_eq!(
        h.svc
            .complete(&system(), &j.id, &c.lease_id, None)
            .await
            .unwrap()
            .status,
        JobStatus::Succeeded
    );
}

#[tokio::test]
async fn cancellation() {
    let h = harness().await;
    // Queued: cancelled at once.
    let q = h.svc.enqueue(&system(), job("k")).await.unwrap().0;
    assert_eq!(
        h.svc.cancel(&system(), &q.id).await.unwrap().status,
        JobStatus::Cancelled
    );
    assert!(claim_one(&h.svc, "w").await.is_none());
    let err = h.svc.cancel(&system(), &q.id).await.unwrap_err();
    assert_eq!((err.code(), err.kind()), ("job_finished", ErrorKind::Conflict));
    h.svc.delete(&system(), &q.id).await.unwrap();
    assert_eq!(h.svc.get(&system(), &q.id).await.unwrap_err().code(), "job_not_found");

    // Running: cooperative. The worker sees it on the next heartbeat and fails the job.
    let r = h.svc.enqueue(&system(), job("k")).await.unwrap().0;
    let c = claim_one(&h.svc, "w").await.unwrap();
    let flagged = h.svc.cancel(&system(), &r.id).await.unwrap();
    assert_eq!((flagged.status, flagged.cancel_requested), (JobStatus::Running, true));
    assert_eq!(h.svc.delete(&system(), &r.id).await.unwrap_err().code(), "job_active");
    assert!(
        h.svc
            .heartbeat(&system(), &r.id, &c.lease_id)
            .await
            .unwrap()
            .cancel_requested
    );
    let end = h
        .svc
        .fail(&system(), &r.id, &c.lease_id, "stopped", true)
        .await
        .unwrap();
    assert_eq!(end.status, JobStatus::Cancelled, "not retried");

    // Running and the worker dies: the reaper cancels instead of re-queueing.
    let d = h
        .svc
        .enqueue(&system(), job("k").visibility_timeout(Duration::from_millis(600)))
        .await
        .unwrap()
        .0;
    claim_one(&h.svc, "w").await.unwrap();
    h.svc.cancel(&system(), &d.id).await.unwrap();
    h.clock.advance(Duration::from_millis(900));
    assert_eq!(h.svc.reap_expired(&system()).await.unwrap(), 1);
    assert_eq!(h.svc.get(&system(), &d.id).await.unwrap().status, JobStatus::Cancelled);

    // A cancelled job can be retried.
    assert_eq!(h.svc.retry(&system(), &d.id).await.unwrap().status, JobStatus::Queued);
}

#[tokio::test]
async fn claims_respect_queue_and_kind_filters_and_listings_filter_and_page() {
    let h = harness().await;
    h.svc.enqueue(&system(), job("a").queue("q1")).await.unwrap();
    h.svc.enqueue(&system(), job("b").queue("q2")).await.unwrap();
    h.svc.enqueue(&system(), job("c").queue("q2")).await.unwrap();

    let only_c = h
        .svc
        .claim(
            &system(),
            ClaimRequest {
                worker_id: "w".into(),
                kinds: vec!["c".into()],
                max: Some(5),
                ..ClaimRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(only_c.len(), 1);
    assert_eq!(only_c[0].job.kind, "c");
    let q2 = h
        .svc
        .claim(
            &system(),
            ClaimRequest {
                worker_id: "w".into(),
                queues: vec!["q2".into()],
                max: Some(5),
                ..ClaimRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(q2.len(), 1);
    assert_eq!(q2[0].job.kind, "b");
    assert_eq!(
        h.svc.claim(&system(), claim_req("w", 5)).await.unwrap()[0].job.kind,
        "a"
    );

    let running = h
        .svc
        .list(
            &system(),
            JobFilter {
                status: Some(JobStatus::Running),
                ..JobFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(running.total, 3);
    let page = h
        .svc
        .list(
            &system(),
            JobFilter {
                limit: Some(2),
                offset: Some(1),
                ..JobFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!((page.items.len(), page.total), (2, 3));
    let by_queue = h
        .svc
        .list(
            &system(),
            JobFilter {
                queue: Some("q2".into()),
                ..JobFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(by_queue.total, 2);

    let stats = h.svc.stats(&system()).await.unwrap();
    assert_eq!(stats.totals["running"], 3);
    assert_eq!(stats.totals["queued"], 0);
    assert_eq!(
        stats
            .queues
            .iter()
            .map(|q| (q.queue.as_str(), q.running))
            .collect::<Vec<_>>(),
        [("q1", 1), ("q2", 2)]
    );

    let bad = h.svc.claim(&system(), claim_req("w", 0)).await.unwrap_err();
    assert_eq!(bad.code(), "invalid_claim");
}

#[tokio::test]
async fn workers_are_tracked() {
    let h = harness().await;
    let w = h
        .svc
        .register_worker(
            &system(),
            NewWorker {
                name: Some("mailer".into()),
                queues: vec!["mail".into()],
                concurrency: Some(4),
                ..NewWorker::default()
            },
        )
        .await
        .unwrap();
    assert!(w.id.starts_with("w-"));
    assert!(w.online);
    claim_one(&h.svc, "ad-hoc").await;
    let ids: Vec<String> = h
        .svc
        .workers(&system())
        .await
        .unwrap()
        .into_iter()
        .map(|w| w.id)
        .collect();
    assert!(ids.contains(&w.id) && ids.contains(&"ad-hoc".to_owned()), "{ids:?}");
}

#[tokio::test]
async fn cron_schedules_fire_once_per_due_time() {
    let h = harness().await;
    let s = h
        .svc
        .create_schedule(
            &system(),
            NewSchedule {
                name: "tick".into(),
                cron: "* * * * * *".into(),
                kind: "tick".into(),
                payload: json!({"x": 1}),
                ..NewSchedule::default()
            },
        )
        .await
        .unwrap();
    assert!(s.enabled);
    assert!(ms(&s.next_run_at) > h.svc.now() - 1);
    assert_eq!(h.svc.fire_due_schedules(&system()).await.unwrap(), 0, "not due yet");

    h.clock.advance(Duration::from_millis(1200));
    // Two racing firers (think two processes) enqueue one job between them.
    let sys = system();
    let (a, b) = tokio::join!(h.svc.fire_due_schedules(&sys), h.svc.fire_due_schedules(&sys));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a + b, 1, "racing firers enqueue one job between them");

    let jobs = h
        .svc
        .list(
            &system(),
            JobFilter {
                kind: Some("tick".into()),
                ..JobFilter::default()
            },
        )
        .await
        .unwrap();
    assert!(jobs.total >= 1);
    let keys: std::collections::HashSet<_> = jobs.items.iter().map(|j| j.idempotency_key.clone().unwrap()).collect();
    assert_eq!(keys.len(), jobs.items.len(), "one job per due time");
    let jobs = h
        .svc
        .list(
            &system(),
            JobFilter {
                kind: Some("tick".into()),
                limit: Some(1),
                ..JobFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(jobs.items[0].schedule_id.as_deref(), Some(s.id.as_str()));
    assert_eq!(jobs.items[0].payload, json!({"x": 1}));
    assert_eq!(jobs.items[0].created_by, "schedule:tick");
    let after = h.svc.schedule(&system(), &s.id).await.unwrap();
    assert_eq!(after.last_job_id.as_deref(), Some(jobs.items[0].id.as_str()));
    assert!(ms(&after.next_run_at) > h.svc.now() - 1);

    // Disabled schedules stay quiet; enabling again does not replay the gap.
    h.svc
        .update_schedule(
            &system(),
            &s.id,
            SchedulePatch {
                enabled: Some(false),
                ..SchedulePatch::default()
            },
        )
        .await
        .unwrap();
    h.clock.advance(Duration::from_millis(1200));
    assert_eq!(h.svc.fire_due_schedules(&system()).await.unwrap(), 0);
    let on = h
        .svc
        .update_schedule(
            &system(),
            &s.id,
            SchedulePatch {
                enabled: Some(true),
                ..SchedulePatch::default()
            },
        )
        .await
        .unwrap();
    assert!(ms(&on.next_run_at) >= h.svc.now());

    h.svc.delete_schedule(&system(), &s.id).await.unwrap();
    assert_eq!(
        h.svc.schedule(&system(), &s.id).await.unwrap_err().code(),
        "schedule_not_found"
    );
    // The job it enqueued stays, detached from the schedule.
    assert_eq!(h.svc.list(&system(), JobFilter::default()).await.unwrap().total, 1);
}

#[tokio::test]
async fn missed_firings_collapse_into_one_and_bad_schedules_are_rejected() {
    let h = harness().await;
    let s = h
        .svc
        .create_schedule(
            &system(),
            NewSchedule {
                name: "every-minute".into(),
                cron: "* * * * *".into(),
                kind: "k".into(),
                ..NewSchedule::default()
            },
        )
        .await
        .unwrap();
    // The server was "down" for ten minutes.
    let mut tx = forgebase_data::db::Backend::begin(&h.db, forgebase_data::db::TxMode::Normal)
        .await
        .unwrap();
    tx.execute(&forgebase_data::db::Statement::new(
        "UPDATE _forgebase_job_schedules SET next_run_at = ?1 WHERE id = ?2",
        vec![(h.svc.now() - 600_000).into(), s.id.as_str().into()],
    ))
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(h.svc.fire_due_schedules(&system()).await.unwrap(), 1);
    assert_eq!(h.svc.fire_due_schedules(&system()).await.unwrap(), 0);
    assert!(ms(&h.svc.schedule(&system(), &s.id).await.unwrap().next_run_at) > h.svc.now());

    let dup = h
        .svc
        .create_schedule(
            &system(),
            NewSchedule {
                name: "every-minute".into(),
                cron: "* * * * *".into(),
                kind: "k".into(),
                ..NewSchedule::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!((dup.code(), dup.kind()), ("schedule_exists", ErrorKind::Conflict));
    for bad in ["", "* * *", "61 * * * *", "0 0 30 2 *"] {
        let err = h
            .svc
            .create_schedule(
                &system(),
                NewSchedule {
                    name: "bad".into(),
                    cron: bad.into(),
                    kind: "k".into(),
                    ..NewSchedule::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(err.code(), "invalid_cron", "{bad:?}");
    }
    let err = h
        .svc
        .update_schedule(
            &system(),
            &s.id,
            SchedulePatch {
                cron: Some("nope".into()),
                ..SchedulePatch::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_cron");
}

#[tokio::test]
async fn purge_removes_old_finished_jobs_but_never_the_dead_letter_queue() {
    let h = harness().await;
    let ok = h.svc.enqueue(&system(), job("k")).await.unwrap().0;
    let bad = h.svc.enqueue(&system(), job("k")).await.unwrap().0;
    let c = h.svc.claim(&system(), claim_req("w", 2)).await.unwrap();
    for claimed in c {
        if claimed.job.id == ok.id {
            h.svc
                .complete(&system(), &ok.id, &claimed.lease_id, None)
                .await
                .unwrap();
        } else {
            h.svc
                .fail(&system(), &bad.id, &claimed.lease_id, "x", false)
                .await
                .unwrap();
        }
    }
    assert_eq!(
        h.svc.purge_finished(&system(), h.svc.now() - 10_000).await.unwrap(),
        0,
        "too recent"
    );
    assert_eq!(h.svc.purge_finished(&system(), h.svc.now() + 10_000).await.unwrap(), 1);
    assert_eq!(h.svc.get(&system(), &ok.id).await.unwrap_err().code(), "job_not_found");
    assert_eq!(h.svc.get(&system(), &bad.id).await.unwrap().status, JobStatus::Dead);
    assert_eq!(h.svc.events(&system(), &bad.id).await.unwrap().len(), 3);
}

#[tokio::test]
async fn everything_is_authorised_through_the_data_core() {
    let h = harness().await;
    let anon = AccessContext::anonymous();
    let user = AccessContext::user("users", "u1");
    let admin = AccessContext::admin("root");
    let j = h.svc.enqueue(&admin, job("k")).await.unwrap().0;

    for (who, code, kind) in [
        (&anon, "unauthenticated", ErrorKind::Unauthenticated),
        (&user, "forbidden", ErrorKind::Forbidden),
    ] {
        let errs = [
            h.svc.enqueue(who, job("k")).await.map(|_| ()),
            h.svc.get(who, &j.id).await.map(|_| ()),
            h.svc.list(who, JobFilter::default()).await.map(|_| ()),
            h.svc.events(who, &j.id).await.map(|_| ()),
            h.svc.stats(who).await.map(|_| ()),
            h.svc.cancel(who, &j.id).await.map(|_| ()),
            h.svc.retry(who, &j.id).await.map(|_| ()),
            h.svc.delete(who, &j.id).await,
            h.svc.claim(who, claim_req("w", 1)).await.map(|_| ()),
            h.svc.heartbeat(who, &j.id, "l").await.map(|_| ()),
            h.svc.complete(who, &j.id, "l", None).await.map(|_| ()),
            h.svc.fail(who, &j.id, "l", "e", true).await.map(|_| ()),
            h.svc.schedules(who).await.map(|_| ()),
            h.svc.workers(who).await.map(|_| ()),
            h.svc.fire_due_schedules(who).await.map(|_| ()),
            h.svc.reap_expired(who).await.map(|_| ()),
            h.svc.purge_finished(who, 0).await.map(|_| ()),
            h.svc.register_worker(who, NewWorker::default()).await.map(|_| ()),
        ];
        for (i, e) in errs.into_iter().enumerate() {
            let e = e.expect_err(&format!("operation {i} must be denied"));
            assert_eq!((e.code(), e.kind()), (code, kind), "operation {i}");
        }
    }
    // Nothing was touched by the denied calls.
    assert_eq!(h.svc.get(&admin, &j.id).await.unwrap().status, JobStatus::Queued);

    // A worker-only principal can work but not enqueue or manage.
    let h2 = harness_with(Arc::new(Only(vec![Action::WorkJobs]))).await;
    let worker = AccessContext::user("svc", "w1");
    h2.svc.enqueue(&AccessContext::admin("root"), job("k")).await.unwrap();
    let claimed = h2.svc.claim(&worker, claim_req("w", 1)).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(h2.svc.enqueue(&worker, job("k")).await.unwrap_err().code(), "forbidden");
    assert_eq!(
        h2.svc.cancel(&worker, &claimed[0].job.id).await.unwrap_err().code(),
        "forbidden"
    );
    assert_eq!(
        h2.svc.list(&worker, JobFilter::default()).await.unwrap_err().code(),
        "forbidden"
    );
    h2.svc
        .complete(&worker, &claimed[0].job.id, &claimed[0].lease_id, None)
        .await
        .unwrap();
}
