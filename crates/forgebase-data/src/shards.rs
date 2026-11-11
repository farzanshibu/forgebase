//! Shard routing in the data core (FB-171, ADR-0029 §3.5–§3.6).
//!
//! Without a router, or with a router that has one set, every call runs on
//! the service's backend exactly as before. Once the cluster is sharded:
//!
//! * **Per-call set.** Records of a tenant-scoped collection (and views that
//!   read tenant data) live on the tenant's set
//!   ([`forgebase_db::DbRouter::for_tenant`]); every other collection on
//!   control. A call without a tenant on such a collection is refused with
//!   `400 tenant_required_sharded` (on one set a privileged caller still sees
//!   every tenant's rows).
//! * **Relations across placements** (tenant-scoped → shared collection):
//!   no SQL foreign key ([`crate::ddl::relation_table`] leaves it out once
//!   the topology has a shard); the data core checks that targets exist on
//!   control, `expand` reads them from control in a second query, and
//!   deleting a shared record runs its `ON DELETE` actions on every set as a
//!   fan-out ([`RelationFanout`], [`DataService::apply_relation_deletes`]);
//!   `RESTRICT` is checked on every set before the delete.
//! * **Rules and filters that follow such a relation** are refused
//!   (`relation_crosses_shards`): when a rule is saved
//!   ([`DataService::validate_predicate`]) and when a rule, filter or sort
//!   key is used. [`DataService::cross_shard_offenders`] lists the stored
//!   rules that would be refused, for the check before the first extra
//!   shard (open question 2, option A).

use std::collections::BTreeMap;
use std::sync::Arc;

use forgebase_db::{Backend, BoxFuture, DbRouter, Statement, TenantRef, TxMode, Value};
use forgebase_query::{Expr, FieldPath, Operand, PathSeg};
use forgebase_schema::{Collection, FieldKind, now_timestamp};
use serde::{Deserialize, Serialize};

use crate::access::{is_tenant_scoped, reads_tenant_data};
use crate::catalog::CatalogSnapshot;
use crate::{AccessContext, ChangeOp, DataError, DataService, Predicate, finish};

/// Code of a rule, filter or sort key that follows a relation from
/// tenant-scoped data to a shared collection on a sharded cluster.
pub const RELATION_CROSSES_SHARDS: &str = "relation_crosses_shards";

/// Code of a call without a tenant on tenant-scoped data of a sharded cluster.
pub const TENANT_REQUIRED_SHARDED: &str = "tenant_required_sharded";

/// Shared records deleted on control whose `ON DELETE` actions still have
/// to run on every set (cascade, set null, removal from multi-value
/// relations). The payload of the fan-out job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationDeletes {
    /// The shared collection the records were deleted from.
    pub collection_id: String,
    /// Ids of the deleted records.
    pub ids: Vec<String>,
}

/// Runs [`RelationDeletes`] after the delete committed. The server enqueues
/// a job (`data.relation_deletes`) whose handler calls
/// [`DataService::apply_relation_deletes`]; without a hook the data core runs
/// the actions itself right after the commit.
pub trait RelationFanout: Send + Sync + std::fmt::Debug {
    /// Schedules `deletes`. An error is logged; the actions are idempotent,
    /// so a later run (or a repeated delete) repairs them.
    fn schedule(&self, deletes: RelationDeletes) -> BoxFuture<'static, Result<(), String>>;
}

/// A stored rule that follows a relation across placements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrossShardRule {
    /// Collection the rule belongs to.
    pub collection: String,
    /// `list_rule`, `view_rule`, `create_rule`, `update_rule` or `delete_rule`.
    pub rule: String,
    /// The relation path that crosses (`author.team`).
    pub path: String,
}

/// How a set's rows reference a deleted shared record.
#[derive(Debug, Clone)]
struct Referrer {
    coll: Collection,
    field: String,
    multi: bool,
    cascade: bool,
    required: bool,
}

/// Whether field `field` of `coll` is a relation that crosses placements:
/// from a tenant-scoped collection to a shared (non tenant-scoped) one.
pub(crate) fn relation_crosses(cat: &CatalogSnapshot, coll: &Collection, field: &str) -> Option<Collection> {
    let f = coll.field(field)?;
    let FieldKind::Relation { collection_id, .. } = &f.kind else {
        return None;
    };
    let target = cat.get(collection_id)?;
    (is_tenant_scoped(coll) && !is_tenant_scoped(target) && !target.is_view()).then(|| target.clone())
}

/// The first relation path of `expr` that crosses placements, if any.
pub(crate) fn crossing_path(cat: &CatalogSnapshot, coll: &Collection, expr: &Expr) -> Option<String> {
    match expr {
        Expr::And(v) | Expr::Or(v) => v.iter().find_map(|e| crossing_path(cat, coll, e)),
        Expr::Not(e) => crossing_path(cat, coll, e),
        Expr::Const(_) => None,
        Expr::Compare(c) => [&c.left, &c.right].into_iter().find_map(|o| match o {
            Operand::Path(p) => path_crosses(cat, coll, p),
            _ => None,
        }),
    }
}

/// Whether one field path crosses placements at any hop.
pub(crate) fn path_crosses(cat: &CatalogSnapshot, coll: &Collection, path: &FieldPath) -> Option<String> {
    let mut current = coll.clone();
    let names: Vec<&str> = path
        .segments
        .iter()
        .map_while(|s| match s {
            PathSeg::Name(n) => Some(n.as_str()),
            PathSeg::Index(_) => None,
        })
        .collect();
    // The last segment is the column read; only the hops before it follow
    // relations.
    for (i, name) in names.iter().enumerate().take(names.len().saturating_sub(1)) {
        let Some(f) = current.field(name) else { return None };
        let FieldKind::Relation { collection_id, .. } = &f.kind else {
            return None;
        };
        if relation_crosses(cat, &current, name).is_some() {
            return Some(names[..=i + 1].join("."));
        }
        current = cat.get(collection_id)?.clone();
    }
    None
}

/// Replaces `@request…` references outside string literals with a
/// parameter, so a stored rule parses as a filter (for the pre-check only).
fn rule_as_filter(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b'\'' || c == b'"' {
            let start = i;
            i += 1;
            while i < b.len() && b[i] != c {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            out.push_str(&src[start..i]);
            continue;
        }
        if c == b'@' && src[i..].starts_with("@request") {
            let mut j = i + "@request".len();
            while j < b.len() && (b[j] == b'.' || b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            out.push_str("{:rq}");
            i = j;
            continue;
        }
        let ch = src[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

impl DataService {
    /// Whether the attached router has more than one routable set.
    pub fn is_sharded(&self) -> bool {
        self.shard_router().is_some_and(DbRouter::is_sharded)
    }

    /// The router, when the cluster is sharded.
    fn sharded_router(&self) -> Option<&DbRouter> {
        self.shard_router().filter(|r| r.is_sharded())
    }

    /// Whether rows of `coll` live on their tenant's set.
    pub(crate) fn tenant_placed(coll: &Collection) -> bool {
        is_tenant_scoped(coll) || reads_tenant_data(coll)
    }

    /// The backend that holds `coll`'s rows for `ctx` (ADR-0029 §3.5).
    pub(crate) async fn route(&self, ctx: &AccessContext, coll: &Collection) -> Result<Arc<dyn Backend>, DataError> {
        self.route_tenant(coll, ctx.tenant.as_deref()).await
    }

    /// The backend that holds `coll`'s rows of `tenant`.
    pub(crate) async fn route_tenant(
        &self,
        coll: &Collection,
        tenant: Option<&str>,
    ) -> Result<Arc<dyn Backend>, DataError> {
        let Some(router) = self.sharded_router() else {
            return Ok(self.backend().clone());
        };
        if !Self::tenant_placed(coll) {
            return Ok(self.backend().clone());
        }
        match tenant.filter(|t| !t.is_empty()) {
            Some(t) => {
                let h = router.for_tenant(TenantRef::new(Some(t))).await?;
                Ok(if h.is_control() {
                    self.backend().clone()
                } else {
                    Arc::new(h)
                })
            }
            None => Err(tenant_required_sharded(&coll.name)),
        }
    }

    /// Refuses `pred` (a rule) or a filter when it follows a relation
    /// across placements on a sharded cluster.
    pub(crate) fn check_crossing(&self, coll: &Collection, expr: &Expr, what: &str) -> Result<(), DataError> {
        if !self.is_sharded() {
            return Ok(());
        }
        let cat = self.catalog();
        match crossing_path(&cat, coll, expr) {
            Some(path) => Err(crosses_error(coll, &path, what)),
            None => Ok(()),
        }
    }

    /// Same for sort keys and other bare paths.
    pub(crate) fn check_path_crossing(&self, coll: &Collection, path: &FieldPath, what: &str) -> Result<(), DataError> {
        if !self.is_sharded() {
            return Ok(());
        }
        let cat = self.catalog();
        match path_crosses(&cat, coll, path) {
            Some(p) => Err(crosses_error(coll, &p, what)),
            None => Ok(()),
        }
    }

    /// Same for a predicate (a rule or the caller's access predicate).
    pub(crate) fn check_predicate_crossing(&self, coll: &Collection, p: Option<&Predicate>) -> Result<(), DataError> {
        match p {
            Some(p) => self.check_crossing(coll, &p.filter, "rule"),
            None => Ok(()),
        }
    }

    /// Stored collection rules that follow a relation from tenant-scoped data
    /// to a shared collection: on a sharded cluster they are refused. Run it
    /// before the first extra shard is added and fix (or accept) every
    /// offender first (ADR-0029 §3.6). Works on one set too.
    pub async fn cross_shard_offenders(&self) -> Result<Vec<CrossShardRule>, DataError> {
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                "SELECT collection_id, list_rule, view_rule, create_rule, update_rule, delete_rule \
                 FROM _forgebase_collection_policies ORDER BY collection_id",
                vec![],
            ))
            .await?;
        let cat = self.catalog();
        let mut out = Vec::new();
        for r in &rows.rows {
            let Some(coll) = r.first().and_then(Value::as_str).and_then(|id| cat.get(id)) else {
                continue;
            };
            for (i, name) in ["list_rule", "view_rule", "create_rule", "update_rule", "delete_rule"]
                .into_iter()
                .enumerate()
            {
                let Some(text) = r.get(i + 1).and_then(Value::as_str).filter(|t| !t.trim().is_empty()) else {
                    continue;
                };
                let Ok(Some(expr)) = forgebase_query::parse_filter(&rule_as_filter(text)) else {
                    continue;
                };
                if let Some(path) = crossing_path(&cat, coll, &expr) {
                    out.push(CrossShardRule {
                        collection: coll.name.clone(),
                        rule: name.to_owned(),
                        path,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Drops the foreign keys of relations from tenant-scoped collections to
    /// shared ones on control (PostgreSQL; a no-op on SQLite). Tables
    /// created before the first shard have them; once tenant data can live
    /// on another set the data core enforces those relations instead
    /// (existence checks, `RESTRICT` on every set, the `ON DELETE` fan-out).
    /// Idempotent, and a no-op while the topology has no shard set (the keys
    /// then still enforce the relations). FB-174 runs it with
    /// [`DataService::cross_shard_offenders`] once the first shard set row
    /// exists and before the set is activated; the server runs it at start on
    /// a sharded topology. Returns the number of keys dropped.
    pub async fn drop_cross_placement_keys(&self) -> Result<u64, DataError> {
        let d = self.backend().dialect();
        if !crate::ddl::is_pg(d) {
            return Ok(0);
        }
        let _guard = self.inner.schema_lock.lock().await;
        self.reload_locked().await?;
        let cat = self.catalog();
        if !cat.has_shards() {
            return Ok(0); // the foreign keys still enforce the relations
        }
        let mut tx = self.begin(TxMode::SchemaChange).await?;
        let result = async {
            let mut n = 0u64;
            for c in cat.all() {
                for f in &c.fields {
                    let Some(target) = relation_crosses(&cat, c, &f.name) else {
                        continue;
                    };
                    if !matches!(f.kind, FieldKind::Relation { max_select: 1, .. }) {
                        continue; // multi-value relations never had a key
                    }
                    let keys = tx
                        .fetch_all(&Statement::new(
                            "SELECT k.conname::text FROM pg_catalog.pg_constraint k \
                             JOIN pg_catalog.pg_attribute a ON a.attrelid = k.conrelid AND a.attnum = k.conkey[1] \
                             WHERE k.contype = 'f' AND k.conrelid = pg_catalog.to_regclass(CAST(?1 AS text)) \
                               AND k.confrelid = pg_catalog.to_regclass(CAST(?2 AS text)) \
                               AND cardinality(k.conkey) = 1 AND a.attname::text = ?3",
                            vec![
                                crate::ddl::pg_regclass(d, &c.name).into(),
                                crate::ddl::pg_regclass(d, &target.name).into(),
                                f.name.clone().into(),
                            ],
                        ))
                        .await?;
                    for k in &keys.rows {
                        let Some(name) = k.first().and_then(Value::as_str) else { continue };
                        tx.execute(&Statement::raw(format!(
                            "ALTER TABLE {} DROP CONSTRAINT {}",
                            crate::ddl::rel(d, &c.name),
                            d.quote_ident(name)
                        )))
                        .await?;
                        n += 1;
                    }
                }
            }
            Ok(n)
        }
        .await;
        finish(tx, result).await
    }

    /// Installs the fan-out of `ON DELETE` actions (FB-171). Once; `false` if
    /// one was installed already.
    pub fn set_relation_fanout(&self, hook: Arc<dyn RelationFanout>) -> bool {
        self.inner.relation_fanout.set(hook).is_ok()
    }

    /// Tenant-scoped collections whose relation fields point at `target`
    /// (a shared collection): the referencing side of cross-placement
    /// relations.
    fn cross_referrers(&self, target: &Collection) -> Vec<Referrer> {
        let cat = self.catalog();
        let mut out = Vec::new();
        for other in cat.all() {
            if !is_tenant_scoped(other) {
                continue;
            }
            for f in &other.fields {
                let FieldKind::Relation {
                    collection_id,
                    max_select,
                    cascade_delete,
                } = &f.kind
                else {
                    continue;
                };
                if collection_id == &target.id {
                    out.push(Referrer {
                        coll: other.clone(),
                        field: f.name.clone(),
                        multi: *max_select > 1,
                        cascade: *cascade_delete,
                        required: f.required,
                    });
                }
            }
        }
        out
    }

    /// Whether deleting shared record `id` of `target` must leave relation
    /// actions to the fan-out (a sharded cluster with tenant-scoped
    /// collections pointing at it).
    pub(crate) fn fans_out_deletes(&self, target: &Collection) -> bool {
        self.is_sharded() && !is_tenant_scoped(target) && !self.cross_referrers(target).is_empty()
    }

    /// `RESTRICT` across sets: a required, non-cascading relation from any
    /// set's tenant data to `id` blocks the delete (checked on the primaries
    /// before control's delete commits).
    pub(crate) async fn check_cross_restrict(&self, target: &Collection, id: &str) -> Result<(), DataError> {
        let Some(router) = self.sharded_router() else {
            return Ok(());
        };
        let refs: Vec<Referrer> = self
            .cross_referrers(target)
            .into_iter()
            .filter(|r| r.required && !r.cascade)
            .collect();
        if refs.is_empty() {
            return Ok(());
        }
        for set in router.sets() {
            let backend: Arc<dyn Backend> = if set.is_control() {
                self.backend().clone()
            } else {
                Arc::new(set.clone())
            };
            for r in &refs {
                let d = backend.dialect();
                let (t, c) = (d.quote_ident(&r.coll.name), d.quote_ident(&r.field));
                let sql = if r.multi {
                    let (each, elem) = d.json_array_elements(&c, "e");
                    format!(
                        "SELECT {} FROM {t} WHERE {} = 1 AND EXISTS (SELECT 1 FROM {each} WHERE {elem} = ?1) LIMIT 1",
                        d.quote_ident("id"),
                        d.json_array_length(&c)
                    )
                } else {
                    format!("SELECT {} FROM {t} WHERE {c} = ?1 LIMIT 1", d.quote_ident("id"))
                };
                let rows = backend.fetch_all(&Statement::new(sql, vec![id.into()])).await?;
                if let Some(rid) = rows.rows.first().and_then(|r| r.first()).and_then(Value::as_str) {
                    return Err(DataError::conflict(
                        "record_referenced",
                        format!(
                            "{}.{} of record {rid} (database set {}) requires this record",
                            r.coll.name,
                            r.field,
                            set.id()
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    /// Schedules the fan-out of `deletes` after their delete committed: the
    /// installed [`RelationFanout`], or right here without one.
    pub(crate) async fn schedule_relation_deletes(&self, deletes: Vec<RelationDeletes>) {
        for d in deletes {
            match self.inner.relation_fanout.get() {
                Some(hook) => {
                    if let Err(e) = hook.schedule(d.clone()).await {
                        tracing::warn!(collection = %d.collection_id, error = %e, "relation fan-out not scheduled; running it now");
                        if let Err(e) = self.apply_relation_deletes(&d).await {
                            tracing::warn!(collection = %d.collection_id, error = %e, "relation fan-out failed");
                        }
                    }
                }
                None => {
                    if let Err(e) = self.apply_relation_deletes(&d).await {
                        tracing::warn!(collection = %d.collection_id, error = %e, "relation fan-out failed");
                    }
                }
            }
        }
    }

    /// Runs the `ON DELETE` actions of deleted shared records on every set
    /// (cascade, set null, removal from multi-value relations), each set in
    /// its own transaction, without the referencing collections' rules (as
    /// foreign-key actions). Idempotent: rows already handled are not found
    /// again. Returns the number of records changed or deleted per set id.
    pub async fn apply_relation_deletes(&self, d: &RelationDeletes) -> Result<BTreeMap<String, u64>, DataError> {
        let mut out = BTreeMap::new();
        let Some(target) = self.catalog().get(&d.collection_id).cloned() else {
            return Ok(out);
        };
        let refs = self.cross_referrers(&target);
        if refs.is_empty() || d.ids.is_empty() {
            return Ok(out);
        }
        let sets: Vec<(String, Arc<dyn Backend>)> = match self.shard_router() {
            Some(router) if router.is_sharded() => router
                .sets()
                .into_iter()
                .map(|h| {
                    let b: Arc<dyn Backend> = if h.is_control() {
                        self.backend().clone()
                    } else {
                        Arc::new(h.clone())
                    };
                    (h.id().to_owned(), b)
                })
                .collect(),
            _ => vec![(forgebase_db::topology::CONTROL_SET_ID.to_owned(), self.backend().clone())],
        };
        let ctx = AccessContext::system();
        for (set_id, backend) in sets {
            let mut tx = backend.begin(TxMode::Normal).await?;
            let result = async {
                let mut changed = 0u64;
                for id in &d.ids {
                    for r in &refs {
                        changed += self.apply_referrer(&ctx, &mut *tx, r, id).await?;
                    }
                }
                Ok(changed)
            }
            .await;
            let n = finish(tx, result).await?;
            if n > 0 {
                self.changes_committed();
            }
            out.insert(set_id, n);
        }
        Ok(out)
    }

    /// One referencing field on one set.
    async fn apply_referrer(
        &self,
        ctx: &AccessContext,
        tx: &mut dyn forgebase_db::WriteTx,
        r: &Referrer,
        id: &str,
    ) -> Result<u64, DataError> {
        let d = self.backend().dialect();
        let (t, c) = (d.quote_ident(&r.coll.name), d.quote_ident(&r.field));
        let (each, elem) = d.json_array_elements(&c, "e");
        let find = if r.multi {
            format!(
                "SELECT {}, {} FROM {t} WHERE EXISTS (SELECT 1 FROM {each} WHERE {elem} = ?1)",
                d.quote_ident("id"),
                d.quote_ident("_version")
            )
        } else {
            format!(
                "SELECT {}, {} FROM {t} WHERE {c} = ?1",
                d.quote_ident("id"),
                d.quote_ident("_version")
            )
        };
        let rows = tx.fetch_all(&Statement::new(find, vec![id.into()])).await?;
        let mut n = 0;
        for row in &rows.rows {
            let rid = row.first().and_then(Value::as_str).unwrap_or_default().to_owned();
            let version = row.get(1).and_then(Value::as_i64).unwrap_or(1);
            if r.cascade {
                let mut pending = Vec::new();
                self.delete_in_tx(ctx, &mut *tx, &r.coll, &rid, version, 1, &mut pending)
                    .await?;
                n += 1;
                continue;
            }
            let set = if r.multi {
                format!(
                    "{c} = (SELECT {} FROM {each} WHERE {elem} {} ?1)",
                    d.json_array_agg(&elem),
                    d.null_safe_ne()
                )
            } else {
                format!("{c} = NULL")
            };
            tx.execute(&Statement::new(
                format!(
                    "UPDATE {t} SET {set}, {u} = ?2, {v} = {v} + 1 WHERE {i} = ?3",
                    u = d.quote_ident("updated"),
                    v = d.quote_ident("_version"),
                    i = d.quote_ident("id")
                ),
                vec![id.into(), now_timestamp().into(), rid.clone().into()],
            ))
            .await?;
            n += 1;
            if self.has_change_hook() {
                let after = tx.fetch_all(&self.select_by_id(ctx, &r.coll, &rid, None)?).await?;
                if let Some(row) = after.rows.first() {
                    self.record_change(tx, ctx, &r.coll, ChangeOp::Update, &rid, (&after.columns, row))
                        .await?;
                }
            }
        }
        Ok(n)
    }
}

/// `400 tenant_required_sharded`.
pub(crate) fn tenant_required_sharded(collection: &str) -> DataError {
    DataError::invalid(
        TENANT_REQUIRED_SHARDED,
        format!(
            "{collection} is tenant-scoped and this cluster has several database sets: select a tenant (X-ForgeBase-Tenant header)"
        ),
    )
}

fn crosses_error(coll: &Collection, path: &str, what: &str) -> DataError {
    DataError::invalid(
        RELATION_CROSSES_SHARDS,
        format!(
            "the {what} follows {}.{path} from tenant data to a collection shared by all tenants; on a sharded cluster such relations cannot be followed in rules, filters or sort keys (use expand)",
            coll.name
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_references_become_parameters() {
        assert_eq!(
            rule_as_filter("author = @request.auth.id && title != '@request.x'"),
            "author = {:rq} && title != '@request.x'"
        );
        assert_eq!(rule_as_filter("@request.tenant = 'a'"), "{:rq} = 'a'");
    }
}
