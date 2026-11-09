//! Live tests for long-running tasks: progress, checkpoints, queues and workflows (FB-64).

mod common;

use std::time::Duration;

use common::Server;
use forgebase_sdk::generated::{NewJob, QueueSettings};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn progress_checkpoint_attempts_and_result() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let jobs = admin.jobs();

    let job = jobs
        .enqueue(&NewJob {
            kind: "encode".into(),
            queue: Some("video".into()),
            timeout_ms: Some(60_000),
            max_attempts: Some(3),
            ..NewJob::default()
        })
        .await
        .expect("enqueue");
    let mut watch = jobs.watch(&job.id).await.expect("watch");

    let claim = jobs.claim("w1", &["video"], &[], Some(1)).await.expect("claim");
    let lease = claim.jobs[0].lease_id.clone();
    jobs.report_progress(&job.id, &lease, Some(25.0), Some("decoding"), Some("frame 100/400"))
        .await
        .expect("progress");
    let update = loop {
        let u = watch
            .next_timeout(Duration::from_secs(10))
            .await
            .expect("watch")
            .expect("a progress update");
        if u.progress
            .as_ref()
            .is_some_and(|p| p.stage.as_deref() == Some("decoding"))
        {
            break u;
        }
    };
    assert_eq!(update.job_id, job.id);
    assert_eq!(update.progress.as_ref().and_then(|p| p.percent), Some(25.0));

    // The stored progress is on the job too (what a late subscriber reads first).
    let now = jobs.get(&job.id).await.expect("get");
    assert_eq!(now.progress.as_ref().and_then(|p| p.stage.as_deref()), Some("decoding"));

    jobs.save_checkpoint(&job.id, &lease, json!({"frame": 100}))
        .await
        .expect("checkpoint");
    assert_eq!(
        jobs.checkpoint(&job.id).await.expect("read checkpoint").checkpoint,
        Some(json!({"frame": 100}))
    );

    // The attempt fails (retryable): the checkpoint survives and the next claim is handed it.
    jobs.fail(&job.id, &lease, "worker crashed", true).await.expect("fail");
    let again = loop {
        let c = jobs.claim("w2", &["video"], &[], Some(1)).await.expect("claim again");
        if let Some(j) = c.jobs.into_iter().next() {
            break j;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(
        again.checkpoint,
        Some(json!({"frame": 100})),
        "the next attempt resumes from the checkpoint"
    );
    jobs.complete(&job.id, &again.lease_id, Some(json!({"frames": 400})))
        .await
        .expect("complete");

    let attempts = jobs.attempts(&job.id).await.expect("attempts");
    assert_eq!(attempts.len(), 2);
    assert!(attempts[1].from_checkpoint);
    let result = jobs.result(&job.id).await.expect("result");
    assert_eq!(result.result, Some(json!({"frames": 400})));
}

#[tokio::test(flavor = "multi_thread")]
async fn named_queues_can_be_paused_and_limited() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let jobs = admin.jobs();

    let q = jobs
        .set_queue(
            "mail",
            &QueueSettings {
                concurrency: Some(1),
                ..QueueSettings::default()
            },
        )
        .await
        .expect("set queue");
    assert_eq!((q.name.as_str(), q.concurrency), ("mail", Some(1)));
    jobs.enqueue(&NewJob {
        kind: "send".into(),
        queue: Some("mail".into()),
        ..NewJob::default()
    })
    .await
    .expect("enqueue");
    jobs.enqueue(&NewJob {
        kind: "send".into(),
        queue: Some("mail".into()),
        ..NewJob::default()
    })
    .await
    .expect("enqueue");

    // Concurrency 1: a second claim gets nothing while the first job runs.
    let first = jobs.claim("w", &["mail"], &[], Some(2)).await.expect("claim");
    assert_eq!(first.jobs.len(), 1);
    assert!(
        jobs.claim("w", &["mail"], &[], Some(1))
            .await
            .expect("claim")
            .jobs
            .is_empty()
    );

    assert!(jobs.pause_queue("mail").await.expect("pause").paused);
    jobs.complete(&first.jobs[0].job.id, &first.jobs[0].lease_id, None)
        .await
        .expect("complete");
    assert!(
        jobs.claim("w", &["mail"], &[], Some(1))
            .await
            .expect("claim")
            .jobs
            .is_empty(),
        "paused queues hand out nothing"
    );
    assert!(!jobs.resume_queue("mail").await.expect("resume").paused);
    assert_eq!(
        jobs.claim("w", &["mail"], &[], Some(1))
            .await
            .expect("claim")
            .jobs
            .len(),
        1
    );

    assert!(jobs.queues().await.expect("queues").iter().any(|q| q.name == "mail"));
    assert_eq!(jobs.queue("mail").await.expect("queue").name, "mail");
    jobs.delete_queue("mail").await.expect("delete settings");
}

#[tokio::test(flavor = "multi_thread")]
async fn workflows_run_steps_in_dependency_order() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let jobs = admin.jobs();

    let wf: forgebase_sdk::generated::NewWorkflow = serde_json::from_value(json!({
        "name": "etl",
        "steps": [
            {"name": "extract", "kind": "extract"},
            {"name": "load", "kind": "load", "depends_on": ["extract"]}
        ]
    }))
    .expect("workflow");
    let created = jobs.create_workflow(&wf).await.expect("create workflow");
    let id = created.id.clone();

    // Only the first step is runnable; the second waits for it.
    let c1 = jobs.claim("w", &[], &[], Some(5)).await.expect("claim");
    assert_eq!(c1.jobs.len(), 1);
    assert_eq!(c1.jobs[0].job.kind, "extract");
    jobs.complete(&c1.jobs[0].job.id, &c1.jobs[0].lease_id, Some(json!({"rows": 3})))
        .await
        .expect("complete extract");
    let c2 = jobs.claim("w", &[], &[], Some(5)).await.expect("claim");
    assert_eq!(c2.jobs.len(), 1);
    assert_eq!(c2.jobs[0].job.kind, "load");
    jobs.complete(&c2.jobs[0].job.id, &c2.jobs[0].lease_id, None)
        .await
        .expect("complete load");

    let done = jobs.workflow(&id).await.expect("workflow");
    assert_eq!(format!("{:?}", done.status), "Succeeded", "{done:?}");
    assert!(
        jobs.workflows(Some("succeeded"), None, None)
            .await
            .expect("workflows")
            .total
            >= 1
    );
}
