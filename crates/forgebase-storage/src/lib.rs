//! ForgeBase object storage (FB-30, ADR-0018).
//!
//! | Piece | Where |
//! |---|---|
//! | Driver interface; local filesystem and S3-compatible (SigV4) drivers | [`driver`], [`local`], [`s3`], [`sigv4`] |
//! | Native Google Cloud Storage and Azure Blob drivers (ADR-0040) | [`gcs`], [`azure`], [`cloud`] |
//! | Buckets, ACLs, bucket policies, authorization, storage quota; routing over database sets (FB-172) | [`service`] |
//! | Streaming put/get, ranges, checksums, deduplication, versions, copy/move | [`objects`] |
//! | Multipart and resumable uploads | [`uploads`] |
//! | Object rules over object fields (FB-140) | [`rules`] |
//! | Signed URLs (HMAC, expiry; key sealed with the auth keyring) | [`signed`] |
//! | Staging area reconciler and its job | [`reconcile`] |
//! | REST under `/api/v1/storage` with OpenAPI | [`http`] |
//!
//! Object bytes live in a [`StorageDriver`]; the database holds metadata
//! (migration 0017). Content is stored once per SHA-256 (`blobs/<aa>/<sha>`)
//! and referenced by object versions, so copies, moves and version restores
//! move no bytes.

pub mod azure;
pub mod cloud;
pub mod driver;
mod error;
pub mod gcs;
pub mod http;
pub mod local;
pub mod model;
pub mod objects;
pub mod reconcile;
pub mod rules;
pub mod s3;
pub mod service;
pub mod signed;
pub mod sigv4;
pub mod uploads;
pub mod util;

pub use azure::{AzureAuth, AzureConfig, AzureDriver, SasPermission};
pub use driver::{ByteStream, DriverObject, PresignGet, StorageDriver};
pub use error::{Result, StorageError};
pub use gcs::{GcsAuth, GcsConfig, GcsDriver, ServiceAccount};
pub use http::{extension, openapi, router};
pub use local::LocalDriver;
pub use model::*;
pub use objects::{ObjectRead, PutRequest, RangeSpec};
pub use reconcile::register_jobs;
pub use rules::object_schema;
pub use s3::{S3Config, S3Driver};
pub use service::{RECONCILE_JOB, RECONCILE_SCHEDULE, STORAGE_FEATURE, StorageService, StorageSettings};
pub use signed::{SignedParams, body_of};
pub use uploads::AppendOutcome;
