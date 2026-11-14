//! Pieces shared by the native cloud drivers ([`crate::gcs`], [`crate::azure`]):
//! retry with backoff, an OAuth token cache, PEM and RSA helpers, XML text
//! extraction and HTTP dates.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use forgebase_auth::social::HttpResponse;
use ring::rand::SystemRandom;
use ring::signature::{self, RsaKeyPair};
use tokio::sync::Mutex;

use crate::error::{Result, StorageError};

/// Epoch seconds from the wall clock.
pub fn system_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

/// An injectable clock (epoch seconds).
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The wall clock as a [`Clock`].
pub fn system_clock() -> Clock {
    Arc::new(system_now)
}

/// How requests are retried: transport failures and `408`, `429`, `500`,
/// `502`, `503`, `504` answers, with exponential backoff (and `Retry-After`).
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Attempts in total (1 = no retry).
    pub max_attempts: u32,
    /// Delay before the second attempt; doubled for each further one.
    pub base_delay: Duration,
    /// Longest wait between two attempts.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(10),
        }
    }
}

impl RetryPolicy {
    /// No retries.
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    /// The wait before attempt `attempt + 1` (`attempt` counts from 1).
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let exp = self
            .base_delay
            .saturating_mul(1u32.checked_shl(attempt.saturating_sub(1)).unwrap_or(u32::MAX));
        let mut d = exp.min(self.max_delay);
        if let Some(ra) = retry_after {
            d = d.max(ra.min(self.max_delay));
        }
        d
    }
}

/// Whether an HTTP status is worth retrying.
pub fn retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

/// `Retry-After` in seconds, if the response carries one.
pub fn retry_after(resp: &HttpResponse) -> Option<Duration> {
    resp.header("retry-after")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Runs `attempt` until it yields a response that is not retryable, or the
/// attempts are used up (then the last answer, or the last transport error,
/// is returned). `attempt` builds, signs and sends a fresh request each time.
pub async fn with_retry<F, Fut>(policy: &RetryPolicy, mut attempt: F) -> Result<HttpResponse>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<HttpResponse>>,
{
    let max = policy.max_attempts.max(1);
    let mut n = 1;
    loop {
        let r = attempt().await;
        let wait = match &r {
            Ok(resp) if retryable_status(resp.status) => Some(policy.delay(n, retry_after(resp))),
            Err(StorageError::Driver(_)) => Some(policy.delay(n, None)),
            _ => None,
        };
        match wait {
            Some(w) if n < max => {
                tokio::time::sleep(w).await;
                n += 1;
            }
            _ => return r,
        }
    }
}

/// An access token and when it stops being valid (epoch seconds).
#[derive(Debug, Clone)]
pub struct Token {
    /// The bearer token.
    pub value: String,
    /// Expiry, epoch seconds.
    pub expires_at: i64,
}

/// Seconds before expiry at which a token is replaced.
const REFRESH_SKEW: i64 = 120;

/// A cached bearer token, refreshed shortly before it expires. Concurrent
/// callers share one refresh.
#[derive(Debug, Default)]
pub struct TokenCache {
    slot: Mutex<Option<Token>>,
}

impl TokenCache {
    /// The cached token, or the result of `fetch` when there is none or it
    /// is about to expire.
    pub async fn get<F, Fut>(&self, now: i64, fetch: F) -> Result<String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Token>>,
    {
        let mut slot = self.slot.lock().await;
        if let Some(t) = slot.as_ref().filter(|t| t.expires_at - REFRESH_SKEW > now) {
            return Ok(t.value.clone());
        }
        let t = fetch().await?;
        let v = t.value.clone();
        *slot = Some(t);
        Ok(v)
    }

    /// Drops the cached token (after a `401`).
    pub async fn invalidate(&self) {
        *self.slot.lock().await = None;
    }
}

/// Reads the `access_token` / `expires_in` pair of an OAuth or metadata
/// server answer. `expires_in` may be a number or a numeric string.
pub fn parse_token(resp: &HttpResponse, what: &str, now: i64) -> Result<Token> {
    if !(200..300).contains(&resp.status) {
        return Err(StorageError::Driver(format!(
            "{what} returned {}: {}",
            resp.status,
            error_summary(&resp.body)
        )));
    }
    let json: serde_json::Value = serde_json::from_slice(&resp.body)
        .map_err(|_| StorageError::Driver(format!("{what} returned a body that is not JSON")))?;
    let value = json
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| StorageError::Driver(format!("{what} returned no access_token")))?
        .to_owned();
    let secs = json
        .get("expires_in")
        .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
        .unwrap_or(3600);
    Ok(Token {
        value,
        expires_at: now + secs,
    })
}

/// A short, safe description of an error body (never the whole body).
pub fn error_summary(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
        let msg = v
            .pointer("/error/message")
            .or_else(|| v.get("error_description"))
            .or_else(|| v.get("error"))
            .and_then(serde_json::Value::as_str);
        if let Some(m) = msg {
            return truncate(m);
        }
    }
    if let Some(c) = xml_tag(&text, "Code") {
        return truncate(&c);
    }
    truncate(text.trim())
}

fn truncate(s: &str) -> String {
    s.chars().take(200).collect()
}

/// The contents of the PEM block `label` in `text` (or a bare base64 body).
pub fn pem_der(text: &str, label: &str) -> Option<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let body = match text.find(&begin) {
        Some(s) => {
            let rest = &text[s + begin.len()..];
            &rest[..rest.find(&end)?]
        }
        None => text,
    };
    let bare: String = body.split_whitespace().collect();
    base64::engine::general_purpose::STANDARD.decode(bare.as_bytes()).ok()
}

/// An RSA private key that signs RSASSA-PKCS1-v1_5 / SHA-256.
pub struct RsaSigner {
    key: RsaKeyPair,
}

impl std::fmt::Debug for RsaSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RsaSigner(..)")
    }
}

impl RsaSigner {
    /// From a PKCS#8 PEM (`-----BEGIN PRIVATE KEY-----`), as Google's
    /// service-account JSON keys carry it.
    pub fn from_pem(pem: &str) -> Result<Self> {
        let der = pem_der(pem, "PRIVATE KEY")
            .ok_or_else(|| StorageError::invalid("invalid_gcs_key", "the private key is not a PEM PKCS#8 key"))?;
        let key = RsaKeyPair::from_pkcs8(&der)
            .map_err(|_| StorageError::invalid("invalid_gcs_key", "the private key is not a PKCS#8 RSA key"))?;
        Ok(Self { key })
    }

    /// Signs `msg` (hashed with SHA-256 first).
    pub fn sign(&self, msg: &[u8]) -> Result<Vec<u8>> {
        let mut sig = vec![0u8; self.key.public().modulus_len()];
        self.key
            .sign(&signature::RSA_PKCS1_SHA256, &SystemRandom::new(), msg, &mut sig)
            .map_err(|_| StorageError::Driver("RSA signing failed".into()))?;
        Ok(sig)
    }
}

/// Base64url without padding.
pub fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Standard base64 with padding.
pub fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decodes standard base64.
pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(s.trim().as_bytes())
        .ok()
}

/// Escapes text for XML.
pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Text of the first `<name>...</name>` in `xml`.
pub fn xml_tag(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml_unescape(&xml[start..end]))
}

/// The inner text of every `<name>...</name>` block in `xml`.
pub fn xml_blocks<'a>(xml: &'a str, name: &str) -> Vec<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(s) = rest.find(&open) {
        let after = &rest[s + open.len()..];
        let Some(e) = after.find(&close) else { break };
        out.push(&after[..e]);
        rest = &after[e + close.len()..];
    }
    out
}

const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// An RFC 1123 date (`Fri, 26 Jun 2015 23:39:12 GMT`) for epoch seconds.
pub fn http_date(epoch_secs: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(epoch_secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{}, {:02} {} {:04} {:02}:{:02}:{:02} GMT",
        DAYS[usize::from(t.weekday().number_days_from_monday())],
        t.day(),
        MONTHS[usize::from(u8::from(t.month())) - 1],
        t.year(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// Epoch milliseconds of an RFC 1123 date; `0` when it does not parse.
pub fn parse_http_date_ms(s: &str) -> i64 {
    let mut it = s.split_whitespace();
    let _ = it.next();
    let (Some(day), Some(mon), Some(year), Some(hms)) = (it.next(), it.next(), it.next(), it.next()) else {
        return 0;
    };
    let (Ok(day), Ok(year)) = (day.parse::<u8>(), year.parse::<i32>()) else {
        return 0;
    };
    let Some(mi) = MONTHS.iter().position(|m| m.eq_ignore_ascii_case(mon)) else {
        return 0;
    };
    let mut p = hms.split(':').map(|x| x.parse::<u8>().ok());
    let (Some(Some(h)), Some(Some(m)), Some(Some(sec))) = (p.next(), p.next(), p.next()) else {
        return 0;
    };
    let Ok(month) = time::Month::try_from(u8::try_from(mi + 1).unwrap_or(1)) else {
        return 0;
    };
    let date = time::Date::from_calendar_date(year, month, day);
    let tm = time::Time::from_hms(h, m, sec);
    match (date, tm) {
        (Ok(d), Ok(t)) => time::PrimitiveDateTime::new(d, t)
            .assume_utc()
            .unix_timestamp()
            .saturating_mul(1000),
        _ => 0,
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` for epoch seconds.
pub fn iso_z(epoch_secs: i64) -> String {
    let t = time::OffsetDateTime::from_unix_timestamp(epoch_secs).unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    )
}

/// Epoch milliseconds of an RFC 3339 timestamp; `0` when it does not parse.
pub fn parse_rfc3339_ms(s: &str) -> i64 {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .map_or(0, |t| i64::try_from(t.unix_timestamp_nanos() / 1_000_000).unwrap_or(0))
}

/// Strips leading and trailing `/` from a key prefix.
pub fn clean_prefix(p: &str) -> String {
    p.trim_matches('/').to_owned()
}

/// Joins an optional prefix and a key.
pub fn join_key(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_owned()
    } else {
        format!("{prefix}/{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_and_honours_retry_after() {
        let p = RetryPolicy {
            max_attempts: 5,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
        };
        assert_eq!(p.delay(1, None), Duration::from_millis(100));
        assert_eq!(p.delay(2, None), Duration::from_millis(200));
        assert_eq!(p.delay(4, None), Duration::from_millis(500), "capped");
        assert_eq!(p.delay(1, Some(Duration::from_millis(300))), Duration::from_millis(300));
        assert_eq!(p.delay(40, None), Duration::from_millis(500), "no overflow");
    }

    #[test]
    fn http_dates_round_trip() {
        // The date of Microsoft's documented Shared Key example.
        assert_eq!(http_date(1_435_361_952), "Fri, 26 Jun 2015 23:39:12 GMT");
        assert_eq!(parse_http_date_ms("Fri, 26 Jun 2015 23:39:12 GMT"), 1_435_361_952_000);
        assert_eq!(parse_http_date_ms("garbage"), 0);
        assert_eq!(parse_rfc3339_ms("2026-11-09T12:00:00.000Z"), 1_794_225_600_000);
        assert_eq!(iso_z(1_794_225_600), "2026-11-09T12:00:00Z");
    }

    #[test]
    fn xml_helpers() {
        let x = "<A><B>1&amp;2</B><C><N>x</N></C><C><N>y</N></C></A>";
        assert_eq!(xml_tag(x, "B").as_deref(), Some("1&2"));
        assert_eq!(xml_blocks(x, "C"), vec!["<N>x</N>", "<N>y</N>"]);
        assert_eq!(xml_escape("a<b&\"c\""), "a&lt;b&amp;&quot;c&quot;");
    }

    #[test]
    fn tokens_parse_numbers_and_strings() {
        let r = |body: &str| HttpResponse {
            status: 200,
            headers: vec![],
            body: body.as_bytes().to_vec(),
        };
        let t = parse_token(&r(r#"{"access_token":"a","expires_in":3599}"#), "x", 100).unwrap();
        assert_eq!((t.value.as_str(), t.expires_at), ("a", 3699));
        let t = parse_token(&r(r#"{"access_token":"a","expires_in":"60"}"#), "x", 100).unwrap();
        assert_eq!(t.expires_at, 160);
        assert!(parse_token(&r("{}"), "x", 0).is_err());
    }
}
