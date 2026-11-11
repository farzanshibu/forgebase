//! Admin impersonation and the administrator's view of user accounts
//! (ADR-0014, FB-23).
//!
//! An impersonation is a short user session (`auth_method = impersonation`)
//! with `impersonator` set to the administrator. It has no refresh token and
//! no cookie, never passes a second factor, and cannot change credentials
//! (see `reject_impersonated`). Every audit row written while it is in use
//! records the administrator.
//!
//! Realm administrators (FB-137: `identity:admin` through a realm binding)
//! impersonate, view and manage the users of their own realm only, and
//! cannot impersonate another realm administrator. Platform administrators
//! are unchanged.

use serde::Serialize;
use serde_json::{Value as Json, json};

use super::{AdminScope, AuditEntry, AuthService, ClientInfo, SessionView, audit_entry, audit_in, reject_impersonated};
use crate::store::{Tx, first, i, now, ot, read, rfc3339, rows, t};
use crate::{AuthError, AuthMethod, Authenticated, CredentialKind, Subject};

/// Shortest and longest impersonation, minutes.
pub const IMPERSONATION_MAX_MINUTES: u32 = 60;
/// Default impersonation length, minutes.
pub const IMPERSONATION_DEFAULT_MINUTES: u32 = 15;

/// A started impersonation: the only time the token is returned.
#[derive(Debug, Clone, Serialize)]
pub struct ImpersonationGrant {
    /// Always `Bearer`.
    pub token_type: &'static str,
    /// Access token of the target user, valid until `expires_at`.
    pub access_token: String,
    /// Seconds until it expires.
    pub expires_in: u64,
    /// Expiry (RFC 3339).
    pub expires_at: String,
    /// Session id (use it to end the impersonation).
    pub session_id: String,
    /// The administrator acting: a platform administrator's id, or a realm
    /// administrator's principal (`user:<collection>/<id>`).
    pub impersonator: String,
    /// The user being impersonated.
    pub subject: Subject,
    /// Why.
    pub reason: String,
}

/// One impersonation, past or present.
#[derive(Debug, Clone, Serialize)]
pub struct ImpersonationView {
    /// Session id.
    pub session_id: String,
    /// Administrator who acted (an id, or a realm administrator's principal).
    pub admin_id: String,
    /// Target, `user:<collection>/<id>`.
    pub target: String,
    /// Reason given.
    pub reason: String,
    /// Started.
    pub created_at: String,
    /// Planned end.
    pub expires_at: String,
    /// Ended early (revoked / logged out), if so.
    pub ended_at: Option<String>,
    /// Still usable.
    pub active: bool,
}

fn parse_user_principal(principal: &str) -> Result<(String, String), AuthError> {
    if principal.starts_with("admin:") || principal.starts_with("service:") {
        return Err(AuthError::Forbidden(
            "cannot_impersonate_admin: administrators and service accounts cannot be impersonated".into(),
        ));
    }
    principal
        .strip_prefix("user:")
        .and_then(|r| r.split_once('/'))
        .filter(|(c, id)| !c.is_empty() && !id.is_empty())
        .map(|(c, id)| (c.to_owned(), id.to_owned()))
        .ok_or_else(|| AuthError::invalid("principal must be `user:<collection>/<id>`"))
}

impl AuthService {
    /// Starts an impersonation of a user (administrators with a plain, MFA-passed
    /// session only; a realm administrator's target lives in their realm).
    pub async fn impersonate(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        principal: &str,
        reason: &str,
        ttl_minutes: Option<u32>,
    ) -> Result<ImpersonationGrant, AuthError> {
        reject_impersonated(caller)?;
        let scope = match &caller.subject {
            Subject::Admin { .. } | Subject::User { .. } => self.admin_scope(caller).ok(),
            _ => None,
        };
        let Some(scope) = scope else {
            return Err(AuthError::Forbidden("only administrators can impersonate users".into()));
        };
        let admin_id = match &caller.subject {
            Subject::Admin { id } => id.clone(),
            other => other.actor(),
        };
        if !matches!(
            caller.credential,
            CredentialKind::AccessToken | CredentialKind::SessionCookie
        ) {
            return Err(AuthError::Forbidden(
                "impersonation needs an interactive administrator session".into(),
            ));
        }
        if !caller.mfa && self.mfa_enabled(&caller.subject).await? {
            return Err(AuthError::Forbidden(
                "impersonation needs a session that passed multi-factor authentication".into(),
            ));
        }
        let reason = reason.trim();
        if !(3..=500).contains(&reason.chars().count()) {
            return Err(AuthError::invalid("reason is required (3 to 500 characters)"));
        }
        let minutes = ttl_minutes.unwrap_or(IMPERSONATION_DEFAULT_MINUTES);
        if !(1..=IMPERSONATION_MAX_MINUTES).contains(&minutes) {
            return Err(AuthError::invalid(format!(
                "ttl_minutes must be between 1 and {IMPERSONATION_MAX_MINUTES}"
            )));
        }
        let (collection, user_id) = parse_user_principal(principal)?;
        let target = Subject::User {
            collection,
            id: user_id,
        };
        self.require_admin_of(caller, &target)?;
        if scope != AdminScope::Platform && self.realm_admin_realm(&target).is_some() {
            return Err(AuthError::Forbidden(
                "cannot_impersonate_admin: realm administrators cannot be impersonated by a realm administrator".into(),
            ));
        }
        if !self.subject_exists(&target).await? {
            return Err(AuthError::NotFound("user not found".into()));
        }

        let started = now();
        let ttl = u64::from(minutes) * 60;
        let expires_at = started + i64::try_from(ttl).unwrap_or(900);
        let sid = crate::crypto::random_id(24)?;
        let (st, coll, tid) = super::subject_cols(&target);
        let mut tx = Tx::begin(self.db()).await?;
        tx.exec(
            "INSERT INTO _forgebase_auth_sessions
                (id, subject_type, collection, subject_id, auth_method, mfa, ip, user_agent, device_name,
                 created_at, last_seen_at, expires_at, impersonator, impersonation_reason)
             VALUES (?1, ?2, ?3, ?4, 'impersonation', 0, ?5, ?6, 'impersonation', ?7, ?7, ?8, ?9, ?10)",
            vec![
                t(sid.clone()),
                t(st),
                ot(coll),
                t(tid),
                ot(ci.ip.clone()),
                ot(ci.user_agent.clone()),
                i(started),
                i(expires_at),
                t(admin_id.clone()),
                t(reason),
            ],
        )
        .await?;
        audit_in(
            &mut tx,
            "impersonation.start",
            true,
            ci,
            &caller.subject.actor(),
            Some(&target),
            json!({"session_id": sid, "reason": reason, "expires_at": rfc3339(expires_at), "admin_id": admin_id}),
        )
        .await?;
        tx.commit().await?;
        let (token, expires_in) = self
            .sign_access_ttl(&target, &sid, AuthMethod::Impersonation, false, ttl)
            .await?;
        Ok(ImpersonationGrant {
            token_type: "Bearer",
            access_token: token,
            expires_in,
            expires_at: rfc3339(expires_at),
            session_id: sid,
            impersonator: admin_id,
            subject: target,
            reason: reason.to_owned(),
        })
    }

    /// Impersonations, newest first (administrators; a realm administrator
    /// sees those of their realm's users).
    pub async fn list_impersonations(&self, caller: &Authenticated) -> Result<Vec<ImpersonationView>, AuthError> {
        let scope = self.admin_scope(caller)?;
        let (realm_sql, params) = match scope.realm() {
            Some(r) => self.realm_users_sql(r, "collection", 1),
            None => ("1 = 1".to_owned(), Vec::new()),
        };
        let r = read(
            self.db(),
            &format!(
                "SELECT id, collection, subject_id, impersonator, impersonation_reason, created_at, expires_at, revoked_at
                 FROM _forgebase_auth_sessions WHERE impersonator IS NOT NULL AND {realm_sql}
                 ORDER BY created_at DESC, id LIMIT 200"
            ),
            params,
        )
        .await?;
        let n = now();
        Ok(rows(&r)
            .map(|row| ImpersonationView {
                session_id: row.str("id"),
                admin_id: row.str("impersonator"),
                target: format!(
                    "user:{}/{}",
                    row.opt_str("collection").unwrap_or_default(),
                    row.str("subject_id")
                ),
                reason: row.opt_str("impersonation_reason").unwrap_or_default(),
                created_at: rfc3339(row.i64("created_at")),
                expires_at: rfc3339(row.i64("expires_at")),
                ended_at: row.opt_i64("revoked_at").map(rfc3339),
                active: row.opt_i64("revoked_at").is_none() && row.i64("expires_at") > n,
            })
            .collect())
    }

    /// Ends an impersonation now: any administrator of the target (a realm
    /// administrator: of their realm's users), or the impersonation session
    /// itself. Takes effect on the next request.
    pub async fn end_impersonation(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        session_id: &str,
    ) -> Result<(), AuthError> {
        let own = caller.impersonator.is_some() && caller.session_id() == Some(session_id);
        let realm = if own {
            None
        } else {
            self.admin_scope(caller)?.realm().map(str::to_owned)
        };
        let (realm_sql, mut params) = match &realm {
            Some(r) => self.realm_users_sql(r, "collection", 3),
            None => ("1 = 1".to_owned(), Vec::new()),
        };
        params.splice(0..0, [t(session_id), i(now())]);
        let mut tx = Tx::begin(self.db()).await?;
        let r = tx
            .read(
                &format!(
                    "UPDATE _forgebase_auth_sessions SET revoked_at = ?2, revoked_reason = 'impersonation_ended'
                     WHERE id = ?1 AND impersonator IS NOT NULL AND revoked_at IS NULL AND {realm_sql}
                     RETURNING collection, subject_id"
                ),
                params,
            )
            .await?;
        let Some(row) = first(&r) else {
            return Err(AuthError::NotFound("active impersonation not found".into()));
        };
        let target = Subject::User {
            collection: row.opt_str("collection").unwrap_or_default(),
            id: row.str("subject_id"),
        };
        audit_in(
            &mut tx,
            "impersonation.end",
            true,
            ci,
            &caller.subject.actor(),
            Some(&target),
            json!({"session_id": session_id}),
        )
        .await?;
        tx.commit().await
    }

    // -----------------------------------------------------------------------
    // The administrator's view of a user
    // -----------------------------------------------------------------------

    /// The user `collection`/`id` as an administrator's target: platform
    /// administrators reach every user, realm administrators their realm's.
    fn admin_user(&self, caller: &Authenticated, collection: &str, id: &str) -> Result<Subject, AuthError> {
        let subject = Subject::User {
            collection: collection.to_owned(),
            id: id.to_owned(),
        };
        self.require_admin_of(caller, &subject)?;
        Ok(subject)
    }

    /// A user with their MFA state and session count (administrators).
    pub async fn admin_user_detail(
        &self,
        caller: &Authenticated,
        collection: &str,
        id: &str,
    ) -> Result<Json, AuthError> {
        let subject = self.admin_user(caller, collection, id)?;
        let user = self.user_view(collection, id).await?;
        let sessions = self.sessions_of(&subject, None).await?;
        let last = read(
            self.db(),
            "SELECT MAX(at) AS at FROM _forgebase_auth_audit
             WHERE event = 'login' AND outcome = 'success' AND subject_type = 'user' AND collection = ?1 AND subject_id = ?2",
            vec![t(collection), t(id)],
        )
        .await?;
        let last_login = first(&last).and_then(|r| r.opt_i64("at")).map(rfc3339);
        Ok(json!({
            "user": user,
            "mfa_enabled": self.mfa_enabled(&subject).await?,
            "active_sessions": sessions.len(),
            "last_login_at": last_login,
        }))
    }

    /// Active sessions of a user (administrators).
    pub async fn admin_user_sessions(
        &self,
        caller: &Authenticated,
        collection: &str,
        id: &str,
    ) -> Result<Vec<SessionView>, AuthError> {
        let subject = self.admin_user(caller, collection, id)?;
        self.user_view(collection, id).await?;
        self.sessions_of(&subject, None).await
    }

    /// Revokes every active session of a user (administrators); returns the
    /// number revoked.
    pub async fn admin_revoke_user_sessions(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        collection: &str,
        id: &str,
    ) -> Result<u64, AuthError> {
        let subject = self.admin_user(caller, collection, id)?;
        self.user_view(collection, id).await?;
        let mut tx = Tx::begin(self.db()).await?;
        let n = tx
            .exec(
                "UPDATE _forgebase_auth_sessions SET revoked_at = ?4, revoked_reason = 'revoked_by_admin'
                 WHERE subject_type = 'user' AND collection = ?1 AND subject_id = ?2 AND revoked_at IS NULL AND expires_at > ?3",
                vec![t(collection), t(id), i(now()), i(now())],
            )
            .await?
            .rows_affected;
        audit_in(
            &mut tx,
            "session.revoked_all",
            true,
            ci,
            &caller.subject.actor(),
            Some(&subject),
            json!({"revoked": n}),
        )
        .await?;
        tx.commit().await?;
        Ok(n)
    }

    /// Sign-in history of a user (administrators).
    pub async fn admin_user_login_history(
        &self,
        caller: &Authenticated,
        collection: &str,
        id: &str,
        limit: u32,
    ) -> Result<Vec<AuditEntry>, AuthError> {
        self.admin_user(caller, collection, id)?;
        let r = read(
            self.db(),
            "SELECT id, at, event, outcome, actor, subject_type, collection, subject_id, ip, user_agent, request_id, detail, impersonator
             FROM _forgebase_auth_audit
             WHERE subject_type = 'user' AND collection = ?1 AND subject_id = ?2
               AND (event LIKE 'login%' OR event = 'mfa.verify' OR event LIKE 'impersonation%')
             ORDER BY id DESC LIMIT ?3",
            vec![t(collection), t(id), i(i64::from(limit.clamp(1, 200)))],
        )
        .await?;
        Ok(rows(&r).map(audit_entry).collect())
    }

    /// Every API key and PAT, newest first (administrators; a realm
    /// administrator sees the keys of their realm's users and non-admin
    /// service accounts).
    pub async fn list_all_keys(&self, caller: &Authenticated) -> Result<Vec<super::ApiKeyView>, AuthError> {
        let scope = self.admin_scope(caller)?;
        let (realm_sql, params) = match scope.realm() {
            Some(r) => self.realm_principals_sql(r, "owner_type", "collection", "owner_id", 1),
            None => ("1 = 1".to_owned(), Vec::new()),
        };
        let r = read(
            self.data().backend(),
            &format!(
                "SELECT {} FROM _forgebase_auth_api_keys WHERE {realm_sql} ORDER BY created_at DESC, id LIMIT 500",
                super::machine::KEY_COLS
            ),
            params,
        )
        .await?;
        Ok(rows(&r).map(super::machine::key_view).collect())
    }
}
