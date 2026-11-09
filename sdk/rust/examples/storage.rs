//! Upload, download, range reads, signed URLs and multipart uploads.
//!
//!     FORGEBASE_URL=http://localhost:8090 FORGEBASE_API_KEY=fb_... cargo run --example storage

use bytes::Bytes;
use forgebase_sdk::generated::{BucketInput, SignRequest, SignedMethod};
use forgebase_sdk::{ForgeBase, PutOptions};

#[tokio::main]
async fn main() -> forgebase_sdk::Result<()> {
    let url = std::env::var("FORGEBASE_URL").unwrap_or_else(|_| "http://localhost:8090".into());
    let key = std::env::var("FORGEBASE_API_KEY").unwrap_or_default();
    let fb = ForgeBase::builder(url).api_key(key).build()?;
    let st = fb.storage();

    let _ = st
        .create_bucket(&BucketInput {
            name: "docs".into(),
            ..BucketInput::default()
        })
        .await;
    let info = st
        .put_object(
            "docs",
            "notes/hello.txt",
            "hello world",
            &PutOptions {
                content_type: Some("text/plain".into()),
                ..PutOptions::default()
            },
        )
        .await?;
    println!(
        "stored {} ({} bytes, sha256 {:?})",
        info.key, info.size, info.checksum_sha256
    );

    let all = st.get_object("docs", "notes/hello.txt", None).await?;
    let tail = st.get_object_range("docs", "notes/hello.txt", 6, None).await?;
    println!("{} / {}", String::from_utf8_lossy(&all), String::from_utf8_lossy(&tail));

    // A link anyone can fetch for a minute: no credentials needed.
    let link = st
        .sign(
            "docs",
            &SignRequest {
                key: "notes/hello.txt".into(),
                method: Some(SignedMethod::GET),
                expires_in: Some(60),
                ..SignRequest::default()
            },
        )
        .await?;
    let shared = fb.storage().download_signed(&link).await?;
    println!("share: fetched {} bytes through the signed URL", shared.len());

    // Big files: multipart, one part at a time (aborted for you on failure).
    let big = Bytes::from(vec![7u8; 5 * 1024 * 1024]);
    let obj = st
        .upload_multipart("docs", "big.bin", &big, 1024 * 1024, Some("application/octet-stream"))
        .await?;
    println!("multipart upload done: {} bytes", obj.size);
    Ok(())
}
