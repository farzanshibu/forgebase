//! The `vector.convert` job kind (ADR-0029 §1.4, FB-162).

use forgebase_data::AccessContext;
use forgebase_jobs::{HandlerError, JobContext, JobRegistry};

use crate::error::VectorError;
use crate::model::VectorEngine;
use crate::service::VectorService;

/// Job kind that moves the canonical vectors between the portable engine
/// and pgvector (payload `{"to": "pgvector" | "portable"}`); reserved: only
/// the system and privileged callers may enqueue it.
pub const CONVERT_JOB: &str = "vector.convert";

/// Registers the [`CONVERT_JOB`] handler.
#[must_use]
pub fn register_jobs(registry: JobRegistry, svc: VectorService) -> JobRegistry {
    registry.register(CONVERT_JOB, move |ctx: JobContext| {
        let svc = svc.clone();
        async move {
            let Some(to) = ctx
                .payload()
                .get("to")
                .and_then(|v| v.as_str())
                .and_then(VectorEngine::parse)
            else {
                return Err(HandlerError::fatal(
                    "payload needs \"to\": \"pgvector\" or \"portable\"",
                ));
            };
            match svc.convert(&AccessContext::system(), to).await {
                Ok(r) => Ok(serde_json::to_value(r).unwrap_or_default()),
                Err(e @ VectorError::Invalid { .. }) => Err(HandlerError::fatal(e.to_string())),
                Err(e) => Err(HandlerError::retry(e.to_string())),
            }
        }
    })
}
