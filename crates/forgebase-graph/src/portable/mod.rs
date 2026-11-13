//! The portable openCypher engine (ADR-0034): `POST /graph/cypher` on
//! SQLite and on PostgreSQL without Apache AGE (Windows engines, hosted
//! servers, `graph = "off"`, or `auto` without the extension).
//!
//! The query is parsed by [`forgebase_data::query::cypher`] (a tokenizer
//! and a recursive-descent parser of a read-only subset) and compiled to
//! one SQL statement over the canonical graph tables ([`compile`]), so
//! nothing is materialised and every write is visible at once. Same
//! endpoint, request, permission (`engine:graph_cypher`), label and
//! edge-type rules, tenant isolation and response shape as the AGE engine;
//! the response's `engine` says which one answered.
//!
//! Limits: the parser's ([`forgebase_data::query::cypher`]: 10 hops, 32
//! patterns, nesting, 100 columns), at most [`MAX_WALK_ROWS`] paths per
//! variable-length relationship (`cypher_too_complex`, checked before the
//! query runs), `limit` rows (≤ 1000) and `timeout_ms` (PostgreSQL:
//! `statement_timeout`; SQLite: the request stops waiting).

pub(crate) mod compile;

use std::time::Duration;

use forgebase_data::query::cypher::{self, CypherError};
use forgebase_data::{AccessContext, Action, ResourceOp};
use forgebase_db::{ReadIntent, RunAs, Value};
use serde_json::{Map, Value as Json, json};

pub use compile::MAX_WALK_ROWS;

use crate::cypher::{CypherQuery, CypherResult, cypher_error};
use crate::error::GraphError;
use crate::service::GraphService;

type Result<T> = std::result::Result<T, GraphError>;

/// The engine name the response reports.
pub const ENGINE: &str = "portable";

fn refused(e: CypherError) -> GraphError {
    GraphError::invalid(e.code, e.message)
}

impl GraphService {
    /// Runs `q` on the portable engine (ADR-0034) whatever the server's
    /// openCypher engine is: what `POST /graph/cypher` does where Apache
    /// AGE is not in use, and the reference the differential tests compare
    /// AGE with.
    pub async fn cypher_portable(&self, ctx: &AccessContext, q: CypherQuery) -> Result<CypherResult> {
        self.on(ctx).await?.cypher_portable_here(ctx, q).await
    }

    pub(crate) async fn cypher_portable_here(&self, ctx: &AccessContext, q: CypherQuery) -> Result<CypherResult> {
        let tenant = self.tenant_of(ctx, Action::GraphCypher)?;
        self.data.authorize_action(ctx, Action::GraphCypher)?;
        let checked = crate::cypher::check_request(&q)?;
        let params = match &q.params {
            Some(Json::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        let query = q.query.trim();
        let ast = cypher::parse(query).map_err(refused)?;
        let acc = self
            .access_in(ctx, &tenant, Action::GraphRead, ResourceOp::List)
            .await?;
        let compiled = compile::compile(
            self.backend().dialect(),
            query,
            &ast,
            &params,
            &acc,
            &tenant,
            checked.limit,
        )
        .map_err(refused)?;
        let claims = json!({
            "v": 1, "principal": ctx.principal_key(), "tenant": tenant, "action": "graph_cypher",
        })
        .to_string();
        // The rules are in the SQL (as in the REST API): the server's own
        // role reads, with the request's statement timeout.
        let run_as = RunAs::session(claims).with_setting("statement_timeout", format!("{}ms", checked.timeout_ms));
        let intent = ReadIntent::replica("graph.cypher");
        let backend = self.backend();
        let run = async {
            if let Some(pre) = &compiled.preflight {
                let counts = backend
                    .fetch_routed_as(pre, &intent, &run_as)
                    .await
                    .map_err(cypher_error)?;
                let over = counts
                    .rows
                    .first()
                    .is_some_and(|r| r.iter().any(|v| v.as_i64().unwrap_or(0) > MAX_WALK_ROWS as i64));
                if over {
                    return Err(GraphError::invalid(
                        "cypher_too_complex",
                        format!(
                            "a variable-length relationship matches more than {MAX_WALK_ROWS} paths: \
                             constrain its start node or lower its maximum length"
                        ),
                    ));
                }
            }
            backend
                .fetch_routed_as(&compiled.main, &intent, &run_as)
                .await
                .map_err(cypher_error)
        };
        let rows = match tokio::time::timeout(Duration::from_millis(u64::from(checked.timeout_ms)), run).await {
            Ok(r) => r?,
            Err(_) => {
                return Err(GraphError::invalid(
                    "cypher_timeout",
                    "the query ran longer than timeout_ms",
                ));
            }
        };
        let n = compiled.columns.len();
        let mut out = Vec::with_capacity(rows.rows.len());
        for r in &rows.rows {
            let mut cells = Vec::with_capacity(n);
            for (i, v) in r.iter().take(n).enumerate() {
                let cell = match v {
                    Value::Null => Json::Null,
                    Value::Text(t) => serde_json::from_str(t)
                        .map_err(|e| GraphError::Internal(format!("openCypher value is not JSON: {e}")))?,
                    Value::Integer(i) => Json::from(*i),
                    Value::Real(f) => Json::from(*f),
                    other => return Err(GraphError::Internal(format!("unexpected openCypher value {other:?}"))),
                };
                cells.push(match cell {
                    Json::Number(n) if compiled.floats.get(i) == Some(&true) && !n.is_f64() => {
                        n.as_f64().map_or(Json::Number(n), Json::from)
                    }
                    other => other,
                });
            }
            out.push(cells);
        }
        let limit = checked.limit as usize;
        let truncated = compiled.detect_truncation && out.len() > limit;
        out.truncate(limit);
        Ok(CypherResult {
            columns: compiled.columns,
            rows: out,
            truncated,
            engine: ENGINE.to_owned(),
        })
    }
}
