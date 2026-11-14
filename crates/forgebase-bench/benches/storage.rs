//! Object storage on the local driver: put and get of 4 KiB, 1 MiB and 16 MiB
//! objects, a four-part multipart upload and listing with prefixes (ADR-0018).
//!
//! Real persistence: object metadata in the selected backend
//! (`FORGEBASE_TEST_BACKEND`), blobs as files under the rig's data
//! directory (content-addressed, staged, then renamed). Each put writes a
//! different body (a counter in the first bytes) so deduplication does not
//! turn the write into a metadata-only operation; the listing bench seeds
//! identical bodies on purpose, because it measures the listing.
//!
//! Smoke scale (`cargo test --benches`) uses a small tree; `cargo bench`
//! uses the full one (see [`forgebase_bench::smoke`]).
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use bytes::Bytes;
use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use forgebase_auth::crypto::Keyring;
use forgebase_bench::{Rig, scaled};
use forgebase_core::DataLayout;
use forgebase_data::AccessContext;
use forgebase_entitlements::SystemClock;
use forgebase_storage::{
    BucketInput, ByteStream, CompleteInput, CompletePart, LocalDriver, PutRequest, StorageService, StorageSettings,
    UploadInput,
};
use futures_util::StreamExt;

const CHUNK: usize = 64 * 1024;

/// A body of `size` bytes streamed in 64 KiB chunks; `tag` makes it unique.
fn body(size: usize, tag: u64) -> ByteStream {
    let mut first = vec![0x5A_u8; size.min(CHUNK)];
    first[..size.min(8)].copy_from_slice(&tag.to_le_bytes()[..size.min(8)]);
    let filler = Bytes::from(vec![0xA5_u8; CHUNK]);
    let mut chunks = vec![Bytes::from(first)];
    let mut left = size.saturating_sub(CHUNK);
    while left > 0 {
        let n = left.min(CHUNK);
        chunks.push(filler.slice(..n));
        left -= n;
    }
    Box::pin(futures_util::stream::iter(chunks.into_iter().map(Ok)))
}

fn put_request(bucket: &str, key: String, size: usize, tag: u64) -> PutRequest {
    PutRequest {
        bucket: bucket.into(),
        key,
        content_type: Some("application/octet-stream".into()),
        metadata: Default::default(),
        expected_sha256: None,
        content_length: Some(size as u64),
        body: body(size, tag),
    }
}

async fn open_storage(rig: &Rig) -> StorageService {
    let layout = DataLayout::new(rig.tmp.path());
    layout.ensure().unwrap();
    let data = rig.data().await;
    StorageService::open(
        rig.backend(),
        data,
        Arc::new(LocalDriver::new(layout.storage_dir())),
        Arc::new(SystemClock),
        StorageSettings {
            quota_feature: None,
            ..StorageSettings::default()
        },
        Keyring::from_master(&[7_u8; 32]),
    )
    .await
    .unwrap()
}

async fn drain(mut s: ByteStream) -> usize {
    let mut n = 0;
    while let Some(c) = s.next().await {
        n += c.unwrap().len();
    }
    n
}

fn bench(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let rig = rt.block_on(Rig::open());
    let storage = rt.block_on(open_storage(&rig));
    let ctx = AccessContext::admin("root");
    rt.block_on(async {
        storage
            .create_bucket(
                &ctx,
                BucketInput {
                    name: "bench".into(),
                    ..BucketInput::default()
                },
            )
            .await
            .unwrap();
    });
    // One stored object per size for the get benches.
    let sizes = [("4k", 4 * 1024_usize), ("1m", 1024 * 1024), ("16m", 16 * 1024 * 1024)];
    for (name, size) in sizes {
        rt.block_on(async {
            storage
                .put_object(&ctx, put_request("bench", format!("get/{name}"), size, 1))
                .await
                .unwrap();
        });
    }

    let mut g = c.benchmark_group("storage");
    let mut tag = 1_000_u64;
    for (name, size) in sizes {
        g.throughput(Throughput::Bytes(size as u64));
        // Big objects are slow and each one stays on disk: fewer samples.
        g.sample_size(if size >= 1024 * 1024 { 10 } else { 30 });
        g.bench_function(format!("put_{name}"), |b| {
            b.to_async(&rt).iter(|| {
                tag += 1;
                let (storage, ctx, tag) = (storage.clone(), ctx.clone(), tag);
                async move {
                    storage
                        .put_object(&ctx, put_request("bench", format!("put/{name}/{tag}"), size, tag))
                        .await
                        .unwrap();
                }
            });
        });
        g.bench_function(format!("get_{name}"), |b| {
            b.to_async(&rt).iter(|| {
                let (storage, ctx) = (storage.clone(), ctx.clone());
                async move {
                    let r = storage
                        .get_object(&ctx, "bench", &format!("get/{name}"), None, None)
                        .await
                        .unwrap();
                    assert_eq!(drain(r.stream).await, size);
                }
            });
        });
    }

    // Multipart: create, four 4 MiB parts, complete (16 MiB in all).
    let part = 4 * 1024 * 1024_usize;
    g.throughput(Throughput::Bytes(4 * part as u64));
    g.sample_size(10);
    g.bench_function("multipart_4x4m", |b| {
        b.to_async(&rt).iter(|| {
            tag += 1;
            let (storage, ctx, tag) = (storage.clone(), ctx.clone(), tag);
            async move {
                let up = storage
                    .create_upload(
                        &ctx,
                        "bench",
                        UploadInput {
                            key: format!("multi/{tag}"),
                            kind: Some("multipart".into()),
                            ..UploadInput::default()
                        },
                    )
                    .await
                    .unwrap();
                let mut parts = Vec::new();
                for n in 1..=4_u32 {
                    let p = storage
                        .upload_part(&ctx, &up.id, n, None, body(part, tag * 10 + u64::from(n)))
                        .await
                        .unwrap();
                    parts.push(CompletePart {
                        part_number: p.part_number,
                        checksum_sha256: Some(p.checksum_sha256),
                    });
                }
                let info = storage
                    .complete_upload(
                        &ctx,
                        &up.id,
                        CompleteInput {
                            parts: Some(parts),
                            checksum_sha256: None,
                        },
                    )
                    .await
                    .unwrap();
                assert_eq!(info.size, 4 * part as u64);
            }
        });
    });

    // Listing: `top` top-level folders of `per` objects each (tiny, identical
    // bodies: the blob is stored once).
    let (top, per) = (scaled(20, 4), scaled(250, 10));
    rt.block_on(async {
        for t in 0..top {
            for o in 0..per {
                storage
                    .put_object(&ctx, put_request("bench", format!("tree/d{t:02}/o{o:04}"), 16, 7))
                    .await
                    .unwrap();
            }
        }
    });
    g.sample_size(30);
    g.throughput(Throughput::Elements(1));
    g.bench_function("list_root_delimiter", |b| {
        b.to_async(&rt).iter(|| {
            let (storage, ctx) = (storage.clone(), ctx.clone());
            async move {
                let p = storage
                    .list_objects(&ctx, "bench", "tree/", Some("/"), 100, None)
                    .await
                    .unwrap();
                assert_eq!(p.prefixes.len(), top);
            }
        });
    });
    g.bench_function("list_prefix_page_100", |b| {
        b.to_async(&rt).iter(|| {
            let (storage, ctx) = (storage.clone(), ctx.clone());
            async move {
                let p = storage
                    .list_objects(&ctx, "bench", "tree/d03/", None, 100, None)
                    .await
                    .unwrap();
                assert_eq!(p.objects.len(), per.min(100));
            }
        });
    });
    g.bench_function("list_prefix_deep_page", |b| {
        b.iter_batched(
            || (storage.clone(), ctx.clone()),
            |(storage, ctx)| {
                rt.block_on(async move {
                    // Page through one folder to the end by cursor.
                    let mut cursor: Option<String> = None;
                    let mut seen = 0;
                    loop {
                        let p = storage
                            .list_objects(&ctx, "bench", "tree/d02/", None, 100, cursor.as_deref())
                            .await
                            .unwrap();
                        seen += p.objects.len();
                        match p.next_cursor {
                            Some(c) => cursor = Some(c),
                            None => break,
                        }
                    }
                    assert_eq!(seen, per);
                });
            },
            BatchSize::SmallInput,
        );
    });
    g.finish();
    drop(storage);
    rt.block_on(rig.close());
}

criterion_group!(benches, bench);
criterion_main!(benches);
