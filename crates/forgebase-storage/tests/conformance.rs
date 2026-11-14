//! The driver conformance suite on the local filesystem driver (the S3, GCS
//! and Azure drivers run the same suite in `s3.rs`, `gcs.rs` and `azure.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use forgebase_storage::LocalDriver;

#[tokio::test]
async fn conformance_on_the_local_driver() {
    let tmp = tempfile::tempdir().unwrap();
    let d = LocalDriver::new(tmp.path().join("storage"));
    support::conformance::run(&d, "local", 300_000).await;
}
