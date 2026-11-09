//! Signed URLs: time-limited, unforgeable links to one object operation.
//!
//! A URL carries `expires` (epoch seconds), optional `version`, `download`
//! (file name), `ct` (required content type of an upload), `sub` (the
//! billing subject of an upload) and `sig`, the hex HMAC-SHA256, keyed with
//! the instance's signing key (generated on first start and kept in the
//! database), over method, bucket, key and every one of those values. Change
//! any of them and the signature no longer matches (`403 signature_invalid`);
//! past `expires` the URL answers `403 url_expired`. The signer needs read
//! (download) or write (upload) access to the key when the URL is made; the
//! holder needs nothing.

use std::collections::BTreeMap;

use axum::body::Bytes;
use forgebase_data::AccessContext;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::driver::ByteStream;
use crate::error::{Result, StorageError};
use crate::model::{ObjectInfo, SignRequest, SignedMethod, SignedUrl};
use crate::objects::{ObjectRead, PutRequest, RangeSpec};
use crate::service::{Need, StorageService};
use crate::sigv4::encode;
use crate::util::{hex, rfc3339, unhex, validate_key};

type HmacSha256 = Hmac<Sha256>;

/// The values a signed URL carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedParams {
    /// Method the URL allows.
    pub method: SignedMethod,
    /// Bucket.
    pub bucket: String,
    /// Key.
    pub key: String,
    /// Expiry, epoch seconds.
    pub expires: i64,
    /// Version to read.
    pub version: Option<String>,
    /// Download file name.
    pub download: Option<String>,
    /// Content type an upload must declare.
    pub content_type: Option<String>,
    /// Billing subject of an upload.
    pub subject: Option<String>,
    /// Hex signature.
    pub sig: String,
}

impl SignedParams {
    /// Reads the values from a path (bucket, key) and the query pairs.
    pub fn from_request(
        method: SignedMethod,
        bucket: &str,
        key: &str,
        query: &BTreeMap<String, String>,
    ) -> Result<Self> {
        let expires = query
            .get("expires")
            .and_then(|e| e.parse::<i64>().ok())
            .ok_or(StorageError::SignatureInvalid)?;
        Ok(Self {
            method,
            bucket: bucket.to_owned(),
            key: key.to_owned(),
            expires,
            version: query.get("version").cloned(),
            download: query.get("download").cloned(),
            content_type: query.get("ct").cloned(),
            subject: query.get("sub").cloned(),
            sig: query.get("sig").cloned().ok_or(StorageError::SignatureInvalid)?,
        })
    }

    fn message(&self) -> String {
        format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.method.as_str(),
            self.bucket,
            self.key,
            self.expires,
            self.version.as_deref().unwrap_or(""),
            self.download.as_deref().unwrap_or(""),
            self.content_type.as_deref().unwrap_or(""),
            self.subject.as_deref().unwrap_or("")
        )
    }
}

impl StorageService {
    fn mac(&self, message: &str) -> HmacSha256 {
        #[allow(clippy::expect_used)]
        let mut m =
            <HmacSha256 as KeyInit>::new_from_slice(&self.inner.signing_key).expect("HMAC accepts any key length");
        m.update(message.as_bytes());
        m
    }

    /// Makes a signed URL (path and query) for one operation on one key.
    pub async fn sign_url(&self, ctx: &AccessContext, bucket: &str, req: SignRequest) -> Result<SignedUrl> {
        validate_key(&req.key)?;
        let b = self.bucket_row(bucket).await?;
        let method = req.method.unwrap_or(SignedMethod::Get);
        let need = if method == SignedMethod::Get {
            Need::Read
        } else {
            Need::Write
        };
        self.authorize(ctx, &b, Some(&req.key), need).await?;
        if method == SignedMethod::Get {
            // Fails early (404) for a key or version that is not there.
            self.head_object(ctx, bucket, &req.key, req.version_id.as_deref())
                .await?;
        }
        let s = &self.inner.settings;
        let ttl = req.expires_in.unwrap_or(s.default_signed_ttl_secs);
        if ttl == 0 || ttl > s.max_signed_ttl_secs {
            return Err(StorageError::invalid(
                "invalid_expiry",
                format!("expires_in is 1 to {} seconds", s.max_signed_ttl_secs),
            ));
        }
        let expires = self.now() / 1000 + i64::try_from(ttl).unwrap_or(i64::MAX / 2);
        let mut p = SignedParams {
            method,
            bucket: bucket.to_owned(),
            key: req.key.clone(),
            expires,
            version: req.version_id.filter(|v| method == SignedMethod::Get && !v.is_empty()),
            download: req.download_as.filter(|d| method == SignedMethod::Get && !d.is_empty()),
            content_type: req
                .content_type
                .filter(|c| method == SignedMethod::Put && !c.is_empty()),
            subject: (method == SignedMethod::Put).then(|| Self::subject_of(ctx)),
            sig: String::new(),
        };
        p.sig = hex(&self.mac(&p.message()).finalize().into_bytes());
        let mut url = format!(
            "/api/v1/storage/signed/{}/{}?expires={}",
            encode(bucket, false),
            encode(&p.key, true),
            p.expires
        );
        for (name, value) in [
            ("version", &p.version),
            ("download", &p.download),
            ("ct", &p.content_type),
            ("sub", &p.subject),
        ] {
            if let Some(v) = value {
                url.push_str(&format!("&{name}={}", encode(v, false)));
            }
        }
        url.push_str(&format!("&sig={}", p.sig));
        Ok(SignedUrl {
            url,
            method,
            expires_at: rfc3339(expires * 1000),
        })
    }

    /// Checks a signed URL: signature first (constant time), then expiry.
    pub fn verify_signed(&self, p: &SignedParams) -> Result<()> {
        let sig = unhex(&p.sig).ok_or(StorageError::SignatureInvalid)?;
        self.mac(&p.message())
            .verify_slice(&sig)
            .map_err(|_| StorageError::SignatureInvalid)?;
        if p.expires <= self.now() / 1000 {
            return Err(StorageError::SignatureExpired);
        }
        Ok(())
    }

    /// Serves a signed download.
    pub async fn get_signed(&self, p: &SignedParams, range: Option<RangeSpec>) -> Result<ObjectRead> {
        if p.method != SignedMethod::Get {
            return Err(StorageError::SignatureInvalid);
        }
        self.verify_signed(p)?;
        self.read_row(&p.bucket, &p.key, p.version.as_deref(), range).await
    }

    /// Accepts a signed upload.
    pub async fn put_signed(
        &self,
        p: &SignedParams,
        content_type: Option<String>,
        metadata: BTreeMap<String, String>,
        expected_sha256: Option<String>,
        content_length: Option<u64>,
        body: ByteStream,
    ) -> Result<ObjectInfo> {
        if p.method != SignedMethod::Put {
            return Err(StorageError::SignatureInvalid);
        }
        self.verify_signed(p)?;
        if let Some(required) = &p.content_type {
            let declared = content_type
                .as_deref()
                .map(|c| c.split(';').next().unwrap_or("").trim());
            if declared != Some(required.split(';').next().unwrap_or("").trim()) {
                return Err(StorageError::invalid(
                    "content_type_mismatch",
                    format!("this URL accepts only {required}"),
                ));
            }
        }
        let bucket = self.bucket_row(&p.bucket).await?;
        let subject = p
            .subject
            .clone()
            .ok_or_else(|| StorageError::Internal("signed upload without a subject".into()))?;
        self.put_inner(
            &subject,
            "signed-url",
            &bucket,
            PutRequest {
                bucket: p.bucket.clone(),
                key: p.key.clone(),
                content_type,
                metadata,
                expected_sha256,
                content_length,
                body,
            },
        )
        .await
    }
}

/// A body of one in-memory chunk (tests and small writes).
pub fn body_of(bytes: impl Into<Bytes>) -> ByteStream {
    crate::driver::once(bytes)
}
