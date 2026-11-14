//! Mock cloud services and the driver conformance suite shared by the
//! storage driver tests (ADR-0040).
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

pub mod azure_mock;
pub mod conformance;
pub mod gcs_mock;
pub mod http_mock;

use forgebase_storage::ByteStream;

/// Deterministic, non-repeating-looking bytes.
pub fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| u8::try_from((i * 7 + i / 251) % 251).unwrap()).collect()
}

/// A body of `data` cut into chunks of `chunk` bytes.
pub fn chunks_of(data: Vec<u8>, chunk: usize) -> ByteStream {
    let parts: Vec<std::io::Result<axum::body::Bytes>> = data
        .chunks(chunk.max(1))
        .map(|c| Ok(axum::body::Bytes::copy_from_slice(c)))
        .collect();
    Box::pin(futures_util::stream::iter(parts))
}

/// A body that delivers `data` in chunks of `chunk` bytes and then fails.
pub fn failing_after(data: Vec<u8>, chunk: usize) -> ByteStream {
    let mut parts: Vec<std::io::Result<axum::body::Bytes>> = data
        .chunks(chunk.max(1))
        .map(|c| Ok(axum::body::Bytes::copy_from_slice(c)))
        .collect();
    parts.push(Err(std::io::Error::other("client went away")));
    Box::pin(futures_util::stream::iter(parts))
}
