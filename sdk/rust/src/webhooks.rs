//! Webhook verification for push subscriptions (ADR-0022 §13, ADR-0031).
//!
//! The server POSTs `{subscription, delivery_attempt, message}` as JSON with
//!
//! ```text
//! X-Forgebase-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256(secret, "<t>.<raw body>")>
//! ```
//!
//! The key is the UTF-8 text of the subscription's secret (`whsec_...`, prefix included). While a
//! secret is being rotated the header may carry several `v1=` values, one per secret that is still
//! valid: a request is accepted when any of them matches. Signatures are compared in constant
//! time, and every candidate is compared whatever matched first.
//!
//! Pass the **raw** body bytes exactly as received, never a parsed and re-serialised value.
//!
//! ```
//! use std::time::Duration;
//! use forgebase_sdk::webhooks::{self, DEFAULT_TOLERANCE};
//!
//! # fn run() -> Result<(), webhooks::WebhookError> {
//! let secret = "whsec_4f1c9d2a7b";
//! let body = br#"{"subscription":"orders-push","delivery_attempt":1,"message":{"id":"m1","topic":"orders","data":{"order":42}}}"#;
//! let header = webhooks::sign(secret, body, webhooks::now_secs());
//! // In a handler: `header` is the value of `X-Forgebase-Signature`.
//! let event = webhooks::construct_event::<serde_json::Value>(secret, Some(&header), body, DEFAULT_TOLERANCE)?;
//! assert_eq!(event.subscription, "orders-push");
//! # Ok(()) }
//! # run().unwrap();
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;

/// Header the server signs push requests with (HTTP header names are case-insensitive).
pub const SIGNATURE_HEADER: &str = "X-Forgebase-Signature";

/// Default accepted clock distance between the signed timestamp and now: five minutes.
pub const DEFAULT_TOLERANCE: Duration = Duration::from_secs(300);

/// Why a request failed verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum WebhookFailure {
    /// No `X-Forgebase-Signature` header (or an empty one).
    MissingHeader,
    /// The header is not `t=<secs>,v1=<hex>[,v1=<hex>...]`.
    MalformedHeader,
    /// The signed timestamp is further from now than the tolerance (a replay, or a wrong clock).
    TimestampOutOfTolerance,
    /// No signature matches the payload and secret.
    SignatureMismatch,
    /// The signature is valid but the body is not the push envelope.
    InvalidPayload,
}

impl WebhookFailure {
    /// A stable snake_case reason: `missing_header`, `malformed_header`, `timestamp_out_of_tolerance`,
    /// `signature_mismatch` or `invalid_payload`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MissingHeader => "missing_header",
            Self::MalformedHeader => "malformed_header",
            Self::TimestampOutOfTolerance => "timestamp_out_of_tolerance",
            Self::SignatureMismatch => "signature_mismatch",
            Self::InvalidPayload => "invalid_payload",
        }
    }
}

/// A request that must not be trusted. Answer it with a 4xx so the server does not count it as delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookError {
    /// Why verification failed.
    pub reason: WebhookFailure,
    /// Human-readable message.
    pub message: String,
}

impl WebhookError {
    fn new(reason: WebhookFailure, message: &str) -> Self {
        Self {
            reason,
            message: message.to_owned(),
        }
    }

    /// Stable code, `webhook_<reason>` (like the other SDKs).
    #[must_use]
    pub fn code(&self) -> String {
        format!("webhook_{}", self.reason.as_str())
    }
}

impl fmt::Display for WebhookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason.as_str(), self.message)
    }
}

impl std::error::Error for WebhookError {}

/// A message inside a [`WebhookEvent`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebhookMessage<T = Value> {
    /// Message id.
    #[serde(default)]
    pub id: String,
    /// Topic.
    #[serde(default)]
    pub topic: String,
    /// Sequence number inside the topic.
    #[serde(default)]
    pub seq: i64,
    /// The payload that was published.
    pub data: T,
    /// Attributes.
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    /// Ordering key.
    #[serde(default)]
    pub ordering_key: Option<String>,
    /// Dedup id.
    #[serde(default)]
    pub dedup_id: Option<String>,
    /// Who published it.
    #[serde(default)]
    pub publisher: String,
    /// Size in bytes.
    #[serde(default)]
    pub bytes: i64,
    /// Publish time (RFC 3339).
    #[serde(default)]
    pub published_at: String,
}

/// What a push subscription delivers: the body of the POST.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WebhookEvent<T = Value> {
    /// Subscription name (without the tenant prefix).
    pub subscription: String,
    /// 1 on the first delivery, then counts up with every retry.
    pub delivery_attempt: i64,
    /// The message.
    pub message: WebhookMessage<T>,
}

/// The current time in Unix seconds.
#[must_use]
pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Builds the header value the server would send: `t=<timestamp>,v1=<hex>`. For tests of your own
/// handler, or to drive it from a script; the server signs with the same function. `timestamp` is
/// Unix seconds.
#[must_use]
pub fn sign(secret: &str, body: &[u8], timestamp: u64) -> String {
    format!("t={timestamp},v1={}", to_hex(&mac_bytes(secret, timestamp, body)))
}

/// Verifies one signing secret. `header` is the value of `X-Forgebase-Signature` (`None` when the
/// request has none); `tolerance` bounds how far the signed timestamp may be from now in either
/// direction ([`DEFAULT_TOLERANCE`] is five minutes; `Duration::MAX` turns the check off, for tests).
pub fn verify(secret: &str, header: Option<&str>, body: &[u8], tolerance: Duration) -> Result<(), WebhookError> {
    verify_any(&[secret], header, body, tolerance)
}

/// Like [`verify`], accepting any of several secrets: the new and the old one while you roll the
/// secret over. An empty list never verifies.
pub fn verify_any(
    secrets: &[&str],
    header: Option<&str>,
    body: &[u8],
    tolerance: Duration,
) -> Result<(), WebhookError> {
    verify_at(secrets, header, body, tolerance, now_secs())
}

/// Like [`verify_any`] with an explicit clock (`now` in Unix seconds), for deterministic tests.
pub fn verify_at(
    secrets: &[&str],
    header: Option<&str>,
    body: &[u8],
    tolerance: Duration,
    now: u64,
) -> Result<(), WebhookError> {
    let header = match header {
        Some(h) if !h.trim().is_empty() => h,
        _ => {
            return Err(WebhookError::new(
                WebhookFailure::MissingHeader,
                "the X-Forgebase-Signature header is missing",
            ));
        }
    };
    let parsed = parse_header(header).ok_or_else(|| {
        WebhookError::new(
            WebhookFailure::MalformedHeader,
            "the X-Forgebase-Signature header is malformed",
        )
    })?;
    if now.abs_diff(parsed.timestamp) > tolerance.as_secs() {
        return Err(WebhookError::new(
            WebhookFailure::TimestampOutOfTolerance,
            "the webhook timestamp is outside the tolerance window",
        ));
    }
    // Every secret and every candidate is compared, whatever matched first, so timing says
    // nothing about which one did. `verify_slice` compares in constant time.
    let mut ok = false;
    for secret in secrets {
        let mac = keyed(secret, parsed.timestamp, body);
        for candidate in &parsed.signatures {
            ok |= mac.clone().verify_slice(candidate).is_ok();
        }
    }
    if ok {
        Ok(())
    } else {
        Err(WebhookError::new(
            WebhookFailure::SignatureMismatch,
            "no signature matches the payload and secret",
        ))
    }
}

/// Verifies the request, then parses the body into the push envelope. `T` is the type of the
/// published `data` ([`serde_json::Value`] for anything). Fails with a [`WebhookError`] (`reason`
/// says why) when the signature, the timestamp or the payload is not acceptable. The body is never
/// parsed before its signature checks out.
pub fn construct_event<T: serde::de::DeserializeOwned>(
    secret: &str,
    header: Option<&str>,
    body: &[u8],
    tolerance: Duration,
) -> Result<WebhookEvent<T>, WebhookError> {
    construct_event_any(&[secret], header, body, tolerance)
}

/// Like [`construct_event`], accepting any of several secrets.
pub fn construct_event_any<T: serde::de::DeserializeOwned>(
    secrets: &[&str],
    header: Option<&str>,
    body: &[u8],
    tolerance: Duration,
) -> Result<WebhookEvent<T>, WebhookError> {
    construct_event_at(secrets, header, body, tolerance, now_secs())
}

/// Like [`construct_event_any`] with an explicit clock (`now` in Unix seconds).
pub fn construct_event_at<T: serde::de::DeserializeOwned>(
    secrets: &[&str],
    header: Option<&str>,
    body: &[u8],
    tolerance: Duration,
    now: u64,
) -> Result<WebhookEvent<T>, WebhookError> {
    verify_at(secrets, header, body, tolerance, now)?;
    let invalid = |m: &str| WebhookError::new(WebhookFailure::InvalidPayload, m);
    let text = std::str::from_utf8(body).map_err(|_| invalid("the webhook body is not valid UTF-8 JSON"))?;
    let v: Value = serde_json::from_str(text).map_err(|_| invalid("the webhook body is not valid JSON"))?;
    let is_envelope = v.as_object().is_some_and(|o| {
        o.get("subscription").is_some_and(Value::is_string)
            && o.get("delivery_attempt").is_some_and(Value::is_number)
            && o.get("message").is_some_and(Value::is_object)
    });
    if !is_envelope {
        return Err(invalid(
            "the webhook body is not a {subscription, delivery_attempt, message} envelope",
        ));
    }
    serde_json::from_value(v).map_err(|e| invalid(&format!("the webhook envelope does not decode: {e}")))
}

struct ParsedHeader {
    timestamp: u64,
    signatures: Vec<Vec<u8>>,
}

/// `t=<secs>,v1=<hex>[,v1=<hex>...]`; unknown keys (future schemes) are ignored; undecodable `v1`
/// values never match; a repeated or non-numeric `t`, a missing `t` or a missing `v1` is malformed.
fn parse_header(header: &str) -> Option<ParsedHeader> {
    let mut timestamp: Option<u64> = None;
    let mut signatures = Vec::new();
    let mut saw_v1 = false;
    for part in header.split(',') {
        let Some((key, value)) = part.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "t" => {
                if timestamp.is_some()
                    || value.is_empty()
                    || value.len() > 15
                    || !value.bytes().all(|b| b.is_ascii_digit())
                {
                    return None;
                }
                timestamp = Some(value.parse().ok()?);
            }
            "v1" => {
                saw_v1 = true;
                if let Some(bytes) = from_hex(value) {
                    signatures.push(bytes);
                }
            }
            _ => {}
        }
    }
    if !saw_v1 {
        return None;
    }
    Some(ParsedHeader {
        timestamp: timestamp?,
        signatures,
    })
}

/// HMAC-SHA256 keyed with the secret and fed `"<ts>."` and the body.
fn keyed(secret: &str, ts: u64, body: &[u8]) -> Hmac<Sha256> {
    // HMAC accepts keys of any length, so this cannot fail.
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes())
        .unwrap_or_else(|_| unreachable!("HMAC takes keys of any length"));
    mac.update(format!("{ts}.").as_bytes());
    mac.update(body);
    mac
}

fn mac_bytes(secret: &str, ts: u64, body: &[u8]) -> Vec<u8> {
    keyed(secret, ts, body).finalize().into_bytes().to_vec()
}

fn from_hex(hex: &str) -> Option<Vec<u8>> {
    if hex.is_empty() || !hex.len().is_multiple_of(2) || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    use fmt::Write as _;
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
