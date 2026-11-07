//! The admin dashboard, embedded in the binary.
//!
//! `dashboard/dist` (the Vite build, `base: "/admin/"`) is compiled into the
//! binary by `rust-embed`. When the directory is missing at build time the
//! binary still builds and `/admin/` serves a short page explaining how to
//! build the dashboard.
//!
//! * `/admin` redirects to `/admin/`.
//! * Existing files are served with a content type, a strong `ETag` and
//!   `If-None-Match` support. Files under `assets/` (content-hashed by Vite)
//!   are cached for a year; everything else must revalidate.
//! * Unknown paths without a file extension are client-side routes and get
//!   `index.html`; unknown paths with an extension are `404 not_found`.

use axum::body::Body;
use axum::extract::Path;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use rust_embed::RustEmbed;

use crate::error::ApiError;

#[derive(RustEmbed)]
#[folder = "../../dashboard/dist"]
#[allow_missing = true]
struct Dashboard;

/// Whether a dashboard build was embedded at compile time.
pub fn dashboard_embedded() -> bool {
    Dashboard::get("index.html").is_some()
}

const MISSING_PAGE: &str = r#"<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>ForgeBase admin</title></head>
<body style="font-family: system-ui, sans-serif; max-width: 40rem; margin: 4rem auto; line-height: 1.5">
<h1>ForgeBase admin dashboard not built</h1>
<p>This binary was compiled without <code>dashboard/dist</code>. The API is running normally.</p>
<p>To include the dashboard, run <code>npm ci &amp;&amp; npm run build</code> in <code>dashboard/</code>,
then rebuild with <code>cargo build --release</code>.</p>
<p>Health: <a href="/health">/health</a> &middot; System info: <a href="/api/system/info">/api/system/info</a></p>
</body>
</html>
"#;

/// `GET /admin` → `/admin/`.
pub async fn redirect() -> Redirect {
    Redirect::permanent("/admin/")
}

/// `GET /admin/`.
pub async fn index(headers: HeaderMap) -> Response {
    serve("index.html", &headers)
}

/// `GET /admin/{*path}`.
pub async fn asset(Path(path): Path<String>, headers: HeaderMap) -> Response {
    serve(&path, &headers)
}

fn serve(path: &str, headers: &HeaderMap) -> Response {
    let path = path.trim_start_matches('/');
    if let Some(file) = Dashboard::get(path) {
        return file_response(path, file, headers);
    }
    let looks_like_file = path.rsplit('/').next().is_some_and(|last| last.contains('.'));
    if looks_like_file && path != "index.html" {
        return ApiError::not_found(format!("no admin asset at /admin/{path}")).into_response();
    }
    match Dashboard::get("index.html") {
        Some(file) => file_response("index.html", file, headers),
        None => (
            StatusCode::OK,
            [
                (CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8")),
                (CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            MISSING_PAGE,
        )
            .into_response(),
    }
}

fn file_response(path: &str, file: rust_embed::EmbeddedFile, headers: &HeaderMap) -> Response {
    let hash: String = file.metadata.sha256_hash().iter().map(|b| format!("{b:02x}")).collect();
    let etag = format!("\"{hash}\"");
    let cache = if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let etag_value = HeaderValue::from_str(&etag).unwrap_or_else(|_| HeaderValue::from_static("\"\""));
    let matches = headers
        .get(IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    if matches {
        return (
            StatusCode::NOT_MODIFIED,
            [(ETAG, etag_value), (CACHE_CONTROL, HeaderValue::from_static(cache))],
        )
            .into_response();
    }
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let textual = mime.type_() == "text" || matches!(mime.subtype().as_str(), "javascript" | "json" | "svg");
    let mime = if textual {
        format!("{mime}; charset=utf-8")
    } else {
        mime.to_string()
    };
    let content_type =
        HeaderValue::from_str(&mime).unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    (
        StatusCode::OK,
        [
            (CONTENT_TYPE, content_type),
            (ETAG, etag_value),
            (CACHE_CONTROL, HeaderValue::from_static(cache)),
        ],
        Body::from(file.data),
    )
        .into_response()
}
