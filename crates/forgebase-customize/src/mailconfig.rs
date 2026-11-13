//! Which mail transport a realm (or the instance) sends through, and its
//! settings (ADR-0039).
//!
//! A configuration is a JSON object with a `provider` (`log`, `smtp`,
//! `resend`, `postmark`, `sendgrid`, `mailgun`, `ses`), the provider's
//! settings and an optional `from_address` (the `From` of every message;
//! default `CustomizeConfig::from_address`):
//!
//! ```json
//! {"provider": "resend", "api_key": "re_…", "from_address": "no-reply@acme.test"}
//! {"provider": "postmark", "server_token": "…", "message_stream": "outbound"}
//! {"provider": "sendgrid", "api_key": "SG.…", "region": "eu"}
//! {"provider": "mailgun", "api_key": "key-…", "domain": "mg.acme.test", "region": "eu"}
//! {"provider": "ses", "access_key_id": "AKIA…", "secret_access_key": "…", "region": "eu-west-1"}
//! {"provider": "smtp", "host": "smtp.acme.test", "tls": "starttls", "username": "u", "password": "p",
//!  "envelope_from": "bounces@acme.test"}
//! ```
//!
//! Configurations are stored in `_forgebase_email_providers`, one row per
//! realm and one for the instance (`realm = "*"`), with the secret fields
//! sealed under the master key (`sealed:…`), never returned (`"********"`
//! stands for a stored secret and keeps it when sent back) and never logged.
//! A realm without its own row uses the instance's, which falls back to the
//! provider installed with [`CustomizeService::set_mail_provider`] (the log
//! provider by default). Only platform administrators change them.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use forgebase_auth::crypto::Keyring;
use forgebase_auth::social::HttpClient;
use forgebase_data::AccessContext;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use utoipa::ToSchema;

use crate::error::{CustomizeError, Result};
use crate::mail::{MailProvider, OutMail, SendError, SmtpConfig, SmtpProvider, SmtpTls, from_header, valid_address};
use crate::service::{CustomizeService, check_key, text};
use crate::transports::{
    MailgunProvider, PostmarkProvider, ResendProvider, SendGridProvider, SesProvider, SesSettings,
};

/// Row key of the instance-wide configuration.
pub const INSTANCE_REALM: &str = "*";
/// What a stored secret looks like in a response; sending it back keeps the secret.
pub const SECRET_MASK: &str = "********";

/// Secret fields of any configuration: sealed at rest, masked in responses.
const SECRET_FIELDS: &[&str] = &[
    "password",
    "api_key",
    "server_token",
    "secret_access_key",
    "session_token",
];

/// The settings of a mail transport (see the module docs for the JSON).
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum MailConfig {
    /// Log the envelope only (development).
    Log,
    /// SMTP.
    Smtp {
        /// Server host (also the TLS name).
        host: String,
        /// Port; 587 with `starttls`, 465 with `implicit`, 25 with `none`.
        #[serde(default)]
        port: Option<u16>,
        /// `starttls` (default), `implicit` or `none`.
        #[serde(default)]
        tls: Option<String>,
        /// Login name.
        #[serde(default)]
        username: Option<String>,
        /// Password.
        #[serde(default)]
        password: Option<String>,
        /// Envelope sender (`MAIL FROM`).
        envelope_from: String,
        /// Name announced in `EHLO`.
        #[serde(default)]
        helo: Option<String>,
    },
    /// Resend.
    Resend {
        /// API key.
        api_key: String,
        /// Endpoint override.
        #[serde(default)]
        base_url: Option<String>,
    },
    /// Postmark.
    Postmark {
        /// Server API token.
        server_token: String,
        /// Message stream (default `outbound`).
        #[serde(default)]
        message_stream: Option<String>,
        /// Endpoint override.
        #[serde(default)]
        base_url: Option<String>,
    },
    /// SendGrid.
    Sendgrid {
        /// API key.
        api_key: String,
        /// `global` (default) or `eu`.
        #[serde(default)]
        region: Option<String>,
        /// Endpoint override.
        #[serde(default)]
        base_url: Option<String>,
    },
    /// Mailgun.
    Mailgun {
        /// API key.
        api_key: String,
        /// Sending domain.
        domain: String,
        /// `us` (default) or `eu`.
        #[serde(default)]
        region: Option<String>,
        /// Endpoint override.
        #[serde(default)]
        base_url: Option<String>,
    },
    /// Amazon SES v2.
    Ses {
        /// IAM access key id.
        access_key_id: String,
        /// IAM secret access key.
        secret_access_key: String,
        /// Temporary-credential session token.
        #[serde(default)]
        session_token: Option<String>,
        /// SES region.
        region: String,
        /// Configuration set.
        #[serde(default)]
        configuration_set: Option<String>,
        /// Endpoint override.
        #[serde(default)]
        endpoint: Option<String>,
    },
}

fn check_url(u: &Option<String>, what: &str) -> std::result::Result<(), String> {
    let Some(u) = u else { return Ok(()) };
    match url::Url::parse(u) {
        Ok(p) if matches!(p.scheme(), "http" | "https") && p.host_str().is_some() && p.username().is_empty() => Ok(()),
        _ => Err(format!("{what} is not an http(s) URL")),
    }
}

impl MailConfig {
    /// Builds the transport. Errors name the setting that is wrong.
    pub fn build(self, http: Arc<dyn HttpClient>) -> std::result::Result<Arc<dyn MailProvider>, String> {
        let nonempty = |v: &str, what: &str| {
            if v.trim().is_empty() {
                Err(format!("{what} is required"))
            } else {
                Ok(())
            }
        };
        Ok(match self {
            MailConfig::Log => Arc::new(crate::mail::LogProvider::new()),
            MailConfig::Smtp {
                host,
                port,
                tls,
                username,
                password,
                envelope_from,
                helo,
            } => {
                nonempty(&host, "smtp `host`")?;
                if !valid_address(&envelope_from) {
                    return Err("smtp `envelope_from` is not a valid address".into());
                }
                let mode = match tls.as_deref().unwrap_or("starttls") {
                    "starttls" => SmtpTls::StartTls,
                    "implicit" => SmtpTls::Implicit,
                    "none" => SmtpTls::None,
                    _ => return Err("smtp `tls` is starttls, implicit or none".into()),
                };
                if username.is_some() != password.is_some() {
                    return Err("smtp `username` and `password` go together".into());
                }
                let mut c = SmtpConfig::new(host, envelope_from);
                c.tls = mode;
                c.port = port.unwrap_or(match mode {
                    SmtpTls::StartTls => 587,
                    SmtpTls::Implicit => 465,
                    SmtpTls::None => 25,
                });
                c.username = username;
                c.password = password;
                if let Some(h) = helo {
                    nonempty(&h, "smtp `helo`")?;
                    c.helo = h;
                }
                c.timeout = Duration::from_secs(30);
                Arc::new(SmtpProvider::new(c))
            }
            MailConfig::Resend { api_key, base_url } => {
                nonempty(&api_key, "resend `api_key`")?;
                check_url(&base_url, "resend `base_url`")?;
                Arc::new(ResendProvider::new(api_key, base_url, http))
            }
            MailConfig::Postmark {
                server_token,
                message_stream,
                base_url,
            } => {
                nonempty(&server_token, "postmark `server_token`")?;
                check_url(&base_url, "postmark `base_url`")?;
                if let Some(s) = &message_stream
                    && (s.is_empty() || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
                {
                    return Err("postmark `message_stream` is letters, digits, - and _".into());
                }
                Arc::new(PostmarkProvider::new(server_token, message_stream, base_url, http))
            }
            MailConfig::Sendgrid {
                api_key,
                region,
                base_url,
            } => {
                nonempty(&api_key, "sendgrid `api_key`")?;
                check_url(&base_url, "sendgrid `base_url`")?;
                let eu = match region.as_deref().unwrap_or("global") {
                    "global" => false,
                    "eu" => true,
                    _ => return Err("sendgrid `region` is global or eu".into()),
                };
                Arc::new(SendGridProvider::new(api_key, eu, base_url, http))
            }
            MailConfig::Mailgun {
                api_key,
                domain,
                region,
                base_url,
            } => {
                nonempty(&api_key, "mailgun `api_key`")?;
                check_url(&base_url, "mailgun `base_url`")?;
                let domain_ok = !domain.is_empty()
                    && domain.len() <= 253
                    && domain.contains('.')
                    && domain
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
                if !domain_ok {
                    return Err("mailgun `domain` is a sending domain such as mg.example.com".into());
                }
                let eu = match region.as_deref().unwrap_or("us") {
                    "us" => false,
                    "eu" => true,
                    _ => return Err("mailgun `region` is us or eu".into()),
                };
                Arc::new(MailgunProvider::new(api_key, domain, eu, base_url, http))
            }
            MailConfig::Ses {
                access_key_id,
                secret_access_key,
                session_token,
                region,
                configuration_set,
                endpoint,
            } => {
                check_url(&endpoint, "ses `endpoint`")?;
                Arc::new(SesProvider::new(
                    SesSettings {
                        access_key_id,
                        secret_access_key,
                        session_token,
                        region,
                        configuration_set,
                        endpoint,
                    },
                    http,
                )?)
            }
        })
    }
}

/// A transport with the `From` address its realm sends from.
#[derive(Clone)]
pub(crate) struct Configured {
    pub provider: Arc<dyn MailProvider>,
    pub from_address: Option<String>,
}

/// Configured transports: the instance's and each realm's.
#[derive(Default)]
pub(crate) struct MailRegistry {
    pub instance: Option<Configured>,
    pub realms: HashMap<String, Configured>,
}

/// What the stored configurations need to be read and built.
pub(crate) struct MailRuntime {
    pub http: Arc<dyn HttpClient>,
    pub keyring: Keyring,
}

/// Result of a test send.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TestMailResult {
    /// Transport that handled it (`resend`, `ses`, ...).
    pub provider: String,
    /// The provider's message id, when it returned one.
    pub message_id: Option<String>,
    /// The `From` address used.
    pub from: String,
}

fn aad(realm: &str, field: &str) -> Vec<u8> {
    format!("mail:{realm}:provider:{field}").into_bytes()
}

impl CustomizeService {
    /// Gives the service what it needs to build HTTP transports and open
    /// sealed settings, then loads the stored configurations (a stored one
    /// that no longer builds is skipped with a warning).
    pub async fn attach_mail_runtime(&self, http: Arc<dyn HttpClient>, keyring: Keyring) -> Result<()> {
        let _ = self.inner.mail_runtime.set(MailRuntime { http, keyring });
        self.load_mail_providers().await
    }

    fn runtime(&self) -> Result<&MailRuntime> {
        self.inner.mail_runtime.get().ok_or_else(|| {
            CustomizeError::conflict(
                "mail_runtime_unavailable",
                "mail providers cannot be configured: no HTTP client or master key is attached",
            )
        })
    }

    /// Splits `from_address` off a configuration and builds the transport.
    fn build_configured(&self, mut cfg: Json) -> Result<Configured> {
        let rt = self.runtime()?;
        let from_address = match cfg.as_object_mut().and_then(|o| o.remove("from_address")) {
            None | Some(Json::Null) => None,
            Some(Json::String(s)) if valid_address(s.trim()) => Some(s.trim().to_owned()),
            Some(_) => {
                return Err(CustomizeError::invalid(
                    "invalid_mail_config",
                    "`from_address` is not a valid email address",
                ));
            }
        };
        let parsed: MailConfig = serde_json::from_value(cfg)
            .map_err(|e| CustomizeError::invalid("invalid_mail_config", format!("mail configuration: {e}")))?;
        let provider = parsed
            .build(rt.http.clone())
            .map_err(|e| CustomizeError::invalid("invalid_mail_config", e))?;
        Ok(Configured { provider, from_address })
    }

    fn unseal(&self, realm: &str, stored: &mut Json) -> Result<()> {
        let rt = self.runtime()?;
        for f in SECRET_FIELDS {
            let Some(v) = stored.get_mut(*f) else { continue };
            let Some(s) = v.as_str().and_then(|s| s.strip_prefix("sealed:")) else {
                *v = Json::Null;
                continue;
            };
            let blob = URL_SAFE_NO_PAD
                .decode(s)
                .map_err(|_| CustomizeError::Internal("corrupt sealed mail secret".into()))?;
            let plain = rt
                .keyring
                .open(&blob, &aad(realm, f))
                .map_err(|e| CustomizeError::Internal(e.to_string()))?;
            *v = Json::String(
                String::from_utf8(plain).map_err(|_| CustomizeError::Internal("corrupt sealed mail secret".into()))?,
            );
        }
        Ok(())
    }

    async fn stored_mail_config(&self, realm: &str) -> Result<Option<(Json, i64, i64)>> {
        let rows = self
            .query(
                "SELECT config, created_at, updated_at FROM _forgebase_email_providers WHERE realm = ?1",
                vec![realm.into()],
            )
            .await?;
        Ok(rows.rows.first().map(|r| {
            (
                serde_json::from_str(&text(r, 0)).unwrap_or(Json::Null),
                crate::service::int(r, 1),
                crate::service::int(r, 2),
            )
        }))
    }

    /// Loads every stored configuration into the registry.
    pub(crate) async fn load_mail_providers(&self) -> Result<()> {
        let rows = self
            .query("SELECT realm, config FROM _forgebase_email_providers", vec![])
            .await?;
        let mut reg = MailRegistry::default();
        for r in &rows.rows {
            let realm = text(r, 0);
            let mut cfg: Json = serde_json::from_str(&text(r, 1)).unwrap_or(Json::Null);
            let built = self.unseal(&realm, &mut cfg).and_then(|()| self.build_configured(cfg));
            match built {
                Ok(c) if realm == INSTANCE_REALM => reg.instance = Some(c),
                Ok(c) => {
                    reg.realms.insert(realm, c);
                }
                Err(e) => tracing::warn!(%realm, error = %e, "stored mail provider unusable"),
            }
        }
        if let Ok(mut m) = self.inner.mail.write() {
            *m = reg;
        }
        Ok(())
    }

    /// The transport and `From` address for a realm: its own configuration,
    /// else the instance's, else the provider installed with
    /// [`Self::set_mail_provider`].
    pub(crate) fn mail_for(&self, realm: &str) -> (Arc<dyn MailProvider>, Option<String>) {
        let m = self.inner.mail.read().unwrap_or_else(|e| e.into_inner());
        match m.realms.get(realm).or(m.instance.as_ref()) {
            Some(c) => (c.provider.clone(), c.from_address.clone()),
            None => (self.provider(), None),
        }
    }

    fn mail_scope(&self, ctx: &AccessContext, realm: Option<&str>) -> Result<String> {
        self.require_caller(ctx)?;
        if !ctx.is_privileged() {
            return Err(CustomizeError::Forbidden(
                "mail providers are managed by platform administrators".into(),
            ));
        }
        let r = realm
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .unwrap_or(ctx.realm_id.as_str());
        if r != INSTANCE_REALM {
            check_key("realm", r, false)?;
        }
        Ok(r.to_owned())
    }

    fn mail_view(realm: &str, stored: Option<(&Json, i64, i64)>) -> Json {
        let Some((cfg, created, updated)) = stored else {
            return json!({"realm": realm, "configured": false, "config": null});
        };
        let mut v = cfg.clone();
        if let Some(o) = v.as_object_mut() {
            for f in SECRET_FIELDS {
                if let Some(slot) = o.get_mut(*f) {
                    *slot = if slot.as_str().is_some_and(|s| !s.is_empty()) {
                        Json::String(SECRET_MASK.into())
                    } else {
                        Json::Null
                    };
                }
            }
        }
        json!({"realm": realm, "configured": true, "config": v, "created_at": created, "updated_at": updated})
    }

    /// The configuration of `realm` (`*`: the instance), secrets masked.
    /// `configured: false` means the instance's (or the installed) provider applies.
    pub async fn mail_provider_config(&self, ctx: &AccessContext, realm: Option<&str>) -> Result<Json> {
        let realm = self.mail_scope(ctx, realm)?;
        let s = self.stored_mail_config(&realm).await?;
        Ok(Self::mail_view(&realm, s.as_ref().map(|(c, a, b)| (c, *a, *b))))
    }

    /// Sets the mail provider of `realm` (`*`: the instance). Secrets that are
    /// absent or `"********"` keep the stored value of the same provider type.
    /// The configuration must build; nothing is sent.
    pub async fn put_mail_provider(&self, ctx: &AccessContext, realm: Option<&str>, mut body: Json) -> Result<Json> {
        let realm = self.mail_scope(ctx, realm)?;
        if realm != INSTANCE_REALM {
            self.require_realm(&realm).await?;
        }
        if !body.is_object() {
            return Err(CustomizeError::invalid(
                "invalid_mail_config",
                "the mail configuration must be an object",
            ));
        }
        let rt = self.runtime()?;
        let old = self.stored_mail_config(&realm).await?;
        // Keeping a secret only makes sense for the same provider type.
        let same = old.as_ref().is_some_and(|o| o.0["provider"] == body["provider"]);
        let mut old_plain = old.as_ref().filter(|_| same).map(|o| o.0.clone());
        if let Some(o) = old_plain.as_mut() {
            self.unseal(&realm, o)?;
        }
        for f in SECRET_FIELDS {
            let given = body.get(*f).cloned();
            let keep = match &given {
                None | Some(Json::Null) => true,
                Some(Json::String(s)) if s == SECRET_MASK => true,
                Some(Json::String(_)) => false,
                Some(_) => {
                    return Err(CustomizeError::invalid(
                        "invalid_mail_config",
                        format!("{f} must be a string"),
                    ));
                }
            };
            let value = if keep {
                old_plain
                    .as_ref()
                    .and_then(|o| o.get(*f))
                    .and_then(Json::as_str)
                    .map(str::to_owned)
            } else {
                given
                    .and_then(|g| g.as_str().map(str::to_owned))
                    .filter(|s| !s.is_empty())
            };
            if let Some(o) = body.as_object_mut() {
                match value {
                    Some(s) => {
                        o.insert((*f).to_owned(), Json::String(s));
                    }
                    None => {
                        o.remove(*f);
                    }
                }
            }
        }
        let configured = self.build_configured(body.clone())?;
        let mut stored = body.clone();
        for f in SECRET_FIELDS {
            if let Some(v) = stored.get_mut(*f)
                && let Some(s) = v.as_str().map(str::to_owned)
            {
                let sealed = rt
                    .keyring
                    .seal(s.as_bytes(), &aad(&realm, f))
                    .map_err(|e| CustomizeError::Internal(e.to_string()))?;
                *v = Json::String(format!("sealed:{}", URL_SAFE_NO_PAD.encode(sealed)));
            }
        }
        let now = self.now();
        let mut tx = self.begin().await?;
        tx.exec(
            "INSERT INTO _forgebase_email_providers (realm, config, created_at, updated_at, updated_by)
             VALUES (?1, ?2, ?3, ?3, ?4)
             ON CONFLICT (realm) DO UPDATE SET config = excluded.config, updated_at = excluded.updated_at,
                 updated_by = excluded.updated_by",
            vec![
                realm.clone().into(),
                stored.to_string().into(),
                now.into(),
                ctx.principal_key().into(),
            ],
        )
        .await?;
        tx.commit().await?;
        self.install_mail(&realm, Some(configured));
        let created = old.as_ref().map_or(now, |o| o.1);
        // The view shows the plain body with its secrets masked.
        Ok(Self::mail_view(&realm, Some((&body, created, now))))
    }

    /// Removes the realm's provider (`*`: the instance's); the next level applies.
    pub async fn delete_mail_provider(&self, ctx: &AccessContext, realm: Option<&str>) -> Result<()> {
        let realm = self.mail_scope(ctx, realm)?;
        let mut tx = self.begin().await?;
        let n = tx
            .exec(
                "DELETE FROM _forgebase_email_providers WHERE realm = ?1",
                vec![realm.clone().into()],
            )
            .await?;
        tx.commit().await?;
        if n == 0 {
            return Err(CustomizeError::NotFound("mail provider configuration".into()));
        }
        self.install_mail(&realm, None);
        Ok(())
    }

    fn install_mail(&self, realm: &str, c: Option<Configured>) {
        let mut m = self.inner.mail.write().unwrap_or_else(|e| e.into_inner());
        if realm == INSTANCE_REALM {
            m.instance = c;
        } else if let Some(c) = c {
            m.realms.insert(realm.to_owned(), c);
        } else {
            m.realms.remove(realm);
        }
    }

    /// Sends one test message to `to` through the provider `realm` uses
    /// (`*`: the instance's), now and not through the queue, and reports what
    /// the provider answered. Not written to the delivery log.
    pub async fn send_test_mail(&self, ctx: &AccessContext, realm: Option<&str>, to: &str) -> Result<TestMailResult> {
        let realm = self.mail_scope(ctx, realm)?;
        let to = to.trim();
        if !valid_address(to) {
            return Err(CustomizeError::invalid(
                "invalid_recipient",
                "`to` is not a valid email address",
            ));
        }
        let lookup = if realm == INSTANCE_REALM {
            "default"
        } else {
            realm.as_str()
        };
        let (provider, from_override) = if realm == INSTANCE_REALM {
            let m = self.inner.mail.read().unwrap_or_else(|e| e.into_inner());
            match m.instance.as_ref() {
                Some(c) => (c.provider.clone(), c.from_address.clone()),
                None => (self.provider(), None),
            }
        } else {
            self.mail_for(&realm)
        };
        let display = self.branding_of(lookup).await?.display_name.unwrap_or_default();
        let from_addr = from_override.unwrap_or_else(|| self.inner.cfg.from_address.clone());
        let from = from_header(&display, &from_addr);
        let mail = OutMail {
            id: crate::service::new_id("test"),
            realm: lookup.to_owned(),
            to: to.to_owned(),
            from: from.clone(),
            subject: "ForgeBase test email".into(),
            text: "This is a test message from ForgeBase. Your mail provider is configured correctly.".into(),
            html: Some(
                "<p>This is a test message from ForgeBase. Your mail provider is configured correctly.</p>".into(),
            ),
            reply_to: None,
        };
        match provider.send(&mail).await {
            Ok(r) => Ok(TestMailResult {
                provider: provider.name().to_owned(),
                message_id: r.message_id,
                from,
            }),
            Err(SendError::Transient(m) | SendError::Permanent(m)) => Err(CustomizeError::conflict(
                "mail_test_failed",
                format!("the {} provider did not accept the test message: {m}", provider.name()),
            )),
        }
    }
}
