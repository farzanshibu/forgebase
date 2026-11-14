//! AWS Signature Version 4 request signing (header-based) for S3.
//!
//! The signer is `forgebase_auth::sigv4`, shared with the SES and SNS
//! adapters (ADR-0039); it is re-exported here, so the S3, GCS and Azure
//! drivers and their tests keep one import path. For the `s3` service it
//! signs and sends `x-amz-content-sha256`, as the S3 driver needs.

pub use forgebase_auth::sigv4::{Credentials, SignInput, encode, sha256_hex, sign, stamps};

#[cfg(test)]
mod tests {
    use super::*;

    fn aws_example() -> Credentials {
        Credentials {
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
            region: "us-east-1".into(),
            service: "s3".into(),
        }
    }

    #[test]
    fn stamps_match_the_calendar() {
        assert_eq!(stamps(1_369_353_600), ("20130524".into(), "20130524T000000Z".into()));
        assert_eq!(stamps(0).0, "19700101");
        assert_eq!(stamps(1_794_225_600).1, "20261109T120000Z");
        // Leap day.
        assert_eq!(stamps(1_709_164_800).0, "20240229");
    }

    #[test]
    fn encoding_follows_sigv4() {
        assert_eq!(encode("a b/c~d", true), "a%20b/c~d");
        assert_eq!(encode("a/b", false), "a%2Fb");
        assert_eq!(encode("é", false), "%C3%A9");
    }

    /// "GET Object" with a Range header, from the AWS S3 documentation
    /// (Signature Calculations for the Authorization Header).
    #[test]
    fn aws_documented_get_object_signature() {
        let headers = vec![("range".to_owned(), "bytes=0-9".to_owned())];
        let out = sign(
            &aws_example(),
            &SignInput {
                method: "GET",
                host: "examplebucket.s3.amazonaws.com",
                path: "/test.txt",
                query: &[],
                headers: &headers,
                payload_sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                now: 1_369_353_600,
            },
        );
        let auth = &out.iter().find(|(k, _)| k == "authorization").unwrap().1;
        assert!(
            auth.ends_with("Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"),
            "{auth}"
        );
        assert!(auth.contains("SignedHeaders=host;range;x-amz-content-sha256;x-amz-date"));
    }
}
