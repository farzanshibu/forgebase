//! Scheduled and background functions through the durable job queue (FB-61).
//!
//! The job kind [`INVOKE_JOB`] runs a function with the job's payload
//! `{"function": "<name>", "input": <any JSON>}`; the function's JSON output
//! becomes the job result. A job schedule (cron) of this kind is a scheduled
//! function; enqueueing one is a background task or queue consumer. Retries,
//! backoff and the dead-letter queue are the job queue's: a timeout or busy
//! engine is retried, a missing function or a function fault is not.

use forgebase_jobs::{HandlerError, JobContext, JobRegistry};
use serde_json::{Value as Json, json};

use crate::FunctionService;
use crate::error::{ExecError, FunctionError};
use crate::model::Trigger;

/// Job kind that invokes a function.
pub const INVOKE_JOB: &str = "function.invoke";

/// Registers the [`INVOKE_JOB`] handler.
pub fn register_jobs(registry: JobRegistry, svc: FunctionService) -> JobRegistry {
    registry.register(INVOKE_JOB, move |ctx: JobContext| {
        let svc = svc.clone();
        async move {
            let name = ctx
                .payload()
                .get("function")
                .and_then(Json::as_str)
                .ok_or_else(|| HandlerError::fatal("payload needs \"function\""))?
                .to_owned();
            let input = json!({
                "trigger": "job",
                "job_id": ctx.job().id,
                "attempt": ctx.attempt(),
                "input": ctx.payload().get("input").cloned().unwrap_or(Json::Null),
            });
            match svc.invoke(&name, Trigger::Job, &input, None).await {
                Ok(out) => Ok(out
                    .json()
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| json!({"version": out.version}))),
                Err(FunctionError::NotFound(n)) => Err(HandlerError::fatal(format!("no function {n:?}"))),
                Err(e @ (FunctionError::Busy(_) | FunctionError::Execution(ExecError::Timeout(_)))) => {
                    Err(HandlerError::retry(e.to_string()))
                }
                Err(e @ FunctionError::Execution(_)) => Err(HandlerError::fatal(e.to_string())),
                Err(e) => Err(HandlerError::retry(e.to_string())),
            }
        }
    })
}
