//! Outgoing SMS (FB-25, ADR-0015 §4): the pluggable [`SmsProvider`] trait,
//! a development provider, and two gateways over the auth service's HTTPS
//! client (Twilio's Messages API and a generic JSON webhook).
//!
//! The provider comes from `<data_dir>/auth/sms.json` when present (it holds
//! gateway credentials; keep it 0600):
//!
//! ```json
//! {"provider": "twilio", "account_sid": "AC…", "auth_token": "…", "from": "+15550100"}
//! {"provider": "webhook", "url": "https://sms.example/send", "bearer_token": "…"}
//! {"provider": "dev"}
//! ```
//!
//! or from [`AuthService::set_sms_provider`]. Providers never log the text
//! (it carries the code).

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::social::{HttpClient, HttpRequest, Secret};
use crate::{AuthError, AuthService};

/// Boxed future returned by [`SmsProvider::send`].
pub type SmsFuture<'a> = std::pin::Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

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
    /// Delivers one message.
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
            self.write_file(sms).map_err(|e| e.to_string())?;
            self.outbox.lock().unwrap_or_else(|e| e.into_inner()).push(sms.clone());
            Ok(())
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
    http: Arc<dyn HttpClient>,
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
            http,
        })
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
            let basic = base64::engine::general_purpose::STANDARD
                .encode(format!("{}:{}", self.account_sid, self.auth_token.expose()));
            let req = HttpRequest::post_form(url, &fields).header("authorization", format!("Basic {basic}"));
            let res = self.http.send(req).await.map_err(|e| e.to_string())?;
            if (200..300).contains(&res.status) {
                Ok(())
            } else {
                Err(format!("twilio answered HTTP {}", res.status))
            }
        })
    }
}

/// Generic gateway: `POST <url>` with `{"to","text","kind"}` and an optional
/// bearer token; any 2xx is success.
#[derive(Debug)]
pub struct WebhookSms {
    url: String,
    bearer_token: Option<Secret>,
    http: Arc<dyn HttpClient>,
}

impl WebhookSms {
    /// A webhook sender using `http`.
    pub fn new(url: impl Into<String>, bearer_token: Option<String>, http: Arc<dyn HttpClient>) -> Self {
        Self {
            url: url.into(),
            bearer_token: bearer_token.map(Secret::new),
            http,
        }
    }
}

impl SmsProvider for WebhookSms {
    fn send<'a>(&'a self, sms: &'a Sms) -> SmsFuture<'a> {
        Box::pin(async move {
            let body = serde_json::json!({"to": sms.to, "text": sms.text, "kind": sms.kind}).to_string();
            let mut req = HttpRequest {
                method: "POST",
                url: self.url.clone(),
                headers: vec![("content-type".into(), "application/json".into())],
                body: body.into_bytes(),
            };
            if let Some(t) = &self.bearer_token {
                req = req.header("authorization", format!("Bearer {}", t.expose()));
            }
            let res = self.http.send(req).await.map_err(|e| e.to_string())?;
            if (200..300).contains(&res.status) {
                Ok(())
            } else {
                Err(format!("SMS gateway answered HTTP {}", res.status))
            }
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
}

impl AuthService {
    /// Loads `<data_dir>/auth/sms.json` when it exists.
    pub(crate) async fn load_sms_file(&self) -> Result<(), AuthError> {
        let Some(dir) = self.config().master_key_path.parent() else {
            return Ok(());
        };
        let path = dir.join("sms.json");
        let text = match tokio::fs::read_to_string(&path).await {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.set_sms_provider(Arc::new(DevSmsProvider::with_dir(dir.with_file_name("sms"))));
                return Ok(());
            }
            Err(e) => return Err(AuthError::Internal(format!("reading sms.json: {e}"))),
        };
        let f: SmsFile = serde_json::from_str(&text).map_err(|e| AuthError::invalid(format!("sms.json: {e}")))?;
        let http = self.http_client();
        let p: Arc<dyn SmsProvider> = match f {
            SmsFile::Dev => Arc::new(DevSmsProvider::with_dir(dir.with_file_name("sms"))),
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
                    return Err(AuthError::invalid("sms.json: bad webhook url"));
                }
                Arc::new(WebhookSms::new(url, bearer_token, http))
            }
        };
        tracing::info!(provider = ?p, "loaded SMS provider");
        self.set_sms_provider(p);
        Ok(())
    }
}
