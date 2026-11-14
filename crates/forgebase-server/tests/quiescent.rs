//! The quiescent start mode of restore drills (ADR-0041): the server answers
//! requests on the restored data but nothing acts on its own. A control run
//! of the ordinary start proves each probe can see what it looks for.
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use forgebase_core::Config;
use forgebase_data::AccessContext;
use forgebase_jobs::{JobContext, JobRegistry, JobStatus, NewJob, NewSchedule, PoolConfig};
use forgebase_queue::{AttemptQuery, NewMessage, NewSubscription, NewTopic, PushConfig, PushKind};
use forgebase_server::{RunningServer, start_quiescent_with_jobs, start_with_jobs};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn config(dir: &Path) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg.queue.poll_interval_ms = 50;
    // `FORGEBASE_TEST_BACKEND=postgres` (with `FORGEBASE_PG_ENGINE_DIR`) runs it on the bundled engine.
    if std::env::var("FORGEBASE_TEST_BACKEND").as_deref() == Ok("postgres") {
        cfg.database.backend = forgebase_core::config::DatabaseBackend::Postgres;
    }
    cfg
}

struct Probe {
    runs: Arc<AtomicUsize>,
    registry: JobRegistry,
}

fn probe() -> Probe {
    let runs = Arc::new(AtomicUsize::new(0));
    let r = runs.clone();
    let registry = JobRegistry::new().register("probe.tick", move |_c: JobContext| {
        let r = r.clone();
        async move {
            r.fetch_add(1, Ordering::SeqCst);
            Ok(json!({}))
        }
    });
    Probe { runs, registry }
}

/// Stores a one-second schedule, a queued job and a push subscription with a
/// message waiting; returns the job id.
async fn seed(server: &RunningServer) -> String {
    let sys = AccessContext::system();
    let jobs = server.jobs();
    jobs.create_schedule(
        &sys,
        NewSchedule {
            name: "every-second".into(),
            cron: "* * * * * *".into(),
            kind: "probe.tick".into(),
            queue: None,
            payload: json!({}),
            priority: None,
            max_attempts: None,
            backoff_base_ms: None,
            backoff_max_ms: None,
            visibility_timeout_ms: None,
            enabled: Some(true),
        },
    )
    .await
    .unwrap();
    let (job, _) = jobs
        .enqueue(&sys, NewJob::new("probe.tick", json!({"direct": true})))
        .await
        .unwrap();
    let q = server.queue().expect("queue on by default");
    q.create_topic(
        &sys,
        NewTopic {
            name: "events".into(),
            ..NewTopic::default()
        },
    )
    .await
    .unwrap();
    q.create_subscription(
        &sys,
        NewSubscription {
            name: "hook".into(),
            topic: "events".into(),
            backoff_base_ms: Some(0),
            push: Some(PushConfig {
                // Delivery to a function needs no network: an attempt (it fails: there is no such
                // function) is what shows that a push was tried.
                kind: PushKind::Function,
                endpoint: "no-such-function".to_owned(),
            }),
            ..NewSubscription::default()
        },
    )
    .await
    .unwrap();
    q.publish(&sys, "events", vec![NewMessage::new(json!({"n": 1}))])
        .await
        .unwrap();
    job.id
}

async fn push_attempts(server: &RunningServer) -> usize {
    server
        .queue()
        .unwrap()
        .push_attempts(
            &AccessContext::system(),
            "hook",
            AttemptQuery {
                limit: 100,
                ..AttemptQuery::default()
            },
        )
        .await
        .unwrap()
        .items
        .len()
}

async fn http_get(server: &RunningServer, path: &str) -> u16 {
    let mut s = tokio::net::TcpStream::connect(server.local_addr()).await.unwrap();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: forgebase\r\nConnection: close\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0)
}

/// Not one thing fires in a quiescent server: no scheduled job, no queued
/// job, no push delivery. The same data in an ordinary server does.
#[tokio::test]
async fn nothing_fires_in_a_quiescent_server_and_everything_does_in_an_ordinary_one() {
    // Control: the ordinary start runs the schedule, the job and the push.
    let tmp = tempfile::tempdir().unwrap();
    let p = probe();
    let pool = PoolConfig {
        poll_interval: Duration::from_millis(20),
        maintenance_interval: Duration::from_millis(20),
        ..PoolConfig::default()
    };
    let server = start_with_jobs(&config(tmp.path()), p.registry.clone(), pool)
        .await
        .unwrap();
    seed(&server).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let control_runs = p.runs.load(Ordering::SeqCst);
    let control_attempts = push_attempts(&server).await;
    server.shutdown().await.unwrap();
    assert!(
        control_runs >= 2,
        "the control run fires the schedule and the job: {control_runs}"
    );
    assert!(control_attempts > 0, "the control run tries the push");

    // The same, quiescent.
    let tmp = tempfile::tempdir().unwrap();
    let p = probe();
    let server = start_quiescent_with_jobs(&config(tmp.path()), p.registry.clone())
        .await
        .unwrap();
    let job_id = seed(&server).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(p.runs.load(Ordering::SeqCst), 0, "no job handler ran");
    let job = server.jobs().get(&AccessContext::system(), &job_id).await.unwrap();
    assert_eq!(job.status, JobStatus::Queued, "the job was not claimed");
    assert_eq!(job.attempts, 0);
    let all = server
        .jobs()
        .list(&AccessContext::system(), forgebase_jobs::JobFilter::default())
        .await
        .unwrap();
    assert_eq!(all.total, 1, "the schedule enqueued nothing");
    assert_eq!(push_attempts(&server).await, 0, "no push attempt was made");
    // It still serves.
    assert_eq!(http_get(&server, "/health").await, 200);
    server.shutdown().await.unwrap();
}
