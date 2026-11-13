//! The inbound webhook inspector (ADR-0032): every request that reaches
//! `POST /api/v1/billing/webhooks/{provider}` is recorded in
//! `_forgebase_billing_webhook_deliveries`, verified or not, with what
//! verification and processing made of it.
//!
//! | Recorded | Rule |
//! |---|---|
//! | provider, account, received time, client address | always |
//! | headers | an allowlist only (content type, user agent, forwarding headers, the provider signature headers needed to verify the body again); never `authorization`, cookies or API keys |
//! | raw body | up to [`WebhookLogConfig::body_max_bytes`] (`body_truncated` when longer) |
//! | verification | `verified`, `signature_invalid`, `timestamp_out_of_tolerance`, `unknown_provider`, `malformed`, `rejected`, `error`, with a safe reason (the stable code and the driver's fixed message; never a secret or an expected signature) |
//! | processing | `applied`, `ignored`, `duplicate` (the event was handled before), `failed` (with the error), the event id and type, the internal objects it touched, latency |
//!
//! **Abuse safety.** A verified delivery is authenticated by the provider and
//! always recorded. An unverified one is recorded only within a budget per
//! provider (deliveries and stored bytes per minute; unknown provider names
//! share one budget) and under a total cap (rows and bytes, oldest unverified
//! rows evicted first), so forged traffic cannot fill the disk. Over the byte
//! budget the row keeps no body or headers; over the count budget nothing is
//! stored. Recording never changes the HTTP answer to the provider, and an
//! unverified payload is never processed. Rows older than
//! [`WebhookLogConfig::retention_days`] are pruned by `billing.maintenance`.
//!
//! **Operators** (platform administrators): list, read (headers and body),
//! **replay** a verified delivery through the normal processing path (the
//! event dedup makes a processed event a reported no-op, `already_applied`),
//! and **re-verify** a rejected delivery with the current secrets, after a
//! misconfigured secret was fixed; it is processed if it now verifies,
//! exactly once (one operator action at a time, a claim on the row, and the
//! event dedup in the write transaction). Both are audit-logged.
//!
//! Metrics: `forgebase_billing_webhook_deliveries_total{provider,outcome}`,
//! `forgebase_billing_webhook_processing_seconds{provider}` and
//! `forgebase_billing_webhook_log_dropped_total{provider,kind}`; the labels
//! come from fixed lists (unknown provider names are `other`).

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use base64::Engine as _;
use forgebase_core::ErrorCode;
use forgebase_data::AccessContext;
use forgebase_db::Value;
use forgebase_entitlements::model::{ts, ts_opt};
use forgebase_observability::names;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

use crate::error::{BillingError, Result};
use crate::model::WebhookOutcome;
use crate::money::hex;
use crate::service::{BillingService, DAY, Tx, int, new_id, opt_int, opt_text, text};

/// Provider names the inspector knows; anything else is labelled `other`.
pub const KNOWN_PROVIDERS: &[&str] = &[
    "stripe",
    "razorpay",
    "wechat_pay",
    "manual",
    "app_store",
    "google_play",
    "creem",
];

/// Headers kept with a delivery (lower case). Everything else is dropped,
/// so `authorization`, `cookie`, `proxy-authorization` and API keys are
/// never stored. The signature headers authenticate one stored body under
/// the provider's secret; they do not reveal the secret and are what a
/// re-verification checks again.
pub const HEADER_ALLOWLIST: &[&str] = &[
    "content-type",
    "content-length",
    "content-encoding",
    "user-agent",
    "from",
    "x-request-id",
    "request-id",
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-proto",
    "x-real-ip",
    "stripe-signature",
    "x-razorpay-signature",
    "x-razorpay-event-id",
    "wechatpay-signature",
    "wechatpay-timestamp",
    "wechatpay-nonce",
    "wechatpay-serial",
    "wechatpay-signature-type",
    "creem-signature",
];

/// Longest stored header value (bytes); longer values are cut.
const MAX_HEADER_VALUE: usize = 512;
/// Longest stored reason or detail (bytes).
const MAX_REASON: usize = 500;
/// A re-verification claim older than this is abandoned (crash) and can be
/// taken again.
const CLAIM_LEASE_MS: i64 = 5 * 60_000;
/// Rows deleted per prune statement.
const PRUNE_BATCH: i64 = 500;

/// Settings of the inspector. The server sets them from `[billing]`
/// (`webhook_log_retention_days`) and `[auth] trust_forwarded_for`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookLogConfig {
    /// Days a delivery is kept (default 30, at least 1).
    pub retention_days: u32,
    /// Longest stored body in bytes (default 256 KiB, the route's request
    /// limit); a longer body is stored cut (`body_truncated`) and cannot be
    /// replayed or re-verified.
    pub body_max_bytes: usize,
    /// Unverified deliveries recorded per provider and minute (default 60).
    pub unverified_per_minute: u32,
    /// Bytes (body and headers) of unverified deliveries stored per provider
    /// and minute (default 1 MiB); over it the row keeps neither.
    pub unverified_bytes_per_minute: usize,
    /// Unverified rows kept in all (default 10 000; the oldest go first).
    pub unverified_max_rows: u32,
    /// Bytes of unverified rows kept in all (default 64 MiB).
    pub unverified_max_bytes: u64,
    /// The client address is the last `X-Forwarded-For` entry rather than the
    /// socket peer (`[auth] trust_forwarded_for`).
    pub trust_forwarded_for: bool,
}

impl Default for WebhookLogConfig {
    fn default() -> Self {
        Self {
            retention_days: 30,
            body_max_bytes: 256 * 1024,
            unverified_per_minute: 60,
            unverified_bytes_per_minute: 1024 * 1024,
            unverified_max_rows: 10_000,
            unverified_max_bytes: 64 * 1024 * 1024,
            trust_forwarded_for: false,
        }
    }
}

/// The per-provider budget of one minute.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BudgetWindow {
    minute: i64,
    count: u32,
    bytes: usize,
}

/// The inspector's state in the service.
#[derive(Debug, Default)]
pub(crate) struct InspectorState {
    pub config: std::sync::RwLock<WebhookLogConfig>,
    pub budget: std::sync::Mutex<HashMap<&'static str, BudgetWindow>>,
    /// One operator replay or re-verification at a time.
    pub actions: tokio::sync::Mutex<()>,
}

/// What the budget lets an unverified delivery keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    All,
    /// A row without body and headers.
    Metadata,
    Nothing,
}

// ---------------------------------------------------------------------------
// Outcomes
// ---------------------------------------------------------------------------

/// What verification made of a delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebhookVerification {
    /// Signature (and timestamp, origin) accepted.
    Verified,
    /// Missing, forged or tampered signature.
    SignatureInvalid,
    /// Signed timestamp (or push token) outside the tolerance: a replayed old delivery.
    TimestampOutOfTolerance,
    /// No such provider, or it is not configured.
    UnknownProvider,
    /// The body or a required header cannot be read.
    Malformed,
    /// Refused for another reason (another app or merchant, live mode, bad certificate).
    Rejected,
    /// Verification could not complete (the store API or the database failed).
    Error,
}

impl WebhookVerification {
    /// Stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::SignatureInvalid => "signature_invalid",
            Self::TimestampOutOfTolerance => "timestamp_out_of_tolerance",
            Self::UnknownProvider => "unknown_provider",
            Self::Malformed => "malformed",
            Self::Rejected => "rejected",
            Self::Error => "error",
        }
    }

    /// Parses a stored name.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "verified" => Self::Verified,
            "signature_invalid" => Self::SignatureInvalid,
            "timestamp_out_of_tolerance" => Self::TimestampOutOfTolerance,
            "unknown_provider" => Self::UnknownProvider,
            "malformed" => Self::Malformed,
            "rejected" => Self::Rejected,
            "error" => Self::Error,
            _ => return None,
        })
    }
}

/// Every unverified state (SQL list).
const UNVERIFIED_SQL: &str =
    "('signature_invalid', 'timestamp_out_of_tolerance', 'unknown_provider', 'malformed', 'rejected', 'error')";

/// What processing made of a verified delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebhookProcessing {
    /// The event changed billing state.
    Applied,
    /// Recorded without effect (`stale`, `too_old`, `unknown_invoice`, ...).
    Ignored,
    /// The event was processed or ignored before: nothing changed.
    Duplicate,
    /// Processing failed; the provider's redelivery (or a replay) retries it.
    Failed,
}

impl WebhookProcessing {
    /// Stored name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Ignored => "ignored",
            Self::Duplicate => "duplicate",
            Self::Failed => "failed",
        }
    }

    fn of_status(status: &str) -> Self {
        match status {
            "processed" => Self::Applied,
            "ignored" => Self::Ignored,
            _ => Self::Failed,
        }
    }
}

/// Every value of `outcome`: the processing outcome of a verified delivery,
/// else its verification outcome.
pub const OUTCOMES: &[&str] = &[
    "applied",
    "ignored",
    "duplicate",
    "failed",
    "signature_invalid",
    "timestamp_out_of_tolerance",
    "unknown_provider",
    "malformed",
    "rejected",
    "error",
];

/// Maps a verification error to its outcome and a safe reason: the stable
/// code and the driver's fixed message for refusals, only the code for
/// failures whose message may echo upstream text.
pub(crate) fn classify(e: &BillingError) -> (WebhookVerification, String) {
    use WebhookVerification as V;
    let code = e.code();
    let v = match e {
        BillingError::NotConfigured(_) => V::UnknownProvider,
        BillingError::Invalid {
            code: "unknown_provider",
            ..
        } => V::UnknownProvider,
        BillingError::Webhook {
            code: "invalid_signature",
            ..
        } => V::SignatureInvalid,
        BillingError::Webhook {
            code: "stale_signature",
            ..
        } => V::TimestampOutOfTolerance,
        BillingError::Webhook {
            code: "invalid_payload",
            ..
        } => V::Malformed,
        BillingError::Webhook { .. }
        | BillingError::Invalid { .. }
        | BillingError::Conflict { .. }
        | BillingError::Forbidden(_) => V::Rejected,
        _ => V::Error,
    };
    let reason = match e {
        BillingError::Webhook { message, .. }
        | BillingError::Invalid { message, .. }
        | BillingError::Conflict { message, .. } => format!("{code}: {message}"),
        BillingError::NotConfigured(m) => format!("{code}: {m}"),
        _ => code.to_owned(),
    };
    (v, cut(&reason, MAX_REASON))
}

fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_owned()
}

/// Internal objects a delivery touched.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct Links {
    /// Row of the event dedup table (`webhook-events`).
    #[serde(rename = "webhook_event_id")]
    pub webhook_event: Option<String>,
    /// Billing customer.
    #[serde(rename = "customer_id")]
    pub customer: Option<String>,
    /// Subscription.
    #[serde(rename = "subscription_id")]
    pub subscription: Option<String>,
    /// One-time order.
    #[serde(rename = "order_id")]
    pub order: Option<String>,
    /// Invoice.
    #[serde(rename = "invoice_id")]
    pub invoice: Option<String>,
    /// Payment.
    #[serde(rename = "payment_id")]
    pub payment: Option<String>,
    /// Refund.
    #[serde(rename = "refund_id")]
    pub refund: Option<String>,
    /// Store purchase (App Store, Google Play, Creem).
    #[serde(rename = "store_purchase_id")]
    pub store_purchase: Option<String>,
}

/// How a delivery is processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// As it arrives from the provider.
    Live,
    /// An operator replay of a verified stored delivery.
    Replay,
    /// An operator re-verification of a stored rejected delivery.
    Reverify,
}

/// A delivery whose event was handled before.
pub(crate) struct Duplicate {
    pub outcome: WebhookOutcome,
    /// Status of the event row (`processed`, `ignored`).
    pub prior: String,
    /// Id of the event row.
    pub event_row: String,
}

/// What verifying and processing one delivery did.
pub(crate) struct Handled {
    /// The answer to the caller (unchanged by recording).
    pub outcome: Result<WebhookOutcome>,
    pub verification: WebhookVerification,
    pub reason: Option<String>,
    pub account: Option<String>,
    pub event_id: Option<String>,
    pub event_type: Option<String>,
    pub processing: Option<WebhookProcessing>,
    pub detail: Option<String>,
    /// Status of the event before a duplicate delivery.
    pub prior: Option<String>,
    pub links: Links,
}

impl Handled {
    /// Verification refused the delivery; nothing was processed.
    pub fn unverified(e: BillingError, account: Option<String>) -> Self {
        let (verification, reason) = classify(&e);
        Self {
            outcome: Err(e),
            verification,
            reason: Some(reason),
            account,
            event_id: None,
            event_type: None,
            processing: None,
            detail: None,
            prior: None,
            links: Links::default(),
        }
    }

    /// `outcome` as stored and labelled: processing if verified, else verification.
    pub fn outcome_name(&self) -> &'static str {
        match (self.verification, self.processing) {
            (WebhookVerification::Verified, Some(p)) => p.as_str(),
            (WebhookVerification::Verified, None) => "failed",
            (v, _) => v.as_str(),
        }
    }
}

/// A verified delivery being processed.
pub(crate) struct Verified {
    account: Option<String>,
    event_id: String,
    event_type: String,
}

impl Verified {
    pub fn new(account: Option<String>, event_id: &str, event_type: &str) -> Self {
        Self {
            account,
            event_id: event_id.to_owned(),
            event_type: event_type.to_owned(),
        }
    }

    fn handled(
        self,
        outcome: Result<WebhookOutcome>,
        processing: WebhookProcessing,
        detail: Option<String>,
    ) -> Handled {
        Handled {
            outcome,
            verification: WebhookVerification::Verified,
            reason: None,
            account: self.account,
            event_id: Some(self.event_id),
            event_type: Some(self.event_type),
            processing: Some(processing),
            detail,
            prior: None,
            links: Links::default(),
        }
    }

    pub fn duplicate(self, d: Duplicate) -> Handled {
        let detail = d.outcome.detail.clone();
        let mut h = self.handled(Ok(d.outcome), WebhookProcessing::Duplicate, detail);
        h.prior = Some(d.prior);
        h.links.webhook_event = Some(d.event_row);
        h
    }

    pub fn failed(self, e: BillingError, event_row: Option<String>) -> Handled {
        let detail = cut(&format!("{}: {e}", e.code()), MAX_REASON);
        let mut h = self.handled(Err(e), WebhookProcessing::Failed, Some(detail));
        h.links.webhook_event = event_row;
        h
    }

    /// `status` is the event row's (`processed`, `ignored`).
    pub fn done(self, status: &str, detail: Option<String>, links: Links) -> Handled {
        let outcome = WebhookOutcome {
            result: status.to_owned(),
            detail: detail.clone(),
            event_id: self.event_id.clone(),
        };
        let mut h = self.handled(Ok(outcome), WebhookProcessing::of_status(status), detail);
        h.links = links;
        h
    }
}

/// A request as it arrived.
pub(crate) struct Incoming<'a> {
    pub provider: &'a str,
    pub headers: &'a [(String, String)],
    pub body: &'a [u8],
    pub client_addr: Option<String>,
    pub received_at: i64,
}

/// The metric and budget label of a provider name.
fn label(provider: &str) -> &'static str {
    KNOWN_PROVIDERS
        .iter()
        .find(|p| **p == provider)
        .copied()
        .unwrap_or("other")
}

/// The provider name as stored: known names as they are, anything else cut
/// to 64 characters of `[A-Za-z0-9_.-]` (others become `_`).
fn stored_provider(provider: &str) -> String {
    if KNOWN_PROVIDERS.contains(&provider) {
        return provider.to_owned();
    }
    let s: String = provider
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "_".to_owned() } else { s }
}

/// The allowlisted headers as a JSON object (repeated names joined by `, `).
fn kept_headers(headers: &[(String, String)]) -> Map<String, Json> {
    let mut out: Map<String, Json> = Map::new();
    for (k, v) in headers {
        let k = k.to_ascii_lowercase();
        if !HEADER_ALLOWLIST.contains(&k.as_str()) {
            continue;
        }
        let v = cut(v, MAX_HEADER_VALUE);
        let joined = match out.get(&k).and_then(Json::as_str) {
            Some(prev) => cut(&format!("{prev}, {v}"), MAX_HEADER_VALUE),
            None => v,
        };
        out.insert(k, Json::String(joined));
    }
    out
}

/// The client address of a webhook request: the socket peer, or the last
/// `X-Forwarded-For` entry when the server trusts it.
pub fn client_addr(
    trust_forwarded_for: bool,
    headers: &[(String, String)],
    peer: Option<SocketAddr>,
) -> Option<String> {
    if trust_forwarded_for
        && let Some(ip) = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("x-forwarded-for"))
            .and_then(|(_, v)| v.rsplit(',').next())
            .and_then(|v| v.trim().parse::<IpAddr>().ok())
    {
        return Some(ip.to_string());
    }
    peer.map(|p| p.ip().to_string())
}

// ---------------------------------------------------------------------------
// API shapes
// ---------------------------------------------------------------------------

/// One inbound webhook request (without headers and body).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WebhookDelivery {
    /// Id (`whd_…`).
    pub id: String,
    /// Provider named in the path.
    pub provider: String,
    /// Provider account it was for, when known (Razorpay key id, WeChat Pay
    /// merchant id, App Store bundle id, Google Play package name).
    pub account: Option<String>,
    /// Received.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub received_at: i64,
    /// Client address (socket peer, or the trusted `X-Forwarded-For`).
    pub client_addr: Option<String>,
    /// Body size as received, in bytes.
    pub body_size: i64,
    /// The body is stored (unverified deliveries over the budget are not).
    pub body_stored: bool,
    /// Only the first `body_max_bytes` are stored.
    pub body_truncated: bool,
    /// SHA-256 of the whole body, hex (the event's `payload_hash`).
    pub body_sha256: String,
    /// What verification made of it.
    pub verification: WebhookVerification,
    /// Why verification refused it (stable code and a fixed message).
    pub reason: Option<String>,
    /// Event id at the provider (verified deliveries).
    pub event_id: Option<String>,
    /// Event type.
    pub event_type: Option<String>,
    /// What processing made of it (verified deliveries).
    pub processing: Option<WebhookProcessing>,
    /// `processing` when verified, else `verification`.
    pub outcome: String,
    /// Processing detail (`invoice_paid`, `stale`, `too_old`, ...) or the error.
    pub detail: Option<String>,
    /// Internal objects it touched.
    pub links: Links,
    /// Verification and processing time in microseconds.
    pub latency_us: Option<i64>,
    /// Operator replays.
    pub replays: i64,
    /// Operator re-verifications.
    pub reverify_attempts: i64,
    /// When a re-verification accepted it.
    #[serde(with = "ts_opt")]
    #[schema(value_type = Option<String>)]
    pub reverified_at: Option<i64>,
    /// Last change.
    #[serde(with = "ts")]
    #[schema(value_type = String)]
    pub updated_at: i64,
}

/// A delivery with its stored headers and body. Bodies can contain personal
/// data (names, emails, addresses): platform administrators only.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WebhookDeliveryDetail {
    /// The delivery.
    #[serde(flatten)]
    pub delivery: WebhookDelivery,
    /// Allowlisted headers (lower-case names).
    #[schema(value_type = Object)]
    pub headers: Json,
    /// The stored body: text when it is UTF-8, else base64.
    pub body: Option<String>,
    /// `utf8` or `base64`.
    pub body_encoding: Option<String>,
}

/// A page of deliveries.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WebhookDeliveryPage {
    /// Deliveries, newest first.
    pub items: Vec<WebhookDelivery>,
    /// All deliveries matching the filters.
    pub total: i64,
    /// Page size.
    pub limit: i64,
    /// Rows skipped.
    pub offset: i64,
}

/// Filters of the delivery list.
#[derive(Debug, Clone, Default)]
pub struct WebhookDeliveryFilter {
    /// Only this provider.
    pub provider: Option<String>,
    /// Only this outcome (one of [`OUTCOMES`]).
    pub outcome: Option<String>,
    /// Only this event type.
    pub event_type: Option<String>,
    /// Only this provider event id.
    pub event_id: Option<String>,
    /// Received at or after (epoch ms).
    pub from: Option<i64>,
    /// Received before (epoch ms).
    pub to: Option<i64>,
    /// Page size (1-500, default 50).
    pub limit: Option<u32>,
    /// Rows skipped.
    pub offset: Option<u32>,
}

/// What a replay or re-verification did.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WebhookDeliveryResult {
    /// `applied`, `ignored`, `failed`, `already_applied` (the event was
    /// processed before: nothing changed), `already_ignored` (it was
    /// ignored before: nothing changed) or `unverified` (the stored delivery
    /// does not verify with the current secrets: nothing was processed).
    pub result: String,
    /// Processing detail, or why it does not verify.
    pub detail: Option<String>,
    /// The delivery afterwards.
    pub delivery: WebhookDelivery,
}

const COLUMNS: &str = "id, provider, account, received_at, client_addr, body_size, body_sha256, body_truncated, \
     verification, reason, event_id, event_type, processing, outcome, detail, webhook_event_id, customer_id, \
     subscription_id, order_id, invoice_id, payment_id, refund_id, store_purchase_id, latency_us, replays, \
     reverify_attempts, reverified_at, updated_at, CASE WHEN body IS NULL THEN 0 ELSE 1 END";

fn delivery_row(r: &[Value]) -> WebhookDelivery {
    WebhookDelivery {
        id: text(r, 0),
        provider: text(r, 1),
        account: opt_text(r, 2),
        received_at: int(r, 3),
        client_addr: opt_text(r, 4),
        body_size: int(r, 5),
        body_sha256: text(r, 6),
        body_truncated: int(r, 7) != 0,
        verification: WebhookVerification::parse(&text(r, 8)).unwrap_or(WebhookVerification::Error),
        reason: opt_text(r, 9),
        event_id: opt_text(r, 10),
        event_type: opt_text(r, 11),
        processing: match opt_text(r, 12).as_deref() {
            Some("applied") => Some(WebhookProcessing::Applied),
            Some("ignored") => Some(WebhookProcessing::Ignored),
            Some("duplicate") => Some(WebhookProcessing::Duplicate),
            Some("failed") => Some(WebhookProcessing::Failed),
            _ => None,
        },
        outcome: text(r, 13),
        detail: opt_text(r, 14),
        links: Links {
            webhook_event: opt_text(r, 15),
            customer: opt_text(r, 16),
            subscription: opt_text(r, 17),
            order: opt_text(r, 18),
            invoice: opt_text(r, 19),
            payment: opt_text(r, 20),
            refund: opt_text(r, 21),
            store_purchase: opt_text(r, 22),
        },
        latency_us: opt_int(r, 23),
        replays: int(r, 24),
        reverify_attempts: int(r, 25),
        reverified_at: opt_int(r, 26),
        updated_at: int(r, 27),
        body_stored: int(r, 28) != 0,
    }
}

fn blob(r: &[Value], i: usize) -> Option<Vec<u8>> {
    match r.get(i)? {
        Value::Blob(b) => Some(b.clone()),
        Value::Text(s) => Some(s.clone().into_bytes()),
        _ => None,
    }
}

/// A stored delivery with what a replay needs.
struct Stored {
    delivery: WebhookDelivery,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

impl Stored {
    /// The complete stored body, or `409 body_not_stored`.
    fn full_body(&self) -> Result<&[u8]> {
        match &self.body {
            Some(b) if !self.delivery.body_truncated => Ok(b),
            Some(_) => Err(BillingError::conflict(
                "body_not_stored",
                "only part of this delivery's body is stored; it cannot be processed again",
            )),
            None => Err(BillingError::conflict(
                "body_not_stored",
                "this delivery's body was not stored (over the unverified budget)",
            )),
        }
    }
}

fn result_of(h: &Handled) -> (String, Option<String>) {
    match (h.verification, h.processing) {
        (WebhookVerification::Verified, Some(WebhookProcessing::Duplicate)) => (
            match h.prior.as_deref() {
                Some("ignored") => "already_ignored".to_owned(),
                _ => "already_applied".to_owned(),
            },
            h.detail.clone(),
        ),
        (WebhookVerification::Verified, p) => (
            p.unwrap_or(WebhookProcessing::Failed).as_str().to_owned(),
            h.detail.clone(),
        ),
        _ => ("unverified".to_owned(), h.reason.clone()),
    }
}

impl BillingService {
    /// Replaces the inspector settings.
    pub fn set_webhook_log(&self, config: WebhookLogConfig) {
        if let Ok(mut c) = self.inner.inspector.config.write() {
            *c = config;
        }
    }

    /// The inspector settings.
    pub fn webhook_log(&self) -> WebhookLogConfig {
        self.inner
            .inspector
            .config
            .read()
            .map(|c| c.clone())
            .unwrap_or_default()
    }

    /// Takes room in the provider's budget for an unverified delivery.
    fn admit(&self, key: &'static str, now: i64, bytes: usize, cfg: &WebhookLogConfig) -> Keep {
        let Ok(mut m) = self.inner.inspector.budget.lock() else {
            return Keep::Nothing;
        };
        let minute = now.div_euclid(60_000);
        let w = m.entry(key).or_default();
        if w.minute != minute {
            *w = BudgetWindow {
                minute,
                count: 0,
                bytes: 0,
            };
        }
        if w.count >= cfg.unverified_per_minute {
            return Keep::Nothing;
        }
        w.count += 1;
        if w.bytes.saturating_add(bytes) > cfg.unverified_bytes_per_minute {
            return Keep::Metadata;
        }
        w.bytes += bytes;
        Keep::All
    }

    /// Records a delivery and its metrics. Never fails and never changes the
    /// answer: a recording error is logged.
    pub(crate) async fn log_delivery(&self, d: &Incoming<'_>, h: &Handled, latency: Duration) {
        let key = label(d.provider);
        let outcome = h.outcome_name();
        metrics::counter!(names::BILLING_WEBHOOK_DELIVERIES_TOTAL, "provider" => key, "outcome" => outcome)
            .increment(1);
        metrics::histogram!(names::BILLING_WEBHOOK_PROCESSING_SECONDS, "provider" => key).record(latency.as_secs_f64());
        let cfg = self.webhook_log();
        let verified = h.verification == WebhookVerification::Verified;
        let truncated = d.body.len() > cfg.body_max_bytes;
        let body = &d.body[..d.body.len().min(cfg.body_max_bytes)];
        let headers = Json::Object(kept_headers(d.headers)).to_string();
        let keep = if verified {
            Keep::All
        } else {
            self.admit(key, d.received_at, body.len() + headers.len(), &cfg)
        };
        let (body, headers) = match keep {
            Keep::All => (Some(body), headers),
            Keep::Metadata => {
                metrics::counter!(names::BILLING_WEBHOOK_LOG_DROPPED_TOTAL, "provider" => key, "kind" => "body")
                    .increment(1);
                (None, "{}".to_owned())
            }
            Keep::Nothing => {
                metrics::counter!(names::BILLING_WEBHOOK_LOG_DROPPED_TOTAL, "provider" => key, "kind" => "delivery")
                    .increment(1);
                return;
            }
        };
        let stored_bytes = body.map_or(0, <[u8]>::len) + headers.len();
        let latency_us = i64::try_from(latency.as_micros()).unwrap_or(i64::MAX);
        let l = &h.links;
        let params: Vec<Value> = vec![
            new_id("whd").into(),
            stored_provider(d.provider).into(),
            h.account.clone().into(),
            d.received_at.into(),
            d.client_addr.clone().into(),
            headers.into(),
            body.map(<[u8]>::to_vec).map_or(Value::Null, Value::Blob),
            i64::try_from(d.body.len()).unwrap_or(i64::MAX).into(),
            hex(&Sha256::digest(d.body)).into(),
            i64::from(truncated && body.is_some()).into(),
            i64::try_from(stored_bytes).unwrap_or(i64::MAX).into(),
            h.verification.as_str().into(),
            h.reason.clone().into(),
            h.event_id.clone().into(),
            h.event_type.clone().into(),
            h.processing.map(WebhookProcessing::as_str).into(),
            outcome.into(),
            h.detail.clone().into(),
            l.webhook_event.clone().into(),
            l.customer.clone().into(),
            l.subscription.clone().into(),
            l.order.clone().into(),
            l.invoice.clone().into(),
            l.payment.clone().into(),
            l.refund.clone().into(),
            l.store_purchase.clone().into(),
            latency_us.into(),
        ];
        let res = async {
            let mut tx = self.begin().await?;
            let res = async {
                let mut t = Tx(tx.as_mut());
                if !verified {
                    evict_unverified(&mut t, &cfg, stored_bytes).await?;
                }
                t.exec(
                    "INSERT INTO _forgebase_billing_webhook_deliveries (id, provider, account, received_at, client_addr,
                         headers, body, body_size, body_sha256, body_truncated, stored_bytes, verification, reason,
                         event_id, event_type, processing, outcome, detail, webhook_event_id, customer_id,
                         subscription_id, order_id, invoice_id, payment_id, refund_id, store_purchase_id, latency_us,
                         updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19,
                         ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?4)",
                    params,
                )
                .await?;
                Ok(((), true))
            }
            .await;
            self.end(tx, res).await
        }
        .await;
        if let Err(e) = res {
            tracing::warn!(provider = key, error = %e, "billing webhook delivery not recorded");
        }
    }

    // -----------------------------------------------------------------------
    // Operators
    // -----------------------------------------------------------------------

    /// Inbound webhook deliveries, newest first, with filters and paging
    /// (platform administrators).
    pub async fn webhook_deliveries(
        &self,
        ctx: &AccessContext,
        f: WebhookDeliveryFilter,
    ) -> Result<WebhookDeliveryPage> {
        self.require_admin(ctx)?;
        let mut clauses: Vec<String> = Vec::new();
        let mut params: Vec<Value> = Vec::new();
        let mut eq = |col: &str, v: Option<String>, clauses: &mut Vec<String>| {
            if let Some(v) = v.filter(|v| !v.trim().is_empty()) {
                params.push(v.trim().to_owned().into());
                clauses.push(format!("{col} = ?{}", params.len()));
            }
        };
        if let Some(o) = f.outcome.as_deref().filter(|o| !o.is_empty())
            && !OUTCOMES.contains(&o)
        {
            return Err(BillingError::invalid(
                "invalid_parameter",
                format!("outcome must be one of {}", OUTCOMES.join(", ")),
            ));
        }
        eq("provider", f.provider, &mut clauses);
        eq("outcome", f.outcome, &mut clauses);
        eq("event_type", f.event_type, &mut clauses);
        eq("event_id", f.event_id, &mut clauses);
        if let Some(t) = f.from {
            params.push(t.into());
            clauses.push(format!("received_at >= ?{}", params.len()));
        }
        if let Some(t) = f.to {
            params.push(t.into());
            clauses.push(format!("received_at < ?{}", params.len()));
        }
        let filter = if clauses.is_empty() {
            String::new()
        } else {
            format!(" WHERE {}", clauses.join(" AND "))
        };
        let total = self
            .fetch(
                &format!("SELECT COUNT(*) FROM _forgebase_billing_webhook_deliveries{filter}"),
                params.clone(),
            )
            .await?
            .rows
            .first()
            .map(|r| int(r, 0))
            .unwrap_or(0);
        let limit = i64::from(f.limit.unwrap_or(50).clamp(1, 500));
        let offset = i64::from(f.offset.unwrap_or(0));
        let n = params.len();
        params.push(limit.into());
        params.push(offset.into());
        let rows = self
            .fetch(
                &format!(
                    "SELECT {COLUMNS} FROM _forgebase_billing_webhook_deliveries{filter}
                     ORDER BY received_at DESC, id DESC LIMIT ?{} OFFSET ?{}",
                    n + 1,
                    n + 2
                ),
                params,
            )
            .await?;
        Ok(WebhookDeliveryPage {
            items: rows.rows.iter().map(|r| delivery_row(r)).collect(),
            total,
            limit,
            offset,
        })
    }

    async fn stored_delivery(&self, id: &str) -> Result<Stored> {
        let rows = self
            .fetch(
                &format!("SELECT {COLUMNS}, headers, body FROM _forgebase_billing_webhook_deliveries WHERE id = ?1"),
                vec![id.into()],
            )
            .await?;
        let r = rows
            .rows
            .first()
            .ok_or_else(|| BillingError::NotFound(format!("webhook delivery {id}")))?;
        let headers = serde_json::from_str::<Map<String, Json>>(&text(r, 29))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|v| (k, v.to_owned())))
            .collect();
        Ok(Stored {
            delivery: delivery_row(r),
            headers,
            body: blob(r, 30),
        })
    }

    /// One delivery with its headers and body (platform administrators).
    pub async fn webhook_delivery(&self, ctx: &AccessContext, id: &str) -> Result<WebhookDeliveryDetail> {
        self.require_admin(ctx)?;
        let s = self.stored_delivery(id).await?;
        let (body, body_encoding) = match s.body {
            None => (None, None),
            Some(b) => match String::from_utf8(b) {
                Ok(t) => (Some(t), Some("utf8".to_owned())),
                Err(e) => (
                    Some(base64::engine::general_purpose::STANDARD.encode(e.into_bytes())),
                    Some("base64".to_owned()),
                ),
            },
        };
        Ok(WebhookDeliveryDetail {
            headers: Json::Object(s.headers.into_iter().map(|(k, v)| (k, Json::String(v))).collect()),
            delivery: s.delivery,
            body,
            body_encoding,
        })
    }

    /// Replays a verified stored delivery through the normal processing path
    /// (platform administrators). The stored body is verified again with the
    /// current secrets at the time it was received (Google Play pushes are
    /// re-read instead: their token is not stored, and the purchase state is
    /// read from the store API), so a stored row cannot inject a payload. The
    /// event dedup makes a processed event a reported no-op
    /// (`already_applied`); a failed one is processed again.
    pub async fn replay_webhook_delivery(&self, ctx: &AccessContext, id: &str) -> Result<WebhookDeliveryResult> {
        self.require_admin(ctx)?;
        let _one = self.inner.inspector.actions.lock().await;
        let s = self.stored_delivery(id).await?;
        if s.delivery.verification != WebhookVerification::Verified {
            return Err(BillingError::conflict(
                "delivery_not_verified",
                "only a verified delivery can be replayed; re-verify a rejected one",
            ));
        }
        let body = s.full_body()?;
        let started = Instant::now();
        let h = self
            .process_delivery(
                &s.delivery.provider,
                &s.headers,
                body,
                s.delivery.received_at,
                Mode::Replay,
            )
            .await;
        let latency = started.elapsed();
        metrics::histogram!(names::BILLING_WEBHOOK_PROCESSING_SECONDS, "provider" => label(&s.delivery.provider))
            .record(latency.as_secs_f64());
        let (result, detail) = result_of(&h);
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            if h.verification == WebhookVerification::Verified {
                update_processed(&mut t, id, &h, latency, now, "replays = replays + 1").await?;
            } else {
                t.exec(
                    "UPDATE _forgebase_billing_webhook_deliveries SET replays = replays + 1, updated_at = ?2 WHERE id = ?1",
                    vec![id.into(), now.into()],
                )
                .await?;
            }
            t.audit(
                &ctx.principal_key(),
                "webhook_delivery.replay",
                "webhook_delivery",
                id,
                json!({"provider": s.delivery.provider, "event_id": h.event_id, "result": result, "detail": detail}),
                now,
            )
            .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        Ok(WebhookDeliveryResult {
            result,
            detail,
            delivery: self.stored_delivery(id).await?.delivery,
        })
    }

    /// Verifies a stored rejected delivery again with the current secrets
    /// (after an operator fixed a misconfigured secret; platform
    /// administrators) and processes it if it now verifies, exactly once:
    /// operator actions run one at a time, the row is claimed, and the event
    /// dedup runs inside the write transaction. The signature timestamp and
    /// the replay window are checked against the time it was received.
    pub async fn reverify_webhook_delivery(&self, ctx: &AccessContext, id: &str) -> Result<WebhookDeliveryResult> {
        self.require_admin(ctx)?;
        let _one = self.inner.inspector.actions.lock().await;
        let s = self.stored_delivery(id).await?;
        if s.delivery.verification == WebhookVerification::Verified {
            return Err(BillingError::conflict(
                "delivery_already_verified",
                "this delivery is verified; replay it instead",
            ));
        }
        if s.delivery.provider == "google_play" {
            return Err(BillingError::conflict(
                "not_reverifiable",
                "Google Play pushes are authenticated by a short-lived token that is not stored; Pub/Sub redelivers unacknowledged messages",
            ));
        }
        let body = s.full_body()?.to_vec();
        let now = self.now();
        let claim = new_id("claim");
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let n = t
                .exec(
                    &format!(
                        "UPDATE _forgebase_billing_webhook_deliveries
                         SET claim = ?2, claimed_at = ?3, reverify_attempts = reverify_attempts + 1, updated_at = ?3
                         WHERE id = ?1 AND verification IN {UNVERIFIED_SQL} AND (claim IS NULL OR claimed_at < ?4)"
                    ),
                    vec![
                        id.into(),
                        claim.clone().into(),
                        now.into(),
                        (now - CLAIM_LEASE_MS).into(),
                    ],
                )
                .await?;
            Ok((n, n > 0))
        }
        .await;
        if self.end(tx, res).await? == 0 {
            let again = self.stored_delivery(id).await?;
            return Err(if again.delivery.verification == WebhookVerification::Verified {
                BillingError::conflict(
                    "delivery_already_verified",
                    "this delivery is verified; replay it instead",
                )
            } else {
                BillingError::conflict(
                    "reverify_in_progress",
                    "another re-verification of this delivery is running",
                )
            });
        }
        let started = Instant::now();
        let h = self
            .process_delivery(
                &s.delivery.provider,
                &s.headers,
                &body,
                s.delivery.received_at,
                Mode::Reverify,
            )
            .await;
        let latency = started.elapsed();
        metrics::histogram!(names::BILLING_WEBHOOK_PROCESSING_SECONDS, "provider" => label(&s.delivery.provider))
            .record(latency.as_secs_f64());
        let (result, detail) = result_of(&h);
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            if h.verification == WebhookVerification::Verified {
                update_processed(
                    &mut t,
                    id,
                    &h,
                    latency,
                    now,
                    "verification = 'verified', reason = NULL, reverified_at = ?3, claim = NULL, claimed_at = NULL",
                )
                .await?;
            } else {
                t.exec(
                    "UPDATE _forgebase_billing_webhook_deliveries SET verification = ?2, reason = ?3, outcome = ?2,
                         claim = NULL, claimed_at = NULL, updated_at = ?4
                     WHERE id = ?1",
                    vec![
                        id.into(),
                        h.verification.as_str().into(),
                        h.reason.clone().into(),
                        now.into(),
                    ],
                )
                .await?;
            }
            t.audit(
                &ctx.principal_key(),
                "webhook_delivery.reverify",
                "webhook_delivery",
                id,
                json!({"provider": s.delivery.provider, "event_id": h.event_id, "result": result, "detail": detail,
                       "verification": h.verification.as_str()}),
                now,
            )
            .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        Ok(WebhookDeliveryResult {
            result,
            detail,
            delivery: self.stored_delivery(id).await?.delivery,
        })
    }

    /// Deletes deliveries older than the retention (maintenance); returns how
    /// many.
    pub(crate) async fn prune_webhook_deliveries(&self) -> Result<i64> {
        let days = i64::from(self.webhook_log().retention_days.max(1));
        let cutoff = self.now() - days * DAY;
        let mut pruned = 0;
        for _ in 0..20 {
            let mut tx = self.begin().await?;
            let res = async {
                let mut t = Tx(tx.as_mut());
                let n = t
                    .exec(
                        "DELETE FROM _forgebase_billing_webhook_deliveries WHERE id IN (
                             SELECT id FROM _forgebase_billing_webhook_deliveries WHERE received_at < ?1
                             ORDER BY received_at LIMIT ?2)",
                        vec![cutoff.into(), PRUNE_BATCH.into()],
                    )
                    .await?;
                Ok((n, n > 0))
            }
            .await;
            let n = i64::try_from(self.end(tx, res).await?).unwrap_or(i64::MAX);
            pruned += n;
            if n < PRUNE_BATCH {
                break;
            }
        }
        Ok(pruned)
    }
}

/// Makes room for one more unverified row of `adding` bytes under the total
/// caps, oldest unverified rows first.
async fn evict_unverified(t: &mut Tx<'_>, cfg: &WebhookLogConfig, adding: usize) -> Result<()> {
    let rows = t
        .fetch(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(stored_bytes), 0) FROM _forgebase_billing_webhook_deliveries
                 WHERE verification IN {UNVERIFIED_SQL}"
            ),
            Vec::new(),
        )
        .await?;
    let (mut count, mut bytes) = rows.rows.first().map(|r| (int(r, 0), int(r, 1))).unwrap_or((0, 0));
    let max_rows = i64::from(cfg.unverified_max_rows.max(1));
    let max_bytes = i64::try_from(cfg.unverified_max_bytes).unwrap_or(i64::MAX);
    let adding = i64::try_from(adding).unwrap_or(i64::MAX);
    while count >= max_rows || (count > 0 && bytes.saturating_add(adding) > max_bytes) {
        let oldest = t
            .fetch(
                &format!(
                    "SELECT id, stored_bytes FROM _forgebase_billing_webhook_deliveries
                     WHERE verification IN {UNVERIFIED_SQL} ORDER BY received_at, id LIMIT 100"
                ),
                Vec::new(),
            )
            .await?;
        if oldest.rows.is_empty() {
            break;
        }
        let mut doomed: Vec<String> = Vec::new();
        for r in &oldest.rows {
            if count < max_rows && bytes.saturating_add(adding) <= max_bytes {
                break;
            }
            doomed.push(text(r, 0));
            count -= 1;
            bytes -= int(r, 1);
        }
        for id in doomed {
            t.exec(
                "DELETE FROM _forgebase_billing_webhook_deliveries WHERE id = ?1",
                vec![id.into()],
            )
            .await?;
        }
    }
    Ok(())
}

/// Writes a processing result onto a delivery row; `extra` is more SQL for
/// the `SET` list (`?3` is now).
async fn update_processed(
    t: &mut Tx<'_>,
    id: &str,
    h: &Handled,
    latency: Duration,
    now: i64,
    extra: &str,
) -> Result<()> {
    let l = &h.links;
    t.exec(
        &format!(
            "UPDATE _forgebase_billing_webhook_deliveries SET processing = ?2, updated_at = ?3, outcome = ?4,
                 detail = ?5, event_id = COALESCE(?6, event_id), event_type = COALESCE(?7, event_type),
                 account = COALESCE(?8, account), latency_us = ?9,
                 webhook_event_id = COALESCE(?10, webhook_event_id), customer_id = COALESCE(?11, customer_id),
                 subscription_id = COALESCE(?12, subscription_id), order_id = COALESCE(?13, order_id),
                 invoice_id = COALESCE(?14, invoice_id), payment_id = COALESCE(?15, payment_id),
                 refund_id = COALESCE(?16, refund_id), store_purchase_id = COALESCE(?17, store_purchase_id),
                 {extra}
             WHERE id = ?1"
        ),
        vec![
            id.into(),
            h.processing.map(WebhookProcessing::as_str).into(),
            now.into(),
            h.outcome_name().into(),
            h.detail.clone().into(),
            h.event_id.clone().into(),
            h.event_type.clone().into(),
            h.account.clone().into(),
            i64::try_from(latency.as_micros()).unwrap_or(i64::MAX).into(),
            l.webhook_event.clone().into(),
            l.customer.clone().into(),
            l.subscription.clone().into(),
            l.order.clone().into(),
            l.invoice.clone().into(),
            l.payment.clone().into(),
            l.refund.clone().into(),
            l.store_purchase.clone().into(),
        ],
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_allowlisted_headers_are_kept() {
        let h = vec![
            ("Authorization".to_owned(), "Bearer secret".to_owned()),
            ("cookie".to_owned(), "fb_session=x".to_owned()),
            ("stripe-signature".to_owned(), "t=1,v1=ab".to_owned()),
            ("x-forwarded-for".to_owned(), "203.0.113.1".to_owned()),
            ("x-forwarded-for".to_owned(), "10.0.0.1".to_owned()),
            ("content-type".to_owned(), "x".repeat(2000)),
        ];
        let kept = kept_headers(&h);
        assert!(!kept.contains_key("authorization") && !kept.contains_key("cookie"));
        assert_eq!(kept["stripe-signature"], "t=1,v1=ab");
        assert_eq!(kept["x-forwarded-for"], "203.0.113.1, 10.0.0.1");
        assert_eq!(kept["content-type"].as_str().unwrap().len(), MAX_HEADER_VALUE);
    }

    #[test]
    fn provider_names_are_labelled_and_stored_safely() {
        assert_eq!(label("stripe"), "stripe");
        assert_eq!(label("evil\"provider"), "other");
        assert_eq!(stored_provider("../etc/passwd"), ".._etc_passwd");
        assert_eq!(stored_provider(&"a".repeat(300)).len(), 64);
        assert_eq!(stored_provider(""), "_");
    }

    #[test]
    fn verification_errors_are_classified_without_secrets() {
        let (v, r) = classify(&BillingError::webhook(
            "invalid_signature",
            "the webhook signature does not match",
        ));
        assert_eq!(v, WebhookVerification::SignatureInvalid);
        assert_eq!(r, "invalid_signature: the webhook signature does not match");
        let (v, _) = classify(&BillingError::webhook("stale_signature", "old"));
        assert_eq!(v, WebhookVerification::TimestampOutOfTolerance);
        let (v, _) = classify(&BillingError::webhook("invalid_payload", "not JSON"));
        assert_eq!(v, WebhookVerification::Malformed);
        let (v, _) = classify(&BillingError::NotConfigured("x".into()));
        assert_eq!(v, WebhookVerification::UnknownProvider);
        // Upstream text is never copied.
        let (v, r) = classify(&BillingError::Provider("HTTP 500: key sk_test_123 is bad".into()));
        assert_eq!((v, r.as_str()), (WebhookVerification::Error, "provider_error"));
    }

    #[test]
    fn client_address_trusts_forwarding_only_when_asked() {
        let h = vec![("x-forwarded-for".to_owned(), "198.51.100.7, 203.0.113.9".to_owned())];
        let peer: SocketAddr = "10.1.2.3:4567".parse().unwrap();
        assert_eq!(client_addr(false, &h, Some(peer)).as_deref(), Some("10.1.2.3"));
        assert_eq!(client_addr(true, &h, Some(peer)).as_deref(), Some("203.0.113.9"));
        assert_eq!(client_addr(true, &[], None), None);
    }
}
