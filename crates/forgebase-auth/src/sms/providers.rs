//! SMS gateways beyond Twilio and the generic webhook (ADR-0039): Vonage
//! (Messages API), MessageBird, Bird (the MessageBird successor), Amazon SNS
//! (SigV4) and Plivo. Each validates the recipient as E.164, sends one
//! request through the auth service's HTTPS client (so the SSRF guard
//! applies), maps the gateway's answer to [`SmsError`] and returns the
//! gateway's message id.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{Value as Json, json};

use super::gateway::{Gateway, SmsError, SmsReceipt, check_e164, classify_status, json_detail, xml_tag};
use super::{Sms, SmsFuture, SmsProvider};
use crate::AuthError;
use crate::sigv4::{self, Credentials, SignInput};
use crate::social::{HttpClient, HttpRequest, HttpResponse, Secret};

fn basic(user: &str, pass: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"))
    )
}

fn json_post(url: &str, body: &Json) -> HttpRequest {
    HttpRequest {
        method: "POST",
        url: url.to_owned(),
        headers: vec![("content-type".into(), "application/json".into())],
        body: body.to_string().into_bytes(),
    }
}

fn str_at(res: &HttpResponse, pointer: &str) -> Option<String> {
    serde_json::from_slice::<Json>(&res.body)
        .ok()?
        .pointer(pointer)
        .and_then(Json::as_str)
        .map(str::to_owned)
}

fn base(url: Option<String>, default: &str) -> String {
    url.unwrap_or_else(|| default.to_owned())
        .trim_end_matches('/')
        .to_owned()
}

// ---------------------------------------------------------------------------
// Vonage
// ---------------------------------------------------------------------------

/// Vonage Messages API (`POST /v1/messages`, HTTP Basic with the API key and
/// secret). Vonage wants the number without the leading `+`.
#[derive(Debug)]
pub struct VonageSms {
    api_key: String,
    api_secret: Secret,
    from: String,
    base_url: String,
    gw: Gateway,
}

impl VonageSms {
    /// A Vonage sender; `from` is a number or an alphanumeric sender id.
    pub fn new(
        api_key: impl Into<String>,
        api_secret: impl Into<String>,
        from: impl Into<String>,
        base_url: Option<String>,
        http: Arc<dyn HttpClient>,
    ) -> Result<Self, AuthError> {
        let (api_key, from) = (api_key.into(), from.into());
        if api_key.is_empty() || from.is_empty() {
            return Err(AuthError::invalid("vonage needs `api_key`, `api_secret` and `from`"));
        }
        Ok(Self {
            api_key,
            api_secret: Secret::new(api_secret),
            from: from.trim_start_matches('+').to_owned(),
            base_url: base(base_url, "https://api.nexmo.com"),
            gw: Gateway::new(http),
        })
    }

    /// Replaces the gateway (retry policy).
    #[must_use]
    pub fn with_gateway(mut self, gw: Gateway) -> Self {
        self.gw = gw;
        self
    }
}

impl SmsProvider for VonageSms {
    fn send<'a>(&'a self, sms: &'a Sms) -> SmsFuture<'a> {
        Box::pin(async move {
            check_e164(&sms.to)?;
            let body = json!({
                "message_type": "text", "channel": "sms",
                "to": sms.to.trim_start_matches('+'), "from": self.from, "text": sms.text,
            });
            let url = format!("{}/v1/messages", self.base_url);
            self.gw
                .call(
                    || json_post(&url, &body).header("authorization", basic(&self.api_key, self.api_secret.expose())),
                    |res| {
                        // 422 `Invalid params` names the offending field.
                        classify_status(res, || json_detail(res, &["/detail", "/title"]))
                            .map_err(|e| invalid_number_if(e, res, &["/detail"], "to"))?;
                        Ok(SmsReceipt {
                            id: str_at(res, "/message_uuid"),
                        })
                    },
                )
                .await
        })
    }
}

/// A rejection whose detail points at the recipient field becomes
/// [`SmsError::InvalidRecipient`].
fn invalid_number_if(e: SmsError, res: &HttpResponse, pointers: &[&str], needle: &str) -> SmsError {
    match e {
        SmsError::Rejected(m) => {
            let d = pointers.iter().find_map(|p| str_at(res, p)).unwrap_or_default();
            if d.to_ascii_lowercase().contains(needle) {
                SmsError::InvalidRecipient(m)
            } else {
                SmsError::Rejected(m)
            }
        }
        other => other,
    }
}

// ---------------------------------------------------------------------------
// MessageBird and Bird
// ---------------------------------------------------------------------------

/// MessageBird REST API (`POST /messages`, `Authorization: AccessKey …`).
#[derive(Debug)]
pub struct MessageBirdSms {
    access_key: Secret,
    originator: String,
    base_url: String,
    gw: Gateway,
}

impl MessageBirdSms {
    /// A MessageBird sender; `originator` is a number or a sender id (≤ 11 characters).
    pub fn new(
        access_key: impl Into<String>,
        originator: impl Into<String>,
        base_url: Option<String>,
        http: Arc<dyn HttpClient>,
    ) -> Result<Self, AuthError> {
        let originator = originator.into();
        if originator.is_empty() {
            return Err(AuthError::invalid("messagebird needs `api_key` and `from`"));
        }
        Ok(Self {
            access_key: Secret::new(access_key),
            originator,
            base_url: base(base_url, "https://rest.messagebird.com"),
            gw: Gateway::new(http),
        })
    }

    /// Replaces the gateway (retry policy).
    #[must_use]
    pub fn with_gateway(mut self, gw: Gateway) -> Self {
        self.gw = gw;
        self
    }
}

/// MessageBird's numeric error codes (`errors[0].code`).
fn messagebird_error(res: &HttpResponse) -> SmsError {
    let first = serde_json::from_slice::<Json>(&res.body)
        .ok()
        .and_then(|v| v.pointer("/errors/0").cloned());
    let code = first.as_ref().and_then(|e| e["code"].as_i64()).unwrap_or(0);
    let text: String = first
        .as_ref()
        .and_then(|e| e["description"].as_str())
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    let m = format!("HTTP {} code {code}: {text}", res.status);
    match code {
        2 | 98 => SmsError::Auth(m),
        9 | 21 => SmsError::InvalidRecipient(m),
        25 => SmsError::RateLimited {
            retry_after_secs: super::gateway::retry_after(res),
        },
        _ => SmsError::Rejected(m),
    }
}

impl SmsProvider for MessageBirdSms {
    fn send<'a>(&'a self, sms: &'a Sms) -> SmsFuture<'a> {
        Box::pin(async move {
            check_e164(&sms.to)?;
            let body = json!({"originator": self.originator, "recipients": [sms.to.trim_start_matches('+')], "body": sms.text});
            let url = format!("{}/messages", self.base_url);
            self.gw
                .call(
                    || {
                        json_post(&url, &body)
                            .header("authorization", format!("AccessKey {}", self.access_key.expose()))
                    },
                    |res| match res.status {
                        200..=299 => Ok(SmsReceipt { id: str_at(res, "/id") }),
                        // 422 carries MessageBird's own codes; 401/429/5xx the generic ones.
                        422 | 400 => Err(messagebird_error(res)),
                        _ => classify_status(res, || json_detail(res, &["/errors/0/description"]))
                            .map(|()| SmsReceipt::default()),
                    },
                )
                .await
        })
    }
}

/// Bird (the MessageBird successor): `POST /workspaces/{ws}/channels/{ch}/messages`.
#[derive(Debug)]
pub struct BirdSms {
    access_key: Secret,
    workspace_id: String,
    channel_id: String,
    base_url: String,
    gw: Gateway,
}

impl BirdSms {
    /// A Bird sender for one SMS channel of a workspace.
    pub fn new(
        access_key: impl Into<String>,
        workspace_id: impl Into<String>,
        channel_id: impl Into<String>,
        base_url: Option<String>,
        http: Arc<dyn HttpClient>,
    ) -> Result<Self, AuthError> {
        let (workspace_id, channel_id) = (workspace_id.into(), channel_id.into());
        let id_ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !id_ok(&workspace_id) || !id_ok(&channel_id) {
            return Err(AuthError::invalid(
                "bird needs `api_key`, `workspace_id` and `channel_id` (letters, digits, dashes)",
            ));
        }
        Ok(Self {
            access_key: Secret::new(access_key),
            workspace_id,
            channel_id,
            base_url: base(base_url, "https://api.bird.com"),
            gw: Gateway::new(http),
        })
    }

    /// Replaces the gateway (retry policy).
    #[must_use]
    pub fn with_gateway(mut self, gw: Gateway) -> Self {
        self.gw = gw;
        self
    }
}

impl SmsProvider for BirdSms {
    fn send<'a>(&'a self, sms: &'a Sms) -> SmsFuture<'a> {
        Box::pin(async move {
            check_e164(&sms.to)?;
            let body = json!({
                "receiver": {"contacts": [{"identifierKey": "phonenumber", "identifierValue": sms.to}]},
                "body": {"type": "text", "text": {"text": sms.text}},
            });
            let url = format!(
                "{}/workspaces/{}/channels/{}/messages",
                self.base_url, self.workspace_id, self.channel_id
            );
            self.gw
                .call(
                    || {
                        json_post(&url, &body)
                            .header("authorization", format!("AccessKey {}", self.access_key.expose()))
                    },
                    |res| {
                        classify_status(res, || json_detail(res, &["/message", "/code"]))
                            .map_err(|e| invalid_number_if(e, res, &["/message"], "phone"))?;
                        Ok(SmsReceipt { id: str_at(res, "/id") })
                    },
                )
                .await
        })
    }
}

// ---------------------------------------------------------------------------
// Plivo
// ---------------------------------------------------------------------------

/// Plivo (`POST /v1/Account/{auth_id}/Message/`, HTTP Basic with the auth id and token).
#[derive(Debug)]
pub struct PlivoSms {
    auth_id: String,
    auth_token: Secret,
    from: String,
    base_url: String,
    gw: Gateway,
}

impl PlivoSms {
    /// A Plivo sender; `from` is a Plivo number or a registered sender id.
    pub fn new(
        auth_id: impl Into<String>,
        auth_token: impl Into<String>,
        from: impl Into<String>,
        base_url: Option<String>,
        http: Arc<dyn HttpClient>,
    ) -> Result<Self, AuthError> {
        let (auth_id, from) = (auth_id.into(), from.into());
        if auth_id.is_empty() || !auth_id.bytes().all(|b| b.is_ascii_alphanumeric()) || from.is_empty() {
            return Err(AuthError::invalid(
                "plivo needs `auth_id` (letters and digits), `auth_token` and `from`",
            ));
        }
        Ok(Self {
            auth_id,
            auth_token: Secret::new(auth_token),
            from,
            base_url: base(base_url, "https://api.plivo.com"),
            gw: Gateway::new(http),
        })
    }

    /// Replaces the gateway (retry policy).
    #[must_use]
    pub fn with_gateway(mut self, gw: Gateway) -> Self {
        self.gw = gw;
        self
    }
}

impl SmsProvider for PlivoSms {
    fn send<'a>(&'a self, sms: &'a Sms) -> SmsFuture<'a> {
        Box::pin(async move {
            check_e164(&sms.to)?;
            let body = json!({"src": self.from, "dst": sms.to.trim_start_matches('+'), "text": sms.text});
            let url = format!("{}/v1/Account/{}/Message/", self.base_url, self.auth_id);
            self.gw
                .call(
                    || json_post(&url, &body).header("authorization", basic(&self.auth_id, self.auth_token.expose())),
                    |res| {
                        classify_status(res, || json_detail(res, &["/error"]))
                            .map_err(|e| invalid_number_if(e, res, &["/error"], "dst"))?;
                        // `message_uuid` is a list (one entry per destination).
                        let id = serde_json::from_slice::<Json>(&res.body)
                            .ok()
                            .and_then(|v| v["message_uuid"][0].as_str().map(str::to_owned));
                        Ok(SmsReceipt { id })
                    },
                )
                .await
        })
    }
}

// ---------------------------------------------------------------------------
// Amazon SNS
// ---------------------------------------------------------------------------

/// Amazon SNS direct-to-phone `Publish` (Query API, SigV4 service `sns`).
#[derive(Debug)]
pub struct SnsSms {
    creds_access: String,
    creds_secret: Secret,
    session_token: Option<Secret>,
    region: String,
    sender_id: Option<String>,
    sms_type: String,
    endpoint: String,
    host: String,
    path: String,
    gw: Gateway,
    clock: fn() -> i64,
}

/// SNS settings.
#[derive(Debug, Clone)]
pub struct SnsConfig {
    /// IAM access key id.
    pub access_key_id: String,
    /// IAM secret access key.
    pub secret_access_key: String,
    /// Temporary-credential session token.
    pub session_token: Option<String>,
    /// AWS region with SMS enabled (`us-east-1`).
    pub region: String,
    /// Alphanumeric sender id (not supported in every country).
    pub sender_id: Option<String>,
    /// `Transactional` (default) or `Promotional`.
    pub sms_type: Option<String>,
    /// Endpoint override (a VPC endpoint, LocalStack, tests).
    pub endpoint: Option<String>,
}

impl SnsSms {
    /// An SNS sender.
    pub fn new(c: SnsConfig, http: Arc<dyn HttpClient>) -> Result<Self, AuthError> {
        let region_ok = !c.region.is_empty()
            && c.region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if c.access_key_id.is_empty() || c.secret_access_key.is_empty() || !region_ok {
            return Err(AuthError::invalid(
                "sns needs `access_key_id`, `secret_access_key` and a `region` such as us-east-1",
            ));
        }
        let sms_type = c.sms_type.unwrap_or_else(|| "Transactional".into());
        if sms_type != "Transactional" && sms_type != "Promotional" {
            return Err(AuthError::invalid("sns `sms_type` is Transactional or Promotional"));
        }
        if let Some(s) = &c.sender_id
            && !(1..=11).contains(&s.len())
        {
            return Err(AuthError::invalid("sns `sender_id` is 1-11 characters"));
        }
        let endpoint = base(c.endpoint, &format!("https://sns.{}.amazonaws.com", c.region));
        let url = url::Url::parse(&endpoint).map_err(|_| AuthError::invalid("sns `endpoint` is not a URL"))?;
        let host = match (url.host_str(), url.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_owned(),
            _ => return Err(AuthError::invalid("sns `endpoint` has no host")),
        };
        let path = if url.path().is_empty() {
            "/".to_owned()
        } else {
            url.path().to_owned()
        };
        Ok(Self {
            creds_access: c.access_key_id,
            creds_secret: Secret::new(c.secret_access_key),
            session_token: c.session_token.map(Secret::new),
            region: c.region,
            sender_id: c.sender_id,
            sms_type,
            endpoint,
            host,
            path,
            gw: Gateway::new(http),
            clock: crate::store::now,
        })
    }

    /// Replaces the gateway (retry policy).
    #[must_use]
    pub fn with_gateway(mut self, gw: Gateway) -> Self {
        self.gw = gw;
        self
    }

    /// Replaces the clock used for the signature timestamp (tests).
    #[must_use]
    pub fn with_clock(mut self, clock: fn() -> i64) -> Self {
        self.clock = clock;
        self
    }

    /// The signed `Publish` request for `sms` at epoch second `now`.
    pub fn request(&self, sms: &Sms, now: i64) -> HttpRequest {
        let mut fields: Vec<(&str, &str)> = vec![
            ("Action", "Publish"),
            ("Version", "2010-03-31"),
            ("PhoneNumber", &sms.to),
            ("Message", &sms.text),
            ("MessageAttributes.entry.1.Name", "AWS.SNS.SMS.SMSType"),
            ("MessageAttributes.entry.1.Value.DataType", "String"),
            ("MessageAttributes.entry.1.Value.StringValue", &self.sms_type),
        ];
        if let Some(s) = &self.sender_id {
            fields.extend([
                ("MessageAttributes.entry.2.Name", "AWS.SNS.SMS.SenderID"),
                ("MessageAttributes.entry.2.Value.DataType", "String"),
                ("MessageAttributes.entry.2.Value.StringValue", s.as_str()),
            ]);
        }
        let mut req = HttpRequest::post_form(&self.endpoint, &fields);
        let c = Credentials {
            access_key: self.creds_access.clone(),
            secret_key: self.creds_secret.expose().to_owned(),
            session_token: self.session_token.as_ref().map(|t| t.expose().to_owned()),
            region: self.region.clone(),
            service: "sns".into(),
        };
        let signed = sigv4::sign(
            &c,
            &SignInput {
                method: "POST",
                host: &self.host,
                path: &self.path,
                query: &[],
                headers: &[("content-type".into(), "application/x-www-form-urlencoded".into())],
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

/// SNS error codes (`<Error><Code>`), 4xx and 5xx alike.
fn sns_error(res: &HttpResponse) -> SmsError {
    let code = xml_tag(&res.body, "Code").unwrap_or_default();
    let msg = xml_tag(&res.body, "Message").unwrap_or_default();
    let m = format!("HTTP {} {code}: {msg}", res.status);
    match code.as_str() {
        "Throttling" | "ThrottledException" | "ThrottlingException" | "RequestLimitExceeded" => SmsError::RateLimited {
            retry_after_secs: super::gateway::retry_after(res),
        },
        "InvalidClientTokenId"
        | "SignatureDoesNotMatch"
        | "AuthorizationError"
        | "AccessDenied"
        | "ExpiredToken"
        | "UnrecognizedClientException"
        | "MissingAuthenticationToken" => SmsError::Auth(m),
        "InvalidParameter" | "InvalidParameterValue" if msg.contains("PhoneNumber") => SmsError::InvalidRecipient(m),
        "ServiceUnavailable" | "InternalFailure" | "InternalError" | "KMSThrottling" => SmsError::Transient(m),
        _ if res.status >= 500 => SmsError::Transient(m),
        _ if res.status == 429 => SmsError::RateLimited {
            retry_after_secs: super::gateway::retry_after(res),
        },
        _ => SmsError::Rejected(m),
    }
}

impl SmsProvider for SnsSms {
    fn send<'a>(&'a self, sms: &'a Sms) -> SmsFuture<'a> {
        Box::pin(async move {
            check_e164(&sms.to)?;
            // Built once per attempt: the signature carries the time.
            self.gw
                .call(
                    || self.request(sms, (self.clock)()),
                    |res| match res.status {
                        200..=299 => Ok(SmsReceipt {
                            id: xml_tag(&res.body, "MessageId"),
                        }),
                        _ => Err(sns_error(res)),
                    },
                )
                .await
        })
    }
}
