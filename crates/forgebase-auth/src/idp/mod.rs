//! ForgeBase as identity provider (FB-26, ADR-0019).
//!
//! * [`clients`]: Client Apps per realm (confidential or public, exact
//!   redirect URI allow list, allowed grants and scopes, hashed secrets with
//!   rotation) and client authentication at the token endpoint.
//! * [`keys`]: ES256 signing keys per realm (rotation, `kid`, JWKS).
//! * [`flow`]: the authorization endpoint (code + PKCE, `prompt`, `max_age`),
//!   consent and remembered grants, single sign-on sessions, device
//!   authorization (RFC 8628) and RP-initiated logout.
//! * [`tokens`]: the token endpoint (authorization code, refresh with
//!   rotation and reuse detection, client credentials, device code), ID
//!   tokens, userinfo, introspection (RFC 7662) and revocation (RFC 7009).
//! * [`http`]: the routes, `/api/auth/oidc/{realm}/…` (protocol) and
//!   `/api/auth/clients`, `/api/auth/consents` (management).
//!
//! The issuer of a realm is `<public_url>/api/auth/oidc/<realm>`; its
//! discovery document is `<issuer>/.well-known/openid-configuration`.

pub mod clients;
pub mod flow;
pub(crate) mod http;
pub mod keys;
pub mod tokens;

use std::collections::{BTreeSet, HashMap};
use std::sync::RwLock;
use std::time::Duration;

use axum::http::StatusCode;
use serde::Serialize;

use crate::AuthError;
use crate::scopes::Scope;

pub use clients::{ClientInput, ClientPatch, ClientType, ClientView, CreatedClient};
pub use flow::{ConsentGrant, ConsentPrompt, DeviceAuthorization};

/// Grant type: authorization code.
pub const GRANT_CODE: &str = "authorization_code";
/// Grant type: refresh token.
pub const GRANT_REFRESH: &str = "refresh_token";
/// Grant type: client credentials.
pub const GRANT_CLIENT_CREDENTIALS: &str = "client_credentials";
/// Grant type: device authorization (RFC 8628).
pub const GRANT_DEVICE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// Every grant type the server supports.
pub const GRANT_TYPES: &[&str] = &[GRANT_CODE, GRANT_REFRESH, GRANT_CLIENT_CREDENTIALS, GRANT_DEVICE];

/// The OpenID Connect scopes the server understands. Any other scope must be
/// a ForgeBase API scope ([`Scope`]).
pub const OIDC_SCOPES: &[&str] = &["openid", "profile", "email", "phone", "offline_access"];

/// `acr` of a session that passed one factor.
pub const ACR_SINGLE: &str = "urn:forgebase:sfa";
/// `acr` of a session that passed a second factor.
pub const ACR_MULTI: &str = "urn:forgebase:mfa";

/// Settings of the identity provider. Defaults are production-safe.
#[derive(Debug, Clone)]
pub struct IdpConfig {
    /// Lifetime of authorization codes.
    pub code_ttl: Duration,
    /// Default lifetime of access tokens (a client may set its own).
    pub access_token_ttl: Duration,
    /// Default absolute lifetime of a refresh-token family.
    pub refresh_token_ttl: Duration,
    /// Lifetime of ID tokens.
    pub id_token_ttl: Duration,
    /// Rotate a realm's signing key after this age.
    pub key_rotation: Duration,
    /// Lifetime of a device code.
    pub device_code_ttl: Duration,
    /// Minimum polling interval of the device flow.
    pub device_poll_interval: Duration,
    /// How long an authorization request waits for sign-in or consent.
    pub request_ttl: Duration,
    /// Login page a browser is sent to when it has no session
    /// (default `<public_url>/auth/login`). It receives `realm` and
    /// `return_to`, signs in with `"cookie": true` and goes to `return_to`.
    pub login_url: Option<String>,
}

impl Default for IdpConfig {
    fn default() -> Self {
        Self {
            code_ttl: Duration::from_secs(60),
            access_token_ttl: Duration::from_secs(15 * 60),
            refresh_token_ttl: Duration::from_secs(30 * 24 * 3600),
            id_token_ttl: Duration::from_secs(3600),
            key_rotation: Duration::from_secs(90 * 24 * 3600),
            device_code_ttl: Duration::from_secs(10 * 60),
            device_poll_interval: Duration::from_secs(5),
            request_ttl: Duration::from_secs(10 * 60),
            login_url: None,
        }
    }
}

/// In-memory state of the identity provider (signing-key cache).
#[derive(Debug, Default)]
pub struct IdpState {
    pub(crate) keys: RwLock<HashMap<String, Vec<keys::OidcKey>>>,
}

// ---------------------------------------------------------------------------
// Protocol errors (RFC 6749 §4.1.2.1, §5.2; RFC 8628 §3.5; OIDC Core §3.1.2.6)
// ---------------------------------------------------------------------------

/// An OAuth 2.0 / OpenID Connect protocol error: `{"error", "error_description"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OAuthError {
    /// Error code (`invalid_grant`, `login_required`, ...).
    pub error: &'static str,
    /// Human-readable description (never contains secrets).
    pub error_description: String,
    /// HTTP status when answered directly.
    #[serde(skip)]
    pub status: u16,
}

macro_rules! oauth_errors {
    ($($fn:ident => $code:literal, $status:literal;)*) => {
        impl OAuthError {
            $(
                #[doc = concat!("`", $code, "`")]
                pub fn $fn(msg: impl Into<String>) -> Self {
                    Self { error: $code, error_description: msg.into(), status: $status }
                }
            )*
        }
    };
}

oauth_errors! {
    invalid_request => "invalid_request", 400;
    invalid_client => "invalid_client", 401;
    invalid_grant => "invalid_grant", 400;
    unauthorized_client => "unauthorized_client", 400;
    unsupported_grant_type => "unsupported_grant_type", 400;
    unsupported_response_type => "unsupported_response_type", 400;
    invalid_scope => "invalid_scope", 400;
    access_denied => "access_denied", 400;
    authorization_pending => "authorization_pending", 400;
    slow_down => "slow_down", 400;
    expired_token => "expired_token", 400;
    login_required => "login_required", 400;
    consent_required => "consent_required", 400;
    request_not_supported => "request_not_supported", 400;
    request_uri_not_supported => "request_uri_not_supported", 400;
    invalid_token => "invalid_token", 401;
    insufficient_scope => "insufficient_scope", 403;
    server_error => "server_error", 500;
    temporarily_unavailable => "temporarily_unavailable", 503;
}

impl OAuthError {
    /// The HTTP status.
    pub fn status_code(&self) -> StatusCode {
        StatusCode::from_u16(self.status).unwrap_or(StatusCode::BAD_REQUEST)
    }
}

impl std::fmt::Display for OAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.error, self.error_description)
    }
}

impl From<AuthError> for OAuthError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::RateLimited { retry_after } => {
                let mut o = Self::temporarily_unavailable(format!("too many requests, retry in {retry_after} seconds"));
                o.status = 429;
                o
            }
            AuthError::AccountSuspended | AuthError::AccountDeactivated | AuthError::InvalidCredentials => {
                Self::invalid_grant("the account cannot sign in")
            }
            AuthError::NotFound(m) => Self::invalid_request(m),
            AuthError::InvalidInput(m) => Self::invalid_request(m),
            AuthError::Forbidden(m) => Self::access_denied(m),
            other => {
                tracing::error!(error = %other, "internal error in the identity provider");
                Self::server_error("internal server error")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Scopes
// ---------------------------------------------------------------------------

/// Whether `s` is an RFC 6749 §3.3 scope token.
fn is_scope_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.bytes()
            .all(|b| b == 0x21 || (0x23..=0x5b).contains(&b) || (0x5d..=0x7e).contains(&b))
}

/// Whether a scope may be registered on a client: an OpenID Connect scope or
/// a ForgeBase API scope.
pub(crate) fn scope_known(s: &str) -> bool {
    OIDC_SCOPES.contains(&s) || (s != "*" && Scope::parse(s).is_ok())
}

/// Parses a space-separated `scope` parameter (deduplicated, sorted).
pub(crate) fn parse_scope_param(raw: &str) -> Result<Vec<String>, OAuthError> {
    let mut out = BTreeSet::new();
    for s in raw.split(' ').filter(|s| !s.is_empty()) {
        if !is_scope_token(s) {
            return Err(OAuthError::invalid_scope("malformed scope"));
        }
        out.insert(s.to_owned());
    }
    Ok(out.into_iter().collect())
}

/// Whether every scope of `want` is in `have`.
pub(crate) fn scopes_within(want: &[String], have: &[String]) -> bool {
    want.iter().all(|w| have.contains(w))
}

/// Space-joined scopes.
pub(crate) fn scope_string(scopes: &[String]) -> String {
    scopes.join(" ")
}

/// Human description of a scope for the consent screen.
pub(crate) fn scope_description(s: &str) -> String {
    match s {
        "openid" => "Sign you in with your account".into(),
        "profile" => "Read your name and profile".into(),
        "email" => "Read your email address".into(),
        "phone" => "Read your phone number".into(),
        "offline_access" => "Stay signed in when you are not using the app".into(),
        other => format!("Access the ForgeBase API: {other}"),
    }
}

/// `amr` values (RFC 8176 where one fits) for a ForgeBase session method.
pub(crate) fn amr_of(method: &str, mfa: bool) -> Vec<String> {
    let first = match method {
        "password" | "ldap" => "pwd",
        "email_otp" => "otp",
        "sms" => "sms",
        "passkey" => "hwk",
        "oauth" => "fed",
        other => other,
    };
    let mut v = vec![first.to_owned()];
    if mfa {
        v.push("mfa".into());
    }
    v
}

/// `acr` of a session.
pub(crate) fn acr_of(mfa: bool) -> &'static str {
    if mfa { ACR_MULTI } else { ACR_SINGLE }
}

/// HTML-escapes text for the built-in pages.
pub(crate) fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_parsing_and_registration_rules() {
        assert_eq!(
            parse_scope_param("openid  email openid").unwrap(),
            vec!["email".to_owned(), "openid".to_owned()]
        );
        assert!(parse_scope_param("bad\"scope").is_err());
        assert!(scope_known("openid"));
        assert!(scope_known("collections:read:posts"));
        assert!(!scope_known("*"));
        assert!(!scope_known("nonsense"));
        assert!(scopes_within(&["a".into()], &["a".into(), "b".into()]));
        assert!(!scopes_within(&["c".into()], &["a".into()]));
    }

    #[test]
    fn escaping_and_amr() {
        assert_eq!(esc("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
        assert_eq!(amr_of("password", true), vec!["pwd", "mfa"]);
        assert_eq!(acr_of(false), ACR_SINGLE);
    }
}
