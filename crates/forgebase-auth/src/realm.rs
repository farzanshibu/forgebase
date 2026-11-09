//! Realm-scoped identity (FB-25, ADR-0015 §3).
//!
//! A realm is a separate user pool. Each realm names one auth collection
//! (`_forgebase_realm_identity`, unique across realms); every auth row keyed
//! by collection (sessions, one-time tokens, MFA, keys, provider identities,
//! passkeys, phones) therefore belongs to exactly one realm, and the same
//! email can be registered once per realm. Auth collections no realm names
//! belong to realm `default`.
//!
//! Requests select a realm with the `X-ForgeBase-Realm` header (also read by
//! the policy tenancy layer); sign-in flows then use the realm's collection,
//! and a `collection` of another realm is "not found". An authenticated
//! request acts in its identity's home realm unless it selects one, and the
//! policy engine refuses cross-realm requests (ADR-0008 §3), reading the
//! home realm from the shared [`RealmDirectory`].
//!
//! The realm configuration holds the enabled sign-in methods, signup, phone
//! login, LDAP and Turnstile settings. Secrets in it are write-only: the API
//! shows `"********"`, and they are sealed under the master key at rest.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use forgebase_data::{AccessContext, DEFAULT_REALM, Principal};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value as Json, json};

use crate::service::{ClientInfo, is_unique_violation, require_admin};
use crate::store::{Tx, first, i, now, read, rfc3339, rows, t};
use crate::{AuthError, AuthService, Authenticated, Subject};

/// Header selecting the realm of a request.
pub const REALM_HEADER: &str = "x-forgebase-realm";

/// Placeholder shown for a secret that is set.
pub const SECRET_MASK: &str = "********";

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// A write-only secret in realm configuration.
///
/// Input: a string sets it, `""` clears it, `"********"`, `null` or an absent
/// field keep the stored value. Output: `"********"` when set, else `null`.
#[derive(Clone, Default, PartialEq, Eq)]
pub enum ConfigSecret {
    /// Keep the stored value (input only).
    #[default]
    Keep,
    /// No secret.
    Unset,
    /// A secret.
    Set(String),
}

impl ConfigSecret {
    /// The secret, if set.
    pub fn get(&self) -> Option<&str> {
        match self {
            ConfigSecret::Set(s) => Some(s),
            _ => None,
        }
    }

    fn resolve(&mut self, old: &ConfigSecret) {
        if *self == ConfigSecret::Keep {
            *self = match old {
                ConfigSecret::Set(s) => ConfigSecret::Set(s.clone()),
                _ => ConfigSecret::Unset,
            };
        }
    }
}

impl std::fmt::Debug for ConfigSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ConfigSecret::Set(_) => "[redacted]",
            ConfigSecret::Unset => "unset",
            ConfigSecret::Keep => "keep",
        })
    }
}

impl Serialize for ConfigSecret {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            ConfigSecret::Set(_) => s.serialize_str(SECRET_MASK),
            _ => s.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for ConfigSecret {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(match Option::<String>::deserialize(d)? {
            None => ConfigSecret::Keep,
            Some(s) if s == SECRET_MASK => ConfigSecret::Keep,
            Some(s) if s.is_empty() => ConfigSecret::Unset,
            Some(s) => ConfigSecret::Set(s),
        })
    }
}

/// Sign-in methods of a realm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoginMethods {
    /// Email + password (and signup with a password).
    pub password: bool,
    /// Magic links by mail.
    pub magic_link: bool,
    /// Email one-time codes.
    pub email_otp: bool,
    /// Social login (OAuth 2 / OpenID Connect providers of the realm).
    pub oauth: bool,
    /// Passkeys (WebAuthn).
    pub passkey: bool,
    /// Phone number + SMS code.
    pub phone: bool,
    /// LDAP directory bind.
    pub ldap: bool,
}

impl Default for LoginMethods {
    fn default() -> Self {
        Self {
            password: true,
            magic_link: true,
            email_otp: true,
            oauth: true,
            passkey: true,
            phone: false,
            ldap: false,
        }
    }
}

/// A sign-in method, for [`RealmIdentity::require`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Email + password.
    Password,
    /// Magic link.
    MagicLink,
    /// Email OTP.
    EmailOtp,
    /// OAuth / OIDC.
    Oauth,
    /// Passkey.
    Passkey,
    /// Phone + SMS.
    Phone,
    /// LDAP.
    Ldap,
}

impl Method {
    fn name(self) -> &'static str {
        match self {
            Method::Password => "password",
            Method::MagicLink => "magic_link",
            Method::EmailOtp => "email_otp",
            Method::Oauth => "oauth",
            Method::Passkey => "passkey",
            Method::Phone => "phone",
            Method::Ldap => "ldap",
        }
    }
}

/// Phone sign-in settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PhoneSettings {
    /// A verified code for an unknown number creates an account (when the
    /// realm allows signup).
    pub signup: bool,
}

impl Default for PhoneSettings {
    fn default() -> Self {
        Self { signup: true }
    }
}

/// Identity configuration of a realm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RealmConfig {
    /// New accounts may be created (password signup, first social/LDAP/phone
    /// sign-in).
    pub signup: bool,
    /// Enabled sign-in methods.
    pub methods: LoginMethods,
    /// Overrides the server-wide `require_verified_email` for password login.
    pub require_verified_email: Option<bool>,
    /// Phone sign-in.
    pub phone: PhoneSettings,
    /// LDAP directory (needed when `methods.ldap`).
    pub ldap: Option<crate::ldap::LdapSettings>,
    /// Cloudflare Turnstile (off unless present and enabled).
    pub turnstile: Option<crate::turnstile::TurnstileSettings>,
}

impl Default for RealmConfig {
    fn default() -> Self {
        Self {
            signup: true,
            methods: LoginMethods::default(),
            require_verified_email: None,
            phone: PhoneSettings::default(),
            ldap: None,
            turnstile: None,
        }
    }
}

impl RealmConfig {
    fn secrets_mut(&mut self) -> Vec<(&'static str, &mut ConfigSecret)> {
        let mut v = Vec::new();
        if let Some(l) = &mut self.ldap {
            v.push(("ldap.bind_password", &mut l.bind_password));
        }
        if let Some(t) = &mut self.turnstile {
            v.push(("turnstile.secret", &mut t.secret));
        }
        v
    }

    fn validate(&self) -> Result<(), AuthError> {
        if self.methods.ldap && self.ldap.is_none() {
            return Err(AuthError::invalid("methods.ldap needs an ldap configuration"));
        }
        if let Some(l) = &self.ldap {
            l.validate()?;
        }
        if let Some(t) = &self.turnstile {
            t.validate()?;
        }
        Ok(())
    }
}

/// A realm's identity: its user collection and configuration.
#[derive(Debug, Clone, Serialize)]
pub struct RealmIdentity {
    /// Realm id.
    pub realm: String,
    /// Auth collection holding the realm's users.
    pub collection: String,
    /// Configuration (secrets masked when serialised).
    pub config: RealmConfig,
    /// Created (RFC 3339).
    pub created_at: String,
    /// Last change (RFC 3339).
    pub updated_at: String,
}

impl RealmIdentity {
    /// Refuses a disabled sign-in method.
    pub fn require(&self, m: Method) -> Result<(), AuthError> {
        let on = match m {
            Method::Password => self.config.methods.password,
            Method::MagicLink => self.config.methods.magic_link,
            Method::EmailOtp => self.config.methods.email_otp,
            Method::Oauth => self.config.methods.oauth,
            Method::Passkey => self.config.methods.passkey,
            Method::Phone => self.config.methods.phone,
            Method::Ldap => self.config.methods.ldap,
        };
        if on {
            Ok(())
        } else {
            Err(AuthError::Forbidden(format!(
                "method_disabled: {} sign-in is not enabled in realm {}",
                m.name(),
                self.realm
            )))
        }
    }

    /// Refuses account creation when the realm disallows signup.
    pub fn require_signup(&self) -> Result<(), AuthError> {
        if self.config.signup {
            Ok(())
        } else {
            Err(AuthError::Forbidden(format!(
                "signup_disabled: realm {} does not accept new accounts",
                self.realm
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// Directory
// ---------------------------------------------------------------------------

/// Which realm each identity belongs to; shared with the policy engine.
/// Cheap to clone (one shared map).
#[derive(Debug, Clone, Default)]
pub struct RealmDirectory {
    inner: Arc<RwLock<Dir>>,
}

#[derive(Debug, Default)]
struct Dir {
    realms: HashMap<String, Arc<RealmIdentity>>,
    by_collection: HashMap<String, String>,
    service_accounts: HashMap<String, String>,
}

impl RealmDirectory {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, Dir> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Realm of the users of an auth collection (`default` when no realm
    /// names it).
    pub fn realm_of_collection(&self, collection: &str) -> String {
        self.read()
            .by_collection
            .get(collection)
            .cloned()
            .unwrap_or_else(|| DEFAULT_REALM.to_owned())
    }

    /// Realm of a service account.
    pub fn realm_of_service_account(&self, id: &str) -> String {
        self.read()
            .service_accounts
            .get(id)
            .cloned()
            .unwrap_or_else(|| DEFAULT_REALM.to_owned())
    }

    /// Home realm of a principal: users and service accounts belong to one
    /// realm; the system, administrators and anonymous callers to none.
    pub fn home_realm(&self, p: &Principal) -> Option<String> {
        match p {
            Principal::User { collection, .. } => Some(self.realm_of_collection(collection)),
            Principal::Service { id, .. } => Some(self.realm_of_service_account(id)),
            _ => None,
        }
    }

    /// Home realm of an auth subject.
    pub fn realm_of_subject(&self, s: &Subject) -> Option<String> {
        match s {
            Subject::User { collection, .. } => Some(self.realm_of_collection(collection)),
            Subject::ServiceAccount { id, .. } => Some(self.realm_of_service_account(id)),
            _ => None,
        }
    }

    /// A realm's identity, if configured.
    pub fn get(&self, realm: &str) -> Option<Arc<RealmIdentity>> {
        self.read().realms.get(realm).cloned()
    }

    /// Every configured realm, by id.
    pub fn list(&self) -> Vec<Arc<RealmIdentity>> {
        let mut v: Vec<_> = self.read().realms.values().cloned().collect();
        v.sort_by(|a, b| a.realm.cmp(&b.realm));
        v
    }

    fn replace(&self, realms: Vec<RealmIdentity>, service_accounts: HashMap<String, String>) {
        let mut d = self.inner.write().unwrap_or_else(|e| e.into_inner());
        d.by_collection = realms
            .iter()
            .map(|r| (r.collection.clone(), r.realm.clone()))
            .collect();
        d.realms = realms.into_iter().map(|r| (r.realm.clone(), Arc::new(r))).collect();
        d.service_accounts = service_accounts;
    }

    pub(crate) fn set_service_account(&self, id: &str, realm: &str) {
        let mut d = self.inner.write().unwrap_or_else(|e| e.into_inner());
        if realm == DEFAULT_REALM {
            d.service_accounts.remove(id);
        } else {
            d.service_accounts.insert(id.to_owned(), realm.to_owned());
        }
    }
}

/// Checks a realm id from a header or path (ids are slugs, ADR-0008).
pub(crate) fn check_realm_id(r: &str) -> Result<(), AuthError> {
    let ok = !r.is_empty()
        && r.len() <= 64
        && r.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(AuthError::invalid("invalid realm id"))
    }
}

fn aad(realm: &str, path: &str) -> Vec<u8> {
    format!("realm-config:{realm}:{path}").into_bytes()
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// `PUT /api/auth/realms/{realm}` body.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RealmInput {
    /// Auth collection of the realm's users (default `<realm>_users`;
    /// created if missing; fixed once set).
    #[serde(default)]
    pub collection: Option<String>,
    /// Configuration (replaces the stored one; secrets follow
    /// [`ConfigSecret`] rules).
    #[serde(default)]
    pub config: RealmConfig,
}

impl AuthService {
    /// Reloads the directory from the database.
    pub async fn reload_realms(&self) -> Result<(), AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT realm_id, collection, config, created_at, updated_at FROM _forgebase_realm_identity",
            vec![],
        )
        .await?;
        let mut realms = Vec::new();
        for row in rows(&r) {
            let realm = row.str("realm_id");
            let mut raw: Json = serde_json::from_str(&row.str("config")).unwrap_or_else(|_| json!({}));
            let sealed = self.take_sealed(&realm, &mut raw)?;
            let mut config: RealmConfig = serde_json::from_value(raw)
                .map_err(|e| AuthError::Internal(format!("realm {realm} configuration: {e}")))?;
            for (path, slot) in config.secrets_mut() {
                *slot = match sealed.get(path) {
                    Some(s) => ConfigSecret::Set(s.clone()),
                    None => ConfigSecret::Unset,
                };
            }
            realms.push(RealmIdentity {
                realm,
                collection: row.str("collection"),
                config,
                created_at: rfc3339(row.i64("created_at")),
                updated_at: rfc3339(row.i64("updated_at")),
            });
        }
        let r = read(
            self.data().backend(),
            "SELECT id, realm FROM _forgebase_service_accounts WHERE realm <> 'default'",
            vec![],
        )
        .await?;
        let sas = rows(&r).map(|row| (row.str("id"), row.str("realm"))).collect();
        self.realms().replace(realms, sas);
        Ok(())
    }

    /// Unseals `"sealed:<b64>"` strings at the secret paths of a stored
    /// configuration, removing them from `raw`.
    fn take_sealed(&self, realm: &str, raw: &mut Json) -> Result<HashMap<&'static str, String>, AuthError> {
        let mut out = HashMap::new();
        for path in ["ldap.bind_password", "turnstile.secret"] {
            let (a, b) = path.split_once('.').unwrap_or((path, ""));
            let Some(slot) = raw.get_mut(a).and_then(|o| o.get_mut(b)) else {
                continue;
            };
            if let Some(s) = slot.as_str().and_then(|s| s.strip_prefix("sealed:")) {
                let blob = URL_SAFE_NO_PAD
                    .decode(s)
                    .map_err(|_| AuthError::Internal("bad sealed secret".into()))?;
                let plain = self.keyring().open(&blob, &aad(realm, path))?;
                out.insert(
                    path,
                    String::from_utf8(plain).map_err(|_| AuthError::Internal("bad sealed secret".into()))?,
                );
            }
            *slot = Json::Null;
        }
        Ok(out)
    }

    fn stored_config(&self, realm: &str, cfg: &RealmConfig) -> Result<String, AuthError> {
        let mut raw = serde_json::to_value(cfg).map_err(|e| AuthError::Internal(e.to_string()))?;
        let mut cfg = cfg.clone();
        for (path, secret) in cfg.secrets_mut() {
            let (a, b) = path.split_once('.').unwrap_or((path, ""));
            let Some(slot) = raw.get_mut(a).and_then(|o| o.get_mut(b)) else {
                continue;
            };
            *slot = match secret.get() {
                Some(s) => {
                    let sealed = self.keyring().seal(s.as_bytes(), &aad(realm, path))?;
                    Json::String(format!("sealed:{}", URL_SAFE_NO_PAD.encode(sealed)))
                }
                None => Json::Null,
            };
        }
        Ok(raw.to_string())
    }

    /// Points the default realm at the configured default auth collection.
    pub(crate) async fn reconcile_default_realm(&self) -> Result<(), AuthError> {
        if let Some(c) = self.config().default_collection.clone() {
            let mut tx = Tx::begin(self.data().backend()).await?;
            let res = tx
                .exec(
                    "UPDATE _forgebase_realm_identity SET collection = ?1, updated_at = ?2
                     WHERE realm_id = 'default' AND collection <> ?1",
                    vec![t(c.clone()), i(now())],
                )
                .await;
            match res {
                Ok(_) => tx.commit().await?,
                Err(e) if is_unique_violation(&e) => {
                    return Err(AuthError::Internal(format!(
                        "default auth collection {c:?} belongs to another realm"
                    )));
                }
                Err(e) => return Err(e),
            }
        }
        self.reload_realms().await
    }

    /// The realm a request selects (`X-ForgeBase-Realm`, else `default`) and
    /// its identity.
    pub(crate) fn request_realm(&self, ci: &ClientInfo) -> Result<Arc<RealmIdentity>, AuthError> {
        let realm = ci.realm.as_deref().unwrap_or(DEFAULT_REALM);
        self.realms()
            .get(realm)
            .ok_or_else(|| AuthError::NotFound(format!("realm {realm:?} has no identity configuration")))
    }

    /// The identity of a realm by id.
    pub(crate) fn realm_identity(&self, realm: &str) -> Result<Arc<RealmIdentity>, AuthError> {
        self.realms()
            .get(realm)
            .ok_or_else(|| AuthError::NotFound(format!("realm {realm:?} has no identity configuration")))
    }

    /// The identity of the realm a collection's users belong to.
    pub(crate) fn realm_of(&self, collection: &str) -> Result<Arc<RealmIdentity>, AuthError> {
        self.realm_identity(&self.realms().realm_of_collection(collection))
    }

    /// The auth collection for a sign-in request: `name` (must belong to the
    /// selected realm) or the realm's own collection.
    pub(crate) async fn request_collection(
        &self,
        ci: &ClientInfo,
        name: Option<&str>,
    ) -> Result<(Arc<RealmIdentity>, forgebase_data::schema::Collection), AuthError> {
        let ident = self.request_realm(ci)?;
        let name = match name.filter(|n| !n.is_empty()) {
            Some(n) => n.to_owned(),
            None if ident.realm == DEFAULT_REALM && self.config().default_collection.is_none() => {
                return Err(AuthError::invalid("collection is required"));
            }
            None => ident.collection.clone(),
        };
        if self.realms().realm_of_collection(&name) != ident.realm {
            return Err(AuthError::NotFound(format!("auth collection {name:?} not found")));
        }
        let coll = self.auth_collection(Some(&name)).await?;
        Ok((ident, coll))
    }

    /// Public sign-in options of the selected realm (for login pages).
    pub fn realm_info(&self, ci: &ClientInfo) -> Result<Json, AuthError> {
        let ident = self.request_realm(ci)?;
        let c = &ident.config;
        let turnstile = c.turnstile.as_ref().filter(|t| t.enabled).map(|t| {
            json!({"site_key": t.site_key, "actions": t.actions})
        });
        Ok(json!({
            "realm": ident.realm,
            "collection": ident.collection,
            "signup": c.signup,
            "methods": c.methods,
            "phone": {"signup": c.phone.signup && c.signup},
            "turnstile": turnstile,
            "oauth_providers": if c.methods.oauth { serde_json::to_value(self.providers().list(&ident.realm)).unwrap_or(Json::Null) } else { json!([]) },
        }))
    }

    /// Every realm identity (administrators).
    pub fn list_realm_identities(&self, caller: &Authenticated) -> Result<Vec<Arc<RealmIdentity>>, AuthError> {
        require_admin(caller)?;
        Ok(self.realms().list())
    }

    /// One realm identity (administrators).
    pub fn get_realm_identity(&self, caller: &Authenticated, realm: &str) -> Result<Arc<RealmIdentity>, AuthError> {
        require_admin(caller)?;
        self.realm_identity(realm)
    }

    /// Creates or replaces a realm's identity configuration (administrators).
    /// The realm must exist (`POST /api/v1/realms`); its collection is
    /// created when missing and cannot change once set.
    pub async fn put_realm_identity(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        realm: &str,
        mut input: RealmInput,
    ) -> Result<Arc<RealmIdentity>, AuthError> {
        require_admin(caller)?;
        check_realm_id(realm)?;
        let exists = read(
            self.data().backend(),
            "SELECT 1 AS x FROM _forgebase_realms WHERE id = ?1",
            vec![t(realm)],
        )
        .await?;
        if first(&exists).is_none() {
            return Err(AuthError::NotFound(format!(
                "realm {realm:?} not found (create it with POST /api/v1/realms)"
            )));
        }
        let old = self.realms().get(realm);
        let collection = match (&old, input.collection.as_deref().filter(|c| !c.is_empty())) {
            (Some(o), Some(c)) if c != o.collection => {
                return Err(AuthError::Conflict(format!(
                    "realm {realm} already uses collection {}; the user pool of a realm cannot move",
                    o.collection
                )));
            }
            (Some(o), _) => o.collection.clone(),
            (None, Some(c)) => c.to_owned(),
            (None, None) => format!("{realm}_users").replace('-', "_"),
        };
        if old.is_none() {
            if self.realms().list().iter().any(|r| r.collection == collection) {
                return Err(AuthError::Conflict(format!(
                    "collection {collection} belongs to another realm"
                )));
            }
            // An existing collection must be an empty auth collection: its
            // users would otherwise move out of the default realm.
            if let Ok(c) = self.auth_collection(Some(&collection)).await {
                let q = self.data().backend().dialect().quote_ident(&c.name);
                let r = read(self.data().backend(), &format!("SELECT 1 AS x FROM {q} LIMIT 1"), vec![]).await?;
                if first(&r).is_some() {
                    return Err(AuthError::Conflict(format!(
                        "collection {collection} already holds users of realm default"
                    )));
                }
            }
        }
        {
            let empty = RealmConfig::default();
            let old_cfg = old.as_ref().map_or(&empty, |o| &o.config);
            let mut old_cfg = old_cfg.clone();
            let olds: HashMap<&str, ConfigSecret> =
                old_cfg.secrets_mut().into_iter().map(|(p, s)| (p, s.clone())).collect();
            for (path, s) in input.config.secrets_mut() {
                s.resolve(olds.get(path).unwrap_or(&ConfigSecret::Unset));
            }
        }
        input.config.validate()?;
        self.ensure_auth_collection(&collection).await?;
        let stored = self.stored_config(realm, &input.config)?;
        let ts = now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        let res = tx
            .exec(
                "INSERT INTO _forgebase_realm_identity (realm_id, collection, config, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT (realm_id) DO UPDATE SET config = excluded.config, updated_at = excluded.updated_at",
                vec![t(realm), t(collection.clone()), t(stored), i(ts)],
            )
            .await;
        if let Err(e) = res {
            if is_unique_violation(&e) {
                return Err(AuthError::Conflict(format!(
                    "collection {collection} belongs to another realm"
                )));
            }
            return Err(e);
        }
        crate::service::audit_in(
            &mut tx,
            "realm.identity_updated",
            true,
            ci,
            &caller.subject.actor(),
            None,
            json!({"realm": realm, "collection": collection, "created": old.is_none()}),
        )
        .await?;
        tx.commit().await?;
        self.reload_realms().await?;
        self.realm_identity(realm)
    }

    /// Home realm of an authenticated caller (`default` for administrators).
    pub fn home_realm_of(&self, a: &Authenticated) -> String {
        self.realms()
            .realm_of_subject(&a.subject)
            .unwrap_or_else(|| DEFAULT_REALM.to_owned())
    }

    /// The data core context of an authenticated caller, in its home realm.
    pub fn access_context_for(&self, a: &Authenticated) -> AccessContext {
        let ctx = a.access_context(None);
        match self.realms().realm_of_subject(&a.subject) {
            Some(r) => ctx.with_realm(r),
            None => ctx,
        }
    }
}
