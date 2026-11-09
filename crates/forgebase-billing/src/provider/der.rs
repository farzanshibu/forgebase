//! Just enough DER, PEM and X.509 to check store certificates (FB-72): a TLV
//! reader, certificate fields (serial, validity, public key, extension OIDs),
//! RSA keys out of a SubjectPublicKeyInfo, and the ECDSA signature
//! conversions JWS needs. Chain building and signature checks are
//! `rustls-webpki` and `ring`; this module only reads structure.

use base64::Engine as _;
use forgebase_entitlements::period::days_from_civil;

use crate::error::{BillingError, Result};

fn bad(what: &str) -> BillingError {
    BillingError::invalid("invalid_certificate", format!("malformed DER: {what}"))
}

/// A DER reader over a byte slice.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Der<'a> {
    buf: &'a [u8],
}

/// One element: tag, content and the whole encoding.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Tlv<'a> {
    pub tag: u8,
    pub content: &'a [u8],
    pub whole: &'a [u8],
}

impl<'a> Der<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf }
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Reads the next element.
    pub fn read(&mut self) -> Result<Tlv<'a>> {
        let b = self.buf;
        if b.len() < 2 {
            return Err(bad("truncated"));
        }
        let tag = b[0];
        if tag & 0x1f == 0x1f {
            return Err(bad("multi-byte tags are not used here"));
        }
        let (len, hdr) = match b[1] {
            l if l < 0x80 => (usize::from(l), 2),
            0x80 => return Err(bad("indefinite length")),
            l => {
                let n = usize::from(l & 0x7f);
                if n > 4 || b.len() < 2 + n {
                    return Err(bad("length"));
                }
                let mut len = 0usize;
                for x in &b[2..2 + n] {
                    len = (len << 8) | usize::from(*x);
                }
                (len, 2 + n)
            }
        };
        let end = hdr.checked_add(len).filter(|e| *e <= b.len()).ok_or_else(|| bad("length"))?;
        self.buf = &b[end..];
        Ok(Tlv {
            tag,
            content: &b[hdr..end],
            whole: &b[..end],
        })
    }

    /// Reads an element that must carry `tag`.
    pub fn expect(&mut self, tag: u8) -> Result<Tlv<'a>> {
        let t = self.read()?;
        if t.tag != tag {
            return Err(bad("unexpected tag"));
        }
        Ok(t)
    }
}

const SEQ: u8 = 0x30;
const INT: u8 = 0x02;
const BIT_STRING: u8 = 0x03;
const OID: u8 = 0x06;

/// The parts of a certificate the store drivers look at.
#[derive(Debug, Clone)]
pub(crate) struct CertInfo<'a> {
    /// Serial number, upper-case hex without leading zero bytes.
    pub serial_hex: String,
    /// Validity, epoch milliseconds.
    pub not_before: i64,
    pub not_after: i64,
    /// The whole SubjectPublicKeyInfo.
    pub spki: &'a [u8],
    /// Extension OIDs (DER content bytes).
    pub extensions: Vec<&'a [u8]>,
}

/// Reads a certificate.
pub(crate) fn cert_info(der: &[u8]) -> Result<CertInfo<'_>> {
    let cert = Der::new(der).expect(SEQ)?;
    let tbs = Der::new(cert.content).expect(SEQ)?;
    let mut r = Der::new(tbs.content);
    let mut t = r.read()?;
    if t.tag == 0xa0 {
        t = r.read()?;
    }
    if t.tag != INT {
        return Err(bad("serial"));
    }
    let serial = t.content.iter().skip_while(|b| **b == 0).copied().collect::<Vec<u8>>();
    let serial_hex = crate::money::hex(&serial).to_ascii_uppercase();
    r.expect(SEQ)?; // signature algorithm
    r.expect(SEQ)?; // issuer
    let validity = r.expect(SEQ)?;
    let mut v = Der::new(validity.content);
    let not_before = asn1_time(v.read()?)?;
    let not_after = asn1_time(v.read()?)?;
    r.expect(SEQ)?; // subject
    let spki = r.expect(SEQ)?.whole;
    let mut extensions = Vec::new();
    while !r.is_empty() {
        let t = r.read()?;
        if t.tag == 0xa3 {
            let list = Der::new(t.content).expect(SEQ)?;
            let mut l = Der::new(list.content);
            while !l.is_empty() {
                let ext = l.expect(SEQ)?;
                extensions.push(Der::new(ext.content).expect(OID)?.content);
            }
        }
    }
    Ok(CertInfo {
        serial_hex,
        not_before,
        not_after,
        spki,
        extensions,
    })
}

/// UTCTime (`YYMMDDHHMMSSZ`) or GeneralizedTime (`YYYYMMDDHHMMSSZ`) to epoch ms.
fn asn1_time(t: Tlv<'_>) -> Result<i64> {
    let s = std::str::from_utf8(t.content).map_err(|_| bad("time"))?;
    let s = s.strip_suffix('Z').ok_or_else(|| bad("time zone"))?;
    let digits = |r: std::ops::Range<usize>| -> Result<i64> {
        s.get(r)
            .filter(|d| d.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|d| d.parse().ok())
            .ok_or_else(|| bad("time"))
    };
    let (year, rest) = match (t.tag, s.len()) {
        (0x17, 12) => {
            let y = digits(0..2)?;
            (if y >= 50 { 1900 + y } else { 2000 + y }, 2)
        }
        (0x18, 14) => (digits(0..4)?, 4),
        _ => return Err(bad("time")),
    };
    let month = u32::try_from(digits(rest..rest + 2)?).map_err(|_| bad("time"))?;
    let day = u32::try_from(digits(rest + 2..rest + 4)?).map_err(|_| bad("time"))?;
    let (h, m, sec) = (
        digits(rest + 4..rest + 6)?,
        digits(rest + 6..rest + 8)?,
        digits(rest + 8..rest + 10)?,
    );
    Ok((days_from_civil(year, month, day) * 86_400 + h * 3600 + m * 60 + sec) * 1000)
}

/// The key bits of a SubjectPublicKeyInfo (for RSA: the PKCS#1 RSAPublicKey).
pub(crate) fn spki_key(spki: &[u8]) -> Result<&[u8]> {
    let seq = Der::new(spki).expect(SEQ)?;
    let mut r = Der::new(seq.content);
    r.expect(SEQ)?;
    let bits = r.expect(BIT_STRING)?;
    match bits.content.split_first() {
        Some((0, key)) => Ok(key),
        _ => Err(bad("public key bits")),
    }
}

/// `(n, e)` of a PKCS#1 RSAPublicKey, without leading zero bytes.
pub(crate) fn rsa_components(rsa_public_key: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let seq = Der::new(rsa_public_key).expect(SEQ)?;
    let mut r = Der::new(seq.content);
    let strip = |b: &[u8]| b.iter().skip_while(|x| **x == 0).copied().collect::<Vec<u8>>();
    let n = strip(r.expect(INT)?.content);
    let e = strip(r.expect(INT)?.content);
    Ok((n, e))
}

/// DER content bytes of a dotted OID such as `1.2.840.113635.100.6.11.1`.
pub(crate) fn oid_bytes(dotted: &str) -> Vec<u8> {
    let parts: Vec<u64> = dotted.split('.').filter_map(|p| p.parse().ok()).collect();
    let mut out = Vec::new();
    if parts.len() < 2 {
        return out;
    }
    let mut push = |mut v: u64| {
        let mut tmp = vec![u8::try_from(v & 0x7f).unwrap_or_default()];
        v >>= 7;
        while v > 0 {
            tmp.push(u8::try_from(v & 0x7f).unwrap_or_default() | 0x80);
            v >>= 7;
        }
        out.extend(tmp.iter().rev());
    };
    push(parts[0] * 40 + parts[1]);
    for p in &parts[2..] {
        push(*p);
    }
    out
}

/// Decodes every PEM block of a text: `(label, der)`.
pub(crate) fn pem_blocks(text: &str) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut label: Option<String> = None;
    let mut body = String::new();
    for line in text.lines().map(str::trim) {
        if let Some(l) = line.strip_prefix("-----BEGIN ").and_then(|l| l.strip_suffix("-----")) {
            label = Some(l.to_owned());
            body.clear();
        } else if line.starts_with("-----END ") {
            if let Some(l) = label.take()
                && let Ok(der) = base64::engine::general_purpose::STANDARD.decode(body.as_bytes())
            {
                out.push((l, der));
            }
            body.clear();
        } else if label.is_some() {
            body.push_str(line);
        }
    }
    out
}

/// The DER of the first PEM block labelled `label` (or the input itself
/// decoded as base64 when it has no PEM armour: App Store `.p8` keys are
/// sometimes pasted bare).
pub(crate) fn pem_der(text: &str, label: &str) -> Result<Vec<u8>> {
    if let Some((_, der)) = pem_blocks(text).into_iter().find(|(l, _)| l == label) {
        return Ok(der);
    }
    let bare: String = text.split_whitespace().collect();
    base64::engine::general_purpose::STANDARD
        .decode(bare.as_bytes())
        .map_err(|_| BillingError::invalid("invalid_key", format!("expected a PEM {label}")))
}

/// JWS ECDSA signature (`r || s`, 32 bytes each) to the DER form X.509 uses.
pub(crate) fn ecdsa_raw_to_der(raw: &[u8]) -> Option<Vec<u8>> {
    if raw.len() != 64 {
        return None;
    }
    let int = |x: &[u8]| {
        let x: Vec<u8> = x.iter().skip_while(|b| **b == 0).copied().collect();
        let mut v = Vec::with_capacity(x.len() + 3);
        v.push(INT);
        if x.first().is_none_or(|b| b & 0x80 != 0) {
            v.push(u8::try_from(x.len() + 1).unwrap_or_default());
            v.push(0);
        } else {
            v.push(u8::try_from(x.len()).unwrap_or_default());
        }
        v.extend_from_slice(&x);
        v
    };
    let (r, s) = (int(&raw[..32]), int(&raw[32..]));
    let mut out = vec![SEQ, u8::try_from(r.len() + s.len()).ok()?];
    out.extend(r);
    out.extend(s);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oids_encode_like_der() {
        assert_eq!(
            oid_bytes("1.2.840.113635.100.6.11.1"),
            vec![0x2a, 0x86, 0x48, 0x86, 0xf7, 0x63, 0x64, 0x06, 0x0b, 0x01]
        );
    }

    #[test]
    fn times_parse() {
        let t = |tag, s: &str| {
            asn1_time(Tlv {
                tag,
                content: s.as_bytes(),
                whole: &[],
            })
            .unwrap()
        };
        assert_eq!(t(0x17, "700101000000Z"), 0);
        assert_eq!(t(0x18, "20261101000000Z"), 1_793_491_200_000);
    }

    #[test]
    fn signatures_convert() {
        let mut raw = [0u8; 64];
        raw[0] = 0x80;
        raw[63] = 1;
        let der = ecdsa_raw_to_der(&raw).unwrap();
        assert_eq!(der[0], 0x30);
        assert_eq!(&der[2..5], &[0x02, 33, 0x00]);
        assert_eq!(&der[der.len() - 3..], &[0x02, 1, 1]);
        assert!(ecdsa_raw_to_der(&raw[..10]).is_none());
    }
}
