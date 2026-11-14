//! Identity administration (`/api/auth/*` beyond sign-in): API keys, personal access tokens,
//! service accounts, OAuth / OIDC clients, realms and their login providers, user lifecycle,
//! impersonation, audit, phone, LDAP, passkeys and account self-service. `client.identity()`.
//!
//! Sign-in, sessions and `create_token` stay on [`AuthApi`](crate::AuthApi); this covers what an
//! admin console or a settings page needs on top. Bodies the OpenAPI document leaves as plain
//! objects are written by hand after the server's JSON (snake_case); open-ended ones (provider
//! configs, SMS gateways, user records) are [`serde_json::Value`]s.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::{SignInOptions, SignInResult};
use crate::error::{Error, Result};
use crate::generated::AuthKeyCreated;
use crate::http::{Req, Transport, decode_items as items, seg};

fn decode<T: serde::de::DeserializeOwned>(v: Value, what: &str) -> Result<T> {
    serde_json::from_value(v).map_err(|e| Error::InvalidResponse(format!("{what}: {e}")))
}

/// An API key or personal access token, never its secret.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiKeyInfo {
    /// Key id.
    pub id: String,
    /// `api_key` or `pat`.
    #[serde(default)]
    pub kind: String,
    /// Name.
    #[serde(default)]
    pub name: String,
    /// Owner type (`user`, `service`, ...).
    #[serde(default)]
    pub owner_type: String,
    /// Owner's collection.
    #[serde(default)]
    pub collection: Option<String>,
    /// Owner id.
    #[serde(default)]
    pub owner_id: String,
    /// Scopes, `<area>:read|write[:<name>]`.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Creation time (RFC 3339).
    #[serde(default)]
    pub created_at: String,
    /// Expiry (RFC 3339).
    #[serde(default)]
    pub expires_at: Option<String>,
    /// Last use (RFC 3339).
    #[serde(default)]
    pub last_used_at: Option<String>,
    /// True once revoked.
    #[serde(default)]
    pub revoked: bool,
    /// Tenant (environment id) the key is bound to.
    #[serde(default)]
    pub tenant: Option<String>,
}

/// A new API key.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NewApiKey {
    /// Name.
    pub name: String,
    /// `<area>:read|write[:<name>]`.
    pub scopes: Vec<String>,
    /// Expiry in days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<u32>,
    /// Bind the key to one tenant: requests then act in it only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
}

impl NewApiKey {
    /// A key named `name` with `scopes`.
    #[must_use]
    pub fn new(name: impl Into<String>, scopes: &[&str]) -> Self {
        Self {
            name: name.into(),
            scopes: scopes.iter().map(|s| (*s).to_owned()).collect(),
            ..Self::default()
        }
    }

    /// Expires after `days`.
    #[must_use]
    pub fn expires_in_days(mut self, days: u32) -> Self {
        self.expires_in_days = Some(days);
        self
    }

    /// Binds the key to one tenant.
    #[must_use]
    pub fn tenant(mut self, tenant: impl Into<String>) -> Self {
        self.tenant = Some(tenant.into());
        self
    }
}

/// A service account.
#[derive(Debug, Clone, Deserialize)]
pub struct ServiceAccount {
    /// Id.
    pub id: String,
    /// Name.
    #[serde(default)]
    pub name: String,
    /// Description.
    #[serde(default)]
    pub description: String,
    /// Role (default `service`).
    #[serde(default)]
    pub role: String,
    /// Creator.
    #[serde(default)]
    pub created_by: String,
    /// Creation time (RFC 3339).
    #[serde(default)]
    pub created_at: String,
    /// True when disabled.
    #[serde(default)]
    pub disabled: bool,
    /// Realm.
    #[serde(default)]
    pub realm: String,
}

/// A new service account.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NewServiceAccount {
    /// Name.
    pub name: String,
    /// Description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Role (default `service`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

impl NewServiceAccount {
    /// A service account named `name`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }
}

/// An administrator.
#[derive(Debug, Clone, Deserialize)]
pub struct AdminAccount {
    /// Id.
    pub id: String,
    /// Email.
    #[serde(default)]
    pub email: String,
    /// Creation time (RFC 3339).
    #[serde(default)]
    pub created_at: String,
    /// Last sign-in (RFC 3339).
    #[serde(default)]
    pub last_login_at: Option<String>,
}

/// One session of a user.
#[derive(Debug, Clone, Deserialize)]
pub struct SessionInfo {
    /// Session id.
    pub id: String,
    /// How it was opened.
    #[serde(default)]
    pub auth_method: String,
    /// Whether MFA was used.
    #[serde(default)]
    pub mfa: bool,
    /// Cookie session.
    #[serde(default)]
    pub cookie: bool,
    /// Client IP.
    #[serde(default)]
    pub ip: Option<String>,
    /// User agent.
    #[serde(default)]
    pub user_agent: Option<String>,
    /// Device name.
    #[serde(default)]
    pub device_name: Option<String>,
    /// Creation time.
    #[serde(default)]
    pub created_at: String,
    /// Last seen.
    #[serde(default)]
    pub last_seen_at: String,
    /// Expiry.
    #[serde(default)]
    pub expires_at: String,
    /// The caller's own session.
    #[serde(default)]
    pub current: bool,
    /// Impersonating admin.
    #[serde(default)]
    pub impersonator: Option<String>,
}

/// One row of the authentication audit log.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthAuditEntry {
    /// Row id (page with [`AuthAuditQuery::before`]).
    #[serde(default)]
    pub id: i64,
    /// Time (RFC 3339).
    #[serde(default)]
    pub at: String,
    /// Event name, e.g. `login.failed`.
    #[serde(default)]
    pub event: String,
    /// `ok` / `failed`.
    #[serde(default)]
    pub outcome: String,
    /// Actor.
    #[serde(default)]
    pub actor: String,
    /// Subject type.
    #[serde(default)]
    pub subject_type: Option<String>,
    /// Subject collection.
    #[serde(default)]
    pub collection: Option<String>,
    /// Subject id.
    #[serde(default)]
    pub subject_id: Option<String>,
    /// Client IP.
    #[serde(default)]
    pub ip: Option<String>,
    /// User agent.
    #[serde(default)]
    pub user_agent: Option<String>,
    /// Request id.
    #[serde(default)]
    pub request_id: Option<String>,
    /// Event detail.
    #[serde(default)]
    pub detail: Value,
}

/// Filters of [`IdentityApi::audit`].
#[derive(Debug, Clone, Default)]
pub struct AuthAuditQuery {
    /// Event name.
    pub event: Option<String>,
    /// Subject id.
    pub subject_id: Option<String>,
    /// Only rows with an id below this one (paging).
    pub before: Option<i64>,
    /// 1 to 500 (default set by the server).
    pub limit: Option<u32>,
}

/// A grant to act as another user.
#[derive(Debug, Clone, Deserialize)]
pub struct ImpersonationGrant {
    /// Token type.
    #[serde(default)]
    pub token_type: String,
    /// Access token acting as the target; use it with a bearer client.
    pub access_token: String,
    /// Seconds until it expires.
    #[serde(default)]
    pub expires_in: i64,
    /// Expiry (RFC 3339).
    #[serde(default)]
    pub expires_at: String,
    /// Grant session id (pass to [`ImpersonationApi::end`]).
    #[serde(default)]
    pub session_id: String,
    /// The admin.
    #[serde(default)]
    pub impersonator: String,
    /// The target subject.
    #[serde(default)]
    pub subject: Value,
    /// Reason given.
    #[serde(default)]
    pub reason: String,
}

/// An impersonation grant in the list.
#[derive(Debug, Clone, Deserialize)]
pub struct ImpersonationInfo {
    /// Grant session id.
    pub session_id: String,
    /// Admin id.
    #[serde(default)]
    pub admin_id: String,
    /// Target principal.
    #[serde(default)]
    pub target: String,
    /// Reason.
    #[serde(default)]
    pub reason: String,
    /// Start.
    #[serde(default)]
    pub created_at: String,
    /// Expiry.
    #[serde(default)]
    pub expires_at: String,
    /// End, when ended early.
    #[serde(default)]
    pub ended_at: Option<String>,
    /// Still usable.
    #[serde(default)]
    pub active: bool,
}

/// Lifecycle state of an account.
#[derive(Debug, Clone, Deserialize)]
pub struct AccountStatus {
    /// `active`, `suspended`, `deactivated` or `anonymised`.
    pub status: String,
    /// Reason.
    #[serde(default)]
    pub reason: Option<String>,
    /// Suspension end (RFC 3339), if limited.
    #[serde(default)]
    pub until: Option<String>,
    /// Who changed it.
    #[serde(default)]
    pub changed_by: Option<String>,
    /// When.
    #[serde(default)]
    pub changed_at: Option<String>,
}

/// An OAuth / OIDC client app, never its secret.
#[derive(Debug, Clone, Deserialize)]
pub struct OAuthClient {
    /// Client id.
    pub client_id: String,
    /// Realm.
    #[serde(default)]
    pub realm: String,
    /// Name.
    #[serde(default)]
    pub name: String,
    /// Description.
    #[serde(default)]
    pub description: Option<String>,
    /// `confidential` or `public`.
    #[serde(rename = "type", default)]
    pub client_type: String,
    /// Exact-match redirect URIs.
    #[serde(default)]
    pub redirect_uris: Vec<String>,
    /// Post-logout redirect URIs.
    #[serde(default)]
    pub post_logout_redirect_uris: Vec<String>,
    /// Grant types.
    #[serde(default)]
    pub grant_types: Vec<String>,
    /// Scopes.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// First-party app: no consent screen.
    #[serde(default)]
    pub skip_consent: bool,
    /// Access token lifetime, seconds.
    #[serde(default)]
    pub access_token_ttl: Option<i64>,
    /// Refresh token lifetime, seconds.
    #[serde(default)]
    pub refresh_token_ttl: Option<i64>,
    /// Disabled.
    #[serde(default)]
    pub disabled: bool,
    /// When the secret was made.
    #[serde(default)]
    pub secret_created_at: Option<String>,
    /// When the previous secret stops working.
    #[serde(default)]
    pub previous_secret_expires_at: Option<String>,
    /// Creation time.
    #[serde(default)]
    pub created_at: String,
    /// Update time.
    #[serde(default)]
    pub updated_at: String,
}

/// A created client, or a client with a rotated secret: `client_secret` is shown this once.
#[derive(Debug, Clone, Deserialize)]
pub struct CreatedOAuthClient {
    /// The client.
    pub client: OAuthClient,
    /// The secret of a confidential client.
    #[serde(default)]
    pub client_secret: Option<String>,
}

/// A new OAuth client; also the patch of [`OAuthClientsApi::update`] (unset fields stay).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ClientInput {
    /// Name (required on create).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Realm (create only; default: the request's realm).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub realm: Option<String>,
    /// Description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `confidential` (default) or `public` (create only).
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub client_type: Option<String>,
    /// Exact-match redirect URIs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub redirect_uris: Option<Vec<String>>,
    /// Post-logout redirect URIs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_logout_redirect_uris: Option<Vec<String>>,
    /// Default `authorization_code` + `refresh_token`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_types: Option<Vec<String>>,
    /// Default `openid profile email offline_access`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    /// First-party app: no consent screen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_consent: Option<bool>,
    /// Seconds, 60 to 86 400.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token_ttl: Option<i64>,
    /// Seconds, 300 to 31 536 000.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token_ttl: Option<i64>,
    /// Update only: `true` stops new authorizations and tokens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
}

impl ClientInput {
    /// A client named `name` with server defaults.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: Some(name.into()),
            ..Self::default()
        }
    }

    /// Adds a redirect URI.
    #[must_use]
    pub fn redirect_uri(mut self, uri: impl Into<String>) -> Self {
        self.redirect_uris.get_or_insert_with(Vec::new).push(uri.into());
        self
    }

    /// Makes it a public client (PKCE, no secret).
    #[must_use]
    pub fn public(mut self) -> Self {
        self.client_type = Some("public".into());
        self
    }
}

/// A remembered consent of a user to a client.
#[derive(Debug, Clone, Deserialize)]
pub struct OidcConsent {
    /// Consent id.
    pub id: String,
    /// Client id.
    #[serde(default)]
    pub client_id: String,
    /// Client name.
    #[serde(default)]
    pub client_name: String,
    /// Realm.
    #[serde(default)]
    pub realm: String,
    /// Scopes granted.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Creation time.
    #[serde(default)]
    pub created_at: String,
    /// Update time.
    #[serde(default)]
    pub updated_at: String,
}

/// A page of consents.
#[derive(Debug, Clone, Deserialize)]
pub struct ConsentPage {
    /// Consents.
    #[serde(default)]
    pub items: Vec<OidcConsent>,
    /// Page size.
    #[serde(default)]
    pub limit: i64,
    /// Offset.
    #[serde(default)]
    pub offset: i64,
}

/// Login methods a realm enables.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RealmLoginMethods {
    /// Password.
    #[serde(default)]
    pub password: bool,
    /// Magic link.
    #[serde(default)]
    pub magic_link: bool,
    /// Email one-time code.
    #[serde(default)]
    pub email_otp: bool,
    /// Social / OIDC login.
    #[serde(default)]
    pub oauth: bool,
    /// Passkeys.
    #[serde(default)]
    pub passkey: bool,
    /// Phone (SMS).
    #[serde(default)]
    pub phone: bool,
    /// LDAP.
    #[serde(default)]
    pub ldap: bool,
}

/// Identity settings of a realm. Secrets are write-only: reads show them masked.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RealmIdentityConfig {
    /// Open sign-up.
    #[serde(default)]
    pub signup: bool,
    /// Enabled login methods.
    #[serde(default)]
    pub methods: RealmLoginMethods,
    /// Require a verified email to sign in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_verified_email: Option<bool>,
    /// Phone settings (`{"signup": bool}`).
    #[serde(default)]
    pub phone: Value,
    /// LDAP settings (secrets masked).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ldap: Option<Value>,
    /// Turnstile settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turnstile: Option<Value>,
}

/// A realm with its identity settings.
#[derive(Debug, Clone, Deserialize)]
pub struct RealmIdentity {
    /// Realm id.
    pub realm: String,
    /// Auth collection of the realm's users.
    #[serde(default)]
    pub collection: String,
    /// Settings.
    #[serde(default)]
    pub config: RealmIdentityConfig,
    /// Creation time.
    #[serde(default)]
    pub created_at: String,
    /// Update time.
    #[serde(default)]
    pub updated_at: String,
}

/// What a sign-in page needs to know about the request's realm (no credentials needed).
#[derive(Debug, Clone, Deserialize)]
pub struct RealmInfo {
    /// Realm.
    pub realm: String,
    /// User collection.
    #[serde(default)]
    pub collection: String,
    /// Sign-up open.
    #[serde(default)]
    pub signup: bool,
    /// Login methods.
    #[serde(default)]
    pub methods: RealmLoginMethods,
    /// Phone settings.
    #[serde(default)]
    pub phone: Value,
    /// Turnstile (`site_key`, `actions`).
    #[serde(default)]
    pub turnstile: Option<Value>,
    /// Social providers shown on the page.
    #[serde(default)]
    pub oauth_providers: Vec<Value>,
}

/// A new realm.
#[derive(Debug, Clone, Default, Serialize)]
pub struct NewRealm {
    /// Realm id (lowercase slug).
    pub id: String,
    /// Display name (default: the id).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Auth collection of the realm's users (default `<id>_users`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Identity settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<RealmIdentityConfig>,
}

impl NewRealm {
    /// A realm with server defaults.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ..Self::default()
        }
    }
}

/// An OpenID Connect signing key.
#[derive(Debug, Clone, Deserialize)]
pub struct OidcKey {
    /// Key id.
    pub kid: String,
    /// Algorithm.
    #[serde(default)]
    pub alg: String,
    /// `current`, `retired`, ...
    #[serde(default)]
    pub status: String,
    /// Creation time.
    #[serde(default)]
    pub created_at: String,
    /// Still verifies until.
    #[serde(default)]
    pub verify_until: Option<String>,
    /// Still used to verify tokens.
    #[serde(default)]
    pub verifying: bool,
}

/// Signing keys of a realm's OpenID provider.
#[derive(Debug, Clone, Deserialize)]
pub struct OidcKeys {
    /// Realm.
    #[serde(default)]
    pub realm: String,
    /// Kid of the key that signs now.
    #[serde(default)]
    pub current: Option<String>,
    /// Keys.
    #[serde(default)]
    pub items: Vec<OidcKey>,
}

/// Login provider kinds the server knows: the OAuth presets of ADR-0039 and the generic OIDC kind.
///
/// A provider's `kind` is a string on the wire; pass [`ProviderPreset::as_str`] as `kind` in a
/// [`RealmProviderConfig`]. The presets decide endpoints, scopes, claim mapping and email trust;
/// `microsoft`, `gitlab` and `keycloak` take `tenant` / `base_url` (see the `RealmProviderConfig` helpers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProviderPreset {
    /// Google.
    Google,
    /// GitHub.
    Github,
    /// Sign in with Apple (ES256 client-secret JWT, `form_post`).
    Apple,
    /// Facebook.
    Facebook,
    /// WeChat.
    Wechat,
    /// Discord.
    Discord,
    /// Any OpenID Connect issuer (discovery or explicit endpoints).
    Oidc,
    /// Microsoft Entra ID (`tenant`: `common`, `organizations`, `consumers` or a tenant GUID).
    Microsoft,
    /// GitLab.com or self-hosted GitLab (`base_url`).
    Gitlab,
    /// Sign in with Slack.
    Slack,
    /// Sign In with LinkedIn (OpenID Connect).
    Linkedin,
    /// Twitch.
    Twitch,
    /// Bitbucket Cloud.
    Bitbucket,
    /// Spotify (never trusts the address as verified).
    Spotify,
    /// A Keycloak realm (`base_url` + `tenant` = realm name).
    Keycloak,
}

impl ProviderPreset {
    /// The `kind` string on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Github => "github",
            Self::Apple => "apple",
            Self::Facebook => "facebook",
            Self::Wechat => "wechat",
            Self::Discord => "discord",
            Self::Oidc => "oidc",
            Self::Microsoft => "microsoft",
            Self::Gitlab => "gitlab",
            Self::Slack => "slack",
            Self::Linkedin => "linkedin",
            Self::Twitch => "twitch",
            Self::Bitbucket => "bitbucket",
            Self::Spotify => "spotify",
            Self::Keycloak => "keycloak",
        }
    }
}

/// A social login provider of a realm (secrets never leave the server).
#[derive(Debug, Clone, Deserialize)]
pub struct RealmProvider {
    /// Provider id.
    pub id: String,
    /// Realm.
    #[serde(default)]
    pub realm: String,
    /// Kind (`google`, `oidc`, ...).
    #[serde(default)]
    pub kind: String,
    /// Display name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// `file` for providers defined in `oauth-providers.json` (read-only).
    #[serde(default)]
    pub source: Option<String>,
    /// True for file-defined providers.
    #[serde(default)]
    pub read_only: bool,
    /// Every other field the server returned (client id, endpoints, scopes, masked secrets).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// The body of [`RealmProvidersApi::put`]: `kind`, client credentials and kind-specific settings.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RealmProviderConfig {
    /// Kind (see [`ProviderPreset`]).
    pub kind: String,
    /// Display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// Client id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Client secret (write-only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
    /// Extra fields the kind needs (`issuer`, `team_id`, `key_id`, `scopes`, ...).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

impl RealmProviderConfig {
    /// A preset provider with client credentials.
    #[must_use]
    pub fn preset(kind: ProviderPreset, client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Self {
            kind: kind.as_str().to_owned(),
            client_id: Some(client_id.into()),
            client_secret: Some(client_secret.into()),
            ..Self::default()
        }
    }

    /// Sets a kind-specific field (`issuer`, `team_id`, ...).
    #[must_use]
    pub fn set(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.extra.insert(key.to_owned(), value.into());
        self
    }

    /// Microsoft: `common` (default), `organizations`, `consumers` or a tenant GUID; Keycloak: the realm name.
    #[must_use]
    pub fn tenant(self, tenant: &str) -> Self {
        self.set("tenant", tenant)
    }

    /// GitLab and Keycloak: the server's base URL.
    #[must_use]
    pub fn base_url(self, url: &str) -> Self {
        self.set("base_url", url)
    }

    /// Microsoft multi-tenant (`common`, `organizations`): the tenant GUIDs allowed to sign in.
    #[must_use]
    pub fn allowed_tenants(self, tenants: &[&str]) -> Self {
        self.set("allowed_tenants", json!(tenants))
    }

    /// Free authorize parameters such as `prompt`, or Slack's `team` (protocol parameters are refused).
    #[must_use]
    pub fn authorize_param(mut self, key: &str, value: &str) -> Self {
        let entry = self
            .extra
            .entry("authorize_params".to_owned())
            .or_insert_with(|| json!({}));
        if let Value::Object(m) = entry {
            m.insert(key.to_owned(), json!(value));
        }
        self
    }
}

/// SMS gateway of a realm, secrets masked; `configured: false` means the instance gateway is used.
#[derive(Debug, Clone, Deserialize)]
pub struct SmsGateway {
    /// Realm.
    #[serde(default)]
    pub realm: Option<String>,
    /// Whether the realm has its own gateway.
    #[serde(default)]
    pub configured: Option<bool>,
    /// Provider (`dev`, `twilio`, `webhook`, ...).
    #[serde(default)]
    pub provider: Option<String>,
    /// Remaining provider-specific fields.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, Value>,
}

/// The phone number linked to the signed-in user.
#[derive(Debug, Clone, Deserialize)]
pub struct PhoneStatus {
    /// The number.
    #[serde(default)]
    pub phone: Option<String>,
    /// Verification time.
    #[serde(default)]
    pub verified_at: Option<String>,
}

/// A registered passkey.
#[derive(Debug, Clone, Deserialize)]
pub struct WebauthnCredential {
    /// Credential id.
    pub id: String,
    /// Name.
    #[serde(default)]
    pub name: Option<String>,
    /// User verification performed.
    #[serde(default)]
    pub user_verified: bool,
    /// Usable as a second factor.
    #[serde(default)]
    pub use_as_mfa: bool,
    /// Backup eligible.
    #[serde(default)]
    pub backup_eligible: bool,
    /// Signature counter.
    #[serde(default)]
    pub sign_count: i64,
    /// Creation time.
    #[serde(default)]
    pub created_at: String,
    /// Last use.
    #[serde(default)]
    pub last_used_at: Option<String>,
}

/// Identity administration and account security: `client.identity()`.
#[derive(Clone)]
pub struct IdentityApi {
    pub(crate) t: Arc<Transport>,
}

impl IdentityApi {
    /// API keys of every owner (administrators).
    #[must_use]
    pub fn api_keys(&self) -> ApiKeysApi {
        ApiKeysApi { t: self.t.clone() }
    }
    /// Personal access tokens of the caller.
    #[must_use]
    pub fn tokens(&self) -> TokensApi {
        TokensApi { t: self.t.clone() }
    }
    /// Service accounts and their keys.
    #[must_use]
    pub fn service_accounts(&self) -> ServiceAccountsApi {
        ServiceAccountsApi { t: self.t.clone() }
    }
    /// Administrators.
    #[must_use]
    pub fn admins(&self) -> AdminsApi {
        AdminsApi { t: self.t.clone() }
    }
    /// OAuth / OIDC client apps (administrators).
    #[must_use]
    pub fn clients(&self) -> OAuthClientsApi {
        OAuthClientsApi { t: self.t.clone() }
    }
    /// The signed-in user's consents to client apps.
    #[must_use]
    pub fn consents(&self) -> ConsentsApi {
        ConsentsApi { t: self.t.clone() }
    }
    /// Discovery documents of a realm's OpenID provider.
    #[must_use]
    pub fn oidc(&self) -> OidcApi {
        OidcApi { t: self.t.clone() }
    }
    /// Realms, their providers, SMS gateways and keys (administrators).
    #[must_use]
    pub fn realms(&self) -> RealmsApi {
        RealmsApi { t: self.t.clone() }
    }
    /// End users: lifecycle, sessions, export (administrators).
    #[must_use]
    pub fn users(&self) -> UsersAdminApi {
        UsersAdminApi { t: self.t.clone() }
    }
    /// "Sign in as" for support (administrators).
    #[must_use]
    pub fn impersonation(&self) -> ImpersonationApi {
        ImpersonationApi { t: self.t.clone() }
    }
    /// Phone numbers.
    #[must_use]
    pub fn phone(&self) -> PhoneApi {
        PhoneApi { t: self.t.clone() }
    }
    /// Passkeys (the browser ceremony stays with the caller).
    #[must_use]
    pub fn webauthn(&self) -> WebauthnApi {
        WebauthnApi { t: self.t.clone() }
    }
    /// LDAP sign-in.
    #[must_use]
    pub fn ldap(&self) -> LdapApi {
        LdapApi { t: self.t.clone() }
    }
    /// Self-service deactivate / delete / export.
    #[must_use]
    pub fn account(&self) -> AccountApi {
        AccountApi { t: self.t.clone() }
    }

    /// Authentication audit log (administrators), newest first.
    pub async fn audit(&self, q: &AuthAuditQuery) -> Result<Vec<AuthAuditEntry>> {
        let v = self
            .t
            .value(
                Req::get("/api/auth/audit")
                    .q_opt("event", q.event.as_deref())
                    .q_opt("subject_id", q.subject_id.as_deref())
                    .q_opt("before", q.before)
                    .q_opt("limit", q.limit),
            )
            .await?;
        items(&v, "audit")
    }

    /// The caller's recent sign-ins.
    pub async fn login_history(&self) -> Result<Vec<Value>> {
        let v = self.t.value(Req::get("/api/auth/login-history")).await?;
        items(&v, "login history")
    }

    /// Rotates the access-token signing key; returns the new key id.
    pub async fn rotate_signing_key(&self) -> Result<String> {
        let v = self
            .t
            .value(Req::post("/api/auth/keys/rotate").json_raw(json!({})))
            .await?;
        v.get("active_kid")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| Error::InvalidResponse("key rotation: missing active_kid".into()))
    }

    /// Rotates the OpenID Connect signing key of a realm (default: the request's); returns `(realm, kid)`.
    pub async fn rotate_oidc_key(&self, realm: Option<&str>) -> Result<(String, String)> {
        let v = self
            .t
            .value(Req::post("/api/auth/keys/oidc/rotate").json_value(json!({"realm": realm})))
            .await?;
        let get = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| Error::InvalidResponse(format!("oidc key rotation: missing {k}")))
        };
        Ok((get("realm")?, get("kid")?))
    }
}

/// API keys of every owner (administrators): `client.identity().api_keys()`. Create them under a
/// service account: [`ServiceAccountKeysApi`].
#[derive(Clone)]
pub struct ApiKeysApi {
    t: Arc<Transport>,
}

impl ApiKeysApi {
    /// Every API key and personal access token of the realm.
    pub async fn list(&self) -> Result<Vec<ApiKeyInfo>> {
        items(&self.t.value(Req::get("/api/auth/api-keys")).await?, "api keys")
    }

    /// Revokes a key at once.
    pub async fn revoke(&self, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/auth/api-keys/{}", seg(id))))
            .await
    }
}

/// Personal access tokens of the caller: `client.identity().tokens()`. Create one with
/// [`AuthApi::create_token`](crate::AuthApi::create_token).
#[derive(Clone)]
pub struct TokensApi {
    t: Arc<Transport>,
}

impl TokensApi {
    /// The caller's tokens (no secrets).
    pub async fn list(&self) -> Result<Vec<ApiKeyInfo>> {
        items(&self.t.value(Req::get("/api/auth/tokens")).await?, "tokens")
    }

    /// Revokes a token.
    pub async fn revoke(&self, id: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("/api/auth/tokens/{}", seg(id)))).await
    }
}

/// Service accounts: `client.identity().service_accounts()`.
#[derive(Clone)]
pub struct ServiceAccountsApi {
    t: Arc<Transport>,
}

impl ServiceAccountsApi {
    /// Keys of one service account.
    #[must_use]
    pub fn keys(&self) -> ServiceAccountKeysApi {
        ServiceAccountKeysApi { t: self.t.clone() }
    }

    /// Every service account.
    pub async fn list(&self) -> Result<Vec<ServiceAccount>> {
        items(
            &self.t.value(Req::get("/api/auth/service-accounts")).await?,
            "service accounts",
        )
    }

    /// Creates a service account.
    pub async fn create(&self, input: &NewServiceAccount) -> Result<ServiceAccount> {
        self.t.json(Req::post("/api/auth/service-accounts").json(input)?).await
    }

    /// One service account.
    pub async fn get(&self, id: &str) -> Result<ServiceAccount> {
        self.t
            .json(Req::get(format!("/api/auth/service-accounts/{}", seg(id))))
            .await
    }

    /// Deletes the account and revokes its keys.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/auth/service-accounts/{}", seg(id))))
            .await
    }
}

/// API keys of one service account: `client.identity().service_accounts().keys()`.
#[derive(Clone)]
pub struct ServiceAccountKeysApi {
    t: Arc<Transport>,
}

impl ServiceAccountKeysApi {
    /// Keys of the account (no secrets).
    pub async fn list(&self, service_account: &str) -> Result<Vec<ApiKeyInfo>> {
        let v = self
            .t
            .value(Req::get(format!(
                "/api/auth/service-accounts/{}/keys",
                seg(service_account)
            )))
            .await?;
        items(&v, "service account keys")
    }

    /// Creates a key. The secret (`key`) is only returned here.
    pub async fn create(&self, service_account: &str, input: &NewApiKey) -> Result<AuthKeyCreated> {
        self.t
            .json(Req::post(format!("/api/auth/service-accounts/{}/keys", seg(service_account))).json(input)?)
            .await
    }
}

/// Administrators: `client.identity().admins()`.
#[derive(Clone)]
pub struct AdminsApi {
    t: Arc<Transport>,
}

impl AdminsApi {
    /// Every administrator.
    pub async fn list(&self) -> Result<Vec<AdminAccount>> {
        items(&self.t.value(Req::get("/api/auth/admins")).await?, "admins")
    }

    /// Creates an administrator.
    pub async fn create(&self, email: &str, password: &str) -> Result<AdminAccount> {
        self.t
            .json(Req::post("/api/auth/admins").json_raw(json!({"email": email, "password": password})))
            .await
    }
}

/// Client apps of the built-in OpenID Connect provider (administrators): `client.identity().clients()`.
#[derive(Clone)]
pub struct OAuthClientsApi {
    t: Arc<Transport>,
}

impl OAuthClientsApi {
    /// Clients of the caller's realm; platform administrators can pass another `realm`.
    pub async fn list(&self, realm: Option<&str>) -> Result<Vec<OAuthClient>> {
        items(
            &self
                .t
                .value(Req::get("/api/auth/clients").q_opt("realm", realm))
                .await?,
            "clients",
        )
    }

    /// Creates a client. The `client_secret` of a confidential client is shown once.
    pub async fn create(&self, input: &ClientInput) -> Result<CreatedOAuthClient> {
        self.t.json(Req::post("/api/auth/clients").json(input)?).await
    }

    /// One client.
    pub async fn get(&self, client_id: &str) -> Result<OAuthClient> {
        self.t
            .json(Req::get(format!("/api/auth/clients/{}", seg(client_id))))
            .await
    }

    /// Changes the listed fields; the rest stay. `disabled: Some(true)` stops new authorizations and tokens.
    pub async fn update(&self, client_id: &str, patch: &ClientInput) -> Result<OAuthClient> {
        self.t
            .json(Req::patch(format!("/api/auth/clients/{}", seg(client_id))).json(patch)?)
            .await
    }

    /// Deletes a client.
    pub async fn delete(&self, client_id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/auth/clients/{}", seg(client_id))))
            .await
    }

    /// Issues a new secret (shown once). The old one keeps working for `grace_seconds` when given.
    pub async fn rotate_secret(&self, client_id: &str, grace_seconds: Option<i64>) -> Result<CreatedOAuthClient> {
        self.t
            .json(
                Req::post(format!("/api/auth/clients/{}/secret", seg(client_id)))
                    .json_value(json!({"grace_seconds": grace_seconds})),
            )
            .await
    }
}

/// The signed-in user's remembered consents: `client.identity().consents()`.
#[derive(Clone)]
pub struct ConsentsApi {
    t: Arc<Transport>,
}

impl ConsentsApi {
    /// The caller's consents.
    pub async fn list(&self) -> Result<Vec<OidcConsent>> {
        items(&self.t.value(Req::get("/api/auth/consents")).await?, "consents")
    }

    /// Withdraws a consent.
    pub async fn revoke(&self, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/auth/consents/{}", seg(id))))
            .await
    }
}

/// Public discovery documents of a realm's OpenID provider: `client.identity().oidc()`. The
/// protocol endpoints (authorize, token, ...) are for OIDC libraries.
#[derive(Clone)]
pub struct OidcApi {
    t: Arc<Transport>,
}

impl OidcApi {
    /// `<base>/api/auth/oidc/<realm>`: the issuer to give an OIDC library.
    #[must_use]
    pub fn issuer(&self, realm: &str) -> String {
        self.t.url(&format!("/api/auth/oidc/{}", seg(realm)))
    }

    /// The `.well-known/openid-configuration` URL.
    #[must_use]
    pub fn discovery_url(&self, realm: &str) -> String {
        format!("{}/.well-known/openid-configuration", self.issuer(realm))
    }

    /// The discovery document.
    pub async fn discovery(&self, realm: &str) -> Result<Value> {
        self.t
            .value(
                Req::get(format!(
                    "/api/auth/oidc/{}/.well-known/openid-configuration",
                    seg(realm)
                ))
                .anonymous(),
            )
            .await
    }

    /// Public signing keys (JWKS).
    pub async fn jwks(&self, realm: &str) -> Result<Value> {
        self.t
            .value(Req::get(format!("/api/auth/oidc/{}/jwks", seg(realm))).anonymous())
            .await
    }
}

/// Realms and their identity settings (administrators): `client.identity().realms()`.
#[derive(Clone)]
pub struct RealmsApi {
    t: Arc<Transport>,
}

impl RealmsApi {
    /// Social providers of one realm.
    #[must_use]
    pub fn providers(&self) -> RealmProvidersApi {
        RealmProvidersApi { t: self.t.clone() }
    }

    /// SMS gateway of one realm.
    #[must_use]
    pub fn sms(&self) -> RealmSmsApi {
        RealmSmsApi { t: self.t.clone() }
    }

    /// What a sign-in page needs for the request's realm (`X-Forgebase-Realm`). No credentials needed.
    pub async fn info(&self) -> Result<RealmInfo> {
        self.t.json(Req::get("/api/auth/realm").anonymous()).await
    }

    /// Every realm.
    pub async fn list(&self) -> Result<Vec<RealmIdentity>> {
        items(&self.t.value(Req::get("/api/auth/realms")).await?, "realms")
    }

    /// One realm.
    pub async fn get(&self, realm: &str) -> Result<RealmIdentity> {
        self.t.json(Req::get(format!("/api/auth/realms/{}", seg(realm)))).await
    }

    /// Creates the realm together with its identity settings.
    pub async fn create(&self, input: &NewRealm) -> Result<RealmIdentity> {
        self.t.json(Req::post("/api/auth/realms").json(input)?).await
    }

    /// Replaces the identity settings (and, once, the user collection).
    pub async fn update(
        &self,
        realm: &str,
        collection: Option<&str>,
        config: &RealmIdentityConfig,
    ) -> Result<RealmIdentity> {
        let body = json!({"collection": collection, "config": config});
        self.t
            .json(Req::put(format!("/api/auth/realms/{}", seg(realm))).json_value(body))
            .await
    }

    /// Signing keys of the realm's OpenID provider.
    pub async fn keys(&self, realm: &str) -> Result<OidcKeys> {
        self.t
            .json(Req::get(format!("/api/auth/realms/{}/keys", seg(realm))))
            .await
    }

    /// Consents users of the realm gave to client apps.
    pub async fn consents(
        &self,
        realm: &str,
        client_id: Option<&str>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<ConsentPage> {
        self.t
            .json(
                Req::get(format!("/api/auth/realms/{}/consents", seg(realm)))
                    .q_opt("client_id", client_id)
                    .q_opt("limit", limit)
                    .q_opt("offset", offset),
            )
            .await
    }

    /// Withdraws a consent.
    pub async fn revoke_consent(&self, realm: &str, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!(
                "/api/auth/realms/{}/consents/{}",
                seg(realm),
                seg(id)
            )))
            .await
    }
}

/// Social providers of one realm (administrators): `client.identity().realms().providers()`.
/// Secrets are write-only.
#[derive(Clone)]
pub struct RealmProvidersApi {
    t: Arc<Transport>,
}

impl RealmProvidersApi {
    /// The realm's providers.
    pub async fn list(&self, realm: &str) -> Result<Vec<RealmProvider>> {
        let v = self
            .t
            .value(Req::get(format!("/api/auth/realms/{}/oauth-providers", seg(realm))))
            .await?;
        items(&v, "providers")
    }

    /// One provider.
    pub async fn get(&self, realm: &str, id: &str) -> Result<RealmProvider> {
        self.t
            .json(Req::get(format!(
                "/api/auth/realms/{}/oauth-providers/{}",
                seg(realm),
                seg(id)
            )))
            .await
    }

    /// Creates or replaces a provider (`id` goes in the URL, not the body). Providers from
    /// `oauth-providers.json` are read-only.
    pub async fn put(&self, realm: &str, id: &str, config: &RealmProviderConfig) -> Result<RealmProvider> {
        self.t
            .json(Req::put(format!("/api/auth/realms/{}/oauth-providers/{}", seg(realm), seg(id))).json(config)?)
            .await
    }

    /// Removes a provider.
    pub async fn delete(&self, realm: &str, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!(
                "/api/auth/realms/{}/oauth-providers/{}",
                seg(realm),
                seg(id)
            )))
            .await
    }
}

/// SMS gateway of one realm (administrators): `client.identity().realms().sms()`.
#[derive(Clone)]
pub struct RealmSmsApi {
    t: Arc<Transport>,
}

impl RealmSmsApi {
    /// The gateway, secrets masked.
    pub async fn get(&self, realm: &str) -> Result<SmsGateway> {
        self.t
            .json(Req::get(format!("/api/auth/realms/{}/sms", seg(realm))))
            .await
    }

    /// `{"provider": "dev"}`, `{"provider": "twilio", "account_sid", "auth_token", "from" | "messaging_service_sid"}`
    /// or `{"provider": "webhook", "url", "bearer_token"?}` (more in ADR-0039).
    pub async fn put(&self, realm: &str, config: &Value) -> Result<SmsGateway> {
        self.t
            .json(Req::put(format!("/api/auth/realms/{}/sms", seg(realm))).json_raw(config.clone()))
            .await
    }

    /// Back to the instance gateway.
    pub async fn delete(&self, realm: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/auth/realms/{}/sms", seg(realm))))
            .await
    }
}

/// Administrator view of end users and their lifecycle: `client.identity().users()`.
#[derive(Clone)]
pub struct UsersAdminApi {
    t: Arc<Transport>,
}

fn user_path(collection: &str, id: &str, tail: &str) -> String {
    format!("/api/auth/users/{}/{}{tail}", seg(collection), seg(id))
}

impl UsersAdminApi {
    /// The user record with security details.
    pub async fn get(&self, collection: &str, id: &str) -> Result<Value> {
        self.t.value(Req::get(user_path(collection, id, ""))).await
    }

    /// Lifecycle state.
    pub async fn status(&self, collection: &str, id: &str) -> Result<AccountStatus> {
        self.t.json(Req::get(user_path(collection, id, "/status"))).await
    }

    /// Blocks sign-in; `until` (Unix seconds) ends the suspension by itself.
    pub async fn suspend(
        &self,
        collection: &str,
        id: &str,
        reason: Option<&str>,
        until: Option<i64>,
    ) -> Result<AccountStatus> {
        self.t
            .json(
                Req::post(user_path(collection, id, "/suspend")).json_value(json!({"reason": reason, "until": until})),
            )
            .await
    }

    /// Lifts a suspension.
    pub async fn unsuspend(&self, collection: &str, id: &str) -> Result<AccountStatus> {
        self.t
            .json(Req::post(user_path(collection, id, "/unsuspend")).json_raw(json!({})))
            .await
    }

    /// Deactivates the account.
    pub async fn deactivate(&self, collection: &str, id: &str, reason: Option<&str>) -> Result<AccountStatus> {
        self.t
            .json(Req::post(user_path(collection, id, "/deactivate")).json_value(json!({"reason": reason})))
            .await
    }

    /// Reactivates the account.
    pub async fn reactivate(&self, collection: &str, id: &str) -> Result<AccountStatus> {
        self.t
            .json(Req::post(user_path(collection, id, "/reactivate")).json_raw(json!({})))
            .await
    }

    /// Strips personal data but keeps the record (GDPR erasure that preserves references).
    pub async fn anonymise(&self, collection: &str, id: &str) -> Result<Value> {
        self.t
            .value(Req::post(user_path(collection, id, "/anonymise")).json_raw(json!({})))
            .await
    }

    /// Deletes the user and their credentials.
    pub async fn delete(&self, collection: &str, id: &str) -> Result<()> {
        self.t
            .unit(Req::post(user_path(collection, id, "/delete")).json_raw(json!({})))
            .await
    }

    /// Everything stored about the user (data portability).
    pub async fn export(&self, collection: &str, id: &str) -> Result<Value> {
        self.t.value(Req::get(user_path(collection, id, "/export"))).await
    }

    /// The user's sessions.
    pub async fn sessions(&self, collection: &str, id: &str) -> Result<Vec<SessionInfo>> {
        let v = self.t.value(Req::get(user_path(collection, id, "/sessions"))).await?;
        items(&v, "sessions")
    }

    /// Signs the user out everywhere; returns the number of sessions revoked.
    pub async fn revoke_sessions(&self, collection: &str, id: &str) -> Result<u64> {
        let v = self
            .t
            .value(Req::post(user_path(collection, id, "/sessions/revoke")).json_raw(json!({})))
            .await?;
        Ok(v.get("revoked").and_then(Value::as_u64).unwrap_or(0))
    }

    /// The user's recent sign-ins.
    pub async fn login_history(&self, collection: &str, id: &str, limit: Option<u32>) -> Result<Vec<Value>> {
        let v = self
            .t
            .value(Req::get(user_path(collection, id, "/login-history")).q_opt("limit", limit))
            .await?;
        items(&v, "login history")
    }
}

/// "Sign in as" for support (administrators): every grant is audited and short lived.
#[derive(Clone)]
pub struct ImpersonationApi {
    t: Arc<Transport>,
}

impl ImpersonationApi {
    /// `principal` is `user:<collection>/<id>`; `ttl_minutes` defaults to 15. Administrators and
    /// service accounts cannot be impersonated.
    pub async fn start(&self, principal: &str, reason: &str, ttl_minutes: Option<u32>) -> Result<ImpersonationGrant> {
        self.t
            .json(
                Req::post("/api/auth/impersonate")
                    .json_value(json!({"principal": principal, "reason": reason, "ttl_minutes": ttl_minutes})),
            )
            .await
    }

    /// Grants, newest first.
    pub async fn list(&self) -> Result<Vec<ImpersonationInfo>> {
        items(
            &self.t.value(Req::get("/api/auth/impersonations")).await?,
            "impersonations",
        )
    }

    /// Ends a grant (and its session) before it expires; `id` is the grant's `session_id`.
    pub async fn end(&self, id: &str) -> Result<()> {
        self.t
            .unit(Req::post(format!("/api/auth/impersonations/{}/end", seg(id))).json_raw(json!({})))
            .await
    }
}

/// Phone numbers: SMS sign-in and linking a number to the account.
#[derive(Clone)]
pub struct PhoneApi {
    t: Arc<Transport>,
}

impl PhoneApi {
    /// Texts a sign-in code (always accepted, whether or not the number can sign in).
    pub async fn request_code(&self, phone: &str, collection: Option<&str>) -> Result<()> {
        self.t
            .unit(
                Req::post("/api/auth/phone/request")
                    .json_value(json!({"phone": phone, "collection": collection}))
                    .anonymous(),
            )
            .await
    }

    /// Signs in with the code; stores the session like [`AuthApi::sign_in`](crate::AuthApi::sign_in).
    pub async fn sign_in(&self, phone: &str, code: &str, opts: &SignInOptions) -> Result<SignInResult> {
        let body = opts.apply(json!({"phone": phone, "code": code}));
        let v = self
            .t
            .value(Req::post("/api/auth/phone/verify").json_value(body).anonymous())
            .await?;
        self.t.accept(&v)
    }

    /// The number linked to the signed-in user.
    pub async fn get(&self) -> Result<PhoneStatus> {
        self.t.json(Req::get("/api/auth/phone")).await
    }

    /// Starts linking a number: a code is texted to it.
    pub async fn add(&self, phone: &str) -> Result<()> {
        self.t
            .unit(Req::post("/api/auth/phone/add").json_raw(json!({"phone": phone})))
            .await
    }

    /// Finishes linking with the texted code.
    pub async fn confirm(&self, phone: &str, code: &str) -> Result<PhoneStatus> {
        self.t
            .json(Req::post("/api/auth/phone/confirm").json_raw(json!({"phone": phone, "code": code})))
            .await
    }

    /// Unlinks the number.
    pub async fn remove(&self) -> Result<()> {
        self.t.unit(Req::delete("/api/auth/phone")).await
    }
}

/// Passkeys (WebAuthn). The `publicKey` options go to the platform's credential API; send the
/// result back as `credential`.
#[derive(Clone)]
pub struct WebauthnApi {
    t: Arc<Transport>,
}

impl WebauthnApi {
    /// Registration options for the signed-in user. `use_as_mfa` makes the key a second factor.
    pub async fn register_options(&self, use_as_mfa: Option<bool>) -> Result<Value> {
        self.t
            .value(Req::post("/api/auth/webauthn/register/options").json_value(json!({"use_as_mfa": use_as_mfa})))
            .await
    }

    /// Finishes registration.
    pub async fn register_verify(
        &self,
        ceremony_id: &str,
        credential: &Value,
        name: Option<&str>,
    ) -> Result<WebauthnCredential> {
        let v = self
            .t
            .value(
                Req::post("/api/auth/webauthn/register/verify")
                    .json_value(json!({"ceremony_id": ceremony_id, "credential": credential, "name": name})),
            )
            .await?;
        decode(v.get("credential").cloned().unwrap_or(Value::Null), "passkey")
    }

    /// Sign-in options; omit `email` for a discoverable (usernameless) passkey.
    pub async fn sign_in_options(&self, collection: Option<&str>, email: Option<&str>) -> Result<Value> {
        self.t
            .value(
                Req::post("/api/auth/webauthn/login/options")
                    .json_value(json!({"collection": collection, "email": email}))
                    .anonymous(),
            )
            .await
    }

    /// Completes the passkey sign-in and stores the session.
    pub async fn sign_in(&self, ceremony_id: &str, credential: &Value, opts: &SignInOptions) -> Result<SignInResult> {
        let body = json!({"ceremony_id": ceremony_id, "credential": credential, "device_name": opts.device_name});
        let v = self
            .t
            .value(
                Req::post("/api/auth/webauthn/login/verify")
                    .json_value(body)
                    .anonymous(),
            )
            .await?;
        self.t.accept(&v)
    }

    /// Second-factor options after a sign-in returned an MFA challenge.
    pub async fn mfa_options(&self, mfa_token: &str) -> Result<Value> {
        self.t
            .value(
                Req::post("/api/auth/webauthn/mfa/options")
                    .json_raw(json!({"mfa_token": mfa_token}))
                    .anonymous(),
            )
            .await
    }

    /// Completes the second factor with a passkey and stores the session.
    pub async fn verify_mfa(&self, mfa_token: &str, ceremony_id: &str, credential: &Value) -> Result<SignInResult> {
        let body = json!({"mfa_token": mfa_token, "ceremony_id": ceremony_id, "credential": credential});
        let v = self
            .t
            .value(Req::post("/api/auth/webauthn/mfa/verify").json_raw(body).anonymous())
            .await?;
        self.t.accept(&v)
    }

    /// The signed-in user's passkeys.
    pub async fn credentials(&self) -> Result<Vec<WebauthnCredential>> {
        let v = self.t.value(Req::get("/api/auth/webauthn/credentials")).await?;
        decode(
            v.get("credentials")
                .cloned()
                .unwrap_or_else(|| Value::Array(Vec::new())),
            "passkeys",
        )
    }

    /// Removes a passkey.
    pub async fn delete_credential(&self, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/auth/webauthn/credentials/{}", seg(id))))
            .await
    }
}

/// LDAP directory sign-in (when the realm enables it).
#[derive(Clone)]
pub struct LdapApi {
    t: Arc<Transport>,
}

impl LdapApi {
    /// Signs in with directory credentials and stores the session.
    pub async fn sign_in(&self, username: &str, password: &str, opts: &SignInOptions) -> Result<SignInResult> {
        let body = opts.apply(json!({"username": username, "password": password}));
        let v = self
            .t
            .value(Req::post("/api/auth/ldap/login").json_value(body).anonymous())
            .await?;
        self.t.accept(&v)
    }
}

/// Self-service account lifecycle of the signed-in user (GDPR): `client.identity().account()`.
#[derive(Clone)]
pub struct AccountApi {
    t: Arc<Transport>,
}

impl AccountApi {
    /// Deactivates the account; an administrator (or the user, signing in again) can reactivate
    /// it. Password accounts re-enter their password.
    pub async fn deactivate(&self, current_password: Option<&str>) -> Result<Value> {
        self.t
            .value(Req::post("/api/auth/account/deactivate").json_value(json!({"current_password": current_password})))
            .await
    }

    /// Deletes the account, or with `anonymise` keeps an anonymised record.
    pub async fn delete(&self, current_password: Option<&str>, anonymise: bool) -> Result<Value> {
        self.t
            .value(
                Req::post("/api/auth/account/delete")
                    .json_value(json!({"current_password": current_password, "anonymise": anonymise.then_some(true)})),
            )
            .await
    }

    /// Everything stored about the signed-in user.
    pub async fn export(&self) -> Result<Value> {
        self.t.value(Req::get("/api/auth/account/export")).await
    }
}
