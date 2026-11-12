//! Jobs through the real server lifecycle: the worker pool starts and stops
//! with the server, and work survives restarts (SPEC §14, §27 #19).
//! These tests bind a local port (`127.0.0.1:0`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use forgebase_core::Config;
use forgebase_data::AccessContext;
use forgebase_jobs::{JobContext, JobRegistry, JobStatus, NewJob, NewSchedule, PoolConfig};
use forgebase_server::{RunningServer, start_with_jobs};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::sleep;

fn config(dir: &Path) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg
}

fn pool(grace_ms: u64) -> PoolConfig {
    PoolConfig {
        poll_interval: Duration::from_millis(20),
        maintenance_interval: Duration::from_millis(20),
        shutdown_grace: Duration::from_millis(grace_ms),
        ..PoolConfig::default()
    }
}

async fn wait<T, F: std::future::Future<Output = Option<T>>>(secs: u64, mut f: impl FnMut() -> F) -> T {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out");
        sleep(Duration::from_millis(10)).await;
    }
}

async fn status(server: &RunningServer, id: &str) -> JobStatus {
    server.jobs().get(&AccessContext::system(), id).await.unwrap().status
}

/// A bare HTTP/1.1 request; returns the status code and the body.
async fn http(addr: SocketAddr, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: forgebase\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, out.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned())
}

/// Stop the server in the middle of a job, start it again on the same data
/// directory: the job is not lost and completes exactly once.
#[tokio::test]
async fn a_job_in_flight_survives_a_server_restart_and_completes_exactly_once() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let sys = AccessContext::system();

    // First run: the handler starts and never finishes.
    let started = Arc::new(AtomicUsize::new(0));
    let finished_first = Arc::new(AtomicUsize::new(0));
    let (s, f) = (started.clone(), finished_first.clone());
    let first = JobRegistry::new().register("report", move |_c: JobContext| {
        let (s, f) = (s.clone(), f.clone());
        async move {
            s.fetch_add(1, Ordering::SeqCst);
            sleep(Duration::from_secs(3600)).await;
            f.fetch_add(1, Ordering::SeqCst);
            Ok(json!("first run"))
        }
    });
    let server = start_with_jobs(&cfg, first, pool(100)).await.unwrap();
    let (job, _) = server
        .jobs()
        .enqueue(&sys, NewJob::new("report", json!({"quarter": "Q3"})))
        .await
        .unwrap();
    wait(60, || async { (started.load(Ordering::SeqCst) == 1).then_some(()) }).await;
    assert_eq!(status(&server, &job.id).await, JobStatus::Running);
    server.shutdown().await.unwrap(); // graceful shutdown mid-job

    // Second run on the same data directory.
    let completed = Arc::new(AtomicUsize::new(0));
    let c = completed.clone();
    let second = JobRegistry::new().register("report", move |ctx: JobContext| {
        c.fetch_add(1, Ordering::SeqCst);
        async move { Ok(json!({"quarter": ctx.payload()["quarter"], "attempt": ctx.attempt()})) }
    });
    let server = start_with_jobs(&cfg, second, pool(100)).await.unwrap();
    let done = wait(60, || async {
        let j = server.jobs().get(&sys, &job.id).await.unwrap();
        (j.status == JobStatus::Succeeded).then_some(j)
    })
    .await;
    sleep(Duration::from_millis(300)).await; // a duplicate run would show up here
    assert_eq!(
        done.result,
        Some(json!({"quarter": "Q3", "attempt": 1})),
        "released, so the attempt was not consumed"
    );
    assert_eq!(completed.load(Ordering::SeqCst), 1, "completed exactly once");
    assert_eq!(finished_first.load(Ordering::SeqCst), 0);
    let events: Vec<_> = server
        .jobs()
        .events(&sys, &job.id)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.event)
        .collect();
    assert_eq!(events, ["enqueued", "claimed", "released", "claimed", "succeeded"]);
    server.shutdown().await.unwrap();
}

/// Delayed jobs and cron schedules are rows: they are still there after a
/// restart and fire on time. The one real-time smoke test of the job queue
/// (a real clock across two server runs): generous margins, no negative timing checks.
#[tokio::test]
async fn smoke_delayed_jobs_and_schedules_survive_a_restart_real_time() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = config(tmp.path());
    let sys = AccessContext::system();

    // First run has no handlers at all: jobs are stored, not executed.
    let server = start_with_jobs(&cfg, JobRegistry::new(), pool(100)).await.unwrap();
    let (delayed, _) = server
        .jobs()
        .enqueue(&sys, NewJob::new("later", json!(1)).delay(Duration::from_millis(1500)))
        .await
        .unwrap();
    server
        .jobs()
        .create_schedule(
            &sys,
            NewSchedule {
                name: "tick".into(),
                cron: "* * * * * *".into(),
                kind: "tick".into(),
                ..NewSchedule::default()
            },
        )
        .await
        .unwrap();
    server.shutdown().await.unwrap();

    let ran_later = Arc::new(AtomicUsize::new(0));
    let ticks = Arc::new(AtomicUsize::new(0));
    let (l, t) = (ran_later.clone(), ticks.clone());
    let registry = JobRegistry::new()
        .register("later", move |_c: JobContext| {
            l.fetch_add(1, Ordering::SeqCst);
            async { Ok(json!("ok")) }
        })
        .register("tick", move |_c: JobContext| {
            t.fetch_add(1, Ordering::SeqCst);
            async { Ok(json!("tock")) }
        });
    let server = start_with_jobs(&cfg, registry, pool(100)).await.unwrap();
    wait(60, || async {
        (status(&server, &delayed.id).await == JobStatus::Succeeded).then_some(())
    })
    .await;
    wait(60, || async { (ticks.load(Ordering::SeqCst) >= 2).then_some(()) }).await;
    assert_eq!(ran_later.load(Ordering::SeqCst), 1);
    server.shutdown().await.unwrap();
}

/// Over a real socket: the jobs API is mounted, documented, and anonymous
/// callers are denied (no auth layer is mounted yet, so everyone is anonymous).
#[tokio::test]
async fn jobs_api_is_mounted_and_denies_anonymous_callers() {
    let tmp = tempfile::tempdir().unwrap();
    let server = start_with_jobs(&config(tmp.path()), JobRegistry::new(), pool(100))
        .await
        .unwrap();
    let addr = server.local_addr();

    for (method, path, body) in [
        ("GET", "/api/v1/jobs", ""),
        ("POST", "/api/v1/jobs", r#"{"kind":"k"}"#),
        ("POST", "/api/v1/jobs/claim", r#"{"worker_id":"w"}"#),
        ("GET", "/api/v1/jobs/schedules", ""),
        ("GET", "/api/v1/jobs/workers", ""),
        ("GET", "/api/v1/jobs/queues", ""),
        ("GET", "/api/v1/jobs/workflows", ""),
        ("POST", "/api/v1/jobs/x1/progress", r#"{"lease_id":"l"}"#),
    ] {
        let (status, text) = http(addr, method, path, body).await;
        assert_eq!(status, 401, "{method} {path}: {text}");
        assert!(text.contains("\"unauthenticated\""), "{text}");
    }
    let (status, doc) = http(addr, "GET", "/api/openapi.json", "").await;
    assert_eq!(status, 200);
    assert!(doc.contains("/api/v1/jobs/claim") && doc.contains("/api/v1/collections"));
    assert_eq!(
        server.jobs().stats(&AccessContext::system()).await.unwrap().totals["queued"],
        0
    );
    server.shutdown().await.unwrap();
}

/// FB-64 through the real server: a checkpoint saved by a worker that died
/// with the server is resumed after the restart, and a result too large for
/// the job row is kept in object storage and read back after another restart.
#[tokio::test]
async fn long_task_resumes_after_restart_and_its_large_result_lives_in_storage() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = config(tmp.path());
    // A 300 KiB result and three server lifecycles are slow on a loaded machine.
    cfg.server.shutdown_timeout_secs = 30;
    let sys = AccessContext::system();

    // First run: an external worker claims the job, checkpoints, then dies with the server.
    // The lease must outlive claim -> checkpoint on a loaded runner (1 s lost it in CI:
    // LeaseLost); the second run reclaims it once it expires, well inside its 180 s wait.
    let server = start_with_jobs(&cfg, JobRegistry::new(), pool(100)).await.unwrap();
    let job = server
        .jobs()
        .enqueue(
            &sys,
            NewJob::new("crunch", json!(null)).visibility_timeout(Duration::from_secs(10)),
        )
        .await
        .unwrap()
        .0;
    let claimed = server
        .jobs()
        .claim(
            &sys,
            forgebase_jobs::ClaimRequest {
                worker_id: "ext".into(),
                max: Some(1),
                ..forgebase_jobs::ClaimRequest::default()
            },
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    server
        .jobs()
        .save_checkpoint(&sys, &job.id, &claimed.lease_id, &json!({"chunk": 3}))
        .await
        .unwrap();
    server.shutdown().await.unwrap();

    // Second run: an in-process handler resumes from the checkpoint.
    let registry = JobRegistry::new().register("crunch", |ctx: JobContext| async move {
        let from = ctx.checkpoint().and_then(|c| c["chunk"].as_i64()).unwrap_or(0);
        Ok(json!({"from": from, "blob": "y".repeat(300 * 1024)}))
    });
    let server = start_with_jobs(&cfg, registry, pool(100)).await.unwrap();
    wait(180, || async {
        (status(&server, &job.id).await == JobStatus::Succeeded).then_some(())
    })
    .await;
    let done = server.jobs().get(&sys, &job.id).await.unwrap();
    assert!(done.result_stored, "a 300 KiB result goes to object storage");
    assert!(done.result.is_none());
    let attempts = server.jobs().attempts(&sys, &job.id).await.unwrap();
    assert_eq!(attempts.len(), 2);
    assert!(attempts[1].from_checkpoint);
    server.shutdown().await.unwrap();

    // Third run: the stored result is still there.
    let server = start_with_jobs(&cfg, JobRegistry::new(), pool(100)).await.unwrap();
    let result = server.jobs().result(&sys, &job.id).await.unwrap().unwrap();
    assert_eq!(result["from"], 3);
    assert_eq!(result["blob"].as_str().map(str::len), Some(300 * 1024));
    server.shutdown().await.unwrap();
}
