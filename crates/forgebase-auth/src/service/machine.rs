//! Administrators, service accounts, API keys and personal access tokens.
//!
//! Key format: `fbk_<id>_<secret>` (API key of a service account) or
//! `fbp_<id>_<secret>` (personal access token), `id` = 16 `[a-z0-9]`,
//! `secret` = 32 random bytes base64url. Only HMAC(pepper, secret) is
//! stored; the full key is returned once, at creation.

use forgebase_data::db::Value;
use serde::Serialize;
use serde_json::{Value as Json, json};

use super::{
    AuthService, ClientInfo, audit_in, flows::normalize_email, is_unique_violation, require_admin, subject_cols,
};
use crate::crypto;
use crate::password::check_password_policy;
use crate::scopes::{Scope, parse_scopes};
use crate::store::{Tx, first, i, now, ot, read, rfc3339, rows, t};
use crate::{AuthError, Authenticated, CredentialKind, Subject};

const API_KEY_PREFIX: &str = "fbk_";
const PAT_PREFIX: &str = "fbp_";
const KEY_ID_LEN: usize = 16;
/// Administrator passwords need at least this many characters.
pub const ADMIN_PASSWORD_MIN: usize = 10;

/// An administrator account.
#[derive(Debug, Clone, Serialize)]
pub struct AdminView {
    /// Id.
    pub id: String,
    /// Email.
    pub email: String,
    /// Created.
    pub created_at: String,
    /// Last sign-in.
    pub last_login_at: Option<String>,
}

/// A service account.
#[derive(Debug, Clone, Serialize)]
pub struct ServiceAccountView {
    /// Id.
    pub id: String,
    /// Unique name.
    pub name: String,
    /// Free text.
    pub description: String,
    /// `service` or `admin`.
    pub role: String,
    /// Who created it.
    pub created_by: String,
    /// Created.
    pub created_at: String,
    /// Disabled (keys stop working).
    pub disabled: bool,
}

/// An API key or PAT without its secret.
#[derive(Debug, Clone, Serialize)]
pub struct ApiKeyView {
    /// Id (also visible inside the key).
    pub id: String,
    /// `api_key` or `pat`.
    pub kind: String,
    /// Name.
    pub name: String,
    /// `service_account`, `user` or `admin`.
    pub owner_type: String,
    /// Owner's collection (users).
    pub collection: Option<String>,
    /// Owner id.
    pub owner_id: String,
    /// Scopes.
    pub scopes: Vec<String>,
    /// Created.
    pub created_at: String,
    /// Expiry, if any.
    pub expires_at: Option<String>,
    /// Last use (updated at most once a minute).
    pub last_used_at: Option<String>,
    /// Revoked.
    pub revoked: bool,
}

/// A newly created key: the only time `key` is returned.
#[derive(Debug, Clone, Serialize)]
pub struct CreatedKey {
    /// The full secret key; store it now, it cannot be shown again.
    pub key: String,
    /// Its metadata.
    #[serde(flatten)]
    pub info: ApiKeyView,
}

fn key_view(row: crate::store::Row<'_>) -> ApiKeyView {
    ApiKeyView {
        id: row.str("id"),
        kind: row.str("kind"),
        name: row.str("name"),
        owner_type: row.str("owner_type"),
        collection: row.opt_str("collection"),
        owner_id: row.str("owner_id"),
        scopes: serde_json::from_str(&row.str("scopes")).unwrap_or_default(),
        created_at: rfc3339(row.i64("created_at")),
        expires_at: row.opt_i64("expires_at").map(rfc3339),
        last_used_at: row.opt_i64("last_used_at").map(rfc3339),
        revoked: row.opt_i64("revoked_at").is_some(),
    }
}

const KEY_COLS: &str =
    "id, kind, name, owner_type, collection, owner_id, scopes, created_at, expires_at, last_used_at, revoked_at";

fn sa_view(row: crate::store::Row<'_>) -> ServiceAccountView {
    ServiceAccountView {
        id: row.str("id"),
        name: row.str("name"),
        description: row.str("description"),
        role: row.str("role"),
        created_by: row.str("created_by"),
        created_at: rfc3339(row.i64("created_at")),
        disabled: row.opt_i64("disabled_at").is_some(),
    }
}

impl AuthService {
    // -----------------------------------------------------------------------
    // Administrators
    // -----------------------------------------------------------------------

    /// Creates an administrator (CLI bootstrap `forgebase admin create`, or an
    /// existing administrator).
    pub async fn create_admin(
        &self,
        email: &str,
        password: &str,
        actor: &str,
        ci: &ClientInfo,
    ) -> Result<AdminView, AuthError> {
        let email = normalize_email(email)?;
        check_password_policy(password, ADMIN_PASSWORD_MIN)?;
        let hash = self.hasher().hash_async(password).await?;
        let id = crypto::random_id(15)?;
        let now = now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        let res = tx
            .exec(
                "INSERT INTO _forgebase_admins (id, email, password_hash, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)",
                vec![t(id.clone()), t(email.clone()), t(hash), i(now)],
            )
            .await;
        if let Err(e) = res {
            if is_unique_violation(&e) {
                return Err(AuthError::Conflict(format!(
                    "an administrator with email {email} already exists"
                )));
            }
            return Err(e);
        }
        let subject = Subject::Admin { id: id.clone() };
        audit_in(
            &mut tx,
            "admin.created",
            true,
            ci,
            actor,
            Some(&subject),
            json!({"email": email}),
        )
        .await?;
        tx.commit().await?;
        Ok(AdminView {
            id,
            email,
            created_at: rfc3339(now),
            last_login_at: None,
        })
    }

    /// Sets an administrator's password (CLI recovery). Revokes their sessions.
    pub async fn reset_admin_password(
        &self,
        email: &str,
        password: &str,
        actor: &str,
        ci: &ClientInfo,
    ) -> Result<(), AuthError> {
        let email = normalize_email(email)?;
        check_password_policy(password, ADMIN_PASSWORD_MIN)?;
        let hash = self.hasher().hash_async(password).await?;
        let mut tx = Tx::begin(self.data().backend()).await?;
        let r = tx
            .read(
                "UPDATE _forgebase_admins SET password_hash = ?2, updated_at = ?3 WHERE email = ?1 RETURNING id",
                vec![t(email.clone()), t(hash), i(now())],
            )
            .await?;
        let id = first(&r)
            .map(|row| row.str("id"))
            .ok_or_else(|| AuthError::NotFound(format!("no administrator with email {email}")))?;
        let subject = Subject::Admin { id };
        Self::revoke_all_in(&mut tx, &subject, "password_reset").await?;
        tx.exec(
            "DELETE FROM _forgebase_auth_lockouts WHERE key = ?1",
            vec![t(format!("pw:admin:{email}"))],
        )
        .await?;
        audit_in(
            &mut tx,
            "admin.password_reset",
            true,
            ci,
            actor,
            Some(&subject),
            json!({}),
        )
        .await?;
        tx.commit().await
    }

    /// Every administrator.
    pub async fn list_admins(&self) -> Result<Vec<AdminView>, AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT id, email, created_at, last_login_at FROM _forgebase_admins ORDER BY created_at, id",
            vec![],
        )
        .await?;
        Ok(rows(&r)
            .map(|row| AdminView {
                id: row.str("id"),
                email: row.str("email"),
                created_at: rfc3339(row.i64("created_at")),
                last_login_at: row.opt_i64("last_login_at").map(rfc3339),
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Service accounts
    // -----------------------------------------------------------------------

    /// Creates a service account (administrators).
    pub async fn create_service_account(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        name: &str,
        description: &str,
        role: &str,
    ) -> Result<ServiceAccountView, AuthError> {
        require_admin(caller)?;
        let name = name.trim();
        if name.is_empty() || name.len() > 100 || name.chars().any(char::is_control) {
            return Err(AuthError::invalid("name must be 1-100 printable characters"));
        }
        if !matches!(role, "service" | "admin") {
            return Err(AuthError::invalid("role must be 'service' or 'admin'"));
        }
        let id = crypto::random_id(15)?;
        let now = now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        let res = tx
            .exec(
                "INSERT INTO _forgebase_service_accounts (id, name, description, role, created_by, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                vec![
                    t(id.clone()),
                    t(name),
                    t(description.chars().take(1000).collect::<String>()),
                    t(role),
                    t(caller.subject.actor()),
                    i(now),
                ],
            )
            .await;
        if let Err(e) = res {
            if is_unique_violation(&e) {
                return Err(AuthError::Conflict(format!(
                    "a service account named {name:?} already exists"
                )));
            }
            return Err(e);
        }
        let subject = Subject::ServiceAccount {
            id: id.clone(),
            admin: role == "admin",
        };
        audit_in(
            &mut tx,
            "service_account.created",
            true,
            ci,
            &caller.subject.actor(),
            Some(&subject),
            json!({"name": name, "role": role}),
        )
        .await?;
        tx.commit().await?;
        self.get_service_account(caller, &id).await
    }

    /// One service account (administrators).
    pub async fn get_service_account(&self, caller: &Authenticated, id: &str) -> Result<ServiceAccountView, AuthError> {
        require_admin(caller)?;
        let r = read(
            self.data().backend(),
            "SELECT * FROM _forgebase_service_accounts WHERE id = ?1",
            vec![t(id)],
        )
        .await?;
        first(&r)
            .map(sa_view)
            .ok_or_else(|| AuthError::NotFound("service account not found".into()))
    }

    /// Every service account (administrators).
    pub async fn list_service_accounts(&self, caller: &Authenticated) -> Result<Vec<ServiceAccountView>, AuthError> {
        require_admin(caller)?;
        let r = read(
            self.data().backend(),
            "SELECT * FROM _forgebase_service_accounts ORDER BY created_at, id",
            vec![],
        )
        .await?;
        Ok(rows(&r).map(sa_view).collect())
    }

    /// Deletes a service account and revokes all its keys (administrators).
    pub async fn delete_service_account(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        id: &str,
    ) -> Result<(), AuthError> {
        require_admin(caller)?;
        if matches!(&caller.subject, Subject::ServiceAccount { id: me, .. } if me == id) {
            return Err(AuthError::Forbidden("a service account cannot delete itself".into()));
        }
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "DELETE FROM _forgebase_auth_api_keys WHERE owner_type = 'service_account' AND owner_id = ?1",
            vec![t(id)],
        )
        .await?;
        let n = tx
            .exec("DELETE FROM _forgebase_service_accounts WHERE id = ?1", vec![t(id)])
            .await?
            .rows_affected;
        if n == 0 {
            return Err(AuthError::NotFound("service account not found".into()));
        }
        let subject = Subject::ServiceAccount {
            id: id.to_owned(),
            admin: false,
        };
        audit_in(
            &mut tx,
            "service_account.deleted",
            true,
            ci,
            &caller.subject.actor(),
            Some(&subject),
            json!({}),
        )
        .await?;
        tx.commit().await
    }

    // -----------------------------------------------------------------------
    // API keys and PATs
    // -----------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    async fn insert_key(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        kind: &str,
        owner: &Subject,
        name: &str,
        scopes: &[String],
        expires_in_days: Option<u32>,
    ) -> Result<CreatedKey, AuthError> {
        let name = name.trim();
        if name.is_empty() || name.len() > 100 || name.chars().any(char::is_control) {
            return Err(AuthError::invalid("name must be 1-100 printable characters"));
        }
        let scopes = parse_scopes(scopes).map_err(AuthError::InvalidInput)?;
        // A scoped credential cannot mint a broader one.
        if let Some(held) = &caller.scopes
            && let Some(s) = scopes.iter().find(|s| !s.within(held))
        {
            return Err(AuthError::Forbidden(format!(
                "scope {:?} exceeds the scopes of the calling credential",
                s.as_str()
            )));
        }
        if expires_in_days.is_some_and(|d| d == 0 || d > 3650) {
            return Err(AuthError::invalid("expires_in_days must be 1-3650"));
        }
        let id = crypto::random_id(KEY_ID_LEN)?;
        let secret = crypto::random_token(32)?;
        let prefix = if kind == "api_key" { API_KEY_PREFIX } else { PAT_PREFIX };
        let key = format!("{prefix}{id}_{secret}");
        let now = now();
        let expires_at = expires_in_days.map(|d| now + i64::from(d) * 86_400);
        let scope_strs: Vec<String> = scopes.iter().map(|s| s.as_str().to_owned()).collect();
        let (owner_type, coll, owner_id) = match owner {
            Subject::ServiceAccount { id, .. } => ("service_account", None, id.clone()),
            other => subject_cols(other),
        };
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "INSERT INTO _forgebase_auth_api_keys (id, kind, name, secret_hash, owner_type, collection, owner_id, scopes, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            vec![
                t(id.clone()),
                t(kind),
                t(name),
                t(self.keyring().token_hash("api_key", &secret)),
                t(owner_type),
                ot(coll.clone()),
                t(owner_id.clone()),
                t(serde_json::to_string(&scope_strs).map_err(|e| AuthError::Internal(e.to_string()))?),
                i(now),
                expires_at.map_or(Value::Null, i),
            ],
        )
        .await?;
        audit_in(
            &mut tx,
            if kind == "api_key" {
                "api_key.created"
            } else {
                "pat.created"
            },
            true,
            ci,
            &caller.subject.actor(),
            Some(owner),
            json!({"key_id": id, "name": name, "scopes": scope_strs}),
        )
        .await?;
        tx.commit().await?;
        Ok(CreatedKey {
            key,
            info: ApiKeyView {
                id,
                kind: kind.into(),
                name: name.into(),
                owner_type: owner_type.into(),
                collection: coll,
                owner_id,
                scopes: scope_strs,
                created_at: rfc3339(now),
                expires_at: expires_at.map(rfc3339),
                last_used_at: None,
                revoked: false,
            },
        })
    }

    /// Creates an API key for a service account (administrators).
    pub async fn create_api_key(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        service_account_id: &str,
        name: &str,
        scopes: &[String],
        expires_in_days: Option<u32>,
    ) -> Result<CreatedKey, AuthError> {
        require_admin(caller)?;
        let sa = self.get_service_account(caller, service_account_id).await?;
        if sa.disabled {
            return Err(AuthError::Conflict("service account is disabled".into()));
        }
        let owner = Subject::ServiceAccount {
            id: sa.id,
            admin: sa.role == "admin",
        };
        self.insert_key(caller, ci, "api_key", &owner, name, scopes, expires_in_days)
            .await
    }

    /// API keys, optionally of one service account (administrators).
    pub async fn list_api_keys(
        &self,
        caller: &Authenticated,
        service_account_id: Option<&str>,
    ) -> Result<Vec<ApiKeyView>, AuthError> {
        require_admin(caller)?;
        let r = read(
            self.data().backend(),
            &format!(
                "SELECT {KEY_COLS} FROM _forgebase_auth_api_keys WHERE kind = 'api_key' AND (?1 IS NULL OR owner_id = ?1)
                 ORDER BY created_at DESC, id"
            ),
            vec![ot(service_account_id.map(str::to_owned))],
        )
        .await?;
        Ok(rows(&r).map(key_view).collect())
    }

    /// Revokes an API key or PAT. Owners may revoke their own PATs;
    /// administrators any key.
    pub async fn revoke_key(&self, caller: &Authenticated, ci: &ClientInfo, key_id: &str) -> Result<(), AuthError> {
        let (st, coll, id) = subject_cols(&caller.subject);
        let admin = caller.subject.is_admin();
        let mut tx = Tx::begin(self.data().backend()).await?;
        let r = tx
            .read(
                "UPDATE _forgebase_auth_api_keys SET revoked_at = ?2
                 WHERE id = ?1 AND revoked_at IS NULL
                   AND (?6 OR (kind = 'pat' AND owner_type = ?3 AND collection IS ?4 AND owner_id = ?5))
                 RETURNING kind",
                vec![t(key_id), i(now()), t(st), ot(coll), t(id), i(i64::from(admin))],
            )
            .await?;
        let Some(row) = first(&r) else {
            return Err(AuthError::NotFound("key not found".into()));
        };
        let event = if row.str("kind") == "api_key" {
            "api_key.revoked"
        } else {
            "pat.revoked"
        };
        audit_in(
            &mut tx,
            event,
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({"key_id": key_id}),
        )
        .await?;
        tx.commit().await
    }

    /// Creates a personal access token for the caller (user or administrator).
    pub async fn create_pat(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        name: &str,
        scopes: &[String],
        expires_in_days: Option<u32>,
    ) -> Result<CreatedKey, AuthError> {
        if !matches!(caller.subject, Subject::User { .. } | Subject::Admin { .. }) {
            return Err(AuthError::Forbidden("service accounts use API keys".into()));
        }
        let owner = caller.subject.clone();
        self.insert_key(caller, ci, "pat", &owner, name, scopes, expires_in_days)
            .await
    }

    /// The caller's personal access tokens.
    pub async fn list_pats(&self, caller: &Authenticated) -> Result<Vec<ApiKeyView>, AuthError> {
        let (st, coll, id) = subject_cols(&caller.subject);
        let r = read(
            self.data().backend(),
            &format!(
                "SELECT {KEY_COLS} FROM _forgebase_auth_api_keys
                 WHERE kind = 'pat' AND owner_type = ?1 AND collection IS ?2 AND owner_id = ?3
                 ORDER BY created_at DESC, id"
            ),
            vec![t(st), ot(coll), t(id)],
        )
        .await?;
        Ok(rows(&r).map(key_view).collect())
    }

    /// Resolves an API key or PAT (`fbk_…` / `fbp_…`).
    pub async fn authenticate_key(&self, key: &str) -> Result<Authenticated, AuthError> {
        let bad = AuthError::InvalidToken("invalid API key");
        let (kind, rest) = if let Some(r) = key.strip_prefix(API_KEY_PREFIX) {
            ("api_key", r)
        } else if let Some(r) = key.strip_prefix(PAT_PREFIX) {
            ("pat", r)
        } else {
            return Err(bad);
        };
        if !rest.is_ascii() || rest.len() < KEY_ID_LEN + 2 || rest.len() > 128 || rest.as_bytes()[KEY_ID_LEN] != b'_' {
            return Err(bad);
        }
        let (id, secret) = (&rest[..KEY_ID_LEN], &rest[KEY_ID_LEN + 1..]);
        let r = read(
            self.data().backend(),
            "SELECT kind, secret_hash, owner_type, collection, owner_id, scopes, expires_at, last_used_at, revoked_at
             FROM _forgebase_auth_api_keys WHERE id = ?1",
            vec![t(id)],
        )
        .await?;
        let candidate = self.keyring().token_hash("api_key", secret);
        let Some(row) = first(&r) else {
            // Compare anyway so a miss costs the same as a wrong secret.
            let _ = crypto::ct_eq(candidate.as_bytes(), candidate.as_bytes());
            return Err(bad);
        };
        if !crypto::ct_eq(candidate.as_bytes(), row.str("secret_hash").as_bytes()) || row.str("kind") != kind {
            return Err(bad);
        }
        let now = now();
        if row.opt_i64("revoked_at").is_some() {
            return Err(AuthError::InvalidToken("API key revoked"));
        }
        if row.opt_i64("expires_at").is_some_and(|e| e <= now) {
            return Err(AuthError::InvalidToken("API key expired"));
        }
        let owner_id = row.str("owner_id");
        let subject = match (row.str("owner_type").as_str(), row.opt_str("collection")) {
            ("service_account", _) => {
                let sa = read(
                    self.data().backend(),
                    "SELECT role, disabled_at FROM _forgebase_service_accounts WHERE id = ?1",
                    vec![t(owner_id.clone())],
                )
                .await?;
                let sa = first(&sa).ok_or(AuthError::InvalidToken("API key owner no longer exists"))?;
                if sa.opt_i64("disabled_at").is_some() {
                    return Err(AuthError::InvalidToken("service account disabled"));
                }
                Subject::ServiceAccount {
                    id: owner_id,
                    admin: sa.str("role") == "admin",
                }
            }
            ("user", Some(collection)) => Subject::User {
                collection,
                id: owner_id,
            },
            ("admin", _) => Subject::Admin { id: owner_id },
            _ => return Err(AuthError::Internal("corrupt API key row".into())),
        };
        if !matches!(subject, Subject::ServiceAccount { .. }) && !self.subject_exists(&subject).await? {
            return Err(AuthError::InvalidToken("API key owner no longer exists"));
        }
        let scopes: Vec<Scope> = serde_json::from_str::<Vec<String>>(&row.str("scopes"))
            .ok()
            .and_then(|v| parse_scopes(&v).ok())
            .ok_or_else(|| AuthError::Internal("corrupt API key scopes".into()))?;
        if row.opt_i64("last_used_at").is_none_or(|t| now - t >= 60) {
            let svc = self.clone();
            let id = id.to_owned();
            tokio::spawn(async move {
                let res = async {
                    let mut tx = Tx::begin(svc.data().backend()).await?;
                    tx.exec(
                        "UPDATE _forgebase_auth_api_keys SET last_used_at = ?2 WHERE id = ?1",
                        vec![t(id), i(now)],
                    )
                    .await?;
                    tx.commit().await
                }
                .await;
                if let Err(e) = res {
                    tracing::debug!(error = %e, "could not record API key use");
                }
            });
        }
        Ok(Authenticated {
            subject,
            credential: if kind == "api_key" {
                CredentialKind::ApiKey
            } else {
                CredentialKind::PersonalAccessToken
            },
            credential_id: id.to_owned(),
            scopes: Some(scopes),
            mfa: false,
        })
    }

    /// Whether `token` looks like an API key or PAT (vs. an access token).
    pub fn is_api_key(token: &str) -> bool {
        token.starts_with(API_KEY_PREFIX) || token.starts_with(PAT_PREFIX)
    }

    /// Describes the caller for `/api/auth/me`.
    pub async fn whoami(&self, caller: &Authenticated) -> Result<Json, AuthError> {
        let mut out = json!({
            "subject": caller.subject,
            "credential": caller.credential,
            "mfa": caller.mfa,
        });
        if let Some(sid) = caller.session_id() {
            out["session_id"] = json!(sid);
        }
        if let Some(s) = &caller.scopes {
            out["scopes"] = json!(s.iter().map(Scope::as_str).collect::<Vec<_>>());
        }
        match &caller.subject {
            Subject::User { collection, id } => {
                out["user"] = Json::Object(self.user_view(collection, id).await?);
                out["mfa_enabled"] = json!(self.mfa_enabled(&caller.subject).await?);
            }
            Subject::Admin { id } => {
                let r = read(
                    self.data().backend(),
                    "SELECT id, email, created_at, last_login_at FROM _forgebase_admins WHERE id = ?1",
                    vec![t(id.clone())],
                )
                .await?;
                if let Some(row) = first(&r) {
                    out["admin"] = json!({"id": row.str("id"), "email": row.str("email")});
                }
                out["mfa_enabled"] = json!(self.mfa_enabled(&caller.subject).await?);
            }
            Subject::ServiceAccount { id, .. } => {
                let r = read(
                    self.data().backend(),
                    "SELECT * FROM _forgebase_service_accounts WHERE id = ?1",
                    vec![t(id.clone())],
                )
                .await?;
                if let Some(row) = first(&r) {
                    out["service_account"] = serde_json::to_value(sa_view(row)).unwrap_or(Json::Null);
                }
            }
        }
        Ok(out)
    }
}
