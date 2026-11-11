//! The move state machine: begin, run (prepare → copy → catch up →
//! switch), abort, cleanup, undo.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use forgebase_core::ErrorCode;

use crate::backend::{Backend, Statement, TxMode, Value};
use crate::router::{DbRouter, RouterError, SetHandle, TenantRef};
use crate::topology::{CONTROL_SET_ID, MoveState, NodeRole, PlacementState, SetState};
use crate::{Database, DbError};

use super::pgsql::{self, lit, qualified};
use super::plan::{self, Feed, MovePlan, TablePlan};
use super::sync::{self, digest_of, digest_sql};
use super::{MoveControl, MoveDetail, MoveError, MoveHooks, MoveOptions, MoveRecord, get, list, need_postgres, store};

/// What [`begin`] needs.
#[derive(Debug, Clone, Default)]
pub struct BeginMove {
    /// The tenant (environment id).
    pub tenant: String,
    /// Target set id.
    pub to: String,
    /// Administrator starting it.
    pub created_by: Option<String>,
    /// Rebalance plan it belongs to.
    pub rebalance_id: Option<String>,
    /// The move this one undoes.
    pub undo_of: Option<String>,
}

fn tenant_exists_sql() -> &'static str {
    "SELECT (SELECT count(*) FROM _forgebase_environments WHERE id = ?1) + \
            (SELECT count(*) FROM _forgebase_tenant_placements WHERE tenant = ?1)"
}

/// Records a new move of `req.tenant` to `req.to` (state `preparing`).
/// Runs a pending cleanup of the tenant's previous move first (so the
/// target holds no stale rows of it). Checks: PostgreSQL, the target is an
/// open set taking tenants, the tenant exists, is `active` (or in cleanup)
/// and has no open move, and no other move runs between the same two sets.
pub async fn begin(
    router: &DbRouter,
    hooks: &dyn MoveHooks,
    options: &MoveOptions,
    req: BeginMove,
) -> Result<MoveRecord, MoveError> {
    let control = router.control();
    need_postgres(control.database())?;
    let tenant = req.tenant.trim();
    if tenant.is_empty() {
        return Err(MoveError::Invalid("a move needs a tenant".into()));
    }
    let target = router.set(&req.to).ok_or_else(|| match router.topology().set(&req.to) {
        None => MoveError::Router(RouterError::UnknownSet(req.to.clone())),
        Some(_) => MoveError::Router(RouterError::SetUnavailable(req.to.clone())),
    })?;
    need_postgres(target.database())?;
    if target.set().state != SetState::Active || !target.set().hosts_tenants {
        return Err(MoveError::Router(RouterError::SetClosed(req.to.clone())));
    }
    let exists = pgsql::int(
        &control
            .fetch_all(&Statement::new(tenant_exists_sql(), vec![tenant.into()]))
            .await?,
    );
    if exists == 0 {
        return Err(MoveError::Invalid(format!("tenant {tenant:?} does not exist")));
    }
    let open = list(
        &control,
        &super::MoveQuery {
            open: true,
            limit: Some(1000),
            ..super::MoveQuery::default()
        },
    )
    .await?;
    // A previous move still keeping its source rows: clean them up now.
    if let Some(prev) = open.iter().find(|m| m.tenant == tenant && m.state == MoveState::Cleanup) {
        cleanup(router, hooks, options, &prev.id).await?;
    } else if open.iter().any(|m| m.tenant == tenant) {
        return Err(MoveError::TenantMoving(tenant.to_owned()));
    }
    let placement = router.placement(tenant);
    let from = placement
        .as_ref()
        .map_or_else(|| CONTROL_SET_ID.to_owned(), |p| p.set_id.clone());
    match placement.as_ref().map(|p| p.state) {
        None | Some(PlacementState::Active | PlacementState::Cleanup) => {}
        Some(_) => return Err(MoveError::TenantMoving(tenant.to_owned())),
    }
    if from == req.to {
        return Err(MoveError::Invalid(format!("tenant {tenant:?} is already on {}", req.to)));
    }
    if let Some(busy) = open
        .iter()
        .find(|m| m.tenant != tenant && m.from_set == from && m.to_set == req.to)
    {
        return Err(MoveError::InProgress(format!(
            "another move ({}) runs from {from} to {}; one move at a time per pair of sets",
            busy.tenant, req.to
        )));
    }
    let id = format!("mv_{}", uuid::Uuid::new_v4().simple());
    let detail = MoveDetail {
        undo_of: req.undo_of.clone(),
        ..MoveDetail::default()
    };
    let now = forgebase_core::now_rfc3339();
    let mut tx = control.begin(TxMode::Normal).await?;
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_db_moves (id, tenant, from_set, to_set, state, rebalance_id, detail, created_by, \
         created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'preparing', ?5, CAST(CAST(?6 AS TEXT) AS jsonb), ?7, ?8, ?8)",
        vec![
            id.clone().into(),
            tenant.into(),
            from.into(),
            req.to.clone().into(),
            req.rebalance_id.clone().into(),
            serde_json::to_string(&detail)
                .map_err(|e| DbError::Statement(e.to_string()))?
                .into(),
            req.created_by.clone().into(),
            now.into(),
        ],
    ))
    .await
    .map_err(|e| {
        if e.code() == "conflict" || e.to_string().contains("one_open") {
            MoveError::TenantMoving(tenant.to_owned())
        } else {
            MoveError::Db(e)
        }
    })?;
    tx.commit().await?;
    get(&control, &id).await
}

/// Starts a move back to the set a switched move came from (ADR-0029 §3.8
/// step 5: offered until the cleanup). The original's source rows are
/// cleaned up first; then it is an ordinary move.
pub async fn undo(
    router: &DbRouter,
    hooks: &dyn MoveHooks,
    options: &MoveOptions,
    id: &str,
    created_by: Option<String>,
) -> Result<MoveRecord, MoveError> {
    let control = router.control();
    let rec = get(&control, id).await?;
    if !rec.is_switched() {
        return Err(MoveError::State(format!(
            "move {id} is {}; only a switched move can be undone (abort an unfinished one)",
            rec.state
        )));
    }
    begin(
        router,
        hooks,
        options,
        BeginMove {
            tenant: rec.tenant.clone(),
            to: rec.from_set.clone(),
            created_by,
            rebalance_id: None,
            undo_of: Some(rec.id.clone()),
        },
    )
    .await
}

/// When a switched move's cleanup may run (RFC 3339), if switched.
pub fn cleanup_due(rec: &MoveRecord, options: &MoveOptions) -> Option<String> {
    let at = time::OffsetDateTime::parse(rec.switched_at.as_deref()?, &time::format_description::well_known::Rfc3339).ok()?;
    let due = at + options.source_retention;
    due.format(&time::format_description::well_known::Rfc3339).ok()
}

/// Set `id` as a handle over the hooks' administrative connection, or the
/// router's handle.
async fn admin_set(router: &DbRouter, hooks: &dyn MoveHooks, id: &str) -> Result<SetHandle, MoveError> {
    let real = router
        .set(id)
        .ok_or_else(|| MoveError::Router(RouterError::SetUnavailable(id.to_owned())))?;
    Ok(match hooks.admin(real.set()).await? {
        Some(db) => SetHandle::new(real.set().clone(), db),
        None => real,
    })
}

struct Ctx<'a> {
    router: &'a DbRouter,
    hooks: &'a dyn MoveHooks,
    control: &'a dyn MoveControl,
    options: &'a MoveOptions,
    control_set: SetHandle,
}

impl Ctx<'_> {
    async fn report(&self, stage: &str, percent: f64, message: String) {
        self.control.report(stage, percent, message).await;
    }

    /// Source and target, over the hooks' administrative connections when
    /// they give one.
    async fn sets(&self, rec: &MoveRecord) -> Result<(SetHandle, SetHandle), MoveError> {
        Ok((
            admin_set(self.router, self.hooks, &rec.from_set).await?,
            admin_set(self.router, self.hooks, &rec.to_set).await?,
        ))
    }

    async fn save(&self, rec: &MoveRecord, state: MoveState, detail: &MoveDetail) -> Result<MoveRecord, MoveError> {
        store(&self.control_set, &rec.id, state, detail, None).await
    }

    fn check_cancel(&self) -> Result<(), MoveError> {
        if self.control.cancelled() {
            Err(MoveError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Runs (or resumes) move `id` until it is switched (`cleanup`) or
/// `aborted`. A failure before the switch aborts the move (nothing visible
/// changed) and is returned, except for transient errors (a set
/// unreachable), which leave it to be resumed.
pub async fn run(
    router: &DbRouter,
    hooks: &dyn MoveHooks,
    control: &dyn MoveControl,
    options: &MoveOptions,
    id: &str,
) -> Result<MoveRecord, MoveError> {
    let ctx = Ctx {
        router,
        hooks,
        control,
        options,
        control_set: router.control(),
    };
    loop {
        let rec = get(&ctx.control_set, id).await?;
        let step = match rec.state {
            MoveState::Preparing => prepare(&ctx, &rec).await,
            MoveState::Copying => copy(&ctx, &rec).await,
            MoveState::CatchingUp => catch_up(&ctx, &rec).await,
            MoveState::Switching => switch(&ctx, &rec).await,
            MoveState::Cleanup | MoveState::Done | MoveState::Aborted => return Ok(rec),
        };
        if let Err(e) = step {
            if e.is_transient() {
                return Err(e);
            }
            // The flip may have happened in a step that failed later.
            let now = get(&ctx.control_set, id).await?;
            if now.is_switched() {
                return Err(e);
            }
            tracing::warn!(move_id = %id, tenant = %rec.tenant, error = %e, "move failed; aborting");
            abort_with(&ctx, &now, &e.to_string()).await?;
            return Err(e);
        }
    }
}

/// Aborts move `id` before its switch: the subscription, publication, slot
/// and copied rows go and the tenant is `active` on the source again.
pub async fn abort(router: &DbRouter, hooks: &dyn MoveHooks, options: &MoveOptions, id: &str, reason: &str) -> Result<MoveRecord, MoveError> {
    let quiet = super::Quiet;
    let ctx = Ctx {
        router,
        hooks,
        control: &quiet,
        options,
        control_set: router.control(),
    };
    let rec = get(&ctx.control_set, id).await?;
    if rec.is_finished() {
        return Ok(rec);
    }
    if rec.is_switched() || router.placement(&rec.tenant).is_some_and(|p| p.set_id == rec.to_set) {
        return Err(MoveError::State(format!(
            "move {id} is switched; undo it (a move back) instead"
        )));
    }
    abort_with(&ctx, &rec, reason).await
}

async fn abort_with(ctx: &Ctx<'_>, rec: &MoveRecord, reason: &str) -> Result<MoveRecord, MoveError> {
    let name = rec.object_name();
    let mut detail = rec.detail.clone();
    let source = admin_set(ctx.router, ctx.hooks, &rec.from_set).await.ok();
    let target = admin_set(ctx.router, ctx.hooks, &rec.to_set).await.ok();
    if let Some(target) = &target {
        pgsql::drop_subscription(target.database(), &name).await?;
    }
    // Unfreeze first: the tenant works on the source while the rest goes.
    ctx.router
        .place(&rec.tenant, &rec.from_set, PlacementState::Active)
        .await?;
    if let Err(e) = ctx.hooks.unfrozen(&rec.tenant).await {
        tracing::warn!(tenant = %rec.tenant, error = %e, "move abort: unfreeze hook failed");
    }
    if let (Some(target), Some(plan)) = (&target, detail.plan.clone()) {
        if detail.subscribed {
            delete_tenant_rows(target.database(), &plan, ctx.options.delete_batch, &detail.queues).await?;
        }
        for t in plan.of(Feed::Stage) {
            sync::drop_staging(target.database(), t).await?;
        }
        drop_indexes(target.database(), &detail.indexes).await;
        let _ = pgsql::clear_barrier(target, &rec.id).await;
    }
    if let Some(source) = &source {
        drop_replication(source.database(), &name).await?;
        restore_identity(ctx, rec, source.database(), &detail).await?;
        let _ = pgsql::clear_barrier(source, &rec.id).await;
    }
    detail.replication_dropped = true;
    let rec = store(&ctx.control_set, &rec.id, MoveState::Aborted, &detail, Some(reason)).await?;
    tracing::info!(move_id = %rec.id, tenant = %rec.tenant, reason, "move aborted");
    Ok(rec)
}

/// Deletes the tenant's rows of every planned table on `db`, deepest
/// first; whole tables (pgmq queues) are dropped.
async fn delete_tenant_rows(db: &Database, plan: &MovePlan, batch: u32, queues: &[String]) -> Result<u64, MoveError> {
    let mut tables: Vec<&TablePlan> = plan.tables.iter().filter(|t| t.feed != Feed::Whole).collect();
    tables.sort_by_key(|t| std::cmp::Reverse(t.depth));
    let mut total = 0;
    for t in tables {
        total += sync::delete_batched(db, &t.qualified(), &t.predicate, batch).await?;
    }
    for q in queues {
        db.run_autocommit(&[format!(
            "SELECT pgmq.drop_queue({}) FROM pgmq.meta WHERE queue_name = {}",
            lit(q),
            lit(q)
        )])
        .await?;
    }
    Ok(total)
}

async fn drop_replication(source: &Database, name: &str) -> Result<(), MoveError> {
    pgsql::wait_slot_inactive(source, name, Duration::from_secs(10)).await?;
    pgsql::drop_slot(source, name).await?;
    if pgsql::publication_exists(source, name).await? {
        source.run_autocommit(&[format!("DROP PUBLICATION IF EXISTS {name}")]).await?;
    }
    Ok(())
}

async fn drop_indexes(db: &Database, indexes: &[String]) {
    for i in indexes {
        if let Err(e) = db.run_autocommit(&[format!("DROP INDEX CONCURRENTLY IF EXISTS {i}")]).await {
            tracing::warn!(index = %i, error = %e, "move: index not dropped");
        }
    }
}

/// Restores replica identities set for this move, unless another open
/// move from the same source still needs them.
async fn restore_identity(ctx: &Ctx<'_>, rec: &MoveRecord, source: &Database, detail: &MoveDetail) -> Result<(), MoveError> {
    if detail.replica_identity.is_empty() {
        return Ok(());
    }
    let others = list(
        &ctx.control_set,
        &super::MoveQuery {
            open: true,
            limit: Some(1000),
            ..super::MoveQuery::default()
        },
    )
    .await?;
    let still: Vec<String> = others
        .iter()
        .filter(|m| m.id != rec.id && m.from_set == rec.from_set && !m.detail.replication_dropped)
        .filter_map(|m| m.detail.plan.as_ref())
        .flat_map(|p| p.tables.iter().filter(|t| t.identity_full).map(TablePlan::qualified))
        .collect();
    let stmts: Vec<String> = detail
        .replica_identity
        .iter()
        .filter(|(t, _)| !still.contains(t))
        .map(|(t, old)| pgsql::replica_identity_sql(t, old))
        .collect();
    if !stmts.is_empty() {
        source.run_autocommit(&stmts).await?;
    }
    Ok(())
}

async fn prepare(ctx: &Ctx<'_>, rec: &MoveRecord) -> Result<(), MoveError> {
    ctx.check_cancel()?;
    let (source, target) = ctx.sets(rec).await?;
    need_postgres(source.database())?;
    need_postgres(target.database())?;
    ctx.report("preparing", 1.0, format!("preparing the move of {} to {}", rec.tenant, target.name()))
        .await;
    if target.schema_version().await? < source.schema_version().await? {
        return Err(MoveError::State(format!(
            "{} is below the schema head of {}; wait for its migration",
            target.name(),
            source.name()
        )));
    }
    let placed = ctx.router.placement(&rec.tenant);
    if !placed
        .as_ref()
        .is_some_and(|p| p.set_id == rec.from_set && p.state == PlacementState::Moving)
    {
        ctx.router
            .place(&rec.tenant, &rec.from_set, PlacementState::Moving)
            .await?;
    }
    pgsql::ensure_move_schema(source.database()).await?;
    pgsql::ensure_move_schema(target.database()).await?;
    let plan = plan::build(&ctx.control_set, source.database(), target.database(), &rec.tenant, &rec.id).await?;
    let mut detail = rec.detail.clone();
    if detail.plan.is_none() {
        let mut busy = Vec::new();
        for t in &plan.tables {
            if t.feed != Feed::Whole && sync::count(target.database(), &t.qualified(), &t.predicate).await? > 0 {
                busy.push(t.name());
            }
        }
        if !busy.is_empty() {
            return Err(MoveError::TargetNotEmpty {
                tenant: rec.tenant.clone(),
                tables: busy,
            });
        }
    }
    let name = rec.object_name();
    // pgmq queues and the tenant's partial indexes on the target.
    if !plan.queues.is_empty() && !pgsql::table_exists(target.database(), "pgmq", "meta").await? {
        target
            .run_autocommit(&["CREATE EXTENSION IF NOT EXISTS pgmq".into()])
            .await
            .map_err(|e| MoveError::State(format!("{} has no pgmq extension: {e}", target.name())))?;
    }
    for q in &plan.queues {
        if !pgsql::table_exists(target.database(), "pgmq", &format!("q_{q}")).await? {
            target
                .run_autocommit(&[format!("SELECT pgmq.create({})", lit(q))])
                .await?;
        }
        if !detail.queues.contains(q) {
            detail.queues.push(q.clone());
        }
    }
    for t in plan.tables.iter().filter(|t| t.feed != Feed::Stage) {
        let have: Vec<String> = pgsql::indexes(target.database(), &t.schema, &t.table)
            .await?
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        for (n, def) in pgsql::indexes(source.database(), &t.schema, &t.table).await? {
            if have.contains(&n) {
                continue;
            }
            let Some(sql) = concurrent_index(&def) else { continue };
            if let Err(e) = target.run_autocommit(&[sql]).await {
                tracing::warn!(index = %n, error = %e, "move: index not built on the target");
                continue;
            }
            let q = qualified(&t.schema, &n);
            if !detail.indexes.contains(&q) {
                detail.indexes.push(q);
            }
        }
    }
    ctx.hooks.prepare_target(&rec.tenant, &source, &target).await?;
    // Row filters need their columns in the replica identity.
    for t in plan.tables.iter().filter(|t| t.identity_full) {
        let q = t.qualified();
        let old = pgsql::replica_identity(source.database(), &t.schema, &t.table).await?;
        if old != "f" {
            source
                .run_autocommit(&[pgsql::replica_identity_sql(&q, "f")])
                .await?;
            if !detail.replica_identity.iter().any(|(n, _)| *n == q) {
                detail.replica_identity.push((q, old));
            }
        }
    }
    if !pgsql::publication_exists(source.database(), &name).await? {
        source
            .run_autocommit(&[format!(
                "CREATE PUBLICATION {name} FOR TABLE {}",
                plan.publication_tables(&rec.id).join(", ")
            )])
            .await?;
    }
    let failover = has_replicas(ctx, &rec.from_set) && pgsql::supports_failover_slots(source.database()).await?;
    pgsql::create_slot(source.database(), &name, failover).await?;
    for t in plan.of(Feed::Stage) {
        sync::create_staging(target.database(), t).await?;
    }
    detail.plan = Some(plan);
    ctx.save(rec, MoveState::Copying, &detail).await?;
    Ok(())
}

fn has_replicas(ctx: &Ctx<'_>, set: &str) -> bool {
    ctx.router
        .topology()
        .nodes_of(set)
        .any(|n| n.role == NodeRole::Replica)
}

/// `CREATE [UNIQUE] INDEX CONCURRENTLY IF NOT EXISTS …` from an
/// `indexdef`; `None` for primary keys and constraint indexes (they come
/// with the table).
fn concurrent_index(def: &str) -> Option<String> {
    let (unique, rest) = if let Some(r) = def.strip_prefix("CREATE UNIQUE INDEX ") {
        ("UNIQUE ", r)
    } else {
        ("", def.strip_prefix("CREATE INDEX ")?)
    };
    Some(format!("CREATE {unique}INDEX CONCURRENTLY IF NOT EXISTS {rest}"))
}

fn plan_of(rec: &MoveRecord) -> Result<&MovePlan, MoveError> {
    rec.detail
        .plan
        .as_ref()
        .ok_or_else(|| MoveError::State(format!("move {} has no plan", rec.id)))
}

async fn check_subscription(target: &Database, name: &str) -> Result<pgsql::SubState, MoveError> {
    let st = pgsql::subscription_state(target, name).await?;
    if !st.exists {
        return Err(MoveError::Replication(format!("subscription {name} is missing on the target")));
    }
    if !st.enabled {
        let (apply, sync) = pgsql::subscription_errors(target, name).await.unwrap_or((0, 0));
        return Err(MoveError::Replication(format!(
            "subscription {name} stopped after an error ({apply} apply, {sync} copy errors; see the target's server log)"
        )));
    }
    Ok(st)
}

async fn copy(ctx: &Ctx<'_>, rec: &MoveRecord) -> Result<(), MoveError> {
    let (source, target) = ctx.sets(rec).await?;
    let name = rec.object_name();
    let mut detail = rec.detail.clone();
    if !detail.subscribed {
        let conninfo = ctx.hooks.source_conninfo(source.set()).await?;
        let failover = has_replicas(ctx, &rec.from_set) && pgsql::supports_failover_slots(source.database()).await?;
        pgsql::create_subscription(target.database(), &name, &conninfo, failover).await?;
        detail.subscribed = true;
        ctx.save(rec, MoveState::Copying, &detail).await?;
    }
    let plan = plan_of(rec)?.clone();
    for t in plan.of(Feed::Stage) {
        ctx.check_cancel()?;
        let s = sync::sync_staged(&source, target.database(), t).await?;
        detail.rows.insert(t.name(), s.rows);
    }
    let started = Instant::now();
    loop {
        ctx.check_cancel()?;
        let st = check_subscription(target.database(), &name).await?;
        if st.tables > 0 && st.ready >= st.tables {
            break;
        }
        if started.elapsed() > ctx.options.copy_timeout {
            return Err(MoveError::Replication(format!(
                "the initial copy did not finish within {} s",
                ctx.options.copy_timeout.as_secs()
            )));
        }
        let bytes = pgsql::copy_bytes(&source).await.unwrap_or(0);
        let frac = if st.tables > 0 {
            st.ready as f64 / st.tables as f64
        } else {
            0.0
        };
        ctx.report(
            "copying",
            5.0 + 55.0 * frac,
            format!("{} of {} tables copied ({} MiB in flight)", st.ready, st.tables, bytes >> 20),
        )
        .await;
        tokio::time::sleep(ctx.options.poll).await;
    }
    ctx.save(rec, MoveState::CatchingUp, &detail).await?;
    Ok(())
}

async fn catch_up(ctx: &Ctx<'_>, rec: &MoveRecord) -> Result<(), MoveError> {
    let (source, target) = ctx.sets(rec).await?;
    let name = rec.object_name();
    let plan = plan_of(rec)?.clone();
    let mut detail = rec.detail.clone();
    for t in plan.of(Feed::Stage) {
        ctx.check_cancel()?;
        let s = sync::sync_staged(&source, target.database(), t).await?;
        detail.rows.insert(t.name(), s.rows);
    }
    let started = Instant::now();
    loop {
        ctx.check_cancel()?;
        check_subscription(target.database(), &name).await?;
        let lag = pgsql::slot_lag(&source, &name).await?.unwrap_or(i64::MAX);
        if u64::try_from(lag).unwrap_or(u64::MAX) <= ctx.options.max_lag_bytes {
            break;
        }
        if started.elapsed() > ctx.options.copy_timeout {
            return Err(MoveError::Replication(format!(
                "catching up did not finish within {} s",
                ctx.options.copy_timeout.as_secs()
            )));
        }
        ctx.report("catching_up", 70.0, format!("{} KiB behind", lag >> 10))
            .await;
        tokio::time::sleep(ctx.options.poll).await;
    }
    ctx.report("catching_up", 85.0, "caught up; switching".into()).await;
    ctx.save(rec, MoveState::Switching, &detail).await?;
    Ok(())
}

struct Freeze {
    start: Instant,
    deadline: Instant,
    parts: BTreeMap<String, u64>,
    mark: Instant,
    limit: Duration,
}

impl Freeze {
    fn new(limit: Duration) -> Self {
        let now = Instant::now();
        Self {
            start: now,
            deadline: now + limit,
            parts: BTreeMap::new(),
            mark: now,
            limit,
        }
    }

    fn remaining(&self) -> Result<Duration, MoveError> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(MoveError::FreezeTimeout(self.limit.as_millis()))
    }

    fn lap(&mut self, part: &str) -> Result<(), MoveError> {
        let now = Instant::now();
        let ms = u64::try_from(now.duration_since(self.mark).as_millis()).unwrap_or(u64::MAX);
        *self.parts.entry(part.to_owned()).or_insert(0) += ms;
        self.mark = now;
        self.remaining().map(|_| ())
    }

    fn total_ms(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

async fn within<T>(f: &Freeze, fut: impl std::future::Future<Output = Result<T, MoveError>>) -> Result<T, MoveError> {
    match tokio::time::timeout(f.remaining()?, fut).await {
        Ok(r) => r,
        Err(_) => Err(MoveError::FreezeTimeout(f.limit.as_millis())),
    }
}

async fn switch(ctx: &Ctx<'_>, rec: &MoveRecord) -> Result<(), MoveError> {
    let (source, target) = ctx.sets(rec).await?;
    let name = rec.object_name();
    let plan = plan_of(rec)?.clone();
    let mut detail = rec.detail.clone();
    // A crash after the flip: finish the switch.
    if ctx
        .router
        .placement(&rec.tenant)
        .is_some_and(|p| p.set_id == rec.to_set)
    {
        let rec = ctx.save(rec, MoveState::Cleanup, &detail).await?;
        return after_switch(ctx, &rec, &source, &target).await;
    }
    ctx.check_cancel()?;
    check_subscription(target.database(), &name).await?;
    let mut f = Freeze::new(ctx.options.freeze_timeout);
    ctx.router
        .place(&rec.tenant, &rec.from_set, PlacementState::Frozen)
        .await?;
    let result = frozen_switch(ctx, rec, &plan, &source, &target, &mut f, &mut detail).await;
    if let Err(e) = result {
        // Unfreeze at once; `run` aborts the move.
        ctx.router
            .place(&rec.tenant, &rec.from_set, PlacementState::Moving)
            .await?;
        detail.freeze_ms = Some(f.total_ms());
        detail.freeze_parts = f.parts.clone();
        let _ = store(&ctx.control_set, &rec.id, MoveState::Switching, &detail, None).await;
        return Err(e);
    }
    detail.freeze_ms = Some(f.total_ms());
    detail.freeze_parts = f.parts;
    let rec = ctx.save(rec, MoveState::Cleanup, &detail).await?;
    tracing::info!(
        move_id = %rec.id,
        tenant = %rec.tenant,
        from = %rec.from_set,
        to = %rec.to_set,
        freeze_ms = rec.detail.freeze_ms.unwrap_or(0),
        "tenant moved"
    );
    after_switch(ctx, &rec, &source, &target).await
}

#[allow(clippy::too_many_arguments)]
async fn frozen_switch(
    ctx: &Ctx<'_>,
    rec: &MoveRecord,
    plan: &MovePlan,
    source: &SetHandle,
    target: &SetHandle,
    f: &mut Freeze,
    detail: &mut MoveDetail,
) -> Result<(), MoveError> {
    let tenant = rec.tenant.as_str();
    let name = rec.object_name();
    within(f, ctx.hooks.frozen(tenant)).await?;
    f.lap("hooks")?;
    if !ctx.router.wait_drained(tenant, f.remaining()?).await {
        return Err(MoveError::FreezeTimeout(f.limit.as_millis()));
    }
    f.lap("drain")?;
    within(f, ctx.hooks.drain(tenant, source)).await?;
    f.lap("relay")?;
    // Barrier: everything the source committed before it is applied once
    // it is visible on the target.
    let n = i64::try_from(time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1000).unwrap_or(1);
    pgsql::write_barrier(source, &rec.id, n).await?;
    loop {
        if pgsql::barrier_arrived(target, &rec.id, n).await? {
            break;
        }
        let st = pgsql::subscription_state(target, &name).await?;
        if !st.enabled {
            return Err(MoveError::Replication(format!("subscription {name} stopped before the barrier")));
        }
        f.remaining()?;
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    target
        .run_autocommit(&[format!("ALTER SUBSCRIPTION {name} DISABLE")])
        .await?;
    f.lap("barrier")?;
    // Rows of containers created during the move (not in the filters).
    for t in plan.of(Feed::Publish) {
        let Some(c) = &t.container else { continue };
        let Some(parent) = plan.table(&format!("{}.{}", plan::SYSTEM_SCHEMA, c.table)) else {
            continue;
        };
        let rows = source
            .fetch_all(&Statement::raw(format!(
                "SELECT CAST({} AS text), pg_typeof({})::text FROM {} WHERE {}",
                pgsql::ident(&c.key),
                pgsql::ident(&c.key),
                parent.qualified(),
                parent.predicate
            )))
            .await?;
        let late: Vec<String> = rows
            .rows
            .iter()
            .filter_map(|r| {
                let v = r.first().and_then(Value::as_str)?;
                let ty = r.get(1).and_then(Value::as_str).unwrap_or("text");
                Some(if matches!(ty, "bigint" | "integer" | "smallint") {
                    v.to_owned()
                } else {
                    lit(v)
                })
            })
            .filter(|k| !c.listed.contains(k))
            .collect();
        if !late.is_empty() {
            let pred = format!("{} IN ({})", pgsql::ident(&c.column), late.join(", "));
            let n = sync::copy_direct(source, target.database(), t, &pred).await?;
            tracing::debug!(table = %t.name(), rows = n, "move: rows of new containers copied in the freeze");
        }
    }
    f.lap("late")?;
    for t in plan.of(Feed::Stage) {
        within(f, async {
            sync::sync_staged(source, target.database(), t)
                .await
                .map_err(MoveError::from)
        })
        .await?;
    }
    f.lap("staged")?;
    // Source digests (the tenant is quiet).
    let mut expected = Vec::new();
    for t in plan.tables.iter().filter(|t| t.feed != Feed::Stage) {
        let n = sync::count(source, &t.qualified(), &t.predicate).await?;
        let sample = u64::try_from(n).unwrap_or(0) > ctx.options.verify_full_rows;
        let d = digest_of(
            &source
                .fetch_all(&Statement::raw(digest_sql(t, &t.qualified(), &t.predicate, sample)))
                .await?,
        );
        expected.push((t, n, sample, d));
    }
    let mut staged = Vec::new();
    for t in plan.of(Feed::Stage) {
        staged.push((t, sync::count(target, &sync::staging_table(t), "true").await?));
    }
    f.lap("verify_source")?;
    let mut tx = target.begin(TxMode::Normal).await?;
    let written = super::switch::apply(tx.as_mut(), plan).await?;
    for (t, n) in &staged {
        let got = written.get(&t.name()).copied().unwrap_or(0);
        if i64::try_from(got).unwrap_or(-1) != *n {
            return Err(MoveError::Verification {
                table: t.name(),
                detail: format!("{n} staged rows, {got} inserted"),
            });
        }
        detail.rows.insert(t.name(), got);
    }
    for (t, n, sample, want) in &expected {
        let got = digest_of(
            &tx.fetch_all(&Statement::raw(digest_sql(t, &t.qualified(), &t.predicate, *sample)))
                .await?,
        );
        let count = sync::tx_int(
            tx.as_mut(),
            format!("SELECT count(*) FROM {} WHERE {}", t.qualified(), t.predicate),
        )
        .await?;
        if count != *n {
            return Err(MoveError::Verification {
                table: t.name(),
                detail: format!("{n} rows on the source, {count} on the target"),
            });
        }
        if got != *want {
            return Err(MoveError::Verification {
                table: t.name(),
                detail: format!(
                    "{} digest differs ({} rows compared)",
                    if *sample { "1 % sample" } else { "SHA-256" },
                    want.0
                ),
            });
        }
        detail.rows.insert(t.name(), u64::try_from(*n).unwrap_or(0));
    }
    f.lap("switch_tx")?;
    tx.commit().await?;
    // The flip: one control transaction, epoch + 1; waiting requests go to
    // the target now.
    ctx.router
        .place(tenant, &rec.to_set, PlacementState::Cleanup)
        .await?;
    let ms = u64::try_from(f.mark.elapsed().as_millis()).unwrap_or(0);
    *f.parts.entry("flip".into()).or_insert(0) += ms;
    Ok(())
}

/// After the flip: hooks, then the replication objects go (the source rows
/// stay until the cleanup).
async fn after_switch(ctx: &Ctx<'_>, rec: &MoveRecord, source: &SetHandle, target: &SetHandle) -> Result<(), MoveError> {
    if let Err(e) = ctx.hooks.switched(&rec.tenant, source, target).await {
        tracing::warn!(tenant = %rec.tenant, error = %e, "move: a service did not take the switch");
    }
    let mut detail = rec.detail.clone();
    if detail.replication_dropped {
        return Ok(());
    }
    let name = rec.object_name();
    pgsql::drop_subscription(target.database(), &name).await?;
    drop_replication(source.database(), &name).await?;
    restore_identity(ctx, rec, source.database(), &detail).await?;
    if let Some(plan) = &detail.plan {
        for t in plan.of(Feed::Stage) {
            sync::drop_staging(target.database(), t).await?;
        }
    }
    let _ = pgsql::clear_barrier(source, &rec.id).await;
    let _ = pgsql::clear_barrier(target, &rec.id).await;
    detail.replication_dropped = true;
    ctx.report(
        "cleanup",
        95.0,
        format!(
            "switched in {} ms; the source keeps the rows for {} h",
            detail.freeze_ms.unwrap_or(0),
            ctx.options.source_retention.as_secs() / 3600
        ),
    )
    .await;
    store(&ctx.control_set, &rec.id, MoveState::Cleanup, &detail, None).await?;
    Ok(())
}

/// Deletes the tenant's rows on the source of switched move `id` (in
/// batches), drops the indexes copied for it there, and makes the
/// placement `active` on the target. The caller runs it after the
/// retention ([`cleanup_due`]); [`begin`] runs it early for a move back.
pub async fn cleanup(router: &DbRouter, hooks: &dyn MoveHooks, options: &MoveOptions, id: &str) -> Result<MoveRecord, MoveError> {
    let control = router.control();
    let rec = get(&control, id).await?;
    match rec.state {
        MoveState::Done | MoveState::Aborted => return Ok(rec),
        MoveState::Cleanup => {}
        other => {
            return Err(MoveError::State(format!(
                "move {id} is {other}; only a switched move is cleaned up"
            )));
        }
    }
    let source = admin_set(router, hooks, &rec.from_set).await?;
    let mut detail = rec.detail.clone();
    if let Some(plan) = detail.plan.clone() {
        let n = delete_tenant_rows(source.database(), &plan, options.delete_batch, &detail.queues).await?;
        detail.cleaned_rows = Some(n);
    }
    drop_indexes(source.database(), &detail.indexes).await;
    if !detail.replication_dropped {
        let name = rec.object_name();
        drop_replication(source.database(), &name).await?;
    }
    if router
        .placement(&rec.tenant)
        .is_some_and(|p| p.set_id == rec.to_set && p.state == PlacementState::Cleanup)
    {
        router
            .place(&rec.tenant, &rec.to_set, PlacementState::Active)
            .await?;
    }
    let rec = store(&control, &rec.id, MoveState::Done, &detail, None).await?;
    tracing::info!(move_id = %rec.id, tenant = %rec.tenant, rows = rec.detail.cleaned_rows.unwrap_or(0), "move cleaned up");
    Ok(rec)
}

/// What [`run_plan`] did.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RebalanceOutcome {
    /// Moves run (switched, or aborted with their error).
    pub moves: Vec<MoveRecord>,
    /// Tenants skipped (no longer on the planned source, or moving).
    pub skipped: Vec<String>,
    /// The first failure, which stopped the plan: `(tenant, error)`.
    pub failed: Option<(String, String)>,
}

/// Runs a rebalance or drain plan: its moves one at a time
/// (`max_parallel_moves = 1`), each through [`begin`] and [`run`] with the
/// plan's id. A tenant no longer on its planned source is skipped; the
/// first failing move stops the plan; cancel stops it between moves.
pub async fn run_plan(
    router: &DbRouter,
    hooks: &dyn MoveHooks,
    control: &dyn MoveControl,
    options: &MoveOptions,
    plan: &super::RebalancePlan,
    created_by: Option<String>,
) -> Result<RebalanceOutcome, MoveError> {
    let mut out = RebalanceOutcome::default();
    let total = plan.moves.len().max(1);
    for (i, m) in plan.moves.iter().enumerate() {
        if control.cancelled() {
            break;
        }
        let here = router
            .placement(&m.tenant)
            .map_or_else(|| CONTROL_SET_ID.to_owned(), |p| p.set_id);
        if here != m.from {
            out.skipped.push(m.tenant.clone());
            continue;
        }
        control
            .report(
                "moving",
                100.0 * i as f64 / total as f64,
                format!("move {} of {}: {} to {}", i + 1, plan.moves.len(), m.tenant, m.to),
            )
            .await;
        let rec = match begin(
            router,
            hooks,
            options,
            BeginMove {
                tenant: m.tenant.clone(),
                to: m.to.clone(),
                created_by: created_by.clone(),
                rebalance_id: Some(plan.id.clone()),
                undo_of: None,
            },
        )
        .await
        {
            Ok(r) => r,
            Err(MoveError::TenantMoving(_)) => {
                out.skipped.push(m.tenant.clone());
                continue;
            }
            Err(e) => {
                out.failed = Some((m.tenant.clone(), e.to_string()));
                break;
            }
        };
        let quiet = super::Quiet;
        match run(router, hooks, &quiet, options, &rec.id).await {
            Ok(r) => out.moves.push(r),
            Err(e) => {
                out.moves.push(get(&router.control(), &rec.id).await?);
                out.failed = Some((m.tenant.clone(), e.to_string()));
                break;
            }
        }
    }
    Ok(out)
}

/// Waits for `tenant`'s routing to settle on a set (used by tests and the
/// server after a move).
pub async fn routed_set(router: &DbRouter, tenant: &str) -> Result<String, MoveError> {
    Ok(router.for_tenant(TenantRef::Tenant(tenant)).await?.id().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexes_are_rebuilt_concurrently() {
        assert_eq!(
            concurrent_index("CREATE INDEX q_x_leased ON pgmq.q_x USING btree (vt) WHERE (x)").as_deref(),
            Some("CREATE INDEX CONCURRENTLY IF NOT EXISTS q_x_leased ON pgmq.q_x USING btree (vt) WHERE (x)")
        );
        assert_eq!(
            concurrent_index("CREATE UNIQUE INDEX u ON t USING btree (a)").as_deref(),
            Some("CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS u ON t USING btree (a)")
        );
        assert!(concurrent_index("ALTER TABLE x").is_none());
    }

    #[test]
    fn cleanup_is_due_after_the_retention() {
        let rec = MoveRecord {
            id: "mv_1".into(),
            tenant: "t".into(),
            from_set: "control".into(),
            to_set: "s".into(),
            state: MoveState::Cleanup,
            job_id: None,
            rebalance_id: None,
            detail: MoveDetail::default(),
            error: None,
            created_by: None,
            created_at: String::new(),
            updated_at: String::new(),
            switched_at: Some("2026-11-11T10:00:00Z".into()),
            finished_at: None,
        };
        let due = cleanup_due(&rec, &MoveOptions::default()).unwrap();
        assert!(due.starts_with("2026-11-12T10:00:00"), "{due}");
    }

    #[test]
    fn freeze_counts_its_parts() {
        let mut f = Freeze::new(Duration::from_secs(5));
        f.lap("a").unwrap();
        assert!(f.parts.contains_key("a"));
        assert!(f.remaining().is_ok());
        let mut g = Freeze::new(Duration::from_nanos(1));
        std::thread::sleep(Duration::from_millis(2));
        assert!(matches!(g.lap("x"), Err(MoveError::FreezeTimeout(_))));
    }
}
