//! The auth service: credentials in, identities and sessions out.
//!
//! Split by concern: this module holds the service handle, signing keys,
//! sessions and token verification, lockout and the audit log;
//! [`flows`](self::flows) the end-user and administrator sign-in flows and MFA;
//! [`machine`](self::machine) service accounts, API keys and personal access
//! tokens.

mod flows;
mod impersonation;
mod machine;

use std::sync::{Arc, RwLock};
use std::time::Duration;

use forgebase_data::db::{Backend, ConstraintKind, DbError, Value};
use forgebase_data::schema::{Collection, CollectionType, FieldKind};
use forgebase_data::{AccessContext, Authorizer, DataError, DataService};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json, json};

use crate::crypto::{self, Keyring};
use crate::limits::RateLimiter;
use crate::mailer::{DevMailer, Mailer};
use crate::password::PasswordHasher;
use crate::store::{self, Tx, first, i, ot, read, rfc3339, rows, t};
use crate::{AuthConfig, AuthError, AuthMethod, Authenticated, CredentialKind, Subject};

pub(crate) use flows::normalize_email;
pub use flows::{LoginOptions, LoginOutcome, TotpSetup, UserView};
pub use impersonation::{ImpersonationGrant, ImpersonationView};
pub use machine::{ADMIN_PASSWORD_MIN, AdminView, ApiKeyView, CreatedKey, ServiceAccountView};

/// Request metadata recorded with sessions and audit events.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientInfo {
    /// Client IP (from the socket, or `X-Forwarded-For` when trusted).
    pub ip: Option<String>,
    /// `User-Agent`, truncated to 256 characters.
    pub user_agent: Option<String>,
    /// Request id.
    pub request_id: Option<String>,
    /// Realm the request selects (`X-ForgeBase-Realm`); `None` = `default`.
    pub realm: Option<String>,
    /// Cloudflare Turnstile token sent with the request (`cf-turnstile-response`
    /// or `x-turnstile-token` header).
    pub turnstile_token: Option<String>,
    /// Administrator acting as the caller (impersonation session); recorded
    /// in every audit row of the request.
    pub impersonator: Option<String>,
}

/// Tokens of a new or refreshed session.
#[derive(Debug, Clone, Serialize)]
pub struct SessionTokens {
    /// Always `Bearer`.
    pub token_type: &'static str,
    /// Short-lived access token (absent in cookie mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    /// Seconds until the access token expires.
    pub expires_in: u64,
    /// Opaque rotating refresh token (absent in cookie mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Session id.
    pub session_id: String,
    /// CSRF token to send as `X-CSRF-Token` (cookie mode only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csrf_token: Option<String>,
    /// Session cookie value (cookie mode; set by the HTTP layer, never serialised).
    #[serde(skip)]
    pub cookie: Option<String>,
    /// Absolute session expiry (RFC 3339).
    pub session_expires_at: String,
    /// The signed-in subject.
    pub subject: Subject,
}

/// A session as shown in the device list.
#[derive(Debug, Clone, Serialize)]
pub struct SessionView {
    /// Session id.
    pub id: String,
    /// First factor.
    pub auth_method: String,
    /// Passed a second factor.
    pub mfa: bool,
    /// Cookie session.
    pub cookie: bool,
    /// IP at sign-in or last refresh.
    pub ip: Option<String>,
    /// User agent.
    pub user_agent: Option<String>,
    /// Name the client gave the device.
    pub device_name: Option<String>,
    /// Created.
    pub created_at: String,
    /// Last refresh or cookie use.
    pub last_seen_at: String,
    /// Absolute expiry.
    pub expires_at: String,
    /// Whether this is the session making the request.
    pub current: bool,
    /// Administrator who started this session as an impersonation.
    pub impersonator: Option<String>,
}

/// One audit-log entry.
#[derive(Debug, Clone, Serialize)]
pub struct AuditEntry {
    /// Sequence number.
    pub id: i64,
    /// Time (RFC 3339).
    pub at: String,
    /// Event name, e.g. `login`, `refresh.reuse_detected`.
    pub event: String,
    /// `success` or `failure`.
    pub outcome: String,
    /// Who acted (`anonymous`, `user:users/abc`, `admin:xyz`, ...).
    pub actor: String,
    /// Subject type of the account concerned.
    pub subject_type: Option<String>,
    /// Collection of the account concerned.
    pub collection: Option<String>,
    /// Id of the account concerned.
    pub subject_id: Option<String>,
    /// Client IP.
    pub ip: Option<String>,
    /// User agent.
    pub user_agent: Option<String>,
    /// Request id.
    pub request_id: Option<String>,
    /// Extra non-secret detail.
    pub detail: Json,
    /// Administrator acting as `actor` (impersonation), if any.
    pub impersonator: Option<String>,
}

/// Filter for [`AuthService::audit_log`].
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AuditQuery {
    /// Events starting with this prefix (`login`, `mfa.`).
    pub event: Option<String>,
    /// Subject id.
    pub subject_id: Option<String>,
    /// Entries with an id lower than this (paging backwards).
    pub before: Option<i64>,
    /// Page size (default 50, at most 500).
    pub limit: Option<u32>,
}

#[derive(Clone)]
struct SigningKey {
    kid: String,
    secret: Vec<u8>,
    active: bool,
    created_at: i64,
    verify_until: Option<i64>,
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SigningKey")
            .field("kid", &self.kid)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

/// Claims of an access token.
#[derive(Debug, Serialize, Deserialize)]
struct AccessClaims {
    iss: String,
    typ: String,
    /// `user` or `admin`.
    st: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    col: Option<String>,
    sub: String,
    sid: String,
    iat: i64,
    exp: i64,
    jti: String,
    amr: Vec<String>,
}

const ISSUER: &str = "forgebase";

/// Lifetime of an admin known-device token (90 days).
pub const ADMIN_DEVICE_TTL_SECS: i64 = 90 * 86_400;
/// Failed admin sign-ins one known device may make per window before its
/// token stops bypassing the per-IP budget and the account lockout.
pub(crate) const ADMIN_DEVICE_FAILURES: u32 = 5;

/// The authentication service. Cheap to clone.
#[derive(Debug, Clone)]
pub struct AuthService {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    data: DataService,
    db: Arc<dyn Backend>,
    cfg: AuthConfig,
    keyring: Keyring,
    hasher: PasswordHasher,
    mailer: Arc<dyn Mailer>,
    limiter: RateLimiter,
    keys: RwLock<Vec<SigningKey>>,
    social: crate::social::Social,
    webauthn: crate::webauthn::WebAuthnConfig,
    realms: crate::realm::RealmDirectory,
    sms: RwLock<Arc<dyn crate::sms::SmsProvider>>,
    realm_sms: RwLock<std::collections::HashMap<String, Arc<dyn crate::sms::SmsProvider>>>,
    change_hook: RwLock<Option<Hook>>,
    realm_creator: RwLock<Option<Creator>>,
    idp: crate::idp::IdpState,
    /// First-run setup token (FB-90).
    setup: crate::setup::SetupState,
}

#[derive(Clone)]
struct Creator(crate::realm::RealmCreator);

impl std::fmt::Debug for Creator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RealmCreator")
    }
}

#[derive(Clone)]
struct Hook(ChangeHook);

impl std::fmt::Debug for Hook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ChangeHook")
    }
}

/// Called after the auth service changed state the policy engine caches
/// (suspensions); the server wires it to `PolicyService::reload`.
pub type ChangeHook = Arc<dyn Fn() + Send + Sync>;

impl AuthService {
    /// Current Unix time in seconds, from the configured clock.
    pub(crate) fn now(&self) -> i64 {
        self.inner.cfg.clock.now()
    }

    /// Builds the data service (with the Argon2id hasher for `password`
    /// fields) and the auth service on a migrated database. Mail goes to the
    /// development mailer in `<data_dir>/mail/` unless `mailer` is given.
    pub async fn open_with_data(
        backend: Arc<dyn Backend>,
        authorizer: Arc<dyn Authorizer>,
        data_dir: &std::path::Path,
        cfg: AuthConfig,
        mailer: Option<Arc<dyn Mailer>>,
    ) -> Result<Self, AuthError> {
        let hasher = PasswordHasher::new(cfg.argon2)?;
        let data = DataService::with_hasher(backend, authorizer, Some(Arc::new(hasher.clone()))).await?;
        let mailer = mailer.unwrap_or_else(|| Arc::new(DevMailer::new(data_dir.join("mail"))));
        Self::open(data, hasher, mailer, cfg).await
    }

    /// Opens the service on an existing data service. `hasher` must be the
    /// one the data service was built with (so prehashed passwords are used).
    pub async fn open(
        data: DataService,
        hasher: PasswordHasher,
        mailer: Arc<dyn Mailer>,
        cfg: AuthConfig,
    ) -> Result<Self, AuthError> {
        let public_url = cfg.public_url.clone();
        let path = cfg.master_key_path.clone();
        let keyring = tokio::task::spawn_blocking(move || Keyring::load_or_create(&path))
            .await
            .map_err(|e| AuthError::Internal(e.to_string()))??;
        let svc = Self {
            inner: Arc::new(Inner {
                db: data.backend().clone(),
                data,
                cfg,
                keyring,
                hasher,
                mailer,
                limiter: RateLimiter::new(),
                keys: RwLock::new(Vec::new()),
                social: crate::social::Social::default(),
                webauthn: crate::webauthn::WebAuthnConfig::from_public_url(&public_url),
                realms: crate::realm::RealmDirectory::default(),
                sms: RwLock::new(Arc::new(crate::sms::DevSmsProvider::default())),
                realm_sms: RwLock::new(std::collections::HashMap::new()),
                change_hook: RwLock::new(None),
                realm_creator: RwLock::new(None),
                idp: crate::idp::IdpState::default(),
                setup: crate::setup::SetupState::default(),
            }),
        };
        svc.install_default_http_client()?;
        svc.ensure_signing_key(false).await?;
        svc.load_provider_file().await?;
        if let Some(name) = svc.inner.cfg.default_collection.clone() {
            svc.ensure_auth_collection(&name).await?;
        }
        svc.reconcile_default_realm().await?;
        svc.load_sms_file().await?;
        svc.load_identity_admin_config().await?;
        Ok(svc)
    }

    /// Trust settings for outbound TLS (`tls_ca_file` added to webpki-roots).
    #[cfg(feature = "https")]
    pub fn tls_settings(&self) -> Result<crate::net::TlsSettings, AuthError> {
        let tls = crate::net::TlsSettings::new();
        match &self.inner.cfg.tls_ca_file {
            Some(p) => tls.with_pem_file(p),
            None => Ok(tls),
        }
    }

    /// Installs the rustls client for provider, Turnstile and SMS calls
    /// (feature `https`); without it the plain-HTTP client stays.
    fn install_default_http_client(&self) -> Result<(), AuthError> {
        #[cfg(feature = "https")]
        {
            let client = crate::net::HttpsClient::new(&self.tls_settings()?)?
                .public_only(!self.inner.cfg.allow_private_outbound);
            self.set_http_client(Arc::new(client));
        }
        Ok(())
    }

    /// The realm directory (shared with the policy engine).
    pub fn realms(&self) -> &crate::realm::RealmDirectory {
        &self.inner.realms
    }

    /// The SMS provider (development provider unless replaced).
    pub fn sms(&self) -> Arc<dyn crate::sms::SmsProvider> {
        self.inner.sms.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Replaces the SMS provider.
    pub fn set_sms_provider(&self, p: Arc<dyn crate::sms::SmsProvider>) {
        *self.inner.sms.write().unwrap_or_else(|e| e.into_inner()) = p;
    }

    /// The SMS gateway configured for a realm, if any.
    pub(crate) fn realm_sms(&self, realm: &str) -> Option<Arc<dyn crate::sms::SmsProvider>> {
        self.inner
            .realm_sms
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(realm)
            .cloned()
    }

    /// Sets or clears (`None`) the SMS gateway of a realm.
    pub(crate) fn set_realm_sms(&self, realm: &str, p: Option<Arc<dyn crate::sms::SmsProvider>>) {
        let mut m = self.inner.realm_sms.write().unwrap_or_else(|e| e.into_inner());
        match p {
            Some(p) => m.insert(realm.to_owned(), p),
            None => m.remove(realm),
        };
    }

    /// Installs the function that creates the realm record (the policy
    /// engine owns realms); the server wires it. Needed for
    /// `POST /api/auth/realms`.
    pub fn set_realm_creator(&self, f: crate::realm::RealmCreator) {
        *self.inner.realm_creator.write().unwrap_or_else(|e| e.into_inner()) = Some(Creator(f));
    }

    pub(crate) fn realm_creator(&self) -> Option<crate::realm::RealmCreator> {
        self.inner
            .realm_creator
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|c| c.0.clone())
    }

    /// Installs the hook run after suspensions change.
    pub fn set_change_hook(&self, hook: ChangeHook) {
        *self.inner.change_hook.write().unwrap_or_else(|e| e.into_inner()) = Some(Hook(hook));
    }

    pub(crate) fn changed(&self) {
        let hook = self.inner.change_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(Hook(h)) = hook {
            h();
        }
    }

    /// The outbound HTTP client (providers, Turnstile, SMS gateways).
    pub fn http_client(&self) -> Arc<dyn crate::social::HttpClient> {
        self.inner.social.http()
    }

    /// The data service (shared; built with the Argon2id hasher).
    pub fn data(&self) -> &DataService {
        &self.inner.data
    }

    /// Settings.
    pub fn config(&self) -> &AuthConfig {
        &self.inner.cfg
    }

    /// The password hasher (also the data core's `SecretHasher`).
    pub fn hasher(&self) -> &PasswordHasher {
        &self.inner.hasher
    }

    pub(crate) fn social(&self) -> &crate::social::Social {
        &self.inner.social
    }

    pub(crate) fn webauthn_config(&self) -> &crate::webauthn::WebAuthnConfig {
        &self.inner.webauthn
    }

    pub(crate) fn idp_state(&self) -> &crate::idp::IdpState {
        &self.inner.idp
    }

    pub(crate) fn keyring(&self) -> &Keyring {
        &self.inner.keyring
    }

    fn db(&self) -> &Arc<dyn Backend> {
        &self.inner.db
    }

    /// Creates the auth collection `name` if it does not exist.
    pub async fn ensure_auth_collection(&self, name: &str) -> Result<(), AuthError> {
        let sys = AccessContext::system();
        match self.inner.data.get_collection(&sys, name).await {
            Ok(c) if c.kind == CollectionType::Auth => Ok(()),
            Ok(_) => Err(AuthError::Internal(format!(
                "collection {name:?} exists but is not an auth collection"
            ))),
            Err(DataError::CollectionNotFound(_)) => {
                let mut c = Collection::base(name);
                c.kind = CollectionType::Auth;
                match self.inner.data.create_collection(&sys, c).await {
                    Ok(_) => {
                        tracing::info!(collection = name, "created default auth collection");
                        Ok(())
                    }
                    // Another process created it concurrently.
                    Err(DataError::AlreadyExists(_)) => self.inner.data.reload_catalog().await.map_err(Into::into),
                    Err(e) => Err(e.into()),
                }
            }
            Err(e) => Err(e.into()),
        }
    }

    // -----------------------------------------------------------------------
    // Rate limits and lockout
    // -----------------------------------------------------------------------

    /// Counts one credential attempt for the client's IP.
    pub(crate) fn limit_attempt(&self, ci: &ClientInfo) -> Result<(), AuthError> {
        let ip = ci.ip.as_deref().unwrap_or("unknown");
        self.inner
            .limiter
            .hit(
                "attempt",
                ip,
                self.inner.cfg.ip_attempts_per_minute,
                Duration::from_secs(60),
            )
            .map_err(|retry_after| AuthError::RateLimited { retry_after })
    }

    /// Gives back the credential attempt of a sign-in that succeeded, so
    /// the per-IP budget counts failures only (FB-80).
    pub(crate) fn refund_attempt(&self, ci: &ClientInfo) {
        let ip = ci.ip.as_deref().unwrap_or("unknown");
        self.inner.limiter.refund("attempt", ip);
    }

    /// Counts one hit of `key` in `bucket` (e.g. SMS sends per number).
    pub(crate) fn limit_key(&self, bucket: &str, key: &str, limit: u32, window: Duration) -> Result<(), AuthError> {
        self.inner
            .limiter
            .hit(bucket, key, limit, window)
            .map_err(|retry_after| AuthError::RateLimited { retry_after })
    }

    /// Counts one mail-sending request for the client's IP.
    pub(crate) fn limit_mail(&self, ci: &ClientInfo) -> Result<(), AuthError> {
        let ip = ci.ip.as_deref().unwrap_or("unknown");
        self.inner
            .limiter
            .hit("mail", ip, self.inner.cfg.ip_mail_per_hour, Duration::from_secs(3600))
            .map_err(|retry_after| AuthError::RateLimited { retry_after })
    }

    /// Fails while `key` is locked.
    pub(crate) async fn lockout_check(&self, key: &str) -> Result<(), AuthError> {
        let r = read(
            self.db(),
            "SELECT locked_until FROM _forgebase_auth_lockouts WHERE key = ?1",
            vec![t(key)],
        )
        .await?;
        let until = first(&r).map(|row| row.i64("locked_until")).unwrap_or(0);
        let now = self.now();
        if until > now {
            return Err(AuthError::RateLimited {
                retry_after: (until - now) as u64,
            });
        }
        Ok(())
    }

    /// Records a failure for `key`; returns `true` when this failure locked it.
    pub(crate) async fn lockout_fail(&self, key: &str) -> Result<bool, AuthError> {
        let cfg = &self.inner.cfg;
        let now = self.now();
        let mut tx = Tx::begin(self.db()).await?;
        let r = tx
            .read(
                "SELECT failures, window_start, lock_count FROM _forgebase_auth_lockouts WHERE key = ?1",
                vec![t(key)],
            )
            .await?;
        let (mut failures, mut window_start, mut lock_count) = first(&r)
            .map(|row| (row.i64("failures"), row.i64("window_start"), row.i64("lock_count")))
            .unwrap_or((0, now, 0));
        if now - window_start >= cfg.lockout_window.as_secs() as i64 {
            failures = 0;
            window_start = now;
        }
        failures += 1;
        let mut locked_until = 0;
        let mut locked = false;
        if failures >= cfg.lockout_threshold {
            let base = cfg.lockout_duration.as_secs() as i64;
            let secs = base.saturating_mul(1i64 << lock_count.clamp(0, 16)).min(24 * 3600);
            locked_until = now + secs;
            lock_count += 1;
            failures = 0;
            window_start = now;
            locked = true;
        }
        tx.exec(
            "INSERT INTO _forgebase_auth_lockouts (key, failures, window_start, locked_until, lock_count)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (key) DO UPDATE SET failures = ?2, window_start = ?3,
                 locked_until = max(locked_until, ?4), lock_count = ?5",
            vec![t(key), i(failures), i(window_start), i(locked_until), i(lock_count)],
        )
        .await?;
        tx.commit().await?;
        Ok(locked)
    }

    /// Clears the failure count of `key` after a success.
    pub(crate) async fn lockout_clear(&self, key: &str) -> Result<(), AuthError> {
        let mut tx = Tx::begin(self.db()).await?;
        tx.exec("DELETE FROM _forgebase_auth_lockouts WHERE key = ?1", vec![t(key)])
            .await?;
        tx.commit().await
    }

    // -----------------------------------------------------------------------
    // Audit
    // -----------------------------------------------------------------------

    /// Appends an audit entry in its own transaction. Failures to audit are
    /// logged and returned.
    pub(crate) async fn audit(
        &self,
        event: &str,
        success: bool,
        ci: &ClientInfo,
        actor: &str,
        subject: Option<&Subject>,
        detail: Json,
    ) -> Result<(), AuthError> {
        let mut tx = Tx::begin(self.db()).await?;
        audit_in(&mut tx, event, success, ci, actor, subject, detail).await?;
        tx.commit().await
    }

    /// Audit entries, newest first (administrators).
    pub async fn audit_log(&self, caller: &Authenticated, q: &AuditQuery) -> Result<Vec<AuditEntry>, AuthError> {
        require_admin(caller)?;
        let limit = i64::from(q.limit.unwrap_or(50).clamp(1, 500));
        let r = read(
            self.db(),
            "SELECT id, at, event, outcome, actor, subject_type, collection, subject_id, ip, user_agent, request_id, detail, impersonator
             FROM _forgebase_auth_audit
             WHERE (?1 IS NULL OR substr(event, 1, length(?1)) = ?1)
               AND (?2 IS NULL OR subject_id = ?2)
               AND (?3 IS NULL OR id < ?3)
             ORDER BY id DESC LIMIT ?4",
            vec![ot(q.event.clone()), ot(q.subject_id.clone()), q.before.map_or(Value::Null, i), i(limit)],
        )
        .await?;
        Ok(rows(&r).map(audit_entry).collect())
    }

    /// Sign-in history of the caller (or, for administrators, of `subject`).
    pub async fn login_history(&self, caller: &Authenticated, limit: u32) -> Result<Vec<AuditEntry>, AuthError> {
        let (st, coll, id) = subject_cols(&caller.subject);
        let r = read(
            self.db(),
            "SELECT id, at, event, outcome, actor, subject_type, collection, subject_id, ip, user_agent, request_id, detail, impersonator
             FROM _forgebase_auth_audit
             WHERE subject_type = ?1 AND collection IS ?2 AND subject_id = ?3
               AND (event LIKE 'login%' OR event = 'mfa.verify')
             ORDER BY id DESC LIMIT ?4",
            vec![t(st), ot(coll), t(id), i(i64::from(limit.clamp(1, 200)))],
        )
        .await?;
        Ok(rows(&r).map(audit_entry).collect())
    }

    // -----------------------------------------------------------------------
    // Signing keys
    // -----------------------------------------------------------------------

    async fn load_keys(&self) -> Result<(), AuthError> {
        let r = read(
            self.db(),
            "SELECT kid, secret_enc, status, created_at, verify_until FROM _forgebase_auth_keys",
            vec![],
        )
        .await?;
        let mut keys = Vec::new();
        for row in rows(&r) {
            let kid = row.str("kid");
            let secret = self.inner.keyring.open(&row.blob("secret_enc"), kid.as_bytes())?;
            keys.push(SigningKey {
                active: row.str("status") == "active",
                created_at: row.i64("created_at"),
                verify_until: row.opt_i64("verify_until"),
                kid,
                secret,
            });
        }
        *self.inner.keys.write().unwrap_or_else(|p| p.into_inner()) = keys;
        Ok(())
    }

    /// Makes sure an active signing key exists; rotates it when it is older
    /// than the rotation period, or when `force` is set. Retired keys keep
    /// verifying until every token they signed has expired.
    pub(crate) async fn ensure_signing_key(&self, force: bool) -> Result<(), AuthError> {
        let now = self.now();
        let rotation = self.inner.cfg.signing_key_rotation.as_secs() as i64;
        let grace = self.inner.cfg.access_token_ttl.as_secs() as i64 + 60;
        let mut tx = Tx::begin(self.db()).await?;
        tx.exec(
            "DELETE FROM _forgebase_auth_keys WHERE status = 'retired' AND verify_until < ?1",
            vec![i(now)],
        )
        .await?;
        let r = tx
            .read(
                "SELECT kid, created_at FROM _forgebase_auth_keys WHERE status = 'active'",
                vec![],
            )
            .await?;
        let active = first(&r).map(|row| (row.str("kid"), row.i64("created_at")));
        let rotate = match &active {
            None => true,
            Some((_, created)) => force || now - created >= rotation,
        };
        if rotate {
            if let Some((kid, _)) = &active {
                tx.exec(
                    "UPDATE _forgebase_auth_keys SET status = 'retired', verify_until = ?2 WHERE kid = ?1",
                    vec![t(kid.clone()), i(now + grace)],
                )
                .await?;
            }
            let kid = crypto::random_id(16)?;
            let secret = crypto::random_bytes(32)?;
            let sealed = self.inner.keyring.seal(&secret, kid.as_bytes())?;
            tx.exec(
                "INSERT INTO _forgebase_auth_keys (kid, alg, secret_enc, status, created_at) VALUES (?1, 'HS256', ?2, 'active', ?3)",
                vec![t(kid.clone()), Value::Blob(sealed), i(now)],
            )
            .await?;
            audit_in(
                &mut tx,
                "signing_key.rotated",
                true,
                &ClientInfo::default(),
                "system",
                None,
                json!({"kid": kid, "forced": force}),
            )
            .await?;
        }
        match tx.commit().await {
            Ok(()) => {}
            // Another process rotated at the same moment: use its key.
            Err(AuthError::Db(DbError::Constraint { .. })) => {}
            Err(e) => return Err(e),
        }
        self.load_keys().await
    }

    /// Rotates the signing key now (administrators). Tokens signed with the
    /// old key stay valid until they expire.
    pub async fn rotate_signing_key(&self, caller: &Authenticated, ci: &ClientInfo) -> Result<String, AuthError> {
        require_admin(caller)?;
        self.ensure_signing_key(true).await?;
        let kid = self.active_key().await?.kid;
        self.audit(
            "signing_key.rotate_requested",
            true,
            ci,
            &caller.subject.actor(),
            None,
            json!({}),
        )
        .await?;
        Ok(kid)
    }

    async fn active_key(&self) -> Result<SigningKey, AuthError> {
        let rotation = self.inner.cfg.signing_key_rotation.as_secs() as i64;
        let current = {
            let keys = self.inner.keys.read().unwrap_or_else(|p| p.into_inner());
            keys.iter().find(|k| k.active).cloned()
        };
        match current {
            Some(k) if self.now() - k.created_at < rotation => Ok(k),
            _ => {
                self.ensure_signing_key(false).await?;
                let keys = self.inner.keys.read().unwrap_or_else(|p| p.into_inner());
                keys.iter()
                    .find(|k| k.active)
                    .cloned()
                    .ok_or_else(|| AuthError::Internal("no active signing key".into()))
            }
        }
    }

    async fn verification_key(&self, kid: &str) -> Result<Option<SigningKey>, AuthError> {
        let find = |svc: &Self| {
            let keys = svc.inner.keys.read().unwrap_or_else(|p| p.into_inner());
            keys.iter().find(|k| k.kid == kid).cloned()
        };
        if let Some(k) = find(self) {
            return Ok(Some(k));
        }
        // Unknown kid: another process may have rotated. Reload once.
        self.load_keys().await?;
        Ok(find(self))
    }

    // -----------------------------------------------------------------------
    // Sessions
    // -----------------------------------------------------------------------

    /// Creates a session for a subject that has completed sign-in (all
    /// required factors) and returns its tokens. Also the entry point for
    /// later sign-in methods (OAuth/OIDC, passkeys).
    pub async fn issue_session(
        &self,
        subject: &Subject,
        method: AuthMethod,
        mfa: bool,
        ci: &ClientInfo,
        opts: &LoginOptions,
    ) -> Result<SessionTokens, AuthError> {
        self.ensure_active(subject).await?;
        let (st, coll, sid_subject) = match subject {
            Subject::User { collection, id } => ("user", Some(collection.clone()), id.clone()),
            Subject::Admin { id } => ("admin", None, id.clone()),
            _ => {
                return Err(AuthError::Forbidden(
                    "service accounts use API keys, not sessions".into(),
                ));
            }
        };
        let cfg = &self.inner.cfg;
        let now = self.now();
        let expires_at = now + cfg.session_ttl.as_secs() as i64;
        let sid = crypto::random_id(24)?;
        let refresh = crypto::random_token(32)?;
        let cookie = if opts.cookie {
            Some(crypto::random_token(32)?)
        } else {
            None
        };
        let kr = &self.inner.keyring;
        let mut tx = Tx::begin(self.db()).await?;
        tx.exec(
            "INSERT INTO _forgebase_auth_sessions
                (id, subject_type, collection, subject_id, auth_method, mfa, cookie_hash, ip, user_agent, device_name,
                 created_at, last_seen_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, ?12)",
            vec![
                t(sid.clone()),
                t(st),
                ot(coll),
                t(sid_subject.clone()),
                t(method.as_str()),
                i(i64::from(mfa)),
                ot(cookie.as_deref().map(|c| kr.token_hash("cookie", c))),
                ot(ci.ip.clone()),
                ot(ci.user_agent.clone()),
                ot(opts.device_name.clone()),
                i(now),
                i(expires_at),
            ],
        )
        .await?;
        if !opts.cookie {
            tx.exec(
                "INSERT INTO _forgebase_auth_refresh_tokens (token_hash, session_id, generation, created_at, expires_at)
                 VALUES (?1, ?2, 1, ?3, ?4)",
                vec![t(kr.token_hash("refresh", &refresh)), t(sid.clone()), i(now), i(expires_at)],
            )
            .await?;
        }
        if st == "admin" {
            tx.exec(
                "UPDATE _forgebase_admins SET last_login_at = ?2 WHERE id = ?1",
                vec![t(sid_subject), i(now)],
            )
            .await?;
        }
        audit_in(
            &mut tx,
            "login",
            true,
            ci,
            &subject.actor(),
            Some(subject),
            json!({"method": method.as_str(), "mfa": mfa, "session_id": sid, "cookie": opts.cookie}),
        )
        .await?;
        tx.commit().await?;

        let (access, expires_in) = if opts.cookie {
            (None, cfg.session_idle_timeout.as_secs())
        } else {
            let (tok, exp) = self.sign_access(subject, &sid, method, mfa).await?;
            (Some(tok), exp)
        };
        Ok(SessionTokens {
            token_type: "Bearer",
            access_token: access,
            expires_in,
            refresh_token: (!opts.cookie).then_some(refresh),
            csrf_token: opts.cookie.then(|| kr.csrf_token(&sid)),
            cookie,
            session_id: sid,
            session_expires_at: rfc3339(expires_at),
            subject: subject.clone(),
        })
    }

    async fn sign_access(
        &self,
        subject: &Subject,
        sid: &str,
        method: AuthMethod,
        mfa: bool,
    ) -> Result<(String, u64), AuthError> {
        let ttl = self.inner.cfg.access_token_ttl.as_secs();
        self.sign_access_ttl(subject, sid, method, mfa, ttl).await
    }

    async fn sign_access_ttl(
        &self,
        subject: &Subject,
        sid: &str,
        method: AuthMethod,
        mfa: bool,
        ttl: u64,
    ) -> Result<(String, u64), AuthError> {
        let key = self.active_key().await?;
        let now = self.now();
        let (st, col, sub) = match subject {
            Subject::User { collection, id } => ("user", Some(collection.clone()), id.clone()),
            Subject::Admin { id } => ("admin", None, id.clone()),
            _ => return Err(AuthError::Internal("cannot sign a token for this subject".into())),
        };
        let mut amr = vec![method.as_str().to_owned()];
        if mfa {
            amr.push("totp".into());
        }
        let claims = AccessClaims {
            iss: ISSUER.into(),
            typ: "access".into(),
            st: st.into(),
            col,
            sub,
            sid: sid.into(),
            iat: now,
            exp: now + ttl as i64,
            jti: crypto::random_id(16)?,
            amr,
        };
        Ok((crypto::jwt_sign(&key.kid, &key.secret, &claims)?, ttl))
    }

    /// Verifies an access token: signature, expiry, issuer, and that its
    /// session is still active (revocation takes effect immediately).
    pub async fn authenticate_access_token(&self, token: &str) -> Result<Authenticated, AuthError> {
        let bad = AuthError::InvalidToken("invalid access token");
        let header = crypto::jwt_header(token).ok_or(AuthError::InvalidToken("malformed access token"))?;
        let key = self.verification_key(&header.kid).await?.ok_or(bad)?;
        let claims: AccessClaims =
            crypto::jwt_verify(token, &key.secret).ok_or(AuthError::InvalidToken("invalid access token"))?;
        let now = self.now();
        if claims.iss != ISSUER || claims.typ != "access" {
            return Err(AuthError::InvalidToken("invalid access token"));
        }
        if claims.exp <= now {
            return Err(AuthError::InvalidToken("access token expired"));
        }
        if claims.iat > now + 60 || (!key.active && key.verify_until.is_some_and(|v| v < now)) {
            return Err(AuthError::InvalidToken("invalid access token"));
        }
        let r = read(
            self.db(),
            "SELECT subject_type, collection, subject_id, mfa, revoked_at, expires_at, last_seen_at, impersonator
             FROM _forgebase_auth_sessions WHERE id = ?1",
            vec![t(claims.sid.clone())],
        )
        .await?;
        let row = first(&r).ok_or(AuthError::InvalidToken("session not found"))?;
        if row.opt_i64("revoked_at").is_some() {
            return Err(AuthError::InvalidToken("session revoked"));
        }
        if row.i64("expires_at") <= now {
            return Err(AuthError::InvalidToken("session expired"));
        }
        if row.str("subject_type") != claims.st
            || row.str("subject_id") != claims.sub
            || row.opt_str("collection") != claims.col
        {
            return Err(AuthError::InvalidToken("invalid access token"));
        }
        let subject = match (claims.st.as_str(), claims.col) {
            ("user", Some(collection)) => Subject::User {
                collection,
                id: claims.sub,
            },
            ("admin", None) => Subject::Admin { id: claims.sub },
            _ => return Err(AuthError::InvalidToken("invalid access token")),
        };
        Ok(Authenticated {
            subject,
            credential: CredentialKind::AccessToken,
            credential_id: claims.sid,
            scopes: None,
            mfa: row.i64("mfa") == 1,
            impersonator: row.opt_str("impersonator"),
        })
    }

    /// Resolves a session cookie. Unsafe methods must additionally pass the
    /// CSRF check (done by the HTTP layer with [`AuthService::csrf_token`]).
    pub async fn authenticate_cookie(&self, cookie: &str) -> Result<Authenticated, AuthError> {
        if cookie.is_empty() || cookie.len() > 256 {
            return Err(AuthError::InvalidToken("invalid session cookie"));
        }
        let hash = self.inner.keyring.token_hash("cookie", cookie);
        let r = read(
            self.db(),
            "SELECT id, subject_type, collection, subject_id, mfa, revoked_at, expires_at, last_seen_at
             FROM _forgebase_auth_sessions WHERE cookie_hash = ?1",
            vec![t(hash)],
        )
        .await?;
        let row = first(&r).ok_or(AuthError::InvalidToken("invalid session cookie"))?;
        let now = self.now();
        let idle = self.inner.cfg.session_idle_timeout.as_secs() as i64;
        if row.opt_i64("revoked_at").is_some() {
            return Err(AuthError::InvalidToken("session revoked"));
        }
        if row.i64("expires_at") <= now || row.i64("last_seen_at") + idle <= now {
            return Err(AuthError::InvalidToken("session expired"));
        }
        let sid = row.str("id");
        let subject = session_subject(&row)?;
        if now - row.i64("last_seen_at") >= 60 {
            // Sliding idle expiry, at most one write per minute per session.
            if !self.subject_exists(&subject).await? {
                self.revoke_sessions_where_id(&sid, "account_deleted").await?;
                return Err(AuthError::InvalidToken("session revoked"));
            }
            let mut tx = Tx::begin(self.db()).await?;
            tx.exec(
                "UPDATE _forgebase_auth_sessions SET last_seen_at = ?2 WHERE id = ?1",
                vec![t(sid.clone()), i(now)],
            )
            .await?;
            tx.commit().await?;
        }
        Ok(Authenticated {
            subject,
            credential: CredentialKind::SessionCookie,
            credential_id: sid,
            scopes: None,
            mfa: row.i64("mfa") == 1,
            impersonator: None,
        })
    }

    /// The CSRF token of a cookie session.
    pub fn csrf_token(&self, session_id: &str) -> String {
        self.inner.keyring.csrf_token(session_id)
    }

    /// First-run setup state (FB-90).
    pub(crate) fn setup_state(&self) -> &crate::setup::SetupState {
        &self.inner.setup
    }

    /// A known-device token for the administrator `email` (FB-80): set as
    /// an `HttpOnly` cookie after a successful admin sign-in.
    pub fn admin_device_token(&self, email: &str) -> String {
        let email = email.trim().to_lowercase();
        let issued = self.now();
        format!("{issued}.{}", self.inner.keyring.admin_device_mac(&email, issued))
    }

    /// Whether `token` is a known-device token for `email`, issued in the
    /// last [`ADMIN_DEVICE_TTL_SECS`].
    pub fn admin_device_valid(&self, email: &str, token: &str) -> bool {
        let email = email.trim().to_lowercase();
        let Some((issued, mac)) = token.split_once('.') else {
            return false;
        };
        let Ok(issued) = issued.parse::<i64>() else {
            return false;
        };
        let now = self.now();
        let fresh = issued <= now + 60 && now - issued <= ADMIN_DEVICE_TTL_SECS;
        fresh
            && crate::crypto::ct_eq(
                mac.as_bytes(),
                self.inner.keyring.admin_device_mac(&email, issued).as_bytes(),
            )
    }

    /// Exchanges a refresh token for a new access token and a new refresh
    /// token. A refresh token works once: presenting a used one is treated as
    /// theft and revokes the whole session (the token family).
    pub async fn refresh(&self, refresh_token: &str, ci: &ClientInfo) -> Result<SessionTokens, AuthError> {
        self.limit_attempt(ci)?;
        if refresh_token.is_empty() || refresh_token.len() > 256 {
            return Err(AuthError::InvalidToken("invalid refresh token"));
        }
        let kr = &self.inner.keyring;
        let cfg = &self.inner.cfg;
        let hash = kr.token_hash("refresh", refresh_token);
        let now = self.now();
        let mut tx = Tx::begin(self.db()).await?;
        let r = tx
            .read(
                "SELECT r.session_id, r.generation, r.used_at, r.expires_at AS token_expires,
                        s.subject_type, s.collection, s.subject_id, s.auth_method, s.mfa, s.revoked_at,
                        s.expires_at, s.last_seen_at
                 FROM _forgebase_auth_refresh_tokens r JOIN _forgebase_auth_sessions s ON s.id = r.session_id
                 WHERE r.token_hash = ?1",
                vec![t(hash.clone())],
            )
            .await?;
        let Some(row) = first(&r) else {
            drop(tx);
            self.audit(
                "refresh",
                false,
                ci,
                "anonymous",
                None,
                json!({"reason": "unknown_token"}),
            )
            .await?;
            return Err(AuthError::InvalidToken("invalid refresh token"));
        };
        let sid = row.str("session_id");
        let subject = session_subject(&row)?;
        if let Err(e) = self.ensure_active(&subject).await {
            tx.exec(
                "UPDATE _forgebase_auth_sessions SET revoked_at = ?2, revoked_reason = 'account_inactive'
                 WHERE id = ?1 AND revoked_at IS NULL",
                vec![t(sid.clone()), i(now)],
            )
            .await?;
            tx.commit().await?;
            return Err(e);
        }
        let reuse = row.opt_i64("used_at").is_some();
        let claimed = if reuse {
            false
        } else {
            tx.exec(
                "UPDATE _forgebase_auth_refresh_tokens SET used_at = ?2 WHERE token_hash = ?1 AND used_at IS NULL",
                vec![t(hash), i(now)],
            )
            .await?
            .rows_affected
                == 1
        };
        if !claimed {
            tx.exec(
                "UPDATE _forgebase_auth_sessions SET revoked_at = ?2, revoked_reason = 'refresh_token_reuse'
                 WHERE id = ?1 AND revoked_at IS NULL",
                vec![t(sid.clone()), i(now)],
            )
            .await?;
            audit_in(
                &mut tx,
                "refresh.reuse_detected",
                false,
                ci,
                "anonymous",
                Some(&subject),
                json!({"session_id": sid, "generation": row.i64("generation")}),
            )
            .await?;
            tx.commit().await?;
            tracing::warn!(session_id = %sid, "refresh token reuse detected; session revoked");
            return Err(AuthError::InvalidToken("refresh token reuse detected; session revoked"));
        }
        let idle = cfg.session_idle_timeout.as_secs() as i64;
        if row.opt_i64("revoked_at").is_some() {
            return Err(AuthError::InvalidToken("session revoked"));
        }
        if row.i64("expires_at") <= now || row.i64("token_expires") <= now || row.i64("last_seen_at") + idle <= now {
            return Err(AuthError::InvalidToken("session expired"));
        }
        let next = crypto::random_token(32)?;
        tx.exec(
            "INSERT INTO _forgebase_auth_refresh_tokens (token_hash, session_id, generation, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            vec![
                t(kr.token_hash("refresh", &next)),
                t(sid.clone()),
                i(row.i64("generation") + 1),
                i(now),
                i(row.i64("expires_at")),
            ],
        )
        .await?;
        tx.exec(
            "UPDATE _forgebase_auth_sessions SET last_seen_at = ?2, ip = coalesce(?3, ip), user_agent = coalesce(?4, user_agent)
             WHERE id = ?1",
            vec![t(sid.clone()), i(now), ot(ci.ip.clone()), ot(ci.user_agent.clone())],
        )
        .await?;
        // Keep only the used chain needed for reuse detection; drop tokens of
        // generations far behind.
        tx.exec(
            "DELETE FROM _forgebase_auth_refresh_tokens WHERE session_id = ?1 AND generation < ?2",
            vec![t(sid.clone()), i(row.i64("generation") - 50)],
        )
        .await?;
        tx.commit().await?;

        if !self.subject_exists(&subject).await? {
            self.revoke_sessions_where_id(&sid, "account_deleted").await?;
            return Err(AuthError::InvalidToken("session revoked"));
        }
        let method = match row.str("auth_method").as_str() {
            "magic_link" => AuthMethod::MagicLink,
            "email_otp" => AuthMethod::EmailOtp,
            _ => AuthMethod::Password,
        };
        let mfa = row.i64("mfa") == 1;
        let (access, expires_in) = self.sign_access(&subject, &sid, method, mfa).await?;
        Ok(SessionTokens {
            token_type: "Bearer",
            access_token: Some(access),
            expires_in,
            refresh_token: Some(next),
            session_id: sid,
            csrf_token: None,
            cookie: None,
            session_expires_at: rfc3339(row.i64("expires_at")),
            subject,
        })
    }

    async fn revoke_sessions_where_id(&self, sid: &str, reason: &str) -> Result<(), AuthError> {
        let mut tx = Tx::begin(self.db()).await?;
        tx.exec(
            "UPDATE _forgebase_auth_sessions SET revoked_at = ?2, revoked_reason = ?3 WHERE id = ?1 AND revoked_at IS NULL",
            vec![t(sid), i(self.now()), t(reason)],
        )
        .await?;
        tx.commit().await
    }

    /// Revokes every active session of `subject` (password reset, account
    /// lock-down). Returns how many were revoked.
    pub(crate) async fn revoke_all_in(tx: &mut Tx, subject: &Subject, reason: &str) -> Result<u64, AuthError> {
        let (st, coll, id) = subject_cols(subject);
        Ok(tx
            .exec(
                "UPDATE _forgebase_auth_sessions SET revoked_at = ?4, revoked_reason = ?5
                 WHERE subject_type = ?1 AND collection IS ?2 AND subject_id = ?3 AND revoked_at IS NULL",
                vec![t(st), ot(coll), t(id), i(store::now()), t(reason)],
            )
            .await?
            .rows_affected)
    }

    /// Ends the caller's current session.
    pub async fn logout(&self, caller: &Authenticated, ci: &ClientInfo) -> Result<(), AuthError> {
        let sid = caller
            .session_id()
            .ok_or_else(|| AuthError::invalid("this credential has no session to end"))?;
        let mut tx = Tx::begin(self.db()).await?;
        tx.exec(
            "UPDATE _forgebase_auth_sessions SET revoked_at = ?2, revoked_reason = 'logout' WHERE id = ?1 AND revoked_at IS NULL",
            vec![t(sid), i(self.now())],
        )
        .await?;
        audit_in(
            &mut tx,
            if caller.impersonator.is_some() {
                "impersonation.end"
            } else {
                "logout"
            },
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({"session_id": sid}),
        )
        .await?;
        tx.commit().await
    }

    /// Active sessions (devices) of the caller.
    pub async fn list_sessions(&self, caller: &Authenticated) -> Result<Vec<SessionView>, AuthError> {
        self.sessions_of(&caller.subject, caller.session_id()).await
    }

    /// Active sessions of `subject`; `current` marks the requesting session.
    pub(crate) async fn sessions_of(
        &self,
        subject: &Subject,
        current: Option<&str>,
    ) -> Result<Vec<SessionView>, AuthError> {
        let (st, coll, id) = subject_cols(subject);
        let now = self.now();
        let idle = self.inner.cfg.session_idle_timeout.as_secs() as i64;
        let r = read(
            self.db(),
            "SELECT id, auth_method, mfa, cookie_hash IS NOT NULL AS cookie, ip, user_agent, device_name,
                    created_at, last_seen_at, expires_at, impersonator
             FROM _forgebase_auth_sessions
             WHERE subject_type = ?1 AND collection IS ?2 AND subject_id = ?3
               AND revoked_at IS NULL AND expires_at > ?4 AND last_seen_at > ?5
             ORDER BY last_seen_at DESC LIMIT 200",
            vec![t(st), ot(coll), t(id), i(now), i(now - idle)],
        )
        .await?;
        Ok(rows(&r)
            .map(|row| SessionView {
                current: current == Some(row.str("id").as_str()),
                id: row.str("id"),
                auth_method: row.str("auth_method"),
                mfa: row.i64("mfa") == 1,
                cookie: row.i64("cookie") == 1,
                ip: row.opt_str("ip"),
                user_agent: row.opt_str("user_agent"),
                device_name: row.opt_str("device_name"),
                created_at: rfc3339(row.i64("created_at")),
                last_seen_at: rfc3339(row.i64("last_seen_at")),
                expires_at: rfc3339(row.i64("expires_at")),
                impersonator: row.opt_str("impersonator"),
            })
            .collect())
    }

    /// Revokes one of the caller's sessions (administrators: any session).
    pub async fn revoke_session(
        &self,
        caller: &Authenticated,
        session_id: &str,
        ci: &ClientInfo,
    ) -> Result<(), AuthError> {
        reject_impersonated(caller)?;
        let (st, coll, id) = subject_cols(&caller.subject);
        let admin = caller.subject.is_admin();
        let mut tx = Tx::begin(self.db()).await?;
        let n = tx
            .exec(
                "UPDATE _forgebase_auth_sessions SET revoked_at = ?5, revoked_reason = 'revoked'
                 WHERE id = ?1 AND revoked_at IS NULL
                   AND (?6 OR (subject_type = ?2 AND collection IS ?3 AND subject_id = ?4))",
                vec![
                    t(session_id),
                    t(st),
                    ot(coll),
                    t(id),
                    i(self.now()),
                    i(i64::from(admin)),
                ],
            )
            .await?
            .rows_affected;
        if n == 0 {
            return Err(AuthError::NotFound("session not found".into()));
        }
        audit_in(
            &mut tx,
            "session.revoked",
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({"session_id": session_id}),
        )
        .await?;
        tx.commit().await
    }

    /// Whether the account behind `subject` still exists (deleted accounts
    /// lose their sessions and keys).
    pub(crate) async fn subject_exists(&self, subject: &Subject) -> Result<bool, AuthError> {
        let r = match subject {
            Subject::User { collection, id } => {
                let coll = match self
                    .inner
                    .data
                    .get_collection(&AccessContext::system(), collection)
                    .await
                {
                    Ok(c) if c.kind == CollectionType::Auth => c,
                    Ok(_) | Err(DataError::CollectionNotFound(_)) => return Ok(false),
                    Err(e) => return Err(e.into()),
                };
                let q = self.db().dialect().quote_ident(&coll.name);
                read(
                    self.db(),
                    &format!("SELECT 1 FROM {q} WHERE id = ?1"),
                    vec![t(id.clone())],
                )
                .await?
            }
            Subject::Admin { id } => {
                read(
                    self.db(),
                    "SELECT 1 FROM _forgebase_admins WHERE id = ?1",
                    vec![t(id.clone())],
                )
                .await?
            }
            Subject::ServiceAccount { id, .. } => {
                read(
                    self.db(),
                    "SELECT 1 FROM _forgebase_service_accounts WHERE id = ?1 AND disabled_at IS NULL",
                    vec![t(id.clone())],
                )
                .await?
            }
        };
        Ok(!r.is_empty())
    }

    /// Minimum password length of an auth collection's `password` field.
    pub(crate) fn password_min(coll: &Collection) -> usize {
        match coll.field("password").map(|f| &f.kind) {
            Some(FieldKind::Password { min_length }) => (*min_length as usize).max(8),
            _ => 8,
        }
    }

    /// Mailer.
    /// The mailer (also used for organization invitations, FB-21).
    pub fn mailer(&self) -> &Arc<dyn Mailer> {
        &self.inner.mailer
    }
}

/// `(subject_type, collection, id)` columns of a subject.
pub(crate) fn subject_cols(s: &Subject) -> (&'static str, Option<String>, String) {
    match s {
        Subject::User { collection, id } => ("user", Some(collection.clone()), id.clone()),
        Subject::Admin { id } => ("admin", None, id.clone()),
        Subject::ServiceAccount { id, .. } => ("service_account", None, id.clone()),
    }
}

fn session_subject(row: &store::Row<'_>) -> Result<Subject, AuthError> {
    match (row.str("subject_type").as_str(), row.opt_str("collection")) {
        ("user", Some(collection)) => Ok(Subject::User {
            collection,
            id: row.str("subject_id"),
        }),
        ("admin", _) => Ok(Subject::Admin {
            id: row.str("subject_id"),
        }),
        _ => Err(AuthError::Internal("corrupt session row".into())),
    }
}

/// Refuses self-service credential changes inside an impersonation session
/// (no escalation: an impersonating administrator cannot leave the target
/// with new passwords, factors or keys; ADR-0014 §4).
pub(crate) fn reject_impersonated(caller: &Authenticated) -> Result<(), AuthError> {
    if caller.impersonator.is_some() {
        Err(AuthError::Forbidden(
            "impersonated_session: this action is not available while impersonating".into(),
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn require_admin(caller: &Authenticated) -> Result<(), AuthError> {
    if caller.subject.is_admin() {
        Ok(())
    } else {
        Err(AuthError::Forbidden("administrator access required".into()))
    }
}

/// Keys that must never reach the audit log, at any depth.
const SECRET_KEYS: &[&str] = &[
    "password",
    "new_password",
    "current_password",
    "token",
    "refresh_token",
    "access_token",
    "code",
    "secret",
    "otp",
    "mfa_token",
    "cookie",
];

fn scrub(v: &mut Json) {
    match v {
        Json::Object(m) => {
            m.retain(|k, _| !SECRET_KEYS.contains(&k.to_ascii_lowercase().as_str()));
            m.values_mut().for_each(scrub);
        }
        Json::Array(a) => a.iter_mut().for_each(scrub),
        _ => {}
    }
}

/// Appends an audit entry inside `tx`.
pub(crate) async fn audit_in(
    tx: &mut Tx,
    event: &str,
    success: bool,
    ci: &ClientInfo,
    actor: &str,
    subject: Option<&Subject>,
    mut detail: Json,
) -> Result<(), AuthError> {
    scrub(&mut detail);
    let (st, coll, sid) = match subject {
        Some(s) => {
            let (a, b, c) = subject_cols(s);
            (Some(a), b, Some(c))
        }
        None => (None, None, None),
    };
    let detail = if detail.as_object().is_some_and(Map::is_empty) || detail.is_null() {
        Value::Null
    } else {
        t(detail.to_string())
    };
    tx.exec(
        "INSERT INTO _forgebase_auth_audit (at, event, outcome, actor, subject_type, collection, subject_id, ip, user_agent, request_id, detail, impersonator, realm)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                 COALESCE((SELECT realm_id FROM _forgebase_realm_identity WHERE collection = ?6),
                          CASE WHEN ?6 IS NULL THEN ?13 ELSE 'default' END))",
        vec![
            i(store::now()),
            t(event),
            t(if success { "success" } else { "failure" }),
            t(actor),
            ot(st),
            ot(coll),
            ot(sid),
            ot(ci.ip.clone()),
            ot(ci.user_agent.clone()),
            ot(ci.request_id.clone()),
            detail,
            ot(ci.impersonator.clone()),
            ot(ci.realm.clone()),
        ],
    )
    .await?;
    Ok(())
}

fn audit_entry(row: store::Row<'_>) -> AuditEntry {
    AuditEntry {
        id: row.i64("id"),
        at: rfc3339(row.i64("at")),
        event: row.str("event"),
        outcome: row.str("outcome"),
        actor: row.str("actor"),
        subject_type: row.opt_str("subject_type"),
        collection: row.opt_str("collection"),
        subject_id: row.opt_str("subject_id"),
        ip: row.opt_str("ip"),
        user_agent: row.opt_str("user_agent"),
        request_id: row.opt_str("request_id"),
        detail: row
            .opt_str("detail")
            .and_then(|d| serde_json::from_str(&d).ok())
            .unwrap_or(Json::Null),
        impersonator: row.opt_str("impersonator"),
    }
}

/// Whether a data-core error is a unique violation (duplicate email/name).
pub(crate) fn is_unique_violation(e: &AuthError) -> bool {
    match e {
        AuthError::Db(DbError::Constraint {
            kind: ConstraintKind::Unique,
            ..
        }) => true,
        AuthError::Data(DataError::Db(DbError::Constraint {
            kind: ConstraintKind::Unique,
            ..
        })) => true,
        AuthError::Data(DataError::AlreadyExists(_)) => true,
        AuthError::Data(DataError::Validation(v)) => format!("{v:?}").contains("unique"),
        _ => false,
    }
}
