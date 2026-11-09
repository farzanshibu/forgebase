//! The graph service: nodes, edges, neighbours, traversal and shortest path
//! over the `_forgebase_graph_*` tables (migration 0006).
//!
//! Rules every method follows:
//!
//! * the [`Authorizer`](forgebase_data::Authorizer) is asked first
//!   ([`Action::GraphRead`] / [`Action::GraphWrite`]), fail closed;
//! * every statement filters on the caller's tenant (`AccessContext::tenant`,
//!   `''` when none) and every value is a bound parameter; the SQL text is
//!   assembled only from fixed fragments;
//! * depth, result size, filter size and search effort are capped.

use std::collections::{HashMap, HashSet};

use forgebase_data::{AccessContext, Action, DataService};
use forgebase_db::{Backend, Rows, Statement, TxMode, Value};
use serde_json::{Map, Value as Json};

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

    fn guard(&self, ctx: &AccessContext, action: Action) -> Result<String> {
        if self.data.authorize(ctx, action, None)?.is_some() {
            return Err(GraphError::Forbidden(
                "row-level policies are not supported for graph data".into(),
            ));
        }
        Ok(ctx.tenant.clone().unwrap_or_default())
    }

    // ---- nodes -----------------------------------------------------------

    /// Creates a node.
    pub async fn create_node(&self, ctx: &AccessContext, new: NewNode) -> Result<Node> {
        let tenant = self.guard(ctx, Action::GraphWrite)?;
        let id = match new.id {
            Some(id) => check_id(&id, "node")?,
            None => generate_id(),
        };
        let label = check_label(new.label.as_deref())?;
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
        let tenant = self.guard(ctx, Action::GraphRead)?;
        self.node(&tenant, id)
            .await?
            .ok_or_else(|| GraphError::NodeNotFound(id.to_owned()))
    }

    async fn node(&self, tenant: &str, id: &str) -> Result<Option<Node>> {
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT {N_COLS} FROM {NODES} n WHERE n.tenant = ?1 AND n.id = ?2"),
                vec![tenant.into(), id.into()],
            ))
            .await?;
        rows.rows.first().map(|r| read_node(r, 0)).transpose()
    }

    /// Replaces (`merge = false`) or merge-patches (`merge = true`) a node.
    pub async fn change_node(&self, ctx: &AccessContext, id: &str, change: NodeChange, merge: bool) -> Result<Node> {
        let tenant = self.guard(ctx, Action::GraphWrite)?;
        let label = change.label.as_deref().map(|l| check_label(Some(l))).transpose()?;
        let props = change.properties.map(|p| check_props(Some(p))).transpose()?;
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let result = async {
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
            Ok(node)
        }
        .await;
        finish(tx, result).await
    }

    /// Deletes a node and every edge that touches it. Returns the number of
    /// edges removed.
    pub async fn delete_node(&self, ctx: &AccessContext, id: &str) -> Result<u64> {
        let tenant = self.guard(ctx, Action::GraphWrite)?;
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let result = async {
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
        let tenant = self.guard(ctx, Action::GraphRead)?;
        let limit = check_limit(limit)?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {N_COLS} FROM {NODES} n WHERE n.tenant = ?1 AND (?2 IS NULL OR n.label = ?2) \
                     AND (?3 IS NULL OR n.id > ?3) ORDER BY n.id LIMIT ?4"
                ),
                vec![tenant.into(), label.into(), after.into(), i64::from(limit + 1).into()],
            ))
            .await?;
        page(rows, limit, |r| read_node(r, 0), |n| n.id.clone())
    }

    // ---- edges -----------------------------------------------------------

    /// Creates an edge between two existing nodes of the caller's tenant.
    pub async fn create_edge(&self, ctx: &AccessContext, new: NewEdge) -> Result<Edge> {
        let tenant = self.guard(ctx, Action::GraphWrite)?;
        let id = match new.id {
            Some(id) => check_id(&id, "edge")?,
            None => generate_id(),
        };
        let edge_type = check_type(&new.edge_type)?;
        let source = check_id(&new.source, "source node")?;
        let target = check_id(&new.target, "target node")?;
        let directed = new.directed.unwrap_or(true);
        let props = check_props(new.properties)?;
        let now = forgebase_core::now_rfc3339();
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let result = async {
            let nodes = tx
                .fetch_all(&Statement::new(
                    format!("SELECT id FROM {NODES} WHERE tenant = ?1 AND id IN (?2, ?3)"),
                    vec![tenant.clone().into(), source.clone().into(), target.clone().into()],
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
        let tenant = self.guard(ctx, Action::GraphRead)?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT {E_COLS} FROM {EDGES} e WHERE e.tenant = ?1 AND e.id = ?2"),
                vec![tenant.into(), id.into()],
            ))
            .await?;
        rows.rows
            .first()
            .map(|r| read_edge(r, 0))
            .transpose()?
            .ok_or_else(|| GraphError::EdgeNotFound(id.to_owned()))
    }

    /// Merge-patches an edge's properties.
    pub async fn change_edge(&self, ctx: &AccessContext, id: &str, change: EdgeChange) -> Result<Edge> {
        let tenant = self.guard(ctx, Action::GraphWrite)?;
        let props = change.properties.map(|p| check_props(Some(p))).transpose()?;
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let result = async {
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
            Ok(edge)
        }
        .await;
        finish(tx, result).await
    }

    /// Deletes an edge.
    pub async fn delete_edge(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        let tenant = self.guard(ctx, Action::GraphWrite)?;
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let result = async {
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
        let tenant = self.guard(ctx, Action::GraphRead)?;
        let limit = check_limit(limit)?;
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                format!(
                    "SELECT {E_COLS} FROM {EDGES} e WHERE e.tenant = ?1 AND (?2 IS NULL OR e.type = ?2) \
                     AND (?3 IS NULL OR e.source = ?3) AND (?4 IS NULL OR e.target = ?4) \
                     AND (?5 IS NULL OR e.id > ?5) ORDER BY e.id LIMIT ?6"
                ),
                vec![
                    tenant.into(),
                    edge_type.into(),
                    source.into(),
                    target.into(),
                    after.into(),
                    i64::from(limit + 1).into(),
                ],
            ))
            .await?;
        page(rows, limit, |r| read_edge(r, 0), |e| e.id.clone())
    }

    // ---- queries ---------------------------------------------------------

    /// Counts of nodes, edges and edges per type.
    pub async fn stats(&self, ctx: &AccessContext) -> Result<GraphStats> {
        let tenant = self.guard(ctx, Action::GraphRead)?;
        let nodes = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT count(*) FROM {NODES} WHERE tenant = ?1"),
                vec![tenant.clone().into()],
            ))
            .await?;
        let types = self
            .backend()
            .fetch_all(&Statement::new(
                format!("SELECT type, count(*) FROM {EDGES} WHERE tenant = ?1 GROUP BY type ORDER BY type"),
                vec![tenant.into()],
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
        let tenant = self.guard(ctx, Action::GraphRead)?;
        let types = check_types(q.edge_types)?;
        let limit = check_limit(q.limit)?;
        let offset = q.offset.unwrap_or(0);
        if self.node(&tenant, id).await?.is_none() {
            return Err(GraphError::NodeNotFound(id.to_owned()));
        }
        let type_sql = if types.is_empty() {
            ""
        } else {
            " AND e.type IN (SELECT value FROM json_each(?3))"
        };
        // One side of the union: edges whose `own` end is the node, joined to
        // the node at the `nbr` end. `dir` says where the queried node sits on
        // the edge (`out` = it is the source). Self loops are listed once.
        let side = |dir: &str, own: &str, nbr: &str, extra: &str| {
            format!(
                "SELECT '{dir}' AS dir, {E_COLS}, {N_COLS} FROM {EDGES} e JOIN {NODES} n \
                 ON n.tenant = e.tenant AND n.id = e.{nbr} WHERE e.tenant = ?1 AND e.{own} = ?2{extra}{type_sql}"
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
        let rows = self
            .backend()
            .fetch_all(&Statement::new(
                sql,
                vec![
                    tenant.into(),
                    id.into(),
                    json_list(&types).into(),
                    i64::from(limit + 1).into(),
                    i64::from(offset).into(),
                ],
            ))
            .await?;
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
        let tenant = self.guard(ctx, Action::GraphRead)?;
        let depth = check_depth(q.max_depth, DEFAULT_DEPTH)?;
        let limit = check_limit(q.limit)?;
        let types = check_types(q.edge_types.into_vec())?;
        let (label, conds) = check_filter(q.filter)?;
        if self.node(&tenant, &q.start).await?.is_none() {
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
        for (key, value) in conds {
            params.push(format!("$.\"{key}\"").into());
            let path = params.len();
            params.push(value);
            filter_sql.push_str(&format!(" AND json_extract(n.properties, ?{path}) = ?{}", path + 1));
        }
        let t = if types.is_empty() {
            ""
        } else {
            " AND e.type IN (SELECT value FROM json_each(?4))"
        };
        // CROSS JOIN pins the walk queue as the outer loop: left to itself the
        // planner scans the edge table for every queue row (measured: 37 s
        // instead of 0.1 s for 15 000 rows on a 10 000-edge graph).
        let step = |join: &str, next: &str, extra: &str| {
            format!(
                "SELECT e.{next}, w.depth + 1 FROM walk w CROSS JOIN {EDGES} e ON e.tenant = ?1 AND {join} \
                 WHERE w.depth < ?3{extra}{t}"
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
        let sql = format!(
            "WITH RECURSIVE walk(node, depth) AS (SELECT ?2, 0 UNION {recursive} LIMIT {MAX_WALK_ROWS}) \
             SELECT {N_COLS}, m.depth FROM (SELECT node, min(depth) AS depth FROM walk GROUP BY node) m \
             JOIN {NODES} n ON n.tenant = ?1 AND n.id = m.node \
             WHERE m.depth >= ?5 AND (?6 IS NULL OR n.label = ?6){filter_sql} \
             ORDER BY m.depth, n.id LIMIT ?7"
        );
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
                ""
            } else {
                " AND e.type IN (SELECT value FROM json_each(?3))"
            };
            let rows = self
                .backend()
                .fetch_all(&Statement::new(
                    format!(
                        "SELECT {E_COLS} FROM {EDGES} e WHERE e.tenant = ?1 \
                         AND e.source IN (SELECT value FROM json_each(?2)) \
                         AND e.target IN (SELECT value FROM json_each(?2)){type_sql} ORDER BY e.id LIMIT ?4"
                    ),
                    vec![
                        tenant.into(),
                        json_list(&ids).into(),
                        json_list(&types).into(),
                        i64::try_from(MAX_TRAVERSE_EDGES + 1).unwrap_or(i64::MAX).into(),
                    ],
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
        let tenant = self.guard(ctx, Action::GraphRead)?;
        let max_depth = check_depth(q.max_depth, 6)?;
        let types = check_types(q.edge_types.into_vec())?;
        for id in [&q.from, &q.to] {
            if self.node(&tenant, id).await?.is_none() {
                return Err(GraphError::NodeNotFound(id.clone()));
            }
        }
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
            ""
        } else {
            " AND e.type IN (SELECT value FROM json_each(?3))"
        };
        let hop = |from: &str, to: &str, extra: &str| {
            format!(
                "SELECT e.{to} AS nxt, e.id AS eid, e.{from} AS prev FROM {EDGES} e WHERE e.tenant = ?1 \
                 AND e.{from} IN (SELECT value FROM json_each(?2)){extra}{t}"
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
            let rows = self
                .backend()
                .fetch_all(&Statement::new(
                    sql.clone(),
                    vec![
                        tenant.clone().into(),
                        json_list(&frontier).into(),
                        json_list(&types).into(),
                        (row_cap + 1).into(),
                    ],
                ))
                .await?;
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
                    "SELECT {N_COLS} FROM {NODES} n WHERE n.tenant = ?1 AND n.id IN (SELECT value FROM json_each(?2))"
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
                    "SELECT {E_COLS} FROM {EDGES} e WHERE e.tenant = ?1 AND e.id IN (SELECT value FROM json_each(?2))"
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
type Conditions = (Option<String>, Vec<(String, Value)>);

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
        let v = match value {
            Json::String(s) => Value::Text(s),
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
        conds.push((key, v));
    }
    Ok((label, conds))
}
