//! Rebalancing (ADR-0029 §3.9): per-tenant statistics, a greedy plan, and
//! drain plans.
//!
//! * **Statistics** ([`refresh_stats`]): rows and bytes per tenant, from
//!   one grouped scan per tenant-column table on every set (estimates for
//!   the load score; the hourly job and every plan refresh them).
//! * **Plan** ([`plan_rebalance`]): projects (a project's environments stay
//!   together, §3.1) move from the most to the least loaded set, smallest
//!   first, while that narrows the spread of load scores (bytes ÷ weight),
//!   until it is within 10 % or `max_moves` is reached. Each move carries
//!   its size and an estimated freeze.
//! * **Drain** ([`plan_drain`]): every tenant of a draining set to the least
//!   loaded sets that take tenants.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::DbError;
use crate::backend::{Backend, Statement, TxMode, Value};
use crate::placement::{self, PlacementClass, TenantKey};
use crate::router::DbRouter;
use crate::topology::{self, CONTROL_SET_ID, DbSet, SetState};

use super::MoveError;
use super::pgsql::{ident, int, qualified};
use super::plan::SYSTEM_SCHEMA;

/// Spread of load scores a plan aims under (10 %).
pub const TARGET_SPREAD: f64 = 0.10;
/// Default most moves of a plan.
pub const DEFAULT_MAX_MOVES: u32 = 20;
/// How long a stored plan may be run (positions shift).
pub const PLAN_TTL: std::time::Duration = std::time::Duration::from_secs(3600);
/// Freeze estimate: a base (hooks, drain, barrier, flip) …
pub const FREEZE_BASE_MS: u64 = 150;
/// … plus this per 1 000 of the tenant's rows (verification digests and the
/// staged sync scan the tenant's rows in the freeze).
pub const FREEZE_MS_PER_1000_ROWS: u64 = 4;

/// One tenant's load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TenantLoad {
    /// Tenant (environment id).
    pub tenant: String,
    /// Project (`None`: a tenant of its own).
    pub project: Option<String>,
    /// Set it is on.
    pub set_id: String,
    /// Estimated rows.
    pub rows: u64,
    /// Estimated bytes.
    pub bytes: u64,
}

/// A set's load in a plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetScore {
    /// Set id.
    pub set_id: String,
    /// Set name.
    pub name: String,
    /// Weight.
    pub weight: u32,
    /// Tenants.
    pub tenants: u64,
    /// Estimated bytes.
    pub bytes: u64,
    /// Bytes ÷ weight.
    pub score: f64,
}

/// One move of a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedMove {
    /// Tenant.
    pub tenant: String,
    /// Its project.
    pub project: Option<String>,
    /// From set.
    pub from: String,
    /// To set.
    pub to: String,
    /// Estimated rows.
    pub rows: u64,
    /// Estimated bytes.
    pub bytes: u64,
    /// Estimated freeze in milliseconds.
    pub est_freeze_ms: u64,
}

/// A rebalance plan, shown to the operator before it runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RebalancePlan {
    /// Plan id (`rb_<hex>`).
    pub id: String,
    /// RFC 3339.
    pub created_at: String,
    /// Moves, in the order they run (one at a time).
    pub moves: Vec<PlannedMove>,
    /// Loads now.
    pub before: Vec<SetScore>,
    /// Loads after the plan.
    pub after: Vec<SetScore>,
    /// Spread now ((max − min) ÷ max of the scores).
    pub spread_before: f64,
    /// Spread after.
    pub spread_after: f64,
    /// Whether the plan drains a set.
    pub drain: Option<String>,
}

/// Options of [`plan_rebalance`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanOptions {
    /// Sets to balance (default: every active set that takes tenants, and
    /// draining sets as sources).
    pub sets: Option<Vec<String>>,
    /// Most moves (default 20).
    pub max_moves: Option<u32>,
}

/// The freeze estimate of a tenant of `rows` rows.
pub fn estimate_freeze_ms(rows: u64) -> u64 {
    FREEZE_BASE_MS + rows / 1000 * FREEZE_MS_PER_1000_ROWS
}

fn score(bytes: u64, weight: u32) -> f64 {
    bytes as f64 / f64::from(weight.max(1))
}

fn spread(scores: &[f64]) -> f64 {
    let max = scores.iter().copied().fold(0.0_f64, f64::max);
    let min = scores.iter().copied().fold(f64::INFINITY, f64::min);
    if max <= 0.0 || !min.is_finite() {
        0.0
    } else {
        (max - min) / max
    }
}

#[derive(Debug, Clone)]
struct Bin {
    set: DbSet,
    bytes: u64,
    tenants: u64,
    target: bool,
}

impl Bin {
    fn score(&self) -> f64 {
        score(self.bytes, self.set.weight)
    }
}

fn scores(bins: &[Bin]) -> Vec<SetScore> {
    bins.iter()
        .map(|b| SetScore {
            set_id: b.set.id.clone(),
            name: b.set.name.clone(),
            weight: b.set.weight,
            tenants: b.tenants,
            bytes: b.bytes,
            score: b.score(),
        })
        .collect()
}

/// Groups tenants into units that move together (a project; a tenant
/// without one alone), with their total size.
fn units<'a>(tenants: &'a [TenantLoad], set: &str) -> Vec<(u64, Vec<&'a TenantLoad>)> {
    let mut by: BTreeMap<String, Vec<&TenantLoad>> = BTreeMap::new();
    for t in tenants.iter().filter(|t| t.set_id == set) {
        let key = t
            .project
            .clone()
            .map_or_else(|| format!("tenant:{}", t.tenant), |p| format!("project:{p}"));
        by.entry(key).or_default().push(t);
    }
    let mut out: Vec<(u64, Vec<&TenantLoad>)> = by
        .into_values()
        .map(|ts| (ts.iter().map(|t| t.bytes).sum(), ts))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1[0].tenant.cmp(&b.1[0].tenant)));
    out
}

fn planned(t: &TenantLoad, to: &str) -> PlannedMove {
    PlannedMove {
        tenant: t.tenant.clone(),
        project: t.project.clone(),
        from: t.set_id.clone(),
        to: to.to_owned(),
        rows: t.rows,
        bytes: t.bytes,
        est_freeze_ms: estimate_freeze_ms(t.rows),
    }
}

/// The greedy plan over `sets` (with their current loads from `tenants`).
/// Pure: no database.
pub fn plan_rebalance(sets: &[DbSet], tenants: &[TenantLoad], options: &PlanOptions) -> RebalancePlan {
    let max_moves = options.max_moves.unwrap_or(DEFAULT_MAX_MOVES) as usize;
    let in_scope = |s: &DbSet| options.sets.as_ref().is_none_or(|ids| ids.contains(&s.id));
    let mut bins: Vec<Bin> = sets
        .iter()
        .filter(|s| in_scope(s))
        .filter(|s| matches!(s.state, SetState::Active | SetState::Draining))
        .map(|s| Bin {
            set: s.clone(),
            bytes: 0,
            tenants: 0,
            target: s.takes_new_tenants(),
        })
        .collect();
    let mut tenants: Vec<TenantLoad> = tenants.to_vec();
    for t in &tenants {
        if let Some(b) = bins.iter_mut().find(|b| b.set.id == t.set_id) {
            b.bytes = b.bytes.saturating_add(t.bytes);
            b.tenants += 1;
        }
    }
    let before = scores(&bins);
    let spread_of = |bins: &[Bin]| spread(&bins.iter().filter(|b| b.target).map(Bin::score).collect::<Vec<_>>());
    let spread_before = spread_of(&bins);
    let mut moves = Vec::new();
    while moves.len() < max_moves {
        let targets: Vec<usize> = (0..bins.len()).filter(|&i| bins[i].target).collect();
        if targets.len() < 2 || spread_of(&bins) <= TARGET_SPREAD {
            break;
        }
        let hi = *targets
            .iter()
            .max_by(|&&a, &&b| bins[a].score().total_cmp(&bins[b].score()))
            .expect("two targets");
        let lo = *targets
            .iter()
            .filter(|&&i| i != hi)
            .min_by(|&&a, &&b| bins[a].score().total_cmp(&bins[b].score()))
            .expect("two targets");
        let old_max = bins[hi].score();
        let pick = units(&tenants, &bins[hi].set.id)
            .into_iter()
            .find(|(bytes, ts)| {
                moves.len() + ts.len() <= max_moves && {
                    let new_hi = score(bins[hi].bytes - bytes, bins[hi].set.weight);
                    let new_lo = score(bins[lo].bytes + bytes, bins[lo].set.weight);
                    new_hi.max(new_lo) < old_max
                }
            })
            .map(|(bytes, ts)| (bytes, ts.into_iter().cloned().collect::<Vec<_>>()));
        let Some((bytes, unit)) = pick else { break };
        let to = bins[lo].set.id.clone();
        for t in &unit {
            moves.push(planned(t, &to));
            if let Some(x) = tenants.iter_mut().find(|x| x.tenant == t.tenant) {
                x.set_id = to.clone();
            }
        }
        let n = unit.len() as u64;
        bins[hi].bytes -= bytes;
        bins[hi].tenants -= n;
        bins[lo].bytes += bytes;
        bins[lo].tenants += n;
    }
    let spread_after = spread_of(&bins);
    RebalancePlan {
        id: format!("rb_{}", uuid::Uuid::new_v4().simple()),
        created_at: forgebase_core::now_rfc3339(),
        moves,
        before,
        after: scores(&bins),
        spread_before,
        spread_after,
        drain: None,
    }
}

/// Moves every tenant of `drain` to the least loaded sets that take
/// tenants (whole projects together, largest first).
pub fn plan_drain(sets: &[DbSet], tenants: &[TenantLoad], drain: &str) -> RebalancePlan {
    let mut bins: Vec<Bin> = sets
        .iter()
        .filter(|s| s.id == drain || s.takes_new_tenants())
        .filter(|s| s.state != SetState::Retired)
        .map(|s| Bin {
            set: s.clone(),
            bytes: 0,
            tenants: 0,
            target: s.id != drain && s.takes_new_tenants(),
        })
        .collect();
    for t in tenants {
        if let Some(b) = bins.iter_mut().find(|b| b.set.id == t.set_id) {
            b.bytes = b.bytes.saturating_add(t.bytes);
            b.tenants += 1;
        }
    }
    let before = scores(&bins);
    let mut moves = Vec::new();
    let mut units = units(tenants, drain);
    units.reverse();
    for (bytes, unit) in units {
        let Some(lo) = (0..bins.len()).filter(|&i| bins[i].target).min_by(|&a, &b| {
            score(bins[a].bytes + bytes, bins[a].set.weight)
                .total_cmp(&score(bins[b].bytes + bytes, bins[b].set.weight))
        }) else {
            break;
        };
        let to = bins[lo].set.id.clone();
        for t in &unit {
            moves.push(planned(t, &to));
        }
        bins[lo].bytes += bytes;
        bins[lo].tenants += unit.len() as u64;
        if let Some(d) = bins.iter_mut().find(|b| b.set.id == drain) {
            d.bytes = d.bytes.saturating_sub(bytes);
            d.tenants = d.tenants.saturating_sub(unit.len() as u64);
        }
    }
    let targets = |bins: &[Bin]| bins.iter().filter(|b| b.target).map(Bin::score).collect::<Vec<_>>();
    RebalancePlan {
        id: format!("rb_{}", uuid::Uuid::new_v4().simple()),
        created_at: forgebase_core::now_rfc3339(),
        spread_before: spread(&targets(&bins)),
        spread_after: spread(&targets(&bins)),
        moves,
        before,
        after: scores(&bins),
        drain: Some(drain.to_owned()),
    }
}

/// Every tenant's load as the router knows it, with its project.
pub async fn tenant_loads(router: &DbRouter) -> Result<Vec<TenantLoad>, MoveError> {
    let control = router.control();
    let rows = control
        .fetch_all(&Statement::raw("SELECT id, project_id FROM _forgebase_environments"))
        .await?;
    let projects: HashMap<String, String> = rows
        .rows
        .iter()
        .filter_map(|r| {
            Some((
                r.first().and_then(Value::as_str)?.to_owned(),
                r.get(1).and_then(Value::as_str)?.to_owned(),
            ))
        })
        .collect();
    Ok(topology::load_placements(&control)
        .await?
        .into_iter()
        .map(|p| TenantLoad {
            project: projects.get(&p.tenant).cloned(),
            tenant: p.tenant,
            set_id: p.set_id,
            rows: p.est_rows,
            bytes: p.est_bytes,
        })
        .collect())
}

/// Writes a placement row (`control`, `active`) for every environment that
/// has none, so control's tenants count in loads and plans (FB-170: before
/// the first shard). Returns how many were written.
pub async fn ensure_control_placements(router: &DbRouter) -> Result<u64, MoveError> {
    let control = router.control();
    let now = forgebase_core::now_rfc3339();
    let mut tx = control.begin(TxMode::Normal).await?;
    let r = tx
        .execute(&Statement::new(
            "INSERT INTO _forgebase_tenant_placements (tenant, set_id, epoch, state, created_at, updated_at) \
             SELECT e.id, ?1, 1, 'active', ?2, ?2 FROM _forgebase_environments e \
             WHERE NOT EXISTS (SELECT 1 FROM _forgebase_tenant_placements p WHERE p.tenant = e.id)",
            vec![CONTROL_SET_ID.into(), now.into()],
        ))
        .await?;
    if r.rows_affected > 0 {
        topology::bump_epoch(tx.as_mut()).await?;
    }
    tx.commit().await?;
    if r.rows_affected > 0 && !router.topology().sets.is_empty() {
        router.reload().await?;
    }
    Ok(r.rows_affected)
}

/// Refreshes `est_rows` / `est_bytes` of every placed tenant from its set:
/// one grouped scan per tenant-column table (system tables and
/// tenant-scoped collections). Returns the tenants updated.
pub async fn refresh_stats(router: &DbRouter) -> Result<usize, MoveError> {
    let control = router.control();
    let placements = topology::load_placements(&control).await?;
    let mut totals: HashMap<String, (u64, u64)> = HashMap::new();
    for set in router.sets() {
        if set.kind() != crate::BackendKind::Postgres {
            continue;
        }
        let mine: Vec<&str> = placements
            .iter()
            .filter(|p| p.set_id == set.id())
            .map(|p| p.tenant.as_str())
            .collect();
        if mine.is_empty() {
            continue;
        }
        let mut tables: Vec<(String, String, String, bool)> = placement::TABLES
            .iter()
            .filter(|t| t.class == PlacementClass::Tenant)
            .filter_map(|t| match t.tenant_key {
                TenantKey::Column { column } => Some((
                    SYSTEM_SCHEMA.to_owned(),
                    t.table.to_owned(),
                    column.to_owned(),
                    t.table == "_forgebase_kv_entries",
                )),
                _ => None,
            })
            .collect();
        let rows = set
            .fetch_all(&Statement::raw(
                "SELECT c.table_schema::text, c.table_name::text FROM information_schema.columns c \
                 JOIN information_schema.tables t ON t.table_schema = c.table_schema AND t.table_name = c.table_name \
                 WHERE c.column_name = '_tenant' AND t.table_type = 'BASE TABLE' \
                   AND c.table_schema NOT IN ('pg_catalog', 'information_schema', 'fb_move')",
            ))
            .await?;
        for r in &rows.rows {
            if let (Some(s), Some(t)) = (r.first().and_then(Value::as_str), r.get(1).and_then(Value::as_str)) {
                tables.push((s.to_owned(), t.to_owned(), "_tenant".to_owned(), false));
            }
        }
        for (schema, table, column, kv) in tables {
            if !super::pgsql::table_exists(&set, &schema, &table).await? {
                continue;
            }
            let key = if kv {
                format!("substr({}, 3)", ident(&column))
            } else {
                ident(&column)
            };
            let rows = set
                .fetch_all(&Statement::raw(format!(
                    "SELECT {key}, count(*), CAST(COALESCE(sum(pg_column_size(t.*)), 0) AS bigint) FROM {} t \
                     WHERE {} IS NOT NULL GROUP BY 1",
                    qualified(&schema, &table),
                    ident(&column)
                )))
                .await?;
            for r in &rows.rows {
                let Some(tenant) = r.first().and_then(Value::as_str) else {
                    continue;
                };
                if !mine.contains(&tenant) {
                    continue;
                }
                let e = totals.entry(tenant.to_owned()).or_default();
                e.0 += u64::try_from(r.get(1).and_then(Value::as_i64).unwrap_or(0)).unwrap_or(0);
                e.1 += u64::try_from(r.get(2).and_then(Value::as_i64).unwrap_or(0)).unwrap_or(0);
            }
        }
    }
    let mut n = 0;
    for p in &placements {
        let (rows, bytes) = totals.get(&p.tenant).copied().unwrap_or((0, 0));
        if topology::record_placement_stats(&control, &p.tenant, rows, bytes).await? {
            n += 1;
        }
    }
    Ok(n)
}

fn plan_key(id: &str) -> String {
    format!("rebalance_plan:{id}")
}

/// Stores a plan on control (`_forgebase_meta`) for [`load_plan`].
pub async fn save_plan(control: &dyn Backend, plan: &RebalancePlan) -> Result<(), MoveError> {
    let json = serde_json::to_string(plan).map_err(|e| DbError::Statement(e.to_string()))?;
    let mut tx = control.begin(TxMode::Normal).await?;
    tx.execute(&Statement::new(
        "INSERT INTO _forgebase_meta (key, value, updated_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        vec![
            plan_key(&plan.id).into(),
            json.into(),
            forgebase_core::now_rfc3339().into(),
        ],
    ))
    .await?;
    // Old plans go.
    tx.execute(&Statement::new(
        "DELETE FROM _forgebase_meta WHERE key LIKE 'rebalance_plan:%' AND key <> ?1 AND updated_at < ?2",
        vec![plan_key(&plan.id).into(), rfc3339_ago(PLAN_TTL * 24).into()],
    ))
    .await?;
    tx.commit().await?;
    Ok(())
}

fn rfc3339_ago(d: std::time::Duration) -> String {
    (time::OffsetDateTime::now_utc() - d)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// When `plan` can no longer be run (RFC 3339).
pub fn expires_at(plan: &RebalancePlan) -> String {
    time::OffsetDateTime::parse(&plan.created_at, &time::format_description::well_known::Rfc3339)
        .ok()
        .and_then(|t| {
            (t + PLAN_TTL)
                .format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_default()
}

/// A stored plan, refused once older than [`PLAN_TTL`].
pub async fn load_plan(control: &dyn Backend, id: &str) -> Result<RebalancePlan, MoveError> {
    let rows = control
        .fetch_all(&Statement::new(
            "SELECT value FROM _forgebase_meta WHERE key = ?1",
            vec![plan_key(id).into()],
        ))
        .await?;
    let Some(json) = rows.scalar().and_then(Value::as_str) else {
        return Err(MoveError::NotFound(id.to_owned()));
    };
    let plan: RebalancePlan =
        serde_json::from_str(json).map_err(|e| MoveError::Invalid(format!("stored plan {id}: {e}")))?;
    if plan.created_at < rfc3339_ago(PLAN_TTL) {
        return Err(MoveError::State(format!(
            "plan {id} is older than {} minutes; make a new one",
            PLAN_TTL.as_secs() / 60
        )));
    }
    Ok(plan)
}

/// Marks shard `set_id` `draining` (no new tenants) and returns the plan
/// that moves its tenants out (ADR-0029 §3.9). Control cannot be drained.
pub async fn drain_set(router: &DbRouter, set_id: &str) -> Result<RebalancePlan, MoveError> {
    let control = router.control();
    let topo = topology::load(&control).await?;
    let set = topo
        .set(set_id)
        .filter(|s| s.state != SetState::Retired)
        .cloned()
        .ok_or_else(|| MoveError::Router(crate::RouterError::UnknownSet(set_id.to_owned())))?;
    if set.is_control() {
        return Err(MoveError::Invalid(
            "control cannot be drained; set hosts_tenants = false to keep new tenants off it".into(),
        ));
    }
    if set.state != SetState::Draining {
        topology::update_set(&control, set_id, Some(SetState::Draining), None, None).await?;
        router.reload().await?;
    }
    let sets = router.topology().sets;
    let plan = plan_drain(&sets, &tenant_loads(router).await?, set_id);
    save_plan(&control, &plan).await?;
    Ok(plan)
}

/// Refuses to remove a set that still holds tenants or takes part in an
/// open move (`409 shard_not_empty` / `operation_in_progress`).
pub async fn check_removable(router: &DbRouter, set_id: &str) -> Result<(), MoveError> {
    let control = router.control();
    let placed = topology::load_placements(&control)
        .await?
        .into_iter()
        .filter(|p| p.set_id == set_id)
        .count();
    if placed > 0 {
        return Err(MoveError::State(format!(
            "{set_id} still holds {placed} tenants: drain it and let the moves finish first"
        )));
    }
    let open = super::list(
        &control,
        &super::MoveQuery {
            open: true,
            limit: Some(1000),
            ..super::MoveQuery::default()
        },
    )
    .await?;
    if let Some(m) = open.iter().find(|m| m.from_set == set_id || m.to_set == set_id) {
        return Err(MoveError::InProgress(format!(
            "move {} of tenant {} still uses {set_id} (state {})",
            m.id, m.tenant, m.state
        )));
    }
    Ok(())
}

/// Rows counted by [`refresh_stats`] for one tenant on one set (tests).
pub async fn count_tenant_rows(db: &dyn Backend, table: &str, column: &str, tenant: &str) -> Result<i64, DbError> {
    Ok(int(&db
        .fetch_all(&Statement::new(
            format!(
                "SELECT count(*) FROM {} WHERE {} = ?1",
                qualified(SYSTEM_SCHEMA, table),
                ident(column)
            ),
            vec![tenant.into()],
        ))
        .await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::topology::SetKind;

    fn set(id: &str, number: u16, weight: u32) -> DbSet {
        DbSet {
            id: id.into(),
            name: id.into(),
            kind: if number == 0 { SetKind::Control } else { SetKind::Shard },
            number,
            state: SetState::Active,
            weight,
            hosts_tenants: true,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn t(tenant: &str, project: &str, set: &str, bytes: u64) -> TenantLoad {
        TenantLoad {
            tenant: tenant.into(),
            project: Some(project.into()),
            set_id: set.into(),
            rows: bytes / 100,
            bytes,
        }
    }

    #[test]
    fn plans_move_small_projects_from_the_fullest_set_until_balanced() {
        let sets = [set("control", 0, 1), set("s1", 1, 1)];
        let tenants = [
            t("a1", "a", "control", 100),
            t("a2", "a", "control", 100),
            t("b", "b", "control", 300),
            t("c", "c", "control", 400),
        ];
        let plan = plan_rebalance(&sets, &tenants, &PlanOptions::default());
        // Project a (200, two tenants) moves together first, then b (300): 400 / 500.
        let moved: Vec<&str> = plan.moves.iter().map(|m| m.tenant.as_str()).collect();
        assert_eq!(moved, ["a1", "a2", "b"]);
        assert!(plan.moves.iter().all(|m| m.to == "s1" && m.from == "control"));
        assert!(plan.spread_before > 0.99);
        assert!(plan.spread_after <= 0.2 + f64::EPSILON, "{}", plan.spread_after);
        assert!(plan.moves[0].est_freeze_ms >= FREEZE_BASE_MS);
        assert_eq!(plan.after.iter().map(|s| s.bytes).sum::<u64>(), 900);
    }

    #[test]
    fn plans_respect_weights_max_moves_and_balanced_sets() {
        let sets = [set("control", 0, 1), set("s1", 1, 3)];
        let tenants: Vec<TenantLoad> = (0..8)
            .map(|i| t(&format!("t{i}"), &format!("p{i}"), "control", 100))
            .collect();
        let plan = plan_rebalance(&sets, &tenants, &PlanOptions::default());
        // Weight 3 takes three times the bytes: 6 of 8 move (200 / 600 → equal scores).
        assert_eq!(plan.moves.len(), 6);
        let capped = plan_rebalance(
            &sets,
            &tenants,
            &PlanOptions {
                max_moves: Some(2),
                ..PlanOptions::default()
            },
        );
        assert_eq!(capped.moves.len(), 2);
        let balanced = [t("x", "x", "control", 100), t("y", "y", "s1", 300)];
        assert!(
            plan_rebalance(&sets, &balanced, &PlanOptions::default())
                .moves
                .is_empty()
        );
        // Out of scope: nothing.
        let none = plan_rebalance(
            &sets,
            &tenants,
            &PlanOptions {
                sets: Some(vec!["control".into()]),
                ..PlanOptions::default()
            },
        );
        assert!(none.moves.is_empty());
    }

    #[test]
    fn drains_move_every_tenant_to_the_least_loaded_sets() {
        let mut d = set("s2", 2, 1);
        d.state = SetState::Draining;
        let sets = [set("control", 0, 1), set("s1", 1, 1), d];
        let tenants = [
            t("a", "a", "s2", 500),
            t("b1", "b", "s2", 100),
            t("b2", "b", "s2", 100),
            t("c", "c", "control", 400),
        ];
        let plan = plan_drain(&sets, &tenants, "s2");
        assert_eq!(plan.drain.as_deref(), Some("s2"));
        assert_eq!(plan.moves.len(), 3);
        assert!(plan.moves.iter().all(|m| m.from == "s2" && m.to != "s2"));
        // The largest unit (a, 500) goes to the empty s1, project b to control.
        assert!(plan.moves.iter().any(|m| m.tenant == "a" && m.to == "s1"));
        assert!(
            plan.moves
                .iter()
                .filter(|m| m.project.as_deref() == Some("b"))
                .all(|m| m.to == "control")
        );
        assert_eq!(plan.after.iter().find(|s| s.set_id == "s2").unwrap().tenants, 0);
    }

    #[test]
    fn spreads() {
        assert_eq!(spread(&[]), 0.0);
        assert_eq!(spread(&[0.0, 0.0]), 0.0);
        assert!((spread(&[100.0, 90.0]) - 0.1).abs() < 1e-9);
        assert_eq!(estimate_freeze_ms(0), FREEZE_BASE_MS);
    }
}
