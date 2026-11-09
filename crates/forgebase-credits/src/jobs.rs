//! Job handler (forgebase-jobs, ADR-0007): `credits.maintenance` runs
//! [`CreditService::run_maintenance`] — expiry of lapsed credits, then
//! charging new usage events — and the schedule `credits-maintenance` fires
//! it every minute.

use forgebase_data::AccessContext;
use forgebase_jobs::{HandlerError, JobContext, JobError, JobRegistry, NewSchedule};
use serde_json::json;

use crate::error::{CreditError, Result};
use crate::service::{CreditService, MAINTENANCE_JOB, MAINTENANCE_SCHEDULE};

/// Registers the maintenance handler.
pub fn register_jobs(registry: JobRegistry, svc: CreditService) -> JobRegistry {
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

impl CreditService {
    /// Creates the maintenance schedule (every minute) unless it exists.
    pub async fn ensure_schedule(&self) -> Result<()> {
        let Some(jobs) = self.inner.jobs.get() else {
            return Ok(());
        };
        let new = NewSchedule {
            name: MAINTENANCE_SCHEDULE.to_owned(),
            cron: "30 * * * * *".to_owned(),
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
            Err(e) => Err(CreditError::Internal(format!("maintenance schedule: {e}"))),
        }
    }
}
