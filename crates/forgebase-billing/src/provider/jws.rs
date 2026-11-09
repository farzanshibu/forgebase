//! Compact JWS for the store drivers (FB-72): parsing, ES256 verification
//! against an `x5c` chain pinned to a root (App Store), RS256 verification
//! with JWKS components (Google push tokens), and ES256 / RS256 signing of
//! the JWTs the drivers send (App Store Server API, Google service account).

use base64::Engine as _;
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, KeyPair as _, RsaKeyPair};
use rustls_pki_types::{CertificateDer, UnixTime};
use serde_json::Value as Json;
use webpki::{EndEntityCert, ExtendedKeyUsageValidator, KeyPurposeIdIter};

use super::der;
use crate::error::{BillingError, Result};

/// Base64url without padding.
pub fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes base64url (padding tolerated).
pub fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('=').as_bytes())
        .ok()
}

fn forged(msg: &str) -> BillingError {
    BillingError::webhook("invalid_signature", msg.to_owned())
}

/// A parsed, not yet verified, compact JWS.
#[derive(Debug, Clone)]
pub struct Jws {
    /// Protected header.
    pub header: Json,
    /// Payload (JSON).
    pub payload: Json,
    signing_input: String,
    signature: Vec<u8>,
}

impl Jws {
    /// Splits and decodes `header.payload.signature`.
    pub fn parse(compact: &str) -> Result<Self> {
        let compact = compact.trim();
        let mut parts = compact.split('.');
        let (Some(h), Some(p), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            return Err(forged("not a compact JWS"));
        };
        let json = |seg: &str| -> Result<Json> {
            b64url_decode(seg)
                .and_then(|b| serde_json::from_slice(&b).ok())
                .ok_or_else(|| forged("the JWS is not base64url JSON"))
        };
        Ok(Self {
            header: json(h)?,
            payload: json(p)?,
            signing_input: format!("{h}.{p}"),
            signature: b64url_decode(s).ok_or_else(|| forged("the JWS signature is not base64url"))?,
        })
    }

    fn alg(&self) -> &str {
        self.header.get("alg").and_then(Json::as_str).unwrap_or_default()
    }

    /// Verifies an ES256 JWS whose `x5c` chain (leaf, intermediate, root)
    /// leads to one of `roots` (DER) at `now_ms`. `leaf_oid` and
    /// `intermediate_oid` (dotted) must be extensions of the leaf and the
    /// intermediate: Apple marks its receipt-signing certificates so that a
    /// developer certificate from the same root cannot sign a transaction.
    pub fn verify_x5c(
        &self,
        roots: &[Vec<u8>],
        now_ms: i64,
        leaf_oid: Option<&str>,
        intermediate_oid: Option<&str>,
    ) -> Result<()> {
        if self.alg() != "ES256" {
            return Err(forged("the JWS must be ES256"));
        }
        let chain: Vec<Vec<u8>> = self
            .header
            .get("x5c")
            .and_then(Json::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Json::as_str)
                    .filter_map(|c| base64::engine::general_purpose::STANDARD.decode(c).ok())
                    .collect()
            })
            .unwrap_or_default();
        if chain.len() < 2 {
            return Err(forged("the JWS carries no certificate chain"));
        }
        let anchors: Vec<_> = roots
            .iter()
            .map(|r| CertificateDer::from(r.as_slice()))
            .collect::<Vec<_>>();
        let anchors: Vec<_> = anchors
            .iter()
            .filter_map(|c| webpki::anchor_from_trusted_cert(c).ok())
            .collect();
        if anchors.is_empty() {
            return Err(BillingError::NotConfigured("no store root certificate".into()));
        }
        let leaf_der = CertificateDer::from(chain[0].as_slice());
        let leaf = EndEntityCert::try_from(&leaf_der).map_err(|_| forged("the signing certificate is malformed"))?;
        let intermediates: Vec<CertificateDer<'_>> =
            chain[1..].iter().map(|c| CertificateDer::from(c.as_slice())).collect();
        let secs = u64::try_from(now_ms.max(0) / 1000).unwrap_or_default();
        leaf.verify_for_usage(
            &[
                webpki::ring::ECDSA_P256_SHA256,
                webpki::ring::ECDSA_P256_SHA384,
                webpki::ring::ECDSA_P384_SHA256,
                webpki::ring::ECDSA_P384_SHA384,
            ],
            &anchors,
            &intermediates,
            UnixTime::since_unix_epoch(std::time::Duration::from_secs(secs)),
            AnyEku,
            None,
            None,
        )
        .map_err(|_| forged("the certificate chain does not lead to the store root"))?;
        let has = |cert: &[u8], oid: &str| -> Result<bool> {
            let want = der::oid_bytes(oid);
            Ok(der::cert_info(cert)?.extensions.contains(&want.as_slice()))
        };
        if let Some(oid) = leaf_oid
            && !has(&chain[0], oid)?
        {
            return Err(forged(
                "the signing certificate is not a store receipt-signing certificate",
            ));
        }
        if let Some(oid) = intermediate_oid
            && !has(&chain[1], oid)?
        {
            return Err(forged("the intermediate certificate is not the store's"));
        }
        let sig = der::ecdsa_raw_to_der(&self.signature).ok_or_else(|| forged("bad ES256 signature length"))?;
        leaf.verify_signature(webpki::ring::ECDSA_P256_SHA256, self.signing_input.as_bytes(), &sig)
            .map_err(|_| forged("the JWS signature does not verify"))
    }

    /// Verifies an RS256 JWS with an RSA public key (`n`, `e` big-endian).
    pub fn verify_rs256(&self, n: &[u8], e: &[u8]) -> Result<()> {
        if self.alg() != "RS256" {
            return Err(forged("the token must be RS256"));
        }
        signature::RsaPublicKeyComponents { n, e }
            .verify(
                &signature::RSA_PKCS1_2048_8192_SHA256,
                self.signing_input.as_bytes(),
                &self.signature,
            )
            .map_err(|_| forged("the token signature does not verify"))
    }

    /// `kid` of the header.
    pub fn kid(&self) -> Option<&str> {
        self.header.get("kid").and_then(Json::as_str)
    }
}

/// Store certificates carry their own extended key usages; the chain and
/// the marker extensions are what is checked.
struct AnyEku;

impl ExtendedKeyUsageValidator for AnyEku {
    fn validate(&self, _iter: KeyPurposeIdIter<'_, '_>) -> std::result::Result<(), webpki::Error> {
        Ok(())
    }
}

fn compact(header: &Json, payload: &Json) -> String {
    format!(
        "{}.{}",
        b64url(header.to_string().as_bytes()),
        b64url(payload.to_string().as_bytes())
    )
}

/// Signs an ES256 JWS with a PKCS#8 P-256 key (App Store `.p8`).
pub fn sign_es256(pkcs8: &[u8], header: &Json, payload: &Json) -> Result<String> {
    let rng = SystemRandom::new();
    let key = EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8, &rng)
        .map_err(|_| BillingError::invalid("invalid_key", "expected a PKCS#8 P-256 private key"))?;
    let input = compact(header, payload);
    let sig = key
        .sign(&rng, input.as_bytes())
        .map_err(|_| BillingError::Internal("ES256 signing failed".into()))?;
    Ok(format!("{input}.{}", b64url(sig.as_ref())))
}

/// The uncompressed public point of a PKCS#8 P-256 key.
pub fn es256_public(pkcs8: &[u8]) -> Result<Vec<u8>> {
    let rng = SystemRandom::new();
    let key = EcdsaKeyPair::from_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8, &rng)
        .map_err(|_| BillingError::invalid("invalid_key", "expected a PKCS#8 P-256 private key"))?;
    Ok(key.public_key().as_ref().to_vec())
}

/// RSASSA-PKCS1-v1_5 SHA-256 of `msg` with a PKCS#8 RSA key.
pub fn rsa_sign(pkcs8: &[u8], msg: &[u8]) -> Result<Vec<u8>> {
    let key = RsaKeyPair::from_pkcs8(pkcs8)
        .map_err(|_| BillingError::invalid("invalid_key", "expected a PKCS#8 RSA private key"))?;
    let mut sig = vec![0u8; key.public().modulus_len()];
    key.sign(&signature::RSA_PKCS1_SHA256, &SystemRandom::new(), msg, &mut sig)
        .map_err(|_| BillingError::Internal("RSA signing failed".into()))?;
    Ok(sig)
}

/// The PKCS#1 RSAPublicKey of a PKCS#8 RSA key.
pub fn rsa_public(pkcs8: &[u8]) -> Result<Vec<u8>> {
    let key = RsaKeyPair::from_pkcs8(pkcs8)
        .map_err(|_| BillingError::invalid("invalid_key", "expected a PKCS#8 RSA private key"))?;
    Ok(key.public().as_ref().to_vec())
}

/// Signs an RS256 JWS with a PKCS#8 RSA key (Google service account).
pub fn sign_rs256(pkcs8: &[u8], header: &Json, payload: &Json) -> Result<String> {
    let input = compact(header, payload);
    let sig = rsa_sign(pkcs8, input.as_bytes())?;
    Ok(format!("{input}.{}", b64url(&sig)))
}

/// Verifies RSASSA-PKCS1-v1_5 SHA-256 with a PKCS#1 RSAPublicKey.
pub fn rsa_verify(rsa_public_key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    signature::UnparsedPublicKey::new(&signature::RSA_PKCS1_2048_8192_SHA256, rsa_public_key)
        .verify(msg, sig)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rejects_garbage() {
        assert!(Jws::parse("a.b").is_err());
        assert!(Jws::parse("a.b.c.d").is_err());
        assert!(Jws::parse("!!.e30.AA").is_err());
        let j = Jws::parse(&format!("{}.{}.AA", b64url(br#"{"alg":"ES256"}"#), b64url(b"{}"))).unwrap();
        assert_eq!(j.alg(), "ES256");
        assert!(j.verify_x5c(&[], 0, None, None).is_err());
    }
}
