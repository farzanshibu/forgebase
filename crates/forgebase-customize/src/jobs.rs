//! Job handler (forgebase-jobs, ADR-0007): `customize.email.send` delivers one
//! queued message. A transient failure is returned as a retry (the queue backs
//! off and calls again, up to the job's attempt limit); a permanent failure or
//! the last attempt ends the job and marks the delivery `failed`.

use forgebase_jobs::{HandlerError, JobContext, JobRegistry};
use serde_json::json;

use crate::model::Attempt;
use crate::service::{CustomizeService, EMAIL_JOB};

/// Registers the delivery handler.
pub fn register_jobs(registry: JobRegistry, svc: CustomizeService) -> JobRegistry {
    registry.register(EMAIL_JOB, move |ctx: JobContext| {
        let svc = svc.clone();
        async move {
            let Some(id) = ctx
                .payload()
                .get("delivery_id")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
            else {
                return Err(HandlerError::fatal("payload has no delivery_id"));
            };
            match svc
                .deliver(&id, ctx.attempt(), ctx.is_last_attempt())
                .await
                .map_err(|e| HandlerError::retry(e.to_string()))?
            {
                Attempt::Sent => Ok(json!({"delivery_id": id, "status": "sent"})),
                Attempt::Retry(m) => Err(HandlerError::retry(m)),
                Attempt::Failed(m) => Err(HandlerError::fatal(m)),
            }
        }
    })
}
