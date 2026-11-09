//! Object storage: buckets, objects, ranges, signed URLs, multipart and resumable uploads.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::generated::{
    Bucket, BucketInput, CompleteInput, CompletePart, ObjectInfo, ObjectPage, PartInfo, SignRequest,
    SignedUrl, Upload, UploadInput,
};
use crate::http::{Req, Transport, describe, key_path, seg};

/// Options for [`StorageApi::put_object`].
#[derive(Debug, Clone, Default)]
pub struct PutOptions {
    /// `Content-Type` of the object (server guesses when `None`).
    pub content_type: Option<String>,
    /// Hex SHA-256; the server rejects the upload with `422 checksum_mismatch` when it differs.
    pub checksum_sha256: Option<String>,
    /// User metadata (`X-ForgeBase-Meta-<name>` headers).
    pub metadata: Vec<(String, String)>,
}

/// Options for [`StorageApi::list_objects`].
#[derive(Debug, Clone, Default)]
pub struct ListObjects {
    /// Only keys with this prefix.
    pub prefix: Option<String>,
    /// Group keys by this delimiter (usually `/`); groups come back as `prefixes`.
    pub delimiter: Option<String>,
    /// Page size.
    pub limit: Option<u32>,
    /// `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

/// What a delete did. (Hand-written: the OpenAPI document names two different schemas `DeleteResult`,
/// so the generated one is not this endpoint's.)
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct DeleteOutcome {
    /// The version removed, or the delete marker created.
    pub version_id: String,
    /// True when a delete marker was added (versioned bucket).
    pub delete_marker: bool,
}

/// A download in progress: read it in chunks instead of holding the whole object.
pub struct ObjectStream {
    res: reqwest::Response,
}

impl ObjectStream {
    /// Content length when the server sent one.
    #[must_use]
    pub fn content_length(&self) -> Option<u64> {
        self.res.content_length()
    }

    /// `Content-Type` of the object.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.res.headers().get("content-type").and_then(|v| v.to_str().ok())
    }

    /// The next chunk; `None` at the end.
    pub async fn chunk(&mut self) -> Result<Option<Bytes>> {
        self.res.chunk().await.map_err(|e| Error::Network(describe(&e)))
    }

    /// The rest of the body.
    pub async fn bytes(self) -> Result<Bytes> {
        self.res.bytes().await.map_err(|e| Error::Network(describe(&e)))
    }
}

/// Object storage: `client.storage()`.
#[derive(Clone)]
pub struct StorageApi {
    pub(crate) t: Arc<Transport>,
}

impl StorageApi {
    fn obj(bucket: &str, key: &str) -> String {
        format!("/api/v1/storage/buckets/{}/objects/{}", seg(bucket), key_path(key))
    }

    // ---- buckets ----

    /// Every bucket the caller may see.
    pub async fn list_buckets(&self) -> Result<Vec<Bucket>> {
        let v = self.t.value(Req::get("/api/v1/storage/buckets")).await?;
        serde_json::from_value(v.get("items").cloned().unwrap_or(Value::Array(Vec::new())))
            .map_err(|e| Error::InvalidResponse(format!("buckets: {e}")))
    }

    /// Creates a bucket.
    pub async fn create_bucket(&self, input: &BucketInput) -> Result<Bucket> {
        self.t.json(Req::post("/api/v1/storage/buckets").json(input)?).await
    }

    /// One bucket.
    pub async fn get_bucket(&self, name: &str) -> Result<Bucket> {
        self.t
            .json(Req::get(format!("/api/v1/storage/buckets/{}", seg(name))))
            .await
    }

    /// Deletes an empty bucket.
    pub async fn delete_bucket(&self, name: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/storage/buckets/{}", seg(name))))
            .await
    }

    // ---- objects ----

    /// Uploads an object in one request. Retried on transient failures (a `PUT` is idempotent).
    pub async fn put_object(
        &self,
        bucket: &str,
        key: &str,
        body: impl Into<Bytes>,
        o: &PutOptions,
    ) -> Result<ObjectInfo> {
        let mut r = Req::put(Self::obj(bucket, key)).bytes(
            body.into(),
            o.content_type
                .clone()
                .unwrap_or_else(|| "application/octet-stream".into()),
        );
        if let Some(c) = &o.checksum_sha256 {
            r = r.header("X-ForgeBase-Checksum-SHA256", c.clone());
        }
        for (k, v) in &o.metadata {
            r = r.header(&format!("X-ForgeBase-Meta-{k}"), v.clone());
        }
        self.t.json(r).await
    }

    /// Downloads an object (a specific `version_id` when given).
    pub async fn get_object(&self, bucket: &str, key: &str, version_id: Option<&str>) -> Result<Bytes> {
        self.t
            .raw(Req::get(Self::obj(bucket, key)).q_opt("version", version_id))
            .await
    }

    /// Downloads bytes `start..=end` (to the end when `end` is `None`).
    pub async fn get_object_range(&self, bucket: &str, key: &str, start: u64, end: Option<u64>) -> Result<Bytes> {
        let range = match end {
            Some(e) => format!("bytes={start}-{e}"),
            None => format!("bytes={start}-"),
        };
        self.t
            .raw(Req::get(Self::obj(bucket, key)).header("Range", range))
            .await
    }

    /// Starts a streaming download.
    pub async fn get_object_stream(&self, bucket: &str, key: &str) -> Result<ObjectStream> {
        Ok(ObjectStream {
            res: self.t.send(&Req::get(Self::obj(bucket, key))).await?,
        })
    }

    /// Lists objects of a bucket.
    pub async fn list_objects(&self, bucket: &str, o: &ListObjects) -> Result<ObjectPage> {
        let r = Req::get(format!("/api/v1/storage/buckets/{}/objects", seg(bucket)))
            .q_opt("prefix", o.prefix.as_deref())
            .q_opt("delimiter", o.delimiter.as_deref())
            .q_opt("limit", o.limit)
            .q_opt("cursor", o.cursor.as_deref());
        self.t.json(r).await
    }

    /// Deletes an object: in a versioned bucket this adds a delete marker (see [`DeleteOutcome`]).
    pub async fn delete_object(&self, bucket: &str, key: &str) -> Result<DeleteOutcome> {
        self.t.json(Req::delete(Self::obj(bucket, key))).await
    }

    /// Permanently deletes one version of an object.
    pub async fn delete_object_version(&self, bucket: &str, key: &str, version_id: &str) -> Result<DeleteOutcome> {
        self.t.json(Req::delete(Self::obj(bucket, key)).q("version", version_id)).await
    }

    /// Copies an object (to another bucket when `dest_bucket` is given).
    pub async fn copy_object(
        &self,
        bucket: &str,
        key: &str,
        dest_key: &str,
        dest_bucket: Option<&str>,
    ) -> Result<ObjectInfo> {
        let body = json!({"key": key, "dest_key": dest_key, "dest_bucket": dest_bucket});
        self.t
            .json(Req::post(format!("/api/v1/storage/buckets/{}/copy", seg(bucket))).json_value(body))
            .await
    }

    /// Moves (renames) an object.
    pub async fn move_object(
        &self,
        bucket: &str,
        key: &str,
        dest_key: &str,
        dest_bucket: Option<&str>,
    ) -> Result<ObjectInfo> {
        let body = json!({"key": key, "dest_key": dest_key, "dest_bucket": dest_bucket});
        self.t
            .json(Req::post(format!("/api/v1/storage/buckets/{}/move", seg(bucket))).json_value(body))
            .await
    }

    /// Every version of a key, newest first.
    pub async fn list_versions(&self, bucket: &str, key: &str) -> Result<Vec<ObjectInfo>> {
        let v = self
            .t
            .value(Req::get(format!(
                "/api/v1/storage/buckets/{}/versions/{}",
                seg(bucket),
                key_path(key)
            )))
            .await?;
        serde_json::from_value(v.get("items").cloned().unwrap_or(Value::Array(Vec::new())))
            .map_err(|e| Error::InvalidResponse(format!("versions: {e}")))
    }

    // ---- signed URLs ----

    /// Creates a time-limited URL anyone can use to GET (download) or PUT (upload) one object.
    pub async fn sign(&self, bucket: &str, req: &SignRequest) -> Result<SignedUrl> {
        self.t
            .json(Req::post(format!("/api/v1/storage/buckets/{}/sign", seg(bucket))).json(req)?)
            .await
    }

    fn signed_req(&self, req: Req, url: &str) -> Result<Req> {
        // The server returns a path (`/api/v1/storage/signed/...?...`) or an absolute URL. Send it as is, no credentials.
        let path = url.strip_prefix(self.t.base.as_str()).unwrap_or(url);
        if !path.starts_with('/') {
            return Err(Error::Config(format!(
                "signed URL {url:?} does not belong to this server"
            )));
        }
        let (p, q) = path.split_once('?').unwrap_or((path, ""));
        let mut r = Req::new(req.method, p.to_owned()).anonymous();
        r.body = req.body;
        for pair in url::form_urlencoded::parse(q.as_bytes()) {
            r = r.q(&pair.0, pair.1);
        }
        Ok(r)
    }

    /// Downloads through a signed GET URL (no credentials are sent).
    pub async fn download_signed(&self, signed: &SignedUrl) -> Result<Bytes> {
        self.t.raw(self.signed_req(Req::get(""), &signed.url)?).await
    }

    /// Uploads through a signed PUT URL (no credentials are sent).
    pub async fn upload_signed(
        &self,
        signed: &SignedUrl,
        body: impl Into<Bytes>,
        content_type: &str,
    ) -> Result<ObjectInfo> {
        self.t
            .json(self.signed_req(Req::put("").bytes(body.into(), content_type), &signed.url)?)
            .await
    }

    // ---- multipart and resumable uploads ----

    /// Opens an upload session.
    pub async fn create_upload(&self, bucket: &str, input: &UploadInput) -> Result<Upload> {
        self.t
            .json(Req::post(format!("/api/v1/storage/buckets/{}/uploads", seg(bucket))).json(input)?)
            .await
    }

    /// State of an upload session (`received_bytes`, uploaded parts): use it to resume.
    pub async fn get_upload(&self, id: &str) -> Result<Upload> {
        self.t
            .json(Req::get(format!("/api/v1/storage/uploads/{}", seg(id))))
            .await
    }

    /// Uploads (or replaces) part `n` (1-based) of a multipart session.
    pub async fn upload_part(&self, id: &str, n: u32, body: impl Into<Bytes>) -> Result<PartInfo> {
        self.t
            .json(
                Req::put(format!("/api/v1/storage/uploads/{}/parts/{n}", seg(id)))
                    .bytes(body.into(), "application/octet-stream"),
            )
            .await
    }

    /// Appends a chunk at `offset` (resumable upload). Returns the session, or the finished object when
    /// the declared size was reached.
    pub async fn append_chunk(&self, id: &str, offset: u64, body: impl Into<Bytes>) -> Result<Value> {
        let r = Req::patch(format!("/api/v1/storage/uploads/{}", seg(id)))
            .header("Upload-Offset", offset.to_string())
            .bytes(body.into(), "application/offset+octet-stream");
        self.t.value(r).await
    }

    /// Assembles the parts into the object.
    pub async fn complete_upload(&self, id: &str, input: &CompleteInput) -> Result<ObjectInfo> {
        self.t
            .json(Req::post(format!("/api/v1/storage/uploads/{}/complete", seg(id))).json(input)?)
            .await
    }

    /// Cancels an upload session.
    pub async fn abort_upload(&self, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/storage/uploads/{}", seg(id))))
            .await
    }

    /// Uploads `data` as a multipart object in `part_size` pieces (at least 1 byte; aborts the session on failure).
    pub async fn upload_multipart(
        &self,
        bucket: &str,
        key: &str,
        data: &Bytes,
        part_size: usize,
        content_type: Option<&str>,
    ) -> Result<ObjectInfo> {
        let size = part_size.max(1);
        let up = self
            .create_upload(
                bucket,
                &UploadInput {
                    key: key.to_owned(),
                    content_type: content_type.map(str::to_owned),
                    size: i64::try_from(data.len()).ok(),
                    kind: Some("multipart".into()),
                    ..UploadInput::default()
                },
            )
            .await?;
        let run = async {
            let mut parts = Vec::new();
            for (i, chunk) in data.chunks(size).enumerate() {
                let n = u32::try_from(i + 1).map_err(|_| Error::Config("too many parts".into()))?;
                let p = self.upload_part(&up.id, n, data.slice_ref(chunk)).await?;
                parts.push(CompletePart {
                    part_number: p.part_number,
                    checksum_sha256: Some(p.checksum_sha256),
                });
            }
            self.complete_upload(
                &up.id,
                &CompleteInput {
                    parts: Some(parts),
                    ..CompleteInput::default()
                },
            )
            .await
        };
        match run.await {
            Ok(o) => Ok(o),
            Err(e) => {
                let _ = self.abort_upload(&up.id).await;
                Err(e)
            }
        }
    }
}
