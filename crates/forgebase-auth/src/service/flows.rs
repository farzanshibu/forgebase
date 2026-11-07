//! Sign-in flows for end users and administrators, and MFA.
//!
//! Uniform answers: requests that send mail (verification, reset, magic link,
//! OTP) succeed whether or not the account exists, and every credential
//! failure is `invalid_credentials`. Unknown accounts still pay for an
//! Argon2id verification so timing does not reveal them either.

use forgebase_data::db::Value;
use forgebase_data::schema::{Collection, CollectionType};
use forgebase_data::{AccessContext, DataError, UpdateOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json, json};

use super::{AuthService, ClientInfo, SessionTokens, audit_in, is_unique_violation, subject_cols};
use crate::crypto;
use crate::mailer::{Mail, MailKind};
use crate::password::check_password_policy;
use crate::store::{Tx, first, i, now, ot, read, t};
use crate::{AuthError, AuthMethod, Authenticated, Subject};

/// A user record as returned to clients (no password, no hidden fields).
pub type UserView = Map<String, Json>;

/// Options of a sign-in request.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct LoginOptions {
    /// Use an HttpOnly session cookie (browser apps) instead of returning
    /// tokens in the body.
    #[serde(default)]
    pub cookie: bool,
    /// Name for the device list.
    #[serde(default)]
    pub device_name: Option<String>,
}

/// Result of a first-factor sign-in.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum LoginOutcome {
    /// Signed in.
    Session(Box<SessionTokens>),
    /// A second factor is required: send a TOTP or recovery code with
    /// `mfa_token` to `/api/auth/mfa/verify`.
    MfaRequired {
        /// Always `true`.
        mfa_required: bool,
        /// Single-use challenge token.
        mfa_token: String,
        /// Seconds the challenge is valid.
        expires_in: u64,
    },
}

/// A TOTP enrolment waiting for confirmation.
#[derive(Debug, Clone, Serialize)]
pub struct TotpSetup {
    /// Base32 secret for manual entry.
    pub secret: String,
    /// `otpauth://` URL for QR codes.
    pub otpauth_url: String,
}

/// A user row read for sign-in.
struct UserRow {
    id: String,
    email: String,
    password: Option<String>,
    verified: bool,
}

const RESERVED_SIGNUP_KEYS: &[&str] = &[
    "id",
    "created",
    "updated",
    "_version",
    "expand",
    "email",
    "password",
    "verified",
    "token_key",
];

/// Lower-cases and checks the shape of an email address.
pub(crate) fn normalize_email(email: &str) -> Result<String, AuthError> {
    let e = email.trim().to_lowercase();
    let ok = e.len() <= 254
        && e.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty()
                && local.len() <= 64
                && domain.contains('.')
                && !domain.starts_with('.')
                && !domain.ends_with('.')
                && !e.chars().any(|c| c.is_whitespace() || c.is_control())
                && domain.matches('@').count() == 0
        });
    if ok {
        Ok(e)
    } else {
        Err(AuthError::invalid("invalid email address"))
    }
}

fn user_agent_safe(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(100).collect()
}

impl AuthService {
    /// An auth collection by name (`None`: the default one).
    pub(crate) async fn auth_collection(&self, name: Option<&str>) -> Result<Collection, AuthError> {
        let name = match name.filter(|n| !n.is_empty()) {
            Some(n) => n.to_owned(),
            None => self
                .config()
                .default_collection
                .clone()
                .ok_or_else(|| AuthError::invalid("collection is required"))?,
        };
        match self.data().get_collection(&AccessContext::system(), &name).await {
            Ok(c) if c.kind == CollectionType::Auth => Ok(c),
            Ok(_) | Err(DataError::CollectionNotFound(_)) => {
                Err(AuthError::NotFound(format!("auth collection {name:?} not found")))
            }
            Err(e) => Err(e.into()),
        }
    }

    async fn find_user(&self, coll: &Collection, email: &str) -> Result<Option<UserRow>, AuthError> {
        let q = self.data().backend().dialect().quote_ident(&coll.name);
        let r = read(
            self.data().backend(),
            &format!("SELECT id, email, password, verified FROM {q} WHERE email = ?1 LIMIT 1"),
            vec![t(email)],
        )
        .await?;
        Ok(first(&r).map(|row| UserRow {
            id: row.str("id"),
            email: row.str("email"),
            password: row.opt_str("password"),
            verified: row.i64("verified") == 1,
        }))
    }

    /// The client view of a user record.
    pub async fn user_view(&self, collection: &str, id: &str) -> Result<UserView, AuthError> {
        let coll = self.auth_collection(Some(collection)).await?;
        let mut rec = self
            .data()
            .get_record(&AccessContext::system(), &coll.name, id, &Default::default())
            .await?;
        for f in &coll.fields {
            if f.hidden || f.name == "password" || f.name == "token_key" {
                rec.remove(&f.name);
            }
        }
        rec.insert("collection".into(), Json::String(coll.name.clone()));
        Ok(rec)
    }

    // -----------------------------------------------------------------------
    // Signup and password sign-in
    // -----------------------------------------------------------------------

    /// Registers a user in an auth collection and sends a verification email.
    pub async fn signup(
        &self,
        ci: &ClientInfo,
        collection: Option<&str>,
        email: &str,
        password: &str,
        extra: Option<Map<String, Json>>,
    ) -> Result<UserView, AuthError> {
        self.limit_mail(ci)?;
        let coll = self.auth_collection(collection).await?;
        let email = normalize_email(email)?;
        check_password_policy(password, Self::password_min(&coll))?;
        let mut doc = extra.unwrap_or_default();
        if let Some(k) = doc.keys().find(|k| RESERVED_SIGNUP_KEYS.contains(&k.as_str())) {
            return Err(AuthError::invalid(format!("field {k:?} cannot be set at signup")));
        }
        doc.insert("email".into(), Json::String(email.clone()));
        doc.insert("password".into(), Json::String(password.to_owned()));
        doc.insert("verified".into(), Json::Bool(false));
        doc.insert("token_key".into(), Json::String(crypto::random_token(24)?));
        self.hasher().prehash(password).await?;
        let rec = match self
            .data()
            .create_record(&AccessContext::system(), &coll.name, Json::Object(doc))
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let e = AuthError::from(e);
                if is_unique_violation(&e) {
                    self.audit(
                        "signup",
                        false,
                        ci,
                        "anonymous",
                        None,
                        json!({"collection": coll.name, "reason": "email_taken"}),
                    )
                    .await?;
                    return Err(AuthError::EmailTaken);
                }
                return Err(e);
            }
        };
        let id = rec.get("id").and_then(Json::as_str).unwrap_or_default().to_owned();
        let subject = Subject::User {
            collection: coll.name.clone(),
            id: id.clone(),
        };
        self.audit(
            "signup",
            true,
            ci,
            &subject.actor(),
            Some(&subject),
            json!({"collection": coll.name}),
        )
        .await?;
        if let Err(e) = self
            .send_link(&subject, &email, "verify_email", MailKind::VerifyEmail)
            .await
        {
            tracing::warn!(error = %e, "could not send the verification email");
        }
        self.user_view(&coll.name, &id).await
    }

    /// Email + password sign-in for an auth-collection user.
    pub async fn login_password(
        &self,
        ci: &ClientInfo,
        collection: Option<&str>,
        email: &str,
        password: &str,
        opts: &LoginOptions,
    ) -> Result<LoginOutcome, AuthError> {
        self.limit_attempt(ci)?;
        let coll = self.auth_collection(collection).await?;
        let email_n = email.trim().to_lowercase();
        let lock_key = format!("pw:user:{}:{email_n}", coll.name);
        if let Err(e) = self.lockout_check(&lock_key).await {
            self.audit(
                "login",
                false,
                ci,
                "anonymous",
                None,
                json!({"collection": coll.name, "reason": "locked"}),
            )
            .await?;
            return Err(e);
        }
        let mut user = self.find_user(&coll, &email_n).await?;
        if user.is_none() && email.trim() != email_n {
            user = self.find_user(&coll, email.trim()).await?;
        }
        let ok = self
            .hasher()
            .verify_async(password, user.as_ref().and_then(|u| u.password.clone()))
            .await;
        let Some(user) = user.filter(|_| ok) else {
            let locked = self.lockout_fail(&lock_key).await?;
            self.audit(
                "login",
                false,
                ci,
                "anonymous",
                None,
                json!({"collection": coll.name, "method": "password", "reason": "invalid_credentials", "locked": locked}),
            )
            .await?;
            return Err(AuthError::InvalidCredentials);
        };
        let subject = Subject::User {
            collection: coll.name.clone(),
            id: user.id.clone(),
        };
        if self.config().require_verified_email && !user.verified {
            self.audit(
                "login",
                false,
                ci,
                &subject.actor(),
                Some(&subject),
                json!({"reason": "email_not_verified"}),
            )
            .await?;
            return Err(AuthError::EmailNotVerified);
        }
        self.lockout_clear(&lock_key).await?;
        if let Some(stored) = &user.password
            && self.hasher().needs_rehash(stored)
        {
            let new_hash = self.hasher().hash_async(password).await?;
            let q = self.data().backend().dialect().quote_ident(&coll.name);
            let mut tx = Tx::begin(self.data().backend()).await?;
            tx.exec(
                &format!("UPDATE {q} SET password = ?2 WHERE id = ?1"),
                vec![t(user.id.clone()), t(new_hash)],
            )
            .await?;
            tx.commit().await?;
        }
        self.complete_first_factor(&subject, AuthMethod::Password, ci, opts)
            .await
    }

    /// After a successful first factor: a session, or an MFA challenge.
    pub(crate) async fn complete_first_factor(
        &self,
        subject: &Subject,
        method: AuthMethod,
        ci: &ClientInfo,
        opts: &LoginOptions,
    ) -> Result<LoginOutcome, AuthError> {
        if self.mfa_enabled(subject).await? {
            let token = crypto::random_token(32)?;
            let ttl = self.config().mfa_challenge_ttl.as_secs();
            self.store_one_time(
                subject,
                "mfa_challenge",
                &token,
                None,
                ttl,
                json!({"method": method.as_str(), "cookie": opts.cookie, "device_name": opts.device_name}),
            )
            .await?;
            self.audit(
                "login.mfa_challenge",
                true,
                ci,
                &subject.actor(),
                Some(subject),
                json!({"method": method.as_str()}),
            )
            .await?;
            return Ok(LoginOutcome::MfaRequired {
                mfa_required: true,
                mfa_token: token,
                expires_in: ttl,
            });
        }
        let mut opts = opts.clone();
        opts.device_name = opts.device_name.as_deref().map(user_agent_safe);
        Ok(LoginOutcome::Session(Box::new(
            self.issue_session(subject, method, false, ci, &opts).await?,
        )))
    }

    /// Email + password sign-in for an administrator.
    pub async fn login_admin(
        &self,
        ci: &ClientInfo,
        email: &str,
        password: &str,
        opts: &LoginOptions,
    ) -> Result<LoginOutcome, AuthError> {
        self.limit_attempt(ci)?;
        let email_n = email.trim().to_lowercase();
        let lock_key = format!("pw:admin:{email_n}");
        if let Err(e) = self.lockout_check(&lock_key).await {
            self.audit("login.admin", false, ci, "anonymous", None, json!({"reason": "locked"}))
                .await?;
            return Err(e);
        }
        let r = read(
            self.data().backend(),
            "SELECT id, password_hash FROM _forgebase_admins WHERE email = ?1",
            vec![t(email_n.clone())],
        )
        .await?;
        let row = first(&r).map(|row| (row.str("id"), row.str("password_hash")));
        let ok = self
            .hasher()
            .verify_async(password, row.as_ref().map(|(_, h)| h.clone()))
            .await;
        let Some((id, stored)) = row.filter(|_| ok) else {
            let locked = self.lockout_fail(&lock_key).await?;
            self.audit(
                "login.admin",
                false,
                ci,
                "anonymous",
                None,
                json!({"reason": "invalid_credentials", "locked": locked}),
            )
            .await?;
            return Err(AuthError::InvalidCredentials);
        };
        self.lockout_clear(&lock_key).await?;
        if self.hasher().needs_rehash(&stored) {
            let new_hash = self.hasher().hash_async(password).await?;
            let mut tx = Tx::begin(self.data().backend()).await?;
            tx.exec(
                "UPDATE _forgebase_admins SET password_hash = ?2, updated_at = ?3 WHERE id = ?1",
                vec![t(id.clone()), t(new_hash), i(now())],
            )
            .await?;
            tx.commit().await?;
        }
        self.complete_first_factor(&Subject::Admin { id }, AuthMethod::Password, ci, opts)
            .await
    }

    /// Changes the caller's password (needs the current one). Every other
    /// session of the account is revoked.
    pub async fn change_password(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        current: &str,
        new: &str,
    ) -> Result<(), AuthError> {
        self.limit_attempt(ci)?;
        let (stored, min, lock_key) = match &caller.subject {
            Subject::User { collection, id } => {
                let coll = self.auth_collection(Some(collection)).await?;
                let q = self.data().backend().dialect().quote_ident(&coll.name);
                let r = read(
                    self.data().backend(),
                    &format!("SELECT password FROM {q} WHERE id = ?1"),
                    vec![t(id.clone())],
                )
                .await?;
                (
                    first(&r).and_then(|row| row.opt_str("password")),
                    Self::password_min(&coll),
                    format!("chpw:{}", caller.subject.actor()),
                )
            }
            Subject::Admin { id } => {
                let r = read(
                    self.data().backend(),
                    "SELECT password_hash FROM _forgebase_admins WHERE id = ?1",
                    vec![t(id.clone())],
                )
                .await?;
                (
                    first(&r).map(|row| row.str("password_hash")),
                    10,
                    format!("chpw:{}", caller.subject.actor()),
                )
            }
            _ => return Err(AuthError::Forbidden("service accounts have no password".into())),
        };
        self.lockout_check(&lock_key).await?;
        if !self.hasher().verify_async(current, stored).await {
            self.lockout_fail(&lock_key).await?;
            self.audit(
                "password.change",
                false,
                ci,
                &caller.subject.actor(),
                Some(&caller.subject),
                json!({}),
            )
            .await?;
            return Err(AuthError::InvalidCredentials);
        }
        check_password_policy(new, min)?;
        self.lockout_clear(&lock_key).await?;
        self.set_password(&caller.subject, new, caller.session_id(), "password_changed", ci)
            .await
    }

    /// Stores a new password and revokes sessions (all but `keep_session`).
    async fn set_password(
        &self,
        subject: &Subject,
        new: &str,
        keep_session: Option<&str>,
        reason: &str,
        ci: &ClientInfo,
    ) -> Result<(), AuthError> {
        match subject {
            Subject::User { collection, id } => {
                self.hasher().prehash(new).await?;
                self.data()
                    .update_record(
                        &AccessContext::system(),
                        collection,
                        id,
                        json!({"password": new, "token_key": crypto::random_token(24)?}),
                        UpdateOptions::default(),
                    )
                    .await?;
            }
            Subject::Admin { id } => {
                let hash = self.hasher().hash_async(new).await?;
                let mut tx = Tx::begin(self.data().backend()).await?;
                tx.exec(
                    "UPDATE _forgebase_admins SET password_hash = ?2, updated_at = ?3 WHERE id = ?1",
                    vec![t(id.clone()), t(hash), i(now())],
                )
                .await?;
                tx.commit().await?;
            }
            _ => return Err(AuthError::Forbidden("service accounts have no password".into())),
        }
        let (st, coll, id) = subject_cols(subject);
        let mut tx = Tx::begin(self.data().backend()).await?;
        let revoked = tx
            .exec(
                "UPDATE _forgebase_auth_sessions SET revoked_at = ?4, revoked_reason = ?5
                 WHERE subject_type = ?1 AND collection IS ?2 AND subject_id = ?3 AND revoked_at IS NULL
                   AND (?6 IS NULL OR id != ?6)",
                vec![
                    t(st),
                    ot(coll),
                    t(id),
                    i(now()),
                    t(reason),
                    ot(keep_session.map(str::to_owned)),
                ],
            )
            .await?
            .rows_affected;
        audit_in(
            &mut tx,
            reason,
            true,
            ci,
            &subject.actor(),
            Some(subject),
            json!({"sessions_revoked": revoked}),
        )
        .await?;
        tx.commit().await
    }

    // -----------------------------------------------------------------------
    // One-time tokens (verification, reset, magic link, OTP, MFA challenge)
    // -----------------------------------------------------------------------

    async fn store_one_time(
        &self,
        subject: &Subject,
        purpose: &str,
        token_or_code: &str,
        email: Option<&str>,
        ttl_secs: u64,
        detail: Json,
    ) -> Result<(), AuthError> {
        let (st, coll, id) = subject_cols(subject);
        let hash = self.one_time_hash(purpose, &id, token_or_code);
        let now = now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        // A new code or link replaces older unused ones of the same purpose.
        tx.exec(
            "UPDATE _forgebase_auth_one_time SET used_at = ?5
             WHERE purpose = ?1 AND subject_type = ?2 AND collection IS ?3 AND subject_id = ?4 AND used_at IS NULL",
            vec![t(purpose), t(st), ot(coll.clone()), t(id.clone()), i(now)],
        )
        .await?;
        tx.exec(
            "INSERT INTO _forgebase_auth_one_time (token_hash, purpose, subject_type, collection, subject_id, email, detail, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            vec![
                t(hash),
                t(purpose),
                t(st),
                ot(coll),
                t(id),
                ot(email.map(str::to_owned)),
                t(detail.to_string()),
                i(now),
                i(now + ttl_secs as i64),
            ],
        )
        .await?;
        tx.commit().await
    }

    /// Hash of a one-time secret. Links are random enough to be looked up by
    /// hash alone; short codes are bound to their subject.
    fn one_time_hash(&self, purpose: &str, subject_id: &str, secret: &str) -> String {
        match purpose {
            "email_otp" => self.keyring().token_hash(purpose, &format!("{subject_id}:{secret}")),
            _ => self.keyring().token_hash(purpose, secret),
        }
    }

    /// Consumes a link token: single use, unexpired. Returns the subject and
    /// stored detail.
    async fn consume_link(&self, purpose: &str, token: &str) -> Result<(Subject, Json), AuthError> {
        let (subject, detail, tx) = self.consume_link_tx(purpose, token).await?;
        tx.commit().await?;
        Ok((subject, detail))
    }

    /// Like [`Self::consume_link`] but leaves the transaction open: dropping
    /// it puts the token back (used when the request turns out invalid).
    async fn consume_link_tx(&self, purpose: &str, token: &str) -> Result<(Subject, Json, Tx), AuthError> {
        let bad = AuthError::InvalidToken("invalid or expired token");
        if token.is_empty() || token.len() > 256 {
            return Err(bad);
        }
        let hash = self.one_time_hash(purpose, "", token);
        let mut tx = Tx::begin(self.data().backend()).await?;
        let r = tx
            .read(
                "UPDATE _forgebase_auth_one_time SET used_at = ?3
                 WHERE token_hash = ?1 AND purpose = ?2 AND used_at IS NULL AND expires_at > ?3
                 RETURNING subject_type, collection, subject_id, detail",
                vec![t(hash), t(purpose), i(now())],
            )
            .await?;
        let Some(row) = first(&r) else {
            return Err(bad);
        };
        let subject = match (row.str("subject_type").as_str(), row.opt_str("collection")) {
            ("user", Some(collection)) => Subject::User {
                collection,
                id: row.str("subject_id"),
            },
            ("admin", _) => Subject::Admin {
                id: row.str("subject_id"),
            },
            _ => return Err(AuthError::Internal("corrupt one-time token row".into())),
        };
        let detail = row
            .opt_str("detail")
            .and_then(|d| serde_json::from_str(&d).ok())
            .unwrap_or(Json::Null);
        Ok((subject, detail, tx))
    }

    async fn send_link(&self, subject: &Subject, email: &str, purpose: &str, kind: MailKind) -> Result<(), AuthError> {
        let cfg = self.config();
        let (ttl, path, subject_line, what) = match kind {
            MailKind::VerifyEmail => (
                cfg.verify_email_ttl,
                "verify-email",
                "Verify your email address",
                "confirm your email address",
            ),
            MailKind::PasswordReset => (
                cfg.password_reset_ttl,
                "reset-password",
                "Reset your password",
                "choose a new password",
            ),
            MailKind::MagicLink => (cfg.magic_link_ttl, "magic-link", "Your sign-in link", "sign in"),
            MailKind::EmailOtp => return Err(AuthError::Internal("not a link mail".into())),
        };
        let token = crypto::random_token(32)?;
        self.store_one_time(subject, purpose, &token, Some(email), ttl.as_secs(), json!({}))
            .await?;
        let link = format!("{}/auth/{path}?token={token}", cfg.public_url.trim_end_matches('/'));
        let minutes = ttl.as_secs() / 60;
        let mail = Mail {
            to: email.to_owned(),
            subject: subject_line.to_owned(),
            text: format!(
                "Open this link to {what}:\n\n{link}\n\nThe link works once and expires in {minutes} minutes. If you did not ask for it, ignore this message."
            ),
            kind,
            secret: token,
            link: Some(link),
        };
        self.mailer().send(&mail).await.map_err(AuthError::Mail)
    }

    async fn user_for_request(
        &self,
        collection: Option<&str>,
        email: &str,
    ) -> Result<(Collection, Option<UserRow>), AuthError> {
        let coll = self.auth_collection(collection).await?;
        let user = match normalize_email(email) {
            Ok(e) => self.find_user(&coll, &e).await?,
            Err(_) => None,
        };
        Ok((coll, user))
    }

    /// Sends a verification link if the account exists and is unverified.
    /// Always succeeds (no account enumeration).
    pub async fn request_verification(
        &self,
        ci: &ClientInfo,
        collection: Option<&str>,
        email: &str,
    ) -> Result<(), AuthError> {
        self.limit_mail(ci)?;
        let (coll, user) = self.user_for_request(collection, email).await?;
        if let Some(u) = user.filter(|u| !u.verified) {
            let subject = Subject::User {
                collection: coll.name,
                id: u.id,
            };
            self.send_link(&subject, &u.email, "verify_email", MailKind::VerifyEmail)
                .await?;
            self.audit(
                "verify_email.requested",
                true,
                ci,
                "anonymous",
                Some(&subject),
                json!({}),
            )
            .await?;
        }
        Ok(())
    }

    /// Confirms an email address with a verification token.
    pub async fn confirm_verification(&self, ci: &ClientInfo, token: &str) -> Result<UserView, AuthError> {
        self.limit_attempt(ci)?;
        let (subject, _) = self.consume_link("verify_email", token).await?;
        let Subject::User { collection, id } = &subject else {
            return Err(AuthError::InvalidToken("invalid or expired token"));
        };
        self.mark_verified(collection, id).await?;
        self.audit(
            "verify_email.confirmed",
            true,
            ci,
            &subject.actor(),
            Some(&subject),
            json!({}),
        )
        .await?;
        self.user_view(collection, id).await
    }

    async fn mark_verified(&self, collection: &str, id: &str) -> Result<(), AuthError> {
        self.data()
            .update_record(
                &AccessContext::system(),
                collection,
                id,
                json!({"verified": true}),
                UpdateOptions::default(),
            )
            .await?;
        Ok(())
    }

    /// Sends a password-reset link if the account exists. Always succeeds.
    pub async fn request_password_reset(
        &self,
        ci: &ClientInfo,
        collection: Option<&str>,
        email: &str,
    ) -> Result<(), AuthError> {
        self.limit_mail(ci)?;
        let (coll, user) = self.user_for_request(collection, email).await?;
        if let Some(u) = user {
            let subject = Subject::User {
                collection: coll.name,
                id: u.id,
            };
            self.send_link(&subject, &u.email, "password_reset", MailKind::PasswordReset)
                .await?;
            self.audit(
                "password_reset.requested",
                true,
                ci,
                "anonymous",
                Some(&subject),
                json!({}),
            )
            .await?;
        }
        Ok(())
    }

    /// Sets a new password with a reset token; revokes every session and
    /// clears the lockout (the user proved control of the mailbox).
    pub async fn confirm_password_reset(
        &self,
        ci: &ClientInfo,
        token: &str,
        new_password: &str,
    ) -> Result<(), AuthError> {
        self.limit_attempt(ci)?;
        let (subject, _, tx) = self.consume_link_tx("password_reset", token).await?;
        let Subject::User { collection, id } = &subject else {
            return Err(AuthError::InvalidToken("invalid or expired token"));
        };
        let coll = self.auth_collection(Some(collection)).await?;
        // A password the policy rejects leaves the token usable (tx dropped).
        check_password_policy(new_password, Self::password_min(&coll))?;
        tx.commit().await?;
        self.set_password(&subject, new_password, None, "password_reset", ci)
            .await?;
        self.mark_verified(collection, id).await?;
        let email = self
            .user_view(collection, id)
            .await?
            .get("email")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_lowercase();
        self.lockout_clear(&format!("pw:user:{collection}:{email}")).await?;
        Ok(())
    }

    /// Sends a magic sign-in link if the account exists. Always succeeds.
    pub async fn request_magic_link(
        &self,
        ci: &ClientInfo,
        collection: Option<&str>,
        email: &str,
    ) -> Result<(), AuthError> {
        self.limit_mail(ci)?;
        let (coll, user) = self.user_for_request(collection, email).await?;
        if let Some(u) = user {
            let subject = Subject::User {
                collection: coll.name,
                id: u.id,
            };
            self.send_link(&subject, &u.email, "magic_link", MailKind::MagicLink)
                .await?;
            self.audit("magic_link.requested", true, ci, "anonymous", Some(&subject), json!({}))
                .await?;
        }
        Ok(())
    }

    /// Signs in with a magic link (marks the email verified).
    pub async fn login_magic_link(
        &self,
        ci: &ClientInfo,
        token: &str,
        opts: &LoginOptions,
    ) -> Result<LoginOutcome, AuthError> {
        self.limit_attempt(ci)?;
        let (subject, _) = match self.consume_link("magic_link", token).await {
            Ok(v) => v,
            Err(e) => {
                self.audit(
                    "login",
                    false,
                    ci,
                    "anonymous",
                    None,
                    json!({"method": "magic_link", "reason": "invalid_token"}),
                )
                .await?;
                return Err(e);
            }
        };
        if let Subject::User { collection, id } = &subject {
            self.mark_verified(collection, id).await?;
        }
        self.complete_first_factor(&subject, AuthMethod::MagicLink, ci, opts)
            .await
    }

    /// Sends a 6-digit sign-in code if the account exists. Always succeeds.
    pub async fn request_email_otp(
        &self,
        ci: &ClientInfo,
        collection: Option<&str>,
        email: &str,
    ) -> Result<(), AuthError> {
        self.limit_mail(ci)?;
        let (coll, user) = self.user_for_request(collection, email).await?;
        if let Some(u) = user {
            let subject = Subject::User {
                collection: coll.name,
                id: u.id,
            };
            let code = crypto::random_digits(6)?;
            let ttl = self.config().email_otp_ttl;
            self.store_one_time(&subject, "email_otp", &code, Some(&u.email), ttl.as_secs(), json!({}))
                .await?;
            let mail = Mail {
                to: u.email.clone(),
                subject: "Your sign-in code".into(),
                text: format!(
                    "Your sign-in code is {code}. It expires in {} minutes. If you did not ask for it, ignore this message.",
                    ttl.as_secs() / 60
                ),
                kind: MailKind::EmailOtp,
                secret: code,
                link: None,
            };
            self.mailer().send(&mail).await.map_err(AuthError::Mail)?;
            self.audit("email_otp.requested", true, ci, "anonymous", Some(&subject), json!({}))
                .await?;
        }
        Ok(())
    }

    /// Signs in with an email OTP. Each code allows a few wrong guesses
    /// before it is burnt; failures also count towards account lockout.
    pub async fn login_email_otp(
        &self,
        ci: &ClientInfo,
        collection: Option<&str>,
        email: &str,
        code: &str,
        opts: &LoginOptions,
    ) -> Result<LoginOutcome, AuthError> {
        self.limit_attempt(ci)?;
        let (coll, user) = self.user_for_request(collection, email).await?;
        let lock_key = format!("otp:user:{}:{}", coll.name, email.trim().to_lowercase());
        self.lockout_check(&lock_key).await?;
        let fail = |svc: &Self, subject: Option<Subject>| {
            let svc = svc.clone();
            let lock_key = lock_key.clone();
            let ci = ci.clone();
            async move {
                let locked = svc.lockout_fail(&lock_key).await?;
                svc.audit(
                    "login",
                    false,
                    &ci,
                    "anonymous",
                    subject.as_ref(),
                    json!({"method": "email_otp", "reason": "invalid_code", "locked": locked}),
                )
                .await?;
                Err::<LoginOutcome, AuthError>(AuthError::InvalidCredentials)
            }
        };
        let Some(u) = user else {
            return fail(self, None).await;
        };
        let subject = Subject::User {
            collection: coll.name.clone(),
            id: u.id.clone(),
        };
        let now = now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        let r = tx
            .read(
                "SELECT token_hash, attempts FROM _forgebase_auth_one_time
                 WHERE purpose = 'email_otp' AND subject_type = 'user' AND collection = ?1 AND subject_id = ?2
                   AND used_at IS NULL AND expires_at > ?3
                 ORDER BY created_at DESC LIMIT 1",
                vec![t(coll.name.clone()), t(u.id.clone()), i(now)],
            )
            .await?;
        let Some(row) = first(&r) else {
            drop(tx);
            return fail(self, Some(subject)).await;
        };
        let stored = row.str("token_hash");
        let candidate = self.one_time_hash("email_otp", &u.id, code.trim());
        if !crypto::ct_eq(candidate.as_bytes(), stored.as_bytes()) {
            let attempts = row.i64("attempts") + 1;
            let burn = attempts >= self.config().otp_max_attempts;
            tx.exec(
                "UPDATE _forgebase_auth_one_time SET attempts = ?2, used_at = CASE WHEN ?3 THEN ?4 ELSE used_at END
                 WHERE token_hash = ?1",
                vec![t(stored), i(attempts), i(i64::from(burn)), i(now)],
            )
            .await?;
            tx.commit().await?;
            return fail(self, Some(subject)).await;
        }
        let claimed = tx
            .exec(
                "UPDATE _forgebase_auth_one_time SET used_at = ?2 WHERE token_hash = ?1 AND used_at IS NULL",
                vec![t(stored), i(now)],
            )
            .await?
            .rows_affected
            == 1;
        tx.commit().await?;
        if !claimed {
            return fail(self, Some(subject)).await;
        }
        self.lockout_clear(&lock_key).await?;
        self.mark_verified(&coll.name, &u.id).await?;
        self.complete_first_factor(&subject, AuthMethod::EmailOtp, ci, opts)
            .await
    }

    // -----------------------------------------------------------------------
    // MFA: TOTP + recovery codes
    // -----------------------------------------------------------------------

    fn mfa_key(subject: &Subject) -> (&'static str, String, String) {
        let (st, coll, id) = subject_cols(subject);
        (st, coll.unwrap_or_default(), id)
    }

    fn mfa_aad(subject: &Subject) -> Vec<u8> {
        let (st, coll, id) = Self::mfa_key(subject);
        format!("totp:{st}:{coll}:{id}").into_bytes()
    }

    /// Whether the subject has an enabled second factor.
    pub async fn mfa_enabled(&self, subject: &Subject) -> Result<bool, AuthError> {
        let (st, coll, id) = Self::mfa_key(subject);
        let r = read(
            self.data().backend(),
            "SELECT enabled FROM _forgebase_auth_mfa WHERE subject_type = ?1 AND collection = ?2 AND subject_id = ?3",
            vec![t(st), t(coll), t(id)],
        )
        .await?;
        Ok(first(&r).is_some_and(|row| row.i64("enabled") == 1))
    }

    fn require_session_subject(caller: &Authenticated) -> Result<(), AuthError> {
        match (&caller.subject, caller.session_id()) {
            (Subject::User { .. } | Subject::Admin { .. }, Some(_)) => Ok(()),
            _ => Err(AuthError::Forbidden("MFA is managed from a signed-in session".into())),
        }
    }

    /// Starts TOTP enrolment: a new secret, pending until confirmed.
    pub async fn totp_setup(&self, caller: &Authenticated, ci: &ClientInfo) -> Result<TotpSetup, AuthError> {
        Self::require_session_subject(caller)?;
        if self.mfa_enabled(&caller.subject).await? {
            return Err(AuthError::Conflict("TOTP is already enabled".into()));
        }
        let secret = crypto::random_bytes(20)?;
        let sealed = self.keyring().seal(&secret, &Self::mfa_aad(&caller.subject))?;
        let (st, coll, id) = Self::mfa_key(&caller.subject);
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "INSERT INTO _forgebase_auth_mfa (subject_type, collection, subject_id, secret_enc, enabled, last_step, created_at)
             VALUES (?1, ?2, ?3, ?4, 0, 0, ?5)
             ON CONFLICT (subject_type, collection, subject_id) DO UPDATE SET secret_enc = ?4, enabled = 0, last_step = 0, created_at = ?5",
            vec![t(st), t(coll), t(id), Value::Blob(sealed), i(now())],
        )
        .await?;
        audit_in(
            &mut tx,
            "mfa.setup",
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({}),
        )
        .await?;
        tx.commit().await?;
        let b32 = crypto::base32_encode(&secret);
        let label = match &caller.subject {
            Subject::User { id, .. } | Subject::Admin { id } => id.clone(),
            Subject::ServiceAccount { id, .. } => id.clone(),
        };
        let issuer = &self.config().totp_issuer;
        let enc = |s: &str| {
            s.bytes()
                .map(|b| {
                    if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                        (b as char).to_string()
                    } else {
                        format!("%{b:02X}")
                    }
                })
                .collect::<String>()
        };
        Ok(TotpSetup {
            otpauth_url: format!(
                "otpauth://totp/{}:{}?secret={b32}&issuer={}&algorithm=SHA1&digits=6&period=30",
                enc(issuer),
                enc(&label),
                enc(issuer)
            ),
            secret: b32,
        })
    }

    /// Checks a TOTP code (`Some(step)`) for a subject with a stored secret.
    async fn check_totp(&self, subject: &Subject, code: &str, require_enabled: bool) -> Result<bool, AuthError> {
        let (st, coll, id) = Self::mfa_key(subject);
        let r = read(
            self.data().backend(),
            "SELECT secret_enc, enabled, last_step FROM _forgebase_auth_mfa WHERE subject_type = ?1 AND collection = ?2 AND subject_id = ?3",
            vec![t(st), t(coll.clone()), t(id.clone())],
        )
        .await?;
        let Some(row) = first(&r) else {
            return Ok(false);
        };
        if require_enabled && row.i64("enabled") != 1 {
            return Ok(false);
        }
        let secret = self.keyring().open(&row.blob("secret_enc"), &Self::mfa_aad(subject))?;
        let now = now() as u64;
        let Some(step) = crypto::totp_check(&secret, code, now, 1, row.i64("last_step").max(0) as u64) else {
            return Ok(false);
        };
        // Record the step so the same code cannot be replayed.
        let mut tx = Tx::begin(self.data().backend()).await?;
        let n = tx
            .exec(
                "UPDATE _forgebase_auth_mfa SET last_step = ?4
                 WHERE subject_type = ?1 AND collection = ?2 AND subject_id = ?3 AND last_step < ?4",
                vec![t(st), t(coll), t(id), i(step as i64)],
            )
            .await?
            .rows_affected;
        tx.commit().await?;
        Ok(n == 1)
    }

    async fn use_recovery_code(&self, subject: &Subject, code: &str) -> Result<bool, AuthError> {
        let norm: String = code
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .map(|c| c.to_ascii_lowercase())
            .collect();
        if norm.len() != 10 {
            return Ok(false);
        }
        let (st, coll, id) = Self::mfa_key(subject);
        let hash = self
            .keyring()
            .token_hash("recovery", &format!("{st}:{coll}:{id}:{norm}"));
        let mut tx = Tx::begin(self.data().backend()).await?;
        let n = tx
            .exec(
                "UPDATE _forgebase_auth_recovery_codes SET used_at = ?2 WHERE code_hash = ?1 AND used_at IS NULL",
                vec![t(hash), i(now())],
            )
            .await?
            .rows_affected;
        tx.commit().await?;
        Ok(n == 1)
    }

    async fn new_recovery_codes(&self, tx: &mut Tx, subject: &Subject) -> Result<Vec<String>, AuthError> {
        let (st, coll, id) = Self::mfa_key(subject);
        tx.exec(
            "DELETE FROM _forgebase_auth_recovery_codes WHERE subject_type = ?1 AND collection = ?2 AND subject_id = ?3",
            vec![t(st), t(coll.clone()), t(id.clone())],
        )
        .await?;
        let mut codes = Vec::with_capacity(10);
        for _ in 0..10 {
            let raw = crypto::random_id(10)?;
            let hash = self
                .keyring()
                .token_hash("recovery", &format!("{st}:{coll}:{id}:{raw}"));
            tx.exec(
                "INSERT INTO _forgebase_auth_recovery_codes (code_hash, subject_type, collection, subject_id, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                vec![t(hash), t(st), t(coll.clone()), t(id.clone()), i(now())],
            )
            .await?;
            codes.push(format!("{}-{}", &raw[..5], &raw[5..]));
        }
        Ok(codes)
    }

    /// Confirms TOTP enrolment with a first code; returns 10 recovery codes
    /// (shown once).
    pub async fn totp_confirm(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        code: &str,
    ) -> Result<Vec<String>, AuthError> {
        Self::require_session_subject(caller)?;
        self.limit_attempt(ci)?;
        if self.mfa_enabled(&caller.subject).await? {
            return Err(AuthError::Conflict("TOTP is already enabled".into()));
        }
        if !self.check_totp(&caller.subject, code, false).await? {
            self.audit(
                "mfa.enable",
                false,
                ci,
                &caller.subject.actor(),
                Some(&caller.subject),
                json!({}),
            )
            .await?;
            return Err(AuthError::InvalidCredentials);
        }
        let (st, coll, id) = Self::mfa_key(&caller.subject);
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "UPDATE _forgebase_auth_mfa SET enabled = 1, enabled_at = ?4 WHERE subject_type = ?1 AND collection = ?2 AND subject_id = ?3",
            vec![t(st), t(coll), t(id), i(now())],
        )
        .await?;
        let codes = self.new_recovery_codes(&mut tx, &caller.subject).await?;
        if let Some(sid) = caller.session_id() {
            tx.exec(
                "UPDATE _forgebase_auth_sessions SET mfa = 1 WHERE id = ?1",
                vec![t(sid)],
            )
            .await?;
        }
        audit_in(
            &mut tx,
            "mfa.enable",
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({}),
        )
        .await?;
        tx.commit().await?;
        Ok(codes)
    }

    /// Checks a second factor (TOTP or recovery code) of an enabled subject.
    async fn second_factor(&self, subject: &Subject, code: &str) -> Result<bool, AuthError> {
        if self.check_totp(subject, code, true).await? {
            return Ok(true);
        }
        self.use_recovery_code(subject, code).await
    }

    /// Disables TOTP (needs a current code or a recovery code).
    pub async fn totp_disable(&self, caller: &Authenticated, ci: &ClientInfo, code: &str) -> Result<(), AuthError> {
        Self::require_session_subject(caller)?;
        self.limit_attempt(ci)?;
        let lock_key = format!("mfa:{}", caller.subject.actor());
        self.lockout_check(&lock_key).await?;
        if !self.second_factor(&caller.subject, code).await? {
            self.lockout_fail(&lock_key).await?;
            self.audit(
                "mfa.disable",
                false,
                ci,
                &caller.subject.actor(),
                Some(&caller.subject),
                json!({}),
            )
            .await?;
            return Err(AuthError::InvalidCredentials);
        }
        let (st, coll, id) = Self::mfa_key(&caller.subject);
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "DELETE FROM _forgebase_auth_mfa WHERE subject_type = ?1 AND collection = ?2 AND subject_id = ?3",
            vec![t(st), t(coll.clone()), t(id.clone())],
        )
        .await?;
        tx.exec(
            "DELETE FROM _forgebase_auth_recovery_codes WHERE subject_type = ?1 AND collection = ?2 AND subject_id = ?3",
            vec![t(st), t(coll), t(id)],
        )
        .await?;
        audit_in(
            &mut tx,
            "mfa.disable",
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({}),
        )
        .await?;
        tx.commit().await
    }

    /// Replaces the recovery codes (needs a current TOTP code).
    pub async fn regenerate_recovery_codes(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        code: &str,
    ) -> Result<Vec<String>, AuthError> {
        Self::require_session_subject(caller)?;
        self.limit_attempt(ci)?;
        if !self.check_totp(&caller.subject, code, true).await? {
            self.audit(
                "mfa.recovery_codes",
                false,
                ci,
                &caller.subject.actor(),
                Some(&caller.subject),
                json!({}),
            )
            .await?;
            return Err(AuthError::InvalidCredentials);
        }
        let mut tx = Tx::begin(self.data().backend()).await?;
        let codes = self.new_recovery_codes(&mut tx, &caller.subject).await?;
        audit_in(
            &mut tx,
            "mfa.recovery_codes",
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({}),
        )
        .await?;
        tx.commit().await?;
        Ok(codes)
    }

    /// Completes a sign-in that is waiting for a second factor.
    pub async fn verify_mfa(&self, ci: &ClientInfo, mfa_token: &str, code: &str) -> Result<SessionTokens, AuthError> {
        self.limit_attempt(ci)?;
        let bad = AuthError::InvalidToken("invalid or expired MFA challenge");
        if mfa_token.is_empty() || mfa_token.len() > 256 {
            return Err(bad);
        }
        let hash = self.one_time_hash("mfa_challenge", "", mfa_token);
        let r = read(
            self.data().backend(),
            "SELECT subject_type, collection, subject_id, detail, attempts FROM _forgebase_auth_one_time
             WHERE token_hash = ?1 AND purpose = 'mfa_challenge' AND used_at IS NULL AND expires_at > ?2",
            vec![t(hash.clone()), i(now())],
        )
        .await?;
        let Some(row) = first(&r) else {
            return Err(bad);
        };
        let subject = match (row.str("subject_type").as_str(), row.opt_str("collection")) {
            ("user", Some(collection)) => Subject::User {
                collection,
                id: row.str("subject_id"),
            },
            ("admin", _) => Subject::Admin {
                id: row.str("subject_id"),
            },
            _ => return Err(bad),
        };
        let detail: Json = row
            .opt_str("detail")
            .and_then(|d| serde_json::from_str(&d).ok())
            .unwrap_or(Json::Null);
        let lock_key = format!("mfa:{}", subject.actor());
        self.lockout_check(&lock_key).await?;
        if !self.second_factor(&subject, code).await? {
            let attempts = row.i64("attempts") + 1;
            let burn = attempts >= self.config().otp_max_attempts;
            let mut tx = Tx::begin(self.data().backend()).await?;
            tx.exec(
                "UPDATE _forgebase_auth_one_time SET attempts = ?2, used_at = CASE WHEN ?3 THEN ?4 ELSE used_at END WHERE token_hash = ?1",
                vec![t(hash), i(attempts), i(i64::from(burn)), i(now())],
            )
            .await?;
            tx.commit().await?;
            let locked = self.lockout_fail(&lock_key).await?;
            self.audit(
                "mfa.verify",
                false,
                ci,
                "anonymous",
                Some(&subject),
                json!({"locked": locked}),
            )
            .await?;
            return Err(AuthError::InvalidCredentials);
        }
        let mut tx = Tx::begin(self.data().backend()).await?;
        let claimed = tx
            .exec(
                "UPDATE _forgebase_auth_one_time SET used_at = ?2 WHERE token_hash = ?1 AND used_at IS NULL",
                vec![t(hash), i(now())],
            )
            .await?
            .rows_affected
            == 1;
        tx.commit().await?;
        if !claimed {
            return Err(bad);
        }
        self.lockout_clear(&lock_key).await?;
        self.audit("mfa.verify", true, ci, &subject.actor(), Some(&subject), json!({}))
            .await?;
        let method = match detail.get("method").and_then(Json::as_str) {
            Some("magic_link") => AuthMethod::MagicLink,
            Some("email_otp") => AuthMethod::EmailOtp,
            _ => AuthMethod::Password,
        };
        let opts = LoginOptions {
            cookie: detail.get("cookie").and_then(Json::as_bool).unwrap_or(false),
            device_name: detail.get("device_name").and_then(Json::as_str).map(str::to_owned),
        };
        self.issue_session(&subject, method, true, ci, &opts).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_normalisation() {
        assert_eq!(normalize_email("  Alice@Example.COM ").unwrap(), "alice@example.com");
        for bad in ["", "no-at", "@x.io", "a@nodot", "a@.x.io", "a b@x.io", "a@x.io."] {
            assert!(normalize_email(bad).is_err(), "{bad}");
        }
    }
}
