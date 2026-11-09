//! Job handler (forgebase-jobs, ADR-0007): `billing.maintenance` runs
//! [`BillingService::run_maintenance`] — usage ingestion, renewals and trial
//! ends, the dunning schedule (retries, `past_due`, final cancellation), late
//! fulfillment and entitlement syncs — and the schedule
//! `billing-maintenance` fires it every minute.

use forgebase_data::AccessContext;
use forgebase_jobs::{HandlerError, JobContext, JobError, JobRegistry, NewSchedule};
use serde_json::json;

use crate::error::{BillingError, Result};
use crate::service::{BillingService, MAINTENANCE_JOB, MAINTENANCE_SCHEDULE};

/// Registers the maintenance handler.
pub fn register_jobs(registry: JobRegistry, svc: BillingService) -> JobRegistry {
    registry.register(MAINTENANCE_JOB, move |_ctx: JobContext| {
        let svc = svc.clone();
        async move {
            let report = svc
                .run_maintenance()
                .await
                .map_err(|e| HandlerError::retry(e.to_string()))?;
            Ok(serde_json::to_value(report).unwrap_or_default())
        }
    })
}

impl BillingService {
    /// Creates the maintenance schedule (every minute) unless it exists.
    pub async fn ensure_schedule(&self) -> Result<()> {
        let Some(jobs) = self.inner.jobs.get() else {
            return Ok(());
        };
        let new = NewSchedule {
            name: MAINTENANCE_SCHEDULE.to_owned(),
            cron: "15 * * * * *".to_owned(),
            kind: MAINTENANCE_JOB.to_owned(),
            queue: None,
            payload: json!({}),
            priority: Some(5),
            max_attempts: Some(3),
            backoff_base_ms: None,
            backoff_max_ms: None,
            visibility_timeout_ms: None,
            enabled: Some(true),
        };
        match jobs.create_schedule(&AccessContext::system(), new).await {
            Ok(_) | Err(JobError::ScheduleExists(_)) => Ok(()),
            Err(e) => Err(BillingError::Internal(format!("maintenance schedule: {e}"))),
        }
    }
}
