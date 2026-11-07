//! The `/api/v1` data API over [`forgebase_data::DataService`] (SPEC §16,
//! ADR-0001 §6).
//!
//! | Area | Routes |
//! |---|---|
//! | collections | `GET/POST /collections`, `GET/PUT/DELETE /collections/{c}`, `GET .../schema`, `GET .../history` |
//! | records | `GET/POST .../records`, `GET/PATCH/PUT/DELETE .../records/{id}`, `GET .../versions`, `POST .../records/query`, `GET .../records/count`, `POST .../aggregate` |
//! | documents | `POST /documents/{c}`, `GET/PATCH/PUT/DELETE /documents/{c}/{id}`, `GET .../versions` |
//! | indexes | `GET/POST /indexes`, `GET/DELETE /indexes/{name}`, `POST .../rebuild`, `GET .../stats`, `POST /indexes/analyze`, `GET /indexes/recommendations`, `POST .../records/explain` |
//! | slow queries | `GET/DELETE /slow-queries`, `PUT /slow-queries/threshold` |
//! | SQL editor | `POST /sql`, `/sql/batch`, `/sql/explain`, `GET /sql/tables[/{name}]` |
//! | import / export | `GET .../export`, `POST .../import` |
//!
//! Every handler reads the request's [`AccessContext`](forgebase_data::AccessContext)
//! (see [`crate::principal`]) and the data core authorises each call.

pub mod collections;
pub mod indexes;
pub mod jsonschema;
pub mod records;
pub mod schemas;
pub mod sql;
pub mod transfer;

use axum::Router;
use axum::routing::{get, post};
use tower_http::limit::RequestBodyLimitLayer;

use crate::error::ApiError;
use crate::{AppState, RouterOptions};

/// Result of a handler.
pub type ApiResult<T> = Result<T, ApiError>;

/// All `/api/v1` routes.
pub fn router(opts: &RouterOptions) -> Router<AppState> {
    // Everything with a JSON body has the small limit ...
    let json = Router::new()
        .route("/collections", get(collections::list).post(collections::create))
        .route(
            "/collections/{collection}",
            get(collections::get)
                .put(collections::update)
                .delete(collections::delete),
        )
        .route("/collections/{collection}/schema", get(collections::schema))
        .route("/collections/{collection}/history", get(collections::history))
        .route(
            "/collections/{collection}/records",
            get(records::list).post(records::create),
        )
        .route("/collections/{collection}/records/query", post(records::query))
        .route("/collections/{collection}/records/count", get(records::count))
        .route("/collections/{collection}/records/explain", post(records::explain))
        .route("/collections/{collection}/aggregate", post(records::aggregate))
        .route(
            "/collections/{collection}/records/{id}",
            get(records::get)
                .patch(records::patch)
                .put(records::put)
                .delete(records::delete),
        )
        .route(
            "/collections/{collection}/records/{id}/versions",
            get(records::versions),
        )
        .route("/documents/{collection}", post(records::doc_create))
        .route(
            "/documents/{collection}/{id}",
            get(records::doc_get)
                .patch(records::doc_patch)
                .put(records::doc_put)
                .delete(records::doc_delete),
        )
        .route("/documents/{collection}/{id}/versions", get(records::doc_versions))
        .route("/indexes", get(indexes::list).post(indexes::create))
        .route("/indexes/recommendations", get(indexes::recommendations))
        .route("/indexes/analyze", post(indexes::analyze))
        .route("/indexes/{name}", get(indexes::get).delete(indexes::drop))
        .route("/indexes/{name}/rebuild", post(indexes::rebuild))
        .route("/indexes/{name}/stats", get(indexes::stats))
        .route(
            "/slow-queries",
            get(indexes::slow_queries).delete(indexes::clear_slow_queries),
        )
        .route("/slow-queries/threshold", axum::routing::put(indexes::set_threshold))
        .route("/sql", post(sql::execute))
        .route("/sql/batch", post(sql::batch))
        .route("/sql/explain", post(sql::explain))
        .route("/sql/tables", get(sql::tables))
        .route("/sql/tables/{name}", get(sql::table))
        .layer(RequestBodyLimitLayer::new(to_usize(opts.max_body_bytes)));

    // ... imports stream and enforce their own, larger, limit.
    let streaming = Router::new()
        .route("/collections/{collection}/export", get(transfer::export))
        .route("/collections/{collection}/import", post(transfer::import));

    Router::new().merge(json).merge(streaming)
}

fn to_usize(n: u64) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}
