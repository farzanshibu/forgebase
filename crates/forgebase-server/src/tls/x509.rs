//! The little X.509 ForgeBase needs for TLS (ADR-0033 §4), on `ring`:
//!
//! * writing: an ECDSA P-256 key, a self-signed certificate (tests, and the
//!   RFC 8737 `tls-alpn-01` challenge certificate with its critical
//!   `acmeIdentifier` extension), and a PKCS#10 certificate signing request
//!   with a subject-alternative-name extension request (ACME finalize);
//! * reading: a certificate's serial, issuer, subject, validity and DNS
//!   names, for renewal scheduling, the status endpoint and `doctor`.
//!
//! Signatures and chain validation are rustls' and webpki's job; this module
//! never decides whether a certificate is trusted. Like the PostgreSQL
//! engine's certificate generator (FB-153) it is a small DER writer instead
//! of a dependency.

use std::fmt::Write as _;
use std::net::IpAddr;

use base64::Engine as _;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};

/// Malformed DER, or a key or signature operation that failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct X509Error(pub String);

fn err(m: impl Into<String>) -> X509Error {
    X509Error(m.into())
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    let n = content.len();
    if n < 0x80 {
        v.push(u8::try_from(n).unwrap_or(0));
    } else {
        let bytes = n.to_be_bytes();
        let skip = bytes.iter().take_while(|b| **b == 0).count();
        let len = &bytes[skip..];
        v.push(0x80 | u8::try_from(len.len()).unwrap_or(0));
        v.extend_from_slice(len);
    }
    v.extend_from_slice(content);
    v
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

fn set(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x31, &parts.concat())
}

fn uint(bytes: &[u8]) -> Vec<u8> {
    let mut b: Vec<u8> = bytes.iter().copied().skip_while(|x| *x == 0).collect();
    if b.is_empty() || b[0] & 0x80 != 0 {
        b.insert(0, 0);
    }
    tlv(0x02, &b)
}

fn bits(bytes: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8];
    b.extend_from_slice(bytes);
    tlv(0x03, &b)
}

fn oid(der: &[u8]) -> Vec<u8> {
    tlv(0x06, der)
}

const OID_ECDSA_SHA256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02];
const OID_EC_PUBLIC_KEY: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
const OID_CN: &[u8] = &[0x55, 0x04, 0x03];
const OID_O: &[u8] = &[0x55, 0x04, 0x0A];
const OID_SAN: &[u8] = &[0x55, 0x1D, 0x11];
/// `id-pe-acmeIdentifier` (RFC 8737 §6.1): 1.3.6.1.5.5.7.1.31.
const OID_ACME_IDENTIFIER: &[u8] = &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x01, 0x1F];
/// PKCS#9 `extensionRequest`: 1.2.840.113549.1.9.14.
const OID_EXTENSION_REQUEST: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x09, 0x0E];

/// Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Days since 1970-01-01 of a civil date (inverse of [`civil`]).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `UTCTime` before 2050, `GeneralizedTime` after (RFC 5280 §4.1.2.5).
fn der_time(unix: i64) -> Vec<u8> {
    let (y, m, d) = civil(unix.div_euclid(86_400));
    let secs = unix.rem_euclid(86_400);
    let (hh, mm, ss) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if (1950..2050).contains(&y) {
        tlv(
            0x17,
            format!("{:02}{m:02}{d:02}{hh:02}{mm:02}{ss:02}Z", y % 100).as_bytes(),
        )
    } else {
        tlv(0x18, format!("{y:04}{m:02}{d:02}{hh:02}{mm:02}{ss:02}Z").as_bytes())
    }
}

fn name(cn: &str) -> Vec<u8> {
    if cn.is_empty() || cn.len() > 64 {
        return seq(&[]);
    }
    seq(&[set(&[seq(&[oid(OID_CN), tlv(0x0C, cn.as_bytes())])])])
}

fn extension(id: &[u8], critical: bool, value: &[u8]) -> Vec<u8> {
    let mut parts = vec![oid(id)];
    if critical {
        parts.push(tlv(0x01, &[0xFF]));
    }
    parts.push(tlv(0x04, value));
    seq(&parts)
}

fn san(names: &[&str]) -> Vec<u8> {
    let items: Vec<Vec<u8>> = names
        .iter()
        .map(|n| match n.parse::<IpAddr>() {
            Ok(IpAddr::V4(a)) => tlv(0x87, &a.octets()),
            Ok(IpAddr::V6(a)) => tlv(0x87, &a.octets()),
            Err(_) => tlv(0x82, n.as_bytes()),
        })
        .collect();
    extension(OID_SAN, false, &seq(&items))
}

fn spki(pair: &EcdsaKeyPair) -> Vec<u8> {
    seq(&[
        seq(&[oid(OID_EC_PUBLIC_KEY), oid(OID_P256)]),
        bits(pair.public_key().as_ref()),
    ])
}

/// A fresh ECDSA P-256 key: `(PKCS#8 DER, key pair)`.
///
/// # Errors
/// The system random source failed.
pub fn generate_key() -> Result<(Vec<u8>, EcdsaKeyPair), X509Error> {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
        .map_err(|_| err("cannot generate an ECDSA P-256 key"))?;
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
        .map_err(|_| err("cannot load the generated key"))?;
    Ok((pkcs8.as_ref().to_vec(), pair))
}

fn sign(pair: &EcdsaKeyPair, tbs: &[u8]) -> Result<Vec<u8>, X509Error> {
    let rng = SystemRandom::new();
    let sig = pair.sign(&rng, tbs).map_err(|_| err("ECDSA signing failed"))?;
    Ok(sig.as_ref().to_vec())
}

/// A self-signed certificate for `names` (DNS names or IP addresses; the
/// first is also the subject CN) valid from `not_before` to `not_after`
/// (Unix seconds), with `extra` extensions (DER `Extension`s) after the SAN.
/// Returns `(certificate DER, PKCS#8 key DER)`.
///
/// # Errors
/// Key generation or signing failed.
pub fn self_signed(
    names: &[&str],
    not_before: i64,
    not_after: i64,
    extra: &[Vec<u8>],
) -> Result<(Vec<u8>, Vec<u8>), X509Error> {
    let (pkcs8, pair) = generate_key()?;
    let cert = self_signed_with(&pair, names, not_before, not_after, extra)?;
    Ok((cert, pkcs8))
}

/// [`self_signed`] with a given key.
///
/// # Errors
/// Signing failed.
pub fn self_signed_with(
    pair: &EcdsaKeyPair,
    names: &[&str],
    not_before: i64,
    not_after: i64,
    extra: &[Vec<u8>],
) -> Result<Vec<u8>, X509Error> {
    let rng = SystemRandom::new();
    let mut serial = [0u8; 16];
    rng.fill(&mut serial).map_err(|_| err("random serial"))?;
    serial[0] &= 0x7F;
    let alg = seq(&[oid(OID_ECDSA_SHA256)]);
    let subject = name(names.first().copied().unwrap_or_default());
    let mut exts = vec![san(names)];
    exts.extend(extra.iter().cloned());
    let tbs = seq(&[
        tlv(0xA0, &uint(&[2])),
        uint(&serial),
        alg.clone(),
        subject.clone(),
        seq(&[der_time(not_before), der_time(not_after)]),
        subject,
        spki(pair),
        tlv(0xA3, &seq(&exts)),
    ]);
    let sig = sign(pair, &tbs)?;
    Ok(seq(&[tbs, alg, bits(&sig)]))
}

/// The RFC 8737 `tls-alpn-01` challenge certificate for `domain`: self-signed,
/// the domain as its only SAN, and the critical `acmeIdentifier` extension
/// holding `digest` (SHA-256 of the key authorization). Valid for a week
/// around `now`. Returns `(certificate DER, PKCS#8 key DER)`.
///
/// # Errors
/// `digest` is not 32 bytes; key generation or signing failed.
pub fn alpn_challenge_certificate(domain: &str, digest: &[u8], now: i64) -> Result<(Vec<u8>, Vec<u8>), X509Error> {
    if digest.len() != 32 {
        return Err(err("the tls-alpn-01 digest must be 32 bytes"));
    }
    let ext = extension(OID_ACME_IDENTIFIER, true, &tlv(0x04, digest));
    self_signed(&[domain], now - 86_400, now + 7 * 86_400, &[ext])
}

/// A PKCS#10 certificate signing request for `names` signed by `pair`: the
/// first name is the subject CN (when it fits in 64 characters), all of
/// them go in a requested SAN extension.
///
/// # Errors
/// Signing failed; no names.
pub fn csr(names: &[String], pair: &EcdsaKeyPair) -> Result<Vec<u8>, X509Error> {
    if names.is_empty() {
        return Err(err("a certificate request needs at least one name"));
    }
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let attrs = tlv(0xA0, &seq(&[oid(OID_EXTENSION_REQUEST), set(&[seq(&[san(&refs)])])]));
    let info = seq(&[uint(&[0]), name(&names[0]), spki(pair), attrs]);
    let sig = sign(pair, &info)?;
    Ok(seq(&[info, seq(&[oid(OID_ECDSA_SHA256)]), bits(&sig)]))
}

/// PEM text of one DER object with `label` (`CERTIFICATE`, `PRIVATE KEY`).
pub fn pem(label: &str, der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut s = format!("-----BEGIN {label}-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        s.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        s.push('\n');
    }
    let _ = writeln!(s, "-----END {label}-----");
    s
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// What ForgeBase reads from a certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertInfo {
    /// Serial number, lowercase hex.
    pub serial: String,
    /// Issuer for display: its CN, with the organisation in parentheses.
    pub issuer: String,
    /// Subject CN (may be empty).
    pub subject: String,
    /// Start of validity, Unix seconds.
    pub not_before: i64,
    /// End of validity, Unix seconds.
    pub not_after: i64,
    /// DNS names and IP addresses of the SAN extension.
    pub names: Vec<String>,
}

impl CertInfo {
    /// Lifetime in seconds (at least 1).
    pub fn lifetime(&self) -> i64 {
        (self.not_after - self.not_before).max(1)
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn next(&mut self) -> Result<(u8, &'a [u8]), X509Error> {
        let b = self.0;
        if b.len() < 2 {
            return Err(err("truncated DER"));
        }
        let tag = b[0];
        let (len, head) = if b[1] < 0x80 {
            (usize::from(b[1]), 2)
        } else {
            let n = usize::from(b[1] & 0x7F);
            if n == 0 || n > 4 || b.len() < 2 + n {
                return Err(err("bad DER length"));
            }
            let mut len = 0usize;
            for x in &b[2..2 + n] {
                len = (len << 8) | usize::from(*x);
            }
            (len, 2 + n)
        };
        let end = head.checked_add(len).ok_or_else(|| err("bad DER length"))?;
        if b.len() < end {
            return Err(err("truncated DER"));
        }
        self.0 = &b[end..];
        Ok((tag, &b[head..end]))
    }

    fn expect(&mut self, want: u8, what: &str) -> Result<&'a [u8], X509Error> {
        match self.next()? {
            (t, v) if t == want => Ok(v),
            (t, _) => Err(err(format!("expected {what} (tag {want:#04x}), found tag {t:#04x}"))),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.0.first().copied()
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

fn digits(s: &[u8]) -> Result<i64, X509Error> {
    if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
        return Err(err("bad time"));
    }
    Ok(s.iter().fold(0i64, |a, d| a * 10 + i64::from(d - b'0')))
}

fn parse_time(tag: u8, v: &[u8]) -> Result<i64, X509Error> {
    let (year, rest) = match (tag, v.len()) {
        (0x17, 13) => {
            let yy = digits(&v[..2])?;
            (if yy < 50 { 2000 + yy } else { 1900 + yy }, &v[2..])
        }
        (0x18, 15) => (digits(&v[..4])?, &v[4..]),
        _ => return Err(err("unsupported certificate time format")),
    };
    if rest.last() != Some(&b'Z') {
        return Err(err("certificate time is not UTC"));
    }
    let f = |i: usize| digits(&rest[i..i + 2]);
    let (m, d, hh, mm, ss) = (f(0)?, f(2)?, f(4)?, f(6)?, f(8)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || hh > 23 || mm > 59 || ss > 60 {
        return Err(err("certificate time out of range"));
    }
    Ok(days_from_civil(year, m, d) * 86_400 + hh * 3600 + mm * 60 + ss)
}

fn string_value(tag: u8, v: &[u8]) -> String {
    match tag {
        // BMPString: UTF-16BE.
        0x1E => {
            let units: Vec<u16> = v
                .chunks(2)
                .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => String::from_utf8_lossy(v).into_owned(),
    }
}

/// `(CN, O)` of a `Name`.
fn name_parts(der: &[u8]) -> Result<(String, String), X509Error> {
    let (mut cn, mut o) = (String::new(), String::new());
    let mut rdns = Reader(der);
    while !rdns.is_empty() {
        let mut atvs = Reader(rdns.expect(0x31, "RDN set")?);
        while !atvs.is_empty() {
            let mut atv = Reader(atvs.expect(0x30, "attribute")?);
            let id = atv.expect(0x06, "attribute type")?;
            let (tag, value) = atv.next()?;
            if id == OID_CN && cn.is_empty() {
                cn = string_value(tag, value);
            } else if id == OID_O && o.is_empty() {
                o = string_value(tag, value);
            }
        }
    }
    Ok((cn, o))
}

fn san_names(value: &[u8]) -> Result<Vec<String>, X509Error> {
    let mut out = Vec::new();
    let mut outer = Reader(value);
    let mut names = Reader(outer.expect(0x30, "GeneralNames")?);
    while !names.is_empty() {
        let (tag, v) = names.next()?;
        match tag {
            0x82 => out.push(String::from_utf8_lossy(v).to_ascii_lowercase()),
            0x87 if v.len() == 4 => out.push(IpAddr::from(<[u8; 4]>::try_from(v).unwrap_or_default()).to_string()),
            0x87 if v.len() == 16 => out.push(IpAddr::from(<[u8; 16]>::try_from(v).unwrap_or_default()).to_string()),
            _ => {}
        }
    }
    Ok(out)
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Reads a DER certificate.
///
/// # Errors
/// The DER is not an X.509 certificate this reader understands.
pub fn parse_certificate(der: &[u8]) -> Result<CertInfo, X509Error> {
    let mut top = Reader(der);
    let mut cert = Reader(top.expect(0x30, "Certificate")?);
    let mut tbs = Reader(cert.expect(0x30, "TBSCertificate")?);
    if tbs.peek() == Some(0xA0) {
        tbs.next()?;
    }
    let serial = tbs.expect(0x02, "serial number")?;
    tbs.expect(0x30, "signature algorithm")?;
    let (issuer_cn, issuer_o) = name_parts(tbs.expect(0x30, "issuer")?)?;
    let mut validity = Reader(tbs.expect(0x30, "validity")?);
    let (t1, v1) = validity.next()?;
    let (t2, v2) = validity.next()?;
    let (not_before, not_after) = (parse_time(t1, v1)?, parse_time(t2, v2)?);
    let (subject, _) = name_parts(tbs.expect(0x30, "subject")?)?;
    tbs.expect(0x30, "subject public key")?;
    let mut names = Vec::new();
    while !tbs.is_empty() {
        let (tag, v) = tbs.next()?;
        if tag != 0xA3 {
            continue;
        }
        let mut wrap = Reader(v);
        let mut exts = Reader(wrap.expect(0x30, "extensions")?);
        while !exts.is_empty() {
            let mut ext = Reader(exts.expect(0x30, "extension")?);
            let id = ext.expect(0x06, "extension id")?;
            if ext.peek() == Some(0x01) {
                ext.next()?;
            }
            let value = ext.expect(0x04, "extension value")?;
            if id == OID_SAN {
                names = san_names(value)?;
            }
        }
    }
    let serial = hex(serial.strip_prefix(&[0]).unwrap_or(serial));
    let issuer = match (issuer_cn.is_empty(), issuer_o.is_empty()) {
        (false, false) if issuer_cn != issuer_o => format!("{issuer_cn} ({issuer_o})"),
        (false, _) => issuer_cn,
        (true, _) => issuer_o,
    };
    Ok(CertInfo {
        serial,
        issuer,
        subject,
        not_before,
        not_after,
        names,
    })
}

/// Unix seconds as RFC 3339 (`2026-11-14T10:00:00Z`).
pub fn rfc3339(unix: i64) -> String {
    let (y, m, d) = civil(unix.div_euclid(86_400));
    let secs = unix.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

    const NOW: i64 = 1_794_614_400; // 2026-11-14T00:00:00Z

    #[test]
    fn dates_round_trip_through_der_time() {
        for t in [0, 951_782_400, NOW, 2_524_608_000 /* 2050-01-01 */, 4_102_444_799] {
            let der = der_time(t);
            let mut r = Reader(&der);
            let (tag, v) = r.next().unwrap();
            assert_eq!(parse_time(tag, v).unwrap(), t, "{t}");
        }
        assert_eq!(der_time(2_524_608_000)[0], 0x18, "2050 switches to GeneralizedTime");
        assert_eq!(rfc3339(NOW), "2026-11-14T00:00:00Z");
    }

    #[test]
    fn a_self_signed_certificate_reads_back_and_rustls_accepts_it() {
        let (cert, key) = self_signed(&["app.example.com", "127.0.0.1"], NOW - 60, NOW + 90 * 86_400, &[]).unwrap();
        let info = parse_certificate(&cert).unwrap();
        assert_eq!(info.subject, "app.example.com");
        assert_eq!(info.issuer, "app.example.com");
        assert_eq!(info.not_before, NOW - 60);
        assert_eq!(info.not_after, NOW + 90 * 86_400);
        assert_eq!(info.names, ["app.example.com", "127.0.0.1"]);
        assert!(info.serial.len() <= 32);
        // rustls' own parser (webpki) accepts it, and the key matches.
        let provider = rustls::crypto::ring::default_provider();
        let ck = rustls::sign::CertifiedKey::from_der(
            vec![CertificateDer::from(cert)],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            &provider,
        )
        .unwrap();
        ck.keys_match().unwrap();
    }

    #[test]
    fn the_alpn_challenge_certificate_carries_a_critical_acme_identifier() {
        let digest = [7u8; 32];
        let (cert, _) = alpn_challenge_certificate("app.example.com", &digest, NOW).unwrap();
        let mut needle = vec![0x06, 0x08];
        needle.extend_from_slice(OID_ACME_IDENTIFIER);
        needle.extend_from_slice(&[0x01, 0x01, 0xFF, 0x04, 0x22, 0x04, 0x20]);
        needle.extend_from_slice(&digest);
        assert!(
            cert.windows(needle.len()).any(|w| w == needle),
            "critical acmeIdentifier with the digest"
        );
        assert_eq!(parse_certificate(&cert).unwrap().names, ["app.example.com"]);
        assert!(alpn_challenge_certificate("a.example.com", &[0; 31], NOW).is_err());
    }

    #[test]
    fn a_csr_holds_the_names_and_a_valid_signature() {
        let (_, pair) = generate_key().unwrap();
        let names = vec!["a.example.com".to_owned(), "b.example.com".to_owned()];
        let der = csr(&names, &pair).unwrap();
        let mut top = Reader(&der);
        let mut req = Reader(top.expect(0x30, "csr").unwrap());
        let info_start = req.0;
        let info = req.expect(0x30, "info").unwrap();
        let info_der = &info_start[..info_start.len() - req.0.len()];
        req.expect(0x30, "alg").unwrap();
        let sig = req.expect(0x03, "sig").unwrap();
        let pk = ring::signature::UnparsedPublicKey::new(
            &ring::signature::ECDSA_P256_SHA256_ASN1,
            pair.public_key().as_ref(),
        );
        pk.verify(info_der, &sig[1..]).unwrap();
        for n in &names {
            assert!(info.windows(n.len()).any(|w| w == n.as_bytes()), "{n}");
        }
        assert!(csr(&[], &pair).is_err());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        for bad in [
            &b""[..],
            &[0x30],
            &[0x30, 0x84, 0xFF, 0xFF, 0xFF, 0xFF],
            &[0x30, 0x03, 0x02, 0x01],
        ] {
            assert!(parse_certificate(bad).is_err());
        }
    }
}
