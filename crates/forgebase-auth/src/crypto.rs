//! Cryptographic building blocks: secure randomness, the master keyring,
//! keyed token hashing, authenticated encryption, constant-time comparison,
//! HS256 tokens and TOTP (RFC 6238).
//!
//! Nothing here logs. Every comparison of a secret against a stored value is
//! constant-time.

use std::fs;
use std::io::Write;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hmac::{Hmac, Mac};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sha1::Sha1;
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::AuthError;

type HmacSha256 = Hmac<Sha256>;
type HmacSha1 = Hmac<Sha1>;

/// Length of the master key in bytes.
pub const MASTER_KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;

/// Fills `buf` from the operating system's CSPRNG.
pub fn fill_random(buf: &mut [u8]) -> Result<(), AuthError> {
    getrandom::fill(buf).map_err(|e| AuthError::Internal(format!("system random generator failed: {e}")))
}

/// `n` random bytes.
pub fn random_bytes(n: usize) -> Result<Vec<u8>, AuthError> {
    let mut v = vec![0u8; n];
    fill_random(&mut v)?;
    Ok(v)
}

/// A URL-safe random token with `bytes` bytes of entropy.
pub fn random_token(bytes: usize) -> Result<String, AuthError> {
    Ok(URL_SAFE_NO_PAD.encode(random_bytes(bytes)?))
}

/// A random identifier of `len` characters from `[a-z0-9]` (unbiased).
pub fn random_id(len: usize) -> Result<String, AuthError> {
    random_from_alphabet(len, b"abcdefghijklmnopqrstuvwxyz0123456789")
}

/// `len` random decimal digits (unbiased), e.g. an email OTP.
pub fn random_digits(len: usize) -> Result<String, AuthError> {
    random_from_alphabet(len, b"0123456789")
}

/// `len` characters from `alphabet` by rejection sampling (no modulo bias).
pub fn random_from_alphabet(len: usize, alphabet: &[u8]) -> Result<String, AuthError> {
    let n = alphabet.len();
    debug_assert!(n > 0 && n <= 256);
    let limit = 256 - (256 % n);
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 64];
    while out.len() < len {
        fill_random(&mut buf)?;
        for &b in &buf {
            if (b as usize) < limit {
                out.push(alphabet[b as usize % n] as char);
                if out.len() == len {
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// Constant-time equality of two byte strings (length may leak).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

/// Base64url without padding.
pub fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn hmac256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    // HMAC accepts keys of any length; this cannot fail.
    let mut mac =
        <HmacSha256 as KeyInit>::new_from_slice(key).unwrap_or_else(|_| unreachable!("HMAC takes any key length"));
    for p in parts {
        mac.update(&(p.len() as u64).to_be_bytes());
        mac.update(p);
    }
    mac.finalize().into_bytes().into()
}

/// Keys derived from the data directory's master key.
///
/// * `pepper`: HMAC key for hashing tokens, codes and API-key secrets.
/// * `enc`: XChaCha20-Poly1305 key for secrets that must be recovered
///   (signing keys, TOTP seeds).
#[derive(Clone)]
pub struct Keyring {
    pepper: [u8; 32],
    enc: [u8; 32],
}

impl std::fmt::Debug for Keyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Keyring(..)")
    }
}

impl Keyring {
    /// Derives the keyring from a master key.
    pub fn from_master(master: &[u8]) -> Self {
        Self {
            pepper: hmac256(master, &[b"forgebase/auth/pepper/v1"]),
            enc: hmac256(master, &[b"forgebase/auth/encryption/v1"]),
        }
    }

    /// Loads the master key from `path`, creating it (0600, 32 random bytes,
    /// base64) if it does not exist.
    pub fn load_or_create(path: &Path) -> Result<Self, AuthError> {
        let master = load_or_create_master_key(path)?;
        Ok(Self::from_master(&master))
    }

    /// Keyed hash of a token for storage and lookup: hex HMAC-SHA256 under the
    /// pepper, domain-separated by `purpose`.
    pub fn token_hash(&self, purpose: &str, token: &str) -> String {
        hex(&hmac256(&self.pepper, &[purpose.as_bytes(), token.as_bytes()]))
    }

    /// Checks `token` against a stored [`Keyring::token_hash`] in constant time.
    pub fn verify_token_hash(&self, purpose: &str, token: &str, stored: &str) -> bool {
        ct_eq(self.token_hash(purpose, token).as_bytes(), stored.as_bytes())
    }

    /// MAC of an admin known-device token (FB-80): binds the admin's email
    /// and the issue time.
    pub fn admin_device_mac(&self, email: &str, issued: i64) -> String {
        b64url(&hmac256(
            &self.pepper,
            &[b"admin-device", &issued.to_be_bytes(), email.as_bytes()],
        ))
    }

    /// The CSRF token bound to a session id.
    pub fn csrf_token(&self, session_id: &str) -> String {
        b64url(&hmac256(&self.pepper, &[b"csrf", session_id.as_bytes()]))
    }

    /// Encrypts `plain` with a random nonce; output is nonce || ciphertext.
    /// `aad` binds the ciphertext to its row (e.g. the key id).
    pub fn seal(&self, plain: &[u8], aad: &[u8]) -> Result<Vec<u8>, AuthError> {
        let cipher = XChaCha20Poly1305::new_from_slice(&self.enc)
            .map_err(|_| AuthError::Internal("bad encryption key length".into()))?;
        let mut nonce = [0u8; NONCE_LEN];
        fill_random(&mut nonce)?;
        let ct = cipher
            .encrypt(
                &XNonce::from(nonce),
                chacha20poly1305::aead::Payload { msg: plain, aad },
            )
            .map_err(|_| AuthError::Internal("encryption failed".into()))?;
        let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }

    /// Decrypts the output of [`Keyring::seal`].
    pub fn open(&self, sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>, AuthError> {
        if sealed.len() < NONCE_LEN + 16 {
            return Err(AuthError::Internal("sealed secret is truncated".into()));
        }
        let cipher = XChaCha20Poly1305::new_from_slice(&self.enc)
            .map_err(|_| AuthError::Internal("bad encryption key length".into()))?;
        let (nonce, ct) = sealed.split_at(NONCE_LEN);
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| AuthError::Internal("bad nonce".into()))?;
        cipher
            .decrypt(&XNonce::from(nonce), chacha20poly1305::aead::Payload { msg: ct, aad })
            .map_err(|_| {
                AuthError::Internal(
                    "cannot decrypt a stored secret: the master key does not match this database".into(),
                )
            })
    }
}

fn load_or_create_master_key(path: &Path) -> Result<Vec<u8>, AuthError> {
    let io = |e: std::io::Error| AuthError::Internal(format!("master key {}: {e}", path.display()));
    match fs::read_to_string(path) {
        Ok(text) => {
            let key = STANDARD
                .decode(text.trim())
                .map_err(|_| AuthError::Internal(format!("master key {} is not valid base64", path.display())))?;
            if key.len() != MASTER_KEY_LEN {
                return Err(AuthError::Internal(format!(
                    "master key {} must be {MASTER_KEY_LEN} bytes",
                    path.display()
                )));
            }
            Ok(key)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent() {
                fs::create_dir_all(dir).map_err(io)?;
            }
            let key = random_bytes(MASTER_KEY_LEN)?;
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            match opts.open(path) {
                Ok(mut f) => {
                    f.write_all(STANDARD.encode(&key).as_bytes()).map_err(io)?;
                    f.write_all(b"\n").map_err(io)?;
                    f.sync_all().map_err(io)?;
                    tracing::info!(path = %path.display(), "created auth master key");
                    Ok(key)
                }
                // Another process created it first: use theirs.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => load_or_create_master_key(path),
                Err(e) => Err(io(e)),
            }
        }
        Err(e) => Err(io(e)),
    }
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}

// ---------------------------------------------------------------------------
// HS256 tokens (a strict subset of JWT)
// ---------------------------------------------------------------------------

/// Signs `claims` as a compact JWS with `alg: HS256` and `kid`.
pub fn jwt_sign<T: Serialize>(kid: &str, secret: &[u8], claims: &T) -> Result<String, AuthError> {
    let header = serde_json::json!({ "alg": "HS256", "typ": "JWT", "kid": kid });
    let h = b64url(&serde_json::to_vec(&header).map_err(|e| AuthError::Internal(e.to_string()))?);
    let p = b64url(&serde_json::to_vec(claims).map_err(|e| AuthError::Internal(e.to_string()))?);
    let signing_input = format!("{h}.{p}");
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(secret).map_err(|_| AuthError::Internal("bad key".into()))?;
    mac.update(signing_input.as_bytes());
    let sig = b64url(&mac.finalize().into_bytes());
    Ok(format!("{signing_input}.{sig}"))
}

/// Parsed but unverified header fields of a token.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct JwtHeader {
    /// Algorithm; only `HS256` is accepted.
    pub alg: String,
    /// Key id.
    pub kid: String,
}

/// Splits a token and returns its header (no verification yet).
pub fn jwt_header(token: &str) -> Option<JwtHeader> {
    if token.len() > 4096 {
        return None;
    }
    let mut parts = token.split('.');
    let h = parts.next()?;
    let header: JwtHeader = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h).ok()?).ok()?;
    (header.alg == "HS256").then_some(header)
}

/// Verifies the signature with `secret` (constant time) and decodes claims.
/// Expiry and other claim checks are the caller's.
pub fn jwt_verify<T: DeserializeOwned>(token: &str, secret: &[u8]) -> Option<T> {
    let mut parts = token.splitn(4, '.');
    let (h, p, s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let header: JwtHeader = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(h).ok()?).ok()?;
    if header.alg != "HS256" {
        return None;
    }
    let sig = URL_SAFE_NO_PAD.decode(s).ok()?;
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(secret).ok()?;
    mac.update(h.as_bytes());
    mac.update(b".");
    mac.update(p.as_bytes());
    mac.verify_slice(&sig).ok()?;
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(p).ok()?).ok()
}

// ---------------------------------------------------------------------------
// TOTP (RFC 6238, HMAC-SHA1, 6 digits, 30 s) and base32 (RFC 4648)
// ---------------------------------------------------------------------------

/// TOTP period in seconds.
pub const TOTP_PERIOD: u64 = 30;
/// TOTP code length.
pub const TOTP_DIGITS: u32 = 6;

/// The HOTP value for counter `step` (RFC 4226).
pub fn hotp(secret: &[u8], step: u64) -> String {
    let mut mac =
        <HmacSha1 as KeyInit>::new_from_slice(secret).unwrap_or_else(|_| unreachable!("HMAC takes any key length"));
    mac.update(&step.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = (digest[digest.len() - 1] & 0x0f) as usize;
    let bin = ((u32::from(digest[offset]) & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    format!("{:0width$}", bin % 10u32.pow(TOTP_DIGITS), width = TOTP_DIGITS as usize)
}

/// Checks a TOTP `code` at Unix time `now` with ±`skew` steps. Returns the
/// matched step, which must be greater than `last_step` (no replay).
pub fn totp_check(secret: &[u8], code: &str, now: u64, skew: u64, last_step: u64) -> Option<u64> {
    let code = code.trim();
    if code.len() != TOTP_DIGITS as usize || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let current = now / TOTP_PERIOD;
    let mut found = None;
    // Check every candidate (no early exit) so timing does not depend on which matched.
    for step in current.saturating_sub(skew)..=current + skew {
        if ct_eq(hotp(secret, step).as_bytes(), code.as_bytes()) && step > last_step {
            found = Some(step);
        }
    }
    found
}

/// RFC 4648 base32 without padding (authenticator apps' secret format).
pub fn base32_encode(bytes: &[u8]) -> String {
    const A: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u64 = 0;
    let mut bits = 0u32;
    for &b in bytes {
        buffer = (buffer << 8) | u64::from(b);
        bits += 8;
        while bits >= 5 {
            out.push(A[((buffer >> (bits - 5)) & 31) as usize] as char);
            bits -= 5;
        }
    }
    if bits > 0 {
        out.push(A[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// Decodes [`base32_encode`] output (case-insensitive, padding and spaces ignored).
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut buffer: u64 = 0;
    let mut bits = 0u32;
    for c in s.bytes().filter(|c| *c != b'=' && *c != b' ') {
        let v = match c.to_ascii_uppercase() {
            c @ b'A'..=b'Z' => c - b'A',
            c @ b'2'..=b'7' => c - b'2' + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | u64::from(v);
        bits += 5;
        if bits >= 8 {
            out.push((buffer >> (bits - 8)) as u8);
            bits -= 8;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6238_sha1_vectors() {
        // RFC 6238 Appendix B, SHA1 seed "12345678901234567890", 8 digits truncated to 6.
        let seed = b"12345678901234567890";
        assert_eq!(hotp(seed, 59 / 30), "287082");
        assert_eq!(hotp(seed, 1_111_111_109 / 30), "081804");
        assert_eq!(hotp(seed, 1_234_567_890 / 30), "005924");
        assert_eq!(hotp(seed, 2_000_000_000 / 30), "279037");
    }

    #[test]
    fn totp_rejects_replay_and_garbage() {
        let seed = b"12345678901234567890";
        let now = 1_111_111_109;
        let step = totp_check(seed, "081804", now, 1, 0).unwrap();
        assert_eq!(step, now / 30);
        assert!(totp_check(seed, "081804", now, 1, step).is_none(), "replay");
        assert!(totp_check(seed, "08180", now, 1, 0).is_none());
        assert!(totp_check(seed, "abcdef", now, 1, 0).is_none());
        assert!(totp_check(seed, "081804", now + 120, 1, 0).is_none(), "too old");
    }

    #[test]
    fn base32_round_trip() {
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        for n in 0..40 {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(base32_decode(&base32_encode(&data)).unwrap(), data);
        }
        assert!(base32_decode("not base32!").is_none());
    }

    #[test]
    fn jwt_round_trip_and_tamper() {
        let claims = serde_json::json!({"sub": "u1", "exp": 10});
        let t = jwt_sign("k1", b"secret-key", &claims).unwrap();
        assert_eq!(jwt_header(&t).unwrap().kid, "k1");
        let back: serde_json::Value = jwt_verify(&t, b"secret-key").unwrap();
        assert_eq!(back, claims);
        assert!(jwt_verify::<serde_json::Value>(&t, b"other-key").is_none());
        let mut parts: Vec<&str> = t.split('.').collect();
        let forged = b64url(br#"{"sub":"admin","exp":10}"#);
        parts[1] = &forged;
        assert!(jwt_verify::<serde_json::Value>(&parts.join("."), b"secret-key").is_none());
        // alg=none is never accepted.
        let none = format!(
            "{}.{}.",
            b64url(br#"{"alg":"none","kid":"k1"}"#),
            b64url(br#"{"sub":"x"}"#)
        );
        assert!(jwt_header(&none).is_none());
        assert!(jwt_verify::<serde_json::Value>(&none, b"secret-key").is_none());
    }

    #[test]
    fn sealing_round_trips_and_binds_aad() {
        let k = Keyring::from_master(&[7u8; 32]);
        let sealed = k.seal(b"hello", b"row-1").unwrap();
        assert_eq!(k.open(&sealed, b"row-1").unwrap(), b"hello");
        assert!(k.open(&sealed, b"row-2").is_err());
        let other = Keyring::from_master(&[8u8; 32]);
        assert!(other.open(&sealed, b"row-1").is_err());
    }

    #[test]
    fn token_hashes_are_keyed_and_domain_separated() {
        let k = Keyring::from_master(&[1u8; 32]);
        let h = k.token_hash("refresh", "abc");
        assert!(k.verify_token_hash("refresh", "abc", &h));
        assert!(!k.verify_token_hash("reset", "abc", &h));
        assert_ne!(Keyring::from_master(&[2u8; 32]).token_hash("refresh", "abc"), h);
    }

    #[test]
    fn random_values_have_the_right_shape() {
        let id = random_id(20).unwrap();
        assert_eq!(id.len(), 20);
        assert!(id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()));
        let d = random_digits(6).unwrap();
        assert_eq!(d.len(), 6);
        assert!(d.bytes().all(|b| b.is_ascii_digit()));
        assert_ne!(random_token(32).unwrap(), random_token(32).unwrap());
    }
}
