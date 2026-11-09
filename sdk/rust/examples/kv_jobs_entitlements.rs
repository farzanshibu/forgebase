//! Key-value, jobs and entitlements.
//!
//!     FORGEBASE_URL=http://localhost:8090 FORGEBASE_API_KEY=fb_... cargo run --example kv_jobs_entitlements

use std::time::Duration;

use forgebase_sdk::generated::NewJob;
use forgebase_sdk::{ForgeBase, HasQuery, MeteredRequest, SetOptions};
use serde_json::json;

#[tokio::main]
async fn main() -> forgebase_sdk::Result<()> {
    let url = std::env::var("FORGEBASE_URL").unwrap_or_else(|_| "http://localhost:8090".into());
    let key = std::env::var("FORGEBASE_API_KEY").unwrap_or_default();
    let fb = ForgeBase::builder(url).api_key(key).build()?;

    // KV: a cache entry that expires after a minute.
    let kv = fb.kv("cache");
    kv.set(
        "greeting",
        "hello",
        &SetOptions {
            ttl_ms: Some(60_000),
            ..SetOptions::default()
        },
    )
    .await?;
    println!("{:?}", kv.get_string("greeting").await?);

    // Jobs: enqueue (idempotent by key), then wait for a worker to finish it.
    let jobs = fb.jobs();
    let job = jobs
        .enqueue(&NewJob {
            kind: "email".into(),
            payload: Some(json!({"to": "a@b.co"})),
            idempotency_key: Some("welcome-a@b.co".into()),
            ..NewJob::default()
        })
        .await?;
    println!("queued {} ({:?})", job.id, job.status);
    let done = jobs
        .wait(&job.id, Duration::from_millis(500), Duration::from_secs(30))
        .await?;
    println!("finished: {:?} result={:?}", done.status, done.result);

    // Entitlements: feature gates and metered usage.
    let ent = fb.entitlements();
    if ent.has(HasQuery::Feature("ai_calls".into()), None).await? {
        // Reserve, do the work, commit; the hold is released automatically if the work fails.
        let req = MeteredRequest::new("ai_calls", "req-42");
        let answer = ent
            .with_reservation(&req, |_hold| async { Ok("generated text") })
            .await?;
        println!("{answer}");
    }
    Ok(())
}
