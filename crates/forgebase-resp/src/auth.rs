//! Who a connection is.
//!
//! A connection starts unauthenticated and may only `PING`, `HELLO`, `AUTH`
//! and `QUIT`. `AUTH` hands the secret to an [`Authenticator`]; the production
//! one ([`KeyAuthenticator`]) accepts ForgeBase API keys and personal access
//! tokens (FB-20) and nothing else, so a password or session token is never
//! sent over this plaintext protocol.

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use forgebase_auth::scopes::{Access, RequiredScope};
use forgebase_auth::{AuthService, Scope, Subject};
use forgebase_data::AccessContext;

use crate::backend::BoxFuture;

/// An authenticated connection's identity.
#[derive(Clone)]
pub struct Identity {
    /// The context every command runs with (principal and tenant).
    pub ctx: AccessContext,
    /// Id of the user, administrator or service account, for ACL-style
    /// `AUTH <user> <token>` (the user must match it, or be `default`).
    pub subject_id: String,
    /// Scopes of the key (`None` = unrestricted). A key needs `kv:read` or
    /// `kv:write` (optionally `:<namespace>`), exactly as for `/api/v1/kv`.
    pub scopes: Option<Vec<Scope>>,
    secret: String,
}

impl fmt::Debug for Identity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Identity")
            .field("ctx", &self.ctx)
            .field("subject_id", &self.subject_id)
            .field("scopes", &self.scopes)
            .finish_non_exhaustive()
    }
}

impl Identity {
    /// An identity. `secret` is kept only to re-check the credential
    /// ([`Authenticator::still_valid`]); it never appears in `Debug` output.
    pub fn new(ctx: AccessContext, subject_id: impl Into<String>, scopes: Option<Vec<Scope>>, secret: &str) -> Self {
        Self {
            ctx,
            subject_id: subject_id.into(),
            scopes,
            secret: secret.to_owned(),
        }
    }

    /// The credential presented at `AUTH`.
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// Whether the key's scopes allow `access` in namespace `ns`.
    pub fn allows(&self, ns: &str, access: Access) -> bool {
        let Some(scopes) = &self.scopes else { return true };
        let need = RequiredScope {
            area: "kv".to_owned(),
            access,
            resource: Some(ns.to_owned()),
        };
        scopes.iter().any(|s| s.grants(&need))
    }

    /// The scope text a refused request needed.
    pub fn needed(ns: &str, access: Access) -> String {
        RequiredScope {
            area: "kv".to_owned(),
            access,
            resource: Some(ns.to_owned()),
        }
        .to_string()
    }
}

/// Why `AUTH` did not succeed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthFailure {
    /// Wrong, expired, revoked or malformed credential (or wrong user name).
    Invalid,
    /// The credential could not be checked (database trouble).
    Unavailable(String),
}

/// Turns the secret of an `AUTH` into an [`Identity`].
pub trait Authenticator: Send + Sync + fmt::Debug {
    /// `username` is `None` for `AUTH <token>`, `Some(user)` for `AUTH <user> <token>`.
    fn authenticate<'a>(
        &'a self,
        username: Option<&'a str>,
        secret: &'a str,
    ) -> BoxFuture<'a, Result<Identity, AuthFailure>>;

    /// Whether `identity` may still be used (the key was not revoked or did
    /// not expire since `AUTH`). Checked every `revalidate_interval` and
    /// closes the connection when `false`. Defaults to `true`.
    fn still_valid<'a>(&'a self, _identity: &'a Identity) -> BoxFuture<'a, bool> {
        Box::pin(std::future::ready(true))
    }
}

/// Accepts ForgeBase API keys (`fbk_...`) and personal access tokens (`fbp_...`).
#[derive(Debug, Clone)]
pub struct KeyAuthenticator {
    auth: AuthService,
}

impl KeyAuthenticator {
    /// An authenticator on the auth service.
    pub fn new(auth: AuthService) -> Self {
        Self { auth }
    }
}

impl Authenticator for KeyAuthenticator {
    fn authenticate<'a>(
        &'a self,
        username: Option<&'a str>,
        secret: &'a str,
    ) -> BoxFuture<'a, Result<Identity, AuthFailure>> {
        Box::pin(async move {
            if !AuthService::is_api_key(secret) {
                return Err(AuthFailure::Invalid);
            }
            let a = self.auth.authenticate_key(secret).await.map_err(|e| match e {
                forgebase_auth::AuthError::Internal(m) => AuthFailure::Unavailable(m),
                _ => AuthFailure::Invalid,
            })?;
            let subject_id = match &a.subject {
                Subject::User { id, .. } | Subject::Admin { id } | Subject::ServiceAccount { id, .. } => id.clone(),
                _ => return Err(AuthFailure::Invalid),
            };
            if let Some(u) = username
                && !u.is_empty()
                && u != "default"
                && u != subject_id
            {
                return Err(AuthFailure::Invalid);
            }
            Ok(Identity::new(
                a.access_context(None),
                subject_id,
                a.scopes.clone(),
                secret,
            ))
        })
    }

    fn still_valid<'a>(&'a self, identity: &'a Identity) -> BoxFuture<'a, bool> {
        Box::pin(async move { self.auth.authenticate_key(identity.secret()).await.is_ok() })
    }
}

/// Counts failed `AUTH`s per client address.
#[derive(Debug)]
pub(crate) struct AuthLimiter {
    max: u32,
    window: Duration,
    seen: Mutex<HashMap<IpAddr, (u32, Instant)>>,
}

impl AuthLimiter {
    pub(crate) fn new(max: u32, window: Duration) -> Self {
        Self {
            max,
            window,
            seen: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<IpAddr, (u32, Instant)>> {
        self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether `ip` has used up its failures for the current window.
    pub(crate) fn blocked(&self, ip: IpAddr) -> bool {
        if self.max == 0 {
            return false;
        }
        self.lock()
            .get(&ip)
            .is_some_and(|(n, since)| *n >= self.max && since.elapsed() < self.window)
    }

    /// Records one failure.
    pub(crate) fn record_failure(&self, ip: IpAddr) {
        let mut m = self.lock();
        if m.len() > 10_000 {
            let w = self.window;
            m.retain(|_, (_, since)| since.elapsed() < w);
        }
        let e = m.entry(ip).or_insert((0, Instant::now()));
        if e.1.elapsed() >= self.window {
            *e = (0, Instant::now());
        }
        e.0 += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_blocks_after_the_limit_and_per_address() {
        let l = AuthLimiter::new(2, Duration::from_secs(60));
        let a: IpAddr = "10.0.0.1".parse().unwrap_or_else(|_| unreachable!());
        let b: IpAddr = "10.0.0.2".parse().unwrap_or_else(|_| unreachable!());
        assert!(!l.blocked(a));
        l.record_failure(a);
        assert!(!l.blocked(a));
        l.record_failure(a);
        assert!(l.blocked(a));
        assert!(!l.blocked(b));
    }

    #[test]
    fn limiter_window_expires() {
        let l = AuthLimiter::new(1, Duration::from_millis(10));
        let a: IpAddr = "10.0.0.1".parse().unwrap_or_else(|_| unreachable!());
        l.record_failure(a);
        assert!(l.blocked(a));
        std::thread::sleep(Duration::from_millis(20));
        assert!(!l.blocked(a));
    }

    #[test]
    fn scopes_narrow_namespaces() {
        let scopes = vec![
            Scope::parse("kv:read").unwrap_or_else(|_| unreachable!()),
            Scope::parse("kv:write:cache").unwrap_or_else(|_| unreachable!()),
        ];
        let id = Identity::new(AccessContext::system(), "x", Some(scopes), "s");
        assert!(id.allows("anything", Access::Read));
        assert!(id.allows("cache", Access::Write));
        assert!(!id.allows("other", Access::Write));
        assert!(Identity::new(AccessContext::system(), "x", None, "s").allows("o", Access::Write));
        assert!(!format!("{id:?}").contains('s') || !format!("{id:?}").contains("secret"));
    }
}
