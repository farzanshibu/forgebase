//! ES256 signing keys of the identity provider, one active key per realm.
//!
//! A key is a P-256 private scalar sealed with XChaCha20-Poly1305 under the
//! master key (associated data `oidc:<kid>`). The active key signs ID tokens
//! and access tokens; it is rotated after [`IdpConfig::key_rotation`]
//! (checked when signing) or on demand. A retired key stays in the JWKS and
//! keeps verifying until every token it signed has expired, then it is
//! deleted. A verifier meeting an unknown `kid` reloads the realm's keys once,
//! so a rotation by another process is picked up.
//!
//! [`IdpConfig::key_rotation`]: super::IdpConfig::key_rotation

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use forgebase_data::db::{DbError, Value};
use p256::ecdsa::signature::{Signer, Verifier};
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value as Json, json};

use crate::crypto::{self, b64url};
use crate::service::{ClientInfo, audit_in, require_admin};
use crate::store::{Tx, first, i, read, rows, t};
use crate::{AuthError, AuthService, Authenticated};

/// A loaded signing key.
#[derive(Clone)]
pub(crate) struct OidcKey {
    pub(crate) kid: String,
    scalar: Vec<u8>,
    x: String,
    y: String,
    pub(crate) active: bool,
    created_at: i64,
    verify_until: Option<i64>,
}

impl std::fmt::Debug for OidcKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcKey")
            .field("kid", &self.kid)
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

impl OidcKey {
    fn signing_key(&self) -> Result<SigningKey, AuthError> {
        SigningKey::from_slice(&self.scalar).map_err(|_| AuthError::Internal("corrupt signing key".into()))
    }

    /// Signs `claims` as a compact JWS (`alg: ES256`, header `typ`).
    pub(crate) fn sign(&self, typ: &str, claims: &Json) -> Result<String, AuthError> {
        let header = json!({"alg": "ES256", "typ": typ, "kid": self.kid});
        let h = b64url(&serde_json::to_vec(&header).map_err(|e| AuthError::Internal(e.to_string()))?);
        let p = b64url(&serde_json::to_vec(claims).map_err(|e| AuthError::Internal(e.to_string()))?);
        let input = format!("{h}.{p}");
        let sig: Signature = self.signing_key()?.sign(input.as_bytes());
        Ok(format!("{input}.{}", b64url(&sig.to_bytes())))
    }

    fn jwk(&self) -> Json {
        json!({"kty": "EC", "crv": "P-256", "x": self.x, "y": self.y, "kid": self.kid, "alg": "ES256", "use": "sig"})
    }
}

/// A verified token: its header `typ` and claims.
#[derive(Debug, Clone)]
pub(crate) struct Verified {
    pub(crate) typ: String,
    pub(crate) claims: Json,
}

fn aad(kid: &str) -> Vec<u8> {
    format!("oidc:{kid}").into_bytes()
}

/// Splits a compact JWS into its parts and decodes the header.
fn split(token: &str) -> Option<(&str, &str, &str, Json)> {
    if token.len() > 8192 {
        return None;
    }
    let mut parts = token.splitn(4, '.');
    let (h, p, s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let header: Json = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h).ok()?).ok()?;
    Some((h, p, s, header))
}

/// Whether a token is a JWS signed by this identity provider's algorithm
/// (cheap check on the header; no verification).
pub(crate) fn looks_like_idp_jwt(token: &str) -> bool {
    split(token).is_some_and(|(_, _, _, h)| h["alg"] == "ES256")
}

impl AuthService {
    async fn load_oidc_keys(&self, realm: &str) -> Result<Vec<OidcKey>, AuthError> {
        let r = read(
            self.data().backend(),
            "SELECT kid, private_enc, public_x, public_y, status, created_at, verify_until
             FROM _forgebase_oidc_keys WHERE realm = ?1 ORDER BY created_at DESC",
            vec![t(realm)],
        )
        .await?;
        let mut keys = Vec::new();
        for row in rows(&r) {
            let kid = row.str("kid");
            let scalar = self.keyring().open(&row.blob("private_enc"), &aad(&kid))?;
            keys.push(OidcKey {
                scalar,
                x: row.str("public_x"),
                y: row.str("public_y"),
                active: row.str("status") == "active",
                created_at: row.i64("created_at"),
                verify_until: row.opt_i64("verify_until"),
                kid,
            });
        }
        self.idp_state()
            .keys
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(realm.to_owned(), keys.clone());
        Ok(keys)
    }

    fn cached_oidc_keys(&self, realm: &str) -> Option<Vec<OidcKey>> {
        self.idp_state()
            .keys
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(realm)
            .cloned()
    }

    /// Makes sure the realm has an active signing key; rotates it when it is
    /// older than the rotation period or when `force` is set.
    pub(crate) async fn ensure_oidc_key(&self, realm: &str, force: bool) -> Result<(), AuthError> {
        let cfg = &self.config().idp;
        let now = self.now();
        let rotation = cfg.key_rotation.as_secs() as i64;
        let grace = cfg.access_token_ttl.max(cfg.id_token_ttl).as_secs() as i64 + 300;
        let mut tx = Tx::begin(self.data().backend()).await?;
        tx.exec(
            "DELETE FROM _forgebase_oidc_keys WHERE realm = ?1 AND status = 'retired' AND verify_until < ?2",
            vec![t(realm), i(now)],
        )
        .await?;
        let r = tx
            .read(
                "SELECT kid, created_at FROM _forgebase_oidc_keys WHERE realm = ?1 AND status = 'active'",
                vec![t(realm)],
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
                    "UPDATE _forgebase_oidc_keys SET status = 'retired', verify_until = ?2 WHERE kid = ?1",
                    vec![t(kid.clone()), i(now + grace)],
                )
                .await?;
            }
            let sk = loop {
                let bytes = crypto::random_bytes(32)?;
                if let Ok(k) = SigningKey::from_slice(&bytes) {
                    break k;
                }
            };
            let point = sk.verifying_key().to_sec1_point(false);
            let (Some(x), Some(y)) = (point.x(), point.y()) else {
                return Err(AuthError::Internal("could not encode the public key".into()));
            };
            let kid = crypto::random_id(20)?;
            let sealed = self.keyring().seal(&sk.to_bytes(), &aad(&kid))?;
            tx.exec(
                "INSERT INTO _forgebase_oidc_keys (kid, realm, alg, private_enc, public_x, public_y, status, created_at)
                 VALUES (?1, ?2, 'ES256', ?3, ?4, ?5, 'active', ?6)",
                vec![
                    t(kid.clone()),
                    t(realm),
                    Value::Blob(sealed),
                    t(b64url(x)),
                    t(b64url(y)),
                    i(now),
                ],
            )
            .await?;
            audit_in(
                &mut tx,
                "oidc.key_rotated",
                true,
                &ClientInfo {
                    realm: Some(realm.to_owned()),
                    ..ClientInfo::default()
                },
                "system",
                None,
                json!({"kid": kid, "realm": realm, "forced": force}),
            )
            .await?;
        }
        match tx.commit().await {
            Ok(()) => {}
            // Another process rotated at the same moment: use its key.
            Err(AuthError::Db(DbError::Constraint { .. })) => {}
            Err(e) => return Err(e),
        }
        self.load_oidc_keys(realm).await.map(|_| ())
    }

    /// The realm's active signing key (created or rotated when due).
    pub(crate) async fn active_oidc_key(&self, realm: &str) -> Result<OidcKey, AuthError> {
        let rotation = self.config().idp.key_rotation.as_secs() as i64;
        let now = self.now();
        let current = self
            .cached_oidc_keys(realm)
            .and_then(|ks| ks.into_iter().find(|k| k.active));
        match current {
            Some(k) if now - k.created_at < rotation => Ok(k),
            _ => {
                self.ensure_oidc_key(realm, false).await?;
                self.cached_oidc_keys(realm)
                    .and_then(|ks| ks.into_iter().find(|k| k.active))
                    .ok_or_else(|| AuthError::Internal("no active OIDC signing key".into()))
            }
        }
    }

    /// The realm's JSON Web Key Set: the active key and retired keys that
    /// still verify.
    pub async fn oidc_jwks(&self, realm: &str) -> Result<Json, AuthError> {
        self.active_oidc_key(realm).await?;
        let now = self.now();
        let keys: Vec<Json> = self
            .cached_oidc_keys(realm)
            .unwrap_or_default()
            .iter()
            .filter(|k| k.active || k.verify_until.is_some_and(|v| v >= now))
            .map(OidcKey::jwk)
            .collect();
        Ok(json!({ "keys": keys }))
    }

    /// Verifies a JWS signed by one of the realm's keys (signature and key
    /// validity only; claim checks are the caller's).
    pub(crate) async fn verify_idp_jwt(&self, realm: &str, token: &str) -> Result<Option<Verified>, AuthError> {
        let Some((h, p, s, header)) = split(token) else {
            return Ok(None);
        };
        if header["alg"] != "ES256" {
            return Ok(None);
        }
        let Some(kid) = header["kid"].as_str() else {
            return Ok(None);
        };
        let find = |ks: Option<Vec<OidcKey>>| ks.and_then(|ks| ks.into_iter().find(|k| k.kid == kid));
        let key = match find(self.cached_oidc_keys(realm)) {
            Some(k) => Some(k),
            None => find(Some(self.load_oidc_keys(realm).await?)),
        };
        let Some(key) = key else {
            return Ok(None);
        };
        if !key.active && key.verify_until.is_some_and(|v| v < self.now()) {
            return Ok(None);
        }
        let Ok(sig) = URL_SAFE_NO_PAD.decode(s) else {
            return Ok(None);
        };
        let Ok(sig) = Signature::from_slice(&sig) else {
            return Ok(None);
        };
        let vk = *key.signing_key()?.verifying_key();
        let mut input = Vec::with_capacity(h.len() + p.len() + 1);
        input.extend_from_slice(h.as_bytes());
        input.push(b'.');
        input.extend_from_slice(p.as_bytes());
        if vk.verify(&input, &sig).is_err() {
            return Ok(None);
        }
        let Some(claims) = URL_SAFE_NO_PAD
            .decode(p)
            .ok()
            .and_then(|b| serde_json::from_slice::<Json>(&b).ok())
            .filter(Json::is_object)
        else {
            return Ok(None);
        };
        Ok(Some(Verified {
            typ: header["typ"].as_str().unwrap_or_default().to_owned(),
            claims,
        }))
    }

    /// Rotates a realm's OIDC signing key now (administrators). Tokens signed
    /// with the old key keep verifying until they expire.
    pub async fn rotate_oidc_key(
        &self,
        caller: &Authenticated,
        realm: &str,
        ci: &ClientInfo,
    ) -> Result<String, AuthError> {
        require_admin(caller)?;
        self.realm_identity(realm)?;
        self.ensure_oidc_key(realm, true).await?;
        let kid = self.active_oidc_key(realm).await?.kid;
        self.audit(
            "oidc.key_rotate_requested",
            true,
            ci,
            &caller.subject.actor(),
            None,
            json!({"realm": realm, "kid": kid}),
        )
        .await?;
        Ok(kid)
    }
}
