//! What every HTTP SMS gateway shares: the typed error, the delivery
//! receipt, E.164 validation and the retry loop for rate limits and
//! transient failures.

use std::fmt;
use std::time::Duration;

use crate::social::{HttpClient, HttpRequest, HttpResponse};

/// Why a message was not delivered to the gateway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SmsError {
    /// The recipient is not an E.164 number, or the gateway refused it.
    InvalidRecipient(String),
    /// The gateway refused the credentials (an operator problem).
    Auth(String),
    /// The gateway throttled the request and the retries did not clear it.
    RateLimited {
        /// The gateway's `Retry-After`, in seconds, when it sent one.
        retry_after_secs: Option<u64>,
    },
    /// A 5xx answer or a connection problem that outlasted the retries.
    Transient(String),
    /// Any other refusal (bad sender id, blocked destination, opted out, ...).
    Rejected(String),
}

impl SmsError {
    /// Whether trying again later may succeed.
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::RateLimited { .. } | Self::Transient(_))
    }
}

impl fmt::Display for SmsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRecipient(m) => write!(f, "invalid recipient: {m}"),
            Self::Auth(m) => write!(f, "gateway refused the credentials: {m}"),
            Self::RateLimited {
                retry_after_secs: Some(s),
            } => write!(f, "gateway rate limit (retry after {s}s)"),
            Self::RateLimited { retry_after_secs: None } => f.write_str("gateway rate limit"),
            Self::Transient(m) => write!(f, "gateway unavailable: {m}"),
            Self::Rejected(m) => write!(f, "gateway rejected the message: {m}"),
        }
    }
}

impl std::error::Error for SmsError {}

/// A message the gateway accepted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SmsReceipt {
    /// The gateway's message id, when it returned one.
    pub id: Option<String>,
}

impl SmsReceipt {
    /// A receipt with the gateway's id.
    pub fn with_id(id: impl Into<String>) -> Self {
        Self { id: Some(id.into()) }
    }
}

/// Checks `to` is a strict E.164 number: `+`, then 8 to 15 digits, no
/// leading zero. (The phone flows normalise user input; providers re-check
/// what they put on the wire.)
pub fn check_e164(to: &str) -> Result<(), SmsError> {
    let ok = to
        .strip_prefix('+')
        .is_some_and(|d| (8..=15).contains(&d.len()) && d.bytes().all(|b| b.is_ascii_digit()) && !d.starts_with('0'));
    if ok {
        Ok(())
    } else {
        Err(SmsError::InvalidRecipient(
            "not an E.164 number (+ and 8-15 digits)".into(),
        ))
    }
}

/// How hard a provider tries before giving up.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Total attempts (first try included).
    pub attempts: u32,
    /// First backoff; doubles per attempt.
    pub base: Duration,
    /// Longest wait, also the cap on a gateway's `Retry-After`.
    pub max_wait: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: 3,
            base: Duration::from_millis(250),
            max_wait: Duration::from_secs(2),
        }
    }
}

/// `Retry-After` in whole seconds (the HTTP-date form is not used by SMS gateways).
pub fn retry_after(res: &HttpResponse) -> Option<u64> {
    res.header("retry-after").and_then(|v| v.trim().parse().ok())
}

/// The HTTP client plus the retry policy of one gateway.
#[derive(Debug, Clone)]
pub struct Gateway {
    http: std::sync::Arc<dyn HttpClient>,
    retry: RetryPolicy,
}

impl Gateway {
    /// A gateway using `http`.
    pub fn new(http: std::sync::Arc<dyn HttpClient>) -> Self {
        Self {
            http,
            retry: RetryPolicy::default(),
        }
    }

    /// Replaces the retry policy (tests use tiny delays).
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Sends the request `build` makes (rebuilt per attempt: signed requests
    /// carry a timestamp) and lets `classify` turn the answer into a receipt
    /// or an error. Rate limits and transient failures are retried, waiting
    /// for the gateway's `Retry-After` (capped) or the doubling backoff.
    pub async fn call(
        &self,
        build: impl Fn() -> HttpRequest,
        classify: impl Fn(&HttpResponse) -> Result<SmsReceipt, SmsError>,
    ) -> Result<SmsReceipt, SmsError> {
        let mut delay = self.retry.base;
        let mut last = SmsError::Transient("no attempt was made".into());
        for attempt in 0..self.retry.attempts.max(1) {
            let wait_hint;
            match self.http.send(build()).await {
                Ok(res) => match classify(&res) {
                    Ok(r) => return Ok(r),
                    Err(e) if e.is_retryable() => {
                        wait_hint = retry_after(&res).map(Duration::from_secs);
                        last = e;
                    }
                    Err(e) => return Err(e),
                },
                Err(e) => {
                    wait_hint = None;
                    last = SmsError::Transient(e.to_string());
                }
            }
            if attempt + 1 < self.retry.attempts {
                let wait = wait_hint.unwrap_or(delay).min(self.retry.max_wait);
                tokio::time::sleep(wait).await;
                delay = delay.saturating_mul(2);
            }
        }
        Err(last)
    }
}

/// The default HTTP status mapping: 2xx is success (the caller extracts the
/// id), 401/403 credentials, 429 rate limit, 5xx transient, other 4xx a refusal.
pub fn classify_status(res: &HttpResponse, detail: impl FnOnce() -> String) -> Result<(), SmsError> {
    match res.status {
        200..=299 => Ok(()),
        401 | 403 => Err(SmsError::Auth(detail())),
        429 => Err(SmsError::RateLimited {
            retry_after_secs: retry_after(res),
        }),
        500..=599 => Err(SmsError::Transient(format!("HTTP {}", res.status))),
        _ => Err(SmsError::Rejected(detail())),
    }
}

/// A short, log-safe excerpt of a JSON error body at `pointer`s (first hit).
pub fn json_detail(res: &HttpResponse, pointers: &[&str]) -> String {
    let status = res.status;
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&res.body) else {
        return format!("HTTP {status}");
    };
    for p in pointers {
        if let Some(s) = v.pointer(p).and_then(|x| x.as_str()) {
            let s: String = s.chars().filter(|c| !c.is_control()).take(200).collect();
            return format!("HTTP {status}: {s}");
        }
    }
    format!("HTTP {status}")
}

/// Text of the first `<tag>…</tag>` in an XML body (the SNS answers).
pub fn xml_tag(body: &[u8], tag: &str) -> Option<String> {
    let s = std::str::from_utf8(body).ok()?;
    let open = format!("<{tag}>");
    let start = s.find(&open)? + open.len();
    let end = s[start..].find(&format!("</{tag}>"))? + start;
    let t: String = s[start..end].chars().filter(|c| !c.is_control()).take(200).collect();
    Some(t)
}
