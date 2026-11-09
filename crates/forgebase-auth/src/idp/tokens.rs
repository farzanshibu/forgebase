//! The token endpoint and what it issues; userinfo, introspection
//! (RFC 7662) and revocation (RFC 7009).
//!
//! * **Access tokens** are ES256 JWTs (`typ: at+jwt`, RFC 9068) signed with
//!   the realm's key: `iss`, `sub` (user id, or the client id for client
//!   credentials), `aud` and `client_id`, `scope`, `iat`, `exp`, `jti`,
//!   `auth_time`, `acr`, `sid`, `fid` (token family) and `act` when issued in
//!   an impersonated session. Resource servers verify them with the JWKS or
//!   ask the introspection endpoint, which also checks revocation.
//! * **ID tokens** (`typ: JWT`): `iss`, `sub`, `aud`, `azp`, `iat`, `exp`,
//!   `auth_time`, `nonce`, `acr`, `amr`, `at_hash`, `sid` and the claims of
//!   the granted scopes.
//! * **Refresh tokens** are opaque, rotate on every use and belong to a token
//!   family; presenting a used one revokes the family. A family bound to a
//!   browser session ends with that session unless it was granted
//!   `offline_access`.
//! * **Authorization codes** are single use: a replayed code is refused and
//!   the family issued from its first redemption is revoked.

use std::collections::HashMap;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};

use super::clients::{ClientCreds, ClientRecord};
use super::keys::OidcKey;
use super::{
    GRANT_CLIENT_CREDENTIALS, GRANT_CODE, GRANT_DEVICE, GRANT_REFRESH, OAuthError, OIDC_SCOPES, parse_scope_param,
    scope_string, scopes_within,
};
use crate::crypto::{self, b64url};
use crate::service::{ClientInfo, audit_in};
use crate::store::{Tx, first, i, ot, read, t};
use crate::{AuthError, AuthService, Subject};

/// What a new token family is issued for.
#[derive(Debug, Clone)]
struct NewFamily {
    realm: String,
    grant_type: &'static str,
    user: Option<(String, String)>,
    scopes: Vec<String>,
    session_id: Option<String>,
    auth_time: Option<i64>,
    acr: Option<String>,
    amr: Vec<String>,
    impersonator: Option<String>,
    nonce: Option<String>,
}

/// An active access token.
#[derive(Debug, Clone)]
pub(crate) struct ActiveToken {
    pub(crate) claims: Json,
    pub(crate) scopes: Vec<String>,
    pub(crate) user: Option<(String, String)>,
}

fn pkce_s256(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

fn verifier_ok(s: &str) -> bool {
    (43..=128).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// `at_hash`: base64url of the left half of SHA-256 of the access token.
pub(crate) fn at_hash(access_token: &str) -> String {
    b64url(&Sha256::digest(access_token.as_bytes())[..16])
}

fn param<'a>(form: &'a HashMap<String, String>, k: &str) -> Option<&'a str> {
    form.get(k).map(String::as_str).filter(|v| !v.is_empty())
}

fn json_list(s: &str) -> Vec<String> {
    serde_json::from_str(s).unwrap_or_default()
}

fn parse_rfc3339(s: &str) -> Option<i64> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(time::OffsetDateTime::unix_timestamp)
}

impl AuthService {
    /// The token endpoint: client authentication, then the grant.
    pub(crate) async fn token_endpoint(
        &self,
        realm: &str,
        form: &HashMap<String, String>,
        creds: &ClientCreds,
        ci: &ClientInfo,
    ) -> Result<Json, OAuthError> {
        if self.realms().get(realm).is_none() {
            return Err(OAuthError::invalid_request("unknown realm"));
        }
        let gt = form
            .get("grant_type")
            .map(String::as_str)
            .ok_or_else(|| OAuthError::invalid_request("grant_type is required"))?;
        let gt: &'static str = match gt {
            GRANT_CODE => GRANT_CODE,
            GRANT_REFRESH => GRANT_REFRESH,
            GRANT_CLIENT_CREDENTIALS => GRANT_CLIENT_CREDENTIALS,
            GRANT_DEVICE => GRANT_DEVICE,
            _ => return Err(OAuthError::unsupported_grant_type("unsupported grant_type")),
        };
        let client = self.authenticate_client(realm, creds, ci).await?;
        if !client.allows_grant(gt) {
            return Err(OAuthError::unauthorized_client(format!(
                "this client may not use the {gt} grant"
            )));
        }
        let out = match gt {
            GRANT_CODE => self.grant_authorization_code(realm, &client, form, ci).await,
            GRANT_REFRESH => self.grant_refresh(realm, &client, form, ci).await,
            GRANT_CLIENT_CREDENTIALS => {
                self.grant_client_credentials(realm, &client, param(form, "scope"), ci)
                    .await
            }
            _ => {
                self.grant_device_code(realm, &client, param(form, "device_code"), ci)
                    .await
            }
        };
        if let Err(e) = &out
            && e.error != "authorization_pending"
            && e.error != "slow_down"
        {
            let detail = json!({"client_id": client.id, "grant_type": gt, "error": e.error});
            if let Err(err) = self
                .audit("oauth.token", false, ci, &format!("client:{}", client.id), None, detail)
                .await
            {
                tracing::warn!(error = %err, "could not audit a token failure");
            }
        }
        out
    }

    async fn grant_authorization_code(
        &self,
        realm: &str,
        client: &ClientRecord,
        form: &HashMap<String, String>,
        ci: &ClientInfo,
    ) -> Result<Json, OAuthError> {
        let code = param(form, "code").ok_or_else(|| OAuthError::invalid_request("code is required"))?;
        let key = self.active_oidc_key(realm).await?;
        let now = self.now();
        let hash = self.keyring().token_hash("oauth_code", code);
        let mut tx = Tx::begin(self.data().backend()).await?;
        let r = tx
            .read(
                "SELECT realm, client_id, redirect_uri, scopes, nonce, code_challenge, collection, user_id, session_id,
                        auth_time, acr, amr, impersonator, expires_at, used_at, family_id
                 FROM _forgebase_oauth_codes WHERE code_hash = ?1",
                vec![t(hash.clone())],
            )
            .await?;
        let Some(row) = first(&r) else {
            return Err(OAuthError::invalid_grant("invalid authorization code"));
        };
        if row.str("client_id") != client.id || row.str("realm") != realm {
            return Err(OAuthError::invalid_grant("invalid authorization code"));
        }
        let claimed = tx
            .exec(
                "UPDATE _forgebase_oauth_codes SET used_at = ?2 WHERE code_hash = ?1 AND used_at IS NULL",
                vec![t(hash.clone()), i(now)],
            )
            .await?
            .rows_affected
            == 1;
        let user = (row.str("collection"), row.str("user_id"));
        let subject = Subject::User {
            collection: user.0.clone(),
            id: user.1.clone(),
        };
        if !claimed {
            let fam = row.opt_str("family_id");
            if let Some(f) = &fam {
                revoke_family_in(&mut tx, f, "code_replay", now).await?;
            }
            audit_in(
                &mut tx,
                "oauth.code_replay",
                false,
                ci,
                &format!("client:{}", client.id),
                Some(&subject),
                json!({"client_id": client.id, "family_id": fam}),
            )
            .await?;
            tx.commit().await?;
            tracing::warn!(client_id = %client.id, "authorization code replay; token family revoked");
            return Err(OAuthError::invalid_grant(
                "authorization code already used; tokens issued from it are revoked",
            ));
        }
        // From here the code is burnt whatever the outcome.
        let fail = |msg: &str| OAuthError::invalid_grant(msg.to_owned());
        let check = (|| {
            if row.i64("expires_at") <= now {
                return Err(fail("authorization code expired"));
            }
            if param(form, "redirect_uri") != Some(row.str("redirect_uri").as_str()) {
                return Err(fail("redirect_uri does not match the authorization request"));
            }
            match (row.opt_str("code_challenge"), param(form, "code_verifier")) {
                (Some(ch), Some(v)) if verifier_ok(v) => {
                    if !crypto::ct_eq(pkce_s256(v).as_bytes(), ch.as_bytes()) {
                        return Err(fail("PKCE verification failed"));
                    }
                }
                (Some(_), _) => return Err(fail("PKCE verification failed")),
                (None, Some(_)) => return Err(fail("code_verifier sent but no code_challenge was")),
                (None, None) if client.is_public() => return Err(fail("PKCE is required")),
                (None, None) => {}
            }
            Ok(())
        })();
        if let Err(e) = check {
            tx.commit().await?;
            return Err(e);
        }
        if self.ensure_active(&subject).await.is_err() || !self.session_alive(&row.str("session_id")).await? {
            tx.commit().await?;
            return Err(fail("the session that authorized this code has ended"));
        }
        let fam = NewFamily {
            realm: realm.to_owned(),
            grant_type: GRANT_CODE,
            user: Some(user),
            scopes: json_list(&row.str("scopes")),
            session_id: Some(row.str("session_id")),
            auth_time: Some(row.i64("auth_time")),
            acr: Some(row.str("acr")),
            amr: json_list(&row.str("amr")),
            impersonator: row.opt_str("impersonator"),
            nonce: row.opt_str("nonce"),
        };
        let with_refresh = client.allows_grant(GRANT_REFRESH);
        let (fid, refresh) = self.create_family_in(&mut tx, client, &fam, with_refresh).await?;
        tx.exec(
            "UPDATE _forgebase_oauth_codes SET family_id = ?2 WHERE code_hash = ?1",
            vec![t(hash), t(fid.clone())],
        )
        .await?;
        audit_in(
            &mut tx,
            "oauth.token",
            true,
            ci,
            &format!("client:{}", client.id),
            Some(&subject),
            json!({"client_id": client.id, "grant_type": GRANT_CODE, "family_id": fid}),
        )
        .await?;
        tx.commit().await?;
        self.mint(&key, client, &fam, &fid, &fam.scopes, refresh).await
    }

    async fn grant_refresh(
        &self,
        realm: &str,
        client: &ClientRecord,
        form: &HashMap<String, String>,
        ci: &ClientInfo,
    ) -> Result<Json, OAuthError> {
        let token =
            param(form, "refresh_token").ok_or_else(|| OAuthError::invalid_request("refresh_token is required"))?;
        if token.len() > 256 {
            return Err(OAuthError::invalid_grant("invalid refresh token"));
        }
        let key = self.active_oidc_key(realm).await?;
        let now = self.now();
        let hash = self.keyring().token_hash("oauth_refresh", token);
        let mut tx = Tx::begin(self.data().backend()).await?;
        let r = tx
            .read(
                "SELECT r.family_id, r.generation, r.used_at, r.expires_at AS token_expires,
                        f.realm, f.client_id, f.collection, f.user_id, f.scopes, f.session_id, f.auth_time, f.acr,
                        f.amr, f.impersonator, f.expires_at, f.revoked_at
                 FROM _forgebase_oauth_refresh_tokens r JOIN _forgebase_oauth_families f ON f.id = r.family_id
                 WHERE r.token_hash = ?1",
                vec![t(hash.clone())],
            )
            .await?;
        let Some(row) = first(&r) else {
            return Err(OAuthError::invalid_grant("invalid refresh token"));
        };
        if row.str("client_id") != client.id || row.str("realm") != realm {
            return Err(OAuthError::invalid_grant("invalid refresh token"));
        }
        let fid = row.str("family_id");
        let user = match (row.opt_str("collection"), row.opt_str("user_id")) {
            (Some(c), Some(u)) => (c, u),
            _ => return Err(OAuthError::invalid_grant("invalid refresh token")),
        };
        let subject = Subject::User {
            collection: user.0.clone(),
            id: user.1.clone(),
        };
        let scopes = json_list(&row.str("scopes"));
        let narrowed = match param(form, "scope").map(parse_scope_param) {
            None => scopes.clone(),
            Some(Err(e)) => return Err(e),
            Some(Ok(want)) if scopes_within(&want, &scopes) && !want.is_empty() => want,
            Some(Ok(_)) => return Err(OAuthError::invalid_scope("scope exceeds the original grant")),
        };
        let claimed = row.opt_i64("used_at").is_none()
            && tx
                .exec(
                    "UPDATE _forgebase_oauth_refresh_tokens SET used_at = ?2 WHERE token_hash = ?1 AND used_at IS NULL",
                    vec![t(hash), i(now)],
                )
                .await?
                .rows_affected
                == 1;
        if !claimed {
            revoke_family_in(&mut tx, &fid, "refresh_token_reuse", now).await?;
            audit_in(
                &mut tx,
                "oauth.refresh.reuse_detected",
                false,
                ci,
                &format!("client:{}", client.id),
                Some(&subject),
                json!({"client_id": client.id, "family_id": fid, "generation": row.i64("generation")}),
            )
            .await?;
            tx.commit().await?;
            tracing::warn!(client_id = %client.id, family_id = %fid, "refresh token reuse; token family revoked");
            return Err(OAuthError::invalid_grant(
                "refresh token reuse detected; the token family is revoked",
            ));
        }
        if row.opt_i64("revoked_at").is_some() {
            tx.commit().await?;
            return Err(OAuthError::invalid_grant("the token family is revoked"));
        }
        if row.i64("expires_at") <= now || row.i64("token_expires") <= now {
            tx.commit().await?;
            return Err(OAuthError::invalid_grant("refresh token expired"));
        }
        let offline = scopes.iter().any(|s| s == "offline_access");
        let session_id = row.opt_str("session_id");
        let session_ended = match (&session_id, offline) {
            (Some(sid), false) => !self.session_alive(sid).await?,
            _ => false,
        };
        if session_ended || self.ensure_active(&subject).await.is_err() || !self.subject_exists(&subject).await? {
            revoke_family_in(&mut tx, &fid, "session_ended", now).await?;
            tx.commit().await?;
            return Err(OAuthError::invalid_grant("the session behind this grant has ended"));
        }
        let next = crypto::random_token(32)?;
        let generation = row.i64("generation") + 1;
        tx.exec(
            "INSERT INTO _forgebase_oauth_refresh_tokens (token_hash, family_id, generation, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            vec![
                t(self.keyring().token_hash("oauth_refresh", &next)),
                t(fid.clone()),
                i(generation),
                i(now),
                i(row.i64("expires_at")),
            ],
        )
        .await?;
        tx.exec(
            "DELETE FROM _forgebase_oauth_refresh_tokens WHERE family_id = ?1 AND generation < ?2",
            vec![t(fid.clone()), i(generation - 50)],
        )
        .await?;
        audit_in(
            &mut tx,
            "oauth.token",
            true,
            ci,
            &format!("client:{}", client.id),
            Some(&subject),
            json!({"client_id": client.id, "grant_type": GRANT_REFRESH, "family_id": fid, "generation": generation}),
        )
        .await?;
        tx.commit().await?;
        let fam = NewFamily {
            realm: realm.to_owned(),
            grant_type: GRANT_REFRESH,
            user: Some(user),
            scopes,
            session_id,
            auth_time: row.opt_i64("auth_time"),
            acr: row.opt_str("acr"),
            amr: json_list(&row.str("amr")),
            impersonator: row.opt_str("impersonator"),
            nonce: None,
        };
        self.mint(&key, client, &fam, &fid, &narrowed, Some(next)).await
    }

    async fn grant_client_credentials(
        &self,
        realm: &str,
        client: &ClientRecord,
        scope: Option<&str>,
        ci: &ClientInfo,
    ) -> Result<Json, OAuthError> {
        if client.is_public() {
            return Err(OAuthError::unauthorized_client(
                "public clients cannot use client_credentials",
            ));
        }
        let allowed: Vec<String> = client
            .scopes
            .iter()
            .filter(|s| !OIDC_SCOPES.contains(&s.as_str()))
            .cloned()
            .collect();
        let scopes = match scope.map(parse_scope_param) {
            None => allowed.clone(),
            Some(Err(e)) => return Err(e),
            Some(Ok(s)) => s,
        };
        if scopes.iter().any(|s| OIDC_SCOPES.contains(&s.as_str())) || !scopes_within(&scopes, &allowed) {
            return Err(OAuthError::invalid_scope(
                "client_credentials may only request the client's ForgeBase API scopes",
            ));
        }
        let key = self.active_oidc_key(realm).await?;
        let fam = NewFamily {
            realm: realm.to_owned(),
            grant_type: GRANT_CLIENT_CREDENTIALS,
            user: None,
            scopes: scopes.clone(),
            session_id: None,
            auth_time: None,
            acr: None,
            amr: Vec::new(),
            impersonator: None,
            nonce: None,
        };
        let mut tx = Tx::begin(self.data().backend()).await?;
        let (fid, _) = self.create_family_in(&mut tx, client, &fam, false).await?;
        audit_in(
            &mut tx,
            "oauth.token",
            true,
            ci,
            &format!("client:{}", client.id),
            None,
            json!({"client_id": client.id, "grant_type": GRANT_CLIENT_CREDENTIALS, "family_id": fid}),
        )
        .await?;
        tx.commit().await?;
        self.mint(&key, client, &fam, &fid, &scopes, None).await
    }

    async fn grant_device_code(
        &self,
        realm: &str,
        client: &ClientRecord,
        device_code: Option<&str>,
        ci: &ClientInfo,
    ) -> Result<Json, OAuthError> {
        let code = device_code.ok_or_else(|| OAuthError::invalid_request("device_code is required"))?;
        let key = self.active_oidc_key(realm).await?;
        let now = self.now();
        let hash = self.keyring().token_hash("oauth_device_code", code);
        let mut tx = Tx::begin(self.data().backend()).await?;
        let r = tx
            .read(
                "SELECT realm, client_id, scopes, status, collection, user_id, session_id, auth_time, acr, amr,
                        interval_secs, last_poll_at, expires_at, family_id
                 FROM _forgebase_oauth_device_codes WHERE device_code_hash = ?1",
                vec![t(hash.clone())],
            )
            .await?;
        let Some(row) = first(&r) else {
            return Err(OAuthError::invalid_grant("invalid device code"));
        };
        if row.str("client_id") != client.id || row.str("realm") != realm {
            return Err(OAuthError::invalid_grant("invalid device code"));
        }
        if row.i64("expires_at") <= now {
            return Err(OAuthError::expired_token("the device code has expired; start again"));
        }
        let status = row.str("status");
        match status.as_str() {
            "used" => return Err(OAuthError::invalid_grant("the device code was already used")),
            "denied" => return Err(OAuthError::access_denied("the user denied the request")),
            _ => {}
        }
        let interval = row.i64("interval_secs");
        if row.opt_i64("last_poll_at").is_some_and(|last| now - last < interval) {
            tx.exec(
                "UPDATE _forgebase_oauth_device_codes SET interval_secs = interval_secs + 5, last_poll_at = ?2
                 WHERE device_code_hash = ?1",
                vec![t(hash), i(now)],
            )
            .await?;
            tx.commit().await?;
            return Err(OAuthError::slow_down(format!(
                "polling too fast; wait at least {} seconds",
                interval + 5
            )));
        }
        if status == "pending" {
            tx.exec(
                "UPDATE _forgebase_oauth_device_codes SET last_poll_at = ?2 WHERE device_code_hash = ?1",
                vec![t(hash), i(now)],
            )
            .await?;
            tx.commit().await?;
            return Err(OAuthError::authorization_pending("the user has not answered yet"));
        }
        let claimed = tx
            .exec(
                "UPDATE _forgebase_oauth_device_codes SET status = 'used', last_poll_at = ?2
                 WHERE device_code_hash = ?1 AND status = 'approved'",
                vec![t(hash.clone()), i(now)],
            )
            .await?
            .rows_affected
            == 1;
        let (Some(coll), Some(uid)) = (row.opt_str("collection"), row.opt_str("user_id")) else {
            return Err(OAuthError::invalid_grant("invalid device code"));
        };
        if !claimed {
            return Err(OAuthError::invalid_grant("the device code was already used"));
        }
        let subject = Subject::User {
            collection: coll.clone(),
            id: uid.clone(),
        };
        if self.ensure_active(&subject).await.is_err() {
            tx.commit().await?;
            return Err(OAuthError::invalid_grant("the account cannot sign in"));
        }
        let fam = NewFamily {
            realm: realm.to_owned(),
            grant_type: GRANT_DEVICE,
            user: Some((coll, uid)),
            scopes: json_list(&row.str("scopes")),
            session_id: row.opt_str("session_id"),
            auth_time: row.opt_i64("auth_time"),
            acr: row.opt_str("acr"),
            amr: json_list(&row.str("amr")),
            impersonator: None,
            nonce: None,
        };
        let with_refresh = client.allows_grant(GRANT_REFRESH);
        let (fid, refresh) = self.create_family_in(&mut tx, client, &fam, with_refresh).await?;
        tx.exec(
            "UPDATE _forgebase_oauth_device_codes SET family_id = ?2 WHERE device_code_hash = ?1",
            vec![t(hash), t(fid.clone())],
        )
        .await?;
        audit_in(
            &mut tx,
            "oauth.token",
            true,
            ci,
            &format!("client:{}", client.id),
            Some(&subject),
            json!({"client_id": client.id, "grant_type": GRANT_DEVICE, "family_id": fid}),
        )
        .await?;
        tx.commit().await?;
        self.mint(&key, client, &fam, &fid, &fam.scopes, refresh).await
    }

    async fn create_family_in(
        &self,
        tx: &mut Tx,
        client: &ClientRecord,
        fam: &NewFamily,
        with_refresh: bool,
    ) -> Result<(String, Option<String>), AuthError> {
        let now = self.now();
        let cfg = &self.config().idp;
        let fid = crypto::random_id(24)?;
        let lifetime = match fam.user {
            Some(_) => client
                .refresh_token_ttl
                .unwrap_or(cfg.refresh_token_ttl.as_secs() as i64),
            None => self.access_ttl(client),
        };
        let expires_at = now + lifetime;
        tx.exec(
            "DELETE FROM _forgebase_oauth_revoked_tokens WHERE expires_at < ?1",
            vec![i(now)],
        )
        .await?;
        tx.exec(
            "INSERT INTO _forgebase_oauth_families
                (id, realm, client_id, grant_type, collection, user_id, scopes, session_id, auth_time, acr, amr,
                 impersonator, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            vec![
                t(fid.clone()),
                t(fam.realm.clone()),
                t(client.id.clone()),
                t(fam.grant_type),
                ot(fam.user.as_ref().map(|u| u.0.clone())),
                ot(fam.user.as_ref().map(|u| u.1.clone())),
                t(serde_json::to_string(&fam.scopes).unwrap_or_default()),
                ot(fam.session_id.clone()),
                fam.auth_time.map_or(forgebase_data::db::Value::Null, i),
                ot(fam.acr.clone()),
                t(serde_json::to_string(&fam.amr).unwrap_or_default()),
                ot(fam.impersonator.clone()),
                i(now),
                i(expires_at),
            ],
        )
        .await?;
        let refresh = if with_refresh && fam.user.is_some() {
            let tok = crypto::random_token(32)?;
            tx.exec(
                "INSERT INTO _forgebase_oauth_refresh_tokens (token_hash, family_id, generation, created_at, expires_at)
                 VALUES (?1, ?2, 1, ?3, ?4)",
                vec![
                    t(self.keyring().token_hash("oauth_refresh", &tok)),
                    t(fid.clone()),
                    i(now),
                    i(expires_at),
                ],
            )
            .await?;
            Some(tok)
        } else {
            None
        };
        Ok((fid, refresh))
    }

    fn access_ttl(&self, client: &ClientRecord) -> i64 {
        client
            .access_token_ttl
            .unwrap_or(self.config().idp.access_token_ttl.as_secs() as i64)
    }

    /// Signs the access token (and ID token when `openid` was granted) and
    /// builds the token response.
    async fn mint(
        &self,
        key: &OidcKey,
        client: &ClientRecord,
        fam: &NewFamily,
        fid: &str,
        scopes: &[String],
        refresh: Option<String>,
    ) -> Result<Json, OAuthError> {
        let now = self.now();
        let ttl = self.access_ttl(client);
        let iss = self.oidc_issuer(&fam.realm);
        let sub = fam.user.as_ref().map_or(client.id.clone(), |u| u.1.clone());
        let mut at = json!({
            "iss": iss,
            "sub": sub,
            "aud": client.id,
            "client_id": client.id,
            "scope": scope_string(scopes),
            "iat": now,
            "exp": now + ttl,
            "jti": crypto::random_id(24)?,
            "fid": fid,
            "realm": fam.realm,
        });
        if let Some(a) = fam.auth_time {
            at["auth_time"] = json!(a);
        }
        if let Some(a) = &fam.acr {
            at["acr"] = json!(a);
        }
        if let Some(sid) = &fam.session_id {
            at["sid"] = json!(self.sid_claim(sid));
        }
        if let Some(admin) = &fam.impersonator {
            at["act"] = json!({"sub": admin});
        }
        let access_token = key.sign("at+jwt", &at)?;
        let mut out = json!({
            "access_token": access_token,
            "token_type": "Bearer",
            "expires_in": ttl,
            "scope": scope_string(scopes),
        });
        if let Some(r) = refresh {
            out["refresh_token"] = json!(r);
        }
        if let Some((coll, uid)) = &fam.user
            && scopes.iter().any(|s| s == "openid")
        {
            let mut id = json!({
                "iss": iss,
                "sub": uid,
                "aud": client.id,
                "azp": client.id,
                "iat": now,
                "exp": now + self.config().idp.id_token_ttl.as_secs() as i64,
                "at_hash": at_hash(&access_token),
                "amr": fam.amr,
            });
            if let Some(a) = fam.auth_time {
                id["auth_time"] = json!(a);
            }
            if let Some(a) = &fam.acr {
                id["acr"] = json!(a);
            }
            if let Some(n) = &fam.nonce {
                id["nonce"] = json!(n);
            }
            if let Some(sid) = &fam.session_id {
                id["sid"] = json!(self.sid_claim(sid));
            }
            if let Some(admin) = &fam.impersonator {
                id["act"] = json!({"sub": admin});
            }
            let claims = self.user_claims(coll, uid, scopes).await?;
            if let Some(obj) = id.as_object_mut() {
                for (k, v) in claims {
                    obj.entry(k).or_insert(v);
                }
            }
            out["id_token"] = json!(key.sign("JWT", &id)?);
        }
        Ok(out)
    }

    /// The claims of the granted scopes (OpenID Connect Core §5.4).
    pub(crate) async fn user_claims(
        &self,
        collection: &str,
        user_id: &str,
        scopes: &[String],
    ) -> Result<Map<String, Json>, AuthError> {
        let has = |s: &str| scopes.iter().any(|x| x == s);
        let rec = self.user_view(collection, user_id).await?;
        let mut out = Map::new();
        out.insert("sub".into(), json!(user_id));
        if has("email") {
            if let Some(e) = rec.get("email").filter(|e| e.is_string()) {
                out.insert("email".into(), e.clone());
            }
            out.insert(
                "email_verified".into(),
                json!(rec.get("verified").and_then(Json::as_bool).unwrap_or(false)),
            );
        }
        if has("profile") {
            for (claim, field) in [
                ("name", "name"),
                ("preferred_username", "username"),
                ("picture", "avatar"),
            ] {
                if let Some(v) = rec.get(field).and_then(Json::as_str).filter(|s| !s.is_empty()) {
                    out.insert(claim.into(), json!(v));
                }
            }
            if let Some(ts) = rec.get("updated").and_then(Json::as_str).and_then(parse_rfc3339) {
                out.insert("updated_at".into(), json!(ts));
            }
        }
        if has("phone") {
            let r = read(
                self.data().backend(),
                "SELECT phone FROM _forgebase_auth_phones WHERE collection = ?1 AND user_id = ?2",
                vec![t(collection), t(user_id)],
            )
            .await?;
            if let Some(row) = first(&r) {
                out.insert("phone_number".into(), json!(row.str("phone")));
                out.insert("phone_number_verified".into(), json!(true));
            }
        }
        Ok(out)
    }

    /// Verifies an access token of `realm` and checks it is still active:
    /// signature, `typ`, issuer, expiry, token family, single revocation, the
    /// browser session (unless `offline_access`) and the account.
    pub(crate) async fn active_access_token(&self, realm: &str, token: &str) -> Result<Option<ActiveToken>, AuthError> {
        let Some(v) = self.verify_idp_jwt(realm, token).await? else {
            return Ok(None);
        };
        let c = &v.claims;
        let now = self.now();
        if v.typ != "at+jwt"
            || c["iss"] != self.oidc_issuer(realm).as_str()
            || c["realm"] != realm
            || c["exp"].as_i64().is_none_or(|e| e <= now)
            || c["iat"].as_i64().is_none_or(|t| t > now + 60)
        {
            return Ok(None);
        }
        let (Some(fid), Some(jti)) = (c["fid"].as_str(), c["jti"].as_str()) else {
            return Ok(None);
        };
        let r = read(
            self.data().backend(),
            "SELECT f.collection, f.user_id, f.scopes, f.session_id, f.revoked_at, f.client_id,
                    (SELECT 1 FROM _forgebase_oauth_revoked_tokens WHERE jti = ?2) AS revoked_jti
             FROM _forgebase_oauth_families f WHERE f.id = ?1",
            vec![t(fid), t(jti)],
        )
        .await?;
        let Some(row) = first(&r) else {
            return Ok(None);
        };
        if row.opt_i64("revoked_at").is_some()
            || row.opt_i64("revoked_jti").is_some()
            || c["client_id"] != row.str("client_id").as_str()
        {
            return Ok(None);
        }
        let family_scopes = json_list(&row.str("scopes"));
        let offline = family_scopes.iter().any(|s| s == "offline_access");
        if let Some(sid) = row.opt_str("session_id")
            && !offline
            && !self.session_alive(&sid).await?
        {
            return Ok(None);
        }
        let user = match (row.opt_str("collection"), row.opt_str("user_id")) {
            (Some(coll), Some(id)) => {
                let s = Subject::User {
                    collection: coll.clone(),
                    id: id.clone(),
                };
                if self.ensure_active(&s).await.is_err() {
                    return Ok(None);
                }
                Some((coll, id))
            }
            _ => None,
        };
        let scopes = c["scope"]
            .as_str()
            .map(|s| s.split(' ').filter(|x| !x.is_empty()).map(str::to_owned).collect())
            .unwrap_or_default();
        Ok(Some(ActiveToken {
            claims: v.claims,
            scopes,
            user,
        }))
    }

    /// The userinfo endpoint: claims of the token's scopes.
    pub(crate) async fn userinfo(&self, realm: &str, token: &str) -> Result<Json, OAuthError> {
        let Some(at) = self.active_access_token(realm, token).await? else {
            return Err(OAuthError::invalid_token(
                "the access token is invalid, expired or revoked",
            ));
        };
        let Some((coll, uid)) = &at.user else {
            return Err(OAuthError::insufficient_scope("this token does not belong to a user"));
        };
        if !at.scopes.iter().any(|s| s == "openid") {
            return Err(OAuthError::insufficient_scope("userinfo needs the openid scope"));
        }
        let claims = self
            .user_claims(coll, uid, &at.scopes)
            .await
            .map_err(|_| OAuthError::invalid_token("the user no longer exists"))?;
        Ok(Json::Object(claims))
    }

    /// Token introspection (RFC 7662) for an authenticated confidential
    /// client of the realm. Unknown, expired, revoked and foreign tokens are
    /// `{"active": false}`.
    pub(crate) async fn introspect(
        &self,
        realm: &str,
        client: &ClientRecord,
        token: &str,
        hint: Option<&str>,
    ) -> Result<Json, OAuthError> {
        if client.is_public() {
            return Err(OAuthError::invalid_client("public clients cannot introspect tokens"));
        }
        let inactive = json!({"active": false});
        let try_access = || async {
            if !super::keys::looks_like_idp_jwt(token) {
                return Ok::<_, AuthError>(None);
            }
            Ok(self.active_access_token(realm, token).await?.map(|at| {
                let c = at.claims;
                let mut out = json!({
                    "active": true,
                    "token_type": "Bearer",
                    "scope": c["scope"],
                    "client_id": c["client_id"],
                    "sub": c["sub"],
                    "aud": c["aud"],
                    "iss": c["iss"],
                    "exp": c["exp"],
                    "iat": c["iat"],
                    "jti": c["jti"],
                });
                for k in ["auth_time", "acr", "sid", "act"] {
                    if !c[k].is_null() {
                        out[k] = c[k].clone();
                    }
                }
                out
            }))
        };
        let try_refresh = || async {
            if token.len() > 256 {
                return Ok::<_, AuthError>(None);
            }
            let r = read(
                self.data().backend(),
                "SELECT r.used_at, r.expires_at, r.created_at, f.realm, f.client_id, f.user_id, f.scopes,
                        f.revoked_at, f.session_id
                 FROM _forgebase_oauth_refresh_tokens r JOIN _forgebase_oauth_families f ON f.id = r.family_id
                 WHERE r.token_hash = ?1",
                vec![t(self.keyring().token_hash("oauth_refresh", token))],
            )
            .await?;
            let Some(row) = first(&r) else {
                return Ok(None);
            };
            let now = self.now();
            if row.str("realm") != realm
                || row.opt_i64("used_at").is_some()
                || row.opt_i64("revoked_at").is_some()
                || row.i64("expires_at") <= now
            {
                return Ok(None);
            }
            let scopes = json_list(&row.str("scopes"));
            if let Some(sid) = row.opt_str("session_id")
                && !scopes.iter().any(|s| s == "offline_access")
                && !self.session_alive(&sid).await?
            {
                return Ok(None);
            }
            Ok(Some(json!({
                "active": true,
                "token_type": "refresh_token",
                "scope": scope_string(&scopes),
                "client_id": row.str("client_id"),
                "sub": row.opt_str("user_id"),
                "iss": self.oidc_issuer(realm),
                "exp": row.i64("expires_at"),
                "iat": row.i64("created_at"),
            })))
        };
        let found = if hint == Some("refresh_token") {
            match try_refresh().await? {
                Some(v) => Some(v),
                None => try_access().await?,
            }
        } else {
            match try_access().await? {
                Some(v) => Some(v),
                None => try_refresh().await?,
            }
        };
        Ok(found.unwrap_or(inactive))
    }

    /// Token revocation (RFC 7009): a refresh token revokes its family, an
    /// access token is revoked alone. Tokens of other clients and unknown
    /// tokens are ignored (the answer is the same).
    pub(crate) async fn revoke_token(
        &self,
        realm: &str,
        client: &ClientRecord,
        token: &str,
        ci: &ClientInfo,
    ) -> Result<(), OAuthError> {
        let now = self.now();
        if super::keys::looks_like_idp_jwt(token) {
            if let Some(v) = self.verify_idp_jwt(realm, token).await?
                && v.typ == "at+jwt"
                && v.claims["client_id"] == client.id.as_str()
                && let (Some(jti), Some(fid), Some(exp)) = (
                    v.claims["jti"].as_str(),
                    v.claims["fid"].as_str(),
                    v.claims["exp"].as_i64(),
                )
                && exp > now
            {
                let mut tx = Tx::begin(self.data().backend()).await?;
                tx.exec(
                    "INSERT OR IGNORE INTO _forgebase_oauth_revoked_tokens (jti, family_id, expires_at) VALUES (?1, ?2, ?3)",
                    vec![t(jti), t(fid), i(exp)],
                )
                .await?;
                audit_in(
                    &mut tx,
                    "oauth.revoke",
                    true,
                    ci,
                    &format!("client:{}", client.id),
                    None,
                    json!({"client_id": client.id, "token_type": "access_token", "family_id": fid}),
                )
                .await?;
                tx.commit().await?;
            }
            return Ok(());
        }
        if token.len() > 256 {
            return Ok(());
        }
        let r = read(
            self.data().backend(),
            "SELECT r.family_id, f.client_id, f.realm FROM _forgebase_oauth_refresh_tokens r
             JOIN _forgebase_oauth_families f ON f.id = r.family_id WHERE r.token_hash = ?1",
            vec![t(self.keyring().token_hash("oauth_refresh", token))],
        )
        .await?;
        if let Some(row) = first(&r)
            && row.str("client_id") == client.id
            && row.str("realm") == realm
        {
            let fid = row.str("family_id");
            let mut tx = Tx::begin(self.data().backend()).await?;
            revoke_family_in(&mut tx, &fid, "revoked", now).await?;
            audit_in(
                &mut tx,
                "oauth.revoke",
                true,
                ci,
                &format!("client:{}", client.id),
                None,
                json!({"client_id": client.id, "token_type": "refresh_token", "family_id": fid}),
            )
            .await?;
            tx.commit().await?;
        }
        Ok(())
    }
}

async fn revoke_family_in(tx: &mut Tx, family_id: &str, reason: &str, now: i64) -> Result<(), AuthError> {
    tx.exec(
        "UPDATE _forgebase_oauth_families SET revoked_at = ?2, revoked_reason = ?3 WHERE id = ?1 AND revoked_at IS NULL",
        vec![t(family_id), i(now), t(reason)],
    )
    .await?;
    Ok(())
}

/// Decodes a JWT payload without verification (tests and diagnostics).
pub fn decode_claims_unverified(token: &str) -> Option<Json> {
    let p = token.split('.').nth(1)?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p).ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_and_at_hash_follow_the_rfcs() {
        // RFC 7636 appendix B.
        assert_eq!(
            pkce_s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert!(verifier_ok("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"));
        assert!(!verifier_ok("short"));
        assert_eq!(at_hash("x").len(), 22);
    }
}
