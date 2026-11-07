//! Request parsing helpers: query strings, JSON bodies, conditional headers.

use std::collections::HashMap;

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use forgebase_data::ListQuery;
use serde::de::DeserializeOwned;
use serde_json::Value as Json;

use crate::error::ApiError;

/// Longest accepted query string.
pub const MAX_QUERY_LEN: usize = 32 * 1024;

/// A decoded query string. Keys may repeat.
#[derive(Debug, Clone, Default)]
pub struct Query(Vec<(String, String)>);

impl<S: Send + Sync> FromRequestParts<S> for Query {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let raw = parts.uri.query().unwrap_or_default();
        if raw.len() > MAX_QUERY_LEN {
            return Err(ApiError::bad_request("query_too_long", "query string is too long"));
        }
        Ok(Query(
            url::form_urlencoded::parse(raw.as_bytes())
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect(),
        ))
    }
}

impl Query {
    /// The last value of `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.iter().rev().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// Comma-separated values of every `name` parameter, trimmed, empties dropped.
    pub fn list(&self, name: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|(k, _)| k == name)
            .flat_map(|(_, v)| v.split(','))
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// A `u32` parameter.
    pub fn u32(&self, name: &str) -> Result<Option<u32>, ApiError> {
        self.get(name)
            .map(|v| {
                v.parse::<u32>().map_err(|_| {
                    ApiError::bad_request("invalid_parameter", format!("{name} must be a positive integer"))
                })
            })
            .transpose()
    }

    /// A boolean parameter (`true`, `false`, `1`, `0`; a bare flag is true).
    pub fn bool(&self, name: &str) -> Result<bool, ApiError> {
        match self.get(name) {
            None => Ok(false),
            Some("" | "true" | "1") => Ok(true),
            Some("false" | "0") => Ok(false),
            Some(_) => Err(ApiError::bad_request(
                "invalid_parameter",
                format!("{name} must be true or false"),
            )),
        }
    }

    /// Filter parameters: every `p.<name>=<value>`. A value that is a JSON
    /// number, `true`, `false` or `null` is typed accordingly; anything else is
    /// a string. (`POST .../records/query` takes typed `params` in JSON.)
    pub fn filter_params(&self) -> HashMap<String, Json> {
        self.0
            .iter()
            .filter_map(|(k, v)| {
                let name = k.strip_prefix("p.")?;
                let typed = match serde_json::from_str::<Json>(v) {
                    Ok(j @ (Json::Null | Json::Bool(_) | Json::Number(_))) => j,
                    _ => Json::String(v.clone()),
                };
                Some((name.to_owned(), typed))
            })
            .collect()
    }

    /// The record-listing options of the query string: `filter`, `p.*`,
    /// `sort`, `fields`, `expand`, `search`, `page`, `per_page`, `cursor`,
    /// `skip_total`.
    pub fn list_query(&self) -> Result<ListQuery, ApiError> {
        let fields = self.list("fields");
        Ok(ListQuery {
            filter: self.get("filter").filter(|f| !f.trim().is_empty()).map(str::to_owned),
            params: self.filter_params(),
            sort: self.get("sort").filter(|f| !f.trim().is_empty()).map(str::to_owned),
            fields: (!fields.is_empty()).then_some(fields),
            expand: self.list("expand"),
            search: self.get("search").filter(|f| !f.trim().is_empty()).map(str::to_owned),
            page: self.u32("page")?,
            per_page: self.u32("per_page")?,
            cursor: self.get("cursor").filter(|c| !c.is_empty()).map(str::to_owned),
            skip_total: self.bool("skip_total")?,
        })
    }
}

/// A JSON body with ForgeBase error envelopes for every failure: `400
/// invalid_json`, `400 invalid_body` (wrong shape), `413 payload_too_large`,
/// `415 unsupported_media_type`. Accepts `application/json` and `+json`
/// types such as `application/merge-patch+json`.
#[derive(Debug)]
pub struct ApiJson<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for ApiJson<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(v)) => Ok(ApiJson(v)),
            Err(rej) => Err(json_rejection(&rej)),
        }
    }
}

fn json_rejection(rej: &JsonRejection) -> ApiError {
    let (status, code) = match rej {
        JsonRejection::MissingJsonContentType(_) => (StatusCode::UNSUPPORTED_MEDIA_TYPE, "unsupported_media_type"),
        JsonRejection::JsonSyntaxError(_) => (StatusCode::BAD_REQUEST, "invalid_json"),
        JsonRejection::JsonDataError(_) => (StatusCode::BAD_REQUEST, "invalid_body"),
        _ if rej.status() == StatusCode::PAYLOAD_TOO_LARGE => (StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large"),
        _ => (StatusCode::BAD_REQUEST, "invalid_body"),
    };
    let message = if status == StatusCode::PAYLOAD_TOO_LARGE {
        "request body is too large".to_owned()
    } else {
        rej.body_text()
    };
    ApiError::new(status, code, message)
}

/// `If-Match: "3"` (or `3`, or `W/"3"`) as the record version the caller expects.
pub fn if_match_version(headers: &HeaderMap) -> Result<Option<i64>, ApiError> {
    let Some(v) = headers.get(axum::http::header::IF_MATCH) else {
        return Ok(None);
    };
    let text = v.to_str().unwrap_or_default().trim();
    let text = text.strip_prefix("W/").unwrap_or(text).trim_matches('"');
    text.parse::<i64>().map(Some).map_err(|_| {
        ApiError::bad_request(
            "invalid_if_match",
            "If-Match must be a record version such as \"3\" (see the ETag of a record)",
        )
    })
}
