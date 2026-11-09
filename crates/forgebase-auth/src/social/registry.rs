//! Provider configuration and the registry that holds it.
//!
//! The registry is keyed by `(realm, provider id)`. Today every provider
//! lives in realm [`DEFAULT_REALM`]; FB-25 adds realms by registering the same
//! provider ids under other realm ids, with no change to this module or to the
//! flows (every state, identity and credential row already carries `realm`).
//!
//! Secrets (`client_secret`, the Apple signing key) are wrapped in [`Secret`]
//! whose `Debug` prints `[redacted]`, and the config is `Deserialize` only,
//! so it cannot be serialised or logged by accident.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use serde::Deserialize;

use crate::AuthError;

/// The realm of all providers until FB-25.
pub const DEFAULT_REALM: &str = "default";

/// A string that never shows up in logs.
#[derive(Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    /// Wraps a secret.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// The secret, for use in a request.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Whether it is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
}

/// The supported provider types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Google (OpenID Connect).
    Google,
    /// GitHub (OAuth 2, no ID token).
    Github,
    /// Sign in with Apple (OpenID Connect, ES256 client secret).
    Apple,
    /// Facebook Login (OAuth 2, Graph API).
    Facebook,
    /// WeChat website QR login (non-standard token exchange).
    Wechat,
    /// Discord (OAuth 2).
    Discord,
    /// Any OpenID Connect provider (discovery or explicit endpoints).
    Oidc,
}

/// Apple "Sign in with Apple" client-secret signing key.
#[derive(Debug, Clone, Deserialize)]
pub struct AppleKey {
    /// Apple developer team id.
    pub team_id: String,
    /// Key id of the `.p8` key.
    pub key_id: String,
    /// The `.p8` private key (PKCS#8 PEM).
    pub private_key_pem: Secret,
}

fn yes() -> bool {
    true
}

/// One configured provider.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    /// Registry id and URL segment (`google`, `acme-sso`, ...).
    pub id: String,
    /// Provider type.
    pub kind: ProviderKind,
    /// OAuth client id.
    pub client_id: String,
    /// OAuth client secret (not needed for Apple, which uses `apple`).
    #[serde(default)]
    pub client_secret: Secret,
    /// Scopes; the provider type's defaults when absent.
    #[serde(default)]
    pub scopes: Option<Vec<String>>,
    /// OIDC issuer (generic OIDC: discovery base).
    #[serde(default)]
    pub issuer: Option<String>,
    /// Endpoint overrides (also how tests point at a mock provider).
    #[serde(default)]
    pub authorize_url: Option<String>,
    /// Token endpoint override.
    #[serde(default)]
    pub token_url: Option<String>,
    /// Userinfo endpoint override.
    #[serde(default)]
    pub userinfo_url: Option<String>,
    /// JWKS endpoint override.
    #[serde(default)]
    pub jwks_url: Option<String>,
    /// GitHub: endpoint listing the user's emails.
    #[serde(default)]
    pub emails_url: Option<String>,
    /// Redirect URI registered at the provider; default
    /// `<public_url>/api/auth/oauth/<id>/callback`.
    #[serde(default)]
    pub redirect_uri: Option<String>,
    /// Apple signing key.
    #[serde(default)]
    pub apple: Option<AppleKey>,
    /// Create users on first sign-in (default true).
    #[serde(default = "yes")]
    pub allow_signup: bool,
    /// Link a provider login to an existing user with the same *verified*
    /// email (default true; see ADR-0009 §4).
    #[serde(default = "yes")]
    pub auto_link_verified_email: bool,
    /// Label for login buttons.
    #[serde(default)]
    pub display_name: Option<String>,
}

impl ProviderConfig {
    /// Checks the config is usable.
    pub fn validate(&self) -> Result<(), AuthError> {
        let id_ok = !self.id.is_empty()
            && self.id.len() <= 64
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if !id_ok {
            return Err(AuthError::invalid("provider id must be 1-64 chars of [a-z0-9_-]"));
        }
        if self.client_id.is_empty() {
            return Err(AuthError::invalid("provider client_id is required"));
        }
        match self.kind {
            ProviderKind::Apple if self.apple.is_none() => {
                return Err(AuthError::invalid("apple providers need an `apple` signing key"));
            }
            ProviderKind::Apple => {}
            _ if self.client_secret.is_empty() => {
                return Err(AuthError::invalid("provider client_secret is required"));
            }
            _ => {}
        }
        if self.kind == ProviderKind::Oidc
            && self.issuer.is_none()
            && (self.authorize_url.is_none() || self.token_url.is_none() || self.jwks_url.is_none())
        {
            return Err(AuthError::invalid(
                "oidc providers need an issuer (discovery) or authorize, token and jwks URLs",
            ));
        }
        Ok(())
    }
}

/// Endpoints and behaviour of a provider after defaults and discovery.
#[derive(Debug, Clone)]
pub struct Endpoints {
    /// Authorization endpoint.
    pub authorize: String,
    /// Token endpoint.
    pub token: String,
    /// Userinfo / profile endpoint.
    pub userinfo: Option<String>,
    /// JWKS endpoint (ID-token providers).
    pub jwks: Option<String>,
    /// GitHub emails endpoint.
    pub emails: Option<String>,
    /// Accepted `iss` values.
    pub issuers: Vec<String>,
}

/// A registered provider.
#[derive(Debug)]
pub struct Provider {
    /// Its configuration.
    pub config: ProviderConfig,
    pub(crate) endpoints: tokio::sync::OnceCell<Endpoints>,
}

impl Provider {
    /// Whether the token response carries an ID token we validate.
    pub fn uses_id_token(&self) -> bool {
        matches!(
            self.config.kind,
            ProviderKind::Google | ProviderKind::Apple | ProviderKind::Oidc
        )
    }

    /// Whether PKCE is sent (WeChat and Apple do not document it).
    pub fn uses_pkce(&self) -> bool {
        !matches!(self.config.kind, ProviderKind::Wechat | ProviderKind::Apple)
    }

    /// Default scopes of the provider type.
    pub fn scopes(&self) -> Vec<String> {
        if let Some(s) = &self.config.scopes {
            return s.clone();
        }
        let d: &[&str] = match self.config.kind {
            ProviderKind::Google | ProviderKind::Oidc => &["openid", "email", "profile"],
            ProviderKind::Github => &["read:user", "user:email"],
            ProviderKind::Apple => &["name", "email"],
            ProviderKind::Facebook => &["email", "public_profile"],
            ProviderKind::Wechat => &["snsapi_login"],
            ProviderKind::Discord => &["identify", "email"],
        };
        d.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Built-in endpoints of the provider type with overrides applied. For a
    /// generic OIDC provider without explicit URLs this needs discovery,
    /// which is done by the caller (see `flow::endpoints`).
    pub(crate) fn static_endpoints(&self) -> Option<Endpoints> {
        let c = &self.config;
        #[allow(clippy::type_complexity)]
        let (a, t, u, j, e, iss): (&str, &str, Option<&str>, Option<&str>, Option<&str>, &[&str]) = match c.kind {
            ProviderKind::Google => (
                "https://accounts.google.com/o/oauth2/v2/auth",
                "https://oauth2.googleapis.com/token",
                Some("https://openidconnect.googleapis.com/v1/userinfo"),
                Some("https://www.googleapis.com/oauth2/v3/certs"),
                None,
                &["https://accounts.google.com", "accounts.google.com"],
            ),
            ProviderKind::Apple => (
                "https://appleid.apple.com/auth/authorize",
                "https://appleid.apple.com/auth/token",
                None,
                Some("https://appleid.apple.com/auth/keys"),
                None,
                &["https://appleid.apple.com"],
            ),
            ProviderKind::Github => (
                "https://github.com/login/oauth/authorize",
                "https://github.com/login/oauth/access_token",
                Some("https://api.github.com/user"),
                None,
                Some("https://api.github.com/user/emails"),
                &[],
            ),
            ProviderKind::Facebook => (
                "https://www.facebook.com/v19.0/dialog/oauth",
                "https://graph.facebook.com/v19.0/oauth/access_token",
                Some("https://graph.facebook.com/v19.0/me"),
                None,
                None,
                &[],
            ),
            ProviderKind::Wechat => (
                "https://open.weixin.qq.com/connect/qrconnect",
                "https://api.weixin.qq.com/sns/oauth2/access_token",
                Some("https://api.weixin.qq.com/sns/userinfo"),
                None,
                None,
                &[],
            ),
            ProviderKind::Discord => (
                "https://discord.com/oauth2/authorize",
                "https://discord.com/api/oauth2/token",
                Some("https://discord.com/api/users/@me"),
                None,
                None,
                &[],
            ),
            ProviderKind::Oidc => {
                let (a, t, j) = (c.authorize_url.as_ref()?, c.token_url.as_ref()?, c.jwks_url.as_ref()?);
                return Some(Endpoints {
                    authorize: a.clone(),
                    token: t.clone(),
                    userinfo: c.userinfo_url.clone(),
                    jwks: Some(j.clone()),
                    emails: None,
                    issuers: c.issuer.iter().cloned().collect(),
                });
            }
        };
        let pick = |over: &Option<String>, d: Option<&str>| over.clone().or_else(|| d.map(str::to_owned));
        let mut issuers: Vec<String> = iss.iter().map(|s| (*s).to_owned()).collect();
        if let Some(i) = &c.issuer
            && !issuers.contains(i)
        {
            issuers.push(i.clone());
        }
        Some(Endpoints {
            authorize: pick(&c.authorize_url, Some(a))?,
            token: pick(&c.token_url, Some(t))?,
            userinfo: pick(&c.userinfo_url, u),
            jwks: pick(&c.jwks_url, j),
            emails: pick(&c.emails_url, e),
            issuers,
        })
    }
}

/// Providers by `(realm, id)`.
#[derive(Debug, Default)]
pub struct ProviderRegistry {
    map: RwLock<HashMap<(String, String), Arc<Provider>>>,
}

/// What clients may know about a provider.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderInfo {
    /// Registry id.
    pub id: String,
    /// Provider type.
    pub kind: ProviderKind,
    /// Button label.
    pub display_name: String,
}

impl ProviderRegistry {
    /// Adds or replaces a provider in a realm.
    pub fn register(&self, realm: &str, config: ProviderConfig) -> Result<(), AuthError> {
        config.validate()?;
        let key = (realm.to_owned(), config.id.clone());
        let p = Arc::new(Provider {
            config,
            endpoints: tokio::sync::OnceCell::new(),
        });
        self.map.write().unwrap_or_else(|e| e.into_inner()).insert(key, p);
        Ok(())
    }

    /// Removes a provider.
    pub fn remove(&self, realm: &str, id: &str) -> bool {
        self.map
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(realm.to_owned(), id.to_owned()))
            .is_some()
    }

    /// A provider.
    pub fn get(&self, realm: &str, id: &str) -> Option<Arc<Provider>> {
        self.map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(realm.to_owned(), id.to_owned()))
            .cloned()
    }

    /// The providers of a realm, sorted by id.
    pub fn list(&self, realm: &str) -> Vec<ProviderInfo> {
        let mut v: Vec<ProviderInfo> = self
            .map
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|((r, _), _)| r == realm)
            .map(|((_, id), p)| ProviderInfo {
                id: id.clone(),
                kind: p.config.kind,
                display_name: p.config.display_name.clone().unwrap_or_else(|| id.clone()),
            })
            .collect();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Loads providers from JSON: `{"providers": [ProviderConfig, ...]}`; an
    /// entry's optional `"realm"` places it in that realm (default `default`).
    pub fn load_json(&self, json: &str) -> Result<usize, AuthError> {
        #[derive(Deserialize)]
        struct File {
            providers: Vec<serde_json::Value>,
        }
        let f: File = serde_json::from_str(json).map_err(|e| AuthError::invalid(format!("providers file: {e}")))?;
        let n = f.providers.len();
        for mut p in f.providers {
            let realm = match p.as_object_mut().and_then(|o| o.remove("realm")) {
                Some(serde_json::Value::String(r)) => r,
                None => DEFAULT_REALM.to_owned(),
                Some(_) => return Err(AuthError::invalid("providers file: realm must be a string")),
            };
            crate::realm::check_realm_id(&realm)?;
            let cfg: ProviderConfig =
                serde_json::from_value(p).map_err(|e| AuthError::invalid(format!("providers file: {e}")))?;
            self.register(&realm, cfg)?;
        }
        Ok(n)
    }
}
