//! Authentication: sessions with automatic refresh, sign-in flows, MFA, tokens and API keys.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::error::{Error, Result};
use crate::http::{Req, Transport, seg};

/// A signed-in session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// Bearer token for API calls.
    pub access_token: String,
    /// Rotating refresh token (single use: every refresh returns a new one).
    pub refresh_token: Option<String>,
    /// Access token expiry, milliseconds since the Unix epoch.
    pub expires_at_ms: u64,
    /// Server session id.
    pub session_id: String,
    /// Absolute session expiry (RFC 3339).
    pub session_expires_at: String,
    /// Who signed in (`{"kind":"user","collection":...,"id":...}` and friends).
    pub subject: Value,
    /// The user record, for user sign-ins.
    pub user: Option<Value>,
}

/// A second factor is required: call [`AuthApi::verify_mfa`] with the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MfaChallenge {
    /// Pass this to [`AuthApi::verify_mfa`].
    pub mfa_token: String,
    /// Seconds the challenge is valid.
    pub expires_in: u64,
}

/// Outcome of a sign-in call.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum SignInResult {
    /// Signed in; the session is stored in the client.
    Session(Session),
    /// A second factor is needed first.
    MfaRequired(MfaChallenge),
}

impl SignInResult {
    /// The session, or an error when MFA is still pending.
    pub fn into_session(self) -> Result<Session> {
        match self {
            Self::Session(s) => Ok(s),
            Self::MfaRequired(_) => Err(Error::InvalidResponse(
                "a second factor is required: call verify_mfa".into(),
            )),
        }
    }
}

/// Options for sign-in calls.
#[derive(Debug, Clone, Default)]
pub struct SignInOptions {
    /// User collection (server default `users`).
    pub collection: Option<String>,
    /// Name shown in the device list.
    pub device_name: Option<String>,
}

impl SignInOptions {
    fn apply(&self, mut body: Value) -> Value {
        if let Value::Object(m) = &mut body {
            if let Some(c) = &self.collection {
                m.insert("collection".into(), json!(c));
            }
            if let Some(d) = &self.device_name {
                m.insert("device_name".into(), json!(d));
            }
        }
        body
    }
}

/// Where the session is kept between process restarts. Implementations must not block for long.
pub trait SessionStore: Send + Sync {
    /// The stored session, if any.
    fn load(&self) -> Option<Session>;
    /// Stores the session (best effort).
    fn save(&self, session: &Session);
    /// Forgets the session.
    fn clear(&self);
}

/// Keeps the session in memory only (the default).
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<Option<Session>>);

impl SessionStore for MemoryStore {
    fn load(&self) -> Option<Session> {
        self.0.lock().ok()?.clone()
    }
    fn save(&self, session: &Session) {
        if let Ok(mut g) = self.0.lock() {
            *g = Some(session.clone());
        }
    }
    fn clear(&self) {
        if let Ok(mut g) = self.0.lock() {
            *g = None;
        }
    }
}

/// Keeps the session in a JSON file. The file holds tokens: protect it like a password.
#[derive(Debug, Clone)]
pub struct FileStore {
    path: PathBuf,
}

impl FileStore {
    /// Stores the session at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl SessionStore for FileStore {
    fn load(&self) -> Option<Session> {
        serde_json::from_slice(&std::fs::read(&self.path).ok()?).ok()
    }
    fn save(&self, session: &Session) {
        if let Ok(bytes) = serde_json::to_vec(session) {
            let _ = std::fs::write(&self.path, bytes);
        }
    }
    fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Credentials shared by every request of a client.
pub(crate) struct AuthState {
    api_key: Mutex<Option<String>>,
    static_token: Mutex<Option<String>>,
    session: Mutex<Option<Session>>,
    store: Arc<dyn SessionStore>,
    tx: watch::Sender<Option<Session>>,
    refresh_lock: tokio::sync::Mutex<()>,
    pub auto_refresh: bool,
    pub skew_ms: u64,
}

impl AuthState {
    pub fn new(
        api_key: Option<String>,
        token: Option<String>,
        store: Arc<dyn SessionStore>,
        auto_refresh: bool,
        skew_ms: u64,
    ) -> Self {
        let session = store.load();
        let (tx, _) = watch::channel(session.clone());
        Self {
            api_key: Mutex::new(api_key),
            static_token: Mutex::new(token),
            session: Mutex::new(session),
            store,
            tx,
            refresh_lock: tokio::sync::Mutex::new(()),
            auto_refresh,
            skew_ms,
        }
    }

    fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn api_key(&self) -> Option<String> {
        Self::lock(&self.api_key).clone()
    }
    pub fn set_api_key(&self, key: Option<String>) {
        *Self::lock(&self.api_key) = key;
    }
    pub fn set_static_token(&self, token: Option<String>) {
        *Self::lock(&self.static_token) = token;
    }
    pub fn session(&self) -> Option<Session> {
        Self::lock(&self.session).clone()
    }
    pub fn set_session(&self, s: Option<Session>) {
        match &s {
            Some(s) => self.store.save(s),
            None => self.store.clear(),
        }
        *Self::lock(&self.session) = s.clone();
        self.tx.send_replace(s);
    }
    pub fn watch(&self) -> watch::Receiver<Option<Session>> {
        self.tx.subscribe()
    }
    fn static_token(&self) -> Option<String> {
        Self::lock(&self.static_token).clone()
    }
}

/// Wire shape of a session.
#[derive(Deserialize)]
struct WireSession {
    access_token: Option<String>,
    expires_in: u64,
    refresh_token: Option<String>,
    session_id: String,
    session_expires_at: String,
    #[serde(default)]
    subject: Value,
    user: Option<Value>,
}

fn to_session(v: &Value) -> Result<Session> {
    let w: WireSession =
        serde_json::from_value(v.clone()).map_err(|e| Error::InvalidResponse(format!("sign-in response: {e}")))?;
    let access_token = w
        .access_token
        .ok_or_else(|| Error::InvalidResponse("sign-in response carries no access token".into()))?;
    Ok(Session {
        access_token,
        refresh_token: w.refresh_token,
        expires_at_ms: now_ms() + w.expires_in.saturating_mul(1000),
        session_id: w.session_id,
        session_expires_at: w.session_expires_at,
        subject: w.subject,
        user: w.user,
    })
}

impl Transport {
    /// Headers that authenticate one request (refreshing the access token first when it is about to expire).
    pub(crate) async fn credentials(&self) -> Vec<(String, String)> {
        if let Some(k) = self.auth.api_key() {
            return vec![("X-API-Key".into(), k)];
        }
        if let Some(t) = self.auth.static_token() {
            return vec![("Authorization".into(), format!("Bearer {t}"))];
        }
        let Some(s) = self.auth.session() else {
            return Vec::new();
        };
        let mut token = s.access_token.clone();
        if self.auth.auto_refresh
            && s.refresh_token.is_some()
            && s.expires_at_ms.saturating_sub(self.auth.skew_ms) <= now_ms()
            && let Ok(next) = self.refresh_if_stale(Some(&s.access_token)).await
        {
            token = next.access_token;
        }
        vec![("Authorization".into(), format!("Bearer {token}"))]
    }

    /// After a 401: true when a retry can succeed (the token was rotated by someone else or just refreshed).
    pub(crate) async fn on_unauthorized(&self, sent_authorization: Option<&str>) -> bool {
        if self.auth.api_key().is_some() || !self.auth.auto_refresh {
            return false;
        }
        let Some(sent) = sent_authorization else { return false };
        let Some(s) = self.auth.session() else { return false };
        if s.refresh_token.is_none() {
            return false;
        }
        let sent_token = sent.strip_prefix("Bearer ").unwrap_or(sent);
        self.refresh_if_stale(Some(sent_token)).await.is_ok()
    }

    /// Rotates the refresh token. When `used_access` is given and the session already holds another
    /// access token, someone else refreshed meanwhile and no request is made.
    pub(crate) async fn refresh_if_stale(&self, used_access: Option<&str>) -> Result<Session> {
        let _guard = self.auth.refresh_lock.lock().await;
        let Some(cur) = self.auth.session() else {
            return Err(Error::Config("no session: sign in first".into()));
        };
        if let Some(used) = used_access
            && cur.access_token != used
        {
            return Ok(cur);
        }
        let rt = cur
            .refresh_token
            .clone()
            .ok_or_else(|| Error::Config("no refresh token: sign in first".into()))?;
        // Boxed: send -> credentials -> refresh -> send is a cycle (the refresh request itself is anonymous).
        let res = Box::pin(
            self.value(
                Req::post("/api/auth/refresh")
                    .json_value(json!({"refresh_token": rt}))
                    .anonymous(),
            ),
        )
        .await;
        match res {
            Ok(v) => {
                let mut next = to_session(&v)?;
                if next.user.is_none() {
                    next.user = cur.user;
                }
                self.auth.set_session(Some(next.clone()));
                Ok(next)
            }
            Err(e) => {
                // A rejected refresh token (revoked, reused) ends the session; transient failures keep it.
                if e.is_unauthenticated() {
                    self.auth.set_session(None);
                }
                Err(e)
            }
        }
    }

    fn accept(&self, v: &Value) -> Result<SignInResult> {
        if v.get("mfa_required").and_then(Value::as_bool) == Some(true) {
            return Ok(SignInResult::MfaRequired(MfaChallenge {
                mfa_token: v
                    .get("mfa_token")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                expires_in: v.get("expires_in").and_then(Value::as_u64).unwrap_or(0),
            }));
        }
        let s = to_session(v)?;
        self.auth.set_session(Some(s.clone()));
        Ok(SignInResult::Session(s))
    }
}

/// `client.auth()`: sign-up, sign-in flows, refresh and account helpers.
#[derive(Clone)]
pub struct AuthApi {
    pub(crate) t: Arc<Transport>,
}

impl AuthApi {
    async fn post(&self, path: &str, body: Value) -> Result<Value> {
        self.t.value(Req::post(path).json_value(body).anonymous()).await
    }

    /// The current session (restored from the [`SessionStore`] at start-up).
    #[must_use]
    pub fn session(&self) -> Option<Session> {
        self.t.auth.session()
    }

    /// Notifies on sign-in, token refresh and sign-out (`None` = signed out).
    #[must_use]
    pub fn watch(&self) -> watch::Receiver<Option<Session>> {
        self.t.auth.watch()
    }

    /// Adopts a session obtained elsewhere (or `None` to forget it).
    pub fn set_session(&self, session: Option<Session>) {
        self.t.auth.set_session(session);
    }

    /// Uses an API key / service-account key / personal access token (`fb_...`) for every request.
    pub fn set_api_key(&self, key: Option<String>) {
        self.t.auth.set_api_key(key);
    }

    /// Uses a fixed bearer token for every request (no refresh).
    pub fn set_access_token(&self, token: Option<String>) {
        self.t.auth.set_static_token(token);
    }

    /// Creates a user (does not sign in). Returns the user record.
    pub async fn sign_up(
        &self,
        email: &str,
        password: &str,
        collection: Option<&str>,
        data: Option<Value>,
    ) -> Result<Value> {
        let mut body = json!({"email": email, "password": password});
        if let Some(c) = collection {
            body["collection"] = json!(c);
        }
        if let Some(d) = data {
            body["data"] = d;
        }
        let v = self.post("/api/auth/signup", body).await?;
        Ok(v.get("user").cloned().unwrap_or(v))
    }

    /// Email and password. Returns a session, or an [`MfaChallenge`] when a second factor is needed.
    pub async fn sign_in(&self, email: &str, password: &str, opts: &SignInOptions) -> Result<SignInResult> {
        let v = self
            .post(
                "/api/auth/login",
                opts.apply(json!({"email": email, "password": password})),
            )
            .await?;
        self.t.accept(&v)
    }

    /// Administrator sign-in.
    pub async fn sign_in_admin(&self, email: &str, password: &str, opts: &SignInOptions) -> Result<SignInResult> {
        let v = self
            .post(
                "/api/auth/admins/login",
                opts.apply(json!({"email": email, "password": password})),
            )
            .await?;
        self.t.accept(&v)
    }

    /// Second step after [`sign_in`](Self::sign_in) returned an [`MfaChallenge`]: a TOTP or recovery code.
    pub async fn verify_mfa(&self, mfa_token: &str, code: &str) -> Result<Session> {
        let v = self
            .post("/api/auth/mfa/verify", json!({"mfa_token": mfa_token, "code": code}))
            .await?;
        self.t.accept(&v)?.into_session()
    }

    /// Emails a magic link; its token goes to [`confirm_magic_link`](Self::confirm_magic_link).
    pub async fn request_magic_link(&self, email: &str, collection: Option<&str>) -> Result<()> {
        self.post(
            "/api/auth/magic-link/request",
            json!({"email": email, "collection": collection}),
        )
        .await
        .map(drop)
    }

    /// Signs in with the token from a magic link.
    pub async fn confirm_magic_link(&self, token: &str, opts: &SignInOptions) -> Result<SignInResult> {
        let v = self
            .post("/api/auth/magic-link/confirm", opts.apply(json!({"token": token})))
            .await?;
        self.t.accept(&v)
    }

    /// Emails a one-time code.
    pub async fn request_otp(&self, email: &str, collection: Option<&str>) -> Result<()> {
        self.post(
            "/api/auth/otp/request",
            json!({"email": email, "collection": collection}),
        )
        .await
        .map(drop)
    }

    /// Signs in with an emailed one-time code.
    pub async fn verify_otp(&self, email: &str, code: &str, opts: &SignInOptions) -> Result<SignInResult> {
        let v = self
            .post(
                "/api/auth/otp/verify",
                opts.apply(json!({"email": email, "code": code})),
            )
            .await?;
        self.t.accept(&v)
    }

    /// Rotates the refresh token now (this also happens automatically before expiry and after a 401).
    pub async fn refresh(&self) -> Result<Session> {
        self.t.refresh_if_stale(None).await
    }

    /// Revokes the session on the server and forgets it locally.
    pub async fn sign_out(&self) -> Result<()> {
        let had = self.t.auth.session().is_some();
        let res = if had {
            self.t.unit(Req::post("/api/auth/logout")).await
        } else {
            Ok(())
        };
        self.t.auth.set_session(None);
        res
    }

    /// The caller as the server sees it (`subject`, `credential`, scopes, ...).
    pub async fn me(&self) -> Result<Value> {
        self.t.value(Req::get("/api/auth/me")).await
    }

    /// Emails a password-reset link.
    pub async fn request_password_reset(&self, email: &str, collection: Option<&str>) -> Result<()> {
        self.post(
            "/api/auth/password-reset/request",
            json!({"email": email, "collection": collection}),
        )
        .await
        .map(drop)
    }

    /// Sets a new password with the token from the reset email.
    pub async fn confirm_password_reset(&self, token: &str, password: &str) -> Result<()> {
        self.post(
            "/api/auth/password-reset/confirm",
            json!({"token": token, "password": password}),
        )
        .await
        .map(drop)
    }

    /// Emails an address-verification link.
    pub async fn request_email_verification(&self, email: &str, collection: Option<&str>) -> Result<()> {
        self.post(
            "/api/auth/verify-email/request",
            json!({"email": email, "collection": collection}),
        )
        .await
        .map(drop)
    }

    /// Verifies an address with the emailed token.
    pub async fn confirm_email_verification(&self, token: &str) -> Result<()> {
        self.post("/api/auth/verify-email/confirm", json!({"token": token}))
            .await
            .map(drop)
    }

    /// Changes the signed-in user's password.
    pub async fn change_password(&self, current: &str, new: &str) -> Result<()> {
        let r = Req::post("/api/auth/password/change")
            .json_value(json!({"current_password": current, "new_password": new}));
        self.t.unit(r).await
    }

    // ---- MFA (TOTP) ----

    /// Starts TOTP enrolment: returns the secret and the `otpauth://` URL for an authenticator app.
    pub async fn mfa_totp_setup(&self) -> Result<crate::generated::AuthTotpSetup> {
        self.t.json(Req::post("/api/auth/mfa/totp/setup")).await
    }

    /// Confirms enrolment with a first code; returns the one-time recovery codes.
    pub async fn mfa_totp_confirm(&self, code: &str) -> Result<crate::generated::AuthTotpEnabled> {
        self.t
            .json(Req::post("/api/auth/mfa/totp/confirm").json_value(json!({"code": code})))
            .await
    }

    /// Turns TOTP off (needs a current code).
    pub async fn mfa_totp_disable(&self, code: &str) -> Result<()> {
        self.t
            .unit(Req::post("/api/auth/mfa/totp/disable").json_value(json!({"code": code})))
            .await
    }

    /// Replaces the recovery codes (needs a current code).
    pub async fn mfa_recovery_codes(&self, code: &str) -> Result<Vec<String>> {
        let r: crate::generated::AuthRecoveryCodes = self
            .t
            .json(Req::post("/api/auth/mfa/recovery-codes").json_value(json!({"code": code})))
            .await?;
        Ok(r.recovery_codes)
    }

    // ---- API keys, personal access tokens, service accounts ----

    /// Creates a personal access token. The secret (`key`) is only returned here; use it with
    /// [`ClientBuilder::api_key`](crate::ClientBuilder::api_key).
    pub async fn create_token(
        &self,
        name: &str,
        scopes: &[&str],
        expires_in_days: Option<u32>,
    ) -> Result<crate::generated::AuthKeyCreated> {
        let body = json!({"name": name, "scopes": scopes, "expires_in_days": expires_in_days});
        self.t.json(Req::post("/api/auth/tokens").json_value(body)).await
    }

    /// Lists the caller's personal access tokens (without secrets).
    pub async fn list_tokens(&self) -> Result<Vec<Value>> {
        items(self.t.value(Req::get("/api/auth/tokens")).await?)
    }

    /// Revokes a personal access token.
    pub async fn revoke_token(&self, id: &str) -> Result<()> {
        self.t.unit(Req::delete(format!("/api/auth/tokens/{}", seg(id)))).await
    }

    /// Lists API keys visible to the caller (without secrets).
    pub async fn list_api_keys(&self) -> Result<Vec<Value>> {
        items(self.t.value(Req::get("/api/auth/api-keys")).await?)
    }

    /// Revokes an API key.
    pub async fn revoke_api_key(&self, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/auth/api-keys/{}", seg(id))))
            .await
    }

    /// Creates a service account (admin).
    pub async fn create_service_account(
        &self,
        name: &str,
        description: Option<&str>,
        role: Option<&str>,
    ) -> Result<Value> {
        let body = json!({"name": name, "description": description, "role": role});
        self.t
            .value(Req::post("/api/auth/service-accounts").json_value(body))
            .await
    }

    /// Creates a key for a service account. The secret is only returned here.
    pub async fn create_service_account_key(
        &self,
        account_id: &str,
        name: &str,
        scopes: &[&str],
        expires_in_days: Option<u32>,
    ) -> Result<crate::generated::AuthKeyCreated> {
        let body = json!({"name": name, "scopes": scopes, "expires_in_days": expires_in_days});
        self.t
            .json(Req::post(format!("/api/auth/service-accounts/{}/keys", seg(account_id))).json_value(body))
            .await
    }

    // ---- sessions ----

    /// The caller's active sessions.
    pub async fn list_sessions(&self) -> Result<Vec<Value>> {
        items(self.t.value(Req::get("/api/auth/sessions")).await?)
    }

    /// Revokes one session.
    pub async fn revoke_session(&self, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/auth/sessions/{}", seg(id))))
            .await
    }

    // ---- OAuth ----

    /// Providers configured on the server.
    pub async fn oauth_providers(&self) -> Result<Vec<Value>> {
        let v = self.t.value(Req::get("/api/auth/oauth/providers").anonymous()).await?;
        Ok(v.get("providers")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Starts a provider flow: returns the URL to open in a browser and the `state` to check on return.
    pub async fn oauth_start(&self, provider: &str, opts: &SignInOptions) -> Result<(String, String)> {
        let v = self
            .post(
                &format!("/api/auth/oauth/{}/start", seg(provider)),
                opts.apply(json!({})),
            )
            .await?;
        let get = |k: &str| {
            v.get(k)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| Error::InvalidResponse(format!("oauth start: missing {k}")))
        };
        Ok((get("authorization_url")?, get("state")?))
    }

    /// Finishes the flow with the `code` and `state` the provider sent to your redirect URI.
    pub async fn oauth_complete(&self, provider: &str, code: &str, state: &str) -> Result<SignInResult> {
        let req = Req::get(format!("/api/auth/oauth/{}/callback", seg(provider)))
            .q("code", code)
            .q("state", state)
            .anonymous();
        let v = self.t.value(req).await?;
        self.t.accept(&v)
    }
}

fn items(v: Value) -> Result<Vec<Value>> {
    match v.get("items") {
        Some(Value::Array(a)) => Ok(a.clone()),
        _ => Err(Error::InvalidResponse("expected an `items` array".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_roundtrip() {
        let s = MemoryStore::default();
        assert!(s.load().is_none());
        let sess = Session {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at_ms: 1,
            session_id: "s".into(),
            session_expires_at: "x".into(),
            subject: json!({}),
            user: None,
        };
        s.save(&sess);
        assert_eq!(s.load(), Some(sess));
        s.clear();
        assert!(s.load().is_none());
    }

    #[test]
    fn file_store_roundtrip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let s = FileStore::new(dir.path().join("session.json"));
        assert!(s.load().is_none());
        let sess = to_session(&json!({"access_token":"a","expires_in":60,"refresh_token":"r","session_id":"s","session_expires_at":"t","subject":{"kind":"user"}})).expect("session");
        s.save(&sess);
        assert_eq!(s.load(), Some(sess));
        s.clear();
        assert!(s.load().is_none());
    }

    #[test]
    fn session_needs_an_access_token() {
        let e = to_session(&json!({"expires_in":60,"session_id":"s","session_expires_at":"t"})).expect_err("no token");
        assert_eq!(e.code(), "invalid_response");
    }
}
