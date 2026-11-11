//! Signed URLs: time-limited, unforgeable links to one object operation.
//!
//! A URL carries `expires` (epoch seconds), optional `version`, `download`
//! (file name), `ct` (required content type of an upload), `sub` (the
//! billing subject of an upload) and `sig`, the hex HMAC-SHA256, keyed with
//! the instance's signing key, over method, bucket, key and every one of
//! those values. URLs made since FB-140 add `v=2`, `t` (the bucket's tenant)
//! and `by` (the signer of an upload, recorded as the object's creator), and
//! sign every value length-prefixed; URLs without `v` are the earlier format
//! and still verify. Change any value and the signature no longer matches
//! (`403 signature_invalid`); past `expires` the URL answers
//! `403 url_expired`.
//!
//! **Checked when issued, not when used** (rls-coverage G15, as in S3 and
//! Supabase). The signer needs read (download) or write (upload) access to
//! the key when the URL is made, bucket rules included: an upload URL is
//! refused when the rule could fail for an upload it accepts (a rule on
//! `size`, or on `content_type` without `content_type` pinned in the URL).
//! The holder needs nothing, and a later change of permissions, ACL, rules
//! or the object itself does not reach a URL already issued until it
//! expires. Hence the short default lifetime (15 minutes, at most
//! `max_signed_ttl_secs`), and the revocation of every URL at once by
//! rotating the signing key ([`StorageService::rotate_signing_key`],
//! `POST /api/v1/storage/maintenance/rotate-signing-key`).
//!
//! The key is 32 random bytes, sealed in `_forgebase_storage_settings` with
//! the auth keyring (G11); a database copy alone cannot forge URLs.

use std::collections::BTreeMap;

use axum::body::Bytes;
use forgebase_data::{AccessContext, Decision};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

use crate::driver::ByteStream;
use crate::error::{Result, StorageError};
use crate::model::{ObjectInfo, SignRequest, SignedMethod, SignedUrl};
use crate::objects::{ObjectRead, PutRequest, RangeSpec};
use crate::rules::{NewRow, WriteCheck, new_row_check};
use crate::service::StorageService;
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
    /// URL format: 1 (before FB-140) or 2.
    pub format: u8,
    /// Tenant of the bucket (format 2).
    pub tenant: Option<String>,
    /// Principal key of an upload's signer (format 2).
    pub signer: Option<String>,
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
            format: match query.get("v").map(String::as_str) {
                None => 1,
                Some("2") => 2,
                Some(_) => return Err(StorageError::SignatureInvalid),
            },
            tenant: query.get("t").cloned(),
            signer: query.get("by").cloned(),
        })
    }

    fn message(&self) -> String {
        if self.format == 2 {
            let mut m = String::from("forgebase-signed-url/v2\n");
            for v in [
                self.method.as_str(),
                self.tenant.as_deref().unwrap_or(""),
                &self.bucket,
                &self.key,
                &self.expires.to_string(),
                self.version.as_deref().unwrap_or(""),
                self.download.as_deref().unwrap_or(""),
                self.content_type.as_deref().unwrap_or(""),
                self.subject.as_deref().unwrap_or(""),
                self.signer.as_deref().unwrap_or(""),
            ] {
                m.push_str(&format!("{}:{v}\n", v.len()));
            }
            return m;
        }
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
        let key = self.signing_key();
        #[allow(clippy::expect_used)]
        let mut m = <HmacSha256 as KeyInit>::new_from_slice(&key).expect("HMAC accepts any key length");
        m.update(message.as_bytes());
        m
    }

    /// Makes a signed URL (path and query) for one operation on one key.
    pub async fn sign_url(&self, ctx: &AccessContext, bucket: &str, req: SignRequest) -> Result<SignedUrl> {
        validate_key(&req.key)?;
        let b = self.bucket_row(ctx, bucket).await?;
        let method = req.method.unwrap_or(SignedMethod::Get);
        if method == SignedMethod::Get {
            // Authorises (bucket rules included) and fails early (404) for a
            // key or version that is not there or not visible to the signer.
            self.head_object(ctx, bucket, &req.key, req.version_id.as_deref())
                .await?;
        } else {
            self.authorize_sign_put(ctx, &b, &req.key, req.content_type.as_deref())
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
            format: 2,
            tenant: b.tenant.clone(),
            signer: (method == SignedMethod::Put).then(|| ctx.principal_key()),
        };
        p.sig = hex(&self.mac(&p.message()).finalize().into_bytes());
        let mut url = format!(
            "/api/v1/storage/signed/{}/{}?v=2&expires={}",
            encode(bucket, false),
            encode(&p.key, true),
            p.expires
        );
        for (name, value) in [
            ("t", &p.tenant),
            ("version", &p.version),
            ("download", &p.download),
            ("ct", &p.content_type),
            ("sub", &p.subject),
            ("by", &p.signer),
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
        let b = self.bucket_in(p.tenant.as_deref(), &p.bucket).await?;
        self.read_row(&b.id, &p.key, p.version.as_deref(), range, None).await
    }

    /// Whether `ctx` may sign an upload URL for `key`: write access, and
    /// a bucket rule that holds for every upload the URL accepts (the size
    /// is unknown, the content type only known when `content_type` pins
    /// it). The rule is not applied again when the URL is used (G15).
    async fn authorize_sign_put(
        &self,
        ctx: &AccessContext,
        b: &crate::service::BucketRow,
        key: &str,
        content_type: Option<&str>,
    ) -> Result<()> {
        let check = self.authorize_write(ctx, b, key).await?;
        self.precheck_write(b, key, &check).await?;
        let exists = match self.find_row(&b.id, key, None, None).await {
            Ok(r) => !r.delete_marker,
            Err(StorageError::NotFound(_)) => false,
            Err(e) => return Err(e),
        };
        match check.decision(exists) {
            Some(Decision::Filter(pred)) => {
                let actor = ctx.principal_key();
                let row = NewRow {
                    key,
                    created_by: &actor,
                    size: None,
                    content_type: content_type.filter(|c| !c.is_empty()),
                };
                let stmt = new_row_check(self.inner.db.dialect(), &row, pred)?;
                if self.inner.db.fetch_all(&stmt).await?.is_empty() {
                    return Err(StorageError::Forbidden(
                        "the bucket's rules cannot be checked for every upload this URL would accept".into(),
                    ));
                }
                Ok(())
            }
            Some(Decision::Allow) | None => Ok(()),
            Some(_) => Err(crate::objects::rule_refusal()),
        }
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
        let bucket = self.bucket_in(p.tenant.as_deref(), &p.bucket).await?;
        let subject = p
            .subject
            .clone()
            .ok_or_else(|| StorageError::Internal("signed upload without a subject".into()))?;
        // Format 2 records the signer as the creator; earlier URLs did not
        // carry one.
        let actor = p.signer.clone().unwrap_or_else(|| "signed-url".to_owned());
        self.put_inner(
            &subject,
            &actor,
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
            WriteCheck::open(),
        )
        .await
    }
}

/// A body of one in-memory chunk (tests and small writes).
pub fn body_of(bytes: impl Into<Bytes>) -> ByteStream {
    crate::driver::once(bytes)
}
