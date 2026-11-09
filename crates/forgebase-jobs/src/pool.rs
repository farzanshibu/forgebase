//! The in-process worker pool and scheduler.
//!
//! One background task polls the queue for the kinds in the [`JobRegistry`],
//! runs up to `concurrency` jobs at once, keeps each job's lease alive with
//! heartbeats, fires due cron schedules, re-queues expired leases and purges old
//! finished jobs. [`PoolHandle::shutdown`] stops claiming, waits for running
//! jobs up to a grace period, then aborts the rest and *releases* their leases
//! (the attempt is not counted), so a restart picks them up at once. If the
//! process dies instead, the visibility timeout does the same later.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use forgebase_data::AccessContext;
use tokio::sync::{Semaphore, watch};
use tokio::task::{AbortHandle, JoinHandle, JoinSet};

use crate::JobError;
use crate::model::{ClaimRequest, ClaimedJob};
use crate::registry::{HandlerError, JobContext, JobRegistry};
use crate::service::JobService;

/// Worker pool settings.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Jobs run at once.
    pub concurrency: usize,
    /// Only these queues (empty: all).
    pub queues: Vec<String>,
    /// How long an idle pool waits before polling again (an enqueue in this
    /// process wakes it sooner).
    pub poll_interval: Duration,
    /// How often schedules and expired leases are checked.
    pub maintenance_interval: Duration,
    /// How long shutdown waits for running jobs before releasing them.
    pub shutdown_grace: Duration,
    /// Succeeded and cancelled jobs older than this are deleted (`None`: keep).
    pub retention: Option<Duration>,
    /// Worker id recorded on claimed jobs.
    pub worker_id: String,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            concurrency: 4,
            queues: Vec::new(),
            poll_interval: Duration::from_millis(500),
            maintenance_interval: Duration::from_millis(250),
            shutdown_grace: Duration::from_secs(10),
            retention: Some(Duration::from_secs(7 * 24 * 3600)),
            worker_id: format!("inproc-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
        }
    }
}

/// A running pool.
#[derive(Debug)]
pub struct PoolHandle {
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl PoolHandle {
    /// Stops claiming, waits for running jobs (up to the configured grace),
    /// releases what is left and returns.
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take()
            && let Err(e) = task.await
        {
            tracing::warn!(error = %e, "job pool task failed");
        }
    }

    /// Kills the pool without releasing leases, like a crashed process
    /// (visibility timeouts recover the jobs). For tests.
    #[doc(hidden)]
    pub async fn crash(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for PoolHandle {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

/// Starts the scheduler and, if the registry has handlers, the worker pool.
pub fn start_pool(service: JobService, registry: JobRegistry, config: PoolConfig) -> PoolHandle {
    let (stop, stop_rx) = watch::channel(false);
    let task = tokio::spawn(run(service, registry, config, stop_rx));
    PoolHandle { stop, task: Some(task) }
}

/// Aborts the wrapped task when dropped.
struct AbortOnDrop(AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

type InFlight = Arc<Mutex<HashMap<String, String>>>;

async fn run(service: JobService, registry: JobRegistry, config: PoolConfig, mut stop: watch::Receiver<bool>) {
    let system = AccessContext::system();
    let kinds = registry.kinds();
    let concurrency = config.concurrency.max(1);
    let permits = Arc::new(Semaphore::new(concurrency));
    let in_flight: InFlight = Arc::default();
    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut last_maintenance = std::time::Instant::now() - config.maintenance_interval;
    let mut last_purge = std::time::Instant::now();
    tracing::info!(worker_id = %config.worker_id, concurrency, kinds = ?kinds, "job pool started");

    loop {
        if *stop.borrow() {
            break;
        }
        while tasks.try_join_next().is_some() {}

        if last_maintenance.elapsed() >= config.maintenance_interval {
            last_maintenance = std::time::Instant::now();
            if let Err(e) = service.fire_due_schedules(&system).await {
                tracing::error!(error = %e, "firing schedules failed");
            }
            if let Err(e) = service.reap_expired(&system).await {
                tracing::error!(error = %e, "re-queueing expired leases failed");
            }
            if let Some(retention) = config.retention
                && last_purge.elapsed() >= Duration::from_secs(60)
            {
                last_purge = std::time::Instant::now();
                let cutoff = service.now() - i64::try_from(retention.as_millis()).unwrap_or(i64::MAX);
                match service.purge_finished(&system, cutoff).await {
                    Ok(n) if n > 0 => tracing::info!(purged = n, "purged finished jobs"),
                    Ok(_) => {}
                    Err(e) => tracing::error!(error = %e, "purging finished jobs failed"),
                }
            }
        }

        let mut claimed_any = false;
        if !kinds.is_empty() {
            let free = permits.available_permits();
            if free > 0 {
                let req = ClaimRequest {
                    worker_id: config.worker_id.clone(),
                    queues: config.queues.clone(),
                    kinds: kinds.clone(),
                    max: Some(i64::try_from(free.min(50)).unwrap_or(1)),
                };
                match service.claim(&system, req).await {
                    Ok(jobs) => {
                        claimed_any = !jobs.is_empty();
                        for claimed in jobs {
                            let Ok(permit) = permits.clone().try_acquire_owned() else {
                                // Cannot happen (we asked for at most `free`); give it back.
                                let _ = service.release(&system, &claimed.job.id, &claimed.lease_id).await;
                                continue;
                            };
                            in_flight
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .insert(claimed.job.id.clone(), claimed.lease_id.clone());
                            let (service, registry, in_flight) = (service.clone(), registry.clone(), in_flight.clone());
                            tasks.spawn(async move {
                                run_job(&service, &registry, claimed, &in_flight).await;
                                drop(permit);
                            });
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "claiming jobs failed"),
                }
            }
        }
        if claimed_any {
            continue;
        }
        // Idle (or saturated): wait for a wake-up, a finishing job, the next
        // maintenance round, the poll interval or shutdown.
        let nap = config
            .poll_interval
            .min(config.maintenance_interval.max(Duration::from_millis(10)));
        tokio::select! {
            () = service.work_available() => {}
            () = tokio::time::sleep(nap) => {}
            _ = stop.changed() => {}
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }

    // Graceful shutdown: no new claims; let running jobs finish for a while.
    if !tasks.is_empty() {
        tracing::info!(running = tasks.len(), grace = ?config.shutdown_grace, "waiting for running jobs");
        let drained = tokio::time::timeout(config.shutdown_grace, async {
            while tasks.join_next().await.is_some() {}
        })
        .await;
        if drained.is_err() {
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
            let leftover: Vec<(String, String)> = in_flight.lock().unwrap_or_else(|p| p.into_inner()).drain().collect();
            for (id, lease) in leftover {
                match service.release(&system, &id, &lease).await {
                    Ok(_) => tracing::info!(job = %id, "released unfinished job"),
                    Err(e) => tracing::warn!(job = %id, error = %e, "could not release job"),
                }
            }
        }
    }
    tracing::info!("job pool stopped");
}

/// Runs one claimed job: heartbeats while the handler works, then reports.
async fn run_job(service: &JobService, registry: &JobRegistry, claimed: ClaimedJob, in_flight: &InFlight) {
    let system = AccessContext::system();
    let ClaimedJob { lease_id, job } = claimed;
    let id = job.id.clone();
    let Some(handler) = registry.get(&job.kind) else {
        // Claimed by kind, so this is a registry race; fail it as retryable.
        report(
            service,
            &system,
            &id,
            &lease_id,
            Err(HandlerError::retry("no handler registered")),
        )
        .await;
        in_flight.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
        return;
    };

    let (cancel_tx, cancel_rx) = watch::channel(false);
    let beat_every = Duration::from_millis(u64::try_from((job.visibility_timeout_ms / 3).max(20)).unwrap_or(20));
    let heartbeat = {
        let (service, id, lease) = (service.clone(), id.clone(), lease_id.clone());
        tokio::spawn(async move {
            let system = AccessContext::system();
            loop {
                tokio::time::sleep(beat_every).await;
                match service.heartbeat(&system, &id, &lease).await {
                    Ok(hb) if hb.cancel_requested => {
                        let _ = cancel_tx.send(true);
                    }
                    Ok(_) => {}
                    Err(JobError::LeaseLost(_)) => {
                        tracing::warn!(job = %id, "lease lost; telling the handler to stop");
                        let _ = cancel_tx.send(true);
                        return;
                    }
                    Err(e) => tracing::warn!(job = %id, error = %e, "heartbeat failed"),
                }
            }
        })
    };
    let _heartbeat_guard = AbortOnDrop(heartbeat.abort_handle());

    // The handler runs in its own task so a panic is caught and reported.
    let work = tokio::spawn(handler.handle(JobContext::new(job, cancel_rx)));
    let _work_guard = AbortOnDrop(work.abort_handle());
    let outcome = match work.await {
        Ok(result) => result,
        Err(e) if e.is_panic() => Err(HandlerError::retry("handler panicked")),
        Err(_) => Err(HandlerError::retry("handler task was cancelled")),
    };
    heartbeat.abort();
    report(service, &system, &id, &lease_id, outcome).await;
    in_flight.lock().unwrap_or_else(|p| p.into_inner()).remove(&id);
}

async fn report(
    service: &JobService,
    ctx: &AccessContext,
    id: &str,
    lease: &str,
    outcome: Result<serde_json::Value, HandlerError>,
) {
    let result = match outcome {
        Ok(value) => service.complete(ctx, id, lease, Some(value)).await,
        Err(HandlerError::Retry(msg)) => service.fail(ctx, id, lease, &msg, true).await,
        Err(HandlerError::Fatal(msg)) => service.fail(ctx, id, lease, &msg, false).await,
    };
    match result {
        Ok(job) => tracing::debug!(job = %id, status = job.status.as_str(), "job reported"),
        Err(JobError::LeaseLost(_)) => {
            tracing::warn!(job = %id, "job finished after its lease was lost; result discarded");
        }
        Err(e) => tracing::error!(job = %id, error = %e, "could not report job outcome"),
    }
}
