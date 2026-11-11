//! The HTTP transport: auth headers, 401 refresh, retries with backoff.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use reqwest::Method;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::auth::AuthState;
use crate::consistency::{COMMIT_HEADER, CONSISTENCY_HEADER, CommitTokens, Consistency, READ_AFTER_HEADER};
use crate::error::{ApiError, Error, Result, parse_retry_after};

/// Retry policy for transient failures.
///
/// Statuses in [`statuses`](Self::statuses) (default 429 and 503) are retried for every method that
/// is safe to repeat; 502 and 504 and network errors are retried for idempotent methods only
/// (`GET`, `HEAD`, `OPTIONS`, `PUT`, `DELETE`, plus calls the SDK knows carry an idempotency key).
/// A 429 whose code says a *quota* is exhausted is never retried.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Retries after the first attempt (default 3). `0` disables retrying.
    pub max_retries: u32,
    /// First backoff delay (default 250 ms); doubles each attempt, with full jitter.
    pub base_delay: Duration,
    /// Upper bound of one backoff delay (default 10 s).
    pub max_delay: Duration,
    /// Longest `Retry-After` the SDK sleeps for (default 60 s); longer asks fail at once.
    pub max_retry_after: Duration,
    /// Statuses retried for every method (default `[429, 503]`).
    pub statuses: Vec<u16>,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(10),
            max_retry_after: Duration::from_secs(60),
            statuses: vec![429, 503],
        }
    }
}

impl RetryPolicy {
    /// No retries.
    #[must_use]
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// Delay before retry number `attempt` (1-based): `Retry-After` when sent, else exponential backoff with full jitter.
    pub(crate) fn backoff(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(ra) = retry_after {
            return ra;
        }
        let exp = self
            .base_delay
            .saturating_mul(2u32.saturating_pow(attempt.saturating_sub(1)));
        let ceiling = exp.min(self.max_delay);
        ceiling.mul_f64(fastrand::f64())
    }
}

/// Request body.
#[derive(Debug, Clone, Default)]
pub(crate) enum Body {
    #[default]
    None,
    Json(Value),
    Bytes {
        data: Bytes,
        content_type: String,
    },
}

/// One request, cheap to clone so retries can resend it.
#[derive(Debug, Clone)]
pub(crate) struct Req {
    pub method: Method,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Body,
    pub headers: Vec<(String, String)>,
    /// Send no credentials (sign-in, refresh, public endpoints).
    pub anonymous: bool,
    /// Treat as safe to repeat even when the method is `POST` (the call carries an idempotency key).
    pub idempotent: bool,
    pub retry: Option<RetryPolicy>,
    /// Overrides the client's consistency for this call.
    pub consistency: Option<Consistency>,
}

impl Req {
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            query: Vec::new(),
            body: Body::None,
            headers: Vec::new(),
            anonymous: false,
            idempotent: false,
            retry: None,
            consistency: None,
        }
    }
    pub fn get(path: impl Into<String>) -> Self {
        Self::new(Method::GET, path)
    }
    pub fn post(path: impl Into<String>) -> Self {
        Self::new(Method::POST, path)
    }
    pub fn put(path: impl Into<String>) -> Self {
        Self::new(Method::PUT, path)
    }
    pub fn patch(path: impl Into<String>) -> Self {
        Self::new(Method::PATCH, path)
    }
    pub fn delete(path: impl Into<String>) -> Self {
        Self::new(Method::DELETE, path)
    }
    /// JSON body.
    pub fn json<T: Serialize + ?Sized>(mut self, body: &T) -> Result<Self> {
        self.body = Body::Json(
            serde_json::to_value(body).map_err(|e| Error::Config(format!("cannot encode request body: {e}")))?,
        );
        Ok(self)
    }
    /// A body the SDK built: top-level `null`s mean "not set" and are left out (the server rejects
    /// `null` for some optional fields).
    pub fn json_value(mut self, mut body: Value) -> Self {
        if let Value::Object(m) = &mut body {
            m.retain(|_, v| !v.is_null());
        }
        self.body = Body::Json(body);
        self
    }
    /// A body the caller built: sent exactly as given.
    pub fn json_raw(mut self, body: Value) -> Self {
        self.body = Body::Json(body);
        self
    }
    pub fn bytes(mut self, data: Bytes, content_type: impl Into<String>) -> Self {
        self.body = Body::Bytes {
            data,
            content_type: content_type.into(),
        };
        self
    }
    pub fn q(mut self, k: &str, v: impl ToString) -> Self {
        self.query.push((k.to_owned(), v.to_string()));
        self
    }
    pub fn q_opt<V: ToString>(self, k: &str, v: Option<V>) -> Self {
        match v {
            Some(v) => self.q(k, v),
            None => self,
        }
    }
    pub fn header(mut self, k: &str, v: impl Into<String>) -> Self {
        self.headers.push((k.to_owned(), v.into()));
        self
    }
    pub fn anonymous(mut self) -> Self {
        self.anonymous = true;
        self
    }
    pub fn idempotent(mut self) -> Self {
        self.idempotent = true;
        self
    }
}

/// Percent-encodes one path segment.
pub(crate) fn seg(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes())
        .collect::<String>()
        .replace('+', "%20")
}

/// Percent-encodes an object key: `/` is kept so `a/b.txt` stays a path.
pub(crate) fn key_path(s: &str) -> String {
    s.split('/').map(seg).collect::<Vec<_>>().join("/")
}

pub(crate) struct Transport {
    pub base: String,
    pub http: reqwest::Client,
    pub retry: RetryPolicy,
    pub auth: Arc<AuthState>,
    pub extra_headers: Vec<(String, String)>,
    /// Commit tokens seen in responses, sent back as `X-ForgeBase-Read-After`.
    pub tokens: Arc<CommitTokens>,
    /// Default consistency of every call.
    pub consistency: Consistency,
}

impl Transport {
    pub fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path)
    }

    /// Sends with retries; returns a 2xx response or a typed error.
    pub async fn send(&self, req: &Req) -> Result<reqwest::Response> {
        let cfg = req.retry.clone().unwrap_or_else(|| self.retry.clone());
        let idempotent = req.idempotent
            || matches!(
                req.method,
                Method::GET | Method::HEAD | Method::OPTIONS | Method::PUT | Method::DELETE
            );
        let url = self.url(&req.path);
        let mut refreshed = false;
        let mut attempt = 0u32;
        loop {
            let mut rb = self
                .http
                .request(req.method.clone(), &url)
                .header("Accept", "application/json");
            if !req.query.is_empty() {
                rb = rb.query(&req.query);
            }
            for (k, v) in &self.extra_headers {
                rb = rb.header(k, v);
            }
            let mut sent_bearer: Option<String> = None;
            if !req.anonymous {
                for (k, v) in self.credentials().await {
                    if k.eq_ignore_ascii_case("authorization") {
                        sent_bearer = Some(v.clone());
                    }
                    rb = rb.header(k, v);
                }
            }
            if let Some(read_after) = self.tokens.header() {
                rb = rb.header(READ_AFTER_HEADER, read_after);
            }
            if req.consistency.unwrap_or(self.consistency) == Consistency::Strong {
                rb = rb.header(CONSISTENCY_HEADER, "strong");
            }
            for (k, v) in &req.headers {
                rb = rb.header(k, v);
            }
            rb = match &req.body {
                Body::None => rb,
                Body::Json(v) => rb.header("Content-Type", "application/json").body(v.to_string()),
                Body::Bytes { data, content_type } => rb.header("Content-Type", content_type).body(data.clone()),
            };

            let res = match rb.send().await {
                Ok(r) => r,
                Err(e) => {
                    if attempt < cfg.max_retries && idempotent {
                        attempt += 1;
                        tokio::time::sleep(cfg.backoff(attempt, None)).await;
                        continue;
                    }
                    return Err(Error::Network(describe(&e)));
                }
            };
            // A failed request may still have committed (a batch, a partial import).
            if let Some(token) = res.headers().get(COMMIT_HEADER).and_then(|v| v.to_str().ok()) {
                self.tokens.note(token);
            }
            let status = res.status().as_u16();
            if res.status().is_success() {
                return Ok(res);
            }

            if status == 401 && !req.anonymous && !refreshed && self.on_unauthorized(sent_bearer.as_deref()).await {
                refreshed = true;
                continue;
            }

            let retry_after = parse_retry_after(res.headers().get("retry-after").and_then(|v| v.to_str().ok()));
            let request_id = res
                .headers()
                .get("x-request-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let body: Option<Value> = res.bytes().await.ok().and_then(|b| serde_json::from_slice(&b).ok());
            let err = ApiError::from_response(status, body, request_id, retry_after);

            let retriable = (cfg.statuses.contains(&status) || ((status == 502 || status == 504) && idempotent))
                && err.kind() != crate::error::ErrorKind::QuotaExceeded;
            if retriable && attempt < cfg.max_retries && retry_after.is_none_or(|ra| ra <= cfg.max_retry_after) {
                attempt += 1;
                tokio::time::sleep(cfg.backoff(attempt, retry_after)).await;
                continue;
            }
            return Err(Error::Api(Box::new(err)));
        }
    }

    /// JSON response decoded into `T`.
    pub async fn json<T: DeserializeOwned>(&self, req: Req) -> Result<T> {
        let res = self.send(&req).await?;
        let bytes = res.bytes().await.map_err(|e| Error::Network(describe(&e)))?;
        serde_json::from_slice(&bytes).map_err(|e| Error::InvalidResponse(format!("{} {}: {e}", req.method, req.path)))
    }

    /// JSON response as a [`Value`]; an empty body is `Value::Null`.
    pub async fn value(&self, req: Req) -> Result<Value> {
        let res = self.send(&req).await?;
        let bytes = res.bytes().await.map_err(|e| Error::Network(describe(&e)))?;
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|e| Error::InvalidResponse(format!("{} {}: {e}", req.method, req.path)))
    }

    /// Response body ignored (204 and friends).
    pub async fn unit(&self, req: Req) -> Result<()> {
        self.send(&req).await.map(drop)
    }

    /// Raw response bytes.
    pub async fn raw(&self, req: Req) -> Result<Bytes> {
        let res = self.send(&req).await?;
        res.bytes().await.map_err(|e| Error::Network(describe(&e)))
    }

    /// Response text.
    pub async fn text(&self, req: Req) -> Result<String> {
        let res = self.send(&req).await?;
        res.text().await.map_err(|e| Error::Network(describe(&e)))
    }
}

pub(crate) fn describe(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    let mut src = std::error::Error::source(e);
    while let Some(c) = src {
        s.push_str(": ");
        s.push_str(&c.to_string());
        src = c.source();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_segments() {
        assert_eq!(seg("a b/c"), "a%20b%2Fc");
        assert_eq!(key_path("dir/a b.txt"), "dir/a%20b.txt");
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        let p = RetryPolicy {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(350),
            ..RetryPolicy::default()
        };
        for _ in 0..50 {
            assert!(p.backoff(1, None) <= Duration::from_millis(100));
            assert!(p.backoff(2, None) <= Duration::from_millis(200));
            assert!(p.backoff(9, None) <= Duration::from_millis(350));
        }
        assert_eq!(p.backoff(1, Some(Duration::from_secs(3))), Duration::from_secs(3));
    }
}
