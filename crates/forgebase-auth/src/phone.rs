//! Phone-number sign-in with SMS codes (FB-25, ADR-0015 §4).
//!
//! * `POST /api/auth/phone/request {phone}` sends a 6-digit code when the
//!   number belongs to an account of the realm, or when the realm lets phone
//!   numbers sign up; the answer is the same either way.
//! * `POST /api/auth/phone/verify {phone, code}` signs in (creating the
//!   account on first use when signup is allowed) and continues like every
//!   first factor (MFA challenge, session).
//! * Signed-in users add a number with `phone/add` + `phone/confirm` (the
//!   code proves possession), read it with `GET phone` and remove it with
//!   `DELETE phone`.
//!
//! Numbers are E.164 and unique per user pool (realm). Codes are HMACed
//! under the pepper, bound to collection + number, single use, expire after
//! `email_otp_ttl`, and are burnt after `otp_max_attempts` wrong guesses;
//! failures also count towards the per-number lockout. Sends are limited per
//! IP (`ip_mail_per_hour`) and per number (5 per hour).

use std::time::Duration;

use forgebase_data::AccessContext;
use forgebase_data::schema::Collection;
use serde::Serialize;
use serde_json::{Value as Json, json};

use crate::crypto;
use crate::realm::{Method, RealmIdentity};
use crate::service::{ClientInfo, LoginOptions, LoginOutcome, audit_in, is_unique_violation};
use crate::sms::{Sms, SmsKind, mask_phone};
use crate::store::{Tx, first, i, ot, read, rfc3339, t};
use crate::turnstile::TurnstileAction;
use crate::{AuthError, AuthMethod, AuthService, Authenticated, Subject};

/// SMS sends allowed per number per hour.
pub const SMS_PER_NUMBER_PER_HOUR: u32 = 5;

/// The phone number of an account.
#[derive(Debug, Clone, Serialize)]
pub struct PhoneView {
    /// E.164 number, `None` when the account has none.
    pub phone: Option<String>,
    /// When it was verified.
    pub verified_at: Option<String>,
}

/// Normalises a phone number to E.164 (`+` and 8-15 digits, no leading 0).
/// Spaces, dots, dashes and parentheses are ignored; `00` is read as `+`.
pub fn normalize_phone(raw: &str) -> Result<String, AuthError> {
    let mut s: String = raw
        .trim()
        .chars()
        .filter(|c| !matches!(c, ' ' | '.' | '-' | '(' | ')'))
        .collect();
    if let Some(rest) = s.strip_prefix("00") {
        s = format!("+{rest}");
    }
    let digits = s.strip_prefix('+').unwrap_or("");
    let ok = (8..=15).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()) && !digits.starts_with('0');
    if ok {
        Ok(format!("+{digits}"))
    } else {
        Err(AuthError::invalid(
            "phone must be an international number such as +4915112345678",
        ))
    }
}

fn session_user(caller: &Authenticated) -> Result<(String, String), AuthError> {
    match (&caller.subject, caller.session_id()) {
        (Subject::User { collection, id }, Some(_)) => Ok((collection.clone(), id.clone())),
        _ => Err(AuthError::Forbidden(
            "phone numbers are managed by signed-in users".into(),
        )),
    }
}

impl AuthService {
    fn sms_hash(&self, purpose: &str, collection: &str, phone: &str, code: &str) -> String {
        self.keyring()
            .token_hash(&format!("sms_{purpose}"), &format!("{collection}:{phone}:{code}"))
    }

    /// The user owning `phone` in `collection`.
    pub(crate) async fn user_by_phone(&self, collection: &str, phone: &str) -> Result<Option<String>, AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT user_id FROM _forgebase_auth_phones WHERE collection = ?1 AND phone = ?2",
            vec![t(collection), t(phone)],
        )
        .await?;
        Ok(first(&r).map(|row| row.str("user_id")))
    }

    async fn send_sms_code(
        &self,
        coll: &str,
        phone: &str,
        purpose: &str,
        user_id: Option<&str>,
    ) -> Result<(), AuthError> {
        self.limit_key("sms_number", phone, SMS_PER_NUMBER_PER_HOUR, Duration::from_secs(3600))?;
        let code = crypto::random_digits(6)?;
        let ttl = self.config().email_otp_ttl.as_secs();
        let ts = self.now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "DELETE FROM _forgebase_auth_sms_codes WHERE expires_at < ?1",
            vec![i(ts - 86_400)],
        )
        .await?;
        // A new code replaces older unused ones.
        tx.exec(
            "UPDATE _forgebase_auth_sms_codes SET used_at = ?4
             WHERE collection = ?1 AND phone = ?2 AND purpose = ?3 AND used_at IS NULL",
            vec![t(coll), t(phone), t(purpose), i(ts)],
        )
        .await?;
        tx.exec(
            "INSERT INTO _forgebase_auth_sms_codes (code_hash, purpose, collection, phone, user_id, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            vec![
                t(self.sms_hash(purpose, coll, phone, &code)),
                t(purpose),
                t(coll),
                t(phone),
                ot(user_id.map(str::to_owned)),
                i(ts),
                i(ts + ttl as i64),
            ],
        )
        .await?;
        tx.commit().await?;
        let sms = Sms {
            to: phone.to_owned(),
            text: format!(
                "Your code is {code}. It expires in {} minutes. Never share it.",
                (ttl / 60).max(1)
            ),
            kind: if purpose == "login" {
                SmsKind::LoginCode
            } else {
                SmsKind::VerifyPhone
            },
            secret: code,
        };
        self.sms().send(&sms).await.map_err(|e| {
            tracing::warn!(to = %mask_phone(phone), error = %e, "SMS delivery failed");
            AuthError::Provider("could not send the SMS".into())
        })
    }

    /// Checks and consumes the latest code; wrong guesses count and burn it.
    async fn check_sms_code(
        &self,
        coll: &str,
        phone: &str,
        purpose: &str,
        code: &str,
        user_id: Option<&str>,
    ) -> Result<bool, AuthError> {
        let ts = self.now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        let r = tx
            .read(
                "SELECT code_hash, attempts, user_id FROM _forgebase_auth_sms_codes
                 WHERE collection = ?1 AND phone = ?2 AND purpose = ?3 AND used_at IS NULL AND expires_at > ?4
                 ORDER BY created_at DESC LIMIT 1",
                vec![t(coll), t(phone), t(purpose), i(ts)],
            )
            .await?;
        let Some(row) = first(&r) else { return Ok(false) };
        let stored = row.str("code_hash");
        let candidate = self.sms_hash(purpose, coll, phone, code.trim());
        let owner_ok = row.opt_str("user_id").as_deref() == user_id;
        if !crypto::ct_eq(candidate.as_bytes(), stored.as_bytes()) || !owner_ok {
            let attempts = row.i64("attempts") + 1;
            let burn = attempts >= self.config().otp_max_attempts;
            tx.exec(
                "UPDATE _forgebase_auth_sms_codes SET attempts = ?2, used_at = CASE WHEN ?3 THEN ?4 ELSE used_at END
                 WHERE code_hash = ?1",
                vec![t(stored), i(attempts), i(i64::from(burn)), i(ts)],
            )
            .await?;
            tx.commit().await?;
            return Ok(false);
        }
        let claimed = tx
            .exec(
                "UPDATE _forgebase_auth_sms_codes SET used_at = ?2 WHERE code_hash = ?1 AND used_at IS NULL",
                vec![t(stored), i(ts)],
            )
            .await?
            .rows_affected
            == 1;
        tx.commit().await?;
        Ok(claimed)
    }

    /// Sends a sign-in code to a phone number. Always answers the same.
    pub async fn request_phone_code(
        &self,
        ci: &ClientInfo,
        collection: Option<&str>,
        phone: &str,
    ) -> Result<(), AuthError> {
        self.limit_mail(ci)?;
        let (realm, coll) = self.request_collection(ci, collection).await?;
        realm.require(Method::Phone)?;
        self.turnstile_check(&realm, TurnstileAction::Otp, ci).await?;
        let phone = normalize_phone(phone)?;
        let user = self.user_by_phone(&coll.name, &phone).await?;
        let subject = user.as_ref().map(|id| Subject::User {
            collection: coll.name.clone(),
            id: id.clone(),
        });
        if user.is_some() || (realm.config.signup && realm.config.phone.signup) {
            self.send_sms_code(&coll.name, &phone, "login", None).await?;
        }
        self.audit(
            "sms_otp.requested",
            true,
            ci,
            "anonymous",
            subject.as_ref(),
            json!({"collection": coll.name, "phone": mask_phone(&phone)}),
        )
        .await
    }

    /// Signs in with a phone number and SMS code.
    pub async fn login_phone(
        &self,
        ci: &ClientInfo,
        collection: Option<&str>,
        phone: &str,
        code: &str,
        opts: &LoginOptions,
    ) -> Result<LoginOutcome, AuthError> {
        self.limit_attempt(ci)?;
        let (realm, coll) = self.request_collection(ci, collection).await?;
        realm.require(Method::Phone)?;
        let phone = normalize_phone(phone)?;
        let lock_key = format!("sms:user:{}:{phone}", coll.name);
        self.lockout_check(&lock_key).await?;
        if !self.check_sms_code(&coll.name, &phone, "login", code, None).await? {
            let locked = self.lockout_fail(&lock_key).await?;
            self.audit(
                "login",
                false,
                ci,
                "anonymous",
                None,
                json!({"collection": coll.name, "method": "sms", "reason": "invalid_code", "locked": locked}),
            )
            .await?;
            return Err(AuthError::InvalidCredentials);
        }
        self.lockout_clear(&lock_key).await?;
        let uid = match self.user_by_phone(&coll.name, &phone).await? {
            Some(u) => u,
            None => self.create_phone_user(ci, &realm, &coll, &phone).await?,
        };
        let subject = Subject::User {
            collection: coll.name.clone(),
            id: uid,
        };
        self.complete_first_factor(&subject, AuthMethod::Sms, ci, opts).await
    }

    /// Creates the account of a phone number on its first verified sign-in.
    async fn create_phone_user(
        &self,
        ci: &ClientInfo,
        realm: &RealmIdentity,
        coll: &Collection,
        phone: &str,
    ) -> Result<String, AuthError> {
        if !(realm.config.signup && realm.config.phone.signup) {
            return Err(AuthError::InvalidCredentials);
        }
        // A reserved placeholder address (keyed hash: the number cannot be
        // read back from it); the user can add a real email later.
        let tag = self
            .keyring()
            .token_hash("phone_placeholder", &format!("{}:{phone}", coll.name));
        let email = format!("phone.{}@phone.invalid", &tag[..24]);
        let password = crypto::random_token(32)?;
        self.hasher().prehash(&password).await?;
        let doc = json!({
            "email": email,
            "password": password,
            "verified": false,
            "token_key": crypto::random_token(24)?,
        });
        let rec = self
            .data()
            .create_record(&AccessContext::system(), &coll.name, doc)
            .await;
        let uid = match rec {
            Ok(r) => r.get("id").and_then(Json::as_str).unwrap_or_default().to_owned(),
            Err(e) => {
                let e = AuthError::from(e);
                // Lost a race to the same number: the winner's account.
                if is_unique_violation(&e)
                    && let Some(u) = self.user_by_phone(&coll.name, phone).await?
                {
                    return Ok(u);
                }
                return Err(e);
            }
        };
        let ts = self.now();
        let subject = Subject::User {
            collection: coll.name.clone(),
            id: uid.clone(),
        };
        let mut tx = Tx::begin(self.data().backend()).await?;
        let res = tx
            .exec(
                "INSERT INTO _forgebase_auth_phones (collection, phone, user_id, verified_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                vec![t(coll.name.clone()), t(phone), t(uid.clone()), i(ts)],
            )
            .await;
        if let Err(e) = res {
            drop(tx);
            let _ = self
                .data()
                .delete_record(&AccessContext::system(), &coll.name, &uid, None)
                .await;
            if is_unique_violation(&e)
                && let Some(u) = self.user_by_phone(&coll.name, phone).await?
            {
                return Ok(u);
            }
            return Err(e);
        }
        audit_in(
            &mut tx,
            "signup",
            true,
            ci,
            &subject.actor(),
            Some(&subject),
            json!({"collection": coll.name, "method": "sms"}),
        )
        .await?;
        tx.commit().await?;
        Ok(uid)
    }

    /// The caller's phone number.
    pub async fn phone_of(&self, caller: &Authenticated) -> Result<PhoneView, AuthError> {
        let (coll, uid) = session_user(caller)?;
        let r = read(
            self.data().backend(),
            "SELECT phone, verified_at FROM _forgebase_auth_phones WHERE collection = ?1 AND user_id = ?2",
            vec![t(coll), t(uid)],
        )
        .await?;
        Ok(match first(&r) {
            Some(row) => PhoneView {
                phone: Some(row.str("phone")),
                verified_at: Some(rfc3339(row.i64("verified_at"))),
            },
            None => PhoneView {
                phone: None,
                verified_at: None,
            },
        })
    }

    /// Sends a code confirming a number for the signed-in caller.
    pub async fn phone_add_start(&self, caller: &Authenticated, ci: &ClientInfo, phone: &str) -> Result<(), AuthError> {
        crate::service::reject_impersonated(caller)?;
        self.limit_mail(ci)?;
        let (coll, uid) = session_user(caller)?;
        self.realm_of(&coll)?.require(Method::Phone)?;
        let phone = normalize_phone(phone)?;
        self.send_sms_code(&coll, &phone, "verify_phone", Some(&uid)).await?;
        let subject = &caller.subject;
        self.audit(
            "phone.verification_sent",
            true,
            ci,
            &subject.actor(),
            Some(subject),
            json!({"phone": mask_phone(&phone)}),
        )
        .await
    }

    /// Confirms the code and sets the caller's number (replacing an older one).
    pub async fn phone_add_confirm(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        phone: &str,
        code: &str,
    ) -> Result<PhoneView, AuthError> {
        crate::service::reject_impersonated(caller)?;
        self.limit_attempt(ci)?;
        let (coll, uid) = session_user(caller)?;
        let phone = normalize_phone(phone)?;
        if !self
            .check_sms_code(&coll, &phone, "verify_phone", code, Some(&uid))
            .await?
        {
            return Err(AuthError::InvalidCredentials);
        }
        let ts = self.now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "DELETE FROM _forgebase_auth_phones WHERE collection = ?1 AND user_id = ?2",
            vec![t(coll.clone()), t(uid.clone())],
        )
        .await?;
        let res = tx
            .exec(
                "INSERT INTO _forgebase_auth_phones (collection, phone, user_id, verified_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                vec![t(coll.clone()), t(phone.clone()), t(uid.clone()), i(ts)],
            )
            .await;
        if let Err(e) = res {
            if is_unique_violation(&e) {
                return Err(AuthError::Conflict(
                    "this phone number belongs to another account".into(),
                ));
            }
            return Err(e);
        }
        audit_in(
            &mut tx,
            "phone.added",
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({"phone": mask_phone(&phone)}),
        )
        .await?;
        tx.commit().await?;
        self.phone_of(caller).await
    }

    /// Removes the caller's number (refused when it is the only way back in).
    pub async fn phone_remove(&self, caller: &Authenticated, ci: &ClientInfo) -> Result<(), AuthError> {
        crate::service::reject_impersonated(caller)?;
        let (coll, uid) = session_user(caller)?;
        if self.sole_way_in(&coll, &uid, None, None, true).await? {
            return Err(AuthError::Conflict(
                "this phone number is the only way to sign in to this account; add an email or passkey first".into(),
            ));
        }
        let mut tx = Tx::begin(self.data().backend()).await?;
        let n = tx
            .exec(
                "DELETE FROM _forgebase_auth_phones WHERE collection = ?1 AND user_id = ?2",
                vec![t(coll), t(uid)],
            )
            .await?
            .rows_affected;
        if n == 0 {
            return Err(AuthError::NotFound("no phone number on this account".into()));
        }
        audit_in(
            &mut tx,
            "phone.removed",
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({}),
        )
        .await?;
        tx.commit().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_numbers_normalise_to_e164() {
        assert_eq!(normalize_phone("+49 151 1234-5678").unwrap(), "+4915112345678");
        assert_eq!(normalize_phone("0049 (151) 12345678").unwrap(), "+4915112345678");
        assert_eq!(normalize_phone("+1.555.010.0199").unwrap(), "+15550100199");
        for bad in [
            "",
            "12345678",
            "+0123456789",
            "+12",
            "+1234567890123456",
            "+1555abc0100",
            "+1 555\n0100",
        ] {
            assert!(normalize_phone(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn masked_numbers_hide_the_middle() {
        assert_eq!(mask_phone("+4915112345678"), "+4915***78");
    }
}
