//! openCypher: `POST /graph/cypher` on two engines, and, on the derived
//! Apache AGE graph (ADR-0029 §1.5, FB-163), `POST /graph/cypher/drop` and
//! the `graph.materialise` job.
//!
//! * **Engines** (ADR-0034): where AGE is in use (PostgreSQL with the
//!   extension, `graph = "auto"` or `"on"`) the query runs on AGE as below;
//!   everywhere else (SQLite, Windows engines, servers without AGE, `graph =
//!   "off"`) the portable engine ([`crate::portable`]) compiles a read-only
//!   subset of openCypher to SQL over the canonical tables. The response's
//!   `engine` (`age` or `portable`) says which one answered. `drop` and the
//!   materialise job exist for AGE only (`400 cypher_unsupported`
//!   elsewhere). The rest of this list is the AGE engine.
//! * **Permission**: `engine:graph_cypher` ([`Action::GraphCypher`]), not
//!   implied by `engine:graph_read`; the caller's graph label and edge-type
//!   rules (asked with [`Action::GraphRead`], operation `list`) apply inside
//!   the query, traversals included (row-level security on the label
//!   tables, see [`crate::age`]).
//! * **Materialised on first use**: the first request of a tenant starts
//!   `graph.materialise` and answers `409 graph_materialising` with the job
//!   id; from then on every canonical write updates the derived graph in its
//!   own transaction. After a tenant move (its placement's set or move time
//!   changed) the graph is rebuilt on next use.
//! * **Read-only**: a `READ ONLY` transaction; non-privileged callers run as
//!   `forgebase_rls` with claims; `statement_timeout`; `search_path =
//!   ag_catalog, pg_catalog`; the tokenizer refuses write clauses first.
//! * **Tenant isolation is structural**: the graph name comes from
//!   `ctx.tenant`, never from the request, and each graph holds one
//!   tenant's data.
//! * **Replicas**: reads go to a replica when the request allows it
//!   (`graph.cypher`, ADR-0029 §2.5); a replica that has not replayed the
//!   graph yet falls back to the primary. The state check reads the primary.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use forgebase_data::{AccessContext, Action, ResourceOp};
use forgebase_db::topology::PlacementState;
use forgebase_db::{BoxFuture, DbError, ReadIntent, ReadVia, RouterError, RunAs, Statement, TxMode};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use utoipa::ToSchema;

use crate::age::{self, Step, tokenize};
use crate::error::GraphError;
use crate::service::GraphService;

type Result<T> = std::result::Result<T, GraphError>;

/// Job kind that builds a tenant's derived graph (payload `{"tenant": …}`);
/// reserved (`graph.` prefix): only the system enqueues it.
pub const MATERIALISE_JOB: &str = "graph.materialise";
/// Longest query, in bytes.
pub const MAX_QUERY_BYTES: usize = 64 * 1024;
/// Most rows a query returns.
pub const MAX_LIMIT: u32 = 1000;
/// Rows when the request names no limit.
pub const DEFAULT_LIMIT: u32 = 100;
/// Longest statement timeout a request may ask for, in milliseconds.
pub const MAX_TIMEOUT_MS: u32 = 30_000;
/// Statement timeout when the request names none.
pub const DEFAULT_TIMEOUT_MS: u32 = 10_000;
/// A materialisation without progress for this long is started again.
pub const STUCK_AFTER: Duration = Duration::from_secs(600);
/// The role non-privileged reads run as (FB-152).
const RLS_ROLE: &str = "forgebase_rls";

/// Enqueues `graph.materialise` jobs: the server's bridge to the jobs
/// service (`forgebase-jobs` builds on `forgebase-api`, which mounts this
/// crate, so the graph crate cannot call it directly). Without one the
/// service materialises in a background task of its own.
pub trait MaterialiseJobs: Send + Sync + fmt::Debug {
    /// Enqueues a [`MATERIALISE_JOB`] for `tenant`; returns the job id.
    fn enqueue<'a>(&'a self, tenant: &'a str) -> BoxFuture<'a, std::result::Result<String, String>>;
}

/// `POST /graph/cypher`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CypherQuery {
    /// One read-only openCypher query with a final `RETURN` (no `RETURN *`).
    pub query: String,
    /// Query parameters (`$name` in the query), a JSON object.
    #[schema(value_type = Option<Object>)]
    pub params: Option<Json>,
    /// Most rows to return, 1 to 1000 (default 100).
    pub limit: Option<u32>,
    /// Statement timeout in milliseconds, 1 to 30 000 (default 10 000).
    pub timeout_ms: Option<u32>,
}

/// The rows of an openCypher query.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct CypherResult {
    /// One name per `RETURN` item (its alias, else its text).
    pub columns: Vec<String>,
    /// The rows: one JSON value per column. Vertices come back as
    /// `{id, label, properties}`, edges as `{id, type, source, target,
    /// directed, properties}` and paths as `{nodes, edges}`, with ForgeBase
    /// ids.
    #[schema(value_type = Vec<Vec<Object>>)]
    pub rows: Vec<Vec<Json>>,
    /// More rows matched than `limit`.
    pub truncated: bool,
    /// The engine that answered: `age` (Apache AGE on the derived graph) or
    /// `portable` (SQL over the canonical tables, ADR-0034).
    pub engine: String,
}

/// A request's checked limits.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Checked {
    pub(crate) limit: u32,
    pub(crate) timeout_ms: u32,
}

/// Checks the parts of a request both engines share: query size, `limit`,
/// `timeout_ms`, `params`.
pub(crate) fn check_request(q: &CypherQuery) -> Result<Checked> {
    let query = q.query.trim();
    if query.is_empty() || query.len() > MAX_QUERY_BYTES {
        return Err(GraphError::invalid(
            "cypher_invalid",
            format!("query must be 1 to {MAX_QUERY_BYTES} bytes"),
        ));
    }
    let limit = match q.limit {
        None => DEFAULT_LIMIT,
        Some(l) if (1..=MAX_LIMIT).contains(&l) => l,
        Some(_) => {
            return Err(GraphError::invalid(
                "invalid_limit",
                format!("limit must be 1 to {MAX_LIMIT}"),
            ));
        }
    };
    let timeout_ms = match q.timeout_ms {
        None => DEFAULT_TIMEOUT_MS,
        Some(t) if (1..=MAX_TIMEOUT_MS).contains(&t) => t,
        Some(_) => {
            return Err(GraphError::invalid(
                "invalid_timeout",
                format!("timeout_ms must be 1 to {MAX_TIMEOUT_MS}"),
            ));
        }
    };
    match &q.params {
        None | Some(Json::Null | Json::Object(_)) => {}
        Some(_) => return Err(GraphError::invalid("cypher_invalid", "params must be a JSON object")),
    }
    Ok(Checked { limit, timeout_ms })
}

/// The state of a tenant's derived openCypher graph (in `GET /graph/stats`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CypherStatus {
    /// `none` (never used), `materialising`, `ready`, `stale` (rebuilt on
    /// next use) or `failed`.
    pub state: String,
    /// The AGE graph name.
    pub graph: String,
    /// The `graph.materialise` job, when one runs it.
    pub job_id: Option<String>,
    /// Nodes to copy (when materialisation started).
    pub nodes: i64,
    /// Edges to copy.
    pub edges: i64,
    /// Rows copied so far.
    pub done: i64,
    /// Why materialisation failed.
    pub error: Option<String>,
}

/// `POST /graph/cypher/drop`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct CypherDropped {
    /// Whether there was a derived graph to drop.
    pub dropped: bool,
}

/// What the `graph.materialise` job did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MaterialiseReport {
    /// The tenant.
    pub tenant: String,
    /// The AGE graph.
    pub graph: String,
    /// The state afterwards.
    pub state: String,
    /// Rows this run copied.
    pub copied: u64,
}

impl GraphService {
    /// Has `graph.materialise` jobs enqueued through `jobs` (the server's
    /// bridge to the jobs service). Without it the service materialises in
    /// a background task.
    #[must_use]
    pub fn with_jobs(self, jobs: Arc<dyn MaterialiseJobs>) -> Self {
        let _ = self.shared.jobs.set(jobs);
        self
    }

    /// Sets the most row ids a caller's filter rules may select for one
    /// openCypher read (default [`crate::age::MAX_VISIBLE_IDS`]); above it
    /// the read gets `403 cypher_rules_unsupported`.
    #[must_use]
    pub fn with_visible_id_cap(self, cap: usize) -> Self {
        self.shared.id_cap.store(cap, std::sync::atomic::Ordering::Relaxed);
        self
    }

    /// Runs one read-only openCypher query on the caller's tenant's graph:
    /// on the derived Apache AGE graph where AGE is in use, else on the
    /// portable engine (see the module documentation).
    pub async fn cypher(&self, ctx: &AccessContext, q: CypherQuery) -> Result<CypherResult> {
        let g = self.on(ctx).await?;
        if g.age_on() {
            g.cypher_here(ctx, q).await
        } else {
            g.cypher_portable_here(ctx, q).await
        }
    }

    /// The openCypher engine this server answers with: `age` or
    /// `portable` (ADR-0034).
    pub fn cypher_engine(&self) -> &'static str {
        if self.age_on() { "age" } else { crate::portable::ENGINE }
    }

    /// Drops the caller's tenant's derived graph (it is rebuilt on next use).
    pub async fn drop_cypher(&self, ctx: &AccessContext) -> Result<CypherDropped> {
        self.on(ctx).await?.drop_cypher_here(ctx).await
    }

    /// The `graph.materialise` job: copies the canonical graph of `tenant`
    /// into its derived graph, in batches, until it is ready. Resumes where
    /// a previous run stopped.
    pub async fn materialise(&self, tenant: &str) -> Result<MaterialiseReport> {
        self.on_tenant(tenant).await?.materialise_here(tenant).await
    }

    /// Marks `tenant`'s derived graph stale: writes stop updating it and the
    /// next openCypher request rebuilds it (a tenant move's switch, ADR-0029
    /// §3.8 step 4.7, calls it on the source). Returns whether there was one.
    pub async fn mark_cypher_stale(&self, tenant: &str) -> Result<bool> {
        let g = self.on_tenant(tenant).await?;
        if !g.age_on() {
            return Ok(false);
        }
        let mut tx = g.backend().begin(TxMode::Normal).await?;
        let r = age::mark_stale(&mut *tx, tenant).await;
        finish(tx, r).await
    }

    /// The placement the derived graph of `tenant` must be built for:
    /// `<set>@<moved_at>`. A move's flip changes it (ADR-0029 §3.8), and so
    /// does a move back; its `moving` and `frozen` states do not, so the
    /// graph keeps answering during a move. `''` without a placement.
    fn placed(&self, tenant: &str) -> String {
        self.router()
            .filter(|_| !tenant.is_empty())
            .and_then(|r| r.placement(tenant))
            .map(|p| format!("{}@{}", p.set_id, p.moved_at.unwrap_or_default()))
            .unwrap_or_default()
    }

    pub(crate) async fn cypher_status_in(&self, tenant: &str) -> Result<Option<CypherStatus>> {
        if !self.age_on() {
            return Ok(None);
        }
        let placed = self.placed(tenant);
        Ok(Some(match age::state(self.backend(), tenant).await? {
            None => CypherStatus {
                state: "none".into(),
                graph: age::graph_name(tenant),
                job_id: None,
                nodes: 0,
                edges: 0,
                done: 0,
                error: None,
            },
            Some(s) => CypherStatus {
                state: if s.placed == placed { s.state } else { "stale".into() },
                graph: s.graph,
                job_id: s.job_id,
                nodes: s.nodes,
                edges: s.edges,
                done: s.done,
                error: s.error,
            },
        }))
    }

    async fn cypher_here(&self, ctx: &AccessContext, q: CypherQuery) -> Result<CypherResult> {
        if !self.age_on() {
            return Err(GraphError::CypherUnsupported);
        }
        let tenant = self.tenant_of(ctx, Action::GraphCypher)?;
        self.data.authorize_action(ctx, Action::GraphCypher)?;
        let Checked {
            limit,
            timeout_ms: timeout,
        } = check_request(&q)?;
        let query = q.query.trim();
        let params = match q.params {
            Some(p @ Json::Object(_)) => Some(p.to_string()),
            _ => None,
        };
        let tag = format!("fb{}", &uuid::Uuid::new_v4().simple().to_string()[..16]);
        let checked = tokenize::check(query, &tag).map_err(|r| GraphError::invalid(r.code, r.message))?;
        let acc = self
            .access_in(ctx, &tenant, Action::GraphRead, ResourceOp::List)
            .await?;
        let graph = self.ready_graph(&tenant).await?;
        let reader = ReadVia::replica(self.backend(), "graph.cypher");
        let vis = if acc.is_unrestricted() {
            json!({"v": {"all": true}, "e": {"all": true}})
        } else {
            let cap = self.shared.id_cap.load(std::sync::atomic::Ordering::Relaxed);
            age::visibility(&reader, &graph, &tenant, &acc, cap).await?
        };
        let claims = json!({
            "v": 1, "principal": ctx.principal_key(), "tenant": tenant, "action": "graph_cypher", "graph": vis,
        })
        .to_string();
        let run_as = if ctx.is_privileged() {
            RunAs::session(claims)
        } else {
            RunAs::new(RLS_ROLE, claims)
        }
        .with_search_path("ag_catalog, pg_catalog")
        .with_setting("statement_timeout", format!("{timeout}ms"));
        let ncols = i64::try_from(checked.columns.len()).unwrap_or(i64::MAX);
        let stmt = Statement::new(
            "SELECT r FROM forgebase.graph_cypher(?1::text, ?2::text, ?3::text, ?4::text, ?5::bigint::integer, ?6::bigint::integer) AS r",
            vec![
                graph.into(),
                query.into(),
                params.into(),
                tag.into(),
                ncols.into(),
                (i64::from(limit) + 1).into(),
            ],
        );
        let intent = ReadIntent::replica("graph.cypher");
        let rows = match self.backend().fetch_routed_as(&stmt, &intent, &run_as).await {
            // A replica that has not replayed the graph (or a new label) yet.
            Err(e) if e.to_string().contains("does not exist") => self.backend().fetch_all_as(&stmt, &run_as).await,
            other => other,
        }
        .map_err(cypher_error)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in &rows.rows {
            let Some(forgebase_db::Value::Text(t)) = r.first() else {
                return Err(GraphError::Internal("openCypher row is not text".into()));
            };
            // A cell is SQL NULL where AGE returns no agtype at all (a
            // missing property): JSON null.
            let cells: Vec<Option<String>> =
                serde_json::from_str(t).map_err(|e| GraphError::Internal(e.to_string()))?;
            out.push(
                cells
                    .iter()
                    .map(|c| match c {
                        Some(c) => age::agtype::parse(c).map_err(GraphError::Internal),
                        None => Ok(Json::Null),
                    })
                    .collect::<Result<Vec<_>>>()?,
            );
        }
        let truncated = out.len() > limit as usize;
        out.truncate(limit as usize);
        Ok(CypherResult {
            columns: checked.columns,
            rows: out,
            truncated,
            engine: "age".to_owned(),
        })
    }

    /// The tenant's ready graph, or `409 graph_materialising` after starting
    /// (or resuming) its materialisation.
    async fn ready_graph(&self, tenant: &str) -> Result<String> {
        let placed = self.placed(tenant);
        if let Some(s) = age::state(self.backend(), tenant).await?
            && s.placed == placed
        {
            match s.state.as_str() {
                "ready" => return Ok(s.graph),
                "materialising" if !stuck(&s.updated_at) => {
                    return Err(GraphError::Materialising { job_id: s.job_id });
                }
                "materialising" => {
                    let job_id = self.launch(tenant).await?;
                    return Err(GraphError::Materialising { job_id });
                }
                _ => {}
            }
        }
        // A move in progress: no new derived graph until it is over.
        if let Some(p) = self.router().and_then(|r| r.placement(tenant))
            && p.state != PlacementState::Active
        {
            return Err(RouterError::TenantMoving(tenant.to_owned()).into());
        }
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let r = async {
            // Another request may have started it meanwhile (one writer).
            if let Some(s) = age::current(&mut *tx, tenant).await?
                && s.placed == placed
                && (s.state == "ready" || s.state == "materialising")
            {
                return Ok(Some(s));
            }
            age::start(&mut *tx, tenant, &placed, None).await?;
            Ok(None)
        }
        .await;
        match finish(tx, r).await? {
            Some(s) if s.state == "ready" => Ok(s.graph),
            Some(s) => Err(GraphError::Materialising { job_id: s.job_id }),
            None => {
                tracing::info!(tenant, graph = %age::graph_name(tenant), "materialising the openCypher graph");
                let job_id = self.launch(tenant).await?;
                Err(GraphError::Materialising { job_id })
            }
        }
    }

    /// Hands the materialisation of `tenant` to the jobs service, or to a
    /// background task without one.
    async fn launch(&self, tenant: &str) -> Result<Option<String>> {
        match self.shared.jobs.get() {
            Some(jobs) => {
                let id = jobs
                    .enqueue(tenant)
                    .await
                    .map_err(|e| GraphError::Internal(format!("cannot enqueue {MATERIALISE_JOB}: {e}")))?;
                let mut tx = self.backend().begin(TxMode::Normal).await?;
                let r = age::set_job(&mut *tx, tenant, &id).await;
                finish(tx, r).await?;
                Ok(Some(id))
            }
            None => {
                let svc = self.clone();
                let tenant = tenant.to_owned();
                tokio::spawn(async move {
                    if let Err(e) = svc.materialise_here(&tenant).await {
                        tracing::warn!(tenant, error = %e, "materialising the openCypher graph failed");
                    }
                });
                Ok(None)
            }
        }
    }

    pub(crate) async fn materialise_here(&self, tenant: &str) -> Result<MaterialiseReport> {
        if !self.age_on() {
            return Err(GraphError::CypherUnsupported);
        }
        let mut copied = 0u64;
        loop {
            let mut tx = self.backend().begin(TxMode::Normal).await?;
            match age::step(&mut *tx, tenant).await {
                Ok(step) => {
                    tx.commit().await?;
                    match step {
                        Step::Copied(n) => copied += n,
                        Step::Ready | Step::Idle => break,
                    }
                }
                Err(e) => {
                    let _ = tx.rollback().await;
                    let mut tx = self.backend().begin(TxMode::Normal).await?;
                    let r = age::fail(&mut *tx, tenant, &e.to_string()).await;
                    finish(tx, r).await?;
                    return Err(e);
                }
            }
        }
        let state = age::state(self.backend(), tenant).await?;
        Ok(MaterialiseReport {
            tenant: tenant.to_owned(),
            graph: age::graph_name(tenant),
            state: state.map_or_else(|| "none".to_owned(), |s| s.state),
            copied,
        })
    }

    async fn drop_cypher_here(&self, ctx: &AccessContext) -> Result<CypherDropped> {
        if !self.age_on() {
            return Err(GraphError::CypherUnsupported);
        }
        let tenant = self.tenant_of(ctx, Action::GraphCypher)?;
        self.data.authorize_action(ctx, Action::GraphCypher)?;
        let mut tx = self.backend().begin(TxMode::Normal).await?;
        let r = age::drop_in(&mut *tx, &tenant).await;
        let dropped = finish(tx, r).await?;
        Ok(CypherDropped { dropped })
    }
}

fn stuck(updated_at: &str) -> bool {
    let Ok(at) = time::OffsetDateTime::parse(updated_at, &time::format_description::well_known::Rfc3339) else {
        return true;
    };
    let age = time::OffsetDateTime::now_utc() - at;
    age > time::Duration::try_from(STUCK_AFTER).unwrap_or(time::Duration::MAX)
}

/// Engine errors of a query, as the caller's errors where they are.
pub(crate) fn cypher_error(e: DbError) -> GraphError {
    match &e {
        DbError::ReadOnly(m) => {
            GraphError::invalid("cypher_read_only", format!("openCypher queries are read-only: {m}"))
        }
        DbError::Statement(m) => GraphError::invalid("cypher_error", m.clone()),
        _ if e.to_string().contains("statement timeout") => {
            GraphError::invalid("cypher_timeout", "the query ran longer than timeout_ms")
        }
        _ => GraphError::Db(e),
    }
}

async fn finish<T>(tx: Box<dyn forgebase_db::WriteTx>, result: Result<T>) -> Result<T> {
    match result {
        Ok(v) => {
            tx.commit().await?;
            Ok(v)
        }
        Err(e) => {
            if let Err(rb) = tx.rollback().await {
                tracing::warn!(error = %rb, "rollback failed");
            }
            Err(e)
        }
    }
}
