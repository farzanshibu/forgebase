//! REST routes under `/api/v1/graph` and the OpenAPI fragment.
//!
//! The router is self-contained: it reads the request's
//! [`AccessContext`] from the request extensions (set by the auth layer; absent
//! means anonymous, which the authorizer denies), enforces its own body limit
//! and writes errors in the standard envelope
//! `{"error":{"code","message","request_id"}}`.

use axum::Json;
use axum::Router;
use axum::extract::{FromRequest, FromRequestParts, Path, Request, State, rejection::JsonRejection};
use axum::http::{StatusCode, request::Parts};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use forgebase_core::{ErrorCode, ErrorKind};
use forgebase_data::AccessContext;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tower_http::limit::RequestBodyLimitLayer;
use utoipa::{OpenApi, ToSchema};

use crate::error::GraphError;
use crate::model::*;
use crate::service::GraphService;

/// Largest JSON request body.
const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Longest accepted query string.
const MAX_QUERY_LEN: usize = 8 * 1024;

type ApiResult = Result<Response, HttpError>;

// ---- errors ----------------------------------------------------------------

/// An error response. The request id is filled in by [`with_request_id`].
#[derive(Debug, Clone)]
struct HttpError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl HttpError {
    fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code,
            message: message.into(),
        }
    }

    fn body(&self, request_id: &str) -> Response {
        let body = json!({"error": {"code": self.code, "message": self.message, "request_id": request_id}});
        (self.status, Json(body)).into_response()
    }
}

impl From<GraphError> for HttpError {
    fn from(e: GraphError) -> Self {
        let status = match e.kind() {
            ErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
            ErrorKind::Unauthenticated => StatusCode::UNAUTHORIZED,
            ErrorKind::Forbidden => StatusCode::FORBIDDEN,
            ErrorKind::NotFound => StatusCode::NOT_FOUND,
            ErrorKind::Conflict => StatusCode::CONFLICT,
            ErrorKind::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorKind::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = if e.kind().is_client_visible() {
            e.to_string()
        } else {
            tracing::error!(code = e.code(), error = %e, "graph request failed");
            "internal server error".to_owned()
        };
        Self {
            status,
            code: e.code(),
            message,
        }
    }
}

impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let mut res = self.body("");
        res.extensions_mut().insert(self);
        res
    }
}

/// Puts the request id (set by the server's outermost layer) into the error
/// body of this router's responses.
async fn with_request_id(req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let mut res = next.run(req).await;
    match res.extensions_mut().remove::<HttpError>() {
        Some(err) => err.body(&id),
        None => res,
    }
}

// ---- extractors ------------------------------------------------------------

/// The request's [`AccessContext`]; anonymous when no layer stored one.
struct Access(AccessContext);

impl<S: Send + Sync> FromRequestParts<S> for Access {
    type Rejection = HttpError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, HttpError> {
        let mut ctx = parts
            .extensions
            .get::<AccessContext>()
            .cloned()
            .unwrap_or_else(AccessContext::anonymous);
        if ctx.request_id.is_none() {
            ctx.request_id = parts
                .headers
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
        }
        Ok(Access(ctx))
    }
}

/// A JSON body with graph-style errors.
struct Body<T>(T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for Body<T> {
    type Rejection = HttpError;

    async fn from_request(req: Request, state: &S) -> Result<Self, HttpError> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(v)) => Ok(Body(v)),
            Err(JsonRejection::MissingJsonContentType(_)) => Err(HttpError {
                status: StatusCode::UNSUPPORTED_MEDIA_TYPE,
                code: "unsupported_media_type",
                message: "send the body as application/json".into(),
            }),
            Err(JsonRejection::BytesRejection(e)) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => Err(HttpError {
                status: StatusCode::PAYLOAD_TOO_LARGE,
                code: "payload_too_large",
                message: "request body is too large".into(),
            }),
            Err(e) => Err(HttpError::bad_request("invalid_body", e.body_text())),
        }
    }
}

/// Decoded query string.
struct Query(Vec<(String, String)>);

impl<S: Send + Sync> FromRequestParts<S> for Query {
    type Rejection = HttpError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, HttpError> {
        let raw = parts.uri.query().unwrap_or_default();
        if raw.len() > MAX_QUERY_LEN {
            return Err(HttpError::bad_request("query_too_long", "query string is too long"));
        }
        Ok(Query(
            url::form_urlencoded::parse(raw.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect(),
        ))
    }
}

impl Query {
    fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    fn u32(&self, name: &str) -> Result<Option<u32>, HttpError> {
        self.get(name)
            .map(|v| {
                v.parse().map_err(|_| {
                    HttpError::bad_request("invalid_parameter", format!("{name} must be a non-negative integer"))
                })
            })
            .transpose()
    }

    fn list(&self, name: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|(k, _)| k == name)
            .flat_map(|(_, v)| v.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }

    fn direction(&self) -> Result<Direction, HttpError> {
        match self.get("direction") {
            None | Some("out") => Ok(Direction::Out),
            Some("in") => Ok(Direction::In),
            Some("both") => Ok(Direction::Both),
            Some(_) => Err(HttpError::bad_request(
                "invalid_parameter",
                "direction must be out, in or both",
            )),
        }
    }
}

fn ok<T: Serialize>(v: T) -> ApiResult {
    Ok(Json(v).into_response())
}

fn created<T: Serialize>(v: T) -> ApiResult {
    Ok((StatusCode::CREATED, Json(v)).into_response())
}

// ---- schemas of paged responses (the generic Page<T> cannot be a schema) ----

/// A page of nodes.
#[derive(Serialize, ToSchema)]
#[allow(dead_code)]
pub struct NodePage {
    items: Vec<Node>,
    /// Pass as `after` for the next page; absent on the last page.
    next_after: Option<String>,
}

/// A page of edges.
#[derive(Serialize, ToSchema)]
#[allow(dead_code)]
pub struct EdgePage {
    items: Vec<Edge>,
    /// Pass as `after` for the next page; absent on the last page.
    next_after: Option<String>,
}

/// A page of neighbours.
#[derive(Serialize, ToSchema)]
#[allow(dead_code)]
pub struct NeighborPage {
    items: Vec<Neighbor>,
    /// Pass as `offset` for the next page; absent on the last page.
    next_after: Option<String>,
}

/// Result of deleting a node.
#[derive(Serialize, ToSchema)]
#[allow(dead_code)]
pub struct NodeDeleted {
    /// Deleted node id.
    id: String,
    /// Edges that touched the node and were deleted with it.
    deleted_edges: u64,
}

// ---- handlers --------------------------------------------------------------

/// Creates a node. The id is generated when absent.
#[utoipa::path(
    post, path = "/api/v1/graph/nodes", tag = "graph",
    request_body = NewNode,
    responses(
        (status = 201, description = "Created", body = Node),
        (status = 409, description = "A node with this id exists"),
    )
)]
async fn create_node(State(g): State<GraphService>, Access(ctx): Access, Body(b): Body<NewNode>) -> ApiResult {
    created(g.create_node(&ctx, b).await?)
}

/// Lists nodes by id, optionally of one label. Parameters: `label`, `after`
/// (an id), `limit` (1 to 1000, default 100).
#[utoipa::path(
    get, path = "/api/v1/graph/nodes", tag = "graph",
    responses((status = 200, description = "A page of nodes", body = NodePage))
)]
async fn list_nodes(State(g): State<GraphService>, Access(ctx): Access, q: Query) -> ApiResult {
    ok(g.list_nodes(&ctx, q.get("label"), q.get("after"), q.u32("limit")?)
        .await?)
}

/// One node.
#[utoipa::path(
    get, path = "/api/v1/graph/nodes/{id}", tag = "graph",
    responses((status = 200, description = "The node", body = Node))
)]
async fn get_node(State(g): State<GraphService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(g.get_node(&ctx, &id).await?)
}

/// Replaces a node's properties (and label when given).
#[utoipa::path(
    put, path = "/api/v1/graph/nodes/{id}", tag = "graph",
    request_body = NodeChange,
    responses((status = 200, description = "The updated node", body = Node))
)]
async fn put_node(
    State(g): State<GraphService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    Body(b): Body<NodeChange>,
) -> ApiResult {
    ok(g.change_node(&ctx, &id, b, false).await?)
}

/// Merge-patches a node's properties (RFC 7396; `null` removes a key).
#[utoipa::path(
    patch, path = "/api/v1/graph/nodes/{id}", tag = "graph",
    request_body = NodeChange,
    responses((status = 200, description = "The updated node", body = Node))
)]
async fn patch_node(
    State(g): State<GraphService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    Body(b): Body<NodeChange>,
) -> ApiResult {
    ok(g.change_node(&ctx, &id, b, true).await?)
}

/// Deletes a node and every edge that touches it.
#[utoipa::path(
    delete, path = "/api/v1/graph/nodes/{id}", tag = "graph",
    responses((status = 200, description = "Deleted", body = NodeDeleted))
)]
async fn delete_node(State(g): State<GraphService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    let deleted_edges = g.delete_node(&ctx, &id).await?;
    ok(json!({"id": id, "deleted_edges": deleted_edges}))
}

/// Nodes one hop away, with the connecting edge. Parameters: `direction`
/// (`out` default, `in`, `both`), `type` (comma-separated edge types), `limit`
/// (1 to 1000, default 100), `offset`.
#[utoipa::path(
    get, path = "/api/v1/graph/nodes/{id}/neighbors", tag = "graph",
    responses((status = 200, description = "A page of neighbours", body = NeighborPage))
)]
async fn neighbors(State(g): State<GraphService>, Access(ctx): Access, Path(id): Path<String>, q: Query) -> ApiResult {
    let query = NeighborQuery {
        direction: q.direction()?,
        edge_types: q.list("type"),
        limit: q.u32("limit")?,
        offset: q.u32("offset")?,
    };
    ok(g.neighbors(&ctx, &id, query).await?)
}

/// Creates an edge between two existing nodes.
#[utoipa::path(
    post, path = "/api/v1/graph/edges", tag = "graph",
    request_body = NewEdge,
    responses(
        (status = 201, description = "Created", body = Edge),
        (status = 409, description = "An edge with this id exists"),
    )
)]
async fn create_edge(State(g): State<GraphService>, Access(ctx): Access, Body(b): Body<NewEdge>) -> ApiResult {
    created(g.create_edge(&ctx, b).await?)
}

/// Lists edges by id. Parameters: `type`, `source`, `target`, `after` (an id),
/// `limit` (1 to 1000, default 100).
#[utoipa::path(
    get, path = "/api/v1/graph/edges", tag = "graph",
    responses((status = 200, description = "A page of edges", body = EdgePage))
)]
async fn list_edges(State(g): State<GraphService>, Access(ctx): Access, q: Query) -> ApiResult {
    ok(g.list_edges(
        &ctx,
        q.get("type"),
        q.get("source"),
        q.get("target"),
        q.get("after"),
        q.u32("limit")?,
    )
    .await?)
}

/// One edge.
#[utoipa::path(
    get, path = "/api/v1/graph/edges/{id}", tag = "graph",
    responses((status = 200, description = "The edge", body = Edge))
)]
async fn get_edge(State(g): State<GraphService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    ok(g.get_edge(&ctx, &id).await?)
}

/// Merge-patches an edge's properties (type and endpoints cannot change).
#[utoipa::path(
    patch, path = "/api/v1/graph/edges/{id}", tag = "graph",
    request_body = EdgeChange,
    responses((status = 200, description = "The updated edge", body = Edge))
)]
async fn patch_edge(
    State(g): State<GraphService>,
    Access(ctx): Access,
    Path(id): Path<String>,
    Body(b): Body<EdgeChange>,
) -> ApiResult {
    ok(g.change_edge(&ctx, &id, b).await?)
}

/// Deletes an edge.
#[utoipa::path(
    delete, path = "/api/v1/graph/edges/{id}", tag = "graph",
    responses((status = 204, description = "Deleted"))
)]
async fn delete_edge(State(g): State<GraphService>, Access(ctx): Access, Path(id): Path<String>) -> ApiResult {
    g.delete_edge(&ctx, &id).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Multi-hop traversal (recursive CTE): the nodes reachable from `start`
/// within `max_depth` hops (at most 10) with their fewest-hops depth, and the
/// edges between them. Cycles are safe.
#[utoipa::path(
    post, path = "/api/v1/graph/traverse", tag = "graph",
    request_body = TraverseQuery,
    responses((status = 200, description = "Reached nodes and edges", body = TraverseResult))
)]
async fn traverse(State(g): State<GraphService>, Access(ctx): Access, Body(b): Body<TraverseQuery>) -> ApiResult {
    ok(g.traverse(&ctx, b).await?)
}

/// Shortest path (fewest hops, breadth-first) between two nodes.
#[utoipa::path(
    post, path = "/api/v1/graph/shortest-path", tag = "graph",
    request_body = PathQuery,
    responses((status = 200, description = "The path, or `found: false`", body = PathResult))
)]
async fn shortest_path(State(g): State<GraphService>, Access(ctx): Access, Body(b): Body<PathQuery>) -> ApiResult {
    ok(g.shortest_path(&ctx, b).await?)
}

/// Node and edge counts.
#[utoipa::path(
    get, path = "/api/v1/graph/stats", tag = "graph",
    responses((status = 200, description = "Counts", body = GraphStats))
)]
async fn stats(State(g): State<GraphService>, Access(ctx): Access) -> ApiResult {
    ok(g.stats(&ctx).await?)
}

/// The graph routes, to be nested at `/api/v1/graph`.
pub fn router(service: GraphService) -> Router {
    Router::new()
        .route("/nodes", get(list_nodes).post(create_node))
        .route(
            "/nodes/{id}",
            get(get_node).put(put_node).patch(patch_node).delete(delete_node),
        )
        .route("/nodes/{id}/neighbors", get(neighbors))
        .route("/edges", get(list_edges).post(create_edge))
        .route("/edges/{id}", get(get_edge).patch(patch_edge).delete(delete_edge))
        .route("/traverse", post(traverse))
        .route("/shortest-path", post(shortest_path))
        .route("/stats", get(stats))
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(middleware::from_fn(with_request_id))
        .with_state(service)
}

/// OpenAPI paths and schemas of the graph API, merged into the server's
/// document by `forgebase-api`.
#[derive(OpenApi)]
#[openapi(
    paths(
        create_node, list_nodes, get_node, put_node, patch_node, delete_node, neighbors,
        create_edge, list_edges, get_edge, patch_edge, delete_edge,
        traverse, shortest_path, stats,
    ),
    components(schemas(
        Node, Edge, NewNode, NodeChange, NewEdge, EdgeChange, Direction, NodeFilter, TraverseQuery,
        TraversedNode, TraverseResult, PathQuery, PathResult, Neighbor, GraphStats, TypeCount,
        NodePage, EdgePage, NeighborPage, NodeDeleted,
    )),
    tags((name = "graph", description = "Graph: nodes, typed edges, neighbours, traversal and shortest path")),
)]
struct GraphApiDoc;

/// The OpenAPI fragment for `/api/v1/graph/*`.
pub fn openapi() -> utoipa::openapi::OpenApi {
    GraphApiDoc::openapi()
}
