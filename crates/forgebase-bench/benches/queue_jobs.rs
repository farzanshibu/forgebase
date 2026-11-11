//! Durable queue publish/pull/ack and jobs enqueue/claim/complete, through the real server lifecycle.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use forgebase_data::AccessContext;
use forgebase_jobs::{ClaimRequest, NewJob, NewWorker};
use forgebase_queue::{NewMessage, NewSubscription, NewTopic, PullRequest};
use serde_json::json;

fn bench(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let tmp = tempfile::tempdir_in(std::env::var_os("TMPDIR").unwrap_or_else(|| "/tmp".into())).unwrap();
    // FB-148: SQLite or the bundled PostgreSQL (FORGEBASE_TEST_BACKEND).
    let mut cfg = forgebase_bench::config(tmp.path());
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    let server = rt.block_on(forgebase_server::start(&cfg)).unwrap();
    let sys = AccessContext::system();
    let q = server.queue().unwrap().clone();
    let jobs = server.jobs().clone();
    let worker = rt.block_on(async {
        q.create_topic(
            &sys,
            NewTopic {
                name: "bench".into(),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap();
        q.create_subscription(
            &sys,
            NewSubscription {
                name: "bench-sub".into(),
                topic: "bench".into(),
                ..NewSubscription::default()
            },
        )
        .await
        .unwrap();
        jobs.register_worker(
            &sys,
            NewWorker {
                id: Some("bw".into()),
                queues: vec![],
                kinds: vec!["bench".into()],
                ..NewWorker::default()
            },
        )
        .await
        .unwrap()
    });

    // A standing backlog, so the pull bench measures a busy subscription even
    // when run alone (FIX-9: pull cost must not grow with the backlog).
    rt.block_on(async {
        for _ in 0..100 {
            let batch = (0..100).map(|i| NewMessage::new(json!({"n": i}))).collect();
            q.publish(&sys, "bench", batch).await.unwrap();
        }
    });

    let mut g = c.benchmark_group("queue_jobs");
    g.sample_size(30);
    g.throughput(Throughput::Elements(1));
    g.bench_function("queue_publish_1", |b| {
        b.to_async(&rt).iter(|| {
            let (q, sys) = (q.clone(), sys.clone());
            async move {
                q.publish(&sys, "bench", vec![NewMessage::new(json!({"n": 1}))])
                    .await
                    .unwrap();
            }
        });
    });
    g.throughput(Throughput::Elements(100));
    g.bench_function("queue_publish_batch_100", |b| {
        b.to_async(&rt).iter(|| {
            let (q, sys) = (q.clone(), sys.clone());
            async move {
                let batch = (0..100).map(|i| NewMessage::new(json!({"n": i}))).collect();
                q.publish(&sys, "bench", batch).await.unwrap();
            }
        });
    });
    g.throughput(Throughput::Elements(10));
    // Each iteration first publishes 10 messages (not timed), so the standing
    // backlog stays the same size and every timed pull has 10 to claim.
    g.bench_function("queue_pull_ack_10", |b| {
        b.to_async(&rt).iter_custom(|iters| {
            let (q, sys) = (q.clone(), sys.clone());
            async move {
                let mut total = std::time::Duration::ZERO;
                for _ in 0..iters {
                    let batch = (0..10).map(|i| NewMessage::new(json!({"n": i}))).collect();
                    q.publish(&sys, "bench", batch).await.unwrap();
                    let start = std::time::Instant::now();
                    let got = q
                        .pull(
                            &sys,
                            "bench-sub",
                            PullRequest {
                                max_messages: Some(10),
                                ..PullRequest::default()
                            },
                        )
                        .await
                        .unwrap();
                    assert_eq!(got.len(), 10, "pull bench needs a standing backlog");
                    let ids: Vec<String> = got.iter().map(|m| m.ack_id.clone()).collect();
                    q.ack(&sys, "bench-sub", &ids).await.unwrap();
                    total += start.elapsed();
                }
                total
            }
        });
    });
    g.throughput(Throughput::Elements(1));
    g.bench_function("jobs_enqueue", |b| {
        b.to_async(&rt).iter(|| {
            let (jobs, sys) = (jobs.clone(), sys.clone());
            async move {
                jobs.enqueue(
                    &sys,
                    NewJob {
                        kind: "bench".into(),
                        payload: json!({"x": 1}),
                        ..NewJob::default()
                    },
                )
                .await
                .unwrap();
            }
        });
    });
    g.bench_function("jobs_claim_complete", |b| {
        b.to_async(&rt).iter(|| {
            let (jobs, sys, wid) = (jobs.clone(), sys.clone(), worker.id.clone());
            async move {
                let got = jobs
                    .claim(
                        &sys,
                        ClaimRequest {
                            worker_id: wid,
                            kinds: vec!["bench".into()],
                            max: Some(1),
                            ..ClaimRequest::default()
                        },
                    )
                    .await
                    .unwrap();
                for j in got {
                    jobs.complete(&sys, &j.job.id, &j.lease_id, None).await.unwrap();
                }
            }
        });
    });
    g.finish();
    rt.block_on(server.shutdown()).unwrap();
}

criterion_group!(benches, bench);
criterion_main!(benches);
