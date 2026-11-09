//! Durable jobs: enqueue, status, wait, cancel, retry, and the worker protocol.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::generated::{
    Attempt, Checkpoint, ClaimResponse, Heartbeat, Job, JobEvent, JobResult, JobStats, JobStatus, JobUpdate, NewJob,
    NewWorkflow, QueueInfo, QueueSettings, WorkflowDetail, WorkflowPage, WorkflowUpdate,
};
use crate::http::{Req, Transport, seg};
use crate::realtime::{Realtime, RealtimeEvent, SubscribeOptions, Subscription};

/// Filters for [`JobsApi::list`].
#[derive(Debug, Clone, Default)]
pub struct ListJobs {
    /// `queued`, `running`, `succeeded`, `dead` or `cancelled`.
    pub status: Option<String>,
    /// Queue name.
    pub queue: Option<String>,
    /// Job kind.
    pub kind: Option<String>,
    /// Page size.
    pub limit: Option<u32>,
    /// Page offset.
    pub offset: Option<u32>,
}

/// Background jobs: `client.jobs()`.
#[derive(Clone)]
pub struct JobsApi {
    pub(crate) t: Arc<Transport>,
    pub(crate) realtime: Realtime,
}

/// Live updates of one job or workflow (progress, status changes), from the realtime channel
/// `jobs:<id>`. Created by [`JobsApi::watch`] / [`JobsApi::watch_workflow`].
pub struct JobWatch<T> {
    sub: Subscription,
    _marker: std::marker::PhantomData<fn() -> T>,
}

impl<T: serde::de::DeserializeOwned> JobWatch<T> {
    /// The next update within `timeout`; `Ok(None)` on timeout, an error when the subscription ended.
    pub async fn next_timeout(&mut self, timeout: Duration) -> Result<Option<T>> {
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            match self.sub.next_timeout(left).await {
                Some(RealtimeEvent::Message(m)) => {
                    if let Ok(u) = serde_json::from_value::<T>(m.data) {
                        return Ok(Some(u));
                    }
                }
                Some(RealtimeEvent::Error { code, message }) => return Err(Error::Realtime { code, message }),
                Some(_) => {}
                None => return Ok(None),
            }
        }
    }

    /// The next update; waits as long as it takes. `Err` when the subscription ended.
    pub async fn next(&mut self) -> Result<T> {
        loop {
            if let Some(u) = self.next_timeout(Duration::from_secs(3600)).await? {
                return Ok(u);
            }
        }
    }
}

fn finished(j: &Job) -> bool {
    matches!(j.status, JobStatus::Succeeded | JobStatus::Dead | JobStatus::Cancelled)
}

impl JobsApi {
    /// Enqueues a job. With an `idempotency_key` a repeat returns the existing job (and the call is
    /// retried safely on network errors).
    pub async fn enqueue(&self, job: &NewJob) -> Result<Job> {
        let mut r = Req::post("/api/v1/jobs").json(job)?;
        if job.idempotency_key.is_some() {
            r = r.idempotent();
        }
        self.t.json(r).await
    }

    /// Status and result of one job.
    pub async fn get(&self, id: &str) -> Result<Job> {
        self.t.json(Req::get(format!("/api/v1/jobs/{}", seg(id)))).await
    }

    /// Jobs matching the filters, with the total.
    pub async fn list(&self, f: &ListJobs) -> Result<(Vec<Job>, u64)> {
        let r = Req::get("/api/v1/jobs")
            .q_opt("status", f.status.as_deref())
            .q_opt("queue", f.queue.as_deref())
            .q_opt("kind", f.kind.as_deref())
            .q_opt("limit", f.limit)
            .q_opt("offset", f.offset);
        let v = self.t.value(r).await?;
        let items = serde_json::from_value(v.get("items").cloned().unwrap_or(Value::Array(Vec::new())))
            .map_err(|e| Error::InvalidResponse(format!("jobs: {e}")))?;
        Ok((items, v.get("total").and_then(Value::as_u64).unwrap_or(0)))
    }

    /// The job's event log (claimed, failed, retried, ...).
    pub async fn events(&self, id: &str) -> Result<Vec<JobEvent>> {
        let v = self
            .t
            .value(Req::get(format!("/api/v1/jobs/{}/events", seg(id))))
            .await?;
        serde_json::from_value(v.get("items").cloned().unwrap_or(Value::Array(Vec::new())))
            .map_err(|e| Error::InvalidResponse(format!("job events: {e}")))
    }

    /// Requests cancellation (a running job sees `cancel_requested` on its next heartbeat).
    pub async fn cancel(&self, id: &str) -> Result<Job> {
        self.t.json(Req::post(format!("/api/v1/jobs/{}/cancel", seg(id)))).await
    }

    /// Re-queues a dead or cancelled job.
    pub async fn retry(&self, id: &str) -> Result<Job> {
        self.t.json(Req::post(format!("/api/v1/jobs/{}/retry", seg(id)))).await
    }

    /// Deletes a finished job.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("/api/v1/jobs/{}", seg(id)))).await
    }

    /// Per-queue counters.
    pub async fn stats(&self) -> Result<JobStats> {
        self.t.json(Req::get("/api/v1/jobs/stats")).await
    }

    /// Polls until the job is `succeeded`, `dead` or `cancelled`, or `timeout` passes (then an error).
    pub async fn wait(&self, id: &str, poll: Duration, timeout: Duration) -> Result<Job> {
        let end = Instant::now() + timeout;
        loop {
            let j = self.get(id).await?;
            if finished(&j) {
                return Ok(j);
            }
            if Instant::now() >= end {
                return Err(Error::Realtime {
                    code: "timeout".into(),
                    message: format!("job {id} did not finish within {timeout:?}"),
                });
            }
            tokio::time::sleep(poll).await;
        }
    }

    // ---- worker protocol (external workers) ----

    /// Claims up to `max` due jobs for `worker_id`. Each claim carries a `lease_id` for the calls below.
    pub async fn claim(
        &self,
        worker_id: &str,
        queues: &[&str],
        kinds: &[&str],
        max: Option<u32>,
    ) -> Result<ClaimResponse> {
        let body = json!({"worker_id": worker_id, "queues": queues, "kinds": kinds, "max": max});
        self.t.json(Req::post("/api/v1/jobs/claim").json_value(body)).await
    }

    /// Extends the lease and reports whether cancellation was requested.
    pub async fn heartbeat(&self, id: &str, lease_id: &str) -> Result<Heartbeat> {
        self.t
            .json(Req::post(format!("/api/v1/jobs/{}/heartbeat", seg(id))).json_value(json!({"lease_id": lease_id})))
            .await
    }

    /// Marks a claimed job succeeded with an optional result.
    pub async fn complete(&self, id: &str, lease_id: &str, result: Option<Value>) -> Result<Job> {
        self.t
            .json(
                Req::post(format!("/api/v1/jobs/{}/complete", seg(id)))
                    .json_value(json!({"lease_id": lease_id, "result": result})),
            )
            .await
    }

    /// Marks a claimed job failed; `retryable` jobs are re-queued with backoff until `max_attempts`.
    pub async fn fail(&self, id: &str, lease_id: &str, error: &str, retryable: bool) -> Result<Job> {
        let body = json!({"lease_id": lease_id, "error": error, "retryable": retryable});
        self.t
            .json(Req::post(format!("/api/v1/jobs/{}/fail", seg(id))).json_value(body))
            .await
    }

    // ---- long-running tasks: progress, checkpoints, results, attempts, children ----

    /// Reports progress of a claimed job (fields left `None` keep their value). Also renews the lease
    /// like a heartbeat, so a long task reporting progress never loses it.
    pub async fn report_progress(
        &self,
        id: &str,
        lease_id: &str,
        percent: Option<f64>,
        stage: Option<&str>,
        message: Option<&str>,
    ) -> Result<Heartbeat> {
        let body = json!({"lease_id": lease_id, "percent": percent, "stage": stage, "message": message});
        self.t
            .json(Req::post(format!("/api/v1/jobs/{}/progress", seg(id))).json_value(body))
            .await
    }

    /// The last checkpoint of a job (state to resume from after a crash or retry).
    pub async fn checkpoint(&self, id: &str) -> Result<Checkpoint> {
        self.t
            .json(Req::get(format!("/api/v1/jobs/{}/checkpoint", seg(id))))
            .await
    }

    /// Saves a checkpoint (JSON up to 256 KiB) under the lease. Renews the lease too.
    pub async fn save_checkpoint(&self, id: &str, lease_id: &str, checkpoint: Value) -> Result<Heartbeat> {
        let body = json!({"lease_id": lease_id, "checkpoint": checkpoint});
        self.t
            .json(Req::post(format!("/api/v1/jobs/{}/checkpoint", seg(id))).json_value(body))
            .await
    }

    /// The result of a succeeded job (large results are fetched from object storage by the server).
    pub async fn result(&self, id: &str) -> Result<JobResult> {
        self.t.json(Req::get(format!("/api/v1/jobs/{}/result", seg(id)))).await
    }

    /// Every attempt of a job, oldest first.
    pub async fn attempts(&self, id: &str) -> Result<Vec<Attempt>> {
        let v = self
            .t
            .value(Req::get(format!("/api/v1/jobs/{}/attempts", seg(id))))
            .await?;
        serde_json::from_value(v.get("items").cloned().unwrap_or(Value::Array(Vec::new())))
            .map_err(|e| Error::InvalidResponse(format!("attempts: {e}")))
    }

    /// Enqueues child jobs (1 to 100) from a running parent; they join the parent's workflow.
    pub async fn spawn_children(&self, id: &str, lease_id: &str, children: &[NewJob]) -> Result<Vec<Job>> {
        let body = json!({"lease_id": lease_id, "jobs": children});
        let v = self
            .t
            .value(Req::post(format!("/api/v1/jobs/{}/children", seg(id))).json_value(body))
            .await?;
        serde_json::from_value(v.get("items").cloned().unwrap_or(Value::Array(Vec::new())))
            .map_err(|e| Error::InvalidResponse(format!("children: {e}")))
    }

    /// Live progress and status of a job: subscribes to the realtime channel `jobs:<id>`.
    pub async fn watch(&self, id: &str) -> Result<JobWatch<JobUpdate>> {
        Ok(JobWatch {
            sub: self
                .realtime
                .subscribe(SubscribeOptions::channel(format!("jobs:{id}")))
                .await?,
            _marker: std::marker::PhantomData,
        })
    }

    /// Live status of a workflow and its steps.
    pub async fn watch_workflow(&self, id: &str) -> Result<JobWatch<WorkflowUpdate>> {
        Ok(JobWatch {
            sub: self
                .realtime
                .subscribe(SubscribeOptions::channel(format!("jobs:{id}")))
                .await?,
            _marker: std::marker::PhantomData,
        })
    }

    // ---- named queues ----

    /// Every queue with its settings and counters.
    pub async fn queues(&self) -> Result<Vec<QueueInfo>> {
        let v = self.t.value(Req::get("/api/v1/jobs/queues")).await?;
        serde_json::from_value(v.get("items").cloned().unwrap_or(Value::Array(Vec::new())))
            .map_err(|e| Error::InvalidResponse(format!("queues: {e}")))
    }

    /// One queue.
    pub async fn queue(&self, name: &str) -> Result<QueueInfo> {
        self.t
            .json(Req::get(format!("/api/v1/jobs/queues/{}", seg(name))))
            .await
    }

    /// Sets a queue's concurrency, rate limit, default priority and timeout (fields left `None` keep their value).
    pub async fn set_queue(&self, name: &str, settings: &QueueSettings) -> Result<QueueInfo> {
        self.t
            .json(Req::put(format!("/api/v1/jobs/queues/{}", seg(name))).json(settings)?)
            .await
    }

    /// Removes a queue's settings (the queue itself exists as long as it has jobs).
    pub async fn delete_queue(&self, name: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/jobs/queues/{}", seg(name))))
            .await
    }

    /// Stops handing out jobs of a queue (running jobs finish).
    pub async fn pause_queue(&self, name: &str) -> Result<QueueInfo> {
        self.t
            .json(Req::post(format!("/api/v1/jobs/queues/{}/pause", seg(name))))
            .await
    }

    /// Resumes a paused queue.
    pub async fn resume_queue(&self, name: &str) -> Result<QueueInfo> {
        self.t
            .json(Req::post(format!("/api/v1/jobs/queues/{}/resume", seg(name))))
            .await
    }

    // ---- workflows ----

    /// Creates a workflow: named steps with `depends_on` (chain, fan-out, fan-in) and a failure policy.
    pub async fn create_workflow(&self, wf: &NewWorkflow) -> Result<WorkflowDetail> {
        self.t.json(Req::post("/api/v1/jobs/workflows").json(wf)?).await
    }

    /// One workflow with its steps.
    pub async fn workflow(&self, id: &str) -> Result<WorkflowDetail> {
        self.t
            .json(Req::get(format!("/api/v1/jobs/workflows/{}", seg(id))))
            .await
    }

    /// A page of workflows (`status`: `running`, `succeeded`, `failed`, `compensating`, `compensated`, `cancelled`).
    pub async fn workflows(
        &self,
        status: Option<&str>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<WorkflowPage> {
        self.t
            .json(
                Req::get("/api/v1/jobs/workflows")
                    .q_opt("status", status)
                    .q_opt("limit", limit)
                    .q_opt("offset", offset),
            )
            .await
    }

    /// Cancels a running workflow and its unfinished steps.
    pub async fn cancel_workflow(&self, id: &str) -> Result<WorkflowDetail> {
        self.t
            .json(Req::post(format!("/api/v1/jobs/workflows/{}/cancel", seg(id))))
            .await
    }
}
