//! Outgoing SMS (FB-25, ADR-0015 §4, ADR-0039): the pluggable [`SmsProvider`]
//! trait, a development provider, and gateways over the auth service's HTTPS
//! client: Twilio, a generic JSON webhook, Vonage, MessageBird, Bird, Amazon
//! SNS and Plivo (the last five in [`providers`]).
//!
//! The provider comes from `<data_dir>/auth/sms.json` when present (it holds
//! gateway credentials; keep it 0600), or per realm from the REST API where
//! the secrets are sealed with the keyring:
//!
//! ```json
//! {"provider": "twilio", "account_sid": "AC…", "auth_token": "…", "from": "+15550100"}
//! {"provider": "webhook", "url": "https://sms.example/send", "bearer_token": "…"}
//! {"provider": "vonage", "api_key": "…", "api_secret": "…", "from": "Acme"}
//! {"provider": "messagebird", "api_key": "…", "from": "Acme"}
//! {"provider": "bird", "api_key": "…", "workspace_id": "…", "channel_id": "…"}
//! {"provider": "sns", "access_key_id": "AKIA…", "secret_access_key": "…", "region": "us-east-1"}
//! {"provider": "plivo", "auth_id": "MA…", "auth_token": "…", "from": "+15550100"}
//! {"provider": "dev"}
//! ```
//!
//! Every gateway checks the recipient is E.164, retries 429 and 5xx a couple
//! of times (honouring `Retry-After`, capped), returns the gateway's message
//! id and maps its answer to a typed [`SmsError`].
//!
//! or from [`AuthService::set_sms_provider`]. Providers never log the text
//! (it carries the code).

pub mod gateway;
pub mod providers;

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

pub use gateway::{Gateway, RetryPolicy, SmsError, SmsReceipt};
pub use providers::{BirdSms, MessageBirdSms, PlivoSms, SnsConfig, SnsSms, VonageSms};

use crate::social::{HttpClient, HttpRequest, Secret};
use crate::{AuthError, AuthService};

/// Boxed future returned by [`SmsProvider::send`].
pub type SmsFuture<'a> = std::pin::Pin<Box<dyn Future<Output = Result<SmsReceipt, SmsError>> + Send + 'a>>;

/// What a message is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SmsKind {
    /// Sign-in code.
    LoginCode,
    /// Code confirming a phone number added to an account.
    VerifyPhone,
}

/// One outgoing message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sms {
    /// E.164 number.
    pub to: String,
    /// Text.
    pub text: String,
    /// Purpose.
    pub kind: SmsKind,
    /// The code the message carries (for the development provider and tests).
    pub secret: String,
}

/// Sends SMS. Implementations must not log `text` or `secret`.
pub trait SmsProvider: Send + Sync + fmt::Debug {
    /// Delivers one message; the receipt carries the gateway's message id.
    fn send<'a>(&'a self, sms: &'a Sms) -> SmsFuture<'a>;
}

/// Masks a number for logs: `+1555***67`.
pub fn mask_phone(p: &str) -> String {
    let keep_head = p.len().min(5);
    let keep_tail = p.len().saturating_sub(keep_head).min(2);
    format!("{}***{}", &p[..keep_head], &p[p.len() - keep_tail..])
}

/// Development provider: keeps messages in memory (and, with a directory,
/// writes each as a 0600 JSON file there) instead of sending them.
#[derive(Debug, Clone, Default)]
pub struct DevSmsProvider {
    outbox: Arc<Mutex<Vec<Sms>>>,
    dir: Option<PathBuf>,
}

impl DevSmsProvider {
    /// Also writes messages into `dir`.
    pub fn with_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            outbox: Arc::default(),
            dir: Some(dir.into()),
        }
    }

    /// Every message sent so far.
    pub fn sent(&self) -> Vec<Sms> {
        self.outbox.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// The last message to `to`.
    pub fn last(&self, to: &str) -> Option<Sms> {
        self.sent().into_iter().rev().find(|m| m.to == to)
    }

    fn write_file(&self, sms: &Sms) -> std::io::Result<()> {
        let Some(dir) = &self.dir else { return Ok(()) };
        std::fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let name = format!(
            "{}-{}.json",
            crate::store::now(),
            crate::crypto::random_id(8).map_err(std::io::Error::other)?
        );
        let body = serde_json::to_vec_pretty(sms).map_err(std::io::Error::other)?;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        use std::io::Write as _;
        opts.open(dir.join(name))?.write_all(&body)
    }
}

impl SmsProvider for DevSmsProvider {
    fn send<'a>(&'a self, sms: &'a Sms) -> SmsFuture<'a> {
        Box::pin(async move {
            tracing::info!(to = %mask_phone(&sms.to), kind = ?sms.kind, "SMS kept by the development provider (not delivered)");
            self.write_file(sms).map_err(|e| SmsError::Transient(e.to_string()))?;
            self.outbox.lock().unwrap_or_else(|e| e.into_inner()).push(sms.clone());
            Ok(SmsReceipt::default())
        })
    }
}

/// Twilio Programmable Messaging (`POST /2010-04-01/Accounts/{sid}/Messages.json`).
#[derive(Debug)]
pub struct TwilioSms {
    account_sid: String,
    auth_token: Secret,
    from: Option<String>,
    messaging_service_sid: Option<String>,
    base_url: String,
    gw: Gateway,
}

impl TwilioSms {
    /// A Twilio sender using `http` (the auth service's HTTPS client).
    pub fn new(
        account_sid: impl Into<String>,
        auth_token: impl Into<String>,
        from: Option<String>,
        messaging_service_sid: Option<String>,
        base_url: Option<String>,
        http: Arc<dyn HttpClient>,
    ) -> Result<Self, AuthError> {
        if from.is_none() && messaging_service_sid.is_none() {
            return Err(AuthError::invalid("twilio needs `from` or `messaging_service_sid`"));
        }
        Ok(Self {
            account_sid: account_sid.into(),
            auth_token: Secret::new(auth_token),
            from,
            messaging_service_sid,
            base_url: base_url.unwrap_or_else(|| "https://api.twilio.com".into()),
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

/// Twilio's error codes (`{"code": 21211, "message": …}`).
fn twilio_error(res: &crate::social::HttpResponse) -> SmsError {
    let v: Json = serde_json::from_slice(&res.body).unwrap_or(Json::Null);
    let code = v["code"].as_i64().unwrap_or(0);
    let msg: String = v["message"]
        .as_str()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect();
    let m = format!("HTTP {} code {code}: {msg}", res.status);
    match (res.status, code) {
        (_, 21211 | 21214 | 21216 | 21217 | 21614) => SmsError::InvalidRecipient(m),
        (401 | 403, _) | (_, 20003) => SmsError::Auth(m),
        (429, _) | (_, 20429) => SmsError::RateLimited {
            retry_after_secs: gateway::retry_after(res),
        },
        (500..=599, _) => SmsError::Transient(m),
        _ => SmsError::Rejected(m),
    }
}

impl SmsProvider for TwilioSms {
    fn send<'a>(&'a self, sms: &'a Sms) -> SmsFuture<'a> {
        Box::pin(async move {
            let url = format!(
                "{}/2010-04-01/Accounts/{}/Messages.json",
                self.base_url.trim_end_matches('/'),
                self.account_sid
            );
            let mut fields = vec![("To", sms.to.as_str()), ("Body", sms.text.as_str())];
            if let Some(s) = &self.messaging_service_sid {
                fields.push(("MessagingServiceSid", s));
            } else if let Some(f) = &self.from {
                fields.push(("From", f));
            }
            let basic = base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                self.account_sid,
                self.auth_token.expose()
            ));
            gateway::check_e164(&sms.to)?;
            self.gw
                .call(
                    || HttpRequest::post_form(url.clone(), &fields).header("authorization", format!("Basic {basic}")),
                    |res| match res.status {
                        200..=299 => Ok(SmsReceipt {
                            id: serde_json::from_slice::<Json>(&res.body)
                                .ok()
                                .and_then(|v| v["sid"].as_str().map(str::to_owned)),
                        }),
                        _ => Err(twilio_error(res)),
                    },
                )
                .await
        })
    }
}

/// Generic gateway: `POST <url>` with `{"to","text","kind"}` and an optional
/// bearer token; any 2xx is success.
#[derive(Debug)]
pub struct WebhookSms {
    url: String,
    bearer_token: Option<Secret>,
    gw: Gateway,
}

impl WebhookSms {
    /// A webhook sender using `http`.
    pub fn new(url: impl Into<String>, bearer_token: Option<String>, http: Arc<dyn HttpClient>) -> Self {
        Self {
            url: url.into(),
            bearer_token: bearer_token.map(Secret::new),
            gw: Gateway::new(http),
        }
    }

    /// Replaces the gateway (retry policy).
    #[must_use]
    pub fn with_gateway(mut self, gw: Gateway) -> Self {
        self.gw = gw;
        self
    }
}

impl SmsProvider for WebhookSms {
    fn send<'a>(&'a self, sms: &'a Sms) -> SmsFuture<'a> {
        Box::pin(async move {
            gateway::check_e164(&sms.to)?;
            let body = serde_json::json!({"to": sms.to, "text": sms.text, "kind": sms.kind}).to_string();
            self.gw
                .call(
                    || {
                        let mut req = HttpRequest {
                            method: "POST",
                            url: self.url.clone(),
                            headers: vec![("content-type".into(), "application/json".into())],
                            body: body.clone().into_bytes(),
                        };
                        if let Some(t) = &self.bearer_token {
                            req = req.header("authorization", format!("Bearer {}", t.expose()));
                        }
                        req
                    },
                    |res| {
                        gateway::classify_status(res, || gateway::json_detail(res, &["/error", "/message"]))?;
                        // A gateway may answer `{"id": …}` or `{"message_id": …}`.
                        let id = serde_json::from_slice::<Json>(&res.body).ok().and_then(|v| {
                            ["id", "message_id"]
                                .iter()
                                .find_map(|k| v[*k].as_str().map(str::to_owned))
                        });
                        Ok(SmsReceipt { id })
                    },
                )
                .await
        })
    }
}

#[derive(Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
enum SmsFile {
    Dev,
    Twilio {
        account_sid: String,
        auth_token: String,
        #[serde(default)]
        from: Option<String>,
        #[serde(default)]
        messaging_service_sid: Option<String>,
        #[serde(default)]
        base_url: Option<String>,
    },
    Webhook {
        url: String,
        #[serde(default)]
        bearer_token: Option<String>,
    },
    Vonage {
        api_key: String,
        api_secret: String,
        from: String,
        #[serde(default)]
        base_url: Option<String>,
    },
    Messagebird {
        api_key: String,
        from: String,
        #[serde(default)]
        base_url: Option<String>,
    },
    Bird {
        api_key: String,
        workspace_id: String,
        channel_id: String,
        #[serde(default)]
        base_url: Option<String>,
    },
    Sns {
        access_key_id: String,
        secret_access_key: String,
        #[serde(default)]
        session_token: Option<String>,
        region: String,
        #[serde(default)]
        sender_id: Option<String>,
        #[serde(default)]
        sms_type: Option<String>,
        #[serde(default)]
        endpoint: Option<String>,
    },
    Plivo {
        auth_id: String,
        auth_token: String,
        from: String,
        #[serde(default)]
        base_url: Option<String>,
    },
}

/// Secret fields of an SMS gateway configuration: sealed at rest, masked in
/// the API, kept when a `PUT` sends the mask back.
pub(crate) const SMS_SECRET_FIELDS: &[&str] = &[
    "auth_token",
    "bearer_token",
    "api_secret",
    "api_key",
    "secret_access_key",
    "session_token",
];

/// Whether `url` may be a gateway base URL (http(s), a host, no credentials).
fn check_base_url(url: &Option<String>, what: &str) -> Result<(), AuthError> {
    let Some(u) = url else { return Ok(()) };
    match url::Url::parse(u) {
        Ok(p) if matches!(p.scheme(), "http" | "https") && p.host_str().is_some() && p.username().is_empty() => Ok(()),
        _ => Err(AuthError::invalid(format!("{what}: bad base_url"))),
    }
}

impl AuthService {
    /// Builds a provider from its configuration.
    fn sms_from_config(&self, f: SmsFile, dev_dir: PathBuf, what: &str) -> Result<Arc<dyn SmsProvider>, AuthError> {
        let http = self.http_client();
        Ok(match f {
            SmsFile::Dev => Arc::new(DevSmsProvider::with_dir(dev_dir)),
            SmsFile::Twilio {
                account_sid,
                auth_token,
                from,
                messaging_service_sid,
                base_url,
            } => Arc::new(TwilioSms::new(
                account_sid,
                auth_token,
                from,
                messaging_service_sid,
                base_url,
                http,
            )?),
            SmsFile::Webhook { url, bearer_token } => {
                if url::Url::parse(&url).is_err() {
                    return Err(AuthError::invalid(format!("{what}: bad webhook url")));
                }
                Arc::new(WebhookSms::new(url, bearer_token, http))
            }
            SmsFile::Vonage {
                api_key,
                api_secret,
                from,
                base_url,
            } => {
                check_base_url(&base_url, what)?;
                Arc::new(VonageSms::new(api_key, api_secret, from, base_url, http)?)
            }
            SmsFile::Messagebird {
                api_key,
                from,
                base_url,
            } => {
                check_base_url(&base_url, what)?;
                Arc::new(MessageBirdSms::new(api_key, from, base_url, http)?)
            }
            SmsFile::Bird {
                api_key,
                workspace_id,
                channel_id,
                base_url,
            } => {
                check_base_url(&base_url, what)?;
                Arc::new(BirdSms::new(api_key, workspace_id, channel_id, base_url, http)?)
            }
            SmsFile::Sns {
                access_key_id,
                secret_access_key,
                session_token,
                region,
                sender_id,
                sms_type,
                endpoint,
            } => {
                check_base_url(&endpoint, what)?;
                Arc::new(SnsSms::new(
                    SnsConfig {
                        access_key_id,
                        secret_access_key,
                        session_token,
                        region,
                        sender_id,
                        sms_type,
                        endpoint,
                    },
                    http,
                )?)
            }
            SmsFile::Plivo {
                auth_id,
                auth_token,
                from,
                base_url,
            } => {
                check_base_url(&base_url, what)?;
                Arc::new(PlivoSms::new(auth_id, auth_token, from, base_url, http)?)
            }
        })
    }

    /// Directory the development provider writes to.
    fn sms_dev_dir(&self) -> Option<PathBuf> {
        self.config().master_key_path.parent().map(|d| d.with_file_name("sms"))
    }

    /// Loads `<data_dir>/auth/sms.json` when it exists.
    pub(crate) async fn load_sms_file(&self) -> Result<(), AuthError> {
        let Some(dir) = self.config().master_key_path.parent() else {
            return Ok(());
        };
        let path = dir.join("sms.json");
        let dev = dir.with_file_name("sms");
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.set_sms_provider(Arc::new(DevSmsProvider::with_dir(dev)));
                return Ok(());
            }
            Err(e) => return Err(AuthError::Internal(format!("reading sms.json: {e}"))),
        };
        let f: SmsFile = serde_json::from_str(&text).map_err(|e| AuthError::invalid(format!("sms.json: {e}")))?;
        let p = self.sms_from_config(f, dev, "sms.json")?;
        tracing::info!(provider = ?p, "loaded SMS provider");
        self.set_sms_provider(p);
        Ok(())
    }

    /// The SMS provider for the realm that owns an auth collection: the
    /// realm's own gateway when one is configured, else the instance's.
    pub(crate) fn sms_for(&self, collection: &str) -> Arc<dyn SmsProvider> {
        let realm = self.realms().realm_of_collection(collection);
        self.realm_sms(&realm).unwrap_or_else(|| self.sms())
    }

    /// Builds a provider from a stored (unsealed) realm SMS configuration.
    pub(crate) fn build_realm_sms(&self, realm: &str, cfg: Json) -> Result<Arc<dyn SmsProvider>, AuthError> {
        let f: SmsFile =
            serde_json::from_value(cfg).map_err(|e| AuthError::invalid(format!("sms configuration: {e}")))?;
        let dev = self.sms_dev_dir().unwrap_or_else(|| PathBuf::from("sms")).join(realm);
        self.sms_from_config(f, dev, "sms configuration")
    }
}
