//! Long-running tasks (FB-64, addendum §B): progress streamed live, resume
//! from a checkpoint after a killed worker, day-long timeouts on a fake
//! clock, queue concurrency and rate limits under load, pause and resume,
//! workflows (chain, fan-out, fan-in, child jobs, fail and compensate),
//! cooperative cancellation, stored results and attempt history.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next};
use common::{Harness, claim_one, claim_req, harness, harness_with, system, wait_for};
use forgebase_data::schema::Collection;
use forgebase_data::{AccessContext, Action, Authorizer, Decision};
use forgebase_jobs::{
    Clock, FailurePolicy, HandlerError, Job, JobAccess, JobChannelAuthorizer, JobContext, JobRegistry, JobService, JobStatus,
    MemoryResultStore, MemorySink, NewJob, NewStep, NewWorkflow, PoolConfig, ProgressReport, QueueSettings, RatePeriod,
    READ_ALL_KEY, WorkflowStatus, channel, start_pool,
};
use http_body_util::BodyExt;
use serde_json::{Value as Json, json};
use tokio::sync::Notify;
use tower::ServiceExt;

fn fast() -> PoolConfig {
    PoolConfig {
        concurrency: 4,
        poll_interval: Duration::from_millis(20),
        maintenance_interval: Duration::from_millis(20),
        shutdown_grace: Duration::from_secs(5),
        heartbeat_interval: Some(Duration::from_millis(20)),
        ..PoolConfig::default()
    }
}

async fn job_of(svc: &JobService, id: &str) -> Job {
    svc.get(&system(), id).await.unwrap()
}

async fn wait_status(svc: &JobService, id: &str, status: JobStatus) -> Job {
    wait_for(Duration::from_secs(30), || async {
        let j = job_of(svc, id).await;
        (j.status == status).then_some(j)
    })
    .await
}

async fn enqueue(svc: &JobService, new: NewJob) -> Job {
    svc.enqueue(&system(), new).await.unwrap().0
}

// ---------------------------------------------------------------- progress

/// Allows channel subscriptions to every signed-in caller, but reading jobs
/// only to `reader`.
#[derive(Debug)]
struct ReaderOnly;

impl Authorizer for ReaderOnly {
    fn authorize(&self, ctx: &AccessContext, action: Action, _c: Option<&Collection>) -> Decision {
        if ctx.is_privileged() {
            return Decision::Allow;
        }
        match action {
            Action::ChannelSubscribe if ctx.principal != forgebase_data::Principal::Anonymous => Decision::Allow,
            Action::ChannelPublish if ctx.principal != forgebase_data::Principal::Anonymous => Decision::Allow,
            Action::ReadJobs if ctx.actor().contains("reader") => Decision::Allow,
            _ => Decision::Deny,
        }
    }
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn as_user(app: Router, ctx: AccessContext) -> Router {
    app.layer(middleware::from_fn(move |mut req: Request<Body>, next: Next| {
        let ctx = ctx.clone();
        async move {
            req.extensions_mut().insert(ctx);
            next.run(req).await
        }
    }))
}

struct Sse {
    body: Body,
    buf: String,
}

impl Sse {
    async fn next(&mut self) -> Json {
        loop {
            if let Some(pos) = self.buf.find("\n\n") {
                let block: String = self.buf.drain(..pos + 2).collect();
                let data: String = block
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .map(str::trim_start)
                    .collect();
                if data.is_empty() {
                    continue;
                }
                return serde_json::from_str(&data).unwrap();
            }
            let frame = tokio::time::timeout(Duration::from_secs(5), self.body.frame())
                .await
                .expect("no SSE data within 5 s")
                .expect("stream ended")
                .unwrap();
            if let Ok(bytes) = frame.into_data() {
                self.buf.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    }

    /// Next channel message (skips welcome/subscribed frames).
    async fn message(&mut self) -> Json {
        loop {
            let v = self.next().await;
            if v["type"] == "message" {
                return v;
            }
        }
    }
}

async fn sse(app: &Router, channel: &str) -> Result<Sse, StatusCode> {
    let subs = json!([{"id": "j", "channel": channel}]).to_string();
    let req = Request::builder()
        .uri(format!("/sse?subscriptions={}", enc(&subs)))
        .body(Body::empty())
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    if res.status() != StatusCode::OK {
        return Err(res.status());
    }
    Ok(Sse {
        body: res.into_body(),
        buf: String::new(),
    })
}

#[tokio::test]
async fn progress_streams_to_a_realtime_subscriber_and_only_to_readers() {
    // FB-142: the reader may watch jobs it did not create with
    // `engine:read_jobs:all`; the channel authorizer shares the service's state.
    let access = JobAccess::new();
    access.attach_permissions(Arc::new(|ctx: &AccessContext, key: &str| {
        key == READ_ALL_KEY && ctx.actor() == "user:users/reader"
    }));
    let h = harness_with(Arc::new(JobChannelAuthorizer::with_access(
        Arc::new(ReaderOnly),
        access.clone(),
    )))
    .await;
    h.svc.attach_access(access);
    let rt = forgebase_realtime::RealtimeService::start(
        h.data.clone(),
        None,
        forgebase_realtime::RealtimeConfig::default(),
        None,
    )
    .await
    .unwrap();
    h.svc.attach_realtime(rt.clone());
    let job = enqueue(&h.svc, NewJob::new("render", json!({"frames": 100}))).await;
    let ch = channel(&job.id);

    let reader = as_user(
        forgebase_realtime::router(rt.clone()),
        AccessContext::user("users", "reader"),
    );
    let outsider = as_user(
        forgebase_realtime::router(rt.clone()),
        AccessContext::user("users", "outsider"),
    );
    // A caller who may subscribe to channels but not read jobs is refused,
    // and so is a job reader who may not see this job (FB-142).
    assert_eq!(sse(&outsider, &ch).await.err(), Some(StatusCode::FORBIDDEN));
    let other_reader = as_user(
        forgebase_realtime::router(rt.clone()),
        AccessContext::user("users", "reader2"),
    );
    assert_eq!(sse(&other_reader, &ch).await.err(), Some(StatusCode::FORBIDDEN));
    let mut stream = sse(&reader, &ch).await.unwrap();
    assert_eq!(stream.next().await["type"], "welcome");
    assert_eq!(stream.next().await["type"], "subscribed");
    // Nobody but the system may publish on a job channel.
    let forged = rt
        .publish(
            &AccessContext::user("users", "reader"),
            &ch,
            json!({"event": "progress"}),
        )
        .await;
    assert!(forged.is_err(), "forged progress must be refused");

    let claimed = claim_one(&h.svc, "w1").await.unwrap();
    assert_eq!(
        stream.message().await["data"]["status"],
        "running",
        "updates arrive in commit order"
    );
    let hb = h
        .svc
        .progress(
            &system(),
            &job.id,
            &claimed.lease_id,
            ProgressReport::percent(40.0).stage("encode").message("frame 40 of 100"),
        )
        .await
        .unwrap();
    assert!(!hb.cancel_requested);
    let m = stream.message().await;
    assert_eq!(m["channel"], ch.as_str());
    assert_eq!(m["data"]["event"], "progress");
    assert_eq!(m["data"]["progress"]["percent"], 40.0);
    assert_eq!(m["data"]["progress"]["stage"], "encode");
    // Stored on the job too; fields left out keep their value.
    h.svc
        .progress(&system(), &job.id, &claimed.lease_id, ProgressReport::percent(90.0))
        .await
        .unwrap();
    let m = stream.message().await;
    assert_eq!(m["data"]["progress"]["percent"], 90.0);
    assert_eq!(m["data"]["progress"]["stage"], "encode");
    let stored = job_of(&h.svc, &job.id).await.progress.unwrap();
    assert_eq!((stored.percent, stored.stage.as_deref()), (Some(90.0), Some("encode")));

    h.svc
        .complete(&system(), &job.id, &claimed.lease_id, Some(json!({"ok": true})))
        .await
        .unwrap();
    let m = stream.message().await;
    assert_eq!(m["data"]["event"], "status");
    assert_eq!(m["data"]["status"], "succeeded");
    // Out-of-range progress is refused.
    assert!(
        h.svc
            .progress(&system(), &job.id, &claimed.lease_id, ProgressReport::percent(120.0))
            .await
            .is_err()
    );
    rt.shutdown();
}

#[tokio::test]
async fn in_process_handlers_report_progress_through_their_context() {
    let h = harness().await;
    let sink = MemorySink::new();
    h.svc.attach_events(Arc::new(sink.clone()));
    let registry = JobRegistry::new().register("steps", |ctx: JobContext| async move {
        for (i, stage) in ["fetch", "transform", "load"].iter().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let pct = (i as f64 + 1.0) * 100.0 / 3.0;
            ctx.progress(ProgressReport::percent(pct.min(100.0)).stage(*stage))
                .await?;
        }
        Ok(json!("done"))
    });
    let pool = start_pool(h.svc.clone(), registry, fast());
    let job = enqueue(&h.svc, NewJob::new("steps", Json::Null)).await;
    let done = wait_status(&h.svc, &job.id, JobStatus::Succeeded).await;
    assert_eq!(done.progress.unwrap().stage.as_deref(), Some("load"));
    let updates = sink.on(&channel(&job.id));
    let stages: Vec<&str> = updates
        .iter()
        .filter(|u| u["event"] == "progress")
        .filter_map(|u| u["progress"]["stage"].as_str())
        .collect();
    assert_eq!(stages, ["fetch", "transform", "load"]);
    assert!(updates.iter().any(|u| u["status"] == "succeeded"));
    pool.shutdown().await;
}

// ------------------------------------------------------------- checkpoints

#[tokio::test]
async fn a_killed_worker_resumes_from_its_checkpoint() {
    let h = harness().await;
    let processed: Arc<Mutex<Vec<(u8, i64)>>> = Arc::default();
    let stuck = Arc::new(Notify::new());

    let handler = |pool_no: u8, processed: Arc<Mutex<Vec<(u8, i64)>>>, stuck: Option<Arc<Notify>>| {
        move |ctx: JobContext| {
            let (processed, stuck) = (processed.clone(), stuck.clone());
            async move {
                let start = ctx.checkpoint().and_then(|c| c["next"].as_i64()).unwrap_or(0);
                for i in start..10 {
                    processed.lock().unwrap().push((pool_no, i));
                    ctx.save_checkpoint(json!({"next": i + 1})).await?;
                    if i == 4
                        && let Some(s) = &stuck
                    {
                        // The process "hangs" here and is then killed.
                        s.notified().await;
                    }
                }
                Ok::<_, HandlerError>(json!({"resumed_from": start}))
            }
        }
    };
    let first = start_pool(
        h.svc.clone(),
        JobRegistry::new().register("batch", handler(1, processed.clone(), Some(stuck.clone()))),
        fast(),
    );
    let job = enqueue(
        &h.svc,
        NewJob::new("batch", Json::Null).visibility_timeout(Duration::from_secs(30)),
    )
    .await;
    wait_for(Duration::from_secs(30), || async {
        let cp = h.svc.checkpoint(&system(), &job.id).await.unwrap();
        (cp.checkpoint == Some(json!({"next": 5}))).then_some(())
    })
    .await;
    first.crash().await;
    assert_eq!(job_of(&h.svc, &job.id).await.status, JobStatus::Running);

    // The lease runs out; a new process picks the job up from the checkpoint.
    h.clock.advance(Duration::from_secs(31));
    let second = start_pool(
        h.svc.clone(),
        JobRegistry::new().register("batch", handler(2, processed.clone(), None)),
        fast(),
    );
    let done = wait_status(&h.svc, &job.id, JobStatus::Succeeded).await;
    assert_eq!(done.result, Some(json!({"resumed_from": 5})));
    assert_eq!(done.attempts, 2);
    let seen = processed.lock().unwrap().clone();
    let by = |p: u8| {
        seen.iter()
            .filter(|(n, _)| *n == p)
            .map(|(_, i)| *i)
            .collect::<Vec<_>>()
    };
    assert_eq!(by(1), [0, 1, 2, 3, 4]);
    assert_eq!(by(2), [5, 6, 7, 8, 9], "no item ran twice");

    let attempts = h.svc.attempts(&system(), &job.id).await.unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[0].outcome.as_deref(), Some("lease_expired"));
    assert!(!attempts[0].from_checkpoint);
    assert_eq!(attempts[1].outcome.as_deref(), Some("succeeded"));
    assert!(attempts[1].from_checkpoint);
    second.shutdown().await;
}

#[tokio::test]
async fn checkpoints_are_size_limited_and_survive_retry() {
    let h = harness().await;
    let job = enqueue(&h.svc, NewJob::new("k", Json::Null)).await;
    let c = claim_one(&h.svc, "w").await.unwrap();
    assert!(c.checkpoint.is_none());
    let big = json!("x".repeat(forgebase_jobs::MAX_CHECKPOINT_BYTES));
    let err = h
        .svc
        .save_checkpoint(&system(), &job.id, &c.lease_id, &big)
        .await
        .unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&err), "checkpoint_too_large");
    h.svc
        .save_checkpoint(&system(), &job.id, &c.lease_id, &json!({"page": 7}))
        .await
        .unwrap();
    h.svc
        .fail(&system(), &job.id, &c.lease_id, "boom", false)
        .await
        .unwrap();
    // A stale lease cannot overwrite it.
    assert!(
        h.svc
            .save_checkpoint(&system(), &job.id, &c.lease_id, &json!({"page": 0}))
            .await
            .is_err()
    );
    h.svc.retry(&system(), &job.id).await.unwrap();
    let again = claim_one(&h.svc, "w").await.unwrap();
    assert_eq!(again.checkpoint, Some(json!({"page": 7})));
    assert!(again.job.has_checkpoint);
}

// ---------------------------------------------------------------- timeouts

const HOUR: Duration = Duration::from_secs(3600);

#[tokio::test]
async fn a_day_long_timeout_does_not_expire_while_heartbeats_continue() {
    let h = harness().await;
    let job = enqueue(
        &h.svc,
        NewJob::new("long", Json::Null)
            .timeout(Duration::from_secs(86_400))
            .visibility_timeout(HOUR),
    )
    .await;
    assert_eq!(job.timeout_ms, Some(86_400_000));
    let c = claim_one(&h.svc, "w").await.unwrap();
    assert!(c.job.deadline_at.is_some());
    // 23.5 hours of heartbeats every 30 minutes.
    for _ in 0..47 {
        h.clock.advance(HOUR / 2);
        let hb = h.svc.heartbeat(&system(), &job.id, &c.lease_id).await.unwrap();
        assert_eq!(hb.deadline_at, c.job.deadline_at);
        assert_eq!(h.svc.reap_expired(&system()).await.unwrap(), 0);
    }
    assert_eq!(job_of(&h.svc, &job.id).await.status, JobStatus::Running);
    // Past the deadline the attempt times out even though heartbeats continue.
    h.clock.advance(HOUR / 2 + Duration::from_millis(1));
    assert_eq!(h.svc.reap_expired(&system()).await.unwrap(), 1);
    let j = job_of(&h.svc, &job.id).await;
    assert_eq!(j.status, JobStatus::Queued, "retried with backoff");
    assert!(j.last_error.unwrap().contains("timed out"));
    assert!(h.svc.heartbeat(&system(), &job.id, &c.lease_id).await.is_err());
    let attempts = h.svc.attempts(&system(), &job.id).await.unwrap();
    assert_eq!(attempts[0].outcome.as_deref(), Some("timed_out"));
    assert_eq!(attempts[0].duration_ms, 86_400_001);
    // Timeouts are bounded: 1 s to 30 days.
    assert!(
        h.svc
            .enqueue(
                &system(),
                NewJob::new("k", Json::Null).timeout(Duration::from_millis(10))
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn the_pool_renews_leases_for_a_day_then_the_timeout_stops_the_handler() {
    let h = harness().await;
    let stopped = Arc::new(AtomicBool::new(false));
    let s = stopped.clone();
    let registry = JobRegistry::new().register("long", move |ctx: JobContext| {
        let s = s.clone();
        async move {
            let token = ctx.cancel_token();
            token.cancelled().await;
            s.store(true, Ordering::SeqCst);
            Err::<Json, _>(HandlerError::retry("stopped"))
        }
    });
    let pool = start_pool(h.svc.clone(), registry, fast());
    let job = enqueue(
        &h.svc,
        NewJob::new("long", Json::Null)
            .timeout(Duration::from_secs(86_400))
            .visibility_timeout(HOUR)
            .retries(1, Duration::ZERO, Duration::ZERO),
    )
    .await;
    wait_status(&h.svc, &job.id, JobStatus::Running).await;
    // 23.5 hours: move the clock 30 minutes, then wait for the pool's next
    // heartbeat at the new time.
    for _ in 0..47 {
        h.clock.advance(HOUR / 2);
        let want = forgebase_jobs::iso(h.clock.now_ms());
        wait_for(Duration::from_secs(10), || async {
            (job_of(&h.svc, &job.id).await.heartbeat_at.as_deref() == Some(want.as_str())).then_some(())
        })
        .await;
    }
    let j = job_of(&h.svc, &job.id).await;
    assert_eq!(j.status, JobStatus::Running, "the lease never expired");
    assert!(!stopped.load(Ordering::SeqCst));
    h.clock.advance(HOUR);
    let dead = wait_status(&h.svc, &job.id, JobStatus::Dead).await;
    assert!(dead.last_error.unwrap().contains("timed out"));
    wait_for(Duration::from_secs(5), || async {
        stopped.load(Ordering::SeqCst).then_some(())
    })
    .await;
    pool.shutdown().await;
}

// ------------------------------------------------------------------ queues

#[tokio::test]
async fn queue_concurrency_limit_holds_across_workers() {
    let h = harness().await;
    h.svc
        .set_queue(
            &system(),
            "limited",
            QueueSettings {
                concurrency: Some(2),
                ..QueueSettings::default()
            },
        )
        .await
        .unwrap();
    for i in 0..4 {
        enqueue(&h.svc, NewJob::new("k", json!(i)).queue("limited")).await;
    }
    enqueue(&h.svc, NewJob::new("k", json!("free"))).await;
    // One claim may not exceed the limit; the other queue is not starved.
    let a = h.svc.claim(&system(), claim_req("a", 10)).await.unwrap();
    assert_eq!(a.len(), 3);
    assert_eq!(a.iter().filter(|c| c.job.queue == "limited").count(), 2);
    assert!(h.svc.claim(&system(), claim_req("b", 10)).await.unwrap().is_empty());
    let first = a.iter().find(|c| c.job.queue == "limited").unwrap();
    h.svc
        .complete(&system(), &first.job.id, &first.lease_id, None)
        .await
        .unwrap();
    assert_eq!(h.svc.claim(&system(), claim_req("b", 10)).await.unwrap().len(), 1);
    let q = h.svc.queue(&system(), "limited").await.unwrap();
    assert_eq!((q.running, q.queued, q.succeeded, q.concurrency), (2, 1, 1, Some(2)));
}

#[tokio::test]
async fn queue_concurrency_holds_under_load_with_two_pools() {
    let h = harness().await;
    h.svc
        .set_queue(
            &system(),
            "busy",
            QueueSettings {
                concurrency: Some(3),
                ..QueueSettings::default()
            },
        )
        .await
        .unwrap();
    let (now, max) = (Arc::new(AtomicI64::new(0)), Arc::new(AtomicI64::new(0)));
    let make = || {
        let (now, max) = (now.clone(), max.clone());
        JobRegistry::new().register("work", move |_ctx: JobContext| {
            let (now, max) = (now.clone(), max.clone());
            async move {
                let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                max.fetch_max(n, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(15)).await;
                now.fetch_sub(1, Ordering::SeqCst);
                Ok::<_, HandlerError>(Json::Null)
            }
        })
    };
    let p1 = start_pool(
        h.svc.clone(),
        make(),
        PoolConfig {
            concurrency: 8,
            ..fast()
        },
    );
    let p2 = start_pool(
        h.svc.clone(),
        make(),
        PoolConfig {
            concurrency: 8,
            ..fast()
        },
    );
    let mut ids = Vec::new();
    for i in 0..30 {
        ids.push(enqueue(&h.svc, NewJob::new("work", json!(i)).queue("busy")).await.id);
    }
    for id in &ids {
        wait_status(&h.svc, id, JobStatus::Succeeded).await;
    }
    let peak = max.load(Ordering::SeqCst);
    assert!((1..=3).contains(&peak), "peak concurrency {peak}");
    p1.shutdown().await;
    p2.shutdown().await;
}

#[tokio::test]
async fn queue_rate_limit_holds_under_load() {
    let h = harness().await;
    h.svc
        .set_queue(
            &system(),
            "rated",
            QueueSettings {
                rate_limit: Some(5),
                rate_period: Some(RatePeriod::Second),
                ..QueueSettings::default()
            },
        )
        .await
        .unwrap();
    let ran = Arc::new(AtomicUsize::new(0));
    let r = ran.clone();
    let registry = JobRegistry::new().register("tick", move |_ctx: JobContext| {
        let r = r.clone();
        async move {
            r.fetch_add(1, Ordering::SeqCst);
            Ok::<_, HandlerError>(Json::Null)
        }
    });
    let pool = start_pool(
        h.svc.clone(),
        registry,
        PoolConfig {
            concurrency: 20,
            ..fast()
        },
    );
    for i in 0..30 {
        enqueue(&h.svc, NewJob::new("tick", json!(i)).queue("rated")).await;
    }
    // The clock stands still: one window, five jobs, however eager the pool.
    wait_for(Duration::from_secs(10), || async {
        (ran.load(Ordering::SeqCst) == 5).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(ran.load(Ordering::SeqCst), 5);
    h.clock.advance(Duration::from_secs(1));
    wait_for(Duration::from_secs(10), || async {
        (ran.load(Ordering::SeqCst) == 10).then_some(())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(ran.load(Ordering::SeqCst), 10);
    pool.shutdown().await;

    // Per minute, with direct claims.
    h.svc
        .set_queue(
            &system(),
            "slow",
            QueueSettings {
                rate_limit: Some(3),
                rate_period: Some(RatePeriod::Minute),
                ..QueueSettings::default()
            },
        )
        .await
        .unwrap();
    for _ in 0..10 {
        enqueue(&h.svc, NewJob::new("x", Json::Null).queue("slow")).await;
    }
    let only = |w: &str| forgebase_jobs::ClaimRequest {
        queues: vec!["slow".into()],
        ..claim_req(w, 10)
    };
    // The next minute boundary (moving forward keeps the jobs due).
    let minute_start = h.clock.now_ms() - h.clock.now_ms().rem_euclid(60_000) + 60_000;
    h.clock.set(minute_start + 1_000);
    assert_eq!(h.svc.claim(&system(), only("a")).await.unwrap().len(), 3);
    h.clock.set(minute_start + 30_000);
    assert!(h.svc.claim(&system(), only("b")).await.unwrap().is_empty());
    h.clock.set(minute_start + 60_000);
    assert_eq!(h.svc.claim(&system(), only("b")).await.unwrap().len(), 3);
}

#[tokio::test]
async fn paused_queues_hand_out_nothing_until_resumed_and_defaults_apply() {
    let h = harness().await;
    h.svc
        .set_queue(
            &system(),
            "q",
            QueueSettings {
                default_priority: Some(7),
                default_timeout_ms: Some(120_000),
                ..QueueSettings::default()
            },
        )
        .await
        .unwrap();
    let j = enqueue(&h.svc, NewJob::new("k", Json::Null).queue("q")).await;
    assert_eq!((j.priority, j.timeout_ms), (7, Some(120_000)));
    let explicit = enqueue(&h.svc, NewJob::new("k", Json::Null).queue("q").priority(1)).await;
    assert_eq!(explicit.priority, 1);
    let paused = h.svc.pause_queue(&system(), "q").await.unwrap();
    assert!(paused.paused);
    assert!(h.svc.claim(&system(), claim_req("w", 10)).await.unwrap().is_empty());
    // Saving other settings keeps the pause.
    let still = h
        .svc
        .set_queue(
            &system(),
            "q",
            QueueSettings {
                concurrency: Some(10),
                ..QueueSettings::default()
            },
        )
        .await
        .unwrap();
    assert!(still.paused);
    h.svc.resume_queue(&system(), "q").await.unwrap();
    assert_eq!(h.svc.claim(&system(), claim_req("w", 10)).await.unwrap().len(), 2);
    let all = h.svc.queues(&system()).await.unwrap();
    assert!(all.iter().any(|q| q.name == "q" && q.configured && q.running == 2));
    h.svc.delete_queue(&system(), "q").await.unwrap();
    assert!(!h.svc.queue(&system(), "q").await.unwrap().configured);
}

// --------------------------------------------------------------- workflows

#[tokio::test]
async fn fan_in_waits_for_all_parents_and_gets_their_results() {
    let h = harness().await;
    let wf = h
        .svc
        .create_workflow(
            &system(),
            NewWorkflow::new(
                "etl",
                vec![
                    NewStep::new("join", "join", Json::Null).after(["a", "b"]),
                    NewStep::new("root", "root", Json::Null),
                    NewStep::new("a", "part", json!("a")).after(["root"]),
                    NewStep::new("b", "part", json!("b")).after(["root"]),
                ],
            ),
        )
        .await
        .unwrap();
    assert_eq!(wf.workflow.status, WorkflowStatus::Running);
    assert_eq!(wf.steps.len(), 4);
    let join_id = wf
        .steps
        .iter()
        .find(|s| s.name.as_deref() == Some("join"))
        .unwrap()
        .job
        .id
        .clone();
    assert_eq!(job_of(&h.svc, &join_id).await.waiting_for, 2);

    // Only the root is claimable.
    let root = h.svc.claim(&system(), claim_req("w", 10)).await.unwrap();
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].job.step.as_deref(), Some("root"));
    h.svc
        .complete(&system(), &root[0].job.id, &root[0].lease_id, Some(json!(1)))
        .await
        .unwrap();
    // Fan-out: both parts at once, each with the root's result.
    let parts = h.svc.claim(&system(), claim_req("w", 10)).await.unwrap();
    assert_eq!(parts.len(), 2);
    assert!(
        parts
            .iter()
            .all(|p| p.parents.len() == 1 && p.parents[0].result == Some(json!(1)))
    );
    let (a, b) = if parts[0].job.step.as_deref() == Some("a") {
        (&parts[0], &parts[1])
    } else {
        (&parts[1], &parts[0])
    };
    h.svc
        .complete(&system(), &a.job.id, &a.lease_id, Some(json!({"rows": 10})))
        .await
        .unwrap();
    // Fan-in: one parent done is not enough.
    assert!(h.svc.claim(&system(), claim_req("w", 10)).await.unwrap().is_empty());
    assert_eq!(job_of(&h.svc, &join_id).await.waiting_for, 1);
    h.svc
        .complete(&system(), &b.job.id, &b.lease_id, Some(json!({"rows": 20})))
        .await
        .unwrap();
    let join = h.svc.claim(&system(), claim_req("w", 10)).await.unwrap().pop().unwrap();
    assert_eq!(join.job.id, join_id);
    let mut results: Vec<(String, Json)> = join
        .parents
        .iter()
        .map(|p| (p.step.clone().unwrap(), p.result.clone().unwrap()))
        .collect();
    results.sort_by(|x, y| x.0.cmp(&y.0));
    assert_eq!(
        results,
        [
            ("a".to_owned(), json!({"rows": 10})),
            ("b".to_owned(), json!({"rows": 20}))
        ]
    );
    h.svc
        .complete(&system(), &join.job.id, &join.lease_id, None)
        .await
        .unwrap();
    let done = h.svc.workflow(&system(), &wf.workflow.id).await.unwrap();
    assert_eq!(done.workflow.status, WorkflowStatus::Succeeded);
    let join_step = done.steps.iter().find(|s| s.job.id == join_id).unwrap();
    assert_eq!(join_step.depends_on.len(), 2);
    assert_eq!(
        h.svc
            .workflows(&system(), Some(WorkflowStatus::Succeeded), None, None)
            .await
            .unwrap()
            .total,
        1
    );
}

#[tokio::test]
async fn a_chain_passes_results_from_parent_to_child_in_process() {
    let h = harness().await;
    let registry = JobRegistry::new().register("add", |ctx: JobContext| async move {
        let parents = ctx.parent_results().await?;
        let sum: i64 = parents.values().filter_map(Json::as_i64).sum();
        Ok(json!(sum + ctx.payload().as_i64().unwrap_or(0)))
    });
    let pool = start_pool(h.svc.clone(), registry, fast());
    let wf = h
        .svc
        .create_workflow(
            &system(),
            NewWorkflow::new(
                "chain",
                vec![
                    NewStep::new("one", "add", json!(1)),
                    NewStep::new("two", "add", json!(10)).after(["one"]),
                    NewStep::new("three", "add", json!(100)).after(["two"]),
                ],
            ),
        )
        .await
        .unwrap();
    let id = wf.workflow.id.clone();
    let done = wait_for(Duration::from_secs(30), || async {
        let w = h.svc.workflow(&system(), &id).await.unwrap();
        (w.workflow.status == WorkflowStatus::Succeeded).then_some(w)
    })
    .await;
    let three = done.steps.iter().find(|s| s.name.as_deref() == Some("three")).unwrap();
    assert_eq!(three.job.result, Some(json!(111)));
    pool.shutdown().await;
}

#[tokio::test]
async fn child_jobs_join_the_workflow_and_a_follow_up_collects_them() {
    let h = harness().await;
    let registry = JobRegistry::new()
        .register("fan", |ctx: JobContext| async move {
            let mut ids = Vec::new();
            for i in 0..3 {
                ids.push(ctx.spawn_child(NewJob::new("square", json!(i))).await?.id);
            }
            ctx.spawn_child(NewJob::new("sum", Json::Null).depends_on(ids)).await?;
            Ok(json!("spawned"))
        })
        .register("square", |ctx: JobContext| async move {
            let n = ctx.payload().as_i64().unwrap_or(0);
            Ok(json!(n * n))
        })
        .register("sum", |ctx: JobContext| async move {
            let total: i64 = ctx.parent_results().await?.values().filter_map(Json::as_i64).sum();
            Ok(json!(total))
        });
    let pool = start_pool(h.svc.clone(), registry, fast());
    let wf = h
        .svc
        .create_workflow(
            &system(),
            NewWorkflow::new("kids", vec![NewStep::new("fan", "fan", Json::Null)]),
        )
        .await
        .unwrap();
    let id = wf.workflow.id.clone();
    let done = wait_for(Duration::from_secs(30), || async {
        let w = h.svc.workflow(&system(), &id).await.unwrap();
        (w.workflow.status == WorkflowStatus::Succeeded).then_some(w)
    })
    .await;
    assert_eq!(done.steps.len(), 5, "parent, three children, follow-up");
    let fan_id = done.steps[0].job.id.clone();
    assert!(
        done.steps[1..]
            .iter()
            .all(|s| s.job.parent_id.as_deref() == Some(fan_id.as_str()))
    );
    let sum = done.steps.iter().find(|s| s.job.kind == "sum").unwrap();
    assert_eq!(sum.job.result, Some(json!(5)));
    let children = h
        .svc
        .list(
            &system(),
            forgebase_jobs::JobFilter {
                parent_id: Some(fan_id),
                ..forgebase_jobs::JobFilter::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(children.total, 4);
    pool.shutdown().await;
}

#[tokio::test]
async fn a_failed_step_fails_the_workflow_and_cancels_the_rest() {
    let h = harness().await;
    let wf = h
        .svc
        .create_workflow(
            &system(),
            NewWorkflow::new(
                "w",
                vec![
                    NewStep::new("a", "k", Json::Null),
                    NewStep::new("side", "k", Json::Null),
                    NewStep::new("b", "k", Json::Null).after(["a"]),
                ],
            ),
        )
        .await
        .unwrap();
    let claimed = h.svc.claim(&system(), claim_req("w", 10)).await.unwrap();
    assert_eq!(claimed.len(), 2);
    let a = claimed.iter().find(|c| c.job.step.as_deref() == Some("a")).unwrap();
    let side = claimed.iter().find(|c| c.job.step.as_deref() == Some("side")).unwrap();
    h.svc
        .fail(&system(), &a.job.id, &a.lease_id, "bad input", false)
        .await
        .unwrap();
    let w = h.svc.workflow(&system(), &wf.workflow.id).await.unwrap();
    assert_eq!(w.workflow.status, WorkflowStatus::Failed);
    assert_eq!(w.workflow.failed_job_id.as_deref(), Some(a.job.id.as_str()));
    assert!(w.workflow.error.as_deref().unwrap().contains("bad input"));
    let b = w.steps.iter().find(|s| s.name.as_deref() == Some("b")).unwrap();
    assert_eq!(b.job.status, JobStatus::Cancelled);
    // The running sibling is asked to stop.
    let hb = h.svc.heartbeat(&system(), &side.job.id, &side.lease_id).await.unwrap();
    assert!(hb.cancel_requested);
}

#[tokio::test]
async fn a_failed_step_compensates_the_succeeded_ones_newest_first() {
    let h = harness().await;
    let wf = h
        .svc
        .create_workflow(
            &system(),
            NewWorkflow::new(
                "order",
                vec![
                    NewStep::new("reserve", "reserve", Json::Null).compensate("release", json!({"what": "stock"})),
                    NewStep::new("charge", "charge", Json::Null)
                        .after(["reserve"])
                        .compensate("refund", json!({"what": "card"})),
                    NewStep::new("ship", "ship", Json::Null).after(["charge"]),
                ],
            )
            .on_failure(FailurePolicy::Compensate),
        )
        .await
        .unwrap();
    for _ in 0..2 {
        let c = claim_one(&h.svc, "w").await.unwrap();
        h.svc
            .complete(&system(), &c.job.id, &c.lease_id, Some(json!(c.job.kind)))
            .await
            .unwrap();
    }
    let ship = claim_one(&h.svc, "w").await.unwrap();
    assert_eq!(ship.job.kind, "ship");
    h.svc
        .fail(&system(), &ship.job.id, &ship.lease_id, "courier down", false)
        .await
        .unwrap();
    assert_eq!(
        h.svc
            .workflow(&system(), &wf.workflow.id)
            .await
            .unwrap()
            .workflow
            .status,
        WorkflowStatus::Compensating
    );
    // Refund first (newest succeeded step), then release; each gets its step's result.
    let refund = claim_one(&h.svc, "w").await.unwrap();
    assert_eq!(refund.job.kind, "refund");
    assert_eq!(refund.parents[0].result, Some(json!("charge")));
    assert!(claim_one(&h.svc, "w").await.is_none(), "release waits for refund");
    h.svc
        .complete(&system(), &refund.job.id, &refund.lease_id, None)
        .await
        .unwrap();
    let release = claim_one(&h.svc, "w").await.unwrap();
    assert_eq!(release.job.kind, "release");
    assert_eq!(release.job.payload, json!({"what": "stock"}));
    h.svc
        .complete(&system(), &release.job.id, &release.lease_id, None)
        .await
        .unwrap();
    let w = h.svc.workflow(&system(), &wf.workflow.id).await.unwrap();
    assert_eq!(w.workflow.status, WorkflowStatus::Compensated);
    assert_eq!(w.steps.iter().filter(|s| s.role == "compensation").count(), 2);
}

#[tokio::test]
async fn dependencies_outside_workflows_and_bad_graphs() {
    let h = harness().await;
    let a = enqueue(&h.svc, NewJob::new("k", Json::Null)).await;
    let b = enqueue(&h.svc, NewJob::new("k", Json::Null).depends_on([a.id.clone()])).await;
    assert_eq!(b.waiting_for, 1);
    h.svc.cancel(&system(), &a.id).await.unwrap();
    let b = job_of(&h.svc, &b.id).await;
    assert_eq!(b.status, JobStatus::Cancelled);
    assert!(b.last_error.unwrap().contains("dependency"));
    // Unknown or failed parents are refused; retrying needs the parent first.
    assert!(
        h.svc
            .enqueue(&system(), NewJob::new("k", Json::Null).depends_on(["job-nope"]))
            .await
            .is_err()
    );
    assert!(
        h.svc
            .enqueue(&system(), NewJob::new("k", Json::Null).depends_on([a.id.clone()]))
            .await
            .is_err()
    );
    assert!(h.svc.retry(&system(), &b.id).await.is_err());
    let cyclic = NewWorkflow::new(
        "c",
        vec![
            NewStep::new("x", "k", Json::Null).after(["y"]),
            NewStep::new("y", "k", Json::Null).after(["x"]),
        ],
    );
    assert!(h.svc.create_workflow(&system(), cyclic).await.is_err());
    // Cancelling a workflow stops its jobs.
    let wf = h
        .svc
        .create_workflow(
            &system(),
            NewWorkflow::new(
                "w",
                vec![
                    NewStep::new("s", "k", Json::Null),
                    NewStep::new("t", "k", Json::Null).after(["s"]),
                ],
            ),
        )
        .await
        .unwrap();
    let w = h.svc.cancel_workflow(&system(), &wf.workflow.id).await.unwrap();
    assert_eq!(w.workflow.status, WorkflowStatus::Cancelled);
    assert!(w.steps.iter().all(|s| s.job.status == JobStatus::Cancelled));
    assert!(h.svc.cancel_workflow(&system(), &wf.workflow.id).await.is_err());
}

// ------------------------------------------------------------ cancellation

#[tokio::test]
async fn cancellation_reaches_a_running_handler_at_once() {
    let h = harness().await;
    let observed = Arc::new(AtomicBool::new(false));
    let o = observed.clone();
    let registry = JobRegistry::new().register("forever", move |ctx: JobContext| {
        let o = o.clone();
        async move {
            let token = ctx.cancel_token();
            tokio::select! {
                () = token.cancelled() => {
                    o.store(true, Ordering::SeqCst);
                    Err(HandlerError::retry("cancelled by request"))
                }
                () = tokio::time::sleep(Duration::from_secs(120)) => Ok(Json::Null),
            }
        }
    });
    // Heartbeats would take 20 s (a third of 60 s): the signal must not wait for them.
    let pool = start_pool(
        h.svc.clone(),
        registry,
        PoolConfig {
            heartbeat_interval: None,
            ..fast()
        },
    );
    let job = enqueue(&h.svc, NewJob::new("forever", Json::Null)).await;
    wait_status(&h.svc, &job.id, JobStatus::Running).await;
    let t0 = std::time::Instant::now();
    let j = h.svc.cancel(&system(), &job.id).await.unwrap();
    assert!(j.cancel_requested);
    let done = wait_status(&h.svc, &job.id, JobStatus::Cancelled).await;
    assert!(t0.elapsed() < Duration::from_secs(5), "took {:?}", t0.elapsed());
    assert!(observed.load(Ordering::SeqCst));
    assert_eq!(done.last_error.as_deref(), Some("cancelled by request"));
    let attempts = h.svc.attempts(&system(), &job.id).await.unwrap();
    assert_eq!(attempts[0].outcome.as_deref(), Some("cancelled"));
    pool.shutdown().await;
}

// ------------------------------------------------------------------ results

#[tokio::test]
async fn large_results_go_to_the_result_store_and_can_be_fetched() {
    let h = harness().await;
    let big = json!({"blob": "z".repeat(forgebase_jobs::MAX_INLINE_RESULT_BYTES + 10)});
    // Without a store a large result is refused, as before.
    let job = enqueue(&h.svc, NewJob::new("k", Json::Null)).await;
    let c = claim_one(&h.svc, "w").await.unwrap();
    let err = h
        .svc
        .complete(&system(), &job.id, &c.lease_id, Some(big.clone()))
        .await
        .unwrap_err();
    assert_eq!(forgebase_core::ErrorCode::code(&err), "payload_too_large");

    let store = MemoryResultStore::new();
    h.svc.attach_result_store(Arc::new(store.clone()));
    let done = h
        .svc
        .complete(&system(), &job.id, &c.lease_id, Some(big.clone()))
        .await
        .unwrap();
    assert!(done.result_stored);
    assert!(done.result.is_none());
    assert!(done.result_size.unwrap() > i64::try_from(forgebase_jobs::MAX_INLINE_RESULT_BYTES).unwrap());
    assert_eq!(store.keys().len(), 1);
    assert_eq!(h.svc.result(&system(), &job.id).await.unwrap(), Some(big.clone()));
    // Small results stay inline.
    let small = enqueue(&h.svc, NewJob::new("k", Json::Null)).await;
    let c2 = claim_one(&h.svc, "w").await.unwrap();
    let s = h
        .svc
        .complete(&system(), &small.id, &c2.lease_id, Some(json!(42)))
        .await
        .unwrap();
    assert!(!s.result_stored);
    assert_eq!(h.svc.result(&system(), &small.id).await.unwrap(), Some(json!(42)));
    // A child gets a stored parent result through its context.
    let child = enqueue(&h.svc, NewJob::new("k", Json::Null).depends_on([job.id.clone()])).await;
    let registry = JobRegistry::new().register("k", |ctx: JobContext| async move {
        let r = ctx.parent_results().await?;
        Ok(json!(r.values().next().and_then(|v| v["blob"].as_str()).map(str::len)))
    });
    let pool = start_pool(h.svc.clone(), registry, fast());
    let got = wait_status(&h.svc, &child.id, JobStatus::Succeeded).await;
    assert_eq!(got.result, Some(json!(forgebase_jobs::MAX_INLINE_RESULT_BYTES + 10)));
    pool.shutdown().await;
    // Deleting the job deletes the stored object.
    h.svc.delete(&system(), &job.id).await.unwrap();
    assert!(store.keys().is_empty());
    // Unfinished jobs have no result.
    let pending = enqueue(&h.svc, NewJob::new("zz", Json::Null)).await;
    assert!(h.svc.result(&system(), &pending.id).await.is_err());
}

#[tokio::test]
async fn attempt_history_records_timings_and_outcomes() {
    let h: Harness = harness().await;
    let job = enqueue(
        &h.svc,
        NewJob::new("k", Json::Null).retries(3, Duration::from_millis(1000), Duration::from_millis(1000)),
    )
    .await;
    let c = claim_one(&h.svc, "w1").await.unwrap();
    h.clock.advance(Duration::from_millis(250));
    h.svc
        .fail(&system(), &job.id, &c.lease_id, "flaky", true)
        .await
        .unwrap();
    h.clock.advance(Duration::from_secs(2));
    let c = claim_one(&h.svc, "w2").await.unwrap();
    h.clock.advance(Duration::from_millis(400));
    h.svc.complete(&system(), &job.id, &c.lease_id, None).await.unwrap();
    let a = h.svc.attempts(&system(), &job.id).await.unwrap();
    assert_eq!(a.len(), 2);
    assert_eq!(
        (
            a[0].attempt,
            a[0].worker_id.as_deref(),
            a[0].outcome.as_deref(),
            a[0].duration_ms
        ),
        (1, Some("w1"), Some("retry"), 250)
    );
    assert_eq!(a[0].error.as_deref(), Some("flaky"));
    assert_eq!(
        (
            a[1].attempt,
            a[1].worker_id.as_deref(),
            a[1].outcome.as_deref(),
            a[1].duration_ms
        ),
        (2, Some("w2"), Some("succeeded"), 400)
    );
}
