//! Signature verification for provider ID tokens and WebAuthn assertions:
//! RS256 (PKCS#1 v1.5 with SHA-256) and ES256 (ECDSA P-256 with SHA-256).
//!
//! Pure Rust, verification only. RSA verification uses public data, so the
//! plain big-integer `modpow` of `num-bigint` is fine (no secret-dependent
//! timing). The encoded message is rebuilt and compared byte for byte, so
//! there is no padding parser to get wrong.

use num_bigint::BigUint;
use p256::ecdsa::signature::Verifier;
use p256::ecdsa::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

/// DigestInfo prefix of SHA-256 (RFC 8017 §9.2).
const SHA256_PREFIX: [u8; 19] = [
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05, 0x00, 0x04, 0x20,
];

/// Smallest accepted RSA modulus.
const MIN_RSA_BITS: u64 = 2048;
/// Largest accepted modulus (bounds the work of one verification).
const MAX_RSA_BITS: u64 = 8192;

/// A public key that can verify signatures.
#[derive(Debug, Clone)]
pub enum PublicKey {
    /// RSA (`RS256`).
    Rsa {
        /// Modulus.
        n: BigUint,
        /// Public exponent.
        e: BigUint,
    },
    /// ECDSA over P-256 (`ES256`).
    Es256(Box<VerifyingKey>),
}

impl PublicKey {
    /// RSA key from big-endian `n` and `e`.
    pub fn rsa(n: &[u8], e: &[u8]) -> Option<Self> {
        let n = BigUint::from_bytes_be(n);
        let e = BigUint::from_bytes_be(e);
        let bits = n.bits();
        let odd_n = n.bit(0);
        let e_ok = e.bit(0) && e >= BigUint::from(3u8) && e.bits() <= 32;
        ((MIN_RSA_BITS..=MAX_RSA_BITS).contains(&bits) && odd_n && e_ok).then_some(PublicKey::Rsa { n, e })
    }

    /// P-256 key from affine coordinates (32 bytes each, big-endian).
    pub fn p256(x: &[u8], y: &[u8]) -> Option<Self> {
        if x.len() != 32 || y.len() != 32 {
            return None;
        }
        let mut sec1 = Vec::with_capacity(65);
        sec1.push(0x04);
        sec1.extend_from_slice(x);
        sec1.extend_from_slice(y);
        VerifyingKey::from_sec1_bytes(&sec1)
            .ok()
            .map(|k| PublicKey::Es256(Box::new(k)))
    }

    /// Verifies a JWS-style signature: RS256 raw signature, or ES256 as
    /// 64-byte `r || s`.
    pub fn verify_jws(&self, message: &[u8], sig: &[u8]) -> bool {
        match self {
            PublicKey::Rsa { n, e } => rs256_verify(n, e, message, sig),
            PublicKey::Es256(k) => Signature::from_slice(sig).is_ok_and(|s| k.verify(message, &s).is_ok()),
        }
    }

    /// Verifies a WebAuthn signature: RS256 raw, or ES256 as ASN.1 DER.
    pub fn verify_webauthn(&self, message: &[u8], sig: &[u8]) -> bool {
        match self {
            PublicKey::Rsa { .. } => self.verify_jws(message, sig),
            PublicKey::Es256(k) => Signature::from_der(sig).is_ok_and(|s| k.verify(message, &s).is_ok()),
        }
    }
}

fn rs256_verify(n: &BigUint, e: &BigUint, message: &[u8], sig: &[u8]) -> bool {
    let k = n.bits().div_ceil(8) as usize;
    if sig.len() != k {
        return false;
    }
    let s = BigUint::from_bytes_be(sig);
    if &s >= n {
        return false;
    }
    let m = s.modpow(e, n).to_bytes_be();
    if m.len() > k {
        return false;
    }
    let mut em = vec![0u8; k - m.len()];
    em.extend_from_slice(&m);

    let hash = Sha256::digest(message);
    let t_len = SHA256_PREFIX.len() + hash.len();
    if k < t_len + 11 {
        return false;
    }
    let mut expected = Vec::with_capacity(k);
    expected.extend_from_slice(&[0x00, 0x01]);
    expected.extend(std::iter::repeat_n(0xff, k - t_len - 3));
    expected.push(0x00);
    expected.extend_from_slice(&SHA256_PREFIX);
    expected.extend_from_slice(&hash);
    crate::crypto::ct_eq(&em, &expected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::SigningKey;
    use p256::ecdsa::signature::Signer;

    #[test]
    fn es256_roundtrip_and_tamper() {
        let sk = SigningKey::from_slice(&[7u8; 32]).unwrap();
        let point = sk.verifying_key().to_sec1_point(false);
        let pk = PublicKey::p256(point.x().unwrap(), point.y().unwrap()).unwrap();
        let sig: Signature = sk.sign(b"hello");
        assert!(pk.verify_jws(b"hello", &sig.to_bytes()));
        assert!(!pk.verify_jws(b"hellp", &sig.to_bytes()));
        assert!(pk.verify_webauthn(b"hello", sig.to_der().as_bytes()));
        assert!(!pk.verify_jws(b"hello", &[0u8; 63]));
    }

    #[test]
    fn rsa_rejects_small_and_malformed_keys() {
        assert!(PublicKey::rsa(&[0xff; 128], &[1, 0, 1]).is_none(), "1024-bit modulus");
        let mut n = vec![0xff; 256];
        n[255] = 0xfe;
        assert!(PublicKey::rsa(&n, &[1, 0, 1]).is_none(), "even modulus");
        n[255] = 0xff;
        assert!(PublicKey::rsa(&n, &[2]).is_none(), "even exponent");
        assert!(PublicKey::rsa(&n, &[1, 0, 1]).is_some());
    }
}
