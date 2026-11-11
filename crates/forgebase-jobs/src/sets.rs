//! Shard routing of the job service (ADR-0029 §3.5, FB-172).
//!
//! Jobs, schedules, workflows and the per-tenant queue settings live on the
//! database set of their tenant; instance-wide ones (no tenant) and the
//! worker registry on control. A [`JobService`] with a router resolves the
//! set per call:
//! - calls about a tenant (enqueue, lists, new schedules and workflows) go
//!   to the caller's tenant's set;
//! - calls about one job, schedule or workflow find its tenant by primary
//!   key (the bound set, the caller's set, then every set; remembered) and
//!   then go to that tenant's set, so a frozen tenant waits like a request;
//! - claims of a view ([`JobService::on_set`]) run on its set; an unbound
//!   privileged claim goes round-robin across sets;
//! - maintenance (schedules, expired leases, purge) runs once per set and
//!   skips the tenants that are moving ([`excluded_tenants`]).
//!
//! Without a router, or with one set, every call runs on the data core's
//! database exactly as before.

use std::collections::BTreeSet;
use std::sync::atomic::Ordering;

use forgebase_data::AccessContext;
use forgebase_db::topology::{CONTROL_SET_ID, PlacementState};
use forgebase_db::{Backend, DbRouter, SetHandle, Statement, TenantRef, Value};

use crate::service::{JobService, Result};

/// Remembered homes before the map starts over.
const MAX_HOMES: usize = 100_000;

/// Tables whose rows calls find by id.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Table {
    Jobs,
    Schedules,
    Workflows,
}

impl Table {
    fn name(self) -> &'static str {
        match self {
            Table::Jobs => "_forgebase_jobs",
            Table::Schedules => "_forgebase_job_schedules",
            Table::Workflows => "_forgebase_job_workflows",
        }
    }

    fn key(self, id: &str) -> String {
        let t = match self {
            Table::Jobs => 'j',
            Table::Schedules => 's',
            Table::Workflows => 'w',
        };
        format!("{t}:{id}")
    }
}

/// The tenants that set `set_id` must skip in claims and maintenance
/// (ADR-0029 §3.5 "Background loops", §3.8): a tenant `frozen` by a move's
/// switch is skipped everywhere; a tenant `moving` or in `cleanup` is
/// skipped on every set other than the one its placement names (the
/// target's partial copy while it moves, the source's stale copy until
/// cleanup). The set the placement names (the source while moving) keeps
/// serving it. Empty when nothing moves.
pub fn excluded_tenants(router: &DbRouter, set_id: &str) -> Vec<String> {
    router
        .placements()
        .into_iter()
        .filter(|p| match p.state {
            PlacementState::Frozen => true,
            PlacementState::Moving | PlacementState::Cleanup => p.set_id != set_id,
            _ => false,
        })
        .map(|p| p.tenant)
        .collect()
}

/// ` AND (<col> IS NULL OR <col> NOT IN (?n, …))` for `excluded`, pushing the
/// values to `params` (placeholders numbered `taken + params.len()`). Empty
/// when nothing is excluded, so unsharded SQL is unchanged.
pub fn tenant_guard(col: &str, excluded: &[String], params: &mut Vec<Value>, taken: usize) -> String {
    if excluded.is_empty() {
        return String::new();
    }
    let marks: Vec<String> = excluded
        .iter()
        .map(|t| {
            params.push(t.as_str().into());
            format!("?{}", taken + params.len())
        })
        .collect();
    format!(" AND ({col} IS NULL OR {col} NOT IN ({}))", marks.join(", "))
}

impl JobService {
    /// Routes calls over the database sets of `router` (FB-172). With one set
    /// nothing changes. Call once, before serving; a second call is ignored.
    pub fn attach_router(&self, router: DbRouter) {
        if self.inner.router.set(router).is_err() {
            tracing::warn!("job service: router attached twice; keeping the first");
        }
    }

    /// The router, if one is attached.
    pub fn router(&self) -> Option<&DbRouter> {
        self.inner.router.get()
    }

    /// The router when more than one set is in use.
    pub(crate) fn sharded(&self) -> Option<&DbRouter> {
        self.inner.router.get().filter(|r| r.is_sharded())
    }

    /// A view of this service bound to `set`: its claims, maintenance and
    /// wake-ups run on that set (the worker pool runs one claim loop per
    /// set). Calls about a tenant or about one job still go to the set of
    /// that tenant.
    #[must_use]
    pub fn on_set(&self, set: SetHandle) -> JobService {
        JobService {
            inner: self.inner.clone(),
            set: Some(set),
        }
    }

    /// The id of the set this view is bound to (control when unbound).
    pub fn set_id(&self) -> &str {
        self.set.as_ref().map_or(CONTROL_SET_ID, SetHandle::id)
    }

    /// Whether this view runs on control.
    pub(crate) fn on_control(&self) -> bool {
        self.set.as_ref().is_none_or(SetHandle::is_control)
    }

    /// The unbound service: control (workers, instance-wide rows).
    pub(crate) fn control_view(&self) -> JobService {
        JobService {
            inner: self.inner.clone(),
            set: None,
        }
    }

    /// One view per set when sharded and unbound (fan-outs and
    /// maintenance), else this view.
    pub(crate) fn views(&self) -> Vec<JobService> {
        match (self.sharded(), &self.set) {
            (Some(router), None) => router.sets().into_iter().map(|s| self.on_set(s)).collect(),
            _ => vec![self.clone()],
        }
    }

    /// The view of `tenant`'s set (control for none). Waits while the
    /// tenant is frozen by a move, then `409 tenant_moving`.
    pub(crate) async fn at_tenant(&self, tenant: Option<&str>) -> Result<JobService> {
        let Some(router) = self.sharded() else {
            return Ok(self.clone());
        };
        let set = router.for_tenant(TenantRef::new(tenant)).await?;
        Ok(self.on_set(set))
    }

    /// The view of the set of job `id`'s tenant.
    pub(crate) async fn at_job(&self, ctx: &AccessContext, id: &str) -> Result<JobService> {
        self.at_row(Table::Jobs, id, ctx.tenant.as_deref()).await
    }

    /// The view of the set of schedule `id`'s tenant.
    pub(crate) async fn at_schedule(&self, ctx: &AccessContext, id: &str) -> Result<JobService> {
        self.at_row(Table::Schedules, id, ctx.tenant.as_deref()).await
    }

    /// The view of the set of workflow `id`'s tenant.
    pub(crate) async fn at_workflow(&self, ctx: &AccessContext, id: &str) -> Result<JobService> {
        self.at_row(Table::Workflows, id, ctx.tenant.as_deref()).await
    }

    /// The view of the set of row `id`'s tenant. A row found nowhere gives
    /// this view (whose call then answers `not found`).
    async fn at_row(&self, table: Table, id: &str, hint: Option<&str>) -> Result<JobService> {
        let Some(router) = self.sharded() else {
            return Ok(self.clone());
        };
        let cached = self
            .inner
            .homes
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&table.key(id))
            .cloned();
        let tenant = match cached {
            Some(t) => t,
            None => match self.find_tenant(router, table, id, hint).await? {
                Some(t) => {
                    self.remember_home(table, id, t.as_deref());
                    t
                }
                None => return Ok(self.clone()),
            },
        };
        self.at_tenant(tenant.as_deref()).await
    }

    /// The tenant of row `id` (`Some(None)`: instance-wide), by primary key
    /// on the bound set, the hint's set, then every set. A set that cannot
    /// be read is skipped; its error is returned only when no set has the row.
    async fn find_tenant(
        &self,
        router: &DbRouter,
        table: Table,
        id: &str,
        hint: Option<&str>,
    ) -> Result<Option<Option<String>>> {
        let mut order: Vec<SetHandle> = Vec::new();
        if let Some(s) = &self.set {
            order.push(s.clone());
        }
        if let Ok(s) = router.try_for_tenant(TenantRef::new(hint)) {
            order.push(s);
        }
        order.extend(router.sets());
        let stmt = Statement::new(
            format!("SELECT tenant FROM {} WHERE id = ?1", table.name()),
            vec![id.into()],
        );
        let mut seen = BTreeSet::new();
        let mut failed = None;
        for set in order {
            if !seen.insert(set.id().to_owned()) {
                continue;
            }
            match set.fetch_all(&stmt).await {
                Ok(rows) if !rows.is_empty() => {
                    let tenant = rows.rows[0].first().and_then(Value::as_str).map(str::to_owned);
                    return Ok(Some(tenant));
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::debug!(set = %set.id(), error = %e, "job lookup skipped a set");
                    failed.get_or_insert(e);
                }
            }
        }
        match failed {
            Some(e) => Err(e.into()),
            None => Ok(None),
        }
    }

    /// Remembers the tenant of row `id` (sharded only; bounded).
    pub(crate) fn remember_home(&self, table: Table, id: &str, tenant: Option<&str>) {
        if self.sharded().is_none() {
            return;
        }
        let mut homes = self.inner.homes.lock().unwrap_or_else(|p| p.into_inner());
        if homes.len() >= MAX_HOMES {
            homes.clear();
        }
        homes.insert(table.key(id), tenant.map(str::to_owned));
    }

    /// The tenants this view's set skips ([`excluded_tenants`]).
    pub(crate) fn excluded(&self) -> Vec<String> {
        match self.sharded() {
            Some(router) => excluded_tenants(router, self.set_id()),
            None => Vec::new(),
        }
    }

    /// The sets in round-robin order for an unbound claim: each call starts
    /// one set further.
    pub(crate) fn claim_order(&self) -> Vec<JobService> {
        let views = self.views();
        if views.len() < 2 {
            return views;
        }
        let start = self.inner.turn.fetch_add(1, Ordering::Relaxed) % views.len();
        let mut ordered = views[start..].to_vec();
        ordered.extend_from_slice(&views[..start]);
        ordered
    }
}
