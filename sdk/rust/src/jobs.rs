//! Durable jobs: enqueue, status, wait, cancel, retry, and the worker protocol.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::generated::{ClaimResponse, Heartbeat, Job, JobEvent, JobStats, JobStatus, NewJob};
use crate::http::{Req, Transport, seg};

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
}
