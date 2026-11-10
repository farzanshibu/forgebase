//! First-run setup (FB-90, `docs/requirements/first-run-setup.md` §1–2).
//!
//! The server is in **setup mode** while no administrator account exists.
//! In that mode `forgebase serve` asks [`AuthService::begin_setup`] for a
//! one-time **setup token** and prints a setup link to standard output (never
//! to the log). Whoever holds the token can create the first administrator
//! with `POST /api/setup/admin`, which signs them in like
//! `POST /api/auth/admins/login`.
//!
//! * The token is 256 random bits, URL-safe base64. Only its SHA-256 digest
//!   is kept, in memory; it is compared in constant time
//!   ([`crypto::ct_eq`] on fixed-length digests), so neither its length nor
//!   a matching prefix leaks through timing.
//! * It is invalidated when the first administrator is created (by this
//!   endpoint or by `forgebase admin create`); a restart issues a new one.
//! * Attempts are rate limited per client address, before anything else is
//!   checked.
//! * Completions are serialised, and "no administrator yet" is re-checked
//!   under that lock, so two requests holding the token cannot both create
//!   an administrator.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, SET_COOKIE};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use forgebase_data::db::Backend;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::crypto;
use crate::http::{Body, Client, HttpError, Res, ok};
use crate::service::{ADMIN_PASSWORD_MIN, ClientInfo, LoginOptions, LoginOutcome, normalize_email};
use crate::store::{first, read};
use crate::{AuthError, AuthMethod, AuthService, Subject};

/// Setup attempts allowed per client address per minute.
pub const SETUP_ATTEMPTS_PER_MINUTE: u32 = 10;

/// Random bytes in a setup token (256 bits; the contract asks for >= 128).
const TOKEN_BYTES: usize = 32;

/// In-memory setup state held by the auth service.
#[derive(Debug, Default)]
pub(crate) struct SetupState {
    /// SHA-256 of the current setup token; `None` when no token is issued.
    digest: Mutex<Option<[u8; 32]>>,
    /// Serialises completions.
    gate: tokio::sync::Mutex<()>,
}

impl SetupState {
    fn set(&self, digest: Option<[u8; 32]>) {
        *self.digest.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = digest;
    }

    /// Whether `candidate` is the current token. Constant time: both sides
    /// are fixed-length digests compared with [`crypto::ct_eq`].
    fn matches(&self, candidate: &str) -> bool {
        let stored = *self.digest.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let sent = digest(candidate);
        // Compare even when no token is issued, so both paths do the same work.
        let expected = stored.unwrap_or([0u8; 32]);
        crypto::ct_eq(&sent, &expected) && stored.is_some()
    }
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Why `POST /api/setup/admin` refused.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    /// The token is missing, wrong or no longer valid.
    #[error("the setup token is missing, wrong or no longer valid; use the link `forgebase serve` printed")]
    InvalidToken,
    /// An administrator already exists.
    #[error("setup is complete: an administrator already exists; sign in instead")]
    Complete,
    /// Validation, rate limit, storage.
    #[error(transparent)]
    Auth(#[from] AuthError),
}

/// Checks an administrator email and password against the rules
/// `forgebase admin create` applies, without touching the database.
///
/// # Errors
///
/// [`AuthError::InvalidInput`] naming the problem.
pub fn check_admin_credentials(email: &str, password: &str) -> Result<(), AuthError> {
    normalize_email(email)?;
    crate::password::check_password_policy(password, ADMIN_PASSWORD_MIN)
}

/// Whether any administrator exists in the database behind `db`. Read only:
/// `forgebase status` asks it over a read-only connection.
///
/// # Errors
///
/// Storage errors (for example a database not migrated yet).
pub async fn admin_exists(db: &Arc<dyn Backend>) -> Result<bool, AuthError> {
    let r = read(db, "SELECT 1 AS one FROM _forgebase_admins LIMIT 1", vec![]).await?;
    Ok(first(&r).is_some())
}

impl AuthService {
    /// Whether the server is in setup mode: no administrator exists.
    ///
    /// # Errors
    ///
    /// Storage errors.
    pub async fn setup_required(&self) -> Result<bool, AuthError> {
        Ok(!admin_exists(self.data().backend()).await?)
    }

    /// Issues a new setup token when no administrator exists (replacing any
    /// earlier one) and returns it; returns `None`, and drops any token,
    /// once an administrator exists. The caller prints the token; it is not
    /// stored anywhere but as a digest in memory.
    ///
    /// # Errors
    ///
    /// Storage or random-number errors.
    pub async fn begin_setup(&self) -> Result<Option<String>, AuthError> {
        if !self.setup_required().await? {
            self.setup_state().set(None);
            return Ok(None);
        }
        let token = crypto::random_token(TOKEN_BYTES)?;
        self.setup_state().set(Some(digest(&token)));
        Ok(Some(token))
    }

    /// Creates the first administrator with the setup token and signs them
    /// in (`actor` in the audit log: `setup`).
    ///
    /// Order of checks: rate limit (per client address), `Complete` when an
    /// administrator exists, the token, then the email and password rules.
    /// A failed validation keeps the token valid.
    ///
    /// # Errors
    ///
    /// [`SetupError`].
    pub async fn complete_setup(
        &self,
        ci: &ClientInfo,
        token: &str,
        email: &str,
        password: &str,
        opts: &LoginOptions,
    ) -> Result<(LoginOutcome, String), SetupError> {
        let ip = ci.ip.as_deref().unwrap_or("unknown");
        self.limit_key("setup_attempt", ip, SETUP_ATTEMPTS_PER_MINUTE, Duration::from_secs(60))?;
        let state = self.setup_state();
        let _serial = state.gate.lock().await;
        if !self.setup_required().await? {
            state.set(None);
            return Err(SetupError::Complete);
        }
        if !state.matches(token) {
            self.audit(
                "setup.admin",
                false,
                ci,
                "anonymous",
                None,
                json!({"reason": "invalid_setup_token"}),
            )
            .await?;
            return Err(SetupError::InvalidToken);
        }
        check_admin_credentials(email, password)?;
        let admin = match self.create_admin(email, password, "setup", ci).await {
            Ok(a) => a,
            // `forgebase admin create` won the race for the same email.
            Err(AuthError::Conflict(_)) => {
                state.set(None);
                return Err(SetupError::Complete);
            }
            Err(e) => return Err(e.into()),
        };
        state.set(None);
        let out = self
            .complete_first_factor(&Subject::Admin { id: admin.id }, AuthMethod::Password, ci, opts)
            .await?;
        Ok((out, self.admin_device_token(&admin.email)))
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

/// `POST /api/setup/admin` body.
#[derive(Deserialize)]
struct SetupReq {
    #[serde(default)]
    token: String,
    email: String,
    password: String,
    /// Cookie session by default: setup is a browser flow. `false` returns
    /// bearer tokens in the body instead.
    #[serde(default = "yes")]
    cookie: bool,
    #[serde(default)]
    device_name: Option<String>,
}

const fn yes() -> bool {
    true
}

/// `GET /api/setup/status` and `POST /api/setup/admin`, ready to merge into
/// the application router (no authentication).
pub(crate) fn routes<S: Clone + Send + Sync + 'static>(auth: AuthService) -> Router<S> {
    Router::new()
        .route("/api/setup/status", get(status))
        .route("/api/setup/admin", post(create_first_admin))
        .with_state(auth)
}

async fn status(State(a): State<AuthService>, Client(_, rid): Client) -> Res {
    let required = a.setup_required().await.map_err(|e| HttpError::new(e, &rid))?;
    Ok(ok(StatusCode::OK, json!({"setup_required": required})))
}

async fn create_first_admin(
    State(a): State<AuthService>,
    Client(ci, rid): Client,
    Body(b): Body<SetupReq>,
) -> Response {
    let opts = LoginOptions {
        cookie: b.cookie,
        device_name: b.device_name.or_else(|| Some("Admin dashboard".into())),
    };
    let (out, device) = match a.complete_setup(&ci, &b.token, &b.email, &b.password, &opts).await {
        Ok(v) => v,
        Err(e) => return setup_error(e, &rid),
    };
    let mut res = match crate::http::login_response(&a, out, &rid).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };
    *res.status_mut() = StatusCode::CREATED;
    if let Some(v) = crate::http::admin_device_cookie(&a, &device) {
        res.headers_mut().append(SET_COOKIE, v);
    }
    res
}

/// The contract's codes: `403 invalid_setup_token`, `409 setup_complete`,
/// `400 validation_failed`; anything else (`429`, storage) as the auth API
/// answers it.
fn setup_error(e: SetupError, rid: &str) -> Response {
    let (status, code) = match e {
        SetupError::InvalidToken => (StatusCode::FORBIDDEN, "invalid_setup_token"),
        SetupError::Complete => (StatusCode::CONFLICT, "setup_complete"),
        SetupError::Auth(AuthError::InvalidInput(_)) => (StatusCode::BAD_REQUEST, "validation_failed"),
        SetupError::Auth(inner) => return HttpError::new(inner, rid).into_response(),
    };
    let mut res = (
        status,
        axum::Json(json!({"error": {"code": code, "message": e.to_string(), "request_id": rid}})),
    )
        .into_response();
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison_is_constant_time_on_digests() {
        let s = SetupState::default();
        assert!(!s.matches(""), "no token issued: nothing matches");
        let token = crypto::random_token(TOKEN_BYTES).unwrap();
        assert!(token.len() >= 43, "256 bits in URL-safe base64");
        assert!(
            token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        s.set(Some(digest(&token)));
        assert!(s.matches(&token));
        assert!(!s.matches(&token[..token.len() - 1]), "prefix");
        assert!(!s.matches(&format!("{token}x")), "longer");
        assert!(!s.matches(""));
        s.set(None);
        assert!(!s.matches(&token), "invalidated");
    }

    /// Guards the implementation: the token is only ever compared through
    /// `crypto::ct_eq` on digests, never with `==`.
    #[test]
    fn matches_uses_the_constant_time_compare() {
        let src = include_str!("setup.rs");
        let body = src
            .split("fn matches(&self, candidate: &str) -> bool {")
            .nth(1)
            .and_then(|s| s.split("\n    }\n").next())
            .unwrap();
        assert!(body.contains("crypto::ct_eq(&sent, &expected)"), "{body}");
        assert!(!body.contains("=="), "plain comparison in matches: {body}");
    }

    #[test]
    fn admin_credentials_follow_the_admin_create_rules() {
        assert!(check_admin_credentials("admin@example.com", "correct horse battery").is_ok());
        assert!(check_admin_credentials("not-an-email", "correct horse battery").is_err());
        assert!(check_admin_credentials("admin@example.com", "short").is_err());
    }
}
