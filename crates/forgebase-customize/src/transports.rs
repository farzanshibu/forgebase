//! HTTP API mail transports (ADR-0039): Resend, Postmark, SendGrid, Mailgun
//! (US and EU regions) and Amazon SES v2 (SigV4, signed with
//! [`forgebase_auth::sigv4`]).
//!
//! They send over the auth service's [`HttpClient`] (rustls, with the same
//! SSRF guard as identity providers and SMS gateways), so a private base URL
//! is refused unless the operator allowed private outbound traffic. Each
//! sends one message per call and maps the answer:
//!
//! | Answer | Result |
//! |---|---|
//! | 2xx | [`SendReceipt`] with the provider's message id |
//! | 408, 429, 5xx, connection trouble | [`SendError::Transient`] (the queue retries with backoff) |
//! | 401, 403 | [`SendError::Permanent`]: the credentials were refused |
//! | any other 4xx | [`SendError::Permanent`], with the provider's own reason |
//!
//! `Idempotency-Key` is sent where the provider honours it (Resend), so a
//! retried send after a lost answer does not deliver twice. The realm's
//! support email travels as `Reply-To`. The existing mail model has no
//! attachments or custom headers; the transports add them when it does.

use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use forgebase_auth::sigv4::{self, Credentials, SignInput};
use forgebase_auth::social::{HttpClient, HttpRequest, HttpResponse, Secret};
use forgebase_db::backend::BoxFuture;
use serde_json::{Value as Json, json};

use crate::mail::{MailProvider, OutMail, SendError, SendReceipt};

/// The display name and address in a `From` header value as
/// [`crate::mail::from_header`] builds it (`"Name" <a@b>`, RFC 2047 encoded
/// name, or a bare address).
pub fn split_from(from: &str) -> (Option<String>, String) {
    let from = from.trim();
    let (Some(lt), true) = (from.rfind('<'), from.ends_with('>')) else {
        return (None, from.to_owned());
    };
    let addr = from[lt + 1..from.len() - 1].trim().to_owned();
    let raw = from[..lt].trim().trim_matches('"').trim();
    let name = match raw
        .strip_prefix("=?UTF-8?B?")
        .and_then(|r| r.strip_suffix("?="))
        .and_then(|b| STANDARD.decode(b).ok())
        .and_then(|b| String::from_utf8(b).ok())
    {
        Some(decoded) => decoded,
        None => raw.to_owned(),
    };
    ((!name.is_empty()).then_some(name), addr)
}

/// `Name <addr>` or `addr`, with nothing in the name that could end it early.
fn display_from(from: &str) -> String {
    match split_from(from) {
        (Some(n), a) => format!("{} <{a}>", n.replace(['<', '>', '"', '\\'], "")),
        (None, a) => a,
    }
}

fn excerpt(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(200).collect()
}

fn body_str(res: &HttpResponse, pointers: &[&str]) -> Option<String> {
    let v: Json = serde_json::from_slice(&res.body).ok()?;
    pointers
        .iter()
        .find_map(|p| v.pointer(p).and_then(Json::as_str))
        .map(excerpt)
}

/// Maps a non-2xx answer: 408/429/5xx retry, everything else is final.
fn failure(provider: &str, res: &HttpResponse, reason: Option<String>) -> SendError {
    let why = reason.map(|r| format!(": {r}")).unwrap_or_default();
    let status = res.status;
    match status {
        429 => {
            let after = res
                .header("retry-after")
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(|s| format!(" (retry after {s}s)"))
                .unwrap_or_default();
            SendError::Transient(format!("{provider} rate limit{after}{why}"))
        }
        408 | 425 | 500..=599 => SendError::Transient(format!("{provider} answered HTTP {status}{why}")),
        401 | 403 => SendError::Permanent(format!("{provider} refused the credentials (HTTP {status}){why}")),
        _ => SendError::Permanent(format!("{provider} rejected the message (HTTP {status}){why}")),
    }
}

async fn call(http: &dyn HttpClient, provider: &str, req: HttpRequest) -> Result<HttpResponse, SendError> {
    http.send(req)
        .await
        .map_err(|e| SendError::Transient(format!("{provider}: {e}")))
}

fn json_post(url: &str, body: &Json) -> HttpRequest {
    HttpRequest {
        method: "POST",
        url: url.to_owned(),
        headers: vec![("content-type".into(), "application/json".into())],
        body: body.to_string().into_bytes(),
    }
}

fn base(url: Option<String>, default: &str) -> String {
    url.unwrap_or_else(|| default.to_owned())
        .trim_end_matches('/')
        .to_owned()
}

macro_rules! debug_redacted {
    ($t:ident) => {
        impl fmt::Debug for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.debug_struct(stringify!($t)).finish_non_exhaustive()
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Resend
// ---------------------------------------------------------------------------

/// Resend (`POST /emails`, bearer API key).
pub struct ResendProvider {
    api_key: Secret,
    base_url: String,
    http: Arc<dyn HttpClient>,
}
debug_redacted!(ResendProvider);

impl ResendProvider {
    /// A Resend sender.
    pub fn new(api_key: impl Into<String>, base_url: Option<String>, http: Arc<dyn HttpClient>) -> Self {
        Self {
            api_key: Secret::new(api_key),
            base_url: base(base_url, "https://api.resend.com"),
            http,
        }
    }

    /// The request for `m`.
    pub fn request(&self, m: &OutMail) -> HttpRequest {
        let mut b = json!({"from": display_from(&m.from), "to": [m.to], "subject": m.subject, "text": m.text});
        if let Some(h) = &m.html {
            b["html"] = json!(h);
        }
        if let Some(r) = &m.reply_to {
            b["reply_to"] = json!(r);
        }
        json_post(&format!("{}/emails", self.base_url), &b)
            .header("authorization", format!("Bearer {}", self.api_key.expose()))
            // The delivery id: a retry after a lost answer is the same message.
            .header("idempotency-key", m.id.clone())
    }
}

impl MailProvider for ResendProvider {
    fn name(&self) -> &'static str {
        "resend"
    }

    fn send<'a>(&'a self, mail: &'a OutMail) -> BoxFuture<'a, Result<SendReceipt, SendError>> {
        Box::pin(async move {
            let res = call(&*self.http, "Resend", self.request(mail)).await?;
            if (200..300).contains(&res.status) {
                return Ok(SendReceipt {
                    message_id: body_str(&res, &["/id"]),
                });
            }
            Err(failure("Resend", &res, body_str(&res, &["/message", "/name"])))
        })
    }
}

// ---------------------------------------------------------------------------
// Postmark
// ---------------------------------------------------------------------------

/// Postmark (`POST /email`, `X-Postmark-Server-Token`).
pub struct PostmarkProvider {
    token: Secret,
    stream: Option<String>,
    base_url: String,
    http: Arc<dyn HttpClient>,
}
debug_redacted!(PostmarkProvider);

impl PostmarkProvider {
    /// A Postmark sender; `stream` is the message stream (default `outbound`).
    pub fn new(
        server_token: impl Into<String>,
        stream: Option<String>,
        base_url: Option<String>,
        http: Arc<dyn HttpClient>,
    ) -> Self {
        Self {
            token: Secret::new(server_token),
            stream,
            base_url: base(base_url, "https://api.postmarkapp.com"),
            http,
        }
    }

    /// The request for `m`.
    pub fn request(&self, m: &OutMail) -> HttpRequest {
        let mut b = json!({"From": display_from(&m.from), "To": m.to, "Subject": m.subject, "TextBody": m.text, "TrackOpens": false});
        if let Some(h) = &m.html {
            b["HtmlBody"] = json!(h);
        }
        if let Some(r) = &m.reply_to {
            b["ReplyTo"] = json!(r);
        }
        if let Some(s) = &self.stream {
            b["MessageStream"] = json!(s);
        }
        json_post(&format!("{}/email", self.base_url), &b)
            .header("x-postmark-server-token", self.token.expose().to_owned())
            .header("accept", "application/json")
    }
}

impl MailProvider for PostmarkProvider {
    fn name(&self) -> &'static str {
        "postmark"
    }

    fn send<'a>(&'a self, mail: &'a OutMail) -> BoxFuture<'a, Result<SendReceipt, SendError>> {
        Box::pin(async move {
            let res = call(&*self.http, "Postmark", self.request(mail)).await?;
            let code = serde_json::from_slice::<Json>(&res.body)
                .ok()
                .and_then(|v| v["ErrorCode"].as_i64());
            // Postmark answers 200 with `ErrorCode: 0`; any other code is a
            // refusal even on a 2xx (and 422 carries the reason).
            if (200..300).contains(&res.status) && code.unwrap_or(0) == 0 {
                return Ok(SendReceipt {
                    message_id: body_str(&res, &["/MessageID"]),
                });
            }
            let why = body_str(&res, &["/Message"]).map(|m| format!("code {}: {m}", code.unwrap_or(0)));
            if (200..300).contains(&res.status) {
                return Err(SendError::Permanent(format!(
                    "Postmark rejected the message ({})",
                    why.unwrap_or_default()
                )));
            }
            Err(failure("Postmark", &res, why))
        })
    }
}

// ---------------------------------------------------------------------------
// SendGrid
// ---------------------------------------------------------------------------

/// SendGrid v3 (`POST /v3/mail/send`, bearer API key). The id is the
/// `X-Message-Id` header of the `202`.
pub struct SendGridProvider {
    api_key: Secret,
    base_url: String,
    http: Arc<dyn HttpClient>,
}
debug_redacted!(SendGridProvider);

impl SendGridProvider {
    /// A SendGrid sender; `eu` uses `api.eu.sendgrid.com` (EU data residency).
    pub fn new(api_key: impl Into<String>, eu: bool, base_url: Option<String>, http: Arc<dyn HttpClient>) -> Self {
        let default = if eu {
            "https://api.eu.sendgrid.com"
        } else {
            "https://api.sendgrid.com"
        };
        Self {
            api_key: Secret::new(api_key),
            base_url: base(base_url, default),
            http,
        }
    }

    /// The request for `m`.
    pub fn request(&self, m: &OutMail) -> HttpRequest {
        let (name, addr) = split_from(&m.from);
        let mut from = json!({"email": addr});
        if let Some(n) = name {
            from["name"] = json!(n);
        }
        // `text/plain` must come first when both are present.
        let mut content = vec![json!({"type": "text/plain", "value": m.text})];
        if let Some(h) = &m.html {
            content.push(json!({"type": "text/html", "value": h}));
        }
        let mut b = json!({
            "personalizations": [{"to": [{"email": m.to}]}],
            "from": from, "subject": m.subject, "content": content,
            "tracking_settings": {"click_tracking": {"enable": false}, "open_tracking": {"enable": false}},
        });
        if let Some(r) = &m.reply_to {
            b["reply_to"] = json!({"email": r});
        }
        json_post(&format!("{}/v3/mail/send", self.base_url), &b)
            .header("authorization", format!("Bearer {}", self.api_key.expose()))
    }
}

impl MailProvider for SendGridProvider {
    fn name(&self) -> &'static str {
        "sendgrid"
    }

    fn send<'a>(&'a self, mail: &'a OutMail) -> BoxFuture<'a, Result<SendReceipt, SendError>> {
        Box::pin(async move {
            let res = call(&*self.http, "SendGrid", self.request(mail)).await?;
            if (200..300).contains(&res.status) {
                return Ok(SendReceipt {
                    message_id: res.header("x-message-id").map(excerpt),
                });
            }
            Err(failure("SendGrid", &res, body_str(&res, &["/errors/0/message"])))
        })
    }
}

// ---------------------------------------------------------------------------
// Mailgun
// ---------------------------------------------------------------------------

/// Mailgun (`POST /v3/{domain}/messages`, HTTP Basic `api:KEY`).
pub struct MailgunProvider {
    api_key: Secret,
    domain: String,
    base_url: String,
    http: Arc<dyn HttpClient>,
}
debug_redacted!(MailgunProvider);

impl MailgunProvider {
    /// A Mailgun sender for sending domain `domain`; `eu` uses `api.eu.mailgun.net`.
    pub fn new(
        api_key: impl Into<String>,
        domain: impl Into<String>,
        eu: bool,
        base_url: Option<String>,
        http: Arc<dyn HttpClient>,
    ) -> Self {
        let default = if eu {
            "https://api.eu.mailgun.net"
        } else {
            "https://api.mailgun.net"
        };
        Self {
            api_key: Secret::new(api_key),
            domain: domain.into(),
            base_url: base(base_url, default),
            http,
        }
    }

    /// The request for `m`.
    pub fn request(&self, m: &OutMail) -> HttpRequest {
        let from = display_from(&m.from);
        let mut fields: Vec<(&str, &str)> = vec![
            ("from", &from),
            ("to", &m.to),
            ("subject", &m.subject),
            ("text", &m.text),
            ("o:tracking", "no"),
        ];
        if let Some(h) = &m.html {
            fields.push(("html", h));
        }
        if let Some(r) = &m.reply_to {
            fields.push(("h:Reply-To", r));
        }
        HttpRequest::post_form(format!("{}/v3/{}/messages", self.base_url, self.domain), &fields).header(
            "authorization",
            format!("Basic {}", STANDARD.encode(format!("api:{}", self.api_key.expose()))),
        )
    }
}

impl MailProvider for MailgunProvider {
    fn name(&self) -> &'static str {
        "mailgun"
    }

    fn send<'a>(&'a self, mail: &'a OutMail) -> BoxFuture<'a, Result<SendReceipt, SendError>> {
        Box::pin(async move {
            let res = call(&*self.http, "Mailgun", self.request(mail)).await?;
            if (200..300).contains(&res.status) {
                return Ok(SendReceipt {
                    message_id: body_str(&res, &["/id"]),
                });
            }
            Err(failure("Mailgun", &res, body_str(&res, &["/message"])))
        })
    }
}

// ---------------------------------------------------------------------------
// Amazon SES v2
// ---------------------------------------------------------------------------

/// Amazon SES settings.
#[derive(Debug, Clone)]
pub struct SesSettings {
    /// IAM access key id.
    pub access_key_id: String,
    /// IAM secret access key.
    pub secret_access_key: String,
    /// Temporary-credential session token.
    pub session_token: Option<String>,
    /// SES region (`us-east-1`).
    pub region: String,
    /// SES configuration set to apply.
    pub configuration_set: Option<String>,
    /// Endpoint override (a VPC endpoint, tests).
    pub endpoint: Option<String>,
}

/// Amazon SES v2 (`POST /v2/email/outbound-emails`, SigV4 service `ses`).
pub struct SesProvider {
    cfg: SesSettings,
    secret: Secret,
    endpoint: String,
    host: String,
    clock: fn() -> i64,
    http: Arc<dyn HttpClient>,
}
debug_redacted!(SesProvider);

const SES_PATH: &str = "/v2/email/outbound-emails";

fn epoch_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

impl SesProvider {
    /// An SES sender; errors name the setting that is wrong.
    pub fn new(cfg: SesSettings, http: Arc<dyn HttpClient>) -> Result<Self, String> {
        let region_ok = !cfg.region.is_empty()
            && cfg
                .region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if cfg.access_key_id.is_empty() || cfg.secret_access_key.is_empty() || !region_ok {
            return Err("ses needs `access_key_id`, `secret_access_key` and a `region` such as us-east-1".into());
        }
        let endpoint = base(
            cfg.endpoint.clone(),
            &format!("https://email.{}.amazonaws.com", cfg.region),
        );
        let url = url::Url::parse(&endpoint).map_err(|_| "ses `endpoint` is not a URL".to_owned())?;
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() {
            return Err("ses `endpoint` must be an http(s) URL without credentials".into());
        }
        let host = match (url.host_str(), url.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_owned(),
            _ => return Err("ses `endpoint` has no host".into()),
        };
        Ok(Self {
            secret: Secret::new(cfg.secret_access_key.clone()),
            cfg,
            endpoint,
            host,
            clock: epoch_now,
            http,
        })
    }

    /// Replaces the clock used for the signature timestamp (tests).
    #[must_use]
    pub fn with_clock(mut self, clock: fn() -> i64) -> Self {
        self.clock = clock;
        self
    }

    /// The signed request for `m` at epoch second `now`.
    pub fn request(&self, m: &OutMail, now: i64) -> HttpRequest {
        let part = |s: &str| json!({"Data": s, "Charset": "UTF-8"});
        let mut body = json!({"Text": part(&m.text)});
        if let Some(h) = &m.html {
            body["Html"] = part(h);
        }
        let mut b = json!({
            // The header form (an RFC 2047 encoded name for non-ASCII) is what SES wants.
            "FromEmailAddress": m.from,
            "Destination": {"ToAddresses": [m.to]},
            "Content": {"Simple": {"Subject": part(&m.subject), "Body": body}},
        });
        if let Some(r) = &m.reply_to {
            b["ReplyToAddresses"] = json!([r]);
        }
        if let Some(c) = &self.cfg.configuration_set {
            b["ConfigurationSetName"] = json!(c);
        }
        let mut req = json_post(&format!("{}{SES_PATH}", self.endpoint), &b);
        let creds = Credentials {
            access_key: self.cfg.access_key_id.clone(),
            secret_key: self.secret.expose().to_owned(),
            session_token: self.cfg.session_token.clone(),
            region: self.cfg.region.clone(),
            service: "ses".into(),
        };
        let signed = sigv4::sign(
            &creds,
            &SignInput {
                method: "POST",
                host: &self.host,
                path: SES_PATH,
                query: &[],
                headers: &[("content-type".into(), "application/json".into())],
                payload_sha256: &sigv4::sha256_hex(&req.body),
                now,
            },
        );
        for (k, v) in signed {
            req = req.header(&k, v);
        }
        req
    }
}

impl MailProvider for SesProvider {
    fn name(&self) -> &'static str {
        "ses"
    }

    fn send<'a>(&'a self, mail: &'a OutMail) -> BoxFuture<'a, Result<SendReceipt, SendError>> {
        Box::pin(async move {
            let res = call(&*self.http, "Amazon SES", self.request(mail, (self.clock)())).await?;
            if (200..300).contains(&res.status) {
                return Ok(SendReceipt {
                    message_id: body_str(&res, &["/MessageId"]),
                });
            }
            // `x-amzn-ErrorType: MessageRejected:http://…` names the failure.
            let kind = res
                .header("x-amzn-errortype")
                .map(|v| excerpt(v.split(':').next().unwrap_or_default()));
            let msg = body_str(&res, &["/message", "/Message"]);
            let why = match (kind.clone(), msg) {
                (Some(k), Some(m)) => Some(format!("{k}: {m}")),
                (k, m) => k.or(m),
            };
            // Throttling and sending-rate errors arrive as 400 on some paths.
            let throttled = kind.as_deref().is_some_and(|k| {
                matches!(
                    k,
                    "TooManyRequestsException" | "ThrottlingException" | "Throttling" | "RequestLimitExceeded"
                )
            });
            if throttled && res.status < 500 && res.status != 429 {
                return Err(SendError::Transient(format!(
                    "Amazon SES rate limit{}",
                    why.map(|w| format!(": {w}")).unwrap_or_default()
                )));
            }
            Err(failure("Amazon SES", &res, why))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_header_round_trips() {
        assert_eq!(
            split_from("\"Acme Inc\" <no-reply@acme.test>"),
            (Some("Acme Inc".into()), "no-reply@acme.test".into())
        );
        assert_eq!(split_from("no-reply@acme.test"), (None, "no-reply@acme.test".into()));
        let enc = crate::mail::from_header("Zoë & Co", "a@b.com");
        assert_eq!(split_from(&enc), (Some("Zoë & Co".into()), "a@b.com".into()));
        // A name cannot end the address early.
        assert_eq!(display_from("\"A\" <a@b.com>"), "A <a@b.com>");
    }
}
