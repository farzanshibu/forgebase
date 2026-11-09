//! The storage driver interface: a flat key → bytes store.
//!
//! The service owns everything above it (buckets, versions, checksums,
//! deduplication); a driver only stores, reads, copies, lists and deletes
//! blobs under internal keys (`blobs/<aa>/<sha256>`, `staging/...`).
//! Bodies are streams, so neither side holds a whole object in memory.

use std::future::Future;
use std::io;
use std::pin::Pin;

use axum::body::Bytes;
use futures_util::Stream;

use crate::error::Result;

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A stream of body chunks.
pub type ByteStream = Pin<Box<dyn Stream<Item = io::Result<Bytes>> + Send>>;

/// What a driver knows about a stored blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverObject {
    /// Driver key.
    pub key: String,
    /// Size in bytes.
    pub size: u64,
    /// Last modification, epoch milliseconds.
    pub modified_ms: i64,
}

/// An interchangeable blob store (local directory, S3-compatible service).
pub trait StorageDriver: Send + Sync + std::fmt::Debug {
    /// Driver name for diagnostics (`local`, `s3`).
    fn name(&self) -> &'static str;

    /// Stores the stream under `key`, replacing any earlier blob. The blob
    /// becomes visible only when the whole stream arrived: a failing stream
    /// leaves nothing under `key`. Returns the bytes stored.
    fn put(&self, key: &str, body: ByteStream) -> BoxFuture<'_, Result<u64>>;

    /// Streams bytes `[start, end)` of the blob (the whole blob for `None`).
    /// `end` is clamped to the blob size.
    fn get(&self, key: &str, range: Option<(u64, u64)>) -> BoxFuture<'_, Result<ByteStream>>;

    /// Size and modification time, or `None` when there is no such blob.
    fn head(&self, key: &str) -> BoxFuture<'_, Result<Option<DriverObject>>>;

    /// Deletes the blob. Deleting a missing blob succeeds.
    fn delete(&self, key: &str) -> BoxFuture<'_, Result<()>>;

    /// Copies a blob.
    fn copy(&self, from: &str, to: &str) -> BoxFuture<'_, Result<()>>;

    /// Moves a blob (the default copies, then deletes the source).
    fn rename<'a>(&'a self, from: &'a str, to: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.copy(from, to).await?;
            self.delete(from).await
        })
    }

    /// Every blob whose key starts with `prefix`, in key order.
    fn list(&self, prefix: &str) -> BoxFuture<'_, Result<Vec<DriverObject>>>;

    /// Checks that the store is reachable and writable.
    fn ping(&self) -> BoxFuture<'_, Result<()>>;
}

/// The stream of a single in-memory chunk.
pub fn once(bytes: impl Into<Bytes>) -> ByteStream {
    let b: Bytes = bytes.into();
    Box::pin(futures_util::stream::once(async move { Ok(b) }))
}

/// Reads a stream to the end into memory (tests, small blobs).
pub async fn collect(mut s: ByteStream) -> io::Result<Vec<u8>> {
    use futures_util::StreamExt;
    let mut out = Vec::new();
    while let Some(chunk) = s.next().await {
        out.extend_from_slice(&chunk?);
    }
    Ok(out)
}
