//! ID token validation (OpenID Connect Core §3.1.3.7) and a JWKS cache.
//!
//! Accepted algorithms: `RS256` and `ES256` only (never `none`, never an HMAC
//! algorithm, so the "public key used as HMAC secret" confusion cannot occur).
//! The key is chosen by `kid` from the provider's JWKS; a token with an
//! unknown `kid` makes the cache refresh once, but not more often than every
//! 30 seconds, so a stream of forged `kid`s cannot hammer the provider.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::Value;

use super::http_client::{HttpClient, HttpRequest};
use super::sig::PublicKey;
use crate::AuthError;

const JWKS_TTL: Duration = Duration::from_secs(3600);
const JWKS_MIN_REFRESH: Duration = Duration::from_secs(30);
/// Clock skew allowed on `exp` and `iat`.
pub const LEEWAY_SECS: i64 = 60;

#[derive(Debug, Clone)]
struct Key {
    kid: Option<String>,
    alg: Option<String>,
    key: PublicKey,
}

#[derive(Debug)]
struct Entry {
    keys: Vec<Key>,
    fetched: Instant,
}

/// Per-URL cache of provider signing keys.
#[derive(Debug, Default)]
pub struct JwksCache {
    entries: Mutex<HashMap<String, Entry>>,
}

fn b64(v: &Value, k: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(v.get(k)?.as_str()?).ok()
}

fn parse_jwks(doc: &Value) -> Vec<Key> {
    doc.get("keys")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|k| {
                    let key = match k.get("kty")?.as_str()? {
                        "RSA" => PublicKey::rsa(&b64(k, "n")?, &b64(k, "e")?)?,
                        "EC" if k.get("crv")?.as_str()? == "P-256" => PublicKey::p256(&b64(k, "x")?, &b64(k, "y")?)?,
                        _ => return None,
                    };
                    if k.get("use").and_then(Value::as_str).is_some_and(|u| u != "sig") {
                        return None;
                    }
                    Some(Key {
                        kid: k.get("kid").and_then(Value::as_str).map(str::to_owned),
                        alg: k.get("alg").and_then(Value::as_str).map(str::to_owned),
                        key,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

impl JwksCache {
    /// `(key if known, age of the cached set)`; `None` when never fetched.
    fn lookup(&self, url: &str, kid: Option<&str>, alg: &str) -> Option<(Option<PublicKey>, Duration)> {
        let g = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let e = g.get(url)?;
        let found = e
            .keys
            .iter()
            .find(|k| k.kid.as_deref() == kid && k.alg.as_deref().is_none_or(|a| a == alg))
            .map(|k| k.key.clone());
        Some((found, e.fetched.elapsed()))
    }

    async fn refresh(&self, http: &dyn HttpClient, url: &str) -> Result<(), AuthError> {
        let res = http.send(HttpRequest::get(url)).await?;
        if res.status != 200 {
            return Err(AuthError::Provider(
                "could not fetch the provider's signing keys".into(),
            ));
        }
        let keys = parse_jwks(&res.json()?);
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).insert(
            url.to_owned(),
            Entry {
                keys,
                fetched: Instant::now(),
            },
        );
        Ok(())
    }

    /// The key for `kid`: from the cache while fresh; otherwise refetched,
    /// but an unknown `kid` refetches at most every 30 seconds.
    pub async fn key(
        &self,
        http: &dyn HttpClient,
        url: &str,
        kid: Option<&str>,
        alg: &str,
    ) -> Result<PublicKey, AuthError> {
        let unknown = AuthError::InvalidToken("ID token signed with an unknown key");
        match self.lookup(url, kid, alg) {
            Some((Some(k), age)) if age <= JWKS_TTL => return Ok(k),
            Some((None, age)) if age <= JWKS_MIN_REFRESH => return Err(unknown),
            _ => {}
        }
        self.refresh(http, url).await?;
        match self.lookup(url, kid, alg) {
            Some((Some(k), _)) => Ok(k),
            _ => Err(unknown),
        }
    }
}

/// What the caller expects of an ID token.
#[derive(Debug, Clone)]
pub struct Expect<'a> {
    /// Accepted `iss` values.
    pub issuers: &'a [String],
    /// Our client id (must be in `aud`).
    pub audience: &'a str,
    /// The nonce sent in the authorization request.
    pub nonce: &'a str,
    /// Current Unix time.
    pub now: i64,
}

/// The verified claims we use.
#[derive(Debug, Clone, Default)]
pub struct Claims {
    /// Subject (stable provider user id).
    pub sub: String,
    /// Email, if present.
    pub email: Option<String>,
    /// Whether the provider vouches for the email.
    pub email_verified: bool,
    /// Display name.
    pub name: Option<String>,
    /// Picture URL.
    pub picture: Option<String>,
    /// All claims, for provider-specific reading.
    pub raw: serde_json::Map<String, Value>,
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => s == "true",
        _ => false,
    }
}

/// Validates `token` against `jwks_url` and `expect`.
pub async fn validate(
    http: &dyn HttpClient,
    cache: &JwksCache,
    jwks_url: &str,
    token: &str,
    expect: &Expect<'_>,
) -> Result<Claims, AuthError> {
    let bad = |m: &'static str| AuthError::InvalidToken(m);
    if token.len() > 16 * 1024 {
        return Err(bad("invalid ID token"));
    }
    let mut parts = token.split('.');
    let (h, p, s) = (
        parts.next().ok_or(bad("invalid ID token"))?,
        parts.next().ok_or(bad("invalid ID token"))?,
        parts.next().ok_or(bad("invalid ID token"))?,
    );
    if parts.next().is_some() {
        return Err(bad("invalid ID token"));
    }
    let dec = |x: &str| URL_SAFE_NO_PAD.decode(x).map_err(|_| bad("invalid ID token"));
    let header: Value = serde_json::from_slice(&dec(h)?).map_err(|_| bad("invalid ID token"))?;
    let alg = header.get("alg").and_then(Value::as_str).unwrap_or_default();
    if alg != "RS256" && alg != "ES256" {
        return Err(bad("unsupported ID token algorithm"));
    }
    let kid = header.get("kid").and_then(Value::as_str);
    let key = cache.key(http, jwks_url, kid, alg).await?;
    let signing_input = format!("{h}.{p}");
    let sig = dec(s)?;
    let key_alg_ok = matches!(
        (&key, alg),
        (PublicKey::Rsa { .. }, "RS256") | (PublicKey::Es256(_), "ES256")
    );
    if !key_alg_ok || !key.verify_jws(signing_input.as_bytes(), &sig) {
        return Err(bad("ID token signature is invalid"));
    }
    let c: Value = serde_json::from_slice(&dec(p)?).map_err(|_| bad("invalid ID token"))?;
    let obj = c.as_object().ok_or(bad("invalid ID token"))?;
    let iss = obj.get("iss").and_then(Value::as_str).unwrap_or_default();
    if !expect.issuers.iter().any(|i| i == iss) {
        return Err(bad("ID token issuer mismatch"));
    }
    let aud_ok = match obj.get("aud") {
        Some(Value::String(a)) => a == expect.audience,
        Some(Value::Array(a)) => {
            a.iter().any(|v| v.as_str() == Some(expect.audience))
                && (a.len() == 1 || obj.get("azp").and_then(Value::as_str) == Some(expect.audience))
        }
        _ => false,
    };
    if !aud_ok {
        return Err(bad("ID token audience mismatch"));
    }
    let exp = obj
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or(bad("ID token has no expiry"))?;
    if exp + LEEWAY_SECS < expect.now {
        return Err(bad("ID token expired"));
    }
    if obj
        .get("iat")
        .and_then(Value::as_i64)
        .is_some_and(|iat| iat - LEEWAY_SECS > expect.now)
    {
        return Err(bad("ID token issued in the future"));
    }
    if obj
        .get("nbf")
        .and_then(Value::as_i64)
        .is_some_and(|nbf| nbf - LEEWAY_SECS > expect.now)
    {
        return Err(bad("ID token not yet valid"));
    }
    match obj.get("nonce").and_then(Value::as_str) {
        Some(n) if crate::crypto::ct_eq(n.as_bytes(), expect.nonce.as_bytes()) => {}
        _ => return Err(bad("ID token nonce mismatch")),
    }
    let sub = obj.get("sub").and_then(Value::as_str).unwrap_or_default();
    if sub.is_empty() || sub.len() > 255 {
        return Err(bad("ID token has no subject"));
    }
    let text = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_owned);
    Ok(Claims {
        sub: sub.to_owned(),
        email: text("email"),
        email_verified: truthy(obj.get("email_verified")),
        name: text("name"),
        picture: text("picture"),
        raw: obj.clone(),
    })
}
