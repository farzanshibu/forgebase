//! Account lifecycle (FB-25, ADR-0015 §7): suspend, deactivate, delete,
//! anonymise, export.
//!
//! | State | Set by | Effect | Undo |
//! |---|---|---|---|
//! | suspended | administrator (`_forgebase_suspensions`, shared with the policy engine and its `/api/v1/suspensions`) | sessions revoked; sign-in, refresh and personal access tokens refused (`account_suspended`); the policy engine denies every call | lift (administrator), or `until` passes |
//! | deactivated | the user or an administrator | sessions revoked; sign-in, refresh and tokens refused (`account_deactivated`) | reactivate (administrator) |
//! | anonymised | the user (as part of a request) or an administrator | personal data replaced, every credential and auth row removed, audit IP/user agent cleared; the record id stays so references remain valid | none |
//! | deleted | the user or an administrator | the record and every auth row removed, audit IP/user agent cleared | none |
//!
//! Self-service changes need a fresh proof of identity: the current password,
//! or a session signed in within the last 10 minutes. Export returns the
//! account's data as JSON (record, phone, linked identities, passkeys,
//! sessions, tokens, MFA state, status, audit trail).

use forgebase_data::{AccessContext, DataError, UpdateOptions};
use serde_json::{Map, Value as Json, json};

use crate::service::{AuthService, ClientInfo, audit_in, require_admin};
use crate::store::{Tx, first, i, now, read, rfc3339, rows, t};
use crate::{AuthError, Authenticated, Subject};

/// How recent a sign-in must be to stand in for the password (seconds).
pub const RECENT_AUTH_SECS: i64 = 600;

/// Refuses the action inside an administrator's impersonation session
/// (ADR-0014): an impersonator must not change or remove the target's ways
/// in, delete or anonymise the account, or take its data away.
pub(crate) fn not_impersonated(caller: &Authenticated) -> Result<(), AuthError> {
    crate::service::reject_impersonated(caller)
}

/// Lifecycle state of an account.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AccountStatus {
    /// `active`, `suspended`, `deactivated` or `anonymised`.
    pub status: String,
    /// Reason given.
    pub reason: Option<String>,
    /// Suspension end (RFC 3339), if limited.
    pub until: Option<String>,
    /// Who set it.
    pub changed_by: Option<String>,
    /// When.
    pub changed_at: Option<String>,
}

fn user_of(subject: &Subject) -> Option<(&str, &str)> {
    match subject {
        Subject::User { collection, id } => Some((collection, id)),
        _ => None,
    }
}

impl AuthService {
    /// Lifecycle state of a user.
    pub async fn account_status(&self, collection: &str, id: &str) -> Result<AccountStatus, AuthError> {
        let principal = format!("user:{collection}/{id}");
        let r = read(
            self.data().backend(),
            "SELECT
               (SELECT status FROM _forgebase_auth_account_status WHERE collection = ?1 AND user_id = ?2) AS status,
               (SELECT reason FROM _forgebase_auth_account_status WHERE collection = ?1 AND user_id = ?2) AS s_reason,
               (SELECT changed_by FROM _forgebase_auth_account_status WHERE collection = ?1 AND user_id = ?2) AS s_by,
               (SELECT changed_at FROM _forgebase_auth_account_status WHERE collection = ?1 AND user_id = ?2) AS s_at,
               (SELECT reason FROM _forgebase_suspensions WHERE principal = ?3 AND (until_at IS NULL OR until_at > ?4)) AS sus_reason,
               (SELECT suspended_by FROM _forgebase_suspensions WHERE principal = ?3 AND (until_at IS NULL OR until_at > ?4)) AS sus_by,
               (SELECT suspended_at FROM _forgebase_suspensions WHERE principal = ?3 AND (until_at IS NULL OR until_at > ?4)) AS sus_at,
               (SELECT until_at FROM _forgebase_suspensions WHERE principal = ?3 AND (until_at IS NULL OR until_at > ?4)) AS sus_until",
            vec![t(collection), t(id), t(principal), i(now())],
        )
        .await?;
        let Some(row) = first(&r) else {
            return Err(AuthError::Internal("status query returned nothing".into()));
        };
        if let Some(status) = row.opt_str("status").filter(|s| s == "anonymised") {
            return Ok(AccountStatus {
                status,
                reason: row.opt_str("s_reason"),
                until: None,
                changed_by: row.opt_str("s_by"),
                changed_at: row.opt_i64("s_at").map(rfc3339),
            });
        }
        if let Some(at) = row.opt_i64("sus_at") {
            return Ok(AccountStatus {
                status: "suspended".into(),
                reason: row.opt_str("sus_reason"),
                until: row.opt_i64("sus_until").map(rfc3339),
                changed_by: row.opt_str("sus_by"),
                changed_at: Some(rfc3339(at)),
            });
        }
        Ok(match row.opt_str("status") {
            Some(status) => AccountStatus {
                status,
                reason: row.opt_str("s_reason"),
                until: None,
                changed_by: row.opt_str("s_by"),
                changed_at: row.opt_i64("s_at").map(rfc3339),
            },
            None => AccountStatus {
                status: "active".into(),
                reason: None,
                until: None,
                changed_by: None,
                changed_at: None,
            },
        })
    }

    /// Refuses sessions and tokens of accounts that are not active. Service
    /// accounts are checked for suspension; administrators are not checked.
    pub(crate) async fn ensure_active(&self, subject: &Subject) -> Result<(), AuthError> {
        match subject {
            Subject::User { collection, id } => match self.account_status(collection, id).await?.status.as_str() {
                "active" => Ok(()),
                "suspended" => Err(AuthError::AccountSuspended),
                "deactivated" => Err(AuthError::AccountDeactivated),
                _ => Err(AuthError::InvalidCredentials),
            },
            Subject::ServiceAccount { id, .. } => {
                let r = read(
                    self.data().backend(),
                    "SELECT 1 AS x FROM _forgebase_suspensions WHERE principal = ?1 AND (until_at IS NULL OR until_at > ?2)",
                    vec![t(format!("service:{id}")), i(now())],
                )
                .await?;
                if first(&r).is_some() {
                    Err(AuthError::AccountSuspended)
                } else {
                    Ok(())
                }
            }
            _ => Ok(()),
        }
    }

    /// The target of an administrator's lifecycle action (must exist).
    async fn admin_target(&self, caller: &Authenticated, collection: &str, id: &str) -> Result<Subject, AuthError> {
        require_admin(caller)?;
        let coll = self.auth_collection(Some(collection)).await?;
        let subject = Subject::User {
            collection: coll.name,
            id: id.to_owned(),
        };
        if !self.subject_exists(&subject).await? {
            return Err(AuthError::NotFound("user not found".into()));
        }
        Ok(subject)
    }

    /// Proof of identity for a self-service lifecycle change: the current
    /// password, or a sign-in within [`RECENT_AUTH_SECS`].
    async fn reauthenticate(&self, caller: &Authenticated, password: Option<&str>) -> Result<(), AuthError> {
        let Some((coll, id)) = user_of(&caller.subject) else {
            return Err(AuthError::Forbidden("only users manage their own account".into()));
        };
        let Some(sid) = caller.session_id() else {
            return Err(AuthError::Forbidden("this needs a signed-in session, not a token".into()));
        };
        if let Some(pw) = password.filter(|p| !p.is_empty()) {
            let q = self.data().backend().dialect().quote_ident(coll);
            let r = read(
                self.data().backend(),
                &format!("SELECT password FROM {q} WHERE id = ?1"),
                vec![t(id)],
            )
            .await?;
            let stored = first(&r).and_then(|row| row.opt_str("password"));
            return if self.hasher().verify_async(pw, stored).await {
                Ok(())
            } else {
                Err(AuthError::InvalidCredentials)
            };
        }
        let r = read(
            self.data().backend(),
            "SELECT created_at FROM _forgebase_auth_sessions WHERE id = ?1",
            vec![t(sid)],
        )
        .await?;
        let recent = first(&r).is_some_and(|row| now() - row.i64("created_at") <= RECENT_AUTH_SECS);
        if recent {
            Ok(())
        } else {
            Err(AuthError::Forbidden(
                "reauthentication_required: send current_password or sign in again".into(),
            ))
        }
    }

    // -----------------------------------------------------------------------
    // Suspend
    // -----------------------------------------------------------------------

    /// Suspends a user (administrators): revokes every session; sign-in,
    /// refresh and tokens are refused until lifted or `until` (Unix seconds).
    pub async fn suspend_user(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        collection: &str,
        id: &str,
        reason: &str,
        until: Option<i64>,
    ) -> Result<AccountStatus, AuthError> {
        let subject = self.admin_target(caller, collection, id).await?;
        if until.is_some_and(|u| u <= now()) {
            return Err(AuthError::invalid("until must be in the future"));
        }
        let reason: String = reason.chars().filter(|c| !c.is_control()).take(500).collect();
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "INSERT OR REPLACE INTO _forgebase_suspensions (principal, reason, suspended_by, suspended_at, until_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            vec![
                t(subject.actor()),
                t(reason.clone()),
                t(caller.subject.actor()),
                i(now()),
                until.map_or(forgebase_data::db::Value::Null, i),
            ],
        )
        .await?;
        let revoked = Self::revoke_all_in(&mut tx, &subject, "account_suspended").await?;
        audit_in(
            &mut tx,
            "account.suspended",
            true,
            ci,
            &caller.subject.actor(),
            Some(&subject),
            json!({"reason": reason, "until": until, "sessions_revoked": revoked}),
        )
        .await?;
        tx.commit().await?;
        self.changed();
        self.account_status(collection, id).await
    }

    /// Lifts a suspension (administrators).
    pub async fn unsuspend_user(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        collection: &str,
        id: &str,
    ) -> Result<AccountStatus, AuthError> {
        let subject = self.admin_target(caller, collection, id).await?;
        let mut tx = Tx::begin(self.data().backend()).await?;
        let n = tx
            .exec(
                "DELETE FROM _forgebase_suspensions WHERE principal = ?1",
                vec![t(subject.actor())],
            )
            .await?
            .rows_affected;
        audit_in(
            &mut tx,
            "account.unsuspended",
            true,
            ci,
            &caller.subject.actor(),
            Some(&subject),
            json!({"was_suspended": n > 0}),
        )
        .await?;
        tx.commit().await?;
        self.changed();
        self.account_status(collection, id).await
    }

    // -----------------------------------------------------------------------
    // Deactivate
    // -----------------------------------------------------------------------

    async fn set_deactivated(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        subject: &Subject,
        reason: &str,
    ) -> Result<(), AuthError> {
        let (coll, id) = user_of(subject).ok_or_else(|| AuthError::invalid("not a user"))?;
        let reason: String = reason.chars().filter(|c| !c.is_control()).take(500).collect();
        let mut tx = Tx::begin(self.data().backend()).await?;
        let r = tx
            .read(
                "SELECT status FROM _forgebase_auth_account_status WHERE collection = ?1 AND user_id = ?2",
                vec![t(coll), t(id)],
            )
            .await?;
        if first(&r).is_some_and(|row| row.str("status") == "anonymised") {
            return Err(AuthError::Conflict("this account is anonymised".into()));
        }
        tx.exec(
            "INSERT OR REPLACE INTO _forgebase_auth_account_status (collection, user_id, status, reason, changed_by, changed_at)
             VALUES (?1, ?2, 'deactivated', ?3, ?4, ?5)",
            vec![t(coll), t(id), t(reason.clone()), t(caller.subject.actor()), i(now())],
        )
        .await?;
        let revoked = Self::revoke_all_in(&mut tx, subject, "account_deactivated").await?;
        audit_in(
            &mut tx,
            "account.deactivated",
            true,
            ci,
            &caller.subject.actor(),
            Some(subject),
            json!({"reason": reason, "sessions_revoked": revoked}),
        )
        .await?;
        tx.commit().await
    }

    /// Deactivates the caller's own account (fresh proof of identity needed).
    pub async fn deactivate_self(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        current_password: Option<&str>,
    ) -> Result<(), AuthError> {
        not_impersonated(caller)?;
        self.limit_attempt(ci)?;
        self.reauthenticate(caller, current_password).await?;
        self.set_deactivated(caller, ci, &caller.subject.clone(), "self").await
    }

    /// Deactivates a user (administrators).
    pub async fn deactivate_user(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        collection: &str,
        id: &str,
        reason: &str,
    ) -> Result<AccountStatus, AuthError> {
        let subject = self.admin_target(caller, collection, id).await?;
        self.set_deactivated(caller, ci, &subject, reason).await?;
        self.account_status(collection, id).await
    }

    /// Reactivates a deactivated user (administrators).
    pub async fn reactivate_user(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        collection: &str,
        id: &str,
    ) -> Result<AccountStatus, AuthError> {
        let subject = self.admin_target(caller, collection, id).await?;
        let mut tx = Tx::begin(self.data().backend()).await?;
        let n = tx
            .exec(
                "DELETE FROM _forgebase_auth_account_status WHERE collection = ?1 AND user_id = ?2 AND status = 'deactivated'",
                vec![t(subject_collection(&subject)), t(id)],
            )
            .await?
            .rows_affected;
        if n == 0 {
            return Err(AuthError::Conflict("this account is not deactivated".into()));
        }
        audit_in(
            &mut tx,
            "account.reactivated",
            true,
            ci,
            &caller.subject.actor(),
            Some(&subject),
            json!({}),
        )
        .await?;
        tx.commit().await?;
        self.account_status(collection, id).await
    }

    // -----------------------------------------------------------------------
    // Delete and anonymise
    // -----------------------------------------------------------------------

    /// Removes every auth row of a user and clears IP and user agent from
    /// its audit trail (the events themselves stay).
    async fn erase_auth_rows(&self, tx: &mut Tx, coll: &str, id: &str, email: Option<&str>) -> Result<(), AuthError> {
        let principal = format!("user:{coll}/{id}");
        let r = tx
            .read(
                "SELECT phone FROM _forgebase_auth_phones WHERE collection = ?1 AND user_id = ?2",
                vec![t(coll), t(id)],
            )
            .await?;
        let phone = first(&r).map(|row| row.str("phone"));
        let by_user = [
            "DELETE FROM _forgebase_auth_refresh_tokens WHERE session_id IN
               (SELECT id FROM _forgebase_auth_sessions WHERE subject_type = 'user' AND collection = ?1 AND subject_id = ?2)",
            "DELETE FROM _forgebase_auth_sessions WHERE subject_type = 'user' AND collection = ?1 AND subject_id = ?2",
            "DELETE FROM _forgebase_auth_one_time WHERE subject_type = 'user' AND collection = ?1 AND subject_id = ?2",
            "DELETE FROM _forgebase_auth_mfa WHERE subject_type = 'user' AND collection = ?1 AND subject_id = ?2",
            "DELETE FROM _forgebase_auth_recovery_codes WHERE subject_type = 'user' AND collection = ?1 AND subject_id = ?2",
            "DELETE FROM _forgebase_auth_api_keys WHERE owner_type = 'user' AND collection = ?1 AND owner_id = ?2",
            "DELETE FROM _forgebase_oauth_identities WHERE collection = ?1 AND user_id = ?2",
            "DELETE FROM _forgebase_oauth_states WHERE collection = ?1 AND link_user_id = ?2",
            "DELETE FROM _forgebase_webauthn_credentials WHERE collection = ?1 AND user_id = ?2",
            "DELETE FROM _forgebase_webauthn_challenges WHERE collection = ?1 AND user_id = ?2",
            "DELETE FROM _forgebase_auth_sms_codes WHERE collection = ?1 AND user_id = ?2",
            "DELETE FROM _forgebase_auth_phones WHERE collection = ?1 AND user_id = ?2",
            "UPDATE _forgebase_auth_audit SET ip = NULL, user_agent = NULL
               WHERE subject_type = 'user' AND collection = ?1 AND subject_id = ?2",
        ];
        for sql in by_user {
            tx.exec(sql, vec![t(coll), t(id)]).await?;
        }
        tx.exec(
            "DELETE FROM _forgebase_suspensions WHERE principal = ?1",
            vec![t(principal)],
        )
        .await?;
        if let Some(p) = &phone {
            tx.exec(
                "DELETE FROM _forgebase_auth_sms_codes WHERE collection = ?1 AND phone = ?2",
                vec![t(coll), t(p.clone())],
            )
            .await?;
            tx.exec(
                "DELETE FROM _forgebase_auth_lockouts WHERE key = ?1",
                vec![t(format!("sms:user:{coll}:{p}"))],
            )
            .await?;
        }
        if let Some(e) = email {
            let e = e.to_lowercase();
            for k in [format!("pw:user:{coll}:{e}"), format!("otp:user:{coll}:{e}")] {
                tx.exec("DELETE FROM _forgebase_auth_lockouts WHERE key = ?1", vec![t(k)])
                    .await?;
            }
        }
        Ok(())
    }

    async fn record_email(&self, coll: &str, id: &str) -> Result<Option<String>, AuthError> {
        let q = self.data().backend().dialect().quote_ident(coll);
        let r = read(
            self.data().backend(),
            &format!("SELECT email FROM {q} WHERE id = ?1"),
            vec![t(id)],
        )
        .await?;
        Ok(first(&r).map(|row| row.str("email")))
    }

    async fn delete_account(&self, caller: &Authenticated, ci: &ClientInfo, subject: &Subject) -> Result<(), AuthError> {
        let (coll, id) = user_of(subject).ok_or_else(|| AuthError::invalid("not a user"))?;
        let email = self.record_email(coll, id).await?;
        match self
            .data()
            .delete_record(&AccessContext::system(), coll, id, None)
            .await
        {
            Ok(_) | Err(DataError::RecordNotFound { .. }) => {}
            Err(e) => return Err(e.into()),
        }
        let mut tx = Tx::begin(self.data().backend()).await?;
        self.erase_auth_rows(&mut tx, coll, id, email.as_deref()).await?;
        tx.exec(
            "DELETE FROM _forgebase_auth_account_status WHERE collection = ?1 AND user_id = ?2",
            vec![t(coll), t(id)],
        )
        .await?;
        audit_in(
            &mut tx,
            "account.deleted",
            true,
            &ClientInfo {
                ip: None,
                user_agent: None,
                ..ci.clone()
            },
            &caller.subject.actor(),
            Some(subject),
            json!({"self": caller.subject == *subject}),
        )
        .await?;
        tx.commit().await?;
        self.changed();
        Ok(())
    }

    /// Replaces a user's personal data and removes every credential; the
    /// record id stays. Returns the required custom fields that kept their
    /// values (they cannot be emptied).
    async fn anonymise_account(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        subject: &Subject,
    ) -> Result<Vec<String>, AuthError> {
        let (coll_name, id) = user_of(subject).ok_or_else(|| AuthError::invalid("not a user"))?;
        let coll = self.auth_collection(Some(coll_name)).await?;
        let email = self.record_email(coll_name, id).await?;
        let password = crate::crypto::random_token(32)?;
        self.hasher().prehash(&password).await?;
        let mut patch = Map::new();
        let mut kept = Vec::new();
        for f in coll.fields.iter().filter(|f| !f.system) {
            match f.name.as_str() {
                "email" | "password" | "verified" | "token_key" => {}
                _ if f.required => kept.push(f.name.clone()),
                _ => {
                    patch.insert(f.name.clone(), Json::Null);
                }
            }
        }
        patch.insert(
            "email".into(),
            Json::String(format!(
                "anonymised.{}@anonymised.invalid",
                crate::crypto::random_id(20)?.to_lowercase()
            )),
        );
        patch.insert("password".into(), Json::String(password));
        patch.insert("verified".into(), Json::Bool(false));
        patch.insert("token_key".into(), Json::String(crate::crypto::random_token(24)?));
        self.data()
            .update_record(
                &AccessContext::system(),
                coll_name,
                id,
                Json::Object(patch),
                UpdateOptions::default(),
            )
            .await?;
        let mut tx = Tx::begin(self.data().backend()).await?;
        self.erase_auth_rows(&mut tx, coll_name, id, email.as_deref()).await?;
        tx.exec(
            "INSERT OR REPLACE INTO _forgebase_auth_account_status (collection, user_id, status, reason, changed_by, changed_at)
             VALUES (?1, ?2, 'anonymised', '', ?3, ?4)",
            vec![t(coll_name), t(id), t(caller.subject.actor()), i(now())],
        )
        .await?;
        audit_in(
            &mut tx,
            "account.anonymised",
            true,
            &ClientInfo {
                ip: None,
                user_agent: None,
                ..ci.clone()
            },
            &caller.subject.actor(),
            Some(subject),
            json!({"self": caller.subject == *subject, "kept_fields": kept}),
        )
        .await?;
        tx.commit().await?;
        self.changed();
        Ok(kept)
    }

    /// Deletes (or, with `anonymise`, anonymises) the caller's own account.
    pub async fn delete_self(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        current_password: Option<&str>,
        anonymise: bool,
    ) -> Result<Json, AuthError> {
        not_impersonated(caller)?;
        self.limit_attempt(ci)?;
        self.reauthenticate(caller, current_password).await?;
        let subject = caller.subject.clone();
        if anonymise {
            let kept = self.anonymise_account(caller, ci, &subject).await?;
            Ok(json!({"anonymised": true, "kept_fields": kept}))
        } else {
            self.delete_account(caller, ci, &subject).await?;
            Ok(json!({"deleted": true}))
        }
    }

    /// Deletes a user (administrators).
    pub async fn delete_user(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        collection: &str,
        id: &str,
    ) -> Result<(), AuthError> {
        not_impersonated(caller)?;
        let subject = self.admin_target(caller, collection, id).await?;
        self.delete_account(caller, ci, &subject).await
    }

    /// Anonymises a user (administrators).
    pub async fn anonymise_user(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        collection: &str,
        id: &str,
    ) -> Result<Json, AuthError> {
        not_impersonated(caller)?;
        let subject = self.admin_target(caller, collection, id).await?;
        let kept = self.anonymise_account(caller, ci, &subject).await?;
        Ok(json!({"anonymised": true, "kept_fields": kept}))
    }

    // -----------------------------------------------------------------------
    // Export
    // -----------------------------------------------------------------------

    async fn export_account(&self, subject: &Subject) -> Result<Json, AuthError> {
        let (coll, id) = user_of(subject).ok_or_else(|| AuthError::invalid("not a user"))?;
        let db = self.data().backend();
        let user = self.user_view(coll, id).await?;
        let q = |sql: &'static str| read(db, sql, vec![t(coll), t(id)]);
        let phone = q("SELECT phone, verified_at FROM _forgebase_auth_phones WHERE collection = ?1 AND user_id = ?2").await?;
        let identities = q(
            "SELECT id, provider, provider_subject, email, email_verified, display_name, created_at, last_login_at
             FROM _forgebase_oauth_identities WHERE collection = ?1 AND user_id = ?2 ORDER BY created_at",
        )
        .await?;
        let passkeys = q(
            "SELECT credential_id, name, use_as_mfa, created_at, last_used_at
             FROM _forgebase_webauthn_credentials WHERE collection = ?1 AND user_id = ?2 ORDER BY created_at",
        )
        .await?;
        let sessions = q(
            "SELECT id, auth_method, mfa, ip, user_agent, device_name, created_at, last_seen_at, expires_at, revoked_at
             FROM _forgebase_auth_sessions WHERE subject_type = 'user' AND collection = ?1 AND subject_id = ?2
             ORDER BY created_at",
        )
        .await?;
        let tokens = q(
            "SELECT id, name, scopes, created_at, expires_at, last_used_at, revoked_at
             FROM _forgebase_auth_api_keys WHERE owner_type = 'user' AND collection = ?1 AND owner_id = ?2
             ORDER BY created_at",
        )
        .await?;
        let mfa = q(
            "SELECT enabled, enabled_at FROM _forgebase_auth_mfa WHERE subject_type = 'user' AND collection = ?1 AND subject_id = ?2",
        )
        .await?;
        let audit = q(
            "SELECT id, at, event, outcome, actor, ip, user_agent, detail FROM _forgebase_auth_audit
             WHERE subject_type = 'user' AND collection = ?1 AND subject_id = ?2 ORDER BY id",
        )
        .await?;
        let ts = |v: Option<i64>| v.map(rfc3339);
        Ok(json!({
            "format": "forgebase.account-export.v1",
            "exported_at": rfc3339(now()),
            "realm": self.realms().realm_of_collection(coll),
            "user": user,
            "status": self.account_status(coll, id).await?,
            "phone": first(&phone).map(|r| json!({"phone": r.str("phone"), "verified_at": rfc3339(r.i64("verified_at"))})),
            "identities": rows(&identities).map(|r| json!({
                "id": r.str("id"), "provider": r.str("provider"), "provider_subject": r.str("provider_subject"),
                "email": r.opt_str("email"), "email_verified": r.i64("email_verified") == 1,
                "display_name": r.opt_str("display_name"), "created_at": rfc3339(r.i64("created_at")),
                "last_login_at": ts(r.opt_i64("last_login_at")),
            })).collect::<Vec<_>>(),
            "passkeys": rows(&passkeys).map(|r| json!({
                "id": r.str("credential_id"), "name": r.opt_str("name"), "use_as_mfa": r.i64("use_as_mfa") == 1,
                "created_at": rfc3339(r.i64("created_at")), "last_used_at": ts(r.opt_i64("last_used_at")),
            })).collect::<Vec<_>>(),
            "sessions": rows(&sessions).map(|r| json!({
                "id": r.str("id"), "auth_method": r.str("auth_method"), "mfa": r.i64("mfa") == 1,
                "ip": r.opt_str("ip"), "user_agent": r.opt_str("user_agent"), "device_name": r.opt_str("device_name"),
                "created_at": rfc3339(r.i64("created_at")), "last_seen_at": rfc3339(r.i64("last_seen_at")),
                "expires_at": rfc3339(r.i64("expires_at")), "revoked_at": ts(r.opt_i64("revoked_at")),
            })).collect::<Vec<_>>(),
            "personal_access_tokens": rows(&tokens).map(|r| json!({
                "id": r.str("id"), "name": r.str("name"),
                "scopes": serde_json::from_str::<Json>(&r.str("scopes")).unwrap_or(Json::Null),
                "created_at": rfc3339(r.i64("created_at")), "expires_at": ts(r.opt_i64("expires_at")),
                "last_used_at": ts(r.opt_i64("last_used_at")), "revoked_at": ts(r.opt_i64("revoked_at")),
            })).collect::<Vec<_>>(),
            "mfa": {"totp_enabled": first(&mfa).is_some_and(|r| r.i64("enabled") == 1)},
            "audit": rows(&audit).map(|r| json!({
                "id": r.i64("id"), "at": rfc3339(r.i64("at")), "event": r.str("event"), "outcome": r.str("outcome"),
                "actor": r.str("actor"), "ip": r.opt_str("ip"), "user_agent": r.opt_str("user_agent"),
                "detail": r.opt_str("detail").and_then(|d| serde_json::from_str::<Json>(&d).ok()),
            })).collect::<Vec<_>>(),
        }))
    }

    /// The caller's own data export.
    pub async fn export_self(&self, caller: &Authenticated, ci: &ClientInfo) -> Result<Json, AuthError> {
        not_impersonated(caller)?;
        self.limit_mail(ci)?;
        if user_of(&caller.subject).is_none() || caller.session_id().is_none() {
            return Err(AuthError::Forbidden("only signed-in users export their account".into()));
        }
        let out = self.export_account(&caller.subject).await?;
        self.audit(
            "account.exported",
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({"self": true}),
        )
        .await?;
        Ok(out)
    }

    /// A user's data export (administrators).
    pub async fn export_user(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        collection: &str,
        id: &str,
    ) -> Result<Json, AuthError> {
        not_impersonated(caller)?;
        let subject = self.admin_target(caller, collection, id).await?;
        let out = self.export_account(&subject).await?;
        self.audit(
            "account.exported",
            true,
            ci,
            &caller.subject.actor(),
            Some(&subject),
            json!({"self": false}),
        )
        .await?;
        Ok(out)
    }

    /// Lifecycle state of a user (administrators).
    pub async fn user_status(&self, caller: &Authenticated, collection: &str, id: &str) -> Result<AccountStatus, AuthError> {
        let subject = self.admin_target(caller, collection, id).await?;
        let coll = subject_collection(&subject);
        self.account_status(&coll, id).await
    }
}

fn subject_collection(s: &Subject) -> String {
    match s {
        Subject::User { collection, .. } => collection.clone(),
        _ => String::new(),
    }
}
