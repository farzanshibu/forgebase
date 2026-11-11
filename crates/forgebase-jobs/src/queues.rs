//! Named queues as first-class settings (FB-64): concurrency limit, rate
//! limit per second or minute, pause and resume, default priority and
//! timeout. The limits are checked inside the claim transaction, so they hold
//! across the in-process pool and every external worker.
//!
//! Settings belong to a tenant (FB-146): a caller configures the queues of
//! its own tenant (none: the tenantless queues), and the settings govern
//! only that tenant's jobs. A tenant's settings are stored under the name
//! `<tenant>/<queue>` ([`queue_key`]; queue names never hold '/').
//! Non-privileged callers see and change only settings of their tenant, and
//! only those they created unless they hold `engine:read_jobs:all`, like
//! jobs (FB-142); queue counts include only the jobs they may see.
//!
//! Sharded (FB-172): the settings of a tenant are read and written on the
//! tenant's set, where its claims check them (`_forgebase_job_queues` is a
//! per-set table, ADR-0029 FB-170 addendum). Every change is mirrored to
//! control, which keeps the settings of every tenant, so a move can copy
//! them to the target ([`JobService::copy_queue_settings`]).

use std::collections::BTreeMap;

use forgebase_data::{AccessContext, Action, DataError};
use forgebase_db::{Statement, Value, WriteTx};

use crate::access::{Owner, Scope};
use crate::error::JobError;
use crate::model::{check_name, iso};
use crate::service::{JobService, Result, Row, finish};
use crate::tasks::{QueueInfo, QueueSettings, RatePeriod};

const QUEUE_COLS: &str = "name, tenant, created_by, concurrency, rate_limit, rate_period_ms, paused, \
    default_priority, default_timeout_ms, created_at, updated_at";

/// Stored name of the settings of queue `name` in `tenant`.
pub(crate) fn queue_key(tenant: Option<&str>, name: &str) -> String {
    match tenant {
        Some(t) => format!("{t}/{name}"),
        None => name.to_owned(),
    }
}

/// SQL expression of the settings name ([`queue_key`]) that governs a job
/// row; `alias` is empty or ends with `.`.
pub(crate) fn job_queue_key(alias: &str) -> String {
    format!("(CASE WHEN {alias}tenant IS NULL THEN {alias}queue ELSE {alias}tenant || '/' || {alias}queue END)")
}

fn blank(tenant: Option<&str>, name: &str) -> QueueInfo {
    QueueInfo {
        name: name.to_owned(),
        tenant: tenant.map(str::to_owned),
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
        created_by: None,
        created_at: None,
        updated_at: None,
    }
}

fn settings_into(q: &mut QueueInfo, r: &Row<'_>) {
    q.configured = true;
    q.created_by = r.opt_text("created_by");
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
    /// Every queue that has settings or jobs the caller may see, by tenant
    /// and name.
    pub async fn queues(&self, ctx: &AccessContext) -> Result<Vec<QueueInfo>> {
        self.at_tenant(ctx.tenant.as_deref()).await?.queues_here(ctx).await
    }

    /// [`Self::queues`] on this view's set.
    pub(crate) async fn queues_here(&self, ctx: &AccessContext) -> Result<Vec<QueueInfo>> {
        self.allow(ctx, Action::ReadJobs)?;
        self.queue_infos(&self.access().scope(ctx), None).await
    }

    /// One queue of the caller's tenant (a queue without settings or jobs has
    /// default settings).
    pub async fn queue(&self, ctx: &AccessContext, name: &str) -> Result<QueueInfo> {
        self.at_tenant(ctx.tenant.as_deref()).await?.queue_here(ctx, name).await
    }

    /// [`Self::queue`] on this view's set.
    pub(crate) async fn queue_here(&self, ctx: &AccessContext, name: &str) -> Result<QueueInfo> {
        self.allow(ctx, Action::ReadJobs)?;
        check_name("queue", name, 64)?;
        let tenant = ctx.tenant.as_deref();
        Ok(self
            .queue_infos(&self.access().scope(ctx), Some((tenant, name)))
            .await?
            .pop()
            .unwrap_or_else(|| blank(tenant, name)))
    }

    async fn queue_infos(&self, scope: &Scope, only: Option<(Option<&str>, &str)>) -> Result<Vec<QueueInfo>> {
        let mut params: Vec<Value> = Vec::new();
        let mut cond = scope.sql("", false, &mut params);
        if let Some((tenant, name)) = only {
            params.push(queue_key(tenant, name).into());
            cond.push_str(&format!(" AND name = ?{}", params.len()));
        }
        let mut out: BTreeMap<String, QueueInfo> = BTreeMap::new();
        let settings = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT {QUEUE_COLS} FROM _forgebase_job_queues WHERE {cond}"),
                params,
            ))
            .await?;
        for i in 0..settings.len() {
            let r = Row::new(&settings, i);
            let tenant = r.opt_text("tenant");
            let key = r.text("name");
            let name = match &tenant {
                Some(t) => key.strip_prefix(&format!("{t}/")).unwrap_or(&key).to_owned(),
                None => key.clone(),
            };
            let q = out.entry(key).or_insert_with(|| blank(tenant.as_deref(), &name));
            settings_into(q, &r);
        }
        let mut params: Vec<Value> = Vec::new();
        let mut jcond = scope.sql("", true, &mut params);
        if let Some((tenant, name)) = only {
            params.push(name.into());
            jcond.push_str(&format!(" AND queue = ?{}", params.len()));
            match tenant {
                Some(t) => {
                    params.push(t.into());
                    jcond.push_str(&format!(" AND tenant = ?{}", params.len()));
                }
                None => jcond.push_str(" AND tenant IS NULL"),
            }
        }
        let counts = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT queue, tenant, status, waiting_for > 0 AS waiting, COUNT(*) AS n FROM _forgebase_jobs \
                     WHERE {jcond} GROUP BY queue, tenant, status, waiting_for > 0"
                ),
                params,
            ))
            .await?;
        for i in 0..counts.len() {
            let r = Row::new(&counts, i);
            let (name, tenant) = (r.text("queue"), r.opt_text("tenant"));
            let q = out
                .entry(queue_key(tenant.as_deref(), &name))
                .or_insert_with(|| blank(tenant.as_deref(), &name));
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
        let view = self.at_tenant(ctx.tenant.as_deref()).await?;
        let out = view.set_queue_here(ctx, name, settings).await?;
        view.mirror_queue(&queue_key(ctx.tenant.as_deref(), name)).await?;
        Ok(out)
    }

    /// [`Self::set_queue`] on this view's set.
    async fn set_queue_here(&self, ctx: &AccessContext, name: &str, settings: QueueSettings) -> Result<QueueInfo> {
        self.allow(ctx, Action::ManageJobs)?;
        check_name("queue", name, 64)?;
        settings.validate()?;
        let now = self.now();
        let key = queue_key(ctx.tenant.as_deref(), name);
        let scope = self.access().scope(ctx);
        let mut tx = self.begin().await?;
        let r = async {
            upsert(&mut *tx, &scope, ctx, &key, now).await?;
            tx.execute(&Statement::new(
                "UPDATE _forgebase_job_queues SET concurrency = ?2, rate_limit = ?3, rate_period_ms = ?4, \
                 paused = COALESCE(?5, paused), default_priority = ?6, default_timeout_ms = ?7, updated_at = ?8 \
                 WHERE name = ?1",
                vec![
                    key.as_str().into(),
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
        let view = self.at_tenant(ctx.tenant.as_deref()).await?;
        let out = view.set_paused_here(ctx, name, paused).await?;
        view.mirror_queue(&queue_key(ctx.tenant.as_deref(), name)).await?;
        Ok(out)
    }

    /// [`Self::set_paused`] on this view's set.
    async fn set_paused_here(&self, ctx: &AccessContext, name: &str, paused: bool) -> Result<QueueInfo> {
        self.allow(ctx, Action::ManageJobs)?;
        check_name("queue", name, 64)?;
        let now = self.now();
        let key = queue_key(ctx.tenant.as_deref(), name);
        let scope = self.access().scope(ctx);
        let mut tx = self.begin().await?;
        let r = async {
            upsert(&mut *tx, &scope, ctx, &key, now).await?;
            tx.execute(&Statement::new(
                "UPDATE _forgebase_job_queues SET paused = ?2, updated_at = ?3 WHERE name = ?1",
                vec![key.as_str().into(), i64::from(paused).into(), now.into()],
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
        let view = self.at_tenant(ctx.tenant.as_deref()).await?;
        view.delete_queue_here(ctx, name).await?;
        view.mirror_queue(&queue_key(ctx.tenant.as_deref(), name)).await
    }

    /// [`Self::delete_queue`] on this view's set.
    async fn delete_queue_here(&self, ctx: &AccessContext, name: &str) -> Result<()> {
        self.allow(ctx, Action::ManageJobs)?;
        check_name("queue", name, 64)?;
        let key = queue_key(ctx.tenant.as_deref(), name);
        let scope = self.access().scope(ctx);
        let mut tx = self.begin().await?;
        let r = async {
            if owner_of(&mut *tx, &key).await?.is_some_and(|o| !scope.sees(&o, None)) {
                return Err(not_yours(name));
            }
            tx.execute(&Statement::new(
                "DELETE FROM _forgebase_job_queues WHERE name = ?1",
                vec![key.as_str().into()],
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

/// The owner of the settings stored as `key`, if they exist.
async fn owner_of(tx: &mut dyn WriteTx, key: &str) -> Result<Option<Owner>> {
    let rows = tx
        .fetch_all(&Statement::new(
            "SELECT tenant, created_by FROM _forgebase_job_queues WHERE name = ?1",
            vec![key.into()],
        ))
        .await?;
    Ok((!rows.is_empty()).then(|| {
        let r = Row::new(&rows, 0);
        Owner {
            tenant: r.opt_text("tenant"),
            created_by: r.opt_text("created_by").unwrap_or_default(),
        }
    }))
}

fn not_yours(name: &str) -> JobError {
    JobError::Access(DataError::Forbidden(format!(
        "the settings of queue {name:?} were created by someone else"
    )))
}

/// Creates the settings row `key` for the caller (its tenant and principal),
/// or checks that the caller may change the existing one.
impl JobService {
    /// Copies the stored settings `key` of this view's set to control
    /// (sharded only, and only from another set).
    async fn mirror_queue(&self, key: &str) -> Result<()> {
        if self.sharded().is_none() || self.on_control() {
            return Ok(());
        }
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT {QUEUE_COLS} FROM _forgebase_job_queues WHERE name = ?1"),
                vec![key.into()],
            ))
            .await?;
        let control = self.control_view();
        let mut tx = control.begin().await?;
        let r = write_settings_row(&mut *tx, key, &rows).await;
        finish(tx, r).await
    }

    /// Copies the queue settings of `tenant` from control to the set the
    /// tenant is placed on now (sharded only). A tenant move (FB-174) calls
    /// it after the switch: `_forgebase_job_queues` is per set and does not
    /// travel with the tenant's rows. Returns how many settings were copied.
    pub async fn copy_queue_settings(&self, tenant: &str) -> Result<usize> {
        let target = self.at_tenant(Some(tenant)).await?;
        if target.sharded().is_none() || target.on_control() {
            return Ok(0);
        }
        let rows = self
            .control_view()
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT {QUEUE_COLS} FROM _forgebase_job_queues WHERE tenant = ?1"),
                vec![tenant.into()],
            ))
            .await?;
        let mut tx = target.begin().await?;
        let r = async {
            for i in 0..rows.len() {
                let key = Row::new(&rows, i).text("name");
                let one = forgebase_db::Rows {
                    columns: rows.columns.clone(),
                    rows: vec![rows.rows[i].clone()],
                };
                write_settings_row(&mut *tx, &key, &one).await?;
            }
            Ok::<_, JobError>(rows.len())
        }
        .await;
        finish(tx, r).await
    }
}

/// Makes the settings row `key` equal `rows` (one row of [`QUEUE_COLS`], or
/// none: deleted). The rate window is not copied: it restarts.
async fn write_settings_row(tx: &mut dyn WriteTx, key: &str, rows: &forgebase_db::Rows) -> Result<()> {
    if rows.is_empty() {
        tx.execute(&Statement::new(
            "DELETE FROM _forgebase_job_queues WHERE name = ?1",
            vec![key.into()],
        ))
        .await?;
        return Ok(());
    }
    let values: Vec<Value> = rows.rows[0].clone();
    tx.execute(&Statement::new(
        format!(
            "INSERT INTO _forgebase_job_queues ({QUEUE_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT (name) DO UPDATE SET tenant = excluded.tenant, created_by = excluded.created_by, \
             concurrency = excluded.concurrency, rate_limit = excluded.rate_limit, \
             rate_period_ms = excluded.rate_period_ms, paused = excluded.paused, \
             default_priority = excluded.default_priority, default_timeout_ms = excluded.default_timeout_ms, \
             created_at = excluded.created_at, updated_at = excluded.updated_at"
        ),
        values,
    ))
    .await?;
    Ok(())
}

async fn upsert(tx: &mut dyn WriteTx, scope: &Scope, ctx: &AccessContext, key: &str, now: i64) -> Result<()> {
    if let Some(owner) = owner_of(tx, key).await? {
        if !scope.sees(&owner, None) {
            let name = key.rsplit('/').next().unwrap_or(key);
            return Err(not_yours(name));
        }
        return Ok(());
    }
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_job_queues (name, tenant, created_by, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?4)",
        vec![key.into(), ctx.tenant.clone().into(), ctx.actor().into(), now.into()],
    ))
    .await?;
    Ok(())
}
