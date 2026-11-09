//! Multipart and resumable upload sessions.
//!
//! A session stages each part as its own file (`staging/<upload>/<n>-<rand>`)
//! and records it in `_forgebase_storage_parts` only after the part arrived
//! whole and matched its checksum, so an interrupted request leaves no part
//! behind. *Multipart* sessions take numbered parts in any order and are
//! completed explicitly. *Resumable* sessions take sequential chunks at an
//! offset (`Upload-Offset`): after a dropped connection the client asks where
//! the server is and continues from there; a chunk at the wrong offset is
//! refused with the server's offset. A resumable session with a declared
//! size completes itself when the last byte arrives. Completion streams the
//! parts through the same hash-and-commit path as a single upload.

use std::io;

use forgebase_data::AccessContext;
use forgebase_db::Value;
use futures_util::StreamExt;

use crate::driver::ByteStream;
use crate::error::{Result, StorageError};
use crate::model::{CompleteInput, ObjectInfo, PartInfo, Upload, UploadInput};
use crate::objects::{CommitMeta, metadata_json};
use crate::service::{BucketRow, Need, StorageService, id, int, iv, opt_int, opt_text, st, text, type_allowed, uint};
use crate::util::{parse_sha256, rfc3339, validate_key};

const MAX_PARTS: u32 = 10_000;

#[derive(Debug, Clone)]
struct UploadRow {
    id: String,
    bucket: String,
    key: String,
    kind: String,
    content_type: String,
    metadata: String,
    declared_size: Option<u64>,
    declared_sha: Option<String>,
    received: u64,
    next_part: u32,
    created_by: String,
    subject: String,
    expires_at: i64,
}

const UPLOAD_COLS: &str = "id, bucket, key, kind, content_type, metadata, declared_size, declared_sha, received_bytes, \
                           next_part, created_by, quota_subject, expires_at";

/// The result of one resumable chunk.
#[derive(Debug, Clone, PartialEq)]
pub struct AppendOutcome {
    /// The session after the chunk (its offset is `received_bytes`).
    pub upload: Upload,
    /// The finished object, when this chunk completed the declared size.
    pub object: Option<ObjectInfo>,
}

impl StorageService {
    async fn upload_row(&self, id: &str) -> Result<UploadRow> {
        let rows = self
            .read(
                &format!("SELECT {UPLOAD_COLS} FROM _forgebase_storage_uploads WHERE id = ?"),
                vec![id.into()],
            )
            .await?;
        let r = rows
            .rows
            .first()
            .ok_or_else(|| StorageError::NotFound(format!("upload {id}")))?;
        Ok(UploadRow {
            id: text(r, 0),
            bucket: text(r, 1),
            key: text(r, 2),
            kind: text(r, 3),
            content_type: text(r, 4),
            metadata: text(r, 5),
            declared_size: opt_int(r, 6).and_then(|n| u64::try_from(n).ok()),
            declared_sha: opt_text(r, 7),
            received: uint(r, 8),
            next_part: u32::try_from(int(r, 9)).unwrap_or(1),
            created_by: text(r, 10),
            subject: text(r, 11),
            expires_at: int(r, 12),
        })
    }

    /// Loads a session and checks the caller may use it: write access to the
    /// target key, and being the creator (administrators may use any).
    async fn session(&self, ctx: &AccessContext, id: &str) -> Result<(UploadRow, BucketRow)> {
        let row = self.upload_row(id).await?;
        let bucket = self.bucket_row(&row.bucket).await?;
        self.authorize(ctx, &bucket, Some(&row.key), Need::Write).await?;
        if !ctx.is_privileged() && ctx.principal_key() != row.created_by {
            return Err(StorageError::NotFound(format!("upload {id}")));
        }
        if row.expires_at <= self.now() {
            return Err(StorageError::conflict(
                "upload_expired",
                "the upload session has expired",
            ));
        }
        Ok((row, bucket))
    }

    async fn parts_of(&self, id: &str) -> Result<Vec<(PartInfo, String)>> {
        let rows = self
            .read(
                "SELECT part_number, size, sha256, staging_key FROM _forgebase_storage_parts \
                 WHERE upload_id = ? ORDER BY part_number",
                vec![id.into()],
            )
            .await?;
        Ok(rows
            .rows
            .iter()
            .map(|r| {
                (
                    PartInfo {
                        part_number: u32::try_from(int(r, 0)).unwrap_or(0),
                        size: uint(r, 1),
                        checksum_sha256: text(r, 2),
                    },
                    text(r, 3),
                )
            })
            .collect())
    }

    async fn wire_upload(&self, row: &UploadRow) -> Result<Upload> {
        Ok(Upload {
            id: row.id.clone(),
            bucket: row.bucket.clone(),
            key: row.key.clone(),
            kind: row.kind.clone(),
            content_type: row.content_type.clone(),
            declared_size: row.declared_size,
            declared_sha256: row.declared_sha.clone(),
            received_bytes: row.received,
            parts: self.parts_of(&row.id).await?.into_iter().map(|(p, _)| p).collect(),
            expires_at: rfc3339(row.expires_at),
        })
    }

    /// Opens an upload session.
    pub async fn create_upload(&self, ctx: &AccessContext, bucket: &str, input: UploadInput) -> Result<Upload> {
        validate_key(&input.key)?;
        let b = self.bucket_row(bucket).await?;
        self.authorize(ctx, &b, Some(&input.key), Need::Write).await?;
        let kind = match input.kind.as_deref() {
            None | Some("multipart") => "multipart",
            Some("resumable") => "resumable",
            Some(_) => {
                return Err(StorageError::invalid(
                    "invalid_upload_kind",
                    "kind is multipart or resumable",
                ));
            }
        };
        let limit = self.object_limit(&b);
        if input.size.is_some_and(|n| n > limit) {
            return Err(StorageError::TooLarge { limit });
        }
        let content_type = input
            .content_type
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or("application/octet-stream")
            .to_owned();
        if input.content_type.is_some() && !type_allowed(&b.allowed, &content_type) {
            return Err(StorageError::invalid(
                "content_type_not_allowed",
                format!("bucket {} does not accept {content_type}", b.name),
            ));
        }
        let metadata = metadata_json(&input.metadata)?;
        let sha = input.checksum_sha256.as_deref().map(parse_sha256).transpose()?;
        let now = self.now();
        let expires = now + i64::try_from(self.inner.settings.upload_ttl_secs).unwrap_or(i64::MAX / 2000) * 1000;
        let upload_id = id();
        let mut tx = self.begin().await?;
        tx.execute(&st(
            "INSERT INTO _forgebase_storage_uploads \
             (id, bucket, key, kind, content_type, metadata, declared_size, declared_sha, received_bytes, next_part, \
              created_by, quota_subject, created_at, expires_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, 1, ?, ?, ?, ?)",
            vec![
                upload_id.as_str().into(),
                bucket.into(),
                input.key.as_str().into(),
                kind.into(),
                content_type.as_str().into(),
                metadata.as_str().into(),
                input.size.map_or(Value::Null, iv),
                sha.map_or(Value::Null, Value::Text),
                ctx.principal_key().into(),
                Self::subject_of(ctx).into(),
                now.into(),
                expires.into(),
            ],
        ))
        .await?;
        tx.commit().await?;
        let row = self.upload_row(&upload_id).await?;
        self.wire_upload(&row).await
    }

    /// The state of a session: bytes received and the parts held. A client
    /// that lost its connection reads `received_bytes` and resumes there.
    pub async fn get_upload(&self, ctx: &AccessContext, id: &str) -> Result<Upload> {
        let (row, _) = self.session(ctx, id).await?;
        self.wire_upload(&row).await
    }

    /// Stores one numbered part of a multipart session (replacing an earlier
    /// part of the same number).
    pub async fn upload_part(
        &self,
        ctx: &AccessContext,
        id: &str,
        part_number: u32,
        expected_sha256: Option<&str>,
        body: ByteStream,
    ) -> Result<PartInfo> {
        let (row, bucket) = self.session(ctx, id).await?;
        if row.kind != "multipart" {
            return Err(StorageError::conflict(
                "wrong_upload_kind",
                "a resumable upload takes chunks at an offset, not numbered parts",
            ));
        }
        if !(1..=MAX_PARTS).contains(&part_number) {
            return Err(StorageError::invalid("invalid_part_number", "part numbers are 1-10000"));
        }
        let expected = expected_sha256.map(parse_sha256).transpose()?;
        let staged = self
            .stage(
                format!("staging/{id}/{part_number:05}-{}", crate::service::id()),
                body,
                self.object_limit(&bucket),
                expected.as_deref(),
            )
            .await?;
        self.register_part(&row, part_number, &staged.key, staged.size, &staged.sha, None)
            .await
    }

    async fn register_part(
        &self,
        row: &UploadRow,
        part_number: u32,
        staging_key: &str,
        size: u64,
        sha: &str,
        at_offset: Option<u64>,
    ) -> Result<PartInfo> {
        let now = self.now();
        let mut tx = self.begin().await?;
        let cur = tx
            .fetch_all(&st(
                "SELECT received_bytes, next_part FROM _forgebase_storage_uploads WHERE id = ?",
                vec![row.id.as_str().into()],
            ))
            .await?;
        let Some(c) = cur.rows.first() else {
            let _ = tx.rollback().await;
            let _ = self.inner.driver.delete(staging_key).await;
            return Err(StorageError::NotFound(format!("upload {}", row.id)));
        };
        if let Some(offset) = at_offset {
            let server = uint(c, 0);
            if server != offset {
                let _ = tx.rollback().await;
                let _ = self.inner.driver.delete(staging_key).await;
                return Err(StorageError::OffsetMismatch { server_offset: server });
            }
            if row.declared_size.is_some_and(|d| server + size > d) {
                let _ = tx.rollback().await;
                let _ = self.inner.driver.delete(staging_key).await;
                return Err(StorageError::invalid(
                    "exceeds_declared_size",
                    "the chunk goes past the declared size",
                ));
            }
        }
        let old = tx
            .fetch_all(&st(
                "SELECT staging_key FROM _forgebase_storage_parts WHERE upload_id = ? AND part_number = ?",
                vec![row.id.as_str().into(), i64::from(part_number).into()],
            ))
            .await?;
        tx.execute(&st(
            "INSERT INTO _forgebase_storage_parts (upload_id, part_number, size, sha256, staging_key, created_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT (upload_id, part_number) DO UPDATE SET size = excluded.size, sha256 = excluded.sha256, \
             staging_key = excluded.staging_key, created_at = excluded.created_at",
            vec![
                row.id.as_str().into(),
                i64::from(part_number).into(),
                iv(size),
                sha.into(),
                staging_key.into(),
                now.into(),
            ],
        ))
        .await?;
        tx.execute(&st(
            "UPDATE _forgebase_storage_uploads SET \
             received_bytes = (SELECT COALESCE(SUM(size), 0) FROM _forgebase_storage_parts WHERE upload_id = ?1), \
             next_part = MAX(next_part, ?2 + 1) WHERE id = ?1",
            vec![row.id.as_str().into(), i64::from(part_number).into()],
        ))
        .await?;
        tx.commit().await?;
        if let Some(o) = old.rows.first() {
            let key = text(o, 0);
            if key != staging_key {
                let _ = self.inner.driver.delete(&key).await;
            }
        }
        Ok(PartInfo {
            part_number,
            size,
            checksum_sha256: sha.to_owned(),
        })
    }

    /// Appends a chunk to a resumable session. `offset` must equal the
    /// session's current offset, otherwise [`StorageError::OffsetMismatch`]
    /// names where the server is. Reaching the declared size completes the
    /// upload.
    pub async fn append_chunk(
        &self,
        ctx: &AccessContext,
        id: &str,
        offset: u64,
        expected_sha256: Option<&str>,
        body: ByteStream,
    ) -> Result<AppendOutcome> {
        let (row, bucket) = self.session(ctx, id).await?;
        if row.kind != "resumable" {
            return Err(StorageError::conflict(
                "wrong_upload_kind",
                "a multipart upload takes numbered parts, not offsets",
            ));
        }
        if offset != row.received {
            return Err(StorageError::OffsetMismatch {
                server_offset: row.received,
            });
        }
        if row.next_part > MAX_PARTS {
            return Err(StorageError::invalid(
                "too_many_chunks",
                "the session holds the most chunks it can; complete it",
            ));
        }
        let remaining = row
            .declared_size
            .map_or(self.object_limit(&bucket), |d| d.saturating_sub(row.received));
        let limit = remaining.min(self.object_limit(&bucket).saturating_sub(row.received));
        let expected = expected_sha256.map(parse_sha256).transpose()?;
        let staged = self
            .stage(
                format!("staging/{id}/{:05}-{}", row.next_part, crate::service::id()),
                body,
                limit,
                expected.as_deref(),
            )
            .await?;
        self.register_part(&row, row.next_part, &staged.key, staged.size, &staged.sha, Some(offset))
            .await?;
        let fresh = self.upload_row(id).await?;
        if fresh.declared_size == Some(fresh.received) {
            let object = self.finish(&fresh, &bucket, &CompleteInput::default()).await?;
            return Ok(AppendOutcome {
                upload: Upload {
                    parts: Vec::new(),
                    ..self.wire_upload_done(&fresh)
                },
                object: Some(object),
            });
        }
        Ok(AppendOutcome {
            upload: self.wire_upload(&fresh).await?,
            object: None,
        })
    }

    fn wire_upload_done(&self, row: &UploadRow) -> Upload {
        Upload {
            id: row.id.clone(),
            bucket: row.bucket.clone(),
            key: row.key.clone(),
            kind: row.kind.clone(),
            content_type: row.content_type.clone(),
            declared_size: row.declared_size,
            declared_sha256: row.declared_sha.clone(),
            received_bytes: row.received,
            parts: Vec::new(),
            expires_at: rfc3339(row.expires_at),
        }
    }

    /// Assembles the parts into the object.
    pub async fn complete_upload(&self, ctx: &AccessContext, id: &str, input: CompleteInput) -> Result<ObjectInfo> {
        let (row, bucket) = self.session(ctx, id).await?;
        self.finish(&row, &bucket, &input).await
    }

    async fn finish(&self, row: &UploadRow, bucket: &BucketRow, input: &CompleteInput) -> Result<ObjectInfo> {
        let held = self.parts_of(&row.id).await?;
        let chosen: Vec<(PartInfo, String)> = match &input.parts {
            None => held,
            Some(list) => {
                let mut out = Vec::new();
                let mut last = 0;
                for want in list {
                    if want.part_number <= last {
                        return Err(StorageError::invalid(
                            "invalid_part_order",
                            "parts must be listed in ascending order, once each",
                        ));
                    }
                    last = want.part_number;
                    let (info, key) = held
                        .iter()
                        .find(|(p, _)| p.part_number == want.part_number)
                        .cloned()
                        .ok_or_else(|| {
                            StorageError::invalid("missing_part", format!("part {} was not uploaded", want.part_number))
                        })?;
                    if let Some(sum) = want.checksum_sha256.as_deref().map(parse_sha256).transpose()?
                        && sum != info.checksum_sha256
                    {
                        return Err(StorageError::ChecksumMismatch {
                            expected: sum,
                            actual: info.checksum_sha256,
                        });
                    }
                    out.push((info, key));
                }
                out
            }
        };
        if chosen.is_empty() {
            return Err(StorageError::invalid("no_parts", "the upload has no parts"));
        }
        let total: u64 = chosen.iter().map(|(p, _)| p.size).sum();
        if row.declared_size.is_some_and(|d| d != total) {
            return Err(StorageError::invalid(
                "size_mismatch",
                format!(
                    "the parts hold {total} bytes, the upload declared {}",
                    row.declared_size.unwrap_or(0)
                ),
            ));
        }
        let expected = match input.checksum_sha256.as_deref() {
            Some(s) => Some(parse_sha256(s)?),
            None => row.declared_sha.clone(),
        };
        let keys: Vec<String> = chosen.iter().map(|(_, k)| k.clone()).collect();
        let driver = self.inner.driver.clone();
        let joined = futures_util::stream::iter(keys)
            .then(move |k| {
                let driver = driver.clone();
                async move {
                    match driver.get(&k, None).await {
                        Ok(s) => s,
                        Err(e) => Box::pin(futures_util::stream::once(async move {
                            Err(io::Error::other(e.to_string()))
                        })) as ByteStream,
                    }
                }
            })
            .flatten();
        let staged = self
            .stage(
                format!("staging/{}/assembled-{}", row.id, id()),
                Box::pin(joined),
                self.object_limit(bucket),
                expected.as_deref(),
            )
            .await?;
        let content_type = Self::resolve_type(Some(&row.content_type), &row.key, &staged.head);
        if !type_allowed(&bucket.allowed, &content_type) {
            let _ = self.inner.driver.delete(&staged.key).await;
            return Err(StorageError::invalid(
                "content_type_not_allowed",
                format!("bucket {} does not accept {content_type}", bucket.name),
            ));
        }
        let info = self
            .commit_staged(
                bucket,
                &row.key,
                staged,
                CommitMeta {
                    subject: row.subject.clone(),
                    actor: row.created_by.clone(),
                    content_type,
                    metadata_json: row.metadata.clone(),
                },
            )
            .await?;
        self.drop_session(&row.id).await?;
        Ok(info)
    }

    /// Deletes a session and its staged parts.
    pub(crate) async fn drop_session(&self, id: &str) -> Result<u64> {
        let parts = self.parts_of(id).await?;
        let mut tx = self.begin().await?;
        tx.execute(&st(
            "DELETE FROM _forgebase_storage_uploads WHERE id = ?",
            vec![id.into()],
        ))
        .await?;
        tx.commit().await?;
        let n = parts.len() as u64;
        for (_, key) in parts {
            let _ = self.inner.driver.delete(&key).await;
        }
        Ok(n)
    }

    /// Cancels a session and discards what it received.
    pub async fn abort_upload(&self, ctx: &AccessContext, id: &str) -> Result<()> {
        let (row, _) = self.session(ctx, id).await?;
        self.drop_session(&row.id).await?;
        Ok(())
    }
}
