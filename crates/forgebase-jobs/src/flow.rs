//! Dependencies and workflows (FB-64): what happens when a job finishes.
//!
//! A job with parents (`depends_on`, or the steps of a workflow) is `queued`
//! with `waiting_for` = parents not succeeded yet; claims skip it until that
//! reaches 0. When a parent succeeds, its dependants count down; when a
//! parent ends `dead` or `cancelled`, its waiting dependants are cancelled
//! (and theirs, transitively). A workflow is a set of such jobs plus a row in
//! `_forgebase_job_workflows`; the first failed step fails it (or starts the
//! compensations of the succeeded steps, newest first, as a chain).
//! Everything happens inside the transaction that finished the job.

use forgebase_data::{AccessContext, Action};
use forgebase_db::{Backend, Statement, Value, WriteTx};

use crate::error::JobError;
use crate::model::{JobStatus, iso};
use crate::access::{Owner, Scope};
use crate::service::{
    Fx, JOB_COLS, JobService, Origin, Result, Row, cancel_tx, event, fetch_job_tx, insert_job, job_from,
};
use crate::tasks::{
    Compensation, FailurePolicy, NewWorkflow, Workflow, WorkflowDetail, WorkflowPage, WorkflowStatus, WorkflowStep,
};

const WORKFLOW_COLS: &str =
    "id, name, status, on_failure, failed_job_id, error, created_by, created_at, updated_at, finished_at, tenant";

fn workflow_from(r: &Row<'_>) -> Result<Workflow> {
    let status = r.text("status");
    Ok(Workflow {
        id: r.text("id"),
        name: r.text("name"),
        status: WorkflowStatus::parse(&status)
            .ok_or_else(|| JobError::Internal(format!("unknown workflow status {status:?}")))?,
        on_failure: FailurePolicy::parse(&r.text("on_failure")),
        failed_job_id: r.opt_text("failed_job_id"),
        error: r.opt_text("error"),
        created_by: r.text("created_by"),
        tenant: r.opt_text("tenant"),
        created_at: iso(r.int("created_at")),
        updated_at: iso(r.int("updated_at")),
        finished_at: r.opt_int("finished_at").map(iso),
    })
}

pub(crate) async fn fetch_workflow(backend: &dyn Backend, id: &str) -> Result<Option<Workflow>> {
    let rows = backend
        .fetch_all(&Statement::new(
            format!("SELECT {WORKFLOW_COLS} FROM _forgebase_job_workflows WHERE id = ?1"),
            vec![id.into()],
        ))
        .await?;
    if rows.is_empty() {
        return Ok(None);
    }
    workflow_from(&Row::new(&rows, 0)).map(Some)
}

impl JobService {
    /// Creates a workflow: every step becomes a job in one transaction;
    /// steps without dependencies are claimable at once, the others wait for
    /// their parents. Parents' results reach a step through
    /// [`crate::ClaimedJob::parents`] / [`crate::JobContext::parents`].
    pub async fn create_workflow(&self, ctx: &AccessContext, new: NewWorkflow) -> Result<WorkflowDetail> {
        self.allow(ctx, Action::EnqueueJob)?;
        let order = new.order()?;
        let now = self.now();
        let access = self.access();
        let mut resolved = Vec::with_capacity(new.steps.len());
        for (i, step) in new.steps.iter().enumerate() {
            let job = step.job();
            let mut r = job
                .resolve(now)
                .map_err(|e| JobError::invalid("invalid_workflow", format!("step {:?}: {e}", step.name)))?;
            // FB-142: every step and compensation kind must be one the
            // caller may enqueue.
            access.admit(ctx, &job.kind, &job.payload)?;
            if let Some(c) = &step.compensate {
                access.admit(ctx, &c.kind, &c.payload)?;
            }
            r.step = Some(step.name.clone());
            if let Some(c) = &step.compensate {
                crate::model::check_name("compensate.kind", &c.kind, 128)?;
                if let Some(q) = &c.queue {
                    crate::model::check_name("compensate.queue", q, 64)?;
                }
                crate::model::check_payload(&c.payload)?;
                r.compensate = Some(serde_json::to_string(c).map_err(|e| JobError::Internal(e.to_string()))?);
            }
            resolved.push((i, r));
        }
        let id = format!("wf-{}", uuid::Uuid::new_v4().simple());
        let policy = new.on_failure.unwrap_or_default();
        let origin = Origin::of(ctx);
        let mut fx = Fx::default();
        let mut tx = self.begin().await?;
        let r = async {
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_job_workflows (id, name, status, on_failure, created_by, created_at, \
                 updated_at, tenant) VALUES (?1, ?2, 'running', ?3, ?4, ?5, ?5, ?6)",
                vec![
                    id.as_str().into(),
                    new.name.as_str().into(),
                    policy.as_str().into(),
                    origin.actor.as_str().into(),
                    now.into(),
                    origin.tenant.clone().into(),
                ],
            ))
            .await?;
            let mut job_ids: Vec<Option<String>> = vec![None; new.steps.len()];
            for &i in &order {
                let step = &new.steps[i];
                let mut r = resolved[i].1.clone();
                r.workflow_id = Some(id.clone());
                r.depends_on = step
                    .depends_on
                    .iter()
                    .filter_map(|d| new.steps.iter().position(|s| &s.name == d))
                    .filter_map(|p| job_ids[p].clone())
                    .collect();
                r.depends_on.dedup();
                let (job, _) = insert_job(&mut *tx, &r, None, &origin, now, &mut fx).await?;
                job_ids[i] = Some(job.id);
            }
            fx.workflows.insert(id.clone());
            workflow_detail_tx(&mut *tx, &id).await
        }
        .await;
        self.commit(tx, r, fx).await
    }

    /// A workflow with its jobs and their dependencies.
    pub async fn workflow(&self, ctx: &AccessContext, id: &str) -> Result<WorkflowDetail> {
        self.allow(ctx, Action::ReadJobs)?;
        let workflow = fetch_workflow(self.backend(), id)
            .await?
            .filter(|w| self.access().scope(ctx).sees(&workflow_owner(w), None))
            .ok_or_else(|| JobError::WorkflowNotFound(id.to_owned()))?;
        self.access().remember(id, workflow_owner(&workflow));
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT {JOB_COLS} FROM _forgebase_jobs WHERE workflow_id = ?1 ORDER BY created_at, rowid"),
                vec![id.into()],
            ))
            .await?;
        let deps = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT d.job_id, d.parent_id FROM _forgebase_job_deps d JOIN _forgebase_jobs j ON j.id = d.job_id \
                 WHERE j.workflow_id = ?1",
                vec![id.into()],
            ))
            .await?;
        steps_from(workflow, &rows, &deps)
    }

    /// Workflows, newest first, optionally of one state.
    pub async fn workflows(
        &self,
        ctx: &AccessContext,
        status: Option<WorkflowStatus>,
        limit: Option<i64>,
        offset: Option<i64>,
    ) -> Result<WorkflowPage> {
        self.allow(ctx, Action::ReadJobs)?;
        let mut params: Vec<Value> = Vec::new();
        let scope = self.access().scope(ctx).sql("", false, &mut params);
        let where_sql = match status {
            Some(s) => {
                params.push(s.as_str().into());
                format!("WHERE {scope} AND status = ?{}", params.len())
            }
            None => format!("WHERE {scope}"),
        };
        let total = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT COUNT(*) FROM _forgebase_job_workflows {where_sql}"),
                params.clone(),
            ))
            .await?
            .scalar()
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let mut params = params;
        let n = params.len();
        params.push(limit.unwrap_or(50).clamp(1, crate::model::MAX_PAGE).into());
        params.push(offset.unwrap_or(0).max(0).into());
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {WORKFLOW_COLS} FROM _forgebase_job_workflows {where_sql} \
                     ORDER BY created_at DESC, rowid DESC LIMIT ?{} OFFSET ?{}",
                    n + 1,
                    n + 2
                ),
                params,
            ))
            .await?;
        let items: Vec<Workflow> = (0..rows.len())
            .map(|i| workflow_from(&Row::new(&rows, i)))
            .collect::<Result<_>>()?;
        for w in &items {
            self.access().remember(&w.id, workflow_owner(w));
        }
        Ok(WorkflowPage { items, total })
    }

    /// Cancels a running (or compensating) workflow: waiting jobs at once,
    /// running ones cooperatively. No compensations run.
    pub async fn cancel_workflow(&self, ctx: &AccessContext, id: &str) -> Result<WorkflowDetail> {
        self.allow(ctx, Action::ManageJobs)?;
        let scope = self.access().scope(ctx);
        let now = self.now();
        let mut fx = Fx::default();
        let mut tx = self.begin().await?;
        let r = async {
            let wf = workflow_tx(&mut *tx, id).await?;
            if scope != Scope::All && !scope.sees(&workflow_owner(&wf), None) {
                return Err(JobError::WorkflowNotFound(id.to_owned()));
            }
            if wf.status.is_terminal() {
                return Err(JobError::state(
                    "workflow_finished",
                    format!("workflow is already {}", wf.status.as_str()),
                ));
            }
            set_status(
                &mut *tx,
                id,
                WorkflowStatus::Cancelled,
                None,
                Some("cancelled"),
                now,
                &mut fx,
            )
            .await?;
            stop_unfinished(&mut *tx, id, false, now, &mut fx).await?;
            workflow_detail_tx(&mut *tx, id).await
        }
        .await;
        self.commit(tx, r, fx).await
    }
}

/// The tenant and creator of a workflow.
fn workflow_owner(w: &Workflow) -> Owner {
    Owner {
        tenant: w.tenant.clone(),
        created_by: w.created_by.clone(),
    }
}

fn steps_from(workflow: Workflow, rows: &forgebase_db::Rows, deps: &forgebase_db::Rows) -> Result<WorkflowDetail> {
    let mut steps = Vec::with_capacity(rows.len());
    for i in 0..rows.len() {
        let r = Row::new(rows, i);
        let job = job_from(&r)?;
        let depends_on = (0..deps.len())
            .map(|k| Row::new(deps, k))
            .filter(|d| d.text("job_id") == job.id)
            .map(|d| d.text("parent_id"))
            .collect();
        steps.push(WorkflowStep {
            name: job.step.clone(),
            role: if job.compensates.is_some() {
                "compensation"
            } else {
                "step"
            }
            .to_owned(),
            job,
            depends_on,
        });
    }
    Ok(WorkflowDetail { workflow, steps })
}

async fn workflow_tx(tx: &mut dyn WriteTx, id: &str) -> Result<Workflow> {
    let rows = tx
        .fetch_all(&Statement::new(
            format!("SELECT {WORKFLOW_COLS} FROM _forgebase_job_workflows WHERE id = ?1"),
            vec![id.into()],
        ))
        .await?;
    if rows.is_empty() {
        return Err(JobError::WorkflowNotFound(id.to_owned()));
    }
    workflow_from(&Row::new(&rows, 0))
}

async fn workflow_detail_tx(tx: &mut dyn WriteTx, id: &str) -> Result<WorkflowDetail> {
    let wf = workflow_tx(tx, id).await?;
    let rows = tx
        .fetch_all(&Statement::new(
            format!("SELECT {JOB_COLS} FROM _forgebase_jobs WHERE workflow_id = ?1 ORDER BY created_at, rowid"),
            vec![id.into()],
        ))
        .await?;
    let deps = tx
        .fetch_all(&Statement::new(
            "SELECT d.job_id, d.parent_id FROM _forgebase_job_deps d JOIN _forgebase_jobs j ON j.id = d.job_id \
             WHERE j.workflow_id = ?1",
            vec![id.into()],
        ))
        .await?;
    steps_from(wf, &rows, &deps)
}

async fn set_status(
    tx: &mut dyn WriteTx,
    id: &str,
    status: WorkflowStatus,
    failed_job: Option<&str>,
    error: Option<&str>,
    now: i64,
    fx: &mut Fx,
) -> Result<()> {
    tx.execute(&Statement::new(
        "UPDATE _forgebase_job_workflows SET status = ?2, failed_job_id = COALESCE(?3, failed_job_id), \
         error = COALESCE(?4, error), updated_at = ?5, finished_at = CASE WHEN ?6 = 1 THEN ?5 ELSE NULL END \
         WHERE id = ?1",
        vec![
            id.into(),
            status.as_str().into(),
            failed_job.map(str::to_owned).into(),
            error.map(|e| e.chars().take(2000).collect::<String>()).into(),
            now.into(),
            i64::from(status.is_terminal()).into(),
        ],
    ))
    .await?;
    fx.workflows.insert(id.to_owned());
    Ok(())
}

/// Cancels the unfinished jobs of a workflow (`compensations`: those, else
/// the steps).
async fn stop_unfinished(tx: &mut dyn WriteTx, wf: &str, compensations: bool, now: i64, fx: &mut Fx) -> Result<()> {
    let role = if compensations { "IS NOT NULL" } else { "IS NULL" };
    let rows = tx
        .fetch_all(&Statement::new(
            format!(
                "SELECT id FROM _forgebase_jobs WHERE workflow_id = ?1 AND compensates {role} \
                 AND status IN ('queued', 'running') ORDER BY rowid"
            ),
            vec![wf.into()],
        ))
        .await?;
    for i in 0..rows.len() {
        cancel_tx(tx, &Row::new(&rows, i).text("id"), now, fx, false).await?;
    }
    Ok(())
}

async fn unfinished(tx: &mut dyn WriteTx, wf: &str, compensations: bool) -> Result<i64> {
    let role = if compensations { "IS NOT NULL" } else { "IS NULL" };
    Ok(tx
        .fetch_all(&Statement::new(
            format!(
                "SELECT COUNT(*) FROM _forgebase_jobs WHERE workflow_id = ?1 AND compensates {role} \
                 AND status IN ('queued', 'running')"
            ),
            vec![wf.into()],
        ))
        .await?
        .scalar()
        .and_then(Value::as_i64)
        .unwrap_or(0))
}

/// Called in the transaction that moved `job_id` to a terminal state.
pub(crate) async fn on_terminal(
    tx: &mut dyn WriteTx,
    job_id: &str,
    status: JobStatus,
    now: i64,
    fx: &mut Fx,
) -> Result<()> {
    let mut work = vec![(job_id.to_owned(), status)];
    while let Some((id, status)) = work.pop() {
        // Dependants that are still waiting.
        let rows = tx
            .fetch_all(&Statement::new(
                "SELECT j.id, j.attempts FROM _forgebase_job_deps d JOIN _forgebase_jobs j ON j.id = d.job_id \
                 WHERE d.parent_id = ?1 AND j.status = 'queued' AND j.waiting_for > 0",
                vec![id.as_str().into()],
            ))
            .await?;
        for i in 0..rows.len() {
            let child = Row::new(&rows, i).text("id");
            if status == JobStatus::Succeeded {
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_jobs SET waiting_for = waiting_for - 1, updated_at = ?2 \
                     WHERE id = ?1 AND waiting_for > 0",
                    vec![child.as_str().into(), now.into()],
                ))
                .await?;
                fx.job(&child, "status");
                fx.wake = true;
            } else {
                let message = format!("dependency {id} ended {}", status.as_str());
                tx.execute(&Statement::new(
                    "UPDATE _forgebase_jobs SET status = 'cancelled', last_error = ?2, finished_at = ?3, \
                     updated_at = ?3 WHERE id = ?1",
                    vec![child.as_str().into(), message.as_str().into(), now.into()],
                ))
                .await?;
                event(
                    tx,
                    &child,
                    now,
                    "cancelled",
                    Row::new(&rows, i).int("attempts"),
                    None,
                    Some(&message),
                )
                .await?;
                fx.job(&child, "status");
                work.push((child, JobStatus::Cancelled));
            }
        }
        step_finished(tx, &id, status, now, fx).await?;
    }
    Ok(())
}

/// Moves the job's workflow on after one of its jobs finished.
async fn step_finished(tx: &mut dyn WriteTx, job_id: &str, status: JobStatus, now: i64, fx: &mut Fx) -> Result<()> {
    let job = fetch_job_tx(tx, job_id).await?;
    let Some(wf_id) = job.workflow_id.clone() else {
        return Ok(());
    };
    let wf = match workflow_tx(tx, &wf_id).await {
        Ok(w) => w,
        Err(JobError::WorkflowNotFound(_)) => return Ok(()),
        Err(e) => return Err(e),
    };
    let compensation = job.compensates.is_some();
    match (wf.status, compensation, status) {
        (WorkflowStatus::Running, false, JobStatus::Succeeded) => {
            if unfinished(tx, &wf_id, false).await? == 0 {
                set_status(tx, &wf_id, WorkflowStatus::Succeeded, None, None, now, fx).await?;
            } else {
                fx.workflows.insert(wf_id);
            }
        }
        (WorkflowStatus::Running, false, _) => {
            let error = format!(
                "step {} ({}) ended {}: {}",
                job.step.as_deref().unwrap_or("-"),
                job.id,
                status.as_str(),
                job.last_error.as_deref().unwrap_or("no error message")
            );
            match wf.on_failure {
                FailurePolicy::Fail => {
                    set_status(tx, &wf_id, WorkflowStatus::Failed, Some(job_id), Some(&error), now, fx).await?;
                    stop_unfinished(tx, &wf_id, false, now, fx).await?;
                }
                FailurePolicy::Compensate => {
                    set_status(
                        tx,
                        &wf_id,
                        WorkflowStatus::Compensating,
                        Some(job_id),
                        Some(&error),
                        now,
                        fx,
                    )
                    .await?;
                    stop_unfinished(tx, &wf_id, false, now, fx).await?;
                    if start_compensations(tx, &wf_id, &wf.created_by, now, fx).await? == 0 {
                        set_status(tx, &wf_id, WorkflowStatus::Compensated, None, None, now, fx).await?;
                    }
                }
            }
        }
        (WorkflowStatus::Compensating, true, JobStatus::Succeeded) => {
            if unfinished(tx, &wf_id, true).await? == 0 {
                set_status(tx, &wf_id, WorkflowStatus::Compensated, None, None, now, fx).await?;
            } else {
                fx.workflows.insert(wf_id);
            }
        }
        (WorkflowStatus::Compensating, true, _) => {
            let error = format!(
                "compensation {} ({}) ended {}: {}",
                job.step.as_deref().unwrap_or("-"),
                job.id,
                status.as_str(),
                job.last_error.as_deref().unwrap_or("no error message")
            );
            set_status(tx, &wf_id, WorkflowStatus::Failed, None, Some(&error), now, fx).await?;
            stop_unfinished(tx, &wf_id, true, now, fx).await?;
        }
        _ => {}
    }
    Ok(())
}

/// Enqueues the compensation of every succeeded step that has one, newest
/// first, each waiting for the previous one. Returns how many.
async fn start_compensations(tx: &mut dyn WriteTx, wf: &str, actor: &str, now: i64, fx: &mut Fx) -> Result<usize> {
    let rows = tx
        .fetch_all(&Statement::new(
            "SELECT id, queue, step, compensate, tenant, context FROM _forgebase_jobs WHERE workflow_id = ?1 \
             AND compensates IS NULL AND status = 'succeeded' AND compensate IS NOT NULL \
             ORDER BY finished_at DESC, rowid DESC",
            vec![wf.into()],
        ))
        .await?;
    let mut previous: Option<String> = None;
    for i in 0..rows.len() {
        let r = Row::new(&rows, i);
        let comp: Compensation = serde_json::from_str(&r.text("compensate"))
            .map_err(|e| JobError::Internal(format!("stored compensation is not valid: {e}")))?;
        let new = crate::model::NewJob {
            kind: comp.kind.clone(),
            payload: comp.payload.clone(),
            queue: Some(comp.queue.clone().unwrap_or_else(|| r.text("queue"))),
            ..crate::model::NewJob::default()
        };
        let mut resolved = new.resolve(now)?;
        resolved.workflow_id = Some(wf.to_owned());
        resolved.compensates = Some(r.text("id"));
        resolved.step = Some(format!("compensate:{}", r.text("step")).chars().take(128).collect());
        resolved.depends_on = previous.iter().cloned().collect();
        // FB-142: a compensation acts like the step it undoes.
        let origin = Origin {
            actor: actor.to_owned(),
            tenant: r.opt_text("tenant"),
            context: r.opt_text("context"),
        };
        let (job, _) = insert_job(tx, &resolved, None, &origin, now, fx).await?;
        previous = Some(job.id);
    }
    Ok(rows.len())
}
