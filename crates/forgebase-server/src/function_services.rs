//! The engines JavaScript functions reach through `ctx` (FB-93, ADR-0027).

use forgebase_data::AccessContext;
use forgebase_db::BoxFuture;
use forgebase_functions::{QueueMessage, QueuePublisher, ServiceError, Services};
use forgebase_queue::{NewMessage, QueueService};
use serde_json::Value as Json;

/// `ctx.queues.publish` through the queue engine (which authorises `queue_publish`).
#[derive(Debug, Clone)]
pub(crate) struct QueueAdapter(pub(crate) QueueService);

impl QueuePublisher for QueueAdapter {
    fn publish<'a>(&'a self, ctx: &'a AccessContext, msg: QueueMessage) -> BoxFuture<'a, Result<Json, ServiceError>> {
        Box::pin(async move {
            let mut m = NewMessage::new(msg.data);
            m.attributes = msg.attributes;
            m.ordering_key = msg.ordering_key;
            m.dedup_id = msg.dedup_id;
            let published = self.0.publish(ctx, &msg.topic, vec![m]).await.map_err(|e| {
                use forgebase_core::ErrorCode;
                let message = if e.kind().is_client_visible() {
                    e.to_string()
                } else {
                    "internal error".to_owned()
                };
                ServiceError::new(e.code(), message)
            })?;
            let first = published
                .into_iter()
                .next()
                .ok_or_else(|| ServiceError::new("internal_error", "nothing was published"))?;
            serde_json::to_value(&first).map_err(|e| ServiceError::new("internal_error", e.to_string()))
        })
    }
}

/// Every engine a function may use.
#[allow(clippy::too_many_arguments)]
pub(crate) fn services(
    vector: &forgebase_vector::VectorService,
    storage: &forgebase_storage::StorageService,
    queue: Option<&QueueService>,
    jobs: &forgebase_jobs::JobService,
    realtime: &forgebase_realtime::RealtimeService,
    auth: &forgebase_auth::AuthService,
    policy: &forgebase_policy::PolicyService,
) -> Services {
    Services {
        vector: Some(vector.clone()),
        storage: Some(storage.clone()),
        queue: queue.map(|q| std::sync::Arc::new(QueueAdapter(q.clone())) as std::sync::Arc<dyn QueuePublisher>),
        jobs: Some(jobs.clone()),
        realtime: Some(realtime.clone()),
        auth: Some(auth.clone()),
        policy: Some(policy.clone()),
    }
}
