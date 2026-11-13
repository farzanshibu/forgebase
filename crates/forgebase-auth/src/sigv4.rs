//! AWS Signature Version 4 request signing (header-based), shared by the
//! Amazon SES v2 mail transport (`forgebase-customize`) and the Amazon SNS
//! SMS provider ([`crate::sms`]).
//!
//! The signer is the same algorithm as the S3 driver's in `forgebase-storage`
//! (which cannot be depended on from here: it depends on this crate), with
//! one difference: `x-amz-content-sha256` is only sent and signed when
//! [`Credentials::service`] is `s3` (other services ignore it, and the
//! public AWS SigV4 test-suite vectors do not include it).

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

use crate::crypto::hex;

type HmacSha256 = Hmac<Sha256>;

/// Credentials and scope of a signer.
#[derive(Clone)]
pub struct Credentials {
    /// Access key id.
    pub access_key: String,
    /// Secret access key.
    pub secret_key: String,
    /// Temporary-credential session token.
    pub session_token: Option<String>,
    /// Region (`us-east-1`).
    pub region: String,
    /// Service (`ses`, `sns`, `s3`).
    pub service: String,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("access_key", &self.access_key)
            .field("region", &self.region)
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    #[allow(clippy::expect_used)]
    let mut m = <HmacSha256 as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

/// Percent-encodes per SigV4: everything but unreserved characters, and `/`
/// when `keep_slash` is set.
pub fn encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') || (keep_slash && b == b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `YYYYMMDD` and `YYYYMMDDTHHMMSSZ` for epoch seconds.
pub fn stamps(epoch_secs: i64) -> (String, String) {
    let days = epoch_secs.div_euclid(86_400);
    let secs = epoch_secs.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let date = format!("{y:04}{m:02}{d:02}");
    let stamp = format!("{date}T{:02}{:02}{:02}Z", secs / 3600, secs % 3600 / 60, secs % 60);
    (date, stamp)
}

/// Lower-case hex SHA-256 of `data` (the payload hash).
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// What [`sign`] needs to know about a request.
pub struct SignInput<'a> {
    /// HTTP method.
    pub method: &'a str,
    /// Host header value (with `:port` when not the default).
    pub host: &'a str,
    /// Absolute path, not yet encoded (`/v2/email/outbound-emails`).
    pub path: &'a str,
    /// Query parameters, not yet encoded.
    pub query: &'a [(String, String)],
    /// Extra headers to sign (names are lower-cased), besides `host`,
    /// `x-amz-date` and, for S3, `x-amz-content-sha256`.
    pub headers: &'a [(String, String)],
    /// Hex SHA-256 of the body ([`sha256_hex`]).
    pub payload_sha256: &'a str,
    /// Epoch seconds.
    pub now: i64,
}

/// The headers to add to the request: `x-amz-date`, an optional session
/// token (and `x-amz-content-sha256` for S3) and `authorization`. Headers
/// passed in [`SignInput::headers`] are signed but not repeated here.
pub fn sign(c: &Credentials, i: &SignInput<'_>) -> Vec<(String, String)> {
    let (date, stamp) = stamps(i.now);
    let s3 = c.service == "s3";
    let mut headers: Vec<(String, String)> = i
        .headers
        .iter()
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    headers.push(("host".into(), i.host.to_owned()));
    if s3 {
        headers.push(("x-amz-content-sha256".into(), i.payload_sha256.to_owned()));
    }
    headers.push(("x-amz-date".into(), stamp.clone()));
    if let Some(t) = &c.session_token {
        headers.push(("x-amz-security-token".into(), t.clone()));
    }
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let mut query: Vec<(String, String)> = i
        .query
        .iter()
        .map(|(k, v)| (encode(k, false), encode(v, false)))
        .collect();
    query.sort();
    let canonical_query = query
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    // Services other than S3 encode the path twice; every path this crate
    // signs is made of unreserved characters and slashes, where the two
    // forms are identical.
    let canonical = format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        i.method,
        encode(i.path, true),
        canonical_query,
        canonical_headers,
        signed_headers,
        i.payload_sha256
    );
    let scope = format!("{date}/{}/{}/aws4_request", c.region, c.service);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}",
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let k_date = hmac(format!("AWS4{}", c.secret_key).as_bytes(), date.as_bytes());
    let k_region = hmac(&k_date, c.region.as_bytes());
    let k_service = hmac(&k_region, c.service.as_bytes());
    let k_signing = hmac(&k_service, b"aws4_request");
    let signature = hex(&hmac(&k_signing, to_sign.as_bytes()));
    let mut out = vec![("x-amz-date".to_owned(), stamp)];
    if s3 {
        out.push(("x-amz-content-sha256".to_owned(), i.payload_sha256.to_owned()));
    }
    if let Some(t) = &c.session_token {
        out.push(("x-amz-security-token".into(), t.clone()));
    }
    out.push((
        "authorization".into(),
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            c.access_key
        ),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn creds(service: &str) -> Credentials {
        Credentials {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
            region: "us-east-1".into(),
            service: service.into(),
        }
    }

    fn auth(out: &[(String, String)]) -> &str {
        &out.iter().find(|(k, _)| k == "authorization").unwrap().1
    }

    #[test]
    fn stamps_match_the_calendar() {
        assert_eq!(stamps(1_369_353_600), ("20130524".into(), "20130524T000000Z".into()));
        assert_eq!(stamps(1_440_938_160), ("20150830".into(), "20150830T123600Z".into()));
        assert_eq!(stamps(0).0, "19700101");
        assert_eq!(stamps(1_709_164_800).0, "20240229");
    }

    #[test]
    fn encoding_follows_sigv4() {
        assert_eq!(encode("a b/c~d", true), "a%20b/c~d");
        assert_eq!(encode("a/b", false), "a%2Fb");
        assert_eq!(encode("é", false), "%C3%A9");
    }

    /// `get-vanilla` of the public AWS Signature Version 4 test suite.
    #[test]
    fn aws_test_suite_get_vanilla() {
        let out = sign(
            &creds("service"),
            &SignInput {
                method: "GET",
                host: "example.amazonaws.com",
                path: "/",
                query: &[],
                headers: &[],
                payload_sha256: EMPTY,
                now: 1_440_938_160,
            },
        );
        assert_eq!(
            auth(&out),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
        assert!(!out.iter().any(|(k, _)| k == "x-amz-content-sha256"));
    }

    /// `post-x-www-form-urlencoded` of the same suite: the form-POST shape
    /// the SNS Query API uses.
    #[test]
    fn aws_test_suite_post_form() {
        let body = b"Param1=value1";
        let out = sign(
            &creds("service"),
            &SignInput {
                method: "POST",
                host: "example.amazonaws.com",
                path: "/",
                query: &[],
                headers: &[("content-type".into(), "application/x-www-form-urlencoded".into())],
                payload_sha256: &sha256_hex(body),
                now: 1_440_938_160,
            },
        );
        assert_eq!(
            auth(&out),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=content-type;host;x-amz-date, \
             Signature=ff11897932ad3f4e8b18135d722051e5ac45fc38421b1da7b9d196a0fe09473a"
        );
    }

    /// "GET Object" with a Range header, from the AWS S3 documentation.
    #[test]
    fn aws_documented_s3_get_object_signature() {
        let c = Credentials {
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
            region: "us-east-1".into(),
            service: "s3".into(),
        };
        let out = sign(
            &c,
            &SignInput {
                method: "GET",
                host: "examplebucket.s3.amazonaws.com",
                path: "/test.txt",
                query: &[],
                headers: &[("range".to_owned(), "bytes=0-9".to_owned())],
                payload_sha256: EMPTY,
                now: 1_369_353_600,
            },
        );
        assert!(
            auth(&out).ends_with("Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"),
            "{}",
            auth(&out)
        );
        assert!(auth(&out).contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"));
    }
}
