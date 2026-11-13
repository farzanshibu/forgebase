//! ForgeBase authentication core (FB-20, ADR-0004).
//!
//! [`AuthService`] turns credentials into identities and manages everything
//! around them:
//!
//! * **Passwords**: Argon2id ([`password`]); also the data core's
//!   `SecretHasher` for `password` fields.
//! * **End users** of auth collections (default `users`): signup, login,
//!   email verification, password reset and change, magic links, email OTP.
//! * **Administrators** (superusers, `_forgebase_admins`), created with
//!   `forgebase admin create`.
//! * **Sessions** (one per device): short-lived HS256 access tokens, opaque
//!   rotating refresh tokens with reuse detection, optional HttpOnly cookie
//!   mode with CSRF protection, device list, revocation, login history.
//! * **MFA**: TOTP (RFC 6238) with single-use recovery codes.
//! * **Machine credentials**: service accounts with API keys, personal access
//!   tokens; both scoped ([`scopes`]).
//! * **Abuse protection**: per-IP rate limits, per-account lockout,
//!   constant-time comparisons, uniform answers for unknown accounts.
//! * **Audit**: every auth event in `_forgebase_auth_audit`.
//!
//! [`http`] exposes it: an Axum layer that resolves a bearer token, API key
//! or session cookie into the data core's [`AccessContext`] request extension
//! (anonymous when absent), and the `/api/auth/*` routes.
//!
//! Extension points for later cards: [`Subject`] and [`AuthMethod`] are
//! `#[non_exhaustive]`; OAuth/OIDC and WebAuthn (FB-22) create sessions
//! through [`AuthService::issue_session`]; the policy engine (FB-21) reads
//! [`Authenticated`] (scopes, method, MFA) next to the `AccessContext`.

pub mod clock;
pub mod crypto;
mod error;
pub mod http;
mod identity_admin;
mod identity_http;
pub mod idp;
pub mod ldap;
pub mod lifecycle;
pub mod limits;
pub mod mailer;
pub mod net;
mod openapi;
pub mod password;
pub mod phone;
pub mod realm;
pub mod scopes;
mod service;
pub mod setup;
pub mod sms;
pub mod social;
pub mod store;
pub mod turnstile;
pub mod webauthn;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use forgebase_data::AccessContext;
use serde::Serialize;

pub use clock::{Clock, ManualClock, SystemClock};
pub use error::AuthError;
pub use forgebase_data as data;
pub use mailer::{DevMailer, Mail, MailKind, Mailer};
pub use openapi::openapi;
pub use password::{Argon2Config, PasswordHasher};
pub use scopes::Scope;
pub use service::{
    AUDIT_PAGE_MAX, AdminScope, AdminView, ApiKeyView, AuditEntry, AuditPage, AuditQuery, AuditRelated, AuditSearch,
    AuditSummary, AuthService, BoundKey, ClientInfo, CreatedKey,
    IDENTITY_ADMIN, ImpersonationGrant, ImpersonationView, KeyOptions, LoginOptions, LoginOutcome, RealmAdminCheck,
    ServiceAccountView, SessionTokens, SessionView, TotpSetup, UserView,
};

/// Collection name FB-20 used for service-account principals before the data
/// core gained `Principal::Service` (FB-21). Kept for compatibility; no
/// principal carries it any more.
pub const SERVICE_ACCOUNT_COLLECTION: &str = "_service_accounts";

/// Auth settings. Defaults are production-safe.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// File holding the 32-byte master key (created 0600 if missing).
    pub master_key_path: PathBuf,
    /// Argon2id cost.
    pub argon2: Argon2Config,
    /// Auth collection created at startup if missing (`None`: none).
    pub default_collection: Option<String>,
    /// Lifetime of access tokens.
    pub access_token_ttl: Duration,
    /// Absolute lifetime of a session (and its refresh tokens).
    pub session_ttl: Duration,
    /// A session unused for this long expires.
    pub session_idle_timeout: Duration,
    /// Rotate the signing key after this age.
    pub signing_key_rotation: Duration,
    /// Lifetime of verification links.
    pub verify_email_ttl: Duration,
    /// Lifetime of password-reset links.
    pub password_reset_ttl: Duration,
    /// Lifetime of magic links.
    pub magic_link_ttl: Duration,
    /// Lifetime of email OTP codes.
    pub email_otp_ttl: Duration,
    /// Lifetime of an MFA challenge after the first factor.
    pub mfa_challenge_ttl: Duration,
    /// Wrong guesses allowed per one-time code before it is burnt.
    pub otp_max_attempts: i64,
    /// Refuse password login until the email is verified.
    pub require_verified_email: bool,
    /// Consecutive failures that lock an account.
    pub lockout_threshold: i64,
    /// Window in which failures are counted.
    pub lockout_window: Duration,
    /// First lock duration; doubles with each further lock, capped at 24 h.
    pub lockout_duration: Duration,
    /// Credential attempts (login, OTP, MFA, refresh) per IP per minute.
    pub ip_attempts_per_minute: u32,
    /// Mail-sending requests (signup, reset, magic link, OTP) per IP per hour.
    pub ip_mail_per_hour: u32,
    /// Base URL used in links sent by mail.
    pub public_url: String,
    /// `Secure` attribute on cookies (turn off only for plain-HTTP development).
    pub cookie_secure: bool,
    /// Trust `X-Forwarded-For` for the client IP (only behind a proxy you
    /// run): the last entry, the one the proxy appended, is the client.
    pub trust_forwarded_for: bool,
    /// Let the default outbound HTTPS client reach loopback, private and
    /// link-local addresses. Off (the default), requests to them are refused
    /// before connecting (SSRF guard, FB-80).
    pub allow_private_outbound: bool,
    /// TOTP issuer shown in authenticator apps.
    pub totp_issuer: String,
    /// Extra trust anchors (PEM) for outbound HTTPS and LDAPS, added to the
    /// webpki-roots set (feature `https`).
    pub tls_ca_file: Option<PathBuf>,
    /// Time source (tests replace it with a [`ManualClock`]).
    pub clock: Arc<dyn Clock>,
    /// ForgeBase as identity provider: OAuth 2.0 / OpenID Connect (FB-26).
    pub idp: idp::IdpConfig,
}

impl AuthConfig {
    /// Defaults for a data directory.
    pub fn for_data_dir(data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        Self {
            master_key_path: data_dir.join("auth").join("master.key"),
            argon2: Argon2Config::default(),
            default_collection: Some("users".into()),
            access_token_ttl: Duration::from_secs(15 * 60),
            session_ttl: Duration::from_secs(30 * 24 * 3600),
            session_idle_timeout: Duration::from_secs(7 * 24 * 3600),
            signing_key_rotation: Duration::from_secs(30 * 24 * 3600),
            verify_email_ttl: Duration::from_secs(24 * 3600),
            password_reset_ttl: Duration::from_secs(30 * 60),
            magic_link_ttl: Duration::from_secs(15 * 60),
            email_otp_ttl: Duration::from_secs(10 * 60),
            mfa_challenge_ttl: Duration::from_secs(5 * 60),
            otp_max_attempts: 5,
            require_verified_email: false,
            lockout_threshold: 5,
            lockout_window: Duration::from_secs(15 * 60),
            lockout_duration: Duration::from_secs(15 * 60),
            ip_attempts_per_minute: 30,
            ip_mail_per_hour: 20,
            public_url: "http://127.0.0.1:8090".into(),
            cookie_secure: true,
            trust_forwarded_for: false,
            allow_private_outbound: false,
            totp_issuer: "ForgeBase".into(),
            tls_ca_file: None,
            clock: Arc::new(SystemClock),
            idp: idp::IdpConfig::default(),
        }
    }
}

/// Who an authenticated caller is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Subject {
    /// A record of an auth collection.
    User {
        /// Auth collection.
        collection: String,
        /// Record id.
        id: String,
    },
    /// An administrator.
    Admin {
        /// Administrator id.
        id: String,
    },
    /// A service account (machine).
    ServiceAccount {
        /// Service-account id.
        id: String,
        /// Whether it has the `admin` role.
        admin: bool,
    },
}

impl Subject {
    /// The data core's view of this subject.
    pub fn access_context(&self) -> AccessContext {
        match self {
            Subject::User { collection, id } => AccessContext::user(collection, id),
            Subject::Admin { id } => AccessContext::admin(id),
            Subject::ServiceAccount { id, admin } => AccessContext::service(id, *admin),
        }
    }

    /// Administrator, or a service account with the admin role.
    pub fn is_admin(&self) -> bool {
        matches!(
            self,
            Subject::Admin { .. } | Subject::ServiceAccount { admin: true, .. }
        )
    }

    /// Short actor string for the audit log.
    pub fn actor(&self) -> String {
        match self {
            Subject::User { collection, id } => format!("user:{collection}/{id}"),
            Subject::Admin { id } => format!("admin:{id}"),
            Subject::ServiceAccount { id, .. } => format!("service_account:{id}"),
        }
    }
}

/// How the current request authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CredentialKind {
    /// `Authorization: Bearer <access token>`.
    AccessToken,
    /// Session cookie (CSRF-checked on unsafe methods).
    SessionCookie,
    /// Service-account API key.
    ApiKey,
    /// Personal access token.
    PersonalAccessToken,
}

/// How a session was established (first factor).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthMethod {
    /// Email + password.
    Password,
    /// Magic link.
    MagicLink,
    /// Email one-time code.
    EmailOtp,
    /// Social login (OAuth 2 / OpenID Connect provider).
    Oauth,
    /// Passkey (WebAuthn).
    Passkey,
    /// Phone number + SMS code.
    Sms,
    /// LDAP directory bind.
    Ldap,
    /// Administrator impersonation (ADR-0014).
    Impersonation,
}

impl AuthMethod {
    fn as_str(self) -> &'static str {
        match self {
            AuthMethod::Password => "password",
            AuthMethod::MagicLink => "magic_link",
            AuthMethod::EmailOtp => "email_otp",
            AuthMethod::Oauth => "oauth",
            AuthMethod::Passkey => "passkey",
            AuthMethod::Sms => "sms",
            AuthMethod::Ldap => "ldap",
            AuthMethod::Impersonation => "impersonation",
        }
    }
}

/// The authenticated identity of a request; a request extension next to the
/// [`AccessContext`] (absent for anonymous requests).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Authenticated {
    /// Who.
    pub subject: Subject,
    /// Which credential.
    pub credential: CredentialKind,
    /// Session id (tokens, cookies) or key id (API keys, PATs).
    pub credential_id: String,
    /// Scopes of an API key or PAT; `None` = unrestricted (sessions).
    pub scopes: Option<Vec<Scope>>,
    /// Whether the session passed a second factor.
    pub mfa: bool,
    /// Administrator acting as this subject (impersonation session, ADR-0014).
    pub impersonator: Option<String>,
}

impl Authenticated {
    /// The data core context for this identity (with the credential's scopes).
    pub fn access_context(&self, request_id: Option<&str>) -> AccessContext {
        let mut ctx = self.subject.access_context();
        if let Some(admin) = &self.impersonator {
            ctx = ctx.with_impersonator(admin.clone());
        }
        if let Some(scopes) = &self.scopes {
            ctx = ctx.with_scopes(scopes.iter().map(|s| s.as_str().to_owned()).collect());
        }
        match request_id {
            Some(id) => ctx.with_request_id(id),
            None => ctx,
        }
    }

    /// Session id, for session-based credentials.
    pub fn session_id(&self) -> Option<&str> {
        matches!(
            self.credential,
            CredentialKind::AccessToken | CredentialKind::SessionCookie
        )
        .then_some(self.credential_id.as_str())
    }
}
