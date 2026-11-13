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
    /// Microsoft Entra ID / Microsoft account (OpenID Connect, v2.0
    /// endpoints; `tenant` is `common`, `organizations`, `consumers` or a
    /// tenant GUID).
    Microsoft,
    /// GitLab.com or a self-hosted GitLab (OpenID Connect; `base_url`).
    Gitlab,
    /// Sign in with Slack (OpenID Connect).
    Slack,
    /// Sign In with LinkedIn using OpenID Connect.
    Linkedin,
    /// Twitch (OpenID Connect; claims are requested explicitly).
    Twitch,
    /// Bitbucket Cloud (OAuth 2, no ID token).
    Bitbucket,
    /// Spotify (OAuth 2, no ID token).
    Spotify,
    /// A Keycloak realm (OpenID Connect discovery; `base_url` + `tenant` = realm name).
    Keycloak,
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
    /// Microsoft: `common` (default), `organizations`, `consumers` or a
    /// tenant GUID. Keycloak: the realm name.
    #[serde(default)]
    pub tenant: Option<String>,
    /// Microsoft: login host (default `https://login.microsoftonline.com`;
    /// sovereign clouds differ). GitLab: the instance (default
    /// `https://gitlab.com`). Keycloak: the server, with its context path
    /// (`https://sso.example/auth` on legacy installs).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Microsoft multi-tenant (`common`, `organizations`): the tenant GUIDs
    /// allowed to sign in; any tenant when absent.
    #[serde(default)]
    pub allowed_tenants: Option<Vec<String>>,
    /// Extra authorization request parameters (`prompt`, `login_hint`, Slack
    /// `team`, Google `hd`, ...). The protocol parameters cannot be set.
    #[serde(default)]
    pub authorize_params: Option<std::collections::BTreeMap<String, String>>,
}

/// Parameters of the authorization request that configuration cannot override.
const RESERVED_PARAMS: &[&str] = &[
    "client_id",
    "appid",
    "response_type",
    "redirect_uri",
    "scope",
    "state",
    "nonce",
    "code_challenge",
    "code_challenge_method",
    "response_mode",
];

/// The tenant GUID of Microsoft's consumer (personal account) directory.
pub const MSA_TENANT: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

/// `8-4-4-4-12` hex digits.
pub(crate) fn is_guid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit(),
        })
}

/// An `http(s)` base URL without credentials, query or fragment.
fn base_url_ok(u: &str) -> bool {
    url::Url::parse(u).is_ok_and(|p| {
        matches!(p.scheme(), "http" | "https")
            && p.host_str().is_some()
            && p.username().is_empty()
            && p.password().is_none()
            && p.query().is_none()
            && p.fragment().is_none()
    })
}

/// Issuer rule of a multi-tenant provider: `iss` must be the template with
/// the token's own `tid` filled in, and the tenant must be allowed.
#[derive(Debug, Clone)]
pub struct TenantRule {
    /// `https://login.microsoftonline.com/{tenantid}/v2.0`.
    pub issuer_template: String,
    /// Only this tenant (`consumers`).
    pub only: Option<String>,
    /// Never this tenant (`organizations` excludes personal accounts).
    pub not: Option<String>,
    /// Allowed tenants (lower-case GUIDs); any when empty.
    pub allow: Vec<String>,
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
        for k in self.authorize_params.iter().flat_map(|m| m.keys()) {
            if RESERVED_PARAMS.contains(&k.as_str()) || k.is_empty() || k.len() > 64 {
                return Err(AuthError::invalid(format!(
                    "authorize_params cannot set `{k}`; the flow sets it"
                )));
            }
        }
        if let Some(b) = &self.base_url
            && !base_url_ok(b)
        {
            return Err(AuthError::invalid(
                "base_url must be an http(s) URL without credentials, query or fragment",
            ));
        }
        match self.kind {
            ProviderKind::Microsoft => {
                let t = self.tenant.as_deref().unwrap_or("common");
                if !matches!(t, "common" | "organizations" | "consumers") && !is_guid(t) {
                    return Err(AuthError::invalid(
                        "microsoft `tenant` must be common, organizations, consumers or a tenant GUID",
                    ));
                }
                if self.allowed_tenants.iter().flatten().any(|g| !is_guid(g)) {
                    return Err(AuthError::invalid("microsoft `allowed_tenants` are tenant GUIDs"));
                }
            }
            ProviderKind::Keycloak => {
                let t = self.tenant.as_deref().unwrap_or_default();
                let realm_ok = !t.is_empty()
                    && t.len() <= 128
                    && t.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b' '));
                if self.base_url.is_none() || !realm_ok {
                    return Err(AuthError::invalid(
                        "keycloak providers need `base_url` (the server) and `tenant` (the realm name)",
                    ));
                }
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
    /// Multi-tenant issuer rule (Microsoft `common` / `organizations` /
    /// `consumers`); replaces `issuers` when set.
    pub tenant: Option<TenantRule>,
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
            ProviderKind::Google
                | ProviderKind::Apple
                | ProviderKind::Oidc
                | ProviderKind::Microsoft
                | ProviderKind::Gitlab
                | ProviderKind::Slack
                | ProviderKind::Linkedin
                | ProviderKind::Twitch
                | ProviderKind::Keycloak
        )
    }

    /// Whether PKCE is sent (WeChat, Apple and Bitbucket do not document it).
    pub fn uses_pkce(&self) -> bool {
        !matches!(
            self.config.kind,
            ProviderKind::Wechat | ProviderKind::Apple | ProviderKind::Bitbucket
        )
    }

    /// Whether the client authenticates at the token endpoint with HTTP Basic
    /// (RFC 6749 §2.3.1) instead of form fields.
    pub fn token_auth_basic(&self) -> bool {
        matches!(self.config.kind, ProviderKind::Bitbucket | ProviderKind::Spotify)
    }

    /// The OIDC issuer: configured, or derived from a Keycloak server and realm.
    pub fn issuer(&self) -> Option<String> {
        let c = &self.config;
        if c.kind == ProviderKind::Keycloak && c.issuer.is_none() {
            let base = c.base_url.as_deref()?.trim_end_matches('/');
            let realm = c.tenant.as_deref()?;
            return Some(format!("{base}/realms/{}", realm.replace(' ', "%20")));
        }
        c.issuer.clone()
    }

    /// Default scopes of the provider type.
    pub fn scopes(&self) -> Vec<String> {
        if let Some(s) = &self.config.scopes {
            return s.clone();
        }
        let d: &[&str] = match self.config.kind {
            ProviderKind::Google
            | ProviderKind::Oidc
            | ProviderKind::Microsoft
            | ProviderKind::Gitlab
            | ProviderKind::Slack
            | ProviderKind::Keycloak => &["openid", "email", "profile"],
            ProviderKind::Linkedin => &["openid", "profile", "email"],
            ProviderKind::Twitch => &["openid", "user:read:email"],
            ProviderKind::Github => &["read:user", "user:email"],
            ProviderKind::Apple => &["name", "email"],
            ProviderKind::Facebook => &["email", "public_profile"],
            ProviderKind::Wechat => &["snsapi_login"],
            ProviderKind::Discord => &["identify", "email"],
            ProviderKind::Bitbucket => &["account", "email"],
            ProviderKind::Spotify => &["user-read-email"],
        };
        d.iter().map(|s| (*s).to_owned()).collect()
    }

    /// Authorization request parameters the provider type needs besides the
    /// protocol ones, then the configured extras.
    pub fn extra_authorize_params(&self) -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = Vec::new();
        if self.config.kind == ProviderKind::Twitch {
            // Twitch only puts claims in the ID token and `userinfo` that the
            // request names.
            let claim =
                serde_json::json!({"email": null, "email_verified": null, "preferred_username": null, "picture": null});
            v.push((
                "claims".into(),
                serde_json::json!({"id_token": claim, "userinfo": claim}).to_string(),
            ));
        }
        for (k, val) in self.config.authorize_params.iter().flatten() {
            v.retain(|(x, _)| x != k);
            v.push((k.clone(), val.clone()));
        }
        v
    }

    /// Built-in endpoints of the provider type with overrides applied. For a
    /// generic OIDC provider without explicit URLs this needs discovery,
    /// which is done by the caller (see `flow::endpoints`).
    pub(crate) fn static_endpoints(&self) -> Option<Endpoints> {
        let c = &self.config;
        let (a, t, u, j, e, iss): (
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Vec<String>,
        );
        let mut tenant = None;
        let lit = |x: &str| x.to_owned();
        match c.kind {
            ProviderKind::Google => {
                (a, t, u, j, e) = (
                    lit("https://accounts.google.com/o/oauth2/v2/auth"),
                    lit("https://oauth2.googleapis.com/token"),
                    Some(lit("https://openidconnect.googleapis.com/v1/userinfo")),
                    Some(lit("https://www.googleapis.com/oauth2/v3/certs")),
                    None,
                );
                iss = vec![lit("https://accounts.google.com"), lit("accounts.google.com")];
            }
            ProviderKind::Apple => {
                (a, t, u, j, e) = (
                    lit("https://appleid.apple.com/auth/authorize"),
                    lit("https://appleid.apple.com/auth/token"),
                    None,
                    Some(lit("https://appleid.apple.com/auth/keys")),
                    None,
                );
                iss = vec![lit("https://appleid.apple.com")];
            }
            ProviderKind::Github => {
                (a, t, u, j, e) = (
                    lit("https://github.com/login/oauth/authorize"),
                    lit("https://github.com/login/oauth/access_token"),
                    Some(lit("https://api.github.com/user")),
                    None,
                    Some(lit("https://api.github.com/user/emails")),
                );
                iss = vec![];
            }
            ProviderKind::Facebook => {
                (a, t, u, j, e) = (
                    lit("https://www.facebook.com/v19.0/dialog/oauth"),
                    lit("https://graph.facebook.com/v19.0/oauth/access_token"),
                    Some(lit("https://graph.facebook.com/v19.0/me")),
                    None,
                    None,
                );
                iss = vec![];
            }
            ProviderKind::Wechat => {
                (a, t, u, j, e) = (
                    lit("https://open.weixin.qq.com/connect/qrconnect"),
                    lit("https://api.weixin.qq.com/sns/oauth2/access_token"),
                    Some(lit("https://api.weixin.qq.com/sns/userinfo")),
                    None,
                    None,
                );
                iss = vec![];
            }
            ProviderKind::Discord => {
                (a, t, u, j, e) = (
                    lit("https://discord.com/oauth2/authorize"),
                    lit("https://discord.com/api/oauth2/token"),
                    Some(lit("https://discord.com/api/users/@me")),
                    None,
                    None,
                );
                iss = vec![];
            }
            ProviderKind::Slack => {
                (a, t, u, j, e) = (
                    lit("https://slack.com/openid/connect/authorize"),
                    lit("https://slack.com/api/openid.connect.token"),
                    Some(lit("https://slack.com/api/openid.connect.userInfo")),
                    Some(lit("https://slack.com/openid/connect/keys")),
                    None,
                );
                iss = vec![lit("https://slack.com")];
            }
            ProviderKind::Linkedin => {
                (a, t, u, j, e) = (
                    lit("https://www.linkedin.com/oauth/v2/authorization"),
                    lit("https://www.linkedin.com/oauth/v2/accessToken"),
                    Some(lit("https://api.linkedin.com/v2/userinfo")),
                    Some(lit("https://www.linkedin.com/oauth/openid/jwks")),
                    None,
                );
                iss = vec![lit("https://www.linkedin.com")];
            }
            ProviderKind::Twitch => {
                (a, t, u, j, e) = (
                    lit("https://id.twitch.tv/oauth2/authorize"),
                    lit("https://id.twitch.tv/oauth2/token"),
                    Some(lit("https://id.twitch.tv/oauth2/userinfo")),
                    Some(lit("https://id.twitch.tv/oauth2/keys")),
                    None,
                );
                iss = vec![lit("https://id.twitch.tv/oauth2")];
            }
            ProviderKind::Bitbucket => {
                (a, t, u, j, e) = (
                    lit("https://bitbucket.org/site/oauth2/authorize"),
                    lit("https://bitbucket.org/site/oauth2/access_token"),
                    Some(lit("https://api.bitbucket.org/2.0/user")),
                    None,
                    Some(lit("https://api.bitbucket.org/2.0/user/emails")),
                );
                iss = vec![];
            }
            ProviderKind::Spotify => {
                (a, t, u, j, e) = (
                    lit("https://accounts.spotify.com/authorize"),
                    lit("https://accounts.spotify.com/api/token"),
                    Some(lit("https://api.spotify.com/v1/me")),
                    None,
                    None,
                );
                iss = vec![];
            }
            ProviderKind::Gitlab => {
                let base = c
                    .base_url
                    .as_deref()
                    .unwrap_or("https://gitlab.com")
                    .trim_end_matches('/');
                (a, t, u, j, e) = (
                    format!("{base}/oauth/authorize"),
                    format!("{base}/oauth/token"),
                    Some(format!("{base}/oauth/userinfo")),
                    Some(format!("{base}/oauth/discovery/keys")),
                    None,
                );
                iss = vec![base.to_owned()];
            }
            ProviderKind::Microsoft => {
                let base = c
                    .base_url
                    .as_deref()
                    .unwrap_or("https://login.microsoftonline.com")
                    .trim_end_matches('/');
                let ten = c.tenant.as_deref().unwrap_or("common");
                (a, t, u, j, e) = (
                    format!("{base}/{ten}/oauth2/v2.0/authorize"),
                    format!("{base}/{ten}/oauth2/v2.0/token"),
                    Some(lit("https://graph.microsoft.com/oidc/userinfo")),
                    Some(format!("{base}/{ten}/discovery/v2.0/keys")),
                    None,
                );
                let allow: Vec<String> = c
                    .allowed_tenants
                    .iter()
                    .flatten()
                    .map(|g| g.to_ascii_lowercase())
                    .collect();
                if is_guid(ten) {
                    // One tenant: its issuer is known up front.
                    iss = vec![format!("{base}/{}/v2.0", ten.to_ascii_lowercase())];
                } else {
                    iss = vec![];
                    tenant = Some(TenantRule {
                        issuer_template: format!("{base}/{{tenantid}}/v2.0"),
                        only: (ten == "consumers").then(|| MSA_TENANT.to_owned()),
                        not: (ten == "organizations").then(|| MSA_TENANT.to_owned()),
                        allow,
                    });
                }
            }
            ProviderKind::Keycloak => {
                // Discovery finds the endpoints unless all are given.
                let (a, t, j) = (c.authorize_url.as_ref()?, c.token_url.as_ref()?, c.jwks_url.as_ref()?);
                return Some(Endpoints {
                    authorize: a.clone(),
                    token: t.clone(),
                    userinfo: c.userinfo_url.clone(),
                    jwks: Some(j.clone()),
                    emails: None,
                    issuers: self.issuer().into_iter().collect(),
                    tenant: None,
                });
            }
            ProviderKind::Oidc => {
                let (a, t, j) = (c.authorize_url.as_ref()?, c.token_url.as_ref()?, c.jwks_url.as_ref()?);
                return Some(Endpoints {
                    authorize: a.clone(),
                    token: t.clone(),
                    userinfo: c.userinfo_url.clone(),
                    jwks: Some(j.clone()),
                    emails: None,
                    issuers: c.issuer.iter().cloned().collect(),
                    tenant: None,
                });
            }
        }
        let pick = |over: &Option<String>, d: Option<String>| over.clone().or(d);
        let mut issuers = iss;
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
            tenant,
        })
    }
}

/// Providers by `(realm, id)`.
#[derive(Debug, Default)]
pub struct ProviderRegistry {
    map: RwLock<HashMap<(String, String), Arc<Provider>>>,
    /// `(realm, id)` of providers defined in `oauth-providers.json`; the API
    /// treats them as read-only.
    file: RwLock<std::collections::HashSet<(String, String)>>,
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

    /// Whether the provider comes from `oauth-providers.json`.
    pub fn is_file_defined(&self, realm: &str, id: &str) -> bool {
        self.file
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&(realm.to_owned(), id.to_owned()))
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
            let id = cfg.id.clone();
            self.register(&realm, cfg)?;
            self.file.write().unwrap_or_else(|e| e.into_inner()).insert((realm, id));
        }
        Ok(n)
    }
}
