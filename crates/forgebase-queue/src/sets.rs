//! Shard routing of the queue service (ADR-0029 §3.5, FB-172).
//!
//! A topic lives on the database set of its tenant (instance-wide topics on
//! control); a subscription and its deliveries, dead letters and snapshots
//! live with its topic, so a tenant's subscription to an instance-wide
//! topic stays on control. A name resolves as on one set (the caller's
//! tenant resource first, else the instance-wide one): the tenant's set is
//! asked for `<tenant>/<name>`, else control. Listings of a tenant caller
//! read its set and control. Background work (maintenance, the record
//! relay, push delivery) runs once per set and skips the tenants that are
//! moving ([`forgebase_jobs::excluded_tenants`]). A record-source topic on
//! control relays the outbox of every set, with one cursor per set stored on
//! control in the transaction of its inserts (dedup id
//! `outbox:<set number>:<seq>`).
//!
//! Without a router, or with one set, every call runs on the data core's
//! database exactly as before.

use std::sync::Arc;

use forgebase_db::{Backend, DbRouter, Rows, SetHandle, Statement, TenantRef};

use crate::access::stored;
use crate::service::{Inner, QueueService, Result};

impl QueueService {
    /// The unbound service over `inner` (control).
    pub(crate) fn unbound(inner: Arc<Inner>) -> Self {
        let engine = Self::engine_slot(&inner, forgebase_db::topology::CONTROL_SET_ID);
        Self {
            inner,
            set: None,
            engine,
        }
    }

    fn engine_slot(inner: &Inner, set_id: &str) -> Arc<crate::pgmq::EngineSlot> {
        inner
            .engines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(set_id.to_owned())
            .or_insert_with(|| Arc::new(tokio::sync::RwLock::new(None)))
            .clone()
    }

    /// Routes calls over the database sets of `router` (FB-172). With one set
    /// nothing changes. Call once, before serving; a second call is ignored.
    pub fn attach_router(&self, router: DbRouter) {
        if self.inner.router.set(router).is_err() {
            tracing::warn!("queue service: router attached twice; keeping the first");
        }
    }

    /// The router, if one is attached.
    pub fn router(&self) -> Option<&DbRouter> {
        self.inner.router.get()
    }

    pub(crate) fn sharded(&self) -> Option<&DbRouter> {
        self.inner.router.get().filter(|r| r.is_sharded())
    }

    /// A view of this service bound to `set` (its maintenance, relay and
    /// push delivery run on that set).
    #[must_use]
    pub fn on_set(&self, set: SetHandle) -> QueueService {
        let engine = Self::engine_slot(&self.inner, set.id());
        QueueService {
            inner: self.inner.clone(),
            set: Some(set),
            engine,
        }
    }

    /// The id of the set this view is bound to (control when unbound).
    pub fn set_id(&self) -> &str {
        self.set
            .as_ref()
            .map_or(forgebase_db::topology::CONTROL_SET_ID, SetHandle::id)
    }

    pub(crate) fn on_control(&self) -> bool {
        self.set.as_ref().is_none_or(SetHandle::is_control)
    }

    /// The unbound service: control.
    pub(crate) fn control_view(&self) -> QueueService {
        Self::unbound(self.inner.clone())
    }

    /// One view per set when sharded and unbound, else this view.
    pub(crate) fn views(&self) -> Vec<QueueService> {
        match (self.sharded(), &self.set) {
            (Some(router), None) => router.sets().into_iter().map(|s| self.on_set(s)).collect(),
            _ => vec![self.clone()],
        }
    }

    /// The view of `tenant`'s set (control for none).
    pub(crate) async fn at_tenant(&self, tenant: Option<&str>) -> Result<QueueService> {
        let Some(router) = self.sharded() else {
            return Ok(self.clone());
        };
        Ok(self.on_set(router.for_tenant(TenantRef::new(tenant)).await?))
    }

    /// The view where topic `name` resolves for a caller in `tenant`.
    pub(crate) async fn at_topic(&self, tenant: Option<&str>, name: &str) -> Result<QueueService> {
        self.at_named("_forgebase_queue_topics", tenant, name).await
    }

    /// The view where subscription `name` resolves for a caller in `tenant`.
    pub(crate) async fn at_sub(&self, tenant: Option<&str>, name: &str) -> Result<QueueService> {
        self.at_named("_forgebase_queue_subscriptions", tenant, name).await
    }

    /// The view where snapshot `name` resolves for a caller in `tenant`.
    pub(crate) async fn at_snapshot(&self, tenant: Option<&str>, name: &str) -> Result<QueueService> {
        self.at_named("_forgebase_queue_snapshots", tenant, name).await
    }

    /// The view of the subscription stored as `stored_name`.
    pub(crate) async fn at_stored_sub(&self, stored_name: &str) -> Result<QueueService> {
        match stored_name.split_once('/') {
            Some((tenant, name)) => self.at_sub(Some(tenant), name).await,
            None => Ok(if self.sharded().is_some() {
                self.control_view()
            } else {
                self.clone()
            }),
        }
    }

    /// The tenant's set when it holds `<tenant>/<name>` in `table`, else
    /// control (instance-wide resources, and a tenant's subscriptions to
    /// them). Waits while the tenant is frozen by a move.
    async fn at_named(&self, table: &str, tenant: Option<&str>, name: &str) -> Result<QueueService> {
        let Some(router) = self.sharded() else {
            return Ok(self.clone());
        };
        let Some(t) = tenant.filter(|t| !t.is_empty()) else {
            return Ok(self.control_view());
        };
        let set = router.for_tenant(TenantRef::Tenant(t)).await?;
        if set.is_control() {
            return Ok(self.control_view());
        }
        let rows = set
            .fetch_all(&Statement::new(
                format!("SELECT 1 FROM {table} WHERE name = ?1"),
                vec![stored(Some(t), name).into()],
            ))
            .await?;
        Ok(if rows.is_empty() {
            self.control_view()
        } else {
            self.on_set(set)
        })
    }

    /// Reads a listing in a tenant caller's scope: its set's rows and, on a
    /// shard, control's (the instance-wide ones).
    pub(crate) async fn fetch_listing(&self, stmt: &Statement) -> Result<Rows> {
        let mut rows = self.backend().fetch_all(stmt).await?;
        if self.sharded().is_some() && !self.on_control() {
            let more = self.control_view().backend().fetch_all(stmt).await?;
            rows.rows.extend(more.rows);
        }
        Ok(rows)
    }

    /// The tenants this view's set skips in background work.
    pub(crate) fn excluded(&self) -> Vec<String> {
        match self.sharded() {
            Some(router) => forgebase_jobs::excluded_tenants(router, self.set_id()),
            None => Vec::new(),
        }
    }
}
