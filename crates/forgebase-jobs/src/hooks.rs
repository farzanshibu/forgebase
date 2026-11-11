//! Where the job queue meets other services: live updates on FB-40 realtime
//! channels, who may subscribe to them, and the store for large results
//! (FB-30 object storage, wired by the server).

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use forgebase_data::schema::Collection;
use forgebase_data::{AccessContext, Action, Authorizer, Decision, FieldAccess, ResourceRef, ResourceSchema};
use forgebase_db::BoxFuture;
use serde_json::Value as Json;

use crate::access::{JobAccess, Scope};

/// Prefix of the realtime channels the job queue publishes on:
/// `jobs:<job id>` and `jobs:<workflow id>`.
pub const CHANNEL_PREFIX: &str = "jobs:";

/// The channel of a job or workflow id.
pub fn channel(id: &str) -> String {
    format!("{CHANNEL_PREFIX}{id}")
}

/// Receives live updates (a [`crate::JobUpdate`] or [`crate::WorkflowUpdate`]
/// as JSON) after the change committed, one at a time and in commit order.
/// Should return quickly (hand off slow work).
pub trait JobEventSink: Send + Sync + fmt::Debug {
    /// Publishes `data` on `channel`.
    fn publish(&self, channel: String, data: Json) -> BoxFuture<'_, ()>;
}

/// Publishes on FB-40 realtime channels as the system principal, so every
/// subscriber of `jobs:<id>` gets them (subject to [`JobChannelAuthorizer`]).
#[derive(Debug, Clone)]
pub struct RealtimeSink(pub forgebase_realtime::RealtimeService);

impl JobEventSink for RealtimeSink {
    fn publish(&self, channel: String, data: Json) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            // In-process delivery; the cross-instance fan-out is spawned by realtime.
            if let Err(e) = self.0.publish(&AccessContext::system(), &channel, data).await {
                tracing::debug!(%channel, error = %e, "job update not published");
            }
        })
    }
}

/// Collects updates in memory (tests, embedding).
#[derive(Debug, Clone, Default)]
pub struct MemorySink {
    events: Arc<Mutex<Vec<(String, Json)>>>,
}

impl MemorySink {
    /// An empty sink.
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything published so far, oldest first.
    pub fn events(&self) -> Vec<(String, Json)> {
        self.events.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The updates published on `channel`.
    pub fn on(&self, channel: &str) -> Vec<Json> {
        self.events()
            .into_iter()
            .filter(|(c, _)| c == channel)
            .map(|(_, d)| d)
            .collect()
    }
}

impl JobEventSink for MemorySink {
    fn publish(&self, channel: String, data: Json) -> BoxFuture<'_, ()> {
        self.events
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push((channel, data));
        Box::pin(async {})
    }
}

/// Wraps the process's authorizer so the job channels are filtered: a
/// subscription to `jobs:<id>` needs [`Action::ReadJobs`] (not just
/// `ChannelSubscribe`) and, for non-privileged callers, the job or workflow
/// must be visible to them (FB-142: their tenant, and created by them unless
/// they hold [`crate::READ_ALL_KEY`]). Nobody but the system publishes
/// there, so updates cannot be forged. Every other question goes to the
/// inner authorizer.
///
/// The check is synchronous, so it reads the owners the [`JobAccess`] state
/// remembers: jobs and workflows enqueued, published, read or listed since
/// the process started (up to 100 000). A non-privileged subscriber to a job
/// not seen since a restart is refused until it reads the job
/// (`GET /api/v1/jobs/{id}`) or lists it; the server shares the state with
/// its [`crate::JobService`] ([`crate::JobService::attach_access`]).
#[derive(Debug, Clone)]
pub struct JobChannelAuthorizer {
    inner: Arc<dyn Authorizer>,
    access: JobAccess,
}

impl JobChannelAuthorizer {
    /// Wraps `inner` with fresh [`JobAccess`] state.
    pub fn new(inner: Arc<dyn Authorizer>) -> Self {
        Self::with_access(inner, JobAccess::new())
    }

    /// Wraps `inner`, checking visibility with `access`.
    pub fn with_access(inner: Arc<dyn Authorizer>, access: JobAccess) -> Self {
        Self { inner, access }
    }

    /// The shared state: hand it to the [`crate::JobService`].
    pub fn access(&self) -> JobAccess {
        self.access.clone()
    }

    /// Whether `ctx` may see the job or workflow of `channel`.
    fn visible(&self, ctx: &AccessContext, channel: &str) -> bool {
        let scope = self.access.scope(ctx);
        if scope == Scope::All {
            return true;
        }
        let id = &channel[CHANNEL_PREFIX.len()..];
        self.access.owner(id).is_some_and(|o| scope.sees(&o, None))
    }
}

impl Authorizer for JobChannelAuthorizer {
    fn authorize(&self, ctx: &AccessContext, action: Action, collection: Option<&Collection>) -> Decision {
        self.inner.authorize(ctx, action, collection)
    }

    fn authorize_channel(&self, ctx: &AccessContext, action: Action, channel: &str) -> Decision {
        if !channel.starts_with(CHANNEL_PREFIX) {
            return self.inner.authorize_channel(ctx, action, channel);
        }
        if ctx.is_system() {
            return Decision::Allow;
        }
        match action {
            Action::ChannelSubscribe => {
                // Both: the channel rule and permission to read jobs.
                if !matches!(self.inner.authorize_channel(ctx, action, channel), Decision::Allow) {
                    return Decision::Deny;
                }
                match self.inner.authorize(ctx, Action::ReadJobs, None) {
                    Decision::Allow if self.visible(ctx, channel) => Decision::Allow,
                    _ => Decision::Deny,
                }
            }
            _ => Decision::Deny,
        }
    }

    fn authorize_resource(&self, ctx: &AccessContext, action: Action, resource: &ResourceRef) -> Decision {
        self.inner.authorize_resource(ctx, action, resource)
    }

    fn register_resource_schema(&self, schema: ResourceSchema) {
        self.inner.register_resource_schema(schema);
    }

    fn field_access(&self, ctx: &AccessContext, collection: &Collection) -> FieldAccess {
        self.inner.field_access(ctx, collection)
    }
}

/// An error of a [`ResultStore`].
pub type StoreError = String;

/// Keeps job results that are too large for the job row. The server backs it
/// with FB-30 object storage; [`MemoryResultStore`] is for tests.
pub trait ResultStore: Send + Sync + fmt::Debug {
    /// Stores `bytes` (JSON) under `key`, replacing what was there.
    fn put(&self, key: String, bytes: Vec<u8>) -> BoxFuture<'_, Result<(), StoreError>>;
    /// The bytes under `key`, `None` if there are none.
    fn get(&self, key: String) -> BoxFuture<'_, Result<Option<Vec<u8>>, StoreError>>;
    /// Removes `key` (no error if it is missing).
    fn delete(&self, key: String) -> BoxFuture<'_, Result<(), StoreError>>;
}

/// A [`ResultStore`] in memory.
#[derive(Debug, Clone, Default)]
pub struct MemoryResultStore {
    objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
}

impl MemoryResultStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stored keys, sorted.
    pub fn keys(&self) -> Vec<String> {
        let mut k: Vec<String> = self
            .objects
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .cloned()
            .collect();
        k.sort();
        k
    }
}

impl ResultStore for MemoryResultStore {
    fn put(&self, key: String, bytes: Vec<u8>) -> BoxFuture<'_, Result<(), StoreError>> {
        self.objects
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, bytes);
        Box::pin(async { Ok(()) })
    }

    fn get(&self, key: String) -> BoxFuture<'_, Result<Option<Vec<u8>>, StoreError>> {
        let v = self
            .objects
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key)
            .cloned();
        Box::pin(async move { Ok(v) })
    }

    fn delete(&self, key: String) -> BoxFuture<'_, Result<(), StoreError>> {
        self.objects.lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
        Box::pin(async { Ok(()) })
    }
}
