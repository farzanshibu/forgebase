//! WebAuthn / passkeys (FB-22, ADR-0009): registration and authentication
//! ceremonies, credential storage, sign-count checks, user verification;
//! a passkey is a first factor (with user verification it also counts as MFA)
//! and a second factor after a password, magic link, OTP or social login.
//!
//! Pure Rust: ES256 and RS256 credentials, attestation `none` semantics (the
//! attestation statement is not verified, so the authenticator model is not
//! trusted; ADR-0009 §6). The ceremony JSON follows the WebAuthn Level 3
//! `...OptionsJSON` / `...ResponseJSON` shapes (base64url everywhere), so
//! browsers can use `PublicKeyCredential.parse*FromJSON` and `toJSON()`.

pub mod cbor;
pub mod http;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::crypto::{self, b64url, ct_eq};
use crate::service::{AuthService, ClientInfo, LoginOptions, SessionTokens, audit_in};
use crate::social::sig::PublicKey;
use crate::store::{Tx, first, i, now, ot, read, rfc3339, rows, t};
use crate::{AuthError, AuthMethod, Authenticated, Subject};
use cbor::Cbor;

/// Lifetime of a ceremony challenge.
const CEREMONY_TTL_SECS: i64 = 300;
/// COSE algorithm ids.
const ALG_ES256: i64 = -7;
const ALG_RS256: i64 = -257;

/// Relying party settings.
#[derive(Debug, Clone)]
pub struct WebAuthnConfig {
    /// RP id (a registrable domain suffix of the origin; no port).
    pub rp_id: String,
    /// Shown by authenticators.
    pub rp_name: String,
    /// Accepted origins (`scheme://host[:port]`).
    pub origins: Vec<String>,
}

impl WebAuthnConfig {
    /// Derives RP id and origin from the public URL.
    pub fn from_public_url(public_url: &str) -> Self {
        let url = url::Url::parse(public_url).ok();
        let host = url
            .as_ref()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_else(|| "localhost".into());
        let origin = url
            .as_ref()
            .map_or_else(|| "http://localhost".to_owned(), |u| u.origin().ascii_serialization());
        Self {
            rp_id: host,
            rp_name: "ForgeBase".into(),
            origins: vec![origin],
        }
    }
}

/// Registration options for `navigator.credentials.create`.
#[derive(Debug, Clone, Serialize)]
pub struct CeremonyOptions {
    /// Id to send back with the result.
    pub ceremony_id: String,
    /// `PublicKeyCredential*OptionsJSON`.
    #[serde(rename = "publicKey")]
    pub public_key: Json,
}

/// A stored passkey, as shown to its owner.
#[derive(Debug, Clone, Serialize)]
pub struct CredentialView {
    /// Credential id (base64url).
    pub id: String,
    /// Label.
    pub name: Option<String>,
    /// Whether the authenticator verified the user at registration.
    pub user_verified: bool,
    /// Whether it can be used as a second factor.
    pub use_as_mfa: bool,
    /// Backup eligible (synced passkey).
    pub backup_eligible: bool,
    /// Signature counter.
    pub sign_count: i64,
    /// Registered at (RFC 3339).
    pub created_at: String,
    /// Last used (RFC 3339).
    pub last_used_at: Option<String>,
}

/// Client result of `navigator.credentials.create` (`RegistrationResponseJSON`).
#[derive(Debug, Clone, Deserialize)]
pub struct RegistrationCredential {
    /// Credential id (base64url).
    pub id: String,
    /// Response fields.
    pub response: RegistrationResponse,
}

/// The attestation response.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationResponse {
    /// Client data JSON (base64url).
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    /// Attestation object (base64url).
    pub attestation_object: String,
    /// Transports reported by the client.
    #[serde(default)]
    pub transports: Vec<String>,
}

/// Client result of `navigator.credentials.get` (`AuthenticationResponseJSON`).
#[derive(Debug, Clone, Deserialize)]
pub struct AuthenticationCredential {
    /// Credential id (base64url).
    pub id: String,
    /// Response fields.
    pub response: AuthenticationResponse,
}

/// The assertion response.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthenticationResponse {
    /// Client data JSON (base64url).
    #[serde(rename = "clientDataJSON")]
    pub client_data_json: String,
    /// Authenticator data (base64url).
    pub authenticator_data: String,
    /// Signature (base64url).
    pub signature: String,
    /// User handle (base64url), for discoverable credentials.
    #[serde(default)]
    pub user_handle: Option<String>,
}

fn dec(s: &str) -> Result<Vec<u8>, AuthError> {
    if s.len() > 64 * 1024 {
        return Err(AuthError::invalid("value too large"));
    }
    URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .map_err(|_| AuthError::invalid("invalid base64url"))
}

fn bad_ceremony() -> AuthError {
    AuthError::InvalidToken("invalid or expired passkey ceremony")
}

fn failed() -> AuthError {
    AuthError::InvalidCredentials
}

/// Parsed authenticator data (WebAuthn §6.1).
struct AuthData<'a> {
    rp_id_hash: &'a [u8],
    flags: u8,
    sign_count: u32,
    attested: Option<&'a [u8]>,
}

impl AuthData<'_> {
    fn up(&self) -> bool {
        self.flags & 0x01 != 0
    }
    fn uv(&self) -> bool {
        self.flags & 0x04 != 0
    }
    fn be(&self) -> bool {
        self.flags & 0x08 != 0
    }
    fn bs(&self) -> bool {
        self.flags & 0x10 != 0
    }
}

fn parse_auth_data(b: &[u8]) -> Option<AuthData<'_>> {
    if b.len() < 37 {
        return None;
    }
    let flags = b[32];
    Some(AuthData {
        rp_id_hash: &b[..32],
        flags,
        sign_count: u32::from_be_bytes(b[33..37].try_into().ok()?),
        attested: (flags & 0x40 != 0).then_some(&b[37..]),
    })
}

/// A COSE public key: `(alg, key)`.
fn parse_cose(key: &Cbor) -> Option<(i64, PublicKey)> {
    let alg = i64::try_from(key.get_int(3)?.as_int()?).ok()?;
    let kty = key.get_int(1)?.as_int()?;
    match (alg, kty) {
        (ALG_ES256, 2) => {
            if key.get_int(-1)?.as_int()? != 1 {
                return None;
            }
            Some((
                alg,
                PublicKey::p256(key.get_int(-2)?.as_bytes()?, key.get_int(-3)?.as_bytes()?)?,
            ))
        }
        (ALG_RS256, 3) => Some((
            alg,
            PublicKey::rsa(key.get_int(-1)?.as_bytes()?, key.get_int(-2)?.as_bytes()?)?,
        )),
        _ => None,
    }
}

/// Re-encodes the stored COSE key; only called with keys we accepted.
fn load_key(cose: &[u8]) -> Option<PublicKey> {
    parse_cose(&Cbor::decode(cose)?.0).map(|(_, k)| k)
}

impl AuthService {
    /// Relying-party settings in effect.
    pub fn webauthn(&self) -> WebAuthnConfig {
        self.webauthn_config().clone()
    }

    fn check_client_data(&self, raw: &[u8], typ: &str, challenge: &str) -> Result<(), AuthError> {
        let cd: Json = serde_json::from_slice(raw).map_err(|_| failed())?;
        let cfg = self.webauthn_config();
        let ok = cd.get("type").and_then(Json::as_str) == Some(typ)
            && cd
                .get("challenge")
                .and_then(Json::as_str)
                .is_some_and(|c| ct_eq(c.as_bytes(), challenge.as_bytes()))
            && cd
                .get("origin")
                .and_then(Json::as_str)
                .is_some_and(|o| cfg.origins.iter().any(|a| a == o))
            && cd.get("crossOrigin") != Some(&Json::Bool(true));
        ok.then_some(()).ok_or_else(failed)
    }

    fn rp_hash_ok(&self, ad: &AuthData<'_>) -> bool {
        ct_eq(ad.rp_id_hash, &Sha256::digest(self.webauthn_config().rp_id.as_bytes()))
    }

    async fn new_ceremony(
        &self,
        purpose: &str,
        subject: Option<(&str, &str)>,
        detail: Json,
    ) -> Result<(String, String), AuthError> {
        let id = crypto::random_token(24)?;
        let challenge = crypto::random_token(32)?;
        let ts = now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "DELETE FROM _forgebase_webauthn_challenges WHERE expires_at < ?1",
            vec![i(ts - 3600)],
        )
        .await?;
        tx.exec(
            "INSERT INTO _forgebase_webauthn_challenges
                (ceremony_hash, purpose, challenge, collection, user_id, detail, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            vec![
                t(self.keyring().token_hash("webauthn_ceremony", &id)),
                t(purpose),
                t(challenge.clone()),
                ot(subject.map(|s| s.0)),
                ot(subject.map(|s| s.1)),
                t(detail.to_string()),
                i(ts),
                i(ts + CEREMONY_TTL_SECS),
            ],
        )
        .await?;
        tx.commit().await?;
        Ok((id, challenge))
    }

    /// Consumes a ceremony: single use, unexpired, of the right purpose.
    /// Returns `(challenge, collection, user_id, detail)`.
    async fn claim_ceremony(
        &self,
        purpose: &str,
        id: &str,
    ) -> Result<(String, Option<String>, Option<String>, Json), AuthError> {
        if id.is_empty() || id.len() > 256 {
            return Err(bad_ceremony());
        }
        let hash = self.keyring().token_hash("webauthn_ceremony", id);
        let r = read(
            self.data().backend(),
            "SELECT challenge, collection, user_id, detail FROM _forgebase_webauthn_challenges
             WHERE ceremony_hash = ?1 AND purpose = ?2 AND used_at IS NULL AND expires_at > ?3",
            vec![t(hash.clone()), t(purpose), i(now())],
        )
        .await?;
        let Some(row) = first(&r) else {
            return Err(bad_ceremony());
        };
        let mut tx = Tx::begin(self.data().backend()).await?;
        let claimed = tx
            .exec(
                "UPDATE _forgebase_webauthn_challenges SET used_at = ?2 WHERE ceremony_hash = ?1 AND used_at IS NULL",
                vec![t(hash), i(now())],
            )
            .await?
            .rows_affected
            == 1;
        tx.commit().await?;
        if !claimed {
            return Err(bad_ceremony());
        }
        Ok((
            row.str("challenge"),
            row.opt_str("collection"),
            row.opt_str("user_id"),
            serde_json::from_str(&row.str("detail")).unwrap_or(Json::Null),
        ))
    }

    async fn user_credentials(&self, coll: &str, uid: &str) -> Result<Vec<(String, Option<String>)>, AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT credential_id, transports FROM _forgebase_webauthn_credentials
             WHERE collection = ?1 AND user_id = ?2 ORDER BY created_at",
            vec![t(coll), t(uid)],
        )
        .await?;
        Ok(rows(&r)
            .map(|r| (r.str("credential_id"), r.opt_str("transports")))
            .collect())
    }

    // -----------------------------------------------------------------------
    // Registration
    // -----------------------------------------------------------------------

    /// Starts registering a passkey for the signed-in user.
    pub async fn webauthn_register_options(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        use_as_mfa: bool,
    ) -> Result<CeremonyOptions, AuthError> {
        self.limit_attempt(ci)?;
        let (coll, uid) = session_user(caller)?;
        let user = self.user_view(&coll, &uid).await?;
        let email = user.get("email").and_then(Json::as_str).unwrap_or_default().to_owned();
        // A stable user handle: opaque, random, not the email or record id.
        let r = read(
            self.data().backend(),
            "SELECT user_handle FROM _forgebase_webauthn_credentials WHERE collection = ?1 AND user_id = ?2 LIMIT 1",
            vec![t(coll.clone()), t(uid.clone())],
        )
        .await?;
        let handle = match first(&r) {
            Some(row) => row.str("user_handle"),
            None => crypto::random_token(32)?,
        };
        let (ceremony_id, challenge) = self
            .new_ceremony(
                "register",
                Some((&coll, &uid)),
                json!({"use_as_mfa": use_as_mfa, "handle": handle}),
            )
            .await?;
        let cfg = self.webauthn_config();
        let exclude: Vec<Json> = self
            .user_credentials(&coll, &uid)
            .await?
            .into_iter()
            .map(|(id, _)| json!({"type": "public-key", "id": id}))
            .collect();
        Ok(CeremonyOptions {
            ceremony_id,
            public_key: json!({
                "rp": {"id": cfg.rp_id, "name": cfg.rp_name},
                "user": {"id": handle, "name": email, "displayName": email},
                "challenge": challenge,
                "pubKeyCredParams": [{"type": "public-key", "alg": ALG_ES256}, {"type": "public-key", "alg": ALG_RS256}],
                "timeout": CEREMONY_TTL_SECS * 1000,
                "attestation": "none",
                "excludeCredentials": exclude,
                "authenticatorSelection": {"residentKey": "preferred", "userVerification": "preferred"},
            }),
        })
    }

    /// Finishes registration and stores the credential.
    pub async fn webauthn_register_verify(
        &self,
        caller: &Authenticated,
        ci: &ClientInfo,
        ceremony_id: &str,
        name: Option<&str>,
        cred: &RegistrationCredential,
    ) -> Result<CredentialView, AuthError> {
        self.limit_attempt(ci)?;
        let (coll, uid) = session_user(caller)?;
        let (challenge, c, u, detail) = self.claim_ceremony("register", ceremony_id).await?;
        if c.as_deref() != Some(coll.as_str()) || u.as_deref() != Some(uid.as_str()) {
            return Err(bad_ceremony());
        }
        let fail = |reason: &'static str| {
            let this = self.clone();
            let ci = ci.clone();
            let subject = caller.subject.clone();
            async move {
                let _ = this
                    .audit(
                        "webauthn.register",
                        false,
                        &ci,
                        &subject.actor(),
                        Some(&subject),
                        json!({"reason": reason}),
                    )
                    .await;
                AuthError::InvalidInput(format!("passkey registration failed: {reason}"))
            }
        };
        let cd = dec(&cred.response.client_data_json)?;
        if self.check_client_data(&cd, "webauthn.create", &challenge).is_err() {
            return Err(fail("client_data").await);
        }
        let att = dec(&cred.response.attestation_object)?;
        let Some((att, _)) = Cbor::decode(&att) else {
            return Err(fail("attestation_object").await);
        };
        let Some(auth_data) = att.get_text("authData").and_then(Cbor::as_bytes) else {
            return Err(fail("authData").await);
        };
        let Some(ad) = parse_auth_data(auth_data) else {
            return Err(fail("authData").await);
        };
        if !self.rp_hash_ok(&ad) {
            return Err(fail("rp_id").await);
        }
        if !ad.up() {
            return Err(fail("user_presence").await);
        }
        let Some(att_data) = ad.attested else {
            return Err(fail("no_credential").await);
        };
        if att_data.len() < 18 {
            return Err(fail("credential_data").await);
        }
        let id_len = usize::from(u16::from_be_bytes([att_data[16], att_data[17]]));
        if id_len == 0 || id_len > 1023 || att_data.len() < 18 + id_len {
            return Err(fail("credential_id").await);
        }
        let cred_id = &att_data[18..18 + id_len];
        if b64url(cred_id) != cred.id.trim_end_matches('=') {
            return Err(fail("credential_id_mismatch").await);
        }
        let Some((key, used)) = Cbor::decode(&att_data[18 + id_len..]) else {
            return Err(fail("public_key").await);
        };
        let Some((alg, _)) = parse_cose(&key) else {
            return Err(fail("unsupported_key").await);
        };
        let cose = &att_data[18 + id_len..18 + id_len + used];
        let use_as_mfa = detail.get("use_as_mfa").and_then(Json::as_bool).unwrap_or(false);
        let handle = detail
            .get("handle")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_owned();
        let transports: Vec<&String> = cred.response.transports.iter().take(8).collect();
        let label = name.map(|n| n.chars().take(64).collect::<String>());
        let ts = now();
        let mut tx = Tx::begin(self.data().backend()).await?;
        let ins = tx
            .exec(
                "INSERT INTO _forgebase_webauthn_credentials
                    (credential_id, realm, collection, user_id, user_handle, public_key, alg, sign_count, uv,
                     backup_eligible, backup_state, transports, name, use_as_mfa, created_at)
                 VALUES (?1, 'default', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                vec![
                    t(b64url(cred_id)),
                    t(coll.clone()),
                    t(uid.clone()),
                    t(handle),
                    forgebase_data::db::Value::Blob(cose.to_vec()),
                    i(alg),
                    i(i64::from(ad.sign_count)),
                    i(i64::from(ad.uv())),
                    i(i64::from(ad.be())),
                    i(i64::from(ad.bs())),
                    ot(serde_json::to_string(&transports).ok()),
                    ot(label),
                    i(i64::from(use_as_mfa)),
                    i(ts),
                ],
            )
            .await;
        if let Err(e) = ins {
            return Err(if crate::service::is_unique_violation(&e) {
                AuthError::Conflict("this passkey is already registered".into())
            } else {
                e
            });
        }
        audit_in(
            &mut tx,
            "webauthn.registered",
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({"credential_id": b64url(cred_id), "uv": ad.uv(), "use_as_mfa": use_as_mfa}),
        )
        .await?;
        tx.commit().await?;
        self.credential_view(&b64url(cred_id)).await
    }

    async fn credential_view(&self, id: &str) -> Result<CredentialView, AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT credential_id, name, uv, use_as_mfa, backup_eligible, sign_count, created_at, last_used_at
             FROM _forgebase_webauthn_credentials WHERE credential_id = ?1",
            vec![t(id)],
        )
        .await?;
        first(&r)
            .map(cred_row)
            .ok_or_else(|| AuthError::NotFound("passkey not found".into()))
    }

    /// The caller's passkeys.
    pub async fn webauthn_credentials(&self, caller: &Authenticated) -> Result<Vec<CredentialView>, AuthError> {
        let (coll, uid) = session_user(caller)?;
        let r = read(
            self.data().backend(),
            "SELECT credential_id, name, uv, use_as_mfa, backup_eligible, sign_count, created_at, last_used_at
             FROM _forgebase_webauthn_credentials WHERE collection = ?1 AND user_id = ?2 ORDER BY created_at",
            vec![t(coll), t(uid)],
        )
        .await?;
        Ok(rows(&r).map(cred_row).collect())
    }

    /// Deletes one of the caller's passkeys.
    pub async fn webauthn_delete(&self, caller: &Authenticated, ci: &ClientInfo, id: &str) -> Result<(), AuthError> {
        let (coll, uid) = session_user(caller)?;
        let r = read(
            self.data().backend(),
            "SELECT 1 FROM _forgebase_webauthn_credentials WHERE credential_id = ?1 AND collection = ?2 AND user_id = ?3",
            vec![t(id), t(coll.clone()), t(uid.clone())],
        )
        .await?;
        if r.is_empty() {
            return Err(AuthError::NotFound("passkey not found".into()));
        }
        if self.sole_way_in(&coll, &uid, None, Some(id)).await? {
            return Err(AuthError::Conflict(
                "this is the only way to sign in to the account; add another passkey or a provider first".into(),
            ));
        }
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "DELETE FROM _forgebase_webauthn_credentials WHERE credential_id = ?1",
            vec![t(id)],
        )
        .await?;
        audit_in(
            &mut tx,
            "webauthn.deleted",
            true,
            ci,
            &caller.subject.actor(),
            Some(&caller.subject),
            json!({"credential_id": id}),
        )
        .await?;
        tx.commit().await
    }

    // -----------------------------------------------------------------------
    // Authentication
    // -----------------------------------------------------------------------

    /// Options for signing in with a passkey. With `email`, the allow list is
    /// that user's credentials (an unknown email gets a deterministic fake
    /// id, so the answer does not reveal whether the account exists); without
    /// it the ceremony is usernameless (discoverable credentials).
    pub async fn webauthn_login_options(
        &self,
        ci: &ClientInfo,
        collection: Option<&str>,
        email: Option<&str>,
    ) -> Result<CeremonyOptions, AuthError> {
        self.limit_attempt(ci)?;
        let coll = self.auth_collection(collection).await?;
        let allow = match email {
            Some(e) => {
                let e = crate::service::normalize_email(e)?;
                let user = self.find_user(&coll, &e).await?;
                let ids = match user {
                    Some(u) => self.user_credentials(&coll.name, &u.id).await?,
                    None => Vec::new(),
                };
                let list: Vec<Json> = if ids.is_empty() {
                    let fake = self
                        .keyring()
                        .token_hash("webauthn_fake_credential", &format!("{}:{e}", coll.name));
                    vec![json!({"type": "public-key", "id": b64url(&Sha256::digest(fake.as_bytes()))})]
                } else {
                    ids.into_iter()
                        .map(|(id, _)| json!({"type": "public-key", "id": id}))
                        .collect()
                };
                Some(list)
            }
            None => None,
        };
        let (ceremony_id, challenge) = self.new_ceremony("login", Some((&coll.name, "")), Json::Null).await?;
        let mut pk = json!({
            "rpId": self.webauthn_config().rp_id,
            "challenge": challenge,
            "timeout": CEREMONY_TTL_SECS * 1000,
            "userVerification": "required",
        });
        if let Some(a) = allow {
            pk["allowCredentials"] = Json::Array(a);
        }
        Ok(CeremonyOptions {
            ceremony_id,
            public_key: pk,
        })
    }

    /// Verifies an assertion against the stored credential. Returns the
    /// credential's owner and whether the user was verified.
    async fn verify_assertion(
        &self,
        ci: &ClientInfo,
        challenge: &str,
        cred: &AuthenticationCredential,
        require_uv: bool,
        only_user: Option<(&str, &str)>,
        only_mfa_credentials: bool,
    ) -> Result<(String, String, bool), AuthError> {
        let cid = cred.id.trim_end_matches('=');
        let r = read(
            self.data().backend(),
            "SELECT collection, user_id, user_handle, public_key, sign_count, use_as_mfa
             FROM _forgebase_webauthn_credentials WHERE credential_id = ?1",
            vec![t(cid)],
        )
        .await?;
        let Some(row) = first(&r) else { return Err(failed()) };
        let (coll, uid) = (row.str("collection"), row.str("user_id"));
        if only_user.is_some_and(|(c, u)| c != coll || u != uid) || (only_mfa_credentials && row.i64("use_as_mfa") != 1)
        {
            return Err(failed());
        }
        let actor = Subject::User {
            collection: coll.clone(),
            id: uid.clone(),
        };
        let reject = |reason: &'static str| {
            let this = self.clone();
            let ci = ci.clone();
            let actor = actor.clone();
            async move {
                let _ = this
                    .audit(
                        "webauthn.login",
                        false,
                        &ci,
                        "anonymous",
                        Some(&actor),
                        json!({"reason": reason}),
                    )
                    .await;
                failed()
            }
        };
        if let Some(h) = &cred.response.user_handle
            && !h.is_empty()
            && !ct_eq(h.trim_end_matches('=').as_bytes(), row.str("user_handle").as_bytes())
        {
            return Err(reject("user_handle").await);
        }
        let cd = dec(&cred.response.client_data_json)?;
        if self.check_client_data(&cd, "webauthn.get", challenge).is_err() {
            return Err(reject("client_data").await);
        }
        let auth_data = dec(&cred.response.authenticator_data)?;
        let Some(ad) = parse_auth_data(&auth_data) else {
            return Err(reject("authData").await);
        };
        if !self.rp_hash_ok(&ad) {
            return Err(reject("rp_id").await);
        }
        if !ad.up() {
            return Err(reject("user_presence").await);
        }
        if require_uv && !ad.uv() {
            return Err(reject("user_verification").await);
        }
        let Some(key) = load_key(&row.blob("public_key")) else {
            return Err(reject("stored_key").await);
        };
        let mut signed = auth_data.clone();
        signed.extend_from_slice(&Sha256::digest(&cd));
        if !key.verify_webauthn(&signed, &dec(&cred.response.signature)?) {
            return Err(reject("signature").await);
        }
        // Clone detection (WebAuthn §6.1.1): when either counter is non-zero
        // the new one must be strictly greater. Synced passkeys that always
        // report 0 pass.
        let stored = row.i64("sign_count");
        let new = i64::from(ad.sign_count);
        if (stored != 0 || new != 0) && new <= stored {
            let _ = self
                .audit(
                    "webauthn.clone_suspected",
                    false,
                    ci,
                    "anonymous",
                    Some(&actor),
                    json!({"credential_id": cid, "stored": stored, "received": new}),
                )
                .await;
            return Err(failed());
        }
        let mut tx = Tx::begin(self.data().backend()).await?;
        // Compare-and-set on the counter: two concurrent assertions with the
        // same counter cannot both pass.
        let moved = tx
            .exec(
                "UPDATE _forgebase_webauthn_credentials
                 SET sign_count = ?2, last_used_at = ?3, backup_state = ?4
                 WHERE credential_id = ?1 AND sign_count = ?5",
                vec![t(cid), i(new.max(stored)), i(now()), i(i64::from(ad.bs())), i(stored)],
            )
            .await?
            .rows_affected
            == 1;
        tx.commit().await?;
        if !moved {
            return Err(failed());
        }
        if !self.subject_exists(&actor).await? {
            return Err(failed());
        }
        Ok((coll, uid, ad.uv()))
    }

    /// Signs in with a passkey (first factor). User verification is
    /// required, so the session counts as multi-factor (`amr` `mfa`).
    pub async fn webauthn_login_verify(
        &self,
        ci: &ClientInfo,
        ceremony_id: &str,
        cred: &AuthenticationCredential,
        opts: &LoginOptions,
    ) -> Result<SessionTokens, AuthError> {
        self.limit_attempt(ci)?;
        let (challenge, _coll, _, _) = self.claim_ceremony("login", ceremony_id).await?;
        let (coll, uid, _uv) = self.verify_assertion(ci, &challenge, cred, true, None, false).await?;
        let subject = Subject::User {
            collection: coll,
            id: uid,
        };
        self.issue_session(&subject, AuthMethod::Passkey, true, ci, opts).await
    }

    // -----------------------------------------------------------------------
    // Passkey as second factor
    // -----------------------------------------------------------------------

    /// Options for answering an MFA challenge with a passkey.
    pub async fn webauthn_mfa_options(&self, ci: &ClientInfo, mfa_token: &str) -> Result<CeremonyOptions, AuthError> {
        self.limit_attempt(ci)?;
        let (_, subject, _) = self.peek_mfa_challenge(mfa_token).await?;
        let Subject::User { collection, id } = &subject else {
            return Err(bad_ceremony());
        };
        let creds: Vec<Json> = {
            let r = read(
                self.data().backend(),
                "SELECT credential_id FROM _forgebase_webauthn_credentials
                 WHERE collection = ?1 AND user_id = ?2 AND use_as_mfa = 1",
                vec![t(collection.clone()), t(id.clone())],
            )
            .await?;
            rows(&r)
                .map(|r| json!({"type": "public-key", "id": r.str("credential_id")}))
                .collect()
        };
        if creds.is_empty() {
            return Err(AuthError::Forbidden("no passkey is enabled as a second factor".into()));
        }
        let (ceremony_id, challenge) = self
            .new_ceremony(
                "mfa",
                Some((collection, id)),
                json!({"mfa_token_hash": self.mfa_hash(mfa_token)}),
            )
            .await?;
        Ok(CeremonyOptions {
            ceremony_id,
            public_key: json!({
                "rpId": self.webauthn_config().rp_id,
                "challenge": challenge,
                "timeout": CEREMONY_TTL_SECS * 1000,
                "allowCredentials": creds,
                "userVerification": "preferred",
            }),
        })
    }

    fn mfa_hash(&self, token: &str) -> String {
        self.keyring().token_hash("mfa_challenge", token)
    }

    /// `(hash, subject, detail)` of a live MFA challenge.
    async fn peek_mfa_challenge(&self, token: &str) -> Result<(String, Subject, Json), AuthError> {
        if token.is_empty() || token.len() > 256 {
            return Err(bad_ceremony());
        }
        let hash = self.mfa_hash(token);
        let r = read(
            self.data().backend(),
            "SELECT subject_type, collection, subject_id, detail FROM _forgebase_auth_one_time
             WHERE token_hash = ?1 AND purpose = 'mfa_challenge' AND used_at IS NULL AND expires_at > ?2",
            vec![t(hash.clone()), i(now())],
        )
        .await?;
        let Some(row) = first(&r) else {
            return Err(bad_ceremony());
        };
        let subject = match (row.str("subject_type").as_str(), row.opt_str("collection")) {
            ("user", Some(collection)) => Subject::User {
                collection,
                id: row.str("subject_id"),
            },
            _ => return Err(bad_ceremony()),
        };
        let detail = row
            .opt_str("detail")
            .and_then(|d| serde_json::from_str(&d).ok())
            .unwrap_or(Json::Null);
        Ok((hash, subject, detail))
    }

    /// Completes an MFA challenge with a passkey.
    pub async fn webauthn_mfa_verify(
        &self,
        ci: &ClientInfo,
        mfa_token: &str,
        ceremony_id: &str,
        cred: &AuthenticationCredential,
    ) -> Result<SessionTokens, AuthError> {
        self.limit_attempt(ci)?;
        let (hash, subject, detail) = self.peek_mfa_challenge(mfa_token).await?;
        let (challenge, c, u, cdetail) = self.claim_ceremony("mfa", ceremony_id).await?;
        // The ceremony must have been opened for this very challenge.
        if cdetail.get("mfa_token_hash").and_then(Json::as_str) != Some(hash.as_str()) {
            return Err(bad_ceremony());
        }
        let Subject::User { collection, id } = &subject else {
            return Err(bad_ceremony());
        };
        if c.as_deref() != Some(collection.as_str()) || u.as_deref() != Some(id.as_str()) {
            return Err(bad_ceremony());
        }
        self.verify_assertion(ci, &challenge, cred, false, Some((collection, id)), true)
            .await?;
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
            return Err(bad_ceremony());
        }
        self.audit(
            "mfa.verify",
            true,
            ci,
            &subject.actor(),
            Some(&subject),
            json!({"factor": "passkey"}),
        )
        .await?;
        let method = match detail.get("method").and_then(Json::as_str) {
            Some("magic_link") => AuthMethod::MagicLink,
            Some("email_otp") => AuthMethod::EmailOtp,
            Some("oauth") => AuthMethod::Oauth,
            _ => AuthMethod::Password,
        };
        let opts = LoginOptions {
            cookie: detail.get("cookie").and_then(Json::as_bool).unwrap_or(false),
            device_name: detail.get("device_name").and_then(Json::as_str).map(str::to_owned),
        };
        self.issue_session(&subject, method, true, ci, &opts).await
    }
}

fn session_user(caller: &Authenticated) -> Result<(String, String), AuthError> {
    match (&caller.subject, caller.session_id()) {
        (Subject::User { collection, id }, Some(_)) => Ok((collection.clone(), id.clone())),
        _ => Err(AuthError::Forbidden("passkeys are managed by signed-in users".into())),
    }
}

fn cred_row(row: crate::store::Row<'_>) -> CredentialView {
    CredentialView {
        id: row.str("credential_id"),
        name: row.opt_str("name"),
        user_verified: row.i64("uv") == 1,
        use_as_mfa: row.i64("use_as_mfa") == 1,
        backup_eligible: row.i64("backup_eligible") == 1,
        sign_count: row.i64("sign_count"),
        created_at: rfc3339(row.i64("created_at")),
        last_used_at: row.opt_i64("last_used_at").map(rfc3339),
    }
}
