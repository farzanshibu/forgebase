//! Named queues as first-class settings (FB-64): concurrency limit, rate
//! limit per second or minute, pause and resume, default priority and
//! timeout. The limits are checked inside the claim transaction, so they hold
//! across the in-process pool and every external worker.

use std::collections::BTreeMap;

use forgebase_data::{AccessContext, Action};
use forgebase_db::{Statement, WriteTx};

use crate::error::JobError;
use crate::model::{check_name, iso};
use crate::service::{JobService, Result, Row, finish};
use crate::tasks::{QueueInfo, QueueSettings, RatePeriod};

const QUEUE_COLS: &str = "name, concurrency, rate_limit, rate_period_ms, paused, default_priority, \
    default_timeout_ms, created_at, updated_at";

fn blank(name: &str) -> QueueInfo {
    QueueInfo {
        name: name.to_owned(),
        configured: false,
        concurrency: None,
        rate_limit: None,
        rate_period: RatePeriod::Second,
        paused: false,
        default_priority: None,
        default_timeout_ms: None,
        queued: 0,
        waiting: 0,
        running: 0,
        succeeded: 0,
        dead: 0,
        cancelled: 0,
        created_at: None,
        updated_at: None,
    }
}

fn settings_into(q: &mut QueueInfo, r: &Row<'_>) {
    q.configured = true;
    q.concurrency = r.opt_int("concurrency");
    q.rate_limit = r.opt_int("rate_limit");
    q.rate_period = RatePeriod::from_ms(r.int("rate_period_ms"));
    q.paused = r.int("paused") == 1;
    q.default_priority = r.opt_int("default_priority");
    q.default_timeout_ms = r.opt_int("default_timeout_ms");
    q.created_at = r.opt_int("created_at").map(iso);
    q.updated_at = r.opt_int("updated_at").map(iso);
}

impl JobService {
    /// Every queue that has settings or jobs, by name.
    pub async fn queues(&self, ctx: &AccessContext) -> Result<Vec<QueueInfo>> {
        self.allow(ctx, Action::ReadJobs)?;
        self.queue_infos(None).await
    }

    /// One queue (a queue without settings or jobs has default settings).
    pub async fn queue(&self, ctx: &AccessContext, name: &str) -> Result<QueueInfo> {
        self.allow(ctx, Action::ReadJobs)?;
        check_name("queue", name, 64)?;
        Ok(self.queue_infos(Some(name)).await?.pop().unwrap_or_else(|| blank(name)))
    }

    async fn queue_infos(&self, only: Option<&str>) -> Result<Vec<QueueInfo>> {
        let (cond, params) = match only {
            Some(n) => ("WHERE name = ?1", vec![n.into()]),
            None => ("", Vec::new()),
        };
        let mut out: BTreeMap<String, QueueInfo> = BTreeMap::new();
        let settings = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT {QUEUE_COLS} FROM _forgebase_job_queues {cond}"),
                params.clone(),
            ))
            .await?;
        for i in 0..settings.len() {
            let r = Row::new(&settings, i);
            let q = out.entry(r.text("name")).or_insert_with(|| blank(&r.text("name")));
            settings_into(q, &r);
        }
        let jcond = if only.is_some() { "WHERE queue = ?1" } else { "" };
        let counts = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT queue, status, waiting_for > 0 AS waiting, COUNT(*) AS n FROM _forgebase_jobs {jcond} \
                     GROUP BY queue, status, waiting_for > 0"
                ),
                params,
            ))
            .await?;
        for i in 0..counts.len() {
            let r = Row::new(&counts, i);
            let name = r.text("queue");
            let q = out.entry(name.clone()).or_insert_with(|| blank(&name));
            let n = r.int("n");
            match (r.text("status").as_str(), r.int("waiting") == 1) {
                ("queued", true) => q.waiting += n,
                ("queued", false) => q.queued += n,
                ("running", _) => q.running += n,
                ("succeeded", _) => q.succeeded += n,
                ("dead", _) => q.dead += n,
                ("cancelled", _) => q.cancelled += n,
                (other, _) => return Err(JobError::Internal(format!("unknown job status {other:?}"))),
            }
        }
        Ok(out.into_values().collect())
    }

    /// Saves the settings of a queue (creating it). Limits left out are
    /// removed; `paused` left out keeps its value. Takes effect at the next
    /// claim of any worker.
    pub async fn set_queue(&self, ctx: &AccessContext, name: &str, settings: QueueSettings) -> Result<QueueInfo> {
        self.allow(ctx, Action::ManageJobs)?;
        check_name("queue", name, 64)?;
        settings.validate()?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let r = async {
            upsert(&mut *tx, name, now).await?;
            tx.execute(&Statement::new(
                "UPDATE _forgebase_job_queues SET concurrency = ?2, rate_limit = ?3, rate_period_ms = ?4, \
                 paused = COALESCE(?5, paused), default_priority = ?6, default_timeout_ms = ?7, updated_at = ?8 \
                 WHERE name = ?1",
                vec![
                    name.into(),
                    settings.concurrency.into(),
                    settings.rate_limit.into(),
                    settings.rate_period.unwrap_or_default().ms().into(),
                    settings.paused.map(i64::from).into(),
                    settings.default_priority.into(),
                    settings.default_timeout_ms.into(),
                    now.into(),
                ],
            ))
            .await?;
            Ok::<_, JobError>(())
        }
        .await;
        finish(tx, r).await?;
        self.wake_pool();
        self.queue(ctx, name).await
    }

    /// Pauses a queue: no worker gets its jobs until it is resumed (running
    /// jobs continue; enqueueing still works).
    pub async fn pause_queue(&self, ctx: &AccessContext, name: &str) -> Result<QueueInfo> {
        self.set_paused(ctx, name, true).await
    }

    /// Resumes a paused queue.
    pub async fn resume_queue(&self, ctx: &AccessContext, name: &str) -> Result<QueueInfo> {
        self.set_paused(ctx, name, false).await
    }

    async fn set_paused(&self, ctx: &AccessContext, name: &str, paused: bool) -> Result<QueueInfo> {
        self.allow(ctx, Action::ManageJobs)?;
        check_name("queue", name, 64)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let r = async {
            upsert(&mut *tx, name, now).await?;
            tx.execute(&Statement::new(
                "UPDATE _forgebase_job_queues SET paused = ?2, updated_at = ?3 WHERE name = ?1",
                vec![name.into(), i64::from(paused).into(), now.into()],
            ))
            .await?;
            Ok::<_, JobError>(())
        }
        .await;
        finish(tx, r).await?;
        if !paused {
            self.wake_pool();
        }
        self.queue(ctx, name).await
    }

    /// Removes the settings of a queue (its jobs stay; it has no limits then).
    pub async fn delete_queue(&self, ctx: &AccessContext, name: &str) -> Result<()> {
        self.allow(ctx, Action::ManageJobs)?;
        check_name("queue", name, 64)?;
        let mut tx = self.begin().await?;
        let r = async {
            tx.execute(&Statement::new(
                "DELETE FROM _forgebase_job_queues WHERE name = ?1",
                vec![name.into()],
            ))
            .await?;
            Ok::<_, JobError>(())
        }
        .await;
        finish(tx, r).await?;
        self.wake_pool();
        Ok(())
    }
}

async fn upsert(tx: &mut dyn WriteTx, name: &str, now: i64) -> Result<()> {
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_job_queues (name, created_at, updated_at) VALUES (?1, ?2, ?2) \
         ON CONFLICT (name) DO NOTHING",
        vec![name.into(), now.into()],
    ))
    .await?;
    Ok(())
}
