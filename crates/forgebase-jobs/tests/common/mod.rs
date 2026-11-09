//! Shared harness: a real SQLite database in a temp dir, migrated, with the
//! data core's authorizer behind the job service.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use forgebase_core::DataLayout;
use forgebase_data::schema::Collection;
use forgebase_data::{AccessContext, Action, Authorizer, DataService, Decision, DefaultAuthorizer};
use forgebase_db::{Database, DbOptions};
use forgebase_jobs::{ClaimRequest, ClaimedJob, JobService, ManualClock};

pub struct Harness {
    pub db: Database,
    pub data: DataService,
    pub svc: JobService,
    /// The clock `svc` reads: time moves only when a test advances it.
    pub clock: ManualClock,
    pub dir: tempfile::TempDir,
}

pub fn system() -> AccessContext {
    AccessContext::system()
}

pub async fn open_db(dir: &Path) -> Database {
    let layout = DataLayout::new(dir.join("data"));
    layout.ensure().unwrap();
    let cfg = forgebase_core::Config::defaults(dir);
    let db = Database::open(DbOptions::from_config(&cfg.database, layout.database_path()))
        .await
        .unwrap();
    db.migrate(Some(&layout.backups_dir())).await.unwrap();
    db
}

pub async fn service(db: &Database, authorizer: Arc<dyn Authorizer>) -> (DataService, JobService, ManualClock) {
    let data = DataService::new(Arc::new(db.clone()), authorizer).await.unwrap();
    let clock = ManualClock::from_system();
    let svc = JobService::with_clock(data.clone(), Arc::new(clock.clone()));
    (data, svc, clock)
}

pub async fn harness() -> Harness {
    harness_with(Arc::new(DefaultAuthorizer)).await
}

pub async fn harness_with(authorizer: Arc<dyn Authorizer>) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let db = open_db(dir.path()).await;
    let (data, svc, clock) = service(&db, authorizer).await;
    Harness {
        db,
        data,
        svc,
        clock,
        dir,
    }
}

pub fn claim_req(worker: &str, max: i64) -> ClaimRequest {
    ClaimRequest {
        worker_id: worker.to_owned(),
        max: Some(max),
        ..ClaimRequest::default()
    }
}

pub async fn claim_one(svc: &JobService, worker: &str) -> Option<ClaimedJob> {
    svc.claim(&system(), claim_req(worker, 1)).await.unwrap().pop()
}

/// Moves the harness clock forward by `by`, then claims one job.
pub async fn claim_after(h: &Harness, worker: &str, by: Duration) -> Option<ClaimedJob> {
    h.clock.advance(by);
    claim_one(&h.svc, worker).await
}

/// Polls `check` until it returns `Some` or `timeout` passes.
pub async fn wait_for<T, F, Fut>(timeout: Duration, mut check: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some(v) = check().await {
            return v;
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out after {timeout:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub fn ms(rfc3339: &str) -> i64 {
    i64::try_from(
        time::OffsetDateTime::parse(rfc3339, &time::format_description::well_known::Rfc3339)
            .unwrap()
            .unix_timestamp_nanos()
            / 1_000_000,
    )
    .unwrap()
}

/// Allows exactly the listed actions to any authenticated caller.
#[derive(Debug)]
pub struct Only(pub Vec<Action>);

impl Authorizer for Only {
    fn authorize(&self, ctx: &AccessContext, action: Action, _c: Option<&Collection>) -> Decision {
        if ctx.is_privileged() || (ctx.principal != forgebase_data::Principal::Anonymous && self.0.contains(&action)) {
            Decision::Allow
        } else {
            Decision::Deny
        }
    }
}
