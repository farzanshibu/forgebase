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
//!
//! **Native downloads** (ADR-0040 addendum). With `[storage] signed_urls =
//! "native"` (or `"auto"` and a driver that can sign), a download URL is the
//! storage provider's own signed URL (S3 SigV4, GCS V4, Azure service SAS)
//! for the one content-addressed blob of the version being read, issued
//! after exactly the checks of a proxy URL (read access, bucket rules,
//! visibility, version, delete markers) with the object row read from the
//! primary. Its lifetime is the requested one capped at the provider's
//! maximum; it carries the object's `Content-Type` and the proxy's
//! `Content-Disposition` as signed `response-*` / `rsct` / `rscd`
//! overrides. ForgeBase does not see, meter or count those downloads, and
//! rotating the signing key does not revoke them (rotate the provider
//! credential). Each one is recorded in the audit log
//! (`storage.signed_url.native`). Uploads always stay on the proxy: the
//! content-addressed key is only known once the bytes are hashed, and the
//! size, content type, checksum, rules and quota are checked while they
//! stream through ForgeBase.

use std::collections::BTreeMap;

use axum::body::Bytes;
use forgebase_core::SignedUrlsMode;
use forgebase_data::{AccessContext, Decision, ResourceOp};
use forgebase_db::{ColumnType, Statement};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::json;
use sha2::Sha256;

use crate::driver::{ByteStream, PresignGet};
use crate::error::{Result, StorageError};
use crate::model::{ObjectInfo, SignRequest, SignedMethod, SignedMode, SignedUrl};
use crate::objects::{ObjectRead, PutRequest, RangeSpec};
use crate::rules::{NewRow, WriteCheck, new_row_check};
use crate::service::{BucketRow, Need, StorageService, blob_key};
use crate::sigv4::encode;
use crate::util::{hex, rfc3339, unhex, validate_key};

/// Audit event of a native signed URL (ADR-0040 addendum).
pub const NATIVE_URL_EVENT: &str = "storage.signed_url.native";

/// Who will serve one signed URL.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Delivery {
    Proxy,
    /// The provider; its longest validity in seconds.
    Native(u64),
    /// `native` was asked for, but the driver cannot sign: the reason.
    Unavailable(String),
}

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

    /// The `signed_urls` mode of a bucket: its `<tenant>/<name>` entry, else
    /// its name's entry, else the instance setting.
    pub fn signed_urls_mode(&self, tenant: Option<&str>, bucket: &str) -> SignedUrlsMode {
        let s = &self.inner.settings;
        let scoped = tenant
            .filter(|t| !t.is_empty())
            .and_then(|t| s.signed_urls_buckets.get(&format!("{t}/{bucket}")));
        scoped
            .or_else(|| s.signed_urls_buckets.get(bucket))
            .copied()
            .unwrap_or(s.signed_urls)
    }

    fn delivery(&self, b: &BucketRow, method: SignedMethod) -> Delivery {
        if method != SignedMethod::Get {
            return Delivery::Proxy;
        }
        let mode = self.signed_urls_mode(b.tenant.as_deref(), &b.name);
        if mode == SignedUrlsMode::Proxy {
            return Delivery::Proxy;
        }
        match (self.inner.driver.presign_support(), mode) {
            (Ok(max), _) => Delivery::Native(max),
            (Err(why), SignedUrlsMode::Native) => Delivery::Unavailable(why),
            (Err(_), _) => Delivery::Proxy,
        }
    }

    /// Makes a signed URL for one operation on one key: ForgeBase's HMAC URL
    /// (path and query), or for a download in native mode the provider's.
    pub async fn sign_url(&self, ctx: &AccessContext, bucket: &str, req: SignRequest) -> Result<SignedUrl> {
        validate_key(&req.key)?;
        let b = self.bucket_row(ctx, bucket).await?;
        let method = req.method.unwrap_or(SignedMethod::Get);
        let delivery = self.delivery(&b, method);
        let mut native_row = None;
        if method == SignedMethod::Get {
            if delivery == Delivery::Proxy {
                // Authorises (bucket rules included) and fails early (404) for a
                // key or version that is not there or not visible to the signer.
                self.head_object(ctx, bucket, &req.key, req.version_id.as_deref())
                    .await?;
            } else {
                // The same checks as `head_object`, with the row read from
                // the primary: the URL is bound to this version's blob.
                let access = self
                    .authorize(ctx, &b, Some(&req.key), Need::Read, ResourceOp::View)
                    .await?;
                let row = self
                    .find_row(&b, &req.key, req.version_id.as_deref(), access.filter())
                    .await?;
                if row.delete_marker || row.blob_sha.is_none() {
                    return Err(StorageError::NotFound(format!("object {}", req.key)));
                }
                native_row = Some(row);
            }
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
        match (delivery, native_row) {
            (Delivery::Unavailable(why), _) => {
                return Err(StorageError::conflict(
                    "native_signing_unavailable",
                    format!(
                        "signed_urls is native for bucket {bucket}, but the {} driver cannot sign: {why}; set signed_urls to auto or proxy, or give the driver credentials that can sign",
                        self.inner.driver.name()
                    ),
                ));
            }
            (Delivery::Native(max), Some(row)) => {
                return self
                    .native_url(ctx, &b, &row, ttl.min(max), req.download_as.as_deref())
                    .await;
            }
            _ => {}
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
            mode: SignedMode::Proxy,
        })
    }

    /// The provider's signed download URL for `row`'s blob, valid `ttl`
    /// seconds, with the headers the proxy path would answer with.
    async fn native_url(
        &self,
        ctx: &AccessContext,
        b: &BucketRow,
        row: &crate::objects::ObjectRow,
        ttl: u64,
        download_as: Option<&str>,
    ) -> Result<SignedUrl> {
        let Some(sha) = row.blob_sha.as_deref() else {
            return Err(StorageError::NotFound(format!("object {}", row.key)));
        };
        let disposition = match download_as.filter(|d| !d.is_empty()) {
            Some(name) => Some(crate::http::disposition(name)),
            None if crate::http::is_active(&row.content_type) => Some(crate::http::disposition(
                row.key.rsplit('/').next().unwrap_or("download"),
            )),
            None => None,
        };
        let now = self.now() / 1000;
        let key = blob_key(sha);
        let url = self.inner.driver.presign_get(&PresignGet {
            key: &key,
            now,
            expires_secs: ttl,
            content_type: Some(row.content_type.as_str()).filter(|c| !c.is_empty()),
            content_disposition: disposition.as_deref(),
        })?;
        let expires = now + i64::try_from(ttl).unwrap_or(i64::MAX / 2);
        let expires_at = rfc3339(expires * 1000);
        self.audit_native(
            ctx,
            json!({
                "bucket": b.name,
                "tenant": b.tenant,
                "key": row.key,
                "version_id": row.id,
                "driver": self.inner.driver.name(),
                "expires_at": expires_at,
            }),
        )
        .await;
        Ok(SignedUrl {
            url,
            method: SignedMethod::Get,
            expires_at,
            mode: SignedMode::Native,
        })
    }

    /// Records a native URL in the audit log of the control database
    /// (`_forgebase_auth_audit`): ForgeBase sees nothing of its downloads.
    /// The URL itself, a bearer credential, is not recorded. A failure is
    /// logged, not returned.
    async fn audit_native(&self, ctx: &AccessContext, detail: serde_json::Value) {
        let db = self.control();
        let now = self.now();
        let r: Result<()> = async {
            let d = db.dialect();
            let mut tx = db.begin().await?;
            let stmt = Statement::new(
                format!(
                    "INSERT INTO _forgebase_auth_audit (at, event, outcome, actor, request_id, detail, \
                     impersonator, realm) VALUES (?1, ?2, 'success', ?3, ?4, CAST(?5 AS {}), ?6, ?7)",
                    d.column_type(ColumnType::Json)
                ),
                vec![
                    now.div_euclid(1000).into(),
                    NATIVE_URL_EVENT.into(),
                    ctx.actor().into(),
                    ctx.request_id.clone().into(),
                    detail.to_string().into(),
                    ctx.impersonator.clone().into(),
                    ctx.realm_id.clone().into(),
                ],
            );
            match tx.execute(&stmt).await {
                Ok(_) => Ok(tx.commit().await?),
                Err(e) => {
                    let _ = tx.rollback().await;
                    Err(e.into())
                }
            }
        }
        .await;
        if let Err(e) = r {
            tracing::warn!(error = %e, event = NATIVE_URL_EVENT, "audit event not recorded");
        }
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

    /// Serves a signed download. The holder has no tenant context: the
    /// bucket, and so its database set (FB-172), are found from the tenant
    /// the URL carries (`t`, format 2; format 1 URLs name instance-wide
    /// buckets only).
    pub async fn get_signed(&self, p: &SignedParams, range: Option<RangeSpec>) -> Result<ObjectRead> {
        if p.method != SignedMethod::Get {
            return Err(StorageError::SignatureInvalid);
        }
        self.verify_signed(p)?;
        let b = self.bucket_in(p.tenant.as_deref(), &p.bucket).await?;
        self.read_row(&b, &p.key, p.version.as_deref(), range, None).await
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
        let exists = match self.find_row(b, key, None, None).await {
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
                let stmt = new_row_check(b.db.dialect(), &row, pred)?;
                if b.db.fetch_all(&stmt).await?.is_empty() {
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
