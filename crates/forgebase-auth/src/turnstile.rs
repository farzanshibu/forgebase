//! Cloudflare Turnstile bot protection (FB-25, ADR-0015 §6).
//!
//! Off unless a realm configures it. When on, the protected requests
//! (signup, password login and LDAP login, password-reset request, OTP
//! requests: email code, magic link, SMS code) must carry a widget token in
//! the `cf-turnstile-response` or `x-turnstile-token` header; the token is
//! checked with Cloudflare's siteverify endpoint (through the auth service's
//! HTTPS client) before anything else happens. Failures are closed: no
//! token, a rejected token, a hostname or action mismatch, or an unreachable
//! siteverify all refuse the request.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::realm::{ConfigSecret, RealmIdentity};
use crate::service::ClientInfo;
use crate::social::HttpRequest;
use crate::{AuthError, AuthService};

/// Cloudflare's verification endpoint.
pub const SITEVERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";

/// Request headers carrying the widget token.
pub const TOKEN_HEADERS: [&str; 2] = ["cf-turnstile-response", "x-turnstile-token"];

/// What a token protects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnstileAction {
    /// Account creation.
    Signup,
    /// Password and LDAP sign-in.
    Login,
    /// Password-reset request.
    PasswordReset,
    /// One-time code and link requests (email code, magic link, SMS code).
    Otp,
}

impl TurnstileAction {
    /// Name, as also expected in the widget's `action` (when one is set).
    pub fn as_str(self) -> &'static str {
        match self {
            TurnstileAction::Signup => "signup",
            TurnstileAction::Login => "login",
            TurnstileAction::PasswordReset => "password_reset",
            TurnstileAction::Otp => "otp",
        }
    }
}

/// Turnstile settings of a realm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TurnstileSettings {
    /// Checks are made only when enabled.
    pub enabled: bool,
    /// Public site key (given to the login page).
    pub site_key: String,
    /// Secret key (write-only).
    pub secret: ConfigSecret,
    /// Protected actions (default: all four).
    pub actions: Vec<TurnstileAction>,
    /// Accepted `hostname` values of a verified token (empty: any).
    pub hostnames: Vec<String>,
    /// Siteverify URL override (tests, egress proxy).
    pub verify_url: Option<String>,
}

impl Default for TurnstileSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            site_key: String::new(),
            secret: ConfigSecret::Keep,
            actions: vec![
                TurnstileAction::Signup,
                TurnstileAction::Login,
                TurnstileAction::PasswordReset,
                TurnstileAction::Otp,
            ],
            hostnames: Vec::new(),
            verify_url: None,
        }
    }
}

impl TurnstileSettings {
    pub(crate) fn validate(&self) -> Result<(), AuthError> {
        if let Some(u) = &self.verify_url {
            let ok = url::Url::parse(u).is_ok_and(|u| matches!(u.scheme(), "https" | "http"));
            if !ok {
                return Err(AuthError::invalid("turnstile.verify_url must be an http(s) URL"));
            }
        }
        if self.enabled && (self.site_key.trim().is_empty() || self.secret.get().is_none()) {
            return Err(AuthError::invalid("an enabled turnstile needs site_key and secret"));
        }
        Ok(())
    }
}

impl AuthService {
    /// Verifies the request's Turnstile token when the realm protects
    /// `action`.
    pub(crate) async fn turnstile_check(
        &self,
        realm: &RealmIdentity,
        action: TurnstileAction,
        ci: &ClientInfo,
    ) -> Result<(), AuthError> {
        let Some(ts) = realm
            .config
            .turnstile
            .as_ref()
            .filter(|t| t.enabled && t.actions.contains(&action))
        else {
            return Ok(());
        };
        let outcome = self.turnstile_verify(ts, action, ci).await;
        if let Err(reason) = &outcome {
            self.audit(
                "turnstile",
                false,
                ci,
                "anonymous",
                None,
                json!({"realm": realm.realm, "action": action.as_str(), "reason": reason}),
            )
            .await?;
        }
        match outcome {
            Ok(()) => Ok(()),
            Err("unavailable") => Err(AuthError::Provider("bot verification is unavailable, try again".into())),
            Err(_) => Err(AuthError::BotCheckFailed),
        }
    }

    async fn turnstile_verify(
        &self,
        ts: &TurnstileSettings,
        action: TurnstileAction,
        ci: &ClientInfo,
    ) -> Result<(), &'static str> {
        let token = ci
            .turnstile_token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .ok_or("missing_token")?;
        if token.len() > 2048 {
            return Err("invalid_token");
        }
        let secret = ts.secret.get().ok_or("not_configured")?;
        let idempotency = crate::crypto::random_token(16).map_err(|_| "unavailable")?;
        let mut fields = vec![
            ("secret", secret),
            ("response", token),
            ("idempotency_key", idempotency.as_str()),
        ];
        if let Some(ip) = ci.ip.as_deref() {
            fields.push(("remoteip", ip));
        }
        let url = ts.verify_url.as_deref().unwrap_or(SITEVERIFY_URL);
        let res = self
            .http_client()
            .send(HttpRequest::post_form(url, &fields))
            .await
            .map_err(|_| "unavailable")?;
        if res.status != 200 {
            return Err("unavailable");
        }
        let body = res.json().map_err(|_| "unavailable")?;
        if body.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
            return Err("rejected");
        }
        if !ts.hostnames.is_empty() {
            let host = body.get("hostname").and_then(serde_json::Value::as_str).unwrap_or("");
            if !ts.hostnames.iter().any(|h| h.eq_ignore_ascii_case(host)) {
                return Err("hostname_mismatch");
            }
        }
        // A widget that names its action must name this one.
        if let Some(a) = body.get("action").and_then(serde_json::Value::as_str)
            && !a.is_empty()
            && a != action.as_str()
        {
            return Err("action_mismatch");
        }
        Ok(())
    }
}
