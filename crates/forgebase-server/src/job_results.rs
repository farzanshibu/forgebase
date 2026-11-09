//! Large job results in FB-30 object storage (FB-64, ADR-0023).
//!
//! `forgebase-storage` depends on `forgebase-jobs`, so the job queue cannot
//! call storage itself; the server joins them with this
//! [`forgebase_jobs::ResultStore`]. Results live as private objects in the
//! bucket [`RESULTS_BUCKET`], written and read as the system principal.

use std::collections::BTreeMap;

use forgebase_data::AccessContext;
use forgebase_db::BoxFuture;
use forgebase_jobs::{ResultStore, StoreError};
use forgebase_storage::{BucketInput, ByteStream, PutRequest, StorageError, StorageService, Visibility};

/// Bucket that holds job results larger than the job row keeps inline.
pub const RESULTS_BUCKET: &str = "forgebase-job-results";

/// A result store on the storage service.
#[derive(Debug, Clone)]
pub struct StorageResults {
    storage: StorageService,
}

impl StorageResults {
    /// Creates the private results bucket when it is missing.
    pub async fn open(storage: StorageService) -> Result<Self, StorageError> {
        let system = AccessContext::system();
        match storage.get_bucket(&system, RESULTS_BUCKET).await {
            Ok(_) => {}
            Err(StorageError::NotFound(_)) => {
                let input = BucketInput {
                    name: RESULTS_BUCKET.to_owned(),
                    visibility: Visibility::Private,
                    versioning: false,
                    max_object_bytes: None,
                    allowed_content_types: None,
                };
                match storage.create_bucket(&system, input).await {
                    Ok(_) | Err(StorageError::Conflict { .. }) => {}
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
        Ok(Self { storage })
    }
}

fn body_of(bytes: Vec<u8>) -> ByteStream {
    Box::pin(futures_util::stream::once(async move {
        Ok::<_, std::io::Error>(axum::body::Bytes::from(bytes))
    }))
}

impl ResultStore for StorageResults {
    fn put(&self, key: String, bytes: Vec<u8>) -> BoxFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
            let req = PutRequest {
                bucket: RESULTS_BUCKET.to_owned(),
                key,
                content_type: Some("application/json".to_owned()),
                metadata: BTreeMap::new(),
                expected_sha256: None,
                content_length: Some(len),
                body: body_of(bytes),
            };
            self.storage
                .put_object(&AccessContext::system(), req)
                .await
                .map(|_| ())
                .map_err(|e| e.to_string())
        })
    }

    fn get(&self, key: String) -> BoxFuture<'_, Result<Option<Vec<u8>>, StoreError>> {
        Box::pin(async move {
            use futures_util::StreamExt;
            let read = match self
                .storage
                .get_object(&AccessContext::system(), RESULTS_BUCKET, &key, None, None)
                .await
            {
                Ok(r) => r,
                Err(StorageError::NotFound(_)) => return Ok(None),
                Err(e) => return Err(e.to_string()),
            };
            let mut out = Vec::with_capacity(usize::try_from(read.size).unwrap_or(0));
            let mut stream = read.stream;
            while let Some(chunk) = stream.next().await {
                out.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
            }
            Ok(Some(out))
        })
    }

    fn delete(&self, key: String) -> BoxFuture<'_, Result<(), StoreError>> {
        Box::pin(async move {
            match self
                .storage
                .delete_object(&AccessContext::system(), RESULTS_BUCKET, &key, None)
                .await
            {
                Ok(_) | Err(StorageError::NotFound(_)) => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        })
    }
}
