//! Objects: streaming put with checksums and deduplication, ranged get,
//! listing, deletes, copies, moves and versions.
//!
//! **Write path** (`put_object`, `complete_upload`): the body streams into a
//! staging file while a SHA-256 and a byte count run alongside; a declared
//! checksum or a size limit that does not hold deletes the staged file and
//! fails the request. The `storage.bytes` quota is charged next (`429` when
//! exceeded). Then, under the blob gate, the staged file becomes the
//! content-addressed blob `blobs/<aa>/<sha256>` (or is dropped when that
//! blob exists: deduplication) and one transaction records the blob's
//! reference count and the new object version. A crash between the steps
//! leaves an orphan the reconciler removes; metadata never points at a blob
//! that is not there.

use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use forgebase_data::AccessContext;
use forgebase_db::{Value, WriteTx};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use crate::driver::ByteStream;
use crate::error::{Result, StorageError};
use crate::model::{DeleteResult, ObjectInfo, ObjectPage};
use crate::service::{BucketRow, Need, StorageService, blob_key, id, int, iv, opt_text, st, text, type_allowed, uint};
use crate::util::{from_extension, hex, rfc3339, sniff, validate_key};

const OBJECT_COLS: &str = "id, bucket, key, blob_sha, size, content_type, metadata, is_latest, delete_marker, created_by, created_at, quota_subject";

/// A byte range request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    /// `bytes=a-`: from `a` to the end.
    From(u64),
    /// `bytes=a-b`: inclusive.
    Bounded(u64, u64),
    /// `bytes=-n`: the last `n` bytes.
    Suffix(u64),
}

impl RangeSpec {
    /// The inclusive `(first, last)` bytes of an object of `size` bytes.
    pub fn resolve(self, size: u64) -> Result<(u64, u64)> {
        let unsatisfiable = || StorageError::RangeNotSatisfiable { size };
        if size == 0 {
            return Err(unsatisfiable());
        }
        match self {
            RangeSpec::From(a) if a < size => Ok((a, size - 1)),
            RangeSpec::Bounded(a, b) if a <= b && a < size => Ok((a, b.min(size - 1))),
            RangeSpec::Suffix(n) if n > 0 => Ok((size.saturating_sub(n), size - 1)),
            _ => Err(unsatisfiable()),
        }
    }
}

/// An upload of one object in one request.
pub struct PutRequest {
    /// Bucket.
    pub bucket: String,
    /// Key.
    pub key: String,
    /// Declared content type; detected from the bytes or the key when absent.
    pub content_type: Option<String>,
    /// User metadata.
    pub metadata: BTreeMap<String, String>,
    /// SHA-256 (hex) the body must have; a mismatch rejects the upload.
    pub expected_sha256: Option<String>,
    /// `Content-Length`, when the client sent one (checked against limits
    /// before the body is read).
    pub content_length: Option<u64>,
    /// The body.
    pub body: ByteStream,
}

/// A read of an object.
pub struct ObjectRead {
    /// The version read.
    pub info: ObjectInfo,
    /// Full size of the object.
    pub size: u64,
    /// The inclusive range served (`None`: the whole object).
    pub range: Option<(u64, u64)>,
    /// The bytes.
    pub stream: ByteStream,
}

impl std::fmt::Debug for ObjectRead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectRead")
            .field("info", &self.info)
            .field("range", &self.range)
            .finish_non_exhaustive()
    }
}

/// A fully received, hashed staging file.
pub(crate) struct Staged {
    pub key: String,
    pub sha: String,
    pub size: u64,
    pub head: Vec<u8>,
}

#[derive(Debug, Clone)]
pub(crate) struct ObjectRow {
    pub id: String,
    pub bucket: String,
    pub key: String,
    pub blob_sha: Option<String>,
    pub size: u64,
    pub content_type: String,
    pub metadata: String,
    pub is_latest: bool,
    pub delete_marker: bool,
    pub created_by: String,
    pub created_at: i64,
    pub subject: String,
}

impl ObjectRow {
    fn decode(r: &[Value]) -> Self {
        Self {
            id: text(r, 0),
            bucket: text(r, 1),
            key: text(r, 2),
            blob_sha: opt_text(r, 3),
            size: uint(r, 4),
            content_type: text(r, 5),
            metadata: text(r, 6),
            is_latest: int(r, 7) == 1,
            delete_marker: int(r, 8) == 1,
            created_by: text(r, 9),
            created_at: int(r, 10),
            subject: text(r, 11),
        }
    }

    pub(crate) fn info(&self) -> ObjectInfo {
        ObjectInfo {
            bucket: self.bucket.clone(),
            key: self.key.clone(),
            version_id: self.id.clone(),
            size: self.size,
            content_type: self.content_type.clone(),
            checksum_sha256: self.blob_sha.clone(),
            metadata: crate::model::json_map(&self.metadata),
            is_latest: self.is_latest,
            delete_marker: self.delete_marker,
            created_by: self.created_by.clone(),
            created_at: rfc3339(self.created_at),
        }
    }
}

/// Who and what a committed object is.
pub(crate) struct CommitMeta {
    pub subject: String,
    pub actor: String,
    pub content_type: String,
    pub metadata_json: String,
}

/// A version to install as the latest of a key.
pub(crate) struct NewVersion {
    pub id: String,
    /// Blob hash and size; `None` for a delete marker.
    pub blob: Option<(String, u64)>,
    pub content_type: String,
    pub metadata_json: String,
    pub subject: String,
    pub actor: String,
}

/// A version that left the store: its quota is given back.
pub(crate) struct Removed {
    pub id: String,
    pub subject: String,
    pub size: u64,
}

#[derive(Default)]
struct Ingest {
    sha: Sha256,
    bytes: u64,
    head: Vec<u8>,
    over: bool,
    body_failed: bool,
}

pub(crate) fn metadata_json(m: &BTreeMap<String, String>) -> Result<String> {
    let total: usize = m.iter().map(|(k, v)| k.len() + v.len()).sum();
    if m.len() > 64 || total > 8 * 1024 || m.keys().any(|k| k.is_empty() || k.len() > 128) {
        return Err(StorageError::invalid(
            "invalid_metadata",
            "metadata holds at most 64 entries of 8 KiB in total",
        ));
    }
    serde_json::to_string(m).map_err(|e| StorageError::Internal(e.to_string()))
}

impl StorageService {
    pub(crate) fn object_limit(&self, bucket: &BucketRow) -> u64 {
        let server = self.inner.settings.max_object_bytes;
        bucket.max_object_bytes.map_or(server, |b| b.min(server))
    }

    /// Streams `body` into `staging_key`, hashing and counting on the way.
    /// `limit` caps the bytes accepted; `expected` is a declared SHA-256.
    pub(crate) async fn stage(
        &self,
        staging_key: String,
        body: ByteStream,
        limit: u64,
        expected: Option<&str>,
    ) -> Result<Staged> {
        let state = Arc::new(Mutex::new(Ingest::default()));
        let shared = state.clone();
        let stream = body.map(move |chunk| match chunk {
            Ok(c) => {
                let mut s = shared.lock().map_err(|_| io::Error::other("ingest poisoned"))?;
                s.bytes += c.len() as u64;
                if s.bytes > limit {
                    s.over = true;
                    return Err(io::Error::other("object too large"));
                }
                s.sha.update(&c);
                if s.head.len() < 16 {
                    let take = (16 - s.head.len()).min(c.len());
                    s.head.extend_from_slice(&c[..take]);
                }
                Ok(c)
            }
            Err(e) => {
                if let Ok(mut s) = shared.lock() {
                    s.body_failed = true;
                }
                Err(e)
            }
        });
        let put = self.inner.driver.put(&staging_key, Box::pin(stream)).await;
        let done = {
            let mut s = state
                .lock()
                .map_err(|_| StorageError::Internal("ingest poisoned".into()))?;
            let sha = hex(&std::mem::take(&mut s.sha).finalize());
            (s.over, s.body_failed, s.bytes, sha, std::mem::take(&mut s.head))
        };
        let (over, body_failed, bytes, sha, head) = done;
        if over {
            let _ = self.inner.driver.delete(&staging_key).await;
            return Err(StorageError::TooLarge { limit });
        }
        if body_failed {
            let _ = self.inner.driver.delete(&staging_key).await;
            return Err(StorageError::invalid(
                "upload_interrupted",
                "the request body ended early",
            ));
        }
        if let Err(e) = put {
            let _ = self.inner.driver.delete(&staging_key).await;
            return Err(e);
        }
        if let Some(want) = expected
            && want != sha
        {
            let _ = self.inner.driver.delete(&staging_key).await;
            return Err(StorageError::ChecksumMismatch {
                expected: want.to_owned(),
                actual: sha,
            });
        }
        Ok(Staged {
            key: staging_key,
            sha,
            size: bytes,
            head,
        })
    }

    pub(crate) fn resolve_type(declared: Option<&str>, key: &str, head: &[u8]) -> String {
        let declared = declared.map(str::trim).filter(|t| !t.is_empty());
        match declared {
            Some(t) if t != "application/octet-stream" => t.to_owned(),
            _ => sniff(head)
                .or_else(|| from_extension(key))
                .unwrap_or("application/octet-stream")
                .to_owned(),
        }
    }

    /// Uploads one object from a stream.
    pub async fn put_object(&self, ctx: &AccessContext, req: PutRequest) -> Result<ObjectInfo> {
        validate_key(&req.key)?;
        let bucket = self.bucket_row(&req.bucket).await?;
        self.authorize(ctx, &bucket, Some(&req.key), Need::Write).await?;
        self.put_inner(&Self::subject_of(ctx), &ctx.principal_key(), &bucket, req)
            .await
    }

    /// The upload after authorisation (signed URLs authorise by signature).
    pub(crate) async fn put_inner(
        &self,
        subject: &str,
        actor: &str,
        bucket: &BucketRow,
        req: PutRequest,
    ) -> Result<ObjectInfo> {
        validate_key(&req.key)?;
        let limit = self.object_limit(bucket);
        if req.content_length.is_some_and(|n| n > limit) {
            return Err(StorageError::TooLarge { limit });
        }
        let metadata = metadata_json(&req.metadata)?;
        let expected = req
            .expected_sha256
            .as_deref()
            .map(crate::util::parse_sha256)
            .transpose()?;
        let staged = self
            .stage(format!("staging/single/{}", id()), req.body, limit, expected.as_deref())
            .await?;
        let content_type = Self::resolve_type(req.content_type.as_deref(), &req.key, &staged.head);
        if !type_allowed(&bucket.allowed, &content_type) {
            let _ = self.inner.driver.delete(&staged.key).await;
            return Err(StorageError::invalid(
                "content_type_not_allowed",
                format!("bucket {} does not accept {content_type}", bucket.name),
            ));
        }
        if !crate::util::declared_matches_bytes(&content_type, &staged.head) {
            let _ = self.inner.driver.delete(&staged.key).await;
            return Err(StorageError::invalid(
                "content_type_mismatch",
                format!("the content does not match its declared type {content_type}"),
            ));
        }
        let meta = CommitMeta {
            subject: subject.to_owned(),
            actor: actor.to_owned(),
            content_type,
            metadata_json: metadata,
        };
        self.commit_staged(bucket, &req.key, staged, meta).await
    }

    /// Turns a staged file into the latest version of `key`.
    pub(crate) async fn commit_staged(
        &self,
        bucket: &BucketRow,
        key: &str,
        staged: Staged,
        meta: CommitMeta,
    ) -> Result<ObjectInfo> {
        let CommitMeta {
            subject,
            actor,
            content_type,
            metadata_json,
        } = meta;
        let version_id = id();
        let size = staged.size;
        let charge = i64::try_from(size).unwrap_or(i64::MAX);
        if let Err(e) = self.meter(&subject, charge, &format!("storage:{version_id}:add")).await {
            let _ = self.inner.driver.delete(&staged.key).await;
            return Err(e);
        }
        let gate = self.inner.gate.read().await;
        let placed = self.place_blob(&staged).await;
        let committed = match placed {
            Ok(()) => {
                let nv = NewVersion {
                    id: version_id.clone(),
                    blob: Some((staged.sha.clone(), size)),
                    content_type: content_type.clone(),
                    metadata_json: metadata_json.clone(),
                    subject: subject.clone(),
                    actor: actor.clone(),
                };
                self.install(bucket, key, &nv).await
            }
            Err(e) => Err(e),
        };
        drop(gate);
        match committed {
            Ok((row, removed)) => {
                self.release(removed).await;
                Ok(row.info())
            }
            Err(e) => {
                let _ = self
                    .meter(&subject, -charge, &format!("storage:{version_id}:undo"))
                    .await;
                Err(e)
            }
        }
    }

    /// Makes sure `blobs/<aa>/<sha>` exists, consuming the staged file.
    async fn place_blob(&self, staged: &Staged) -> Result<()> {
        let key = blob_key(&staged.sha);
        match self.inner.driver.head(&key).await? {
            Some(o) if o.size == staged.size => self.inner.driver.delete(&staged.key).await,
            _ => self.inner.driver.rename(&staged.key, &key).await,
        }
    }

    /// Records `nv` as the latest version of `key` in one transaction.
    pub(crate) async fn install(
        &self,
        bucket: &BucketRow,
        key: &str,
        nv: &NewVersion,
    ) -> Result<(ObjectRow, Vec<Removed>)> {
        let now = self.now();
        let mut tx = self.begin().await?;
        match self.install_in(tx.as_mut(), bucket, key, nv, now).await {
            Ok(out) => {
                tx.commit().await?;
                Ok(out)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }

    async fn install_in(
        &self,
        tx: &mut dyn WriteTx,
        bucket: &BucketRow,
        key: &str,
        nv: &NewVersion,
        now: i64,
    ) -> Result<(ObjectRow, Vec<Removed>)> {
        if let Some((sha, size)) = &nv.blob {
            tx.execute(&st(
                "INSERT INTO _forgebase_storage_blobs (sha256, size, refcount, created_at, zero_since) \
                 VALUES (?, ?, 1, ?, NULL) \
                 ON CONFLICT (sha256) DO UPDATE SET refcount = refcount + 1, zero_since = NULL",
                vec![sha.as_str().into(), iv(*size), now.into()],
            ))
            .await?;
        }
        let prev = tx
            .fetch_all(&st(
                &format!("SELECT {OBJECT_COLS} FROM _forgebase_storage_objects WHERE bucket = ? AND key = ? AND is_latest = 1"),
                vec![bucket.name.as_str().into(), key.into()],
            ))
            .await?;
        let mut removed = Vec::new();
        if let Some(p) = prev.rows.first() {
            let p = ObjectRow::decode(p);
            if bucket.versioning {
                tx.execute(&st(
                    "UPDATE _forgebase_storage_objects SET is_latest = 0 WHERE id = ?",
                    vec![p.id.as_str().into()],
                ))
                .await?;
            } else {
                tx.execute(&st(
                    "DELETE FROM _forgebase_storage_objects WHERE id = ?",
                    vec![p.id.as_str().into()],
                ))
                .await?;
                if let Some(sha) = &p.blob_sha {
                    dec_blob(tx, sha, now).await?;
                    removed.push(Removed {
                        id: p.id.clone(),
                        subject: p.subject.clone(),
                        size: p.size,
                    });
                }
            }
        }
        let marker = nv.blob.is_none();
        let size = nv.blob.as_ref().map_or(0, |(_, s)| *s);
        tx.execute(&st(
            "INSERT INTO _forgebase_storage_objects \
             (id, bucket, key, blob_sha, size, content_type, metadata, is_latest, delete_marker, quota_subject, created_by, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?)",
            vec![
                nv.id.as_str().into(),
                bucket.name.as_str().into(),
                key.into(),
                nv.blob.as_ref().map_or(Value::Null, |(s, _)| Value::Text(s.clone())),
                iv(size),
                nv.content_type.as_str().into(),
                nv.metadata_json.as_str().into(),
                marker.into(),
                nv.subject.as_str().into(),
                nv.actor.as_str().into(),
                now.into(),
            ],
        ))
        .await?;
        let row = ObjectRow {
            id: nv.id.clone(),
            bucket: bucket.name.clone(),
            key: key.to_owned(),
            blob_sha: nv.blob.as_ref().map(|(s, _)| s.clone()),
            size,
            content_type: nv.content_type.clone(),
            metadata: nv.metadata_json.clone(),
            is_latest: true,
            delete_marker: marker,
            created_by: nv.actor.clone(),
            created_at: now,
            subject: nv.subject.clone(),
        };
        Ok((row, removed))
    }

    /// Gives back the quota of versions that left the store.
    pub(crate) async fn release(&self, removed: Vec<Removed>) {
        for r in removed {
            let delta = -i64::try_from(r.size).unwrap_or(i64::MAX);
            if let Err(e) = self.meter(&r.subject, delta, &format!("storage:{}:rm", r.id)).await {
                tracing::warn!(version = %r.id, error = %e, "could not release storage quota");
            }
        }
    }

    async fn find_row(&self, bucket: &str, key: &str, version: Option<&str>) -> Result<ObjectRow> {
        let rows = match version {
            Some(v) => {
                self.read(
                    &format!("SELECT {OBJECT_COLS} FROM _forgebase_storage_objects WHERE bucket = ? AND key = ? AND id = ?"),
                    vec![bucket.into(), key.into(), v.into()],
                )
                .await?
            }
            None => {
                self.read(
                    &format!("SELECT {OBJECT_COLS} FROM _forgebase_storage_objects WHERE bucket = ? AND key = ? AND is_latest = 1"),
                    vec![bucket.into(), key.into()],
                )
                .await?
            }
        };
        rows.rows
            .first()
            .map(|r| ObjectRow::decode(r))
            .ok_or_else(|| StorageError::NotFound(format!("object {key}")))
    }

    /// Metadata of an object version (the latest by default).
    pub async fn head_object(
        &self,
        ctx: &AccessContext,
        bucket: &str,
        key: &str,
        version: Option<&str>,
    ) -> Result<ObjectInfo> {
        let b = self.bucket_row(bucket).await?;
        self.authorize(ctx, &b, Some(key), Need::Read).await?;
        let row = self.find_row(bucket, key, version).await?;
        if row.delete_marker {
            return Err(StorageError::NotFound(format!("object {key}")));
        }
        Ok(row.info())
    }

    /// Reads an object (or a range of it) as a stream.
    pub async fn get_object(
        &self,
        ctx: &AccessContext,
        bucket: &str,
        key: &str,
        version: Option<&str>,
        range: Option<RangeSpec>,
    ) -> Result<ObjectRead> {
        let b = self.bucket_row(bucket).await?;
        self.authorize(ctx, &b, Some(key), Need::Read).await?;
        self.read_row(bucket, key, version, range).await
    }

    /// The read itself, after the caller has been authorised (signed URLs
    /// authorise by signature).
    pub(crate) async fn read_row(
        &self,
        bucket: &str,
        key: &str,
        version: Option<&str>,
        range: Option<RangeSpec>,
    ) -> Result<ObjectRead> {
        let row = self.find_row(bucket, key, version).await?;
        let Some(sha) = row.blob_sha.clone().filter(|_| !row.delete_marker) else {
            return Err(StorageError::NotFound(format!("object {key}")));
        };
        let size = row.size;
        let resolved = range.map(|r| r.resolve(size)).transpose()?;
        let driver_range = resolved.map(|(a, b)| (a, b + 1));
        let stream = self.inner.driver.get(&blob_key(&sha), driver_range).await?;
        Ok(ObjectRead {
            info: row.info(),
            size,
            range: resolved,
            stream,
        })
    }

    /// Lists current objects under `prefix`. With a `delimiter`, keys that
    /// contain it after the prefix collapse into `prefixes`.
    pub async fn list_objects(
        &self,
        ctx: &AccessContext,
        bucket: &str,
        prefix: &str,
        delimiter: Option<&str>,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<ObjectPage> {
        let b = self.bucket_row(bucket).await?;
        self.authorize(ctx, &b, Some(prefix), Need::Read).await?;
        let limit = usize::try_from(limit.clamp(1, 1000)).unwrap_or(1000);
        let delimiter = delimiter.filter(|d| !d.is_empty());
        let plen = i64::try_from(prefix.chars().count()).unwrap_or(0);
        let past_group = |s: &str| format!("{s}\u{10FFFF}");
        let mut lower: String = match (cursor, delimiter) {
            (Some(c), Some(d)) if c.ends_with(d) => past_group(c),
            (Some(c), _) => c.to_owned(),
            (None, _) => String::new(),
        };
        let mut objects = Vec::new();
        let mut prefixes: Vec<String> = Vec::new();
        let mut order: Vec<String> = Vec::new();
        let mut next_cursor = None;
        'outer: loop {
            let rows = self
                .read(
                    &format!(
                        "SELECT {OBJECT_COLS} FROM _forgebase_storage_objects \
                         WHERE bucket = ? AND is_latest = 1 AND delete_marker = 0 AND substr(key, 1, ?) = ? AND key > ? \
                         ORDER BY key LIMIT 1000"
                    ),
                    vec![bucket.into(), plen.into(), prefix.into(), lower.as_str().into()],
                )
                .await?;
            if rows.is_empty() {
                break;
            }
            let full = rows.len() == 1000;
            for r in &rows.rows {
                let row = ObjectRow::decode(r);
                let rest = &row.key[prefix.len()..];
                let group = delimiter.and_then(|d| rest.find(d).map(|p| format!("{prefix}{}", &rest[..p + d.len()])));
                let name = group.clone().unwrap_or_else(|| row.key.clone());
                if order.len() == limit {
                    next_cursor = order.last().cloned();
                    break 'outer;
                }
                order.push(name);
                match group {
                    Some(g) => {
                        lower = past_group(&g);
                        prefixes.push(g);
                        continue 'outer;
                    }
                    None => {
                        lower.clone_from(&row.key);
                        objects.push(row.info());
                    }
                }
            }
            if !full {
                break;
            }
        }
        Ok(ObjectPage {
            objects,
            prefixes,
            next_cursor,
        })
    }

    /// Every version of a key, newest first (needs read access).
    pub async fn list_versions(&self, ctx: &AccessContext, bucket: &str, key: &str) -> Result<Vec<ObjectInfo>> {
        let b = self.bucket_row(bucket).await?;
        self.authorize(ctx, &b, Some(key), Need::Read).await?;
        let rows = self
            .read(
                &format!("SELECT {OBJECT_COLS} FROM _forgebase_storage_objects WHERE bucket = ? AND key = ? ORDER BY seq DESC"),
                vec![bucket.into(), key.into()],
            )
            .await?;
        Ok(rows.rows.iter().map(|r| ObjectRow::decode(r).info()).collect())
    }

    /// Deletes an object. Without `version`, a bucket with versioning adds a
    /// delete marker and any other bucket removes the object; with `version`
    /// that one version is removed for good (the newest remaining version
    /// becomes the current one).
    pub async fn delete_object(
        &self,
        ctx: &AccessContext,
        bucket: &str,
        key: &str,
        version: Option<&str>,
    ) -> Result<DeleteResult> {
        let b = self.bucket_row(bucket).await?;
        self.authorize(ctx, &b, Some(key), Need::Write).await?;
        let now = self.now();
        let _gate = self.inner.gate.read().await;
        if let Some(v) = version {
            let mut tx = self.begin().await?;
            let found = tx
                .fetch_all(&st(
                    &format!(
                        "SELECT {OBJECT_COLS} FROM _forgebase_storage_objects WHERE bucket = ? AND key = ? AND id = ?"
                    ),
                    vec![bucket.into(), key.into(), v.into()],
                ))
                .await?;
            let Some(row) = found.rows.first().map(|r| ObjectRow::decode(r)) else {
                let _ = tx.rollback().await;
                return Err(StorageError::NotFound(format!("version {v} of {key}")));
            };
            tx.execute(&st(
                "DELETE FROM _forgebase_storage_objects WHERE id = ?",
                vec![v.into()],
            ))
            .await?;
            if let Some(sha) = &row.blob_sha {
                dec_blob(tx.as_mut(), sha, now).await?;
            }
            if row.is_latest {
                tx.execute(&st(
                    "UPDATE _forgebase_storage_objects SET is_latest = 1 WHERE id = \
                     (SELECT id FROM _forgebase_storage_objects WHERE bucket = ? AND key = ? ORDER BY seq DESC LIMIT 1)",
                    vec![bucket.into(), key.into()],
                ))
                .await?;
            }
            tx.commit().await?;
            if row.blob_sha.is_some() {
                self.release(vec![Removed {
                    id: row.id.clone(),
                    subject: row.subject.clone(),
                    size: row.size,
                }])
                .await;
            }
            return Ok(DeleteResult {
                version_id: row.id,
                delete_marker: false,
            });
        }
        let row = self.find_row(bucket, key, None).await?;
        if row.delete_marker {
            return Err(StorageError::NotFound(format!("object {key}")));
        }
        let marker_id = id();
        let nv = NewVersion {
            id: marker_id.clone(),
            blob: None,
            content_type: "application/octet-stream".into(),
            metadata_json: "{}".into(),
            subject: Self::subject_of(ctx),
            actor: ctx.principal_key(),
        };
        if b.versioning {
            self.install(&b, key, &nv).await?;
            return Ok(DeleteResult {
                version_id: marker_id,
                delete_marker: true,
            });
        }
        let mut tx = self.begin().await?;
        tx.execute(&st(
            "DELETE FROM _forgebase_storage_objects WHERE id = ?",
            vec![row.id.as_str().into()],
        ))
        .await?;
        if let Some(sha) = &row.blob_sha {
            dec_blob(tx.as_mut(), sha, now).await?;
        }
        tx.commit().await?;
        self.release(vec![Removed {
            id: row.id.clone(),
            subject: row.subject.clone(),
            size: row.size,
        }])
        .await;
        Ok(DeleteResult {
            version_id: row.id,
            delete_marker: false,
        })
    }

    /// Copies an object (the latest version, or `version`) to another key,
    /// possibly in another bucket. No bytes move: the copy references the
    /// same blob. The copy counts against the quota of the caller.
    pub async fn copy_object(
        &self,
        ctx: &AccessContext,
        bucket: &str,
        key: &str,
        version: Option<&str>,
        dest_bucket: &str,
        dest_key: &str,
    ) -> Result<ObjectInfo> {
        validate_key(dest_key)?;
        let src = self.bucket_row(bucket).await?;
        self.authorize(ctx, &src, Some(key), Need::Read).await?;
        let dst = self.bucket_row(dest_bucket).await?;
        self.authorize(ctx, &dst, Some(dest_key), Need::Write).await?;
        let row = self.find_row(bucket, key, version).await?;
        if row.delete_marker {
            return Err(StorageError::NotFound(format!("object {key}")));
        }
        if !type_allowed(&dst.allowed, &row.content_type) {
            return Err(StorageError::invalid(
                "content_type_not_allowed",
                format!("bucket {} does not accept {}", dst.name, row.content_type),
            ));
        }
        if row.size > self.object_limit(&dst) {
            return Err(StorageError::TooLarge {
                limit: self.object_limit(&dst),
            });
        }
        self.clone_version(ctx, &row, &dst, dest_key).await
    }

    /// Moves an object: a copy of its latest version followed by a delete of
    /// the source key.
    pub async fn move_object(
        &self,
        ctx: &AccessContext,
        bucket: &str,
        key: &str,
        dest_bucket: &str,
        dest_key: &str,
    ) -> Result<ObjectInfo> {
        if bucket == dest_bucket && key == dest_key {
            return self.head_object(ctx, bucket, key, None).await;
        }
        let src = self.bucket_row(bucket).await?;
        self.authorize(ctx, &src, Some(key), Need::Write).await?;
        let copied = self.copy_object(ctx, bucket, key, None, dest_bucket, dest_key).await?;
        self.delete_object(ctx, bucket, key, None).await?;
        Ok(copied)
    }

    /// Makes an earlier version the current one again (as a new version).
    pub async fn restore_version(
        &self,
        ctx: &AccessContext,
        bucket: &str,
        key: &str,
        version: &str,
    ) -> Result<ObjectInfo> {
        let b = self.bucket_row(bucket).await?;
        self.authorize(ctx, &b, Some(key), Need::Write).await?;
        let row = self.find_row(bucket, key, Some(version)).await?;
        if row.delete_marker {
            return Err(StorageError::invalid(
                "cannot_restore_marker",
                "a delete marker has no content to restore",
            ));
        }
        self.clone_version(ctx, &row, &b, key).await
    }

    async fn clone_version(
        &self,
        ctx: &AccessContext,
        row: &ObjectRow,
        dst: &BucketRow,
        key: &str,
    ) -> Result<ObjectInfo> {
        let (Some(sha), subject) = (row.blob_sha.clone(), Self::subject_of(ctx)) else {
            return Err(StorageError::NotFound(format!("object {}", row.key)));
        };
        let version_id = id();
        let charge = i64::try_from(row.size).unwrap_or(i64::MAX);
        self.meter(&subject, charge, &format!("storage:{version_id}:add"))
            .await?;
        let gate = self.inner.gate.read().await;
        let nv = NewVersion {
            id: version_id.clone(),
            blob: Some((sha, row.size)),
            content_type: row.content_type.clone(),
            metadata_json: row.metadata.clone(),
            subject: subject.clone(),
            actor: ctx.principal_key(),
        };
        let out = self.install(dst, key, &nv).await;
        drop(gate);
        match out {
            Ok((new, removed)) => {
                self.release(removed).await;
                Ok(new.info())
            }
            Err(e) => {
                let _ = self
                    .meter(&subject, -charge, &format!("storage:{version_id}:undo"))
                    .await;
                Err(e)
            }
        }
    }
}

/// Lowers a blob's reference count; at zero the reconciler may collect it.
pub(crate) async fn dec_blob(tx: &mut dyn WriteTx, sha: &str, now: i64) -> Result<()> {
    tx.execute(&st(
        "UPDATE _forgebase_storage_blobs SET refcount = refcount - 1, \
         zero_since = CASE WHEN refcount - 1 = 0 THEN ? ELSE zero_since END WHERE sha256 = ?",
        vec![now.into(), sha.into()],
    ))
    .await?;
    Ok(())
}
