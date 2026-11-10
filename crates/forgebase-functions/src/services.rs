//! Engines a JavaScript function reaches through `ctx` (ADR-0027 §4).
//!
//! The server attaches them with [`crate::FunctionService::attach_services`]
//! once they are open; a service left out answers `<service>_unavailable`.
//! Queues come through the [`QueuePublisher`] trait because
//! `forgebase-queue` depends on this crate (push subscriptions run
//! functions).

use std::collections::BTreeMap;
use std::sync::Arc;

use forgebase_data::AccessContext;
use forgebase_db::BoxFuture;
use serde_json::Value as Json;

/// An error of an attached service, handed to the function as `{"code", "message"}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceError {
    /// Stable error code.
    pub code: String,
    /// Message the function may see.
    pub message: String,
}

impl ServiceError {
    /// A new error.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// A message to publish on a topic.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueMessage {
    /// Topic.
    pub topic: String,
    /// JSON body.
    pub data: Json,
    /// Ordering key.
    pub ordering_key: Option<String>,
    /// Attributes.
    pub attributes: BTreeMap<String, String>,
    /// Deduplication id.
    pub dedup_id: Option<String>,
}

/// Publishes queue messages as a caller (the queue engine authorises `queue_publish`).
pub trait QueuePublisher: Send + Sync + std::fmt::Debug {
    /// Publishes one message as `ctx`; returns the publish result as JSON.
    fn publish<'a>(&'a self, ctx: &'a AccessContext, msg: QueueMessage) -> BoxFuture<'a, Result<Json, ServiceError>>;
}

/// The engines behind `ctx.vector`, `ctx.storage`, `ctx.queues`, `ctx.jobs`,
/// `ctx.realtime` and `ctx.auth`.
#[derive(Clone, Default)]
pub struct Services {
    /// Vector collections.
    pub vector: Option<forgebase_vector::VectorService>,
    /// Object storage.
    pub storage: Option<forgebase_storage::StorageService>,
    /// Topics and queues.
    pub queue: Option<Arc<dyn QueuePublisher>>,
    /// Background jobs.
    pub jobs: Option<forgebase_jobs::JobService>,
    /// Realtime channels.
    pub realtime: Option<forgebase_realtime::RealtimeService>,
    /// Token verification (`ctx.auth.verify`).
    pub auth: Option<forgebase_auth::AuthService>,
    /// Effective permissions of callers (`ctx.auth.permissions`).
    pub policy: Option<forgebase_policy::PolicyService>,
}

impl std::fmt::Debug for Services {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Services")
            .field("vector", &self.vector.is_some())
            .field("storage", &self.storage.is_some())
            .field("queue", &self.queue.is_some())
            .field("jobs", &self.jobs.is_some())
            .field("realtime", &self.realtime.is_some())
            .field("auth", &self.auth.is_some())
            .field("policy", &self.policy.is_some())
            .finish()
    }
}

impl Services {
    /// Granted permission patterns of `ctx` in its realm and organization
    /// (empty without the policy engine).
    pub fn permissions(&self, ctx: &AccessContext) -> Vec<String> {
        let Some(p) = &self.policy else { return Vec::new() };
        if ctx.principal == forgebase_data::Principal::Anonymous {
            return Vec::new();
        }
        if ctx.is_privileged() {
            return vec!["*".to_owned()];
        }
        let snap = p.snapshot();
        let eff = snap.effective(&ctx.principal_key(), &ctx.realm_id, ctx.org_id.as_deref());
        eff.perms.allow.iter().cloned().collect()
    }
}
