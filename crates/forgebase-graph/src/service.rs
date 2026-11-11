//! The graph service: nodes, edges, neighbours, traversal and shortest path
//! over the `_forgebase_graph_*` tables (migration 0006).
//!
//! Rules every method follows:
//!
//! * the [`Authorizer`](forgebase_data::Authorizer) is asked first
//!   ([`Action::GraphRead`] / [`Action::GraphWrite`]), fail closed, per
//!   label and edge type (FB-139, [`GraphAccess`]): reads compile the
//!   caller's rules into the statement (`list` for listings and walks,
//!   `view` for single reads); writes ask the label or type of the row
//!   (`create`, `update`, `delete`) and check a restricting rule on the
//!   stored row and on the written one before commit;
//! * every statement filters on the caller's tenant (`AccessContext::tenant`;
//!   the shared `''` graph only for privileged callers without one, anyone
//!   else gets `tenant_required`) and every value is a bound parameter; the
//!   SQL text is assembled only from fixed fragments and compiled rules;
//! * depth, result size, filter size and search effort are capped.

use std::collections::{HashMap, HashSet};

use forgebase_data::query::SqlBuilder;
use forgebase_data::{AccessContext, Action, DataError, DataService, Principal, ResourceKind, ResourceOp, ResourceRef};
use forgebase_db::{Backend, Rows, Statement, TxMode, Value, WriteTx};
use serde_json::{Map, Value as Json};

use crate::access::{GraphAccess, One, Rules, and};
use crate::error::GraphError;
use crate::model::*;

type Result<T> = std::result::Result<T, GraphError>;

const NODES: &str = "_forgebase_graph_nodes";
const EDGES: &str = "_forgebase_graph_edges";

/// Node columns, in the order [`read_node`] expects.
const N_COLS: &str = "n.id, n.label, n.properties, n.created_at, n.updated_at";
/// Edge columns, in the order [`read_edge`] expects.
const E_COLS: &str = "e.id, e.type, e.source, e.target, e.directed, e.properties, e.created_at, e.updated_at";
/// Hard cap on rows the recursive walk may produce, whatever the depth.
const MAX_WALK_ROWS: u32 = 200_000;
/// Most distinct labels (and, separately, edge types) a tenant may have for
/// callers whose access is decided per label (FB-139).
pub const MAX_RULE_NAMES: usize = 1000;

/// The graph service. Cheap to clone.
#[derive(Debug, Clone)]
pub struct GraphService {
    data: DataService,
}

impl GraphService {
    /// A graph service on the data core's database and authorizer.
    pub fn new(data: DataService) -> Self {
        Self { data }
    }

    fn backend(&self) -> &dyn Backend {
        self.data.backend().as_ref()
    }

    fn dialect(&self) -> &dyn forgebase_db::Dialect {
        self.backend().dialect()
    }

    /// The caller's tenant: its own, or the shared `''` graph for privileged
    /// callers without one. Anyone else without a tenant gets
    /// `tenant_required`, after the plain check so a denied caller learns
    /// nothing (FB-139).
    fn tenant_of(&self, ctx: &AccessContext, action: Action) -> Result<String> {
        match &ctx.tenant {
            Some(t) => Ok(t.clone()),
            None if ctx.is_privileged() => Ok(String::new()),
            None => {
                self.data.authorize(ctx, action, None)?;
                Err(GraphError::invalid(
                    "tenant_required",
                    "select a tenant (X-ForgeBase-Tenant header, or a tenant-bound key)",
                ))
            }
        }
    }

    fn denied(ctx: &AccessContext, msg: String) -> GraphError {
        if ctx.principal == Principal::Anonymous {
            GraphError::Unauthenticated
        } else {
            GraphError::Forbidden(msg)
        }
    }

    /// The decision for one label (or edge type).
    fn one(&self, ctx: &AccessContext, action: Action, kind: ResourceKind, name: &str, op: ResourceOp) -> Result<One> {
        let r = ResourceRef::new(kind, name, op);
        match self.data.authorize_resource(ctx, action, &r) {
            Ok(None) => Ok(One::Allow),
            Ok(Some(p)) => Ok(One::Filter(p)),
            Err(DataError::Forbidden(_) | DataError::Unauthenticated) => Ok(One::Deny),
            Err(e) => Err(e.into()),
        }
    }

    /// Distinct labels (`column = "label"`) or edge types of a tenant, by
    /// skipping through the index (one probe per name).
    async fn names(&self, tenant: &str, table: &str, column: &str) -> Result<Vec<String>> {
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "WITH RECURSIVE l(v) AS (SELECT (SELECT min({column}) FROM {table} WHERE tenant = ?1) \
                     UNION ALL SELECT (SELECT min({column}) FROM {table} WHERE tenant = ?1 AND {column} > l.v) \
                     FROM l WHERE l.v IS NOT NULL) SELECT v FROM l WHERE v IS NOT NULL LIMIT ?2"
                ),
                vec![
                    tenant.into(),
                    i64::try_from(MAX_RULE_NAMES + 1).unwrap_or(i64::MAX).into(),
                ],
            ))
            .await?;
        if rows.len() > MAX_RULE_NAMES {
            return Err(GraphError::invalid(
                "graph_too_many_names",
                format!("the graph has more than {MAX_RULE_NAMES} labels or edge types; rules cannot be applied"),
            ));
        }
        rows.rows.iter().map(|r| text(&r[0])).collect()
    }

    async fn rules(
        &self,
        ctx: &AccessContext,
        tenant: &str,
        action: Action,
        kind: ResourceKind,
        op: ResourceOp,
    ) -> Result<Rules> {
        let (table, column) = match kind {
            ResourceKind::GraphEdgeType => (EDGES, "type"),
            _ => (NODES, "label"),
        };
        let names = self.names(tenant, table, column).await?;
        if names.is_empty() {
            return Ok(Rules::empty(kind, &self.one(ctx, action, kind, "*", op)?));
        }
        let decisions = names
            .into_iter()
            .map(|n| self.one(ctx, action, kind, &n, op).map(|d| (n, d)))
            .collect::<Result<Vec<_>>>()?;
        Ok(Rules::from_decisions(kind, decisions))
    }

    /// What the caller may see (or change) of its tenant's graph for `op`
    /// (FB-139): the decision of every label and edge type, ready to compile
    /// into SQL. Public so engines that walk the graph with their own SQL
    /// (FB-147, FB-163) apply the same rules and can tell whether any rule
    /// applies ([`GraphAccess::is_unrestricted`]).
    pub async fn access(&self, ctx: &AccessContext, action: Action, op: ResourceOp) -> Result<GraphAccess> {
        let tenant = self.tenant_of(ctx, action)?;
        self.access_in(ctx, &tenant, action, op).await
    }

    async fn access_in(
        &self,
        ctx: &AccessContext,
        tenant: &str,
        action: Action,
        op: ResourceOp,
    ) -> Result<GraphAccess> {
        if ctx.is_privileged() {
            return Ok(GraphAccess::unrestricted());
        }
        let acc = GraphAccess {
            nodes: self.rules(ctx, tenant, action, ResourceKind::GraphLabel, op).await?,
            edges: self.rules(ctx, tenant, action, ResourceKind::GraphEdgeType, op).await?,
        };
        if !acc.nodes.allows_any() && !acc.edges.allows_any() {
            // Nothing at all: the caller has no business here.
            return Err(Self::denied(ctx, format!("{action:?} is not allowed")));
        }
        Ok(acc)
    }

    /// The edge-type rules alone (no "nothing at all" check).
    async fn edge_rules(&self, ctx: &AccessContext, tenant: &str, action: Action, op: ResourceOp) -> Result<Rules> {
        if ctx.is_privileged() {
            return Ok(Rules::unrestricted(ResourceKind::GraphEdgeType));
        }
        self.rules(ctx, tenant, action, ResourceKind::GraphEdgeType, op).await
    }

    async fn read(&self, ctx: &AccessContext, op: ResourceOp) -> Result<(String, GraphAccess)> {
        let tenant = self.tenant_of(ctx, Action::GraphRead)?;
        let acc = self.access_in(ctx, &tenant, Action::GraphRead, op).await?;
        Ok((tenant, acc))
    }

    /// The rules of one name, as [`Rules`].
    fn rules_of(
        &self,
        ctx: &AccessContext,
        action: Action,
        kind: ResourceKind,
        name: &str,
        op: ResourceOp,
    ) -> Result<Rules> {
        if ctx.is_privileged() {
            return Ok(Rules::unrestricted(kind));
        }
        Ok(Rules::from_decisions(
            kind,
            vec![(name.to_owned(), self.one(ctx, action, kind, name, op)?)],
        ))
    }

    /// Whether row `id` of `table` (aliased `alias`) matches `rules`, read
    /// in the write transaction (stored and written rows).
    async fn row_matches(
        &self,
        tx: &mut dyn WriteTx,
        table: &str,
        alias: &str,
        tenant: &str,
        id: &str,
        rules: &Rules,
    ) -> Result<bool> {
        let mut b = SqlBuilder::new(self.dialect());
        let t = b.bind(tenant);
        let i = b.bind(id);
        let Some(cond) = rules.compile(&mut b, alias)? else {
            return Ok(true);
        };
        let rows = tx
            .fetch_all(&Statement::new(
                format!("SELECT 1 FROM {table} {alias} WHERE {alias}.tenant = {t} AND {alias}.id = {i} AND {cond}"),
                b.into_params(),
            ))
            .await?;
        Ok(!rows.is_empty())
    }

    /// The error for a stored row the caller may not change: not found when
    /// the caller may not even view it (no existence oracle), else forbidden.
    #[allow(clippy::too_many_arguments)]
    async fn refuse(
        &self,
        ctx: &AccessContext,
        tx: &mut dyn WriteTx,
        kind: ResourceKind,
        name: &str,
        tenant: &str,
        id: &str,
        what: &str,
    ) -> Result<GraphError> {
        let (table, alias, missing) = match kind {
            ResourceKind::GraphEdgeType => (EDGES, "e", GraphError::EdgeNotFound(id.to_owned())),
            _ => (NODES, "n", GraphError::NodeNotFound(id.to_owned())),
        };
        let view = self.rules_of(ctx, Action::GraphRead, kind, name, ResourceOp::View)?;
        if self.row_matches(tx, table, alias, tenant, id, &view).await? && view.allows_any() {
            Ok(Self::denied(ctx, format!("{what} is not allowed")))
        } else {
            Ok(missing)
        }
    }

    // ---- nodes -----------------------------------------------------------

    /// Creates a node.
    /// The label's `create` rule must allow it (a restricting rule must
    /// hold for the new node).
    pub async fn create_node(&self, ctx: &AccessContext, new: NewNode) -> Result<Node> {
        let tenant = self.tenant_of(ctx, Action::GraphWrite)?;
        let label = check_label(new.label.as_deref())?;
        let rule = self.rules_of(
            ctx,
            Action::GraphWrite,
            ResourceKind::GraphLabel,
            &label,
            ResourceOp::Create,
        )?;
        if !rule.allows_any() {
            return Err(Self::denied(ctx, format!("creating {label:?} nodes is not allowed")));
        }
        let id = match new.id {
            Some(id) => check_id(&id, "node")?,
            None => generate_id(),
        };
        let props = check_props(new.properties)?;
        let now = forgebase_core::now_rfc3339();
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let result = async {
            let exists = tx
                .fetch_all(&Statement::new(
                    format!("SELECT 1 FROM {NODES} WHERE tenant = ?1 AND id = ?2"),
                    vec![tenant.clone().into(), id.clone().into()],
                ))
                .await?;
            if !exists.is_empty() {
                return Err(GraphError::NodeExists(id.clone()));
            }
            tx.execute(&Statement::new(
                format!(
                    "INSERT INTO {NODES} (tenant, id, label, properties, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)"
                ),
                vec![
                    tenant.clone().into(),
                    id.clone().into(),
                    label.clone().into(),
                    props.to_string().into(),
                    now.clone().into(),
                ],
            ))
            .await?;
            if !self.row_matches(&mut *tx, NODES, "n", &tenant, &id, &rule).await? {
                return Err(Self::denied(
                    ctx,
                    format!("the node does not satisfy the create rule of {label:?}"),
                ));
            }
            Ok(())
        }
        .await;
        finish(tx, result).await?;
        Ok(Node {
            id,
            label,
            properties: props,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// One node.
    pub async fn get_node(&self, ctx: &AccessContext, id: &str) -> Result<Node> {
        let (tenant, acc) = self.read(ctx, ResourceOp::View).await?;
        self.node(&tenant, id, &acc)
            .await?
            .ok_or_else(|| GraphError::NodeNotFound(id.to_owned()))
    }

    /// Node `id` when the caller may see it.
    async fn node(&self, tenant: &str, id: &str, acc: &GraphAccess) -> Result<Option<Node>> {
        let mut b = SqlBuilder::new(self.dialect());
        let t = b.bind(tenant);
        let i = b.bind(id);
        let vis = acc.node_sql(&mut b, "n")?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {N_COLS} FROM {NODES} n WHERE n.tenant = {t} AND n.id = {i}{}",
                    and(vis.as_deref())
                ),
                b.into_params(),
            ))
            .await?;
        rows.rows.first().map(|r| read_node(r, 0)).transpose()
    }

    /// The type of edge `id` in the write transaction.
    async fn edge_type_tx(tx: &mut dyn WriteTx, tenant: &str, id: &str) -> Result<Option<String>> {
        let rows = tx
            .fetch_all(&Statement::new(
                format!("SELECT type FROM {EDGES} WHERE tenant = ?1 AND id = ?2"),
                vec![tenant.into(), id.into()],
            ))
            .await?;
        rows.rows.first().map(|r| text(&r[0])).transpose()
    }

    /// The label of node `id` in the write transaction.
    async fn label_tx(tx: &mut dyn WriteTx, tenant: &str, id: &str) -> Result<Option<String>> {
        let rows = tx
            .fetch_all(&Statement::new(
                format!("SELECT label FROM {NODES} WHERE tenant = ?1 AND id = ?2"),
                vec![tenant.into(), id.into()],
            ))
            .await?;
        rows.rows.first().map(|r| text(&r[0])).transpose()
    }

    /// Replaces (`merge = false`) or merge-patches (`merge = true`) a node.
    /// The stored node must pass its label's `update` rule, and the changed
    /// node the `update` rule of its (possibly new) label.
    pub async fn change_node(&self, ctx: &AccessContext, id: &str, change: NodeChange, merge: bool) -> Result<Node> {
        let tenant = self.tenant_of(ctx, Action::GraphWrite)?;
        let label = change.label.as_deref().map(|l| check_label(Some(l))).transpose()?;
        let props = change.properties.map(|p| check_props(Some(p))).transpose()?;
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let result = async {
            let Some(old) = Self::label_tx(&mut *tx, &tenant, id).await? else {
                return Err(GraphError::NodeNotFound(id.to_owned()));
            };
            let k = ResourceKind::GraphLabel;
            let before = self.rules_of(ctx, Action::GraphWrite, k, &old, ResourceOp::Update)?;
            if !self.row_matches(&mut *tx, NODES, "n", &tenant, id, &before).await? {
                return Err(self
                    .refuse(ctx, &mut *tx, k, &old, &tenant, id, "changing this node")
                    .await?);
            }
            let new_label = label.clone().unwrap_or_else(|| old.clone());
            let after = if new_label == old {
                before
            } else {
                self.rules_of(ctx, Action::GraphWrite, k, &new_label, ResourceOp::Update)?
            };
            let props_sql = match (&props, merge) {
                (None, _) => "properties",
                (Some(_), true) => "json_patch(properties, ?4)",
                (Some(_), false) => "?4",
            };
            let done = tx
                .execute(&Statement::new(
                    format!(
                        "UPDATE {NODES} SET label = coalesce(?3, label), properties = {props_sql}, \
                         updated_at = ?5 WHERE tenant = ?1 AND id = ?2"
                    ),
                    vec![
                        tenant.clone().into(),
                        id.into(),
                        label.clone().into(),
                        props.as_ref().map_or(Value::Null, |p| p.to_string().into()),
                        forgebase_core::now_rfc3339().into(),
                    ],
                ))
                .await?;
            if done.rows_affected == 0 {
                return Err(GraphError::NodeNotFound(id.to_owned()));
            }
            let rows = tx
                .fetch_all(&Statement::new(
                    format!("SELECT {N_COLS} FROM {NODES} n WHERE n.tenant = ?1 AND n.id = ?2"),
                    vec![tenant.clone().into(), id.into()],
                ))
                .await?;
            let node = read_node(
                rows.rows
                    .first()
                    .ok_or_else(|| GraphError::NodeNotFound(id.to_owned()))?,
                0,
            )?;
            check_props(Some(node.properties.clone()))?;
            if !self.row_matches(&mut *tx, NODES, "n", &tenant, id, &after).await? {
                return Err(Self::denied(
                    ctx,
                    format!("the changed node does not satisfy the update rule of {new_label:?}"),
                ));
            }
            Ok(node)
        }
        .await;
        finish(tx, result).await
    }

    /// Deletes a node and every edge that touches it. Returns the number of
    /// edges removed.
    /// Needs the label's `delete` rule, and the `delete` rule of every edge
    /// that touches the node.
    pub async fn delete_node(&self, ctx: &AccessContext, id: &str) -> Result<u64> {
        let tenant = self.tenant_of(ctx, Action::GraphWrite)?;
        let edge_rules = self
            .edge_rules(ctx, &tenant, Action::GraphWrite, ResourceOp::Delete)
            .await?;
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let result = async {
            let Some(label) = Self::label_tx(&mut *tx, &tenant, id).await? else {
                return Err(GraphError::NodeNotFound(id.to_owned()));
            };
            let k = ResourceKind::GraphLabel;
            let rule = self.rules_of(ctx, Action::GraphWrite, k, &label, ResourceOp::Delete)?;
            if !self.row_matches(&mut *tx, NODES, "n", &tenant, id, &rule).await? {
                return Err(self
                    .refuse(ctx, &mut *tx, k, &label, &tenant, id, "deleting this node")
                    .await?);
            }
            let mut b = SqlBuilder::new(self.dialect());
            let t = b.bind(tenant.clone());
            let i = b.bind(id);
            if let Some(cond) = edge_rules.compile(&mut b, "e")? {
                let blocked = tx
                    .fetch_all(&Statement::new(
                        format!(
                            "SELECT 1 FROM {EDGES} e WHERE e.tenant = {t} AND (e.source = {i} OR e.target = {i}) \
                             AND NOT coalesce(({cond}), 1 = 0) LIMIT 1"
                        ),
                        b.into_params(),
                    ))
                    .await?;
                if !blocked.is_empty() {
                    return Err(Self::denied(ctx, "the node has edges you may not delete".into()));
                }
            }
            let edges = tx
                .execute(&Statement::new(
                    format!("DELETE FROM {EDGES} WHERE tenant = ?1 AND (source = ?2 OR target = ?2)"),
                    vec![tenant.clone().into(), id.into()],
                ))
                .await?
                .rows_affected;
            let node = tx
                .execute(&Statement::new(
                    format!("DELETE FROM {NODES} WHERE tenant = ?1 AND id = ?2"),
                    vec![tenant.clone().into(), id.into()],
                ))
                .await?;
            if node.rows_affected == 0 {
                return Err(GraphError::NodeNotFound(id.to_owned()));
            }
            Ok(edges)
        }
        .await;
        finish(tx, result).await
    }

    /// Lists nodes ordered by id, optionally of one label.
    pub async fn list_nodes(
        &self,
        ctx: &AccessContext,
        label: Option<&str>,
        after: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Page<Node>> {
        let (tenant, acc) = self.read(ctx, ResourceOp::List).await?;
        let limit = check_limit(limit)?;
        let mut b = SqlBuilder::new(self.dialect());
        bind_all(
            &mut b,
            vec![tenant.into(), label.into(), after.into(), i64::from(limit + 1).into()],
        );
        let vis = and(acc.node_sql(&mut b, "n")?.as_deref());
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {N_COLS} FROM {NODES} n WHERE n.tenant = ?1 AND (?2 IS NULL OR n.label = ?2) \
                     AND (?3 IS NULL OR n.id > ?3){vis} ORDER BY n.id LIMIT ?4"
                ),
                b.into_params(),
            ))
            .await?;
        page(rows, limit, |r| read_node(r, 0), |n| n.id.clone())
    }

    // ---- edges -----------------------------------------------------------

    /// Creates an edge between two existing nodes of the caller's tenant.
    /// The type's `create` rule must allow it, and the caller must be able
    /// to view both nodes.
    pub async fn create_edge(&self, ctx: &AccessContext, new: NewEdge) -> Result<Edge> {
        let tenant = self.tenant_of(ctx, Action::GraphWrite)?;
        let edge_type = check_type(&new.edge_type)?;
        let k = ResourceKind::GraphEdgeType;
        let rule = self.rules_of(ctx, Action::GraphWrite, k, &edge_type, ResourceOp::Create)?;
        if !rule.allows_any() {
            return Err(Self::denied(
                ctx,
                format!("creating {edge_type:?} edges is not allowed"),
            ));
        }
        let view = if ctx.is_privileged() {
            GraphAccess::unrestricted()
        } else {
            GraphAccess {
                nodes: self
                    .rules(
                        ctx,
                        &tenant,
                        Action::GraphRead,
                        ResourceKind::GraphLabel,
                        ResourceOp::View,
                    )
                    .await?,
                edges: Rules::unrestricted(k),
            }
        };
        let id = match new.id {
            Some(id) => check_id(&id, "edge")?,
            None => generate_id(),
        };
        let source = check_id(&new.source, "source node")?;
        let target = check_id(&new.target, "target node")?;
        let directed = new.directed.unwrap_or(true);
        let props = check_props(new.properties)?;
        let now = forgebase_core::now_rfc3339();
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let result = async {
            let mut b = SqlBuilder::new(self.dialect());
            bind_all(&mut b, vec![tenant.clone().into(), source.clone().into(), target.clone().into()]);
            let vis = and(view.node_sql(&mut b, "n")?.as_deref());
            let nodes = tx
                .fetch_all(&Statement::new(
                    format!("SELECT n.id FROM {NODES} n WHERE n.tenant = ?1 AND n.id IN (?2, ?3){vis}"),
                    b.into_params(),
                ))
                .await?;
            let found: HashSet<&str> = nodes.rows.iter().filter_map(|r| r[0].as_str()).collect();
            for (role, n) in [("source", &source), ("target", &target)] {
                if !found.contains(n.as_str()) {
                    return Err(GraphError::invalid(
                        "edge_endpoint_not_found",
                        format!("{role} node {n:?} does not exist"),
                    ));
                }
            }
            let exists = tx
                .fetch_all(&Statement::new(
                    format!("SELECT 1 FROM {EDGES} WHERE tenant = ?1 AND id = ?2"),
                    vec![tenant.clone().into(), id.clone().into()],
                ))
                .await?;
            if !exists.is_empty() {
                return Err(GraphError::EdgeExists(id.clone()));
            }
            tx.execute(&Statement::new(
                format!(
                    "INSERT INTO {EDGES} (tenant, id, type, source, target, directed, properties, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)"
                ),
                vec![
                    tenant.clone().into(),
                    id.clone().into(),
                    edge_type.clone().into(),
                    source.clone().into(),
                    target.clone().into(),
                    directed.into(),
                    props.to_string().into(),
                    now.clone().into(),
                ],
            ))
            .await?;
            if !self.row_matches(&mut *tx, EDGES, "e", &tenant, &id, &rule).await? {
                return Err(Self::denied(
                    ctx,
                    format!("the edge does not satisfy the create rule of {edge_type:?}"),
                ));
            }
            Ok(())
        }
        .await;
        finish(tx, result).await?;
        Ok(Edge {
            id,
            edge_type,
            source,
            target,
            directed,
            properties: props,
            created_at: now.clone(),
            updated_at: now,
        })
    }

    /// One edge.
    pub async fn get_edge(&self, ctx: &AccessContext, id: &str) -> Result<Edge> {
        let (tenant, acc) = self.read(ctx, ResourceOp::View).await?;
        let mut b = SqlBuilder::new(self.dialect());
        b.bind(tenant);
        b.bind(id);
        let vis = and(acc.edge_sql(&mut b, "e", NODES)?.as_deref());
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT {E_COLS} FROM {EDGES} e WHERE e.tenant = ?1 AND e.id = ?2{vis}"),
                b.into_params(),
            ))
            .await?;
        rows.rows
            .first()
            .map(|r| read_edge(r, 0))
            .transpose()?
            .ok_or_else(|| GraphError::EdgeNotFound(id.to_owned()))
    }

    /// Merge-patches an edge's properties.
    /// The stored and the changed edge must pass the type's `update` rule.
    pub async fn change_edge(&self, ctx: &AccessContext, id: &str, change: EdgeChange) -> Result<Edge> {
        let tenant = self.tenant_of(ctx, Action::GraphWrite)?;
        let props = change.properties.map(|p| check_props(Some(p))).transpose()?;
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let result = async {
            let Some(edge_type) = Self::edge_type_tx(&mut *tx, &tenant, id).await? else {
                return Err(GraphError::EdgeNotFound(id.to_owned()));
            };
            let k = ResourceKind::GraphEdgeType;
            let rule = self.rules_of(ctx, Action::GraphWrite, k, &edge_type, ResourceOp::Update)?;
            if !self.row_matches(&mut *tx, EDGES, "e", &tenant, id, &rule).await? {
                return Err(self
                    .refuse(ctx, &mut *tx, k, &edge_type, &tenant, id, "changing this edge")
                    .await?);
            }
            let done = tx
                .execute(&Statement::new(
                    format!(
                        "UPDATE {EDGES} SET properties = coalesce(json_patch(properties, ?3), properties), \
                         updated_at = ?4 WHERE tenant = ?1 AND id = ?2"
                    ),
                    vec![
                        tenant.clone().into(),
                        id.into(),
                        props.as_ref().map_or(Value::Null, |p| p.to_string().into()),
                        forgebase_core::now_rfc3339().into(),
                    ],
                ))
                .await?;
            if done.rows_affected == 0 {
                return Err(GraphError::EdgeNotFound(id.to_owned()));
            }
            let rows = tx
                .fetch_all(&Statement::new(
                    format!("SELECT {E_COLS} FROM {EDGES} e WHERE e.tenant = ?1 AND e.id = ?2"),
                    vec![tenant.clone().into(), id.into()],
                ))
                .await?;
            let edge = read_edge(
                rows.rows
                    .first()
                    .ok_or_else(|| GraphError::EdgeNotFound(id.to_owned()))?,
                0,
            )?;
            check_props(Some(edge.properties.clone()))?;
            if !self.row_matches(&mut *tx, EDGES, "e", &tenant, id, &rule).await? {
                return Err(Self::denied(
                    ctx,
                    format!("the changed edge does not satisfy the update rule of {edge_type:?}"),
                ));
            }
            Ok(edge)
        }
        .await;
        finish(tx, result).await
    }

    /// Deletes an edge.
    /// Needs the type's `delete` rule.
    pub async fn delete_edge(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        let tenant = self.tenant_of(ctx, Action::GraphWrite)?;
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let result = async {
            let Some(edge_type) = Self::edge_type_tx(&mut *tx, &tenant, id).await? else {
                return Err(GraphError::EdgeNotFound(id.to_owned()));
            };
            let k = ResourceKind::GraphEdgeType;
            let rule = self.rules_of(ctx, Action::GraphWrite, k, &edge_type, ResourceOp::Delete)?;
            if !self.row_matches(&mut *tx, EDGES, "e", &tenant, id, &rule).await? {
                return Err(self
                    .refuse(ctx, &mut *tx, k, &edge_type, &tenant, id, "deleting this edge")
                    .await?);
            }
            let done = tx
                .execute(&Statement::new(
                    format!("DELETE FROM {EDGES} WHERE tenant = ?1 AND id = ?2"),
                    vec![tenant.clone().into(), id.into()],
                ))
                .await?;
            if done.rows_affected == 0 {
                return Err(GraphError::EdgeNotFound(id.to_owned()));
            }
            Ok(())
        }
        .await;
        finish(tx, result).await
    }

    /// Lists edges ordered by id; every filter is optional.
    pub async fn list_edges(
        &self,
        ctx: &AccessContext,
        edge_type: Option<&str>,
        source: Option<&str>,
        target: Option<&str>,
        after: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Page<Edge>> {
        let (tenant, acc) = self.read(ctx, ResourceOp::List).await?;
        let limit = check_limit(limit)?;
        let mut b = SqlBuilder::new(self.dialect());
        bind_all(
            &mut b,
            vec![
                tenant.into(),
                edge_type.into(),
                source.into(),
                target.into(),
                after.into(),
                i64::from(limit + 1).into(),
            ],
        );
        let vis = and(acc.edge_sql(&mut b, "e", NODES)?.as_deref());
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {E_COLS} FROM {EDGES} e WHERE e.tenant = ?1 AND (?2 IS NULL OR e.type = ?2) \
                     AND (?3 IS NULL OR e.source = ?3) AND (?4 IS NULL OR e.target = ?4) \
                     AND (?5 IS NULL OR e.id > ?5){vis} ORDER BY e.id LIMIT ?6"
                ),
                b.into_params(),
            ))
            .await?;
        page(rows, limit, |r| read_edge(r, 0), |e| e.id.clone())
    }

    // ---- queries ---------------------------------------------------------

    /// Counts of nodes, edges and edges per type. Spans every label, so only
    /// the plain `engine:graph_read` grants it; the counts still leave out
    /// what the caller's rules hide.
    pub async fn stats(&self, ctx: &AccessContext) -> Result<GraphStats> {
        let tenant = self.tenant_of(ctx, Action::GraphRead)?;
        self.data.authorize_action(ctx, Action::GraphRead)?;
        let acc = self
            .access_in(ctx, &tenant, Action::GraphRead, ResourceOp::List)
            .await?;
        let mut b = SqlBuilder::new(self.dialect());
        b.bind(tenant.clone());
        let vis = and(acc.node_sql(&mut b, "n")?.as_deref());
        let nodes = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT count(*) FROM {NODES} n WHERE n.tenant = ?1{vis}"),
                b.into_params(),
            ))
            .await?;
        let mut b = SqlBuilder::new(self.dialect());
        b.bind(tenant);
        let vis = and(acc.edge_sql(&mut b, "e", NODES)?.as_deref());
        let types = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT e.type, count(*) FROM {EDGES} e WHERE e.tenant = ?1{vis} GROUP BY e.type ORDER BY e.type"
                ),
                b.into_params(),
            ))
            .await?;
        let edge_types = types
            .rows
            .iter()
            .map(|r| {
                Ok(TypeCount {
                    edge_type: text(&r[0])?,
                    count: int(&r[1])?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(GraphStats {
            nodes: nodes.scalar().and_then(Value::as_i64).unwrap_or(0),
            edges: edge_types.iter().map(|t| t.count).sum(),
            edge_types,
        })
    }

    /// Nodes one hop from `id`, with the connecting edge.
    pub async fn neighbors(&self, ctx: &AccessContext, id: &str, q: NeighborQuery) -> Result<Page<Neighbor>> {
        let (tenant, acc) = self.read(ctx, ResourceOp::List).await?;
        let types = check_types(q.edge_types)?;
        let limit = check_limit(q.limit)?;
        let offset = q.offset.unwrap_or(0);
        if self.node(&tenant, id, &acc).await?.is_none() {
            return Err(GraphError::NodeNotFound(id.to_owned()));
        }
        let mut b = SqlBuilder::new(self.dialect());
        bind_all(
            &mut b,
            vec![
                tenant.into(),
                id.into(),
                json_list(&types).into(),
                i64::from(limit + 1).into(),
                i64::from(offset).into(),
            ],
        );
        // The queried node is visible; the edge's type and the node at the
        // other end must be too.
        let vis = format!(
            "{}{}",
            and(acc.edges.compile(&mut b, "e")?.as_deref()),
            and(acc.node_sql(&mut b, "n")?.as_deref())
        );
        let type_sql = if types.is_empty() {
            String::new()
        } else {
            format!(" AND {}", self.dialect().in_json_list("e.type", "?3"))
        };
        // One side of the union: edges whose `own` end is the node, joined to
        // the node at the `nbr` end. `dir` says where the queried node sits on
        // the edge (`out` = it is the source). Self loops are listed once.
        let side = |dir: &str, own: &str, nbr: &str, extra: &str| {
            format!(
                "SELECT '{dir}' AS dir, {E_COLS}, {N_COLS} FROM {EDGES} e JOIN {NODES} n \
                 ON n.tenant = e.tenant AND n.id = e.{nbr} WHERE e.tenant = ?1 AND e.{own} = ?2{extra}{type_sql}{vis}"
            )
        };
        let loops = " AND e.source <> e.target";
        let undirected = " AND e.directed = 0 AND e.source <> e.target";
        let union = match q.direction {
            Direction::Out => format!(
                "{} UNION ALL {}",
                side("out", "source", "target", ""),
                side("in", "target", "source", undirected)
            ),
            Direction::In => format!(
                "{} UNION ALL {}",
                side("in", "target", "source", ""),
                side("out", "source", "target", undirected)
            ),
            Direction::Both => format!(
                "{} UNION ALL {}",
                side("out", "source", "target", ""),
                side("in", "target", "source", loops)
            ),
        };
        let sql = format!("SELECT * FROM ({union}) ORDER BY 10, 2 LIMIT ?4 OFFSET ?5");
        let rows = self.backend().fetch_all(&Statement::new(sql, b.into_params())).await?;
        let has_more = rows.len() > limit as usize;
        let items = rows
            .rows
            .iter()
            .take(limit as usize)
            .map(|r| {
                Ok(Neighbor {
                    direction: if text(&r[0])? == "in" {
                        Direction::In
                    } else {
                        Direction::Out
                    },
                    edge: read_edge(r, 1)?,
                    node: read_node(r, 9)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Page {
            items,
            next_after: has_more.then(|| (offset + limit).to_string()),
        })
    }

    /// Multi-hop traversal with a recursive CTE: every node reachable from
    /// `start` within `max_depth` hops, with its fewest-hops depth. Cycles
    /// cannot loop (the walk is bounded by depth and deduplicated).
    pub async fn traverse(&self, ctx: &AccessContext, q: TraverseQuery) -> Result<TraverseResult> {
        let (tenant, acc) = self.read(ctx, ResourceOp::List).await?;
        let depth = check_depth(q.max_depth, DEFAULT_DEPTH)?;
        let limit = check_limit(q.limit)?;
        let types = check_types(q.edge_types.into_vec())?;
        let (label, conds) = check_filter(q.filter)?;
        if self.node(&tenant, &q.start, &acc).await?.is_none() {
            return Err(GraphError::NodeNotFound(q.start));
        }

        let mut params: Vec<Value> = vec![
            tenant.clone().into(),
            q.start.clone().into(),
            i64::from(depth).into(),
            json_list(&types).into(),
            i64::from(if q.include_start { 0 } else { 1 }).into(),
            label.into(),
            i64::from(limit + 1).into(),
        ];
        let mut filter_sql = String::new();
        let pg = self.dialect().name() == "postgres";
        for (key, value, json) in conds {
            if pg {
                // jsonb equality keeps the JSON type: a boolean is not 1.
                params.push(key.into());
                let path = params.len();
                params.push(json.to_string().into());
                filter_sql.push_str(&format!(" AND n.properties -> ?{path} = CAST(?{} AS jsonb)", path + 1));
            } else {
                params.push(format!("$.\"{key}\"").into());
                let path = params.len();
                params.push(value);
                filter_sql.push_str(&format!(" AND json_extract(n.properties, ?{path}) = ?{}", path + 1));
            }
        }
        // Rules (FB-139): every step crosses a visible edge into a visible
        // node, so hidden parts of the graph are never walked through.
        let mut b = SqlBuilder::new(self.dialect());
        for v in params {
            b.bind(v);
        }
        let evis = and(acc.edges.compile(&mut b, "e")?.as_deref());
        let ncond = acc.node_sql(&mut b, "n")?;
        let nvis = and(ncond.as_deref());
        let params = b.into_params();
        let t = if types.is_empty() {
            String::new()
        } else {
            format!(" AND {}", self.dialect().in_json_list("e.type", "?4"))
        };
        let sql = if pg {
            // PostgreSQL allows one reference to the recursive term, and no
            // LIMIT inside it: the steps are a lateral UNION, the row cap sits
            // outside the CTE (rows are produced on demand, so it stops the
            // walk), and UNION (not ALL) is the cycle guard with the depth
            // bound.
            let step = |join: &str, next: &str, extra: &str| {
                let into = match &ncond {
                    Some(c) => format!(" JOIN {NODES} n ON n.tenant = e.tenant AND n.id = e.{next} AND {c}"),
                    None => String::new(),
                };
                format!("SELECT e.{next} AS nxt FROM {EDGES} e{into} WHERE e.tenant = ?1 AND {join}{extra}{t}{evis}")
            };
            let forward = step("e.source = w.node", "target", "");
            let backward = step("e.target = w.node", "source", "");
            let forward_undirected = step("e.target = w.node", "source", " AND e.directed = 0");
            let backward_undirected = step("e.source = w.node", "target", " AND e.directed = 0");
            let lateral = match q.direction {
                Direction::Out => format!("{forward} UNION {forward_undirected}"),
                Direction::In => format!("{backward} UNION {backward_undirected}"),
                Direction::Both => format!("{forward} UNION {backward}"),
            };
            format!(
                "WITH RECURSIVE walk(node, depth) AS (SELECT CAST(?2 AS text), 0 UNION \
                 SELECT s.nxt, w.depth + 1 FROM walk w CROSS JOIN LATERAL ({lateral}) s WHERE w.depth < ?3) \
                 SELECT {N_COLS}, m.depth FROM (SELECT node, min(depth) AS depth \
                 FROM (SELECT node, depth FROM walk LIMIT {MAX_WALK_ROWS}) capped GROUP BY node) m \
                 JOIN {NODES} n ON n.tenant = ?1 AND n.id = m.node \
                 WHERE m.depth >= ?5 AND (CAST(?6 AS text) IS NULL OR n.label = ?6){filter_sql}{nvis} \
                 ORDER BY m.depth, n.id LIMIT ?7"
            )
        } else {
            // CROSS JOIN pins the walk queue as the outer loop: left to itself the
            // planner scans the edge table for every queue row (measured: 37 s
            // instead of 0.1 s for 15 000 rows on a 10 000-edge graph).
            let step = |join: &str, next: &str, extra: &str| {
                let into = match &ncond {
                    Some(c) => format!(" JOIN {NODES} n ON n.tenant = e.tenant AND n.id = e.{next} AND {c}"),
                    None => String::new(),
                };
                format!(
                    "SELECT e.{next}, w.depth + 1 FROM walk w CROSS JOIN {EDGES} e ON e.tenant = ?1 AND {join}{into} \
                     WHERE w.depth < ?3{extra}{t}{evis}"
                )
            };
            let forward = step("e.source = w.node", "target", "");
            let backward = step("e.target = w.node", "source", "");
            let forward_undirected = step("e.target = w.node", "source", " AND e.directed = 0");
            let backward_undirected = step("e.source = w.node", "target", " AND e.directed = 0");
            let recursive = match q.direction {
                Direction::Out => format!("{forward} UNION {forward_undirected}"),
                Direction::In => format!("{backward} UNION {backward_undirected}"),
                Direction::Both => format!("{forward} UNION {backward}"),
            };
            format!(
                "WITH RECURSIVE walk(node, depth) AS (SELECT ?2, 0 UNION {recursive} LIMIT {MAX_WALK_ROWS}) \
                 SELECT {N_COLS}, m.depth FROM (SELECT node, min(depth) AS depth FROM walk GROUP BY node) m \
                 JOIN {NODES} n ON n.tenant = ?1 AND n.id = m.node \
                 WHERE m.depth >= ?5 AND (?6 IS NULL OR n.label = ?6){filter_sql}{nvis} \
                 ORDER BY m.depth, n.id LIMIT ?7"
            )
        };
        let rows = self.backend().fetch_all(&Statement::new(sql, params)).await?;
        let mut truncated = rows.len() > limit as usize;
        let nodes = rows
            .rows
            .iter()
            .take(limit as usize)
            .map(|r| {
                Ok(TraversedNode {
                    depth: u32::try_from(int(&r[5])?).unwrap_or(0),
                    node: read_node(r, 0)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let mut edges = Vec::new();
        if q.include_edges.unwrap_or(true) {
            let mut ids: Vec<String> = nodes.iter().map(|n| n.node.id.clone()).collect();
            if !q.include_start {
                ids.push(q.start.clone());
            }
            let type_sql = if types.is_empty() {
                String::new()
            } else {
                format!(" AND {}", self.dialect().in_json_list("e.type", "?3"))
            };
            let mut b = SqlBuilder::new(self.dialect());
            bind_all(
                &mut b,
                vec![
                    tenant.into(),
                    json_list(&ids).into(),
                    json_list(&types).into(),
                    i64::try_from(MAX_TRAVERSE_EDGES + 1).unwrap_or(i64::MAX).into(),
                ],
            );
            // Both ends are among the visible nodes; the type must be too.
            let evis = and(acc.edges.compile(&mut b, "e")?.as_deref());
            let src_in = self.dialect().in_json_list("e.source", "?2");
            let tgt_in = self.dialect().in_json_list("e.target", "?2");
            let rows = self
                .backend()
                .fetch_all(&Statement::new(
                    format!(
                        "SELECT {E_COLS} FROM {EDGES} e WHERE e.tenant = ?1 \
                         AND {src_in} \
                         AND {tgt_in}{type_sql}{evis} ORDER BY e.id LIMIT ?4"
                    ),
                    b.into_params(),
                ))
                .await?;
            truncated |= rows.len() > MAX_TRAVERSE_EDGES;
            edges = rows
                .rows
                .iter()
                .take(MAX_TRAVERSE_EDGES)
                .map(|r| read_edge(r, 0))
                .collect::<Result<_>>()?;
        }
        Ok(TraverseResult {
            start: q.start,
            nodes,
            edges,
            truncated,
        })
    }

    /// Fewest-hops path between two nodes (breadth-first, one indexed query
    /// per level, bounded by `max_depth` and [`MAX_VISITED`]).
    pub async fn shortest_path(&self, ctx: &AccessContext, q: PathQuery) -> Result<PathResult> {
        let (tenant, acc) = self.read(ctx, ResourceOp::List).await?;
        let max_depth = check_depth(q.max_depth, 6)?;
        let types = check_types(q.edge_types.into_vec())?;
        for id in [&q.from, &q.to] {
            if self.node(&tenant, id, &acc).await?.is_none() {
                return Err(GraphError::NodeNotFound(id.clone()));
            }
        }
        // Rules (FB-139): hops cross visible edges into visible nodes only.
        let mut b = SqlBuilder::new(self.dialect());
        for _ in 0..4 {
            b.bind(Value::Null);
        }
        let evis = and(acc.edges.compile(&mut b, "e")?.as_deref());
        let ncond = acc.node_sql(&mut b, "n")?;
        let rule_params: Vec<Value> = b.into_params().split_off(4);
        let none = PathResult {
            found: false,
            length: None,
            nodes: vec![],
            edges: vec![],
        };
        // node -> (previous node, edge id); the start has no entry.
        let mut parent: HashMap<String, (String, String)> = HashMap::new();
        let mut seen: HashSet<String> = HashSet::from([q.from.clone()]);
        let mut frontier = vec![q.from.clone()];
        let mut reached = q.from == q.to;
        let t = if types.is_empty() {
            String::new()
        } else {
            format!(" AND {}", self.dialect().in_json_list("e.type", "?3"))
        };
        let hop = |from: &str, to: &str, extra: &str| {
            let from_in = self.dialect().in_json_list(&format!("e.{from}"), "?2");
            let into = match &ncond {
                Some(c) => format!(" JOIN {NODES} n ON n.tenant = e.tenant AND n.id = e.{to} AND {c}"),
                None => String::new(),
            };
            format!(
                "SELECT e.{to} AS nxt, e.id AS eid, e.{from} AS prev FROM {EDGES} e{into} WHERE e.tenant = ?1 \
                 AND {from_in}{extra}{t}{evis}"
            )
        };
        let (a, b) = match q.direction {
            Direction::Out => (
                hop("source", "target", ""),
                hop("target", "source", " AND e.directed = 0"),
            ),
            Direction::In => (
                hop("target", "source", ""),
                hop("source", "target", " AND e.directed = 0"),
            ),
            Direction::Both => (hop("source", "target", ""), hop("target", "source", "")),
        };
        let sql = format!("{a} UNION ALL {b} ORDER BY 1, 2 LIMIT ?4");
        let row_cap = i64::try_from(MAX_VISITED * 2).unwrap_or(i64::MAX);
        for _ in 0..max_depth {
            if reached || frontier.is_empty() {
                break;
            }
            let mut params: Vec<Value> = vec![
                tenant.clone().into(),
                json_list(&frontier).into(),
                json_list(&types).into(),
                (row_cap + 1).into(),
            ];
            params.extend(rule_params.iter().cloned());
            let rows = self.backend().fetch_all(&Statement::new(sql.clone(), params)).await?;
            if rows.len() as i64 > row_cap {
                return Err(search_limit());
            }
            let mut next = Vec::new();
            for r in &rows.rows {
                let (nxt, eid, prev) = (text(&r[0])?, text(&r[1])?, text(&r[2])?);
                if seen.contains(&nxt) {
                    continue;
                }
                seen.insert(nxt.clone());
                parent.insert(nxt.clone(), (prev, eid));
                next.push(nxt);
            }
            if seen.len() > MAX_VISITED {
                return Err(search_limit());
            }
            reached = seen.contains(&q.to);
            frontier = next;
        }
        if !reached {
            return Ok(none);
        }
        let mut node_ids = vec![q.to.clone()];
        let mut edge_ids = Vec::new();
        let mut cur = q.to.clone();
        while let Some((prev, eid)) = parent.get(&cur) {
            edge_ids.push(eid.clone());
            node_ids.push(prev.clone());
            cur = prev.clone();
        }
        node_ids.reverse();
        edge_ids.reverse();
        let nodes = self.nodes_by_ids(&tenant, &node_ids).await?;
        let edges = self.edges_by_ids(&tenant, &edge_ids).await?;
        Ok(PathResult {
            found: true,
            length: Some(u32::try_from(edge_ids.len()).unwrap_or(u32::MAX)),
            nodes,
            edges,
        })
    }

    async fn nodes_by_ids(&self, tenant: &str, ids: &[String]) -> Result<Vec<Node>> {
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {N_COLS} FROM {NODES} n WHERE n.tenant = ?1 AND {}",
                    self.dialect().in_json_list("n.id", "?2")
                ),
                vec![tenant.into(), json_list(ids).into()],
            ))
            .await?;
        let mut by_id: HashMap<String, Node> = HashMap::new();
        for r in &rows.rows {
            let n = read_node(r, 0)?;
            by_id.insert(n.id.clone(), n);
        }
        ids.iter()
            .map(|id| {
                by_id
                    .get(id)
                    .cloned()
                    .ok_or_else(|| GraphError::NodeNotFound(id.clone()))
            })
            .collect()
    }

    async fn edges_by_ids(&self, tenant: &str, ids: &[String]) -> Result<Vec<Edge>> {
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {E_COLS} FROM {EDGES} e WHERE e.tenant = ?1 AND {}",
                    self.dialect().in_json_list("e.id", "?2")
                ),
                vec![tenant.into(), json_list(ids).into()],
            ))
            .await?;
        let mut by_id: HashMap<String, Edge> = HashMap::new();
        for r in &rows.rows {
            let e = read_edge(r, 0)?;
            by_id.insert(e.id.clone(), e);
        }
        ids.iter()
            .map(|id| {
                by_id
                    .get(id)
                    .cloned()
                    .ok_or_else(|| GraphError::EdgeNotFound(id.clone()))
            })
            .collect()
    }
}

// ---- helpers ---------------------------------------------------------------

/// Binds the fixed parameters `?1..?n` of a statement before its rules.
fn bind_all(b: &mut SqlBuilder<'_>, values: Vec<Value>) {
    for v in values {
        b.bind(v);
    }
}

fn search_limit() -> GraphError {
    GraphError::invalid(
        "graph_search_limit",
        format!("the search is larger than {MAX_VISITED} nodes; lower max_depth or filter edge_types"),
    )
}

async fn finish<T>(tx: Box<dyn forgebase_db::WriteTx>, result: Result<T>) -> Result<T> {
    match result {
        Ok(v) => {
            tx.commit().await?;
            Ok(v)
        }
        Err(e) => {
            if let Err(rb) = tx.rollback().await {
                tracing::warn!(error = %rb, "graph rollback failed; writer connection discarded");
            }
            Err(e)
        }
    }
}

fn page<T>(
    rows: Rows,
    limit: u32,
    read: impl Fn(&[Value]) -> Result<T>,
    key: impl Fn(&T) -> String,
) -> Result<Page<T>> {
    let has_more = rows.len() > limit as usize;
    let items = rows
        .rows
        .iter()
        .take(limit as usize)
        .map(|r| read(r))
        .collect::<Result<Vec<_>>>()?;
    let next_after = if has_more { items.last().map(&key) } else { None };
    Ok(Page { items, next_after })
}

fn text(v: &Value) -> Result<String> {
    v.as_str()
        .map(str::to_owned)
        .ok_or_else(|| GraphError::Internal("expected text column".into()))
}

fn int(v: &Value) -> Result<i64> {
    v.as_i64()
        .ok_or_else(|| GraphError::Internal("expected integer column".into()))
}

fn json(v: &Value) -> Result<Json> {
    serde_json::from_str(v.as_str().unwrap_or("{}")).map_err(|e| GraphError::Internal(format!("properties: {e}")))
}

fn read_node(r: &[Value], at: usize) -> Result<Node> {
    Ok(Node {
        id: text(&r[at])?,
        label: text(&r[at + 1])?,
        properties: json(&r[at + 2])?,
        created_at: text(&r[at + 3])?,
        updated_at: text(&r[at + 4])?,
    })
}

fn read_edge(r: &[Value], at: usize) -> Result<Edge> {
    Ok(Edge {
        id: text(&r[at])?,
        edge_type: text(&r[at + 1])?,
        source: text(&r[at + 2])?,
        target: text(&r[at + 3])?,
        directed: int(&r[at + 4])? != 0,
        properties: json(&r[at + 5])?,
        created_at: text(&r[at + 6])?,
        updated_at: text(&r[at + 7])?,
    })
}

fn json_list(items: &[String]) -> String {
    Json::Array(items.iter().map(|s| Json::String(s.clone())).collect()).to_string()
}

fn generate_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

fn id_char(c: char, extra: &[char]) -> bool {
    c.is_ascii_alphanumeric() || extra.contains(&c)
}

fn check_id(id: &str, what: &str) -> Result<String> {
    if id.is_empty() || id.len() > 128 || !id.chars().all(|c| id_char(c, &['_', '.', ':', '@', '-'])) {
        return Err(GraphError::invalid(
            "invalid_id",
            format!("{what} id must be 1 to 128 characters of A-Z a-z 0-9 _ . : @ -"),
        ));
    }
    Ok(id.to_owned())
}

fn check_label(label: Option<&str>) -> Result<String> {
    let label = label.unwrap_or_default();
    if label.chars().count() > 64 || label.chars().any(char::is_control) {
        return Err(GraphError::invalid(
            "invalid_label",
            "label must be at most 64 printable characters",
        ));
    }
    Ok(label.to_owned())
}

fn check_type(t: &str) -> Result<String> {
    if t.is_empty() || t.len() > 64 || !t.chars().all(|c| id_char(c, &['_', '.', ':', '-'])) {
        return Err(GraphError::invalid(
            "invalid_edge_type",
            "edge type must be 1 to 64 characters of A-Z a-z 0-9 _ . : -",
        ));
    }
    Ok(t.to_owned())
}

fn check_types(types: Vec<String>) -> Result<Vec<String>> {
    if types.len() > MAX_EDGE_TYPES {
        return Err(GraphError::invalid(
            "too_many_edge_types",
            format!("at most {MAX_EDGE_TYPES} edge types"),
        ));
    }
    types.iter().map(|t| check_type(t)).collect()
}

fn check_props(p: Option<Json>) -> Result<Json> {
    let p = p.unwrap_or_else(|| Json::Object(Map::new()));
    if !p.is_object() {
        return Err(GraphError::invalid(
            "invalid_properties",
            "properties must be a JSON object",
        ));
    }
    if p.to_string().len() > MAX_PROPERTIES_BYTES {
        return Err(GraphError::invalid(
            "properties_too_large",
            format!("properties are limited to {MAX_PROPERTIES_BYTES} bytes"),
        ));
    }
    Ok(p)
}

fn check_depth(depth: Option<u32>, default: u32) -> Result<u32> {
    let d = depth.unwrap_or(default);
    if d == 0 || d > MAX_DEPTH {
        return Err(GraphError::invalid(
            "invalid_depth",
            format!("max_depth must be between 1 and {MAX_DEPTH}"),
        ));
    }
    Ok(d)
}

fn check_limit(limit: Option<u32>) -> Result<u32> {
    let l = limit.unwrap_or(DEFAULT_LIMIT);
    if l == 0 || l > MAX_LIMIT {
        return Err(GraphError::invalid(
            "invalid_limit",
            format!("limit must be between 1 and {MAX_LIMIT}"),
        ));
    }
    Ok(l)
}

/// Label and property equality conditions of a validated filter.
type Conditions = (Option<String>, Vec<(String, Value, Json)>);

fn check_filter(filter: Option<NodeFilter>) -> Result<Conditions> {
    let Some(f) = filter else {
        return Ok((None, vec![]));
    };
    let label = f.label.map(|l| check_label(Some(&l))).transpose()?;
    let props = f.properties.unwrap_or_default();
    if props.len() > MAX_FILTERS {
        return Err(GraphError::invalid(
            "too_many_filters",
            format!("at most {MAX_FILTERS} property conditions"),
        ));
    }
    let mut conds = Vec::new();
    for (key, value) in props {
        if key.is_empty() || key.len() > 64 || !key.chars().all(|c| id_char(c, &['_', '-'])) {
            return Err(GraphError::invalid(
                "invalid_filter",
                "property names in a filter use A-Z a-z 0-9 _ - (up to 64)",
            ));
        }
        let v = match &value {
            Json::String(s) => Value::Text(s.clone()),
            Json::Bool(b) => Value::Integer(i64::from(b)),
            Json::Number(n) => match n.as_i64() {
                Some(i) => Value::Integer(i),
                None => Value::Real(n.as_f64().unwrap_or(f64::NAN)),
            },
            _ => {
                return Err(GraphError::invalid(
                    "invalid_filter",
                    "property conditions compare to a string, number or boolean",
                ));
            }
        };
        conds.push((key, v, value));
    }
    Ok((label, conds))
}
