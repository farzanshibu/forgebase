//! Job handlers (forgebase-jobs, ADR-0007):
//!
//! * `entitlements.alert` delivers one usage alert (payload `{"alert_id"}`);
//!   enqueued after the transaction that raised it, idempotent by alert id.
//! * `entitlements.maintenance` runs [`EntitlementService::run_maintenance`]
//!   (period resets with rollover, expired reservations, leases and trials,
//!   lost alert jobs); the schedule `entitlements-maintenance` fires it every
//!   minute.

use forgebase_data::AccessContext;
use forgebase_jobs::{HandlerError, JobContext, JobError, JobRegistry, NewSchedule};
use serde_json::json;

use crate::error::Result;
use crate::service::{ALERT_JOB, EntitlementService, MAINTENANCE_JOB, MAINTENANCE_SCHEDULE};

/// Registers the two handlers.
pub fn register_jobs(registry: JobRegistry, svc: EntitlementService) -> JobRegistry {
    let alerts = svc.clone();
    registry
        .register(ALERT_JOB, move |ctx: JobContext| {
            let svc = alerts.clone();
            async move {
                let id = ctx
                    .payload()
                    .get("alert_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| HandlerError::fatal("payload needs alert_id"))?
                    .to_owned();
                match svc.deliver_alert(&id).await {
                    Ok(delivered) => Ok(json!({ "delivered": delivered })),
                    Err(crate::EntitlementError::NotFound(_)) => Err(HandlerError::fatal("no such alert")),
                    Err(e) => Err(HandlerError::retry(e.to_string())),
                }
            }
        })
        .register(MAINTENANCE_JOB, move |_ctx: JobContext| {
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

impl EntitlementService {
    /// Creates the maintenance schedule (every minute) unless it exists.
    pub async fn ensure_schedule(&self) -> Result<()> {
        let Some(jobs) = self.inner.jobs.get() else {
            return Ok(());
        };
        let new = NewSchedule {
            name: MAINTENANCE_SCHEDULE.to_owned(),
            cron: "0 * * * * *".to_owned(),
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
            Err(e) => Err(crate::EntitlementError::Internal(format!("maintenance schedule: {e}"))),
        }
    }
}
