//! The `vector.convert` job kind (ADR-0029 §1.4, FB-162).

use forgebase_data::AccessContext;
use forgebase_jobs::{HandlerError, JobContext, JobRegistry};

use crate::error::VectorError;
use crate::model::VectorEngine;
use crate::service::VectorService;

/// Job kind that moves the canonical vectors between the portable engine
/// and pgvector (payload `{"to": "pgvector" | "portable"}`, and on a
/// sharded cluster optionally `"set": "<set id>"` to convert one database
/// set only, FB-172); reserved: only the system and privileged callers may
/// enqueue it. A set that serves a tenant frozen by a move answers `409
/// tenant_moving` and the job is retried.
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
            let set = match ctx.payload().get("set") {
                None | Some(serde_json::Value::Null) => None,
                Some(serde_json::Value::String(s)) => Some(s.clone()),
                Some(_) => return Err(HandlerError::fatal("\"set\" must be a set id")),
            };
            match svc.convert_set(&AccessContext::system(), set.as_deref(), to).await {
                Ok(r) => Ok(serde_json::to_value(r).unwrap_or_default()),
                Err(
                    e @ (VectorError::Invalid { .. } | VectorError::Router(forgebase_db::RouterError::UnknownSet(_))),
                ) => Err(HandlerError::fatal(e.to_string())),
                Err(e) => Err(HandlerError::retry(e.to_string())),
            }
        }
    })
}
