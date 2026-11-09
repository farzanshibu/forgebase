//! Browser side of the identity provider: the authorization endpoint,
//! single sign-on sessions, consent and remembered grants, the device
//! verification page (RFC 8628) and RP-initiated logout.
//!
//! **Single sign-on.** A realm has one browser session: the ForgeBase cookie
//! session (`fb_session`) of a user of that realm. Every Client App of the
//! realm that sends the browser to `/authorize` reuses it, so the user signs
//! in once. `prompt=none` never shows a page (`login_required`,
//! `consent_required`); `prompt=login` and `max_age` demand a sign-in newer
//! than the request; ending the session (RP-initiated logout or
//! `/api/auth/logout`) ends every token family bound to it that was not
//! granted `offline_access`.
//!
//! **Errors.** An unknown client or a redirect URI that is not registered
//! (exact match) is shown as an error page and never redirected; every other
//! error goes back to the client's redirect URI with `error`, `state` and
//! `iss` (RFC 9207).

use std::collections::HashMap;

use serde::Serialize;
use serde_json::{Value as Json, json};

use super::clients::{ClientRecord, client_brief};
use super::{
    GRANT_CODE, GRANT_DEVICE, OAuthError, acr_of, amr_of, parse_scope_param, scope_description, scopes_within,
};
use crate::crypto;
use crate::service::{ClientInfo, audit_in};
use crate::store::{Tx, first, i, ot, read, rfc3339, rows, t};
use crate::{AuthError, AuthService, Authenticated, Subject};

/// Alphabet of user codes: consonants only (no vowels, so no words; no
/// look-alike digits), RFC 8628 §6.1.
const USER_CODE_ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";

/// The ForgeBase browser session of a realm user.
#[derive(Debug, Clone)]
pub(crate) struct SsoSession {
    pub(crate) id: String,
    pub(crate) collection: String,
    pub(crate) user_id: String,
    pub(crate) auth_time: i64,
    pub(crate) mfa: bool,
    pub(crate) method: String,
    pub(crate) impersonator: Option<String>,
}

impl SsoSession {
    pub(crate) fn subject(&self) -> Subject {
        Subject::User {
            collection: self.collection.clone(),
            id: self.user_id.clone(),
        }
    }

    fn amr(&self) -> Vec<String> {
        amr_of(&self.method, self.mfa)
    }
}

/// The consent screen: what a client asks for.
#[derive(Debug, Clone, Serialize)]
pub struct ConsentPrompt {
    /// Always `true` (lets a JSON caller tell this from other answers).
    pub consent_required: bool,
    /// Handle of the waiting authorization request.
    pub request_id: String,
    /// Realm.
    pub realm: String,
    /// The client (`client_id`, `name`).
    pub client: Json,
    /// Requested scopes with a description each.
    pub scopes: Vec<Json>,
    /// CSRF token to send back with the decision.
    pub csrf_token: String,
    /// Where to POST the decision.
    pub action: String,
}

/// A remembered consent.
#[derive(Debug, Clone, Serialize)]
pub struct ConsentGrant {
    /// Consent id.
    pub id: String,
    /// Client id.
    pub client_id: String,
    /// Client name.
    pub client_name: String,
    /// Realm.
    pub realm: String,
    /// Granted scopes.
    pub scopes: Vec<String>,
    /// First granted.
    pub created_at: String,
    /// Last extended.
    pub updated_at: String,
}

/// Answer of the device authorization endpoint (RFC 8628 §3.2).
#[derive(Debug, Clone, Serialize)]
pub struct DeviceAuthorization {
    /// Secret the device polls the token endpoint with.
    pub device_code: String,
    /// Code the user types on the verification page (`XXXX-XXXX`).
    pub user_code: String,
    /// Verification page.
    pub verification_uri: String,
    /// Verification page with the code filled in.
    pub verification_uri_complete: String,
    /// Seconds until the codes expire.
    pub expires_in: u64,
    /// Minimum seconds between polls.
    pub interval: u64,
}

/// What the authorization endpoint answers.
#[derive(Debug)]
pub(crate) enum AuthorizeOutcome {
    /// 302 to the client (code or error) or to the login page.
    Redirect(String),
    /// Show the consent screen.
    Consent(ConsentPrompt),
    /// Show an error page; never redirect (unknown client, bad redirect URI).
    ErrorPage(OAuthError),
}

/// A pending device authorization as the verification page shows it.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct DeviceInfo {
    pub(crate) client: Json,
    pub(crate) scopes: Vec<Json>,
    pub(crate) user_code: String,
}

/// What RP-initiated logout answers.
#[derive(Debug)]
pub(crate) enum LogoutOutcome {
    /// Ask the user to confirm (no matching `id_token_hint`, not a confirmed POST).
    Confirm {
        /// CSRF token of the session, for the confirmation form.
        csrf_token: String,
    },
    /// Signed out (`ended` = a session was ended); redirect when given.
    Done { redirect: Option<String>, ended: bool },
    /// Bad request; show an error page.
    ErrorPage(OAuthError),
}

/// Validated authorization request.
#[derive(Debug, Clone)]
struct Validated {
    client: ClientRecord,
    redirect_uri: String,
    scopes: Vec<String>,
    state: Option<String>,
    nonce: Option<String>,
    code_challenge: Option<String>,
    prompt_none: bool,
    prompt_login: bool,
    prompt_consent: bool,
    max_age: Option<i64>,
}

/// Appends query parameters to a registered URI.
pub(crate) fn with_params(base: &str, params: &[(&str, &str)]) -> String {
    match url::Url::parse(base) {
        Ok(mut u) => {
            {
                let mut q = u.query_pairs_mut();
                for (k, v) in params {
                    q.append_pair(k, v);
                }
            }
            u.to_string()
        }
        Err(_) => base.to_owned(),
    }
}

fn pkce_ok(s: &str) -> bool {
    (43..=128).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// Upper case without separators; `None` when it cannot be a user code.
pub(crate) fn normalize_user_code(s: &str) -> Option<String> {
    let out: String = s
        .chars()
        .filter(|c| !matches!(c, '-' | ' '))
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (out.len() == 8 && out.bytes().all(|b| USER_CODE_ALPHABET.contains(&b))).then_some(out)
}

fn scope_list(scopes: &[String]) -> Vec<Json> {
    scopes
        .iter()
        .map(|s| json!({"scope": s, "description": scope_description(s)}))
        .collect()
}

impl AuthService {
    /// Issuer identifier of a realm: `<public_url>/api/auth/oidc/<realm>`.
    pub fn oidc_issuer(&self, realm: &str) -> String {
        format!(
            "{}/api/auth/oidc/{realm}",
            self.config().public_url.trim_end_matches('/')
        )
    }

    /// The `sid` claim of a session (keyed hash; the session id stays secret).
    pub(crate) fn sid_claim(&self, session_id: &str) -> String {
        self.keyring().token_hash("oidc_sid", session_id)[..32].to_owned()
    }

    /// Resolves the `fb_session` cookie into a session of a user of `realm`.
    /// Sessions of another realm, administrators, revoked, expired or idle
    /// sessions and inactive accounts are no session.
    pub(crate) async fn sso_session(&self, realm: &str, cookie: Option<&str>) -> Result<Option<SsoSession>, AuthError> {
        let Some(cookie) = cookie.filter(|c| !c.is_empty() && c.len() <= 256) else {
            return Ok(None);
        };
        let hash = self.keyring().token_hash("cookie", cookie);
        let r = read(
            self.data().backend(),
            "SELECT id, subject_type, collection, subject_id, auth_method, mfa, created_at, revoked_at, expires_at,
                    last_seen_at, impersonator
             FROM _forgebase_auth_sessions WHERE cookie_hash = ?1",
            vec![t(hash)],
        )
        .await?;
        let Some(row) = first(&r) else {
            return Ok(None);
        };
        let now = self.now();
        let idle = self.config().session_idle_timeout.as_secs() as i64;
        if row.opt_i64("revoked_at").is_some()
            || row.i64("expires_at") <= now
            || row.i64("last_seen_at") + idle <= now
            || row.str("subject_type") != "user"
        {
            return Ok(None);
        }
        let collection = row.str("collection");
        if self.realms().realm_of_collection(&collection) != realm {
            return Ok(None);
        }
        let s = SsoSession {
            id: row.str("id"),
            collection,
            user_id: row.str("subject_id"),
            auth_time: row.i64("created_at"),
            mfa: row.i64("mfa") == 1,
            method: row.str("auth_method"),
            impersonator: row.opt_str("impersonator"),
        };
        let subject = s.subject();
        if self.ensure_active(&subject).await.is_err() || !self.subject_exists(&subject).await? {
            return Ok(None);
        }
        Ok(Some(s))
    }

    /// Whether a ForgeBase session is still alive (not revoked, not past its
    /// absolute expiry).
    pub(crate) async fn session_alive(&self, session_id: &str) -> Result<bool, AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT revoked_at, expires_at FROM _forgebase_auth_sessions WHERE id = ?1",
            vec![t(session_id)],
        )
        .await?;
        Ok(first(&r).is_some_and(|row| row.opt_i64("revoked_at").is_none() && row.i64("expires_at") > self.now()))
    }

    pub(crate) fn login_redirect(&self, realm: &str, return_to: &str) -> String {
        let base = self
            .config()
            .idp
            .login_url
            .clone()
            .unwrap_or_else(|| format!("{}/auth/login", self.config().public_url.trim_end_matches('/')));
        with_params(&base, &[("realm", realm), ("return_to", return_to)])
    }

    /// Checks client and redirect URI (failures: error page), then every
    /// other parameter (failures: redirect to the client).
    async fn validate_authorize(
        &self,
        realm: &str,
        raw: &HashMap<String, String>,
    ) -> Result<Validated, Box<AuthorizeOutcome>> {
        let get = |k: &str| raw.get(k).map(String::as_str).filter(|v| !v.is_empty());
        let client = match get("client_id") {
            Some(id) => self
                .load_client(id)
                .await
                .map_err(|e| Box::new(AuthorizeOutcome::ErrorPage(e.into())))?,
            None => None,
        };
        let Some(client) = client.filter(|c| c.realm == realm && !c.disabled) else {
            return Err(Box::new(AuthorizeOutcome::ErrorPage(OAuthError::invalid_client(
                "unknown client_id for this realm",
            ))));
        };
        let Some(redirect_uri) = get("redirect_uri").filter(|u| client.redirect_uris.iter().any(|r| r == u)) else {
            return Err(Box::new(AuthorizeOutcome::ErrorPage(OAuthError::invalid_request(
                "redirect_uri is missing or not registered for this client (exact match required)",
            ))));
        };
        let redirect_uri = redirect_uri.to_owned();
        let state = get("state").map(str::to_owned);
        let iss = self.oidc_issuer(realm);
        let fail = |e: OAuthError| {
            let mut p = vec![("error", e.error), ("error_description", e.error_description.as_str())];
            if let Some(s) = &state {
                p.push(("state", s));
            }
            p.push(("iss", &iss));
            Box::new(AuthorizeOutcome::Redirect(with_params(&redirect_uri, &p)))
        };
        if get("request").is_some() {
            return Err(fail(OAuthError::request_not_supported(
                "request objects are not supported",
            )));
        }
        if get("request_uri").is_some() {
            return Err(fail(OAuthError::request_uri_not_supported(
                "request_uri is not supported",
            )));
        }
        if get("response_type") != Some("code") {
            return Err(fail(OAuthError::unsupported_response_type(
                "only response_type=code is supported",
            )));
        }
        if !client.allows_grant(GRANT_CODE) {
            return Err(fail(OAuthError::unauthorized_client(
                "this client may not use the authorization code grant",
            )));
        }
        if !matches!(get("response_mode"), None | Some("query")) {
            return Err(fail(OAuthError::invalid_request(
                "only response_mode=query is supported",
            )));
        }
        let scopes = match get("scope").map(parse_scope_param) {
            Some(Ok(s)) if !s.is_empty() => s,
            Some(Err(e)) => return Err(fail(e)),
            _ => return Err(fail(OAuthError::invalid_scope("scope is required"))),
        };
        if !scopes_within(&scopes, &client.scopes) {
            return Err(fail(OAuthError::invalid_scope(
                "the client may not request one of these scopes",
            )));
        }
        let code_challenge = match (get("code_challenge"), get("code_challenge_method")) {
            (Some(c), Some("S256")) if pkce_ok(c) => Some(c.to_owned()),
            (Some(_), Some("S256")) => return Err(fail(OAuthError::invalid_request("malformed code_challenge"))),
            (Some(_), _) => {
                return Err(fail(OAuthError::invalid_request("code_challenge_method must be S256")));
            }
            (None, Some(_)) => {
                return Err(fail(OAuthError::invalid_request(
                    "code_challenge_method without code_challenge",
                )));
            }
            (None, None) if client.is_public() => {
                return Err(fail(OAuthError::invalid_request(
                    "PKCE with code_challenge_method=S256 is required for public clients",
                )));
            }
            (None, None) => None,
        };
        let nonce = get("nonce").map(str::to_owned);
        if nonce.as_ref().is_some_and(|n| n.len() > 512) || state.as_ref().is_some_and(|s| s.len() > 2048) {
            return Err(fail(OAuthError::invalid_request("nonce or state too long")));
        }
        let prompt: Vec<&str> = get("prompt")
            .map(|p| p.split(' ').filter(|s| !s.is_empty()).collect())
            .unwrap_or_default();
        if prompt
            .iter()
            .any(|p| !matches!(*p, "none" | "login" | "consent" | "select_account"))
        {
            return Err(fail(OAuthError::invalid_request("unsupported prompt value")));
        }
        let prompt_none = prompt.contains(&"none");
        if prompt_none && prompt.len() > 1 {
            return Err(fail(OAuthError::invalid_request(
                "prompt=none cannot be combined with other values",
            )));
        }
        let max_age = match get("max_age") {
            None => None,
            Some(m) => match m.parse::<u32>() {
                Ok(v) => Some(i64::from(v)),
                Err(_) => {
                    return Err(fail(OAuthError::invalid_request(
                        "max_age must be a non-negative integer",
                    )));
                }
            },
        };
        Ok(Validated {
            client,
            redirect_uri,
            scopes,
            state,
            nonce,
            code_challenge,
            prompt_none,
            prompt_login: prompt.contains(&"login") || prompt.contains(&"select_account"),
            prompt_consent: prompt.contains(&"consent"),
            max_age,
        })
    }

    fn error_redirect(&self, realm: &str, v: &Validated, e: &OAuthError) -> AuthorizeOutcome {
        let iss = self.oidc_issuer(realm);
        let mut p = vec![("error", e.error), ("error_description", e.error_description.as_str())];
        if let Some(s) = &v.state {
            p.push(("state", s));
        }
        p.push(("iss", &iss));
        AuthorizeOutcome::Redirect(with_params(&v.redirect_uri, &p))
    }

    async fn load_request(
        &self,
        realm: &str,
        id: &str,
    ) -> Result<Option<(HashMap<String, String>, i64, Option<String>)>, AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT realm, params, session_id, created_at, expires_at, used_at FROM _forgebase_oauth_requests WHERE id = ?1",
            vec![t(id)],
        )
        .await?;
        Ok(first(&r).and_then(|row| {
            (row.str("realm") == realm && row.opt_i64("used_at").is_none() && row.i64("expires_at") > self.now())
                .then(|| {
                    serde_json::from_str(&row.str("params"))
                        .ok()
                        .map(|p| (p, row.i64("created_at"), row.opt_str("session_id")))
                })
                .flatten()
        }))
    }

    async fn save_request(
        &self,
        realm: &str,
        client_id: &str,
        raw: &HashMap<String, String>,
        existing: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<String, AuthError> {
        let now = self.now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "DELETE FROM _forgebase_oauth_requests WHERE expires_at < ?1",
            vec![i(now)],
        )
        .await?;
        let id = match existing {
            Some(id) => {
                tx.exec(
                    "UPDATE _forgebase_oauth_requests SET session_id = ?2 WHERE id = ?1",
                    vec![t(id), ot(session_id)],
                )
                .await?;
                id.to_owned()
            }
            None => {
                let id = crypto::random_token(24)?;
                let mut params = raw.clone();
                params.remove("request_id");
                tx.exec(
                    "INSERT INTO _forgebase_oauth_requests (id, realm, client_id, params, session_id, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    vec![
                        t(id.clone()),
                        t(realm),
                        t(client_id),
                        t(serde_json::to_string(&params).map_err(|e| AuthError::Internal(e.to_string()))?),
                        ot(session_id),
                        i(now),
                        i(now + self.config().idp.request_ttl.as_secs() as i64),
                    ],
                )
                .await?;
                id
            }
        };
        tx.commit().await?;
        Ok(id)
    }

    async fn remembered_scopes(&self, client_id: &str, s: &SsoSession) -> Result<Vec<String>, AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT scopes FROM _forgebase_oauth_consents WHERE client_id = ?1 AND collection = ?2 AND user_id = ?3",
            vec![t(client_id), t(s.collection.clone()), t(s.user_id.clone())],
        )
        .await?;
        Ok(first(&r)
            .and_then(|row| serde_json::from_str(&row.str("scopes")).ok())
            .unwrap_or_default())
    }

    /// The authorization endpoint. `raw` holds the query (GET) or form (POST)
    /// parameters; `request_id` resumes a request saved for sign-in or
    /// consent.
    pub(crate) async fn authorize(
        &self,
        realm: &str,
        raw: &HashMap<String, String>,
        cookie: Option<&str>,
        ci: &ClientInfo,
    ) -> Result<AuthorizeOutcome, AuthError> {
        self.realm_identity(realm)?;
        let (raw, resumed) = match raw.get("request_id") {
            Some(rid) => match self.load_request(realm, rid).await? {
                Some((p, created, _)) => (p, Some((rid.clone(), created))),
                None => {
                    return Ok(AuthorizeOutcome::ErrorPage(OAuthError::invalid_request(
                        "the authorization request is unknown or expired; start again from the application",
                    )));
                }
            },
            None => (raw.clone(), None),
        };
        let v = match self.validate_authorize(realm, &raw).await {
            Ok(v) => v,
            Err(out) => return Ok(*out),
        };
        let now = self.now();
        let session = self.sso_session(realm, cookie).await?;
        let fresh = |s: &SsoSession| resumed.as_ref().is_some_and(|(_, created)| s.auth_time >= *created);
        let needs_login = match &session {
            None => true,
            Some(s) => !fresh(s) && (v.prompt_login || v.max_age.is_some_and(|m| now - s.auth_time > m)),
        };
        let Some(session) = session.filter(|_| !needs_login) else {
            if v.prompt_none {
                return Ok(self.error_redirect(realm, &v, &OAuthError::login_required("no usable session")));
            }
            let rid = self
                .save_request(
                    realm,
                    &v.client.id,
                    &raw,
                    resumed.as_ref().map(|(id, _)| id.as_str()),
                    None,
                )
                .await?;
            let return_to = with_params(
                &format!("{}/authorize", self.oidc_issuer(realm)),
                &[("request_id", &rid)],
            );
            return Ok(AuthorizeOutcome::Redirect(self.login_redirect(realm, &return_to)));
        };
        let remembered = self.remembered_scopes(&v.client.id, &session).await?;
        let needs_consent = !v.client.skip_consent && (v.prompt_consent || !scopes_within(&v.scopes, &remembered));
        if needs_consent {
            if v.prompt_none {
                return Ok(self.error_redirect(realm, &v, &OAuthError::consent_required("consent is required")));
            }
            if session.impersonator.is_some() {
                return Ok(self.error_redirect(
                    realm,
                    &v,
                    &OAuthError::access_denied("consent cannot be granted in an impersonated session"),
                ));
            }
            let rid = self
                .save_request(
                    realm,
                    &v.client.id,
                    &raw,
                    resumed.as_ref().map(|(id, _)| id.as_str()),
                    Some(&session.id),
                )
                .await?;
            return Ok(AuthorizeOutcome::Consent(ConsentPrompt {
                consent_required: true,
                request_id: rid,
                realm: realm.to_owned(),
                client: client_brief(&v.client),
                scopes: scope_list(&v.scopes),
                csrf_token: self.csrf_token(&session.id),
                action: format!("{}/consent", self.oidc_issuer(realm)),
            }));
        }
        let url = self
            .grant_code(
                realm,
                &v,
                &session,
                resumed.as_ref().map(|(id, _)| id.as_str()),
                None,
                ci,
            )
            .await?;
        Ok(AuthorizeOutcome::Redirect(url))
    }

    /// Issues an authorization code (and remembers consent when `consent`
    /// is set) and returns the redirect to the client.
    async fn grant_code(
        &self,
        realm: &str,
        v: &Validated,
        s: &SsoSession,
        request_id: Option<&str>,
        consent: Option<&[String]>,
        ci: &ClientInfo,
    ) -> Result<String, AuthError> {
        let code = crypto::random_token(32)?;
        let now = self.now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "DELETE FROM _forgebase_oauth_codes WHERE expires_at < ?1",
            vec![i(now - 3600)],
        )
        .await?;
        tx.exec(
            "INSERT INTO _forgebase_oauth_codes
                (code_hash, realm, client_id, redirect_uri, scopes, nonce, code_challenge, collection, user_id,
                 session_id, auth_time, acr, amr, impersonator, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            vec![
                t(self.keyring().token_hash("oauth_code", &code)),
                t(realm),
                t(v.client.id.clone()),
                t(v.redirect_uri.clone()),
                t(serde_json::to_string(&v.scopes).unwrap_or_default()),
                ot(v.nonce.clone()),
                ot(v.code_challenge.clone()),
                t(s.collection.clone()),
                t(s.user_id.clone()),
                t(s.id.clone()),
                i(s.auth_time),
                t(acr_of(s.mfa)),
                t(serde_json::to_string(&s.amr()).unwrap_or_default()),
                ot(s.impersonator.clone()),
                i(now),
                i(now + self.config().idp.code_ttl.as_secs() as i64),
            ],
        )
        .await?;
        if let Some(rid) = request_id {
            tx.exec(
                "UPDATE _forgebase_oauth_requests SET used_at = ?2 WHERE id = ?1",
                vec![t(rid), i(now)],
            )
            .await?;
        }
        if let Some(scopes) = consent {
            remember_consent_in(&mut tx, realm, &v.client.id, s, scopes, now).await?;
            audit_in(
                &mut tx,
                "oauth.consent.granted",
                true,
                ci,
                &s.subject().actor(),
                Some(&s.subject()),
                json!({"client_id": v.client.id, "scopes": scopes}),
            )
            .await?;
        }
        audit_in(
            &mut tx,
            "oauth.authorize",
            true,
            ci,
            &s.subject().actor(),
            Some(&s.subject()),
            json!({"client_id": v.client.id, "scopes": v.scopes, "realm": realm}),
        )
        .await?;
        tx.commit().await?;
        let iss = self.oidc_issuer(realm);
        let mut p = vec![("code", code.as_str())];
        if let Some(st) = &v.state {
            p.push(("state", st));
        }
        p.push(("iss", &iss));
        Ok(with_params(&v.redirect_uri, &p))
    }

    /// The user's answer on the consent screen. Refused in an impersonated
    /// session.
    pub(crate) async fn consent_decision(
        &self,
        realm: &str,
        request_id: &str,
        allow: bool,
        session: &SsoSession,
        ci: &ClientInfo,
    ) -> Result<AuthorizeOutcome, AuthError> {
        if session.impersonator.is_some() {
            return Err(AuthError::Forbidden(
                "impersonated_session: consent cannot be granted while impersonating".into(),
            ));
        }
        let Some((raw, _, sid)) = self.load_request(realm, request_id).await? else {
            return Ok(AuthorizeOutcome::ErrorPage(OAuthError::invalid_request(
                "the authorization request is unknown or expired; start again from the application",
            )));
        };
        if sid.as_deref() != Some(session.id.as_str()) {
            return Ok(AuthorizeOutcome::ErrorPage(OAuthError::invalid_request(
                "this authorization request belongs to another session",
            )));
        }
        let v = match self.validate_authorize(realm, &raw).await {
            Ok(v) => v,
            Err(out) => return Ok(*out),
        };
        if !allow {
            let mut tx = Tx::begin(self.data().backend()).await?;
            tx.exec(
                "UPDATE _forgebase_oauth_requests SET used_at = ?2 WHERE id = ?1",
                vec![t(request_id), i(self.now())],
            )
            .await?;
            audit_in(
                &mut tx,
                "oauth.consent.denied",
                true,
                ci,
                &session.subject().actor(),
                Some(&session.subject()),
                json!({"client_id": v.client.id}),
            )
            .await?;
            tx.commit().await?;
            return Ok(self.error_redirect(realm, &v, &OAuthError::access_denied("the user denied the request")));
        }
        let scopes = v.scopes.clone();
        let url = self
            .grant_code(realm, &v, session, Some(request_id), Some(&scopes), ci)
            .await?;
        Ok(AuthorizeOutcome::Redirect(url))
    }

    // -----------------------------------------------------------------------
    // Remembered consent
    // -----------------------------------------------------------------------

    /// The caller's remembered consents.
    pub async fn list_consents(&self, caller: &Authenticated) -> Result<Vec<ConsentGrant>, AuthError> {
        let Subject::User { collection, id } = &caller.subject else {
            return Err(AuthError::Forbidden("only users have consents".into()));
        };
        let r = read(
            self.data().backend(),
            "SELECT c.id, c.client_id, c.realm, c.scopes, c.created_at, c.updated_at, k.name
             FROM _forgebase_oauth_consents c JOIN _forgebase_oauth_clients k ON k.id = c.client_id
             WHERE c.collection = ?1 AND c.user_id = ?2 ORDER BY c.created_at",
            vec![t(collection.clone()), t(id.clone())],
        )
        .await?;
        Ok(rows(&r)
            .map(|row| ConsentGrant {
                id: row.str("id"),
                client_id: row.str("client_id"),
                client_name: row.str("name"),
                realm: row.str("realm"),
                scopes: serde_json::from_str(&row.str("scopes")).unwrap_or_default(),
                created_at: rfc3339(row.i64("created_at")),
                updated_at: rfc3339(row.i64("updated_at")),
            })
            .collect())
    }

    /// Revokes a remembered consent and every token family the client holds
    /// for the caller.
    pub async fn revoke_consent(&self, caller: &Authenticated, ci: &ClientInfo, id: &str) -> Result<(), AuthError> {
        let Subject::User { collection, id: uid } = &caller.subject else {
            return Err(AuthError::Forbidden("only users have consents".into()));
        };
        let mut tx = Tx::begin(self.data().backend()).await?;
        let r = tx
            .read(
                "SELECT client_id FROM _forgebase_oauth_consents WHERE id = ?1 AND collection = ?2 AND user_id = ?3",
                vec![t(id), t(collection.clone()), t(uid.clone())],
            )
            .await?;
        let Some(client_id) = first(&r).map(|row| row.str("client_id")) else {
            return Err(AuthError::NotFound("consent not found".into()));
        };
        tx.exec("DELETE FROM _forgebase_oauth_consents WHERE id = ?1", vec![t(id)])
            .await?;
        let n = tx
            .exec(
                "UPDATE _forgebase_oauth_families SET revoked_at = ?4, revoked_reason = 'consent_revoked'
                 WHERE client_id = ?1 AND collection = ?2 AND user_id = ?3 AND revoked_at IS NULL",
                vec![
                    t(client_id.clone()),
                    t(collection.clone()),
                    t(uid.clone()),
                    i(self.now()),
                ],
            )
            .await?
            .rows_affected;
        audit_in(
            &mut tx,
            "oauth.consent.revoked",
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({"client_id": client_id, "families_revoked": n}),
        )
        .await?;
        tx.commit().await
    }

    // -----------------------------------------------------------------------
    // Device authorization (RFC 8628)
    // -----------------------------------------------------------------------

    /// Starts a device authorization for an authenticated client.
    pub(crate) async fn device_authorize(
        &self,
        realm: &str,
        client: &ClientRecord,
        scope: Option<&str>,
    ) -> Result<DeviceAuthorization, OAuthError> {
        if !client.allows_grant(GRANT_DEVICE) {
            return Err(OAuthError::unauthorized_client(
                "this client may not use the device authorization grant",
            ));
        }
        let scopes = match scope.map(parse_scope_param) {
            Some(Ok(s)) if !s.is_empty() => s,
            Some(Err(e)) => return Err(e),
            _ => return Err(OAuthError::invalid_scope("scope is required")),
        };
        if !scopes_within(&scopes, &client.scopes) {
            return Err(OAuthError::invalid_scope(
                "the client may not request one of these scopes",
            ));
        }
        let cfg = &self.config().idp;
        let now = self.now();
        let ttl = cfg.device_code_ttl.as_secs();
        let interval = cfg.device_poll_interval.as_secs().max(1);
        let device_code = crypto::random_token(32)?;
        let kr = self.keyring();
        for _ in 0..5 {
            let user_code = crypto::random_from_alphabet(8, USER_CODE_ALPHABET)?;
            let mut tx = Tx::begin(self.data().backend()).await?;
            tx.exec(
                "DELETE FROM _forgebase_oauth_device_codes WHERE expires_at < ?1",
                vec![i(now - 3600)],
            )
            .await?;
            let ins = tx
                .exec(
                    "INSERT OR IGNORE INTO _forgebase_oauth_device_codes
                        (device_code_hash, user_code_hash, realm, client_id, scopes, interval_secs, created_at, expires_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    vec![
                        t(kr.token_hash("oauth_device_code", &device_code)),
                        t(kr.token_hash("oauth_user_code", &user_code)),
                        t(realm),
                        t(client.id.clone()),
                        t(serde_json::to_string(&scopes).unwrap_or_default()),
                        i(interval as i64),
                        i(now),
                        i(now + ttl as i64),
                    ],
                )
                .await?;
            if ins.rows_affected == 0 {
                continue;
            }
            tx.commit().await?;
            let display = format!("{}-{}", &user_code[..4], &user_code[4..]);
            let page = format!("{}/device", self.oidc_issuer(realm));
            return Ok(DeviceAuthorization {
                device_code,
                verification_uri_complete: with_params(&page, &[("user_code", &display)]),
                verification_uri: page,
                user_code: display,
                expires_in: ttl,
                interval,
            });
        }
        Err(OAuthError::server_error("could not allocate a user code"))
    }

    /// A pending device authorization by user code (for the verification
    /// page). Rate limited per IP.
    pub(crate) async fn device_lookup(
        &self,
        realm: &str,
        user_code: &str,
        ci: &ClientInfo,
    ) -> Result<Option<DeviceInfo>, AuthError> {
        self.limit_attempt(ci)?;
        let Some(code) = normalize_user_code(user_code) else {
            return Ok(None);
        };
        let r = read(
            self.data().backend(),
            "SELECT d.scopes, d.client_id FROM _forgebase_oauth_device_codes d
             WHERE d.user_code_hash = ?1 AND d.realm = ?2 AND d.status = 'pending' AND d.expires_at > ?3",
            vec![
                t(self.keyring().token_hash("oauth_user_code", &code)),
                t(realm),
                i(self.now()),
            ],
        )
        .await?;
        let Some(row) = first(&r) else {
            return Ok(None);
        };
        let Some(client) = self.load_client(&row.str("client_id")).await?.filter(|c| !c.disabled) else {
            return Ok(None);
        };
        let scopes: Vec<String> = serde_json::from_str(&row.str("scopes")).unwrap_or_default();
        Ok(Some(DeviceInfo {
            client: client_brief(&client),
            scopes: scope_list(&scopes),
            user_code: format!("{}-{}", &code[..4], &code[4..]),
        }))
    }

    /// Approves or denies a pending device authorization (the approval is
    /// also the user's consent). Refused in an impersonated session.
    pub(crate) async fn device_decide(
        &self,
        realm: &str,
        user_code: &str,
        allow: bool,
        s: &SsoSession,
        ci: &ClientInfo,
    ) -> Result<bool, AuthError> {
        if s.impersonator.is_some() {
            return Err(AuthError::Forbidden(
                "impersonated_session: a device cannot be approved while impersonating".into(),
            ));
        }
        self.limit_attempt(ci)?;
        let Some(code) = normalize_user_code(user_code) else {
            return Ok(false);
        };
        let now = self.now();
        let hash = self.keyring().token_hash("oauth_user_code", &code);
        let mut tx = Tx::begin(self.data().backend()).await?;
        let r = tx
            .read(
                "SELECT client_id, scopes FROM _forgebase_oauth_device_codes
                 WHERE user_code_hash = ?1 AND realm = ?2 AND status = 'pending' AND expires_at > ?3",
                vec![t(hash.clone()), t(realm), i(now)],
            )
            .await?;
        let Some((client_id, scopes)) = first(&r).map(|row| {
            (
                row.str("client_id"),
                serde_json::from_str::<Vec<String>>(&row.str("scopes")).unwrap_or_default(),
            )
        }) else {
            return Ok(false);
        };
        tx.exec(
            "UPDATE _forgebase_oauth_device_codes SET status = ?2, collection = ?3, user_id = ?4, session_id = ?5,
                auth_time = ?6, acr = ?7, amr = ?8
             WHERE user_code_hash = ?1 AND status = 'pending'",
            vec![
                t(hash),
                t(if allow { "approved" } else { "denied" }),
                t(s.collection.clone()),
                t(s.user_id.clone()),
                t(s.id.clone()),
                i(s.auth_time),
                t(acr_of(s.mfa)),
                t(serde_json::to_string(&s.amr()).unwrap_or_default()),
            ],
        )
        .await?;
        if allow {
            remember_consent_in(&mut tx, realm, &client_id, s, &scopes, now).await?;
        }
        audit_in(
            &mut tx,
            if allow {
                "oauth.device.approved"
            } else {
                "oauth.device.denied"
            },
            true,
            ci,
            &s.subject().actor(),
            Some(&s.subject()),
            json!({"client_id": client_id, "scopes": scopes}),
        )
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    // -----------------------------------------------------------------------
    // RP-initiated logout (OpenID Connect RP-Initiated Logout 1.0)
    // -----------------------------------------------------------------------

    /// Ends the realm's browser session. A valid `id_token_hint` for the
    /// current session, or a confirmed POST (`confirmed`), ends it at once;
    /// otherwise the user is asked to confirm.
    pub(crate) async fn rp_logout(
        &self,
        realm: &str,
        raw: &HashMap<String, String>,
        cookie: Option<&str>,
        confirmed: bool,
        ci: &ClientInfo,
    ) -> Result<LogoutOutcome, AuthError> {
        self.realm_identity(realm)?;
        let get = |k: &str| raw.get(k).map(String::as_str).filter(|v| !v.is_empty());
        let issuer = self.oidc_issuer(realm);
        let hint = match get("id_token_hint") {
            None => None,
            Some(tok) => match self.verify_idp_jwt(realm, tok).await? {
                Some(v) if v.claims["iss"] == issuer.as_str() && v.typ == "JWT" => Some(v.claims),
                _ => {
                    return Ok(LogoutOutcome::ErrorPage(OAuthError::invalid_request(
                        "id_token_hint is not an ID token of this realm",
                    )));
                }
            },
        };
        let hint_aud = hint.as_ref().and_then(|c| c["aud"].as_str().map(str::to_owned));
        let client_id = match (get("client_id"), hint_aud) {
            (Some(a), Some(b)) if a != b => {
                return Ok(LogoutOutcome::ErrorPage(OAuthError::invalid_request(
                    "client_id does not match id_token_hint",
                )));
            }
            (Some(a), _) => Some(a.to_owned()),
            (None, b) => b,
        };
        let client = match &client_id {
            Some(id) => match self.load_client(id).await? {
                Some(c) if c.realm == realm => Some(c),
                _ => {
                    return Ok(LogoutOutcome::ErrorPage(OAuthError::invalid_request(
                        "unknown client_id for this realm",
                    )));
                }
            },
            None => None,
        };
        let redirect = match get("post_logout_redirect_uri") {
            None => None,
            Some(u) => match &client {
                Some(c) if c.post_logout_redirect_uris.iter().any(|r| r == u) => {
                    let mut p = Vec::new();
                    if let Some(s) = get("state") {
                        p.push(("state", s));
                    }
                    Some(with_params(u, &p))
                }
                _ => {
                    return Ok(LogoutOutcome::ErrorPage(OAuthError::invalid_request(
                        "post_logout_redirect_uri is not registered for this client (exact match required)",
                    )));
                }
            },
        };
        let Some(session) = self.sso_session(realm, cookie).await? else {
            return Ok(LogoutOutcome::Done { redirect, ended: false });
        };
        let hint_matches = hint
            .as_ref()
            .and_then(|c| c["sid"].as_str())
            .is_some_and(|sid| crypto::ct_eq(sid.as_bytes(), self.sid_claim(&session.id).as_bytes()));
        if !hint_matches && !confirmed {
            return Ok(LogoutOutcome::Confirm {
                csrf_token: self.csrf_token(&session.id),
            });
        }
        self.end_sso_session(&session, "oidc_logout", ci).await?;
        Ok(LogoutOutcome::Done { redirect, ended: true })
    }

    /// Revokes a browser session and the token families bound to it that were
    /// not granted `offline_access`.
    async fn end_sso_session(&self, s: &SsoSession, reason: &str, ci: &ClientInfo) -> Result<(), AuthError> {
        let now = self.now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "UPDATE _forgebase_auth_sessions SET revoked_at = ?2, revoked_reason = ?3 WHERE id = ?1 AND revoked_at IS NULL",
            vec![t(s.id.clone()), i(now), t(reason)],
        )
        .await?;
        let n = tx
            .exec(
                "UPDATE _forgebase_oauth_families SET revoked_at = ?2, revoked_reason = 'session_ended'
                 WHERE session_id = ?1 AND revoked_at IS NULL
                   AND NOT EXISTS (SELECT 1 FROM json_each(scopes) WHERE value = 'offline_access')",
                vec![t(s.id.clone()), i(now)],
            )
            .await?
            .rows_affected;
        audit_in(
            &mut tx,
            "oidc.logout",
            true,
            ci,
            &s.subject().actor(),
            Some(&s.subject()),
            json!({"session_id": s.id, "families_revoked": n}),
        )
        .await?;
        tx.commit().await
    }
}

/// Adds `scopes` to the user's remembered consent for a client.
async fn remember_consent_in(
    tx: &mut Tx,
    realm: &str,
    client_id: &str,
    s: &SsoSession,
    scopes: &[String],
    now: i64,
) -> Result<(), AuthError> {
    let r = tx
        .read(
            "SELECT id, scopes FROM _forgebase_oauth_consents WHERE client_id = ?1 AND collection = ?2 AND user_id = ?3",
            vec![t(client_id), t(s.collection.clone()), t(s.user_id.clone())],
        )
        .await?;
    match first(&r) {
        Some(row) => {
            let mut have: Vec<String> = serde_json::from_str(&row.str("scopes")).unwrap_or_default();
            for sc in scopes {
                if !have.contains(sc) {
                    have.push(sc.clone());
                }
            }
            have.sort();
            tx.exec(
                "UPDATE _forgebase_oauth_consents SET scopes = ?2, updated_at = ?3 WHERE id = ?1",
                vec![
                    t(row.str("id")),
                    t(serde_json::to_string(&have).unwrap_or_default()),
                    i(now),
                ],
            )
            .await?;
        }
        None => {
            tx.exec(
                "INSERT INTO _forgebase_oauth_consents (id, realm, client_id, collection, user_id, scopes, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
                vec![
                    t(crypto::random_id(20)?),
                    t(realm),
                    t(client_id),
                    t(s.collection.clone()),
                    t(s.user_id.clone()),
                    t(serde_json::to_string(scopes).unwrap_or_default()),
                    i(now),
                ],
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_codes_normalise() {
        assert_eq!(normalize_user_code("bcdf-ghjk").as_deref(), Some("BCDFGHJK"));
        assert_eq!(normalize_user_code("BCDF GHJK").as_deref(), Some("BCDFGHJK"));
        assert!(
            normalize_user_code("ABCD-EFGH").is_none(),
            "vowels are not in the alphabet"
        );
        assert!(normalize_user_code("BCDF").is_none());
    }

    #[test]
    fn params_are_appended() {
        assert_eq!(
            with_params("https://a.example/cb?x=1", &[("code", "a b"), ("state", "s")]),
            "https://a.example/cb?x=1&code=a+b&state=s"
        );
        assert!(with_params("com.example.app:/cb", &[("code", "c")]).ends_with("?code=c"));
        assert!(pkce_ok(&"a".repeat(43)));
        assert!(!pkce_ok(&"a".repeat(42)));
        assert!(!pkce_ok(&format!("{}+", "a".repeat(43))));
    }
}
