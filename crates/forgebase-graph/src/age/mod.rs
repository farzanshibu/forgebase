//! The derived openCypher graph on Apache AGE (ADR-0029 §1.5, FB-163).
//!
//! ForgeBase's graph tables stay canonical on both backends; on PostgreSQL
//! with AGE each tenant that uses openCypher also gets a derived AGE graph:
//!
//! * **Names.** Graph `fbg_<first 16 hex of sha256(tenant)>` (`fbg_shared`
//!   for the `''` tenant). Vertex label = ForgeBase label, edge label = edge
//!   type; a name AGE cannot take (not `[A-Za-z][A-Za-z0-9_]{0,62}`, e.g.
//!   the empty label or `my-label`) goes to the catch-all label `_fbnode` /
//!   `_fbedge` with a `__label` / `__type` property. Properties are the
//!   ForgeBase JSON plus `__id` (edges also `__source`, `__target` and, when
//!   undirected, `__undirected: true`).
//! * **Map and state** (extension DDL, `forgebase_db::EXTENSION_DDL`):
//!   `_forgebase_graph_age_ids (tenant, kind, id, age_id)` and
//!   `_forgebase_graph_age_state` (one row per tenant: `materialising`,
//!   `ready`, `stale` or `failed`, progress, the placement epoch it was built
//!   for).
//! * **Writes.** Label tables are written with plain SQL (never Cypher
//!   writes, never AGE's CSV loaders) in the same write transaction as the
//!   canonical change ([`sync_node`], [`sync_edge`]), while the tenant's
//!   graph is `materialising` or `ready`. The materialisation job copies in
//!   batches of [`BATCH`] rows, one write transaction each, and skips rows a
//!   write already synced (PostgreSQL has one writer, so a batch and a write
//!   never interleave).
//! * **Visibility.** Every label table (and the parents `_ag_label_vertex` /
//!   `_ag_label_edge`) has row-level security with one policy that reads the
//!   request's claims (`forgebase.graph_vis_*`): label ids allowed whole,
//!   `graphid`s of rows that pass a filter rule, and for edges both ends
//!   visible. [`visibility`] computes them from the caller's
//!   [`GraphAccess`] with the same compiled rules as the REST API, so a
//!   Cypher read sees exactly what REST sees. AGE's
//!   `age.enforce_rls_in_traversal` (on, superuser-only; FB-160 patch 0002)
//!   applies the policies inside variable-length and shortest-path
//!   traversals.

pub(crate) mod agtype;
pub mod tokenize;

use forgebase_data::query::SqlBuilder;
use forgebase_db::{Backend, Statement, Value, WriteTx};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::access::{GraphAccess, Rules};
use crate::error::GraphError;

type Result<T> = std::result::Result<T, GraphError>;

/// The map table.
pub(crate) const MAP: &str = "_forgebase_graph_age_ids";
/// The per-tenant state table.
pub(crate) const STATE: &str = "_forgebase_graph_age_state";
/// Catch-all vertex label.
pub const CATCH_NODE: &str = "_fbnode";
/// Catch-all edge label.
pub const CATCH_EDGE: &str = "_fbedge";
/// Rows copied per materialisation transaction.
pub const BATCH: i64 = 1000;
/// Most row ids a caller's filter rules may select for one Cypher read
/// (they travel in the claims); above it the read is refused with
/// `403 cypher_rules_unsupported`.
pub const MAX_VISIBLE_IDS: usize = 100_000;

const NODES: &str = "_forgebase_graph_nodes";
const EDGES: &str = "_forgebase_graph_edges";

/// Nodes or edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Node,
    Edge,
}

impl Kind {
    fn code(self) -> &'static str {
        match self {
            Kind::Node => "n",
            Kind::Edge => "e",
        }
    }

    fn age(self) -> &'static str {
        match self {
            Kind::Node => "v",
            Kind::Edge => "e",
        }
    }

    fn catch_all(self) -> &'static str {
        match self {
            Kind::Node => CATCH_NODE,
            Kind::Edge => CATCH_EDGE,
        }
    }

    fn parent(self) -> &'static str {
        match self {
            Kind::Node => "_ag_label_vertex",
            Kind::Edge => "_ag_label_edge",
        }
    }
}

/// The AGE graph of `tenant`.
pub fn graph_name(tenant: &str) -> String {
    if tenant.is_empty() {
        return "fbg_shared".to_owned();
    }
    let digest = Sha256::digest(tenant.as_bytes());
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    format!("fbg_{hex}")
}

/// Whether AGE can take `name` as a label of its own.
pub fn representable(name: &str) -> bool {
    let mut chars = name.chars();
    name.len() <= 63
        && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn age_label(name: &str, kind: Kind) -> &str {
    if representable(name) { name } else { kind.catch_all() }
}

/// A PostgreSQL identifier, quoted.
fn q(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

fn text(v: Option<&Value>) -> Option<String> {
    match v {
        Some(Value::Text(s)) => Some(s.clone()),
        Some(Value::Integer(i)) => Some(i.to_string()),
        _ => None,
    }
}

fn int(v: Option<&Value>) -> Option<i64> {
    match v {
        Some(Value::Integer(i)) => Some(*i),
        Some(Value::Text(s)) => s.parse().ok(),
        _ => None,
    }
}

fn stmt(sql: impl Into<String>, params: Vec<Value>) -> Statement {
    Statement::new(sql, params)
}

// ---- state ------------------------------------------------------------------

/// One tenant's derived graph.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct State {
    pub graph: String,
    pub state: String,
    pub phase: String,
    pub job_id: Option<String>,
    pub epoch: i64,
    pub nodes: i64,
    pub edges: i64,
    pub done: i64,
    pub error: Option<String>,
    pub updated_at: String,
}

const STATE_COLS: &str = "graph, state, phase, job_id, epoch, nodes, edges, done, error, updated_at";

fn read_state(r: &[Value]) -> State {
    State {
        graph: text(r.first()).unwrap_or_default(),
        state: text(r.get(1)).unwrap_or_default(),
        phase: text(r.get(2)).unwrap_or_default(),
        job_id: text(r.get(3)),
        epoch: int(r.get(4)).unwrap_or(0),
        nodes: int(r.get(5)).unwrap_or(0),
        edges: int(r.get(6)).unwrap_or(0),
        done: int(r.get(7)).unwrap_or(0),
        error: text(r.get(8)),
        updated_at: text(r.get(9)).unwrap_or_default(),
    }
}

/// The state of `tenant`'s derived graph (read where `b` reads: callers
/// pass the primary).
pub(crate) async fn state(b: &dyn Backend, tenant: &str) -> Result<Option<State>> {
    let rows = b
        .fetch_all(&stmt(
            format!("SELECT {STATE_COLS} FROM {STATE} WHERE tenant = ?1"),
            vec![tenant.into()],
        ))
        .await?;
    Ok(rows.rows.first().map(|r| read_state(r)))
}

async fn state_tx(tx: &mut dyn WriteTx, tenant: &str) -> Result<Option<State>> {
    let rows = tx
        .fetch_all(&stmt(
            format!("SELECT {STATE_COLS} FROM {STATE} WHERE tenant = ?1"),
            vec![tenant.into()],
        ))
        .await?;
    Ok(rows.rows.first().map(|r| read_state(r)))
}

/// The graph that writes of `tenant` keep in step, if any.
async fn synced_graph(tx: &mut dyn WriteTx, tenant: &str) -> Result<Option<String>> {
    let rows = tx
        .fetch_all(&stmt(
            format!("SELECT graph FROM {STATE} WHERE tenant = ?1 AND state IN ('materialising', 'ready')"),
            vec![tenant.into()],
        ))
        .await?;
    Ok(rows.rows.first().and_then(|r| text(r.first())))
}

// ---- graph and label DDL ------------------------------------------------------

/// Puts `ag_catalog` first on the transaction's `search_path` and returns
/// the path to restore: AGE's DDL functions name `graphid_ops` and friends
/// unqualified, and `graphid` comparisons (`=` in statements and in the
/// policies, which store the resolved operator) live in `ag_catalog`.
async fn push_path(tx: &mut dyn WriteTx) -> Result<String> {
    let rows = tx
        .fetch_all(&Statement::raw(
            "SELECT current_setting('search_path'), \
             set_config('search_path', 'ag_catalog, ' || current_setting('search_path'), true)",
        ))
        .await?;
    Ok(text(rows.rows.first().and_then(|r| r.first())).unwrap_or_default())
}

async fn pop_path(tx: &mut dyn WriteTx, saved: String) -> Result<()> {
    tx.fetch_all(&stmt("SELECT set_config('search_path', ?1, true)", vec![saved.into()]))
        .await?;
    Ok(())
}

/// Runs `f` with `ag_catalog` first on the path (see [`push_path`]).
macro_rules! with_ag_path {
    ($tx:expr, $body:expr) => {{
        let saved = push_path(&mut *$tx).await?;
        let r = $body;
        // After an error the transaction is aborted: report that error.
        if r.is_ok() {
            pop_path(&mut *$tx, saved).await?;
        }
        r
    }};
}

async fn graph_exists(tx: &mut dyn WriteTx, graph: &str) -> Result<bool> {
    let rows = tx
        .fetch_all(&stmt(
            "SELECT 1 FROM ag_catalog.ag_graph WHERE name = ?1::text::name",
            vec![graph.into()],
        ))
        .await?;
    Ok(!rows.is_empty())
}

/// The policy of a label table (see the module documentation).
fn policy(kind: Kind) -> &'static str {
    match kind {
        Kind::Node => {
            "(SELECT forgebase.graph_vis_all('v')) \
             OR ag_catalog._extract_label_id(id)::integer = ANY ((SELECT forgebase.graph_vis_labels('v'))::integer[]) \
             OR id IN (SELECT forgebase.graph_vis_ids('v'))"
        }
        Kind::Edge => {
            "((SELECT forgebase.graph_vis_all('e')) \
               OR ag_catalog._extract_label_id(id)::integer = ANY ((SELECT forgebase.graph_vis_labels('e'))::integer[]) \
               OR id IN (SELECT forgebase.graph_vis_ids('e'))) \
             AND ((SELECT forgebase.graph_vis_all('v')) \
               OR ((ag_catalog._extract_label_id(start_id)::integer = ANY ((SELECT forgebase.graph_vis_labels('v'))::integer[]) \
                    OR start_id IN (SELECT forgebase.graph_vis_ids('v'))) \
                   AND (ag_catalog._extract_label_id(end_id)::integer = ANY ((SELECT forgebase.graph_vis_labels('v'))::integer[]) \
                    OR end_id IN (SELECT forgebase.graph_vis_ids('v')))))"
        }
    }
}

/// Row-level security on one table of `graph`: no row without the
/// request's claims. The server and reader roles bypass it (they are
/// trusted); everyone else, `forgebase_rls` and direct logins alike, sees
/// what the claims allow (direct logins have none).
async fn protect(tx: &mut dyn WriteTx, graph: &str, table: &str, kind: Kind) -> Result<()> {
    let t = format!("{}.{}", q(graph), q(table));
    for sql in [
        format!("ALTER TABLE {t} ENABLE ROW LEVEL SECURITY"),
        format!("DROP POLICY IF EXISTS fb_read ON {t}"),
        format!("CREATE POLICY fb_read ON {t} FOR SELECT TO PUBLIC USING ({})", policy(kind)),
    ] {
        tx.execute(&Statement::raw(sql)).await?;
    }
    Ok(())
}

/// Creates `graph` (if missing) with its grants and the parents' policies.
async fn create_graph(tx: &mut dyn WriteTx, graph: &str) -> Result<()> {
    if !graph_exists(tx, graph).await? {
        tx.fetch_all(&stmt(
            "SELECT ag_catalog.create_graph(?1::text::name)::text",
            vec![graph.into()],
        ))
        .await?;
    }
    let g = q(graph);
    for sql in [
        format!("GRANT USAGE ON SCHEMA {g} TO PUBLIC"),
        format!("ALTER DEFAULT PRIVILEGES IN SCHEMA {g} GRANT SELECT ON TABLES TO PUBLIC"),
        format!("GRANT SELECT ON ALL TABLES IN SCHEMA {g} TO PUBLIC"),
    ] {
        tx.execute(&Statement::raw(sql)).await?;
    }
    protect(tx, graph, Kind::Node.parent(), Kind::Node).await?;
    protect(tx, graph, Kind::Edge.parent(), Kind::Edge).await?;
    Ok(())
}

/// An AGE label table.
#[derive(Debug, Clone)]
struct Label {
    /// AGE label id.
    id: i64,
    /// The label (table) name.
    name: String,
    /// Its id sequence, schema-qualified and quoted.
    seq: String,
    catch_all: bool,
}

async fn find_label(tx: &mut dyn WriteTx, graph: &str, name: &str, kind: Kind) -> Result<Option<Label>> {
    let rows = tx
        .fetch_all(&stmt(
            "SELECT l.id::bigint, l.seq_name::text FROM ag_catalog.ag_label l \
             JOIN ag_catalog.ag_graph g ON g.graphid = l.graph \
             WHERE g.name = ?1::text::name AND l.name = ?2::text::name AND l.kind::text = ?3",
            vec![graph.into(), name.into(), kind.age().into()],
        ))
        .await?;
    Ok(rows.rows.first().and_then(|r| {
        Some(Label {
            id: int(r.first())?,
            name: name.to_owned(),
            seq: format!("{}.{}", q(graph), q(&text(r.get(1))?)),
            catch_all: name == kind.catch_all(),
        })
    }))
}

/// The label table of ForgeBase label (or edge type) `fb_name`, created
/// with its policy when missing.
async fn ensure_label(tx: &mut dyn WriteTx, graph: &str, fb_name: &str, kind: Kind) -> Result<Label> {
    let name = age_label(fb_name, kind);
    if let Some(l) = find_label(tx, graph, name, kind).await? {
        return Ok(l);
    }
    let create = match kind {
        Kind::Node => "SELECT ag_catalog.create_vlabel(?1::text::cstring, ?2::text::cstring)::text",
        Kind::Edge => "SELECT ag_catalog.create_elabel(?1::text::cstring, ?2::text::cstring)::text",
    };
    tx.fetch_all(&stmt(create, vec![graph.into(), name.into()])).await?;
    protect(tx, graph, name, kind).await?;
    find_label(tx, graph, name, kind)
        .await?
        .ok_or_else(|| GraphError::Internal(format!("AGE label {name:?} was not created")))
}

// ---- rows -------------------------------------------------------------------

/// Which canonical rows a copy takes: one id, or the ids in `(lo, hi]`.
#[derive(Debug, Clone, Copy)]
enum Range<'a> {
    One(&'a str),
    Between(&'a str, &'a str),
}

impl Range<'_> {
    /// The condition on `alias.id`, with parameters from `?3`.
    fn sql(&self, alias: &str) -> String {
        match self {
            // `?4` is unused but needs a type.
            Range::One(_) => format!("({alias}.id = ?3 AND ?4::text IS NULL)"),
            Range::Between(..) => format!("{alias}.id > ?3 AND {alias}.id <= ?4"),
        }
    }

    fn params(&self) -> Vec<Value> {
        match self {
            Range::One(id) => vec![(*id).into(), Value::Null],
            Range::Between(lo, hi) => vec![(*lo).into(), (*hi).into()],
        }
    }
}

fn node_props(alias: &str, catch_all: bool) -> String {
    let extra = if catch_all {
        format!(", '__label', {alias}.label")
    } else {
        String::new()
    };
    format!("({alias}.properties || jsonb_build_object('__id', {alias}.id{extra}))::ag_catalog.agtype")
}

fn edge_props(alias: &str, catch_all: bool) -> String {
    let extra = if catch_all {
        format!(", '__type', {alias}.type")
    } else {
        String::new()
    };
    format!(
        "({alias}.properties || jsonb_build_object('__id', {alias}.id, '__source', {alias}.source, \
         '__target', {alias}.target{extra}) || CASE WHEN {alias}.directed = 0 \
         THEN jsonb_build_object('__undirected', true) ELSE '{{}}'::jsonb END)::ag_catalog.agtype"
    )
}

/// Copies the canonical nodes of label `fb_label` in `range` that are not
/// in the map yet. Returns how many.
async fn copy_nodes(
    tx: &mut dyn WriteTx,
    graph: &str,
    tenant: &str,
    fb_label: &str,
    range: Range<'_>,
) -> Result<u64> {
    let l = ensure_label(tx, graph, fb_label, Kind::Node).await?;
    let sql = format!(
        "WITH b AS MATERIALIZED (SELECT n.id, n.label, n.properties, \
           ag_catalog._graphid(?5::bigint::integer, nextval(?6::text::regclass)) AS gid FROM {NODES} n \
           WHERE n.tenant = ?1 AND n.label = ?2 AND {range} AND NOT EXISTS \
           (SELECT 1 FROM {MAP} m WHERE m.tenant = n.tenant AND m.kind = 'n' AND m.id = n.id)), \
         ins AS (INSERT INTO {g}.{t} (id, properties) SELECT b.gid, {props} FROM b) \
         INSERT INTO {MAP} (tenant, kind, id, age_id) SELECT ?1, 'n', b.id, b.gid FROM b",
        range = range.sql("n"),
        g = q(graph),
        t = q(&l.name),
        props = node_props("b", l.catch_all),
    );
    let mut params: Vec<Value> = vec![tenant.into(), fb_label.into()];
    params.extend(range.params());
    params.push(l.id.into());
    params.push(l.seq.clone().into());
    Ok(tx.execute(&stmt(sql, params)).await?.rows_affected)
}

/// Copies the canonical edges of type `fb_type` in `range` whose ends are
/// mapped and that are not mapped themselves. Returns how many.
async fn copy_edges(tx: &mut dyn WriteTx, graph: &str, tenant: &str, fb_type: &str, range: Range<'_>) -> Result<u64> {
    let l = ensure_label(tx, graph, fb_type, Kind::Edge).await?;
    let sql = format!(
        "WITH b AS MATERIALIZED (SELECT e.id, e.type, e.source, e.target, e.directed, e.properties, \
           ms.age_id AS s, mt.age_id AS t, ag_catalog._graphid(?5::bigint::integer, nextval(?6::text::regclass)) AS gid \
           FROM {EDGES} e \
           JOIN {MAP} ms ON ms.tenant = e.tenant AND ms.kind = 'n' AND ms.id = e.source \
           JOIN {MAP} mt ON mt.tenant = e.tenant AND mt.kind = 'n' AND mt.id = e.target \
           WHERE e.tenant = ?1 AND e.type = ?2 AND {range} AND NOT EXISTS \
           (SELECT 1 FROM {MAP} m WHERE m.tenant = e.tenant AND m.kind = 'e' AND m.id = e.id)), \
         ins AS (INSERT INTO {g}.{t} (id, start_id, end_id, properties) SELECT b.gid, b.s, b.t, {props} FROM b) \
         INSERT INTO {MAP} (tenant, kind, id, age_id) SELECT ?1, 'e', b.id, b.gid FROM b",
        range = range.sql("e"),
        g = q(graph),
        t = q(&l.name),
        props = edge_props("b", l.catch_all),
    );
    let mut params: Vec<Value> = vec![tenant.into(), fb_type.into()];
    params.extend(range.params());
    params.push(l.id.into());
    params.push(l.seq.clone().into());
    Ok(tx.execute(&stmt(sql, params)).await?.rows_affected)
}

/// Deletes AGE vertex `gid` of node `id`, the AGE edges at it and their
/// map rows.
async fn delete_vertex(tx: &mut dyn WriteTx, graph: &str, tenant: &str, id: &str, gid: &str) -> Result<()> {
    let g = q(graph);
    tx.execute(&stmt(
        format!(
            "WITH d AS (DELETE FROM {g}._ag_label_edge WHERE start_id = ?2::text::ag_catalog.graphid \
             OR end_id = ?2::text::ag_catalog.graphid RETURNING id) \
             DELETE FROM {MAP} m USING d WHERE m.tenant = ?1 AND m.kind = 'e' AND m.age_id = d.id"
        ),
        vec![tenant.into(), gid.into()],
    ))
    .await?;
    tx.execute(&stmt(
        format!("DELETE FROM {g}._ag_label_vertex WHERE id = ?1::text::ag_catalog.graphid"),
        vec![gid.into()],
    ))
    .await?;
    tx.execute(&stmt(
        format!("DELETE FROM {MAP} WHERE tenant = ?1 AND kind = 'n' AND id = ?2"),
        vec![tenant.into(), id.into()],
    ))
    .await?;
    Ok(())
}

/// Brings node `id` of `tenant` in the derived graph in line with the
/// canonical row, in the caller's write transaction (after its change):
/// inserted, updated, moved to another label table (its edges follow) or
/// deleted with its edges. A no-op unless the tenant's graph is
/// materialising or ready.
pub(crate) async fn sync_node(tx: &mut dyn WriteTx, tenant: &str, id: &str) -> Result<()> {
    let Some(graph) = synced_graph(tx, tenant).await? else {
        return Ok(());
    };
    with_ag_path!(tx, sync_node_in(tx, &graph, tenant, id).await)
}

async fn sync_node_in(tx: &mut dyn WriteTx, graph: &str, tenant: &str, id: &str) -> Result<()> {
    let graph = graph.to_owned();
    let rows = tx
        .fetch_all(&stmt(
            format!(
                "SELECT n.label, m.age_id::text, CASE WHEN m.age_id IS NOT NULL \
                 THEN ag_catalog._extract_label_id(m.age_id)::bigint END \
                 FROM (SELECT 1) AS x \
                 LEFT JOIN {NODES} n ON n.tenant = ?1 AND n.id = ?2 \
                 LEFT JOIN {MAP} m ON m.tenant = ?1 AND m.kind = 'n' AND m.id = ?2"
            ),
            vec![tenant.into(), id.into()],
        ))
        .await?;
    let r = rows.rows.first().cloned().unwrap_or_default();
    let (label, gid, old_label) = (text(r.first()), text(r.get(1)), int(r.get(2)));
    match (label, gid) {
        (None, None) => {}
        (None, Some(gid)) => delete_vertex(tx, &graph, tenant, id, &gid).await?,
        (Some(label), None) => {
            copy_nodes(tx, &graph, tenant, &label, Range::One(id)).await?;
        }
        (Some(label), Some(gid)) => {
            let l = ensure_label(tx, &graph, &label, Kind::Node).await?;
            let g = q(&graph);
            if Some(l.id) == old_label {
                tx.execute(&stmt(
                    format!(
                        "UPDATE {g}._ag_label_vertex v SET properties = {props} FROM {NODES} n \
                         WHERE n.tenant = ?1 AND n.id = ?2 AND v.id = ?3::text::ag_catalog.graphid",
                        props = node_props("n", l.catch_all),
                    ),
                    vec![tenant.into(), id.into(), gid.as_str().into()],
                ))
                .await?;
            } else {
                // A new label: a new vertex in the other table; its edges
                // are re-pointed, then the old vertex goes.
                tx.execute(&stmt(
                    format!("DELETE FROM {MAP} WHERE tenant = ?1 AND kind = 'n' AND id = ?2"),
                    vec![tenant.into(), id.into()],
                ))
                .await?;
                copy_nodes(tx, &graph, tenant, &label, Range::One(id)).await?;
                for end in ["start_id", "end_id"] {
                    tx.execute(&stmt(
                        format!(
                            "UPDATE {g}._ag_label_edge SET {end} = m.age_id FROM {MAP} m \
                             WHERE m.tenant = ?1 AND m.kind = 'n' AND m.id = ?2 AND {end} = ?3::text::ag_catalog.graphid"
                        ),
                        vec![tenant.into(), id.into(), gid.as_str().into()],
                    ))
                    .await?;
                }
                tx.execute(&stmt(
                    format!("DELETE FROM {g}._ag_label_vertex WHERE id = ?1::text::ag_catalog.graphid"),
                    vec![gid.as_str().into()],
                ))
                .await?;
            }
        }
    }
    Ok(())
}

/// [`sync_node`] for edge `id` (edges change only their properties).
pub(crate) async fn sync_edge(tx: &mut dyn WriteTx, tenant: &str, id: &str) -> Result<()> {
    let Some(graph) = synced_graph(tx, tenant).await? else {
        return Ok(());
    };
    with_ag_path!(tx, sync_edge_in(tx, &graph, tenant, id).await)
}

async fn sync_edge_in(tx: &mut dyn WriteTx, graph: &str, tenant: &str, id: &str) -> Result<()> {
    let graph = graph.to_owned();
    let rows = tx
        .fetch_all(&stmt(
            format!(
                "SELECT e.type, m.age_id::text \
                 FROM (SELECT 1) AS x \
                 LEFT JOIN {EDGES} e ON e.tenant = ?1 AND e.id = ?2 \
                 LEFT JOIN {MAP} m ON m.tenant = ?1 AND m.kind = 'e' AND m.id = ?2"
            ),
            vec![tenant.into(), id.into()],
        ))
        .await?;
    let r = rows.rows.first().cloned().unwrap_or_default();
    let g = q(&graph);
    match (text(r.first()), text(r.get(1))) {
        (None, None) => {}
        (None, Some(gid)) => {
            tx.execute(&stmt(
                format!("DELETE FROM {g}._ag_label_edge WHERE id = ?1::text::ag_catalog.graphid"),
                vec![gid.as_str().into()],
            ))
            .await?;
            tx.execute(&stmt(
                format!("DELETE FROM {MAP} WHERE tenant = ?1 AND kind = 'e' AND id = ?2"),
                vec![tenant.into(), id.into()],
            ))
            .await?;
        }
        (Some(ty), None) => {
            copy_edges(tx, &graph, tenant, &ty, Range::One(id)).await?;
        }
        (Some(ty), Some(gid)) => {
            let catch_all = !representable(&ty);
            tx.execute(&stmt(
                format!(
                    "UPDATE {g}._ag_label_edge v SET properties = {props} FROM {EDGES} e \
                     WHERE e.tenant = ?1 AND e.id = ?2 AND v.id = ?3::text::ag_catalog.graphid",
                    props = edge_props("e", catch_all),
                ),
                vec![tenant.into(), id.into(), gid.as_str().into()],
            ))
            .await?;
        }
    }
    Ok(())
}

// ---- materialisation ----------------------------------------------------------

/// Starts (or restarts) the derived graph of `tenant` for placement epoch
/// `epoch`: drops what was there, creates the graph and the state row
/// (`materialising`). The caller commits, then runs [`step`] until done.
pub(crate) async fn start(tx: &mut dyn WriteTx, tenant: &str, epoch: i64, job_id: Option<&str>) -> Result<()> {
    drop_in(tx, tenant).await?;
    let graph = graph_name(tenant);
    with_ag_path!(tx, create_graph(tx, &graph).await)?;
    let now = forgebase_core::now_rfc3339();
    tx.execute(&stmt(
        format!(
            "INSERT INTO {STATE} (tenant, graph, state, phase, cursor_id, job_id, epoch, nodes, edges, done, \
             started_at, updated_at) VALUES (?1, ?2, 'materialising', 'nodes', '', ?3, ?4, \
             (SELECT count(*) FROM {NODES} WHERE tenant = ?1), (SELECT count(*) FROM {EDGES} WHERE tenant = ?1), \
             0, ?5, ?5)"
        ),
        vec![tenant.into(), graph.into(), job_id.into(), epoch.into(), now.into()],
    ))
    .await?;
    Ok(())
}

/// Records the job that materialises `tenant`.
pub(crate) async fn set_job(tx: &mut dyn WriteTx, tenant: &str, job_id: &str) -> Result<()> {
    tx.execute(&stmt(
        format!("UPDATE {STATE} SET job_id = ?2 WHERE tenant = ?1"),
        vec![tenant.into(), job_id.into()],
    ))
    .await?;
    Ok(())
}

/// What one [`step`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Copied a batch (rows); more may follow.
    Copied(u64),
    /// The graph is ready.
    Ready,
    /// Nothing to do: no state row, or not materialising.
    Idle,
}

/// One batch of the materialisation of `tenant`, in the caller's write
/// transaction.
pub(crate) async fn step(tx: &mut dyn WriteTx, tenant: &str) -> Result<Step> {
    with_ag_path!(tx, step_in(tx, tenant).await)
}

async fn step_in(tx: &mut dyn WriteTx, tenant: &str) -> Result<Step> {
    let rows = tx
        .fetch_all(&stmt(
            format!("SELECT graph, phase, cursor_id FROM {STATE} WHERE tenant = ?1 AND state = 'materialising'"),
            vec![tenant.into()],
        ))
        .await?;
    let Some(r) = rows.rows.first() else {
        return Ok(Step::Idle);
    };
    let (graph, phase, cursor) = (
        text(r.first()).unwrap_or_default(),
        text(r.get(1)).unwrap_or_default(),
        text(r.get(2)).unwrap_or_default(),
    );
    let (table, column, kind) = match phase.as_str() {
        "nodes" => (NODES, "label", Kind::Node),
        "edges" => (EDGES, "type", Kind::Edge),
        _ => return Ok(Step::Idle),
    };
    let batch = tx
        .fetch_all(&stmt(
            format!("SELECT id, {column} FROM {table} WHERE tenant = ?1 AND id > ?2 ORDER BY id LIMIT ?3"),
            vec![tenant.into(), cursor.as_str().into(), BATCH.into()],
        ))
        .await?;
    let now = forgebase_core::now_rfc3339();
    let Some(last) = batch.rows.last().and_then(|r| text(r.first())) else {
        let (next, state) = if kind == Kind::Node {
            ("edges", "materialising")
        } else {
            ("done", "ready")
        };
        tx.execute(&stmt(
            format!("UPDATE {STATE} SET phase = ?2, state = ?3, cursor_id = '', updated_at = ?4 WHERE tenant = ?1"),
            vec![tenant.into(), next.into(), state.into(), now.into()],
        ))
        .await?;
        return Ok(if kind == Kind::Edge { Step::Ready } else { Step::Copied(0) });
    };
    let mut names: Vec<String> = batch.rows.iter().filter_map(|r| text(r.get(1))).collect();
    names.sort();
    names.dedup();
    let range = Range::Between(&cursor, &last);
    for name in &names {
        match kind {
            Kind::Node => copy_nodes(tx, &graph, tenant, name, range).await?,
            Kind::Edge => copy_edges(tx, &graph, tenant, name, range).await?,
        };
    }
    let n = batch.rows.len() as u64;
    tx.execute(&stmt(
        format!("UPDATE {STATE} SET cursor_id = ?2, done = done + ?3, updated_at = ?4 WHERE tenant = ?1"),
        vec![tenant.into(), last.into(), i64::try_from(n).unwrap_or(i64::MAX).into(), now.into()],
    ))
    .await?;
    Ok(Step::Copied(n))
}

/// Marks a failed materialisation (writes stop syncing; the next openCypher
/// request starts over).
pub(crate) async fn fail(tx: &mut dyn WriteTx, tenant: &str, error: &str) -> Result<()> {
    tx.execute(&stmt(
        format!("UPDATE {STATE} SET state = 'failed', error = ?2, updated_at = ?3 WHERE tenant = ?1"),
        vec![tenant.into(), error.into(), forgebase_core::now_rfc3339().into()],
    ))
    .await?;
    Ok(())
}

/// Marks `tenant`'s derived graph stale (after a move: the next openCypher
/// request rebuilds it). Returns whether there was one.
pub(crate) async fn mark_stale(tx: &mut dyn WriteTx, tenant: &str) -> Result<bool> {
    let done = tx
        .execute(&stmt(
            format!("UPDATE {STATE} SET state = 'stale', updated_at = ?2 WHERE tenant = ?1"),
            vec![tenant.into(), forgebase_core::now_rfc3339().into()],
        ))
        .await?;
    Ok(done.rows_affected > 0)
}

/// Drops `tenant`'s derived graph, its map rows and its state. Returns
/// whether a graph or state existed.
pub(crate) async fn drop_in(tx: &mut dyn WriteTx, tenant: &str) -> Result<bool> {
    let graph = graph_name(tenant);
    let mut existed = false;
    if graph_exists(tx, &graph).await? {
        let drop = stmt("SELECT ag_catalog.drop_graph(?1::text::name, true)::text", vec![graph.into()]);
        with_ag_path!(tx, tx.fetch_all(&drop).await)?;
        existed = true;
    }
    tx.execute(&stmt(format!("DELETE FROM {MAP} WHERE tenant = ?1"), vec![tenant.into()]))
        .await?;
    let st = tx
        .execute(&stmt(format!("DELETE FROM {STATE} WHERE tenant = ?1"), vec![tenant.into()]))
        .await?;
    Ok(existed || st.rows_affected > 0)
}

/// The state row of `tenant` inside a write transaction.
pub(crate) async fn current(tx: &mut dyn WriteTx, tenant: &str) -> Result<Option<State>> {
    state_tx(tx, tenant).await
}

// ---- visibility ---------------------------------------------------------------

/// The claims' `graph` object for a caller with `acc` on `tenant`'s graph
/// (see the module documentation), read with `b` (the routed reader).
pub(crate) async fn visibility(b: &dyn Backend, graph: &str, tenant: &str, acc: &GraphAccess) -> Result<Json> {
    Ok(json!({
        "v": side(b, graph, tenant, &acc.nodes, Kind::Node).await?,
        "e": side(b, graph, tenant, &acc.edges, Kind::Edge).await?,
    }))
}

async fn side(b: &dyn Backend, graph: &str, tenant: &str, rules: &Rules, kind: Kind) -> Result<Json> {
    if rules.is_unrestricted() {
        return Ok(json!({"all": true}));
    }
    let (whole, by_id): (Vec<&String>, Vec<&String>) = rules.allowed().iter().partition(|n| representable(n));
    // Label ids of the names allowed whole.
    let mut labels = Vec::new();
    if !whole.is_empty() {
        let mut params: Vec<Value> = vec![graph.into(), kind.age().into()];
        let list: Vec<String> = whole
            .iter()
            .map(|n| {
                params.push(n.as_str().into());
                format!("?{}::text::name", params.len())
            })
            .collect();
        let rows = b
            .fetch_all(&stmt(
                format!(
                    "SELECT l.id::bigint FROM ag_catalog.ag_label l JOIN ag_catalog.ag_graph g ON g.graphid = l.graph \
                     WHERE g.name = ?1::text::name AND l.kind::text = ?2 AND l.name IN ({})",
                    list.join(", ")
                ),
                params,
            ))
            .await?;
        labels = rows.rows.iter().filter_map(|r| int(r.first())).collect();
    }
    // Row ids: the filtered names' rows that pass, and the allowed names
    // kept in the catch-all table.
    let partial = Rules::partial(
        rules.kind(),
        by_id.into_iter().cloned().collect(),
        rules.filters().to_vec(),
    );
    let mut ids = Vec::new();
    let mut sb = SqlBuilder::new(b.dialect());
    let t = sb.bind(tenant);
    let (table, alias) = match kind {
        Kind::Node => (NODES, "n"),
        Kind::Edge => (EDGES, "e"),
    };
    if let Some(cond) = partial.compile(&mut sb, alias)? {
        let lim = sb.bind(i64::try_from(MAX_VISIBLE_IDS + 1).unwrap_or(i64::MAX));
        let rows = b
            .fetch_all(&Statement::new(
                format!(
                    "SELECT m.age_id::text FROM {table} {alias} JOIN {MAP} m ON m.tenant = {alias}.tenant \
                     AND m.kind = '{k}' AND m.id = {alias}.id WHERE {alias}.tenant = {t} AND {cond} LIMIT {lim}",
                    k = kind.code(),
                ),
                sb.into_params(),
            ))
            .await?;
        if rows.len() > MAX_VISIBLE_IDS {
            return Err(GraphError::RulesUnsupported(format!(
                "your graph rules select more than {MAX_VISIBLE_IDS} {} for openCypher; use the REST graph API",
                if kind == Kind::Node { "nodes" } else { "edges" }
            )));
        }
        ids = rows.rows.iter().filter_map(|r| text(r.first())).collect();
    }
    Ok(json!({"all": false, "labels": labels, "ids": ids}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_stable_and_bounded() {
        assert_eq!(graph_name(""), "fbg_shared");
        let a = graph_name("env1");
        assert_eq!(a.len(), 4 + 16);
        assert!(a.starts_with("fbg_") && a[4..].chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(a, graph_name("env1"));
        assert_ne!(a, graph_name("env2"));
    }

    #[test]
    fn labels_age_cannot_take_go_to_the_catch_all() {
        for ok in ["Person", "p", "KNOWS_2", &"a".repeat(63)] {
            assert!(representable(ok), "{ok}");
        }
        for bad in ["", "my-label", "_ag_label_vertex", "_fbnode", "9lives", "naïve", &"a".repeat(64), "a b"] {
            assert!(!representable(bad), "{bad}");
        }
        assert_eq!(age_label("my-label", Kind::Node), CATCH_NODE);
        assert_eq!(age_label("", Kind::Edge), CATCH_EDGE);
        assert_eq!(age_label("Doc", Kind::Node), "Doc");
    }
}
