//! `storage`: an upload and download mix against `/api/v1/storage` (local
//! driver). 4 KiB and 1 MiB objects carry the traffic; `--storage-large-mib`
//! (default 16, 0 to skip) adds a few large ones. Every upload has unique
//! content, so deduplication does not hide the write.
//!
//! Per 40 operations of a worker: 16 downloads of 4 KiB, 6 of 1 MiB,
//! 8 uploads of 4 KiB, 6 of 1 MiB, 2 downloads and 2 uploads of the large
//! size.

use std::sync::atomic::{AtomicBool, Ordering};

use forgebase_bench::Lcg;
use serde_json::json;

use crate::http::{Conn, Payload};
use crate::stats::Stats;
use crate::{Args, run_workers};

const BUCKET: &str = "load_files";
const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

const OPS: [&str; 6] = [
    "upload_4k",
    "upload_1m",
    "upload_large",
    "download_4k",
    "download_1m",
    "download_large",
];

/// Which of [`OPS`] the `n`-th operation of a worker is (`large` bytes: 0 skips).
fn pick(n: u64, large: bool) -> usize {
    match n % 40 {
        0..=15 => 3,
        16..=21 => 4,
        22..=29 => 0,
        30..=35 => 1,
        36 | 37 if large => 5,
        38 | 39 if large => 2,
        _ => 3,
    }
}

/// `size` bytes whose first 16 are `(worker, n)`, so no two uploads are equal.
fn stamp(buf: &mut [u8], worker: usize, n: u64) {
    buf[..8].copy_from_slice(&(worker as u64).to_le_bytes());
    buf[8..16].copy_from_slice(&n.to_le_bytes());
}

fn filled(size: usize) -> Vec<u8> {
    let mut rng = Lcg::new(size as u64);
    let mut v = vec![0_u8; size];
    for c in v.chunks_mut(8) {
        let r = rng.next_u64().to_le_bytes();
        let n = c.len();
        c.copy_from_slice(&r[..n]);
    }
    v
}

pub(crate) async fn run(a: &Args, token: &str) {
    let mut c = Conn::new(&a.addr);
    let (s, j) = c
        .json(
            "POST",
            "/api/v1/storage/buckets",
            Some(token),
            Some(&json!({"name": BUCKET})),
        )
        .await;
    assert!(s == 201 || s == 409, "create bucket: {s} {j}");
    let large = a.storage_large_mib * MIB;
    // Seed: objects to download.
    let mut seeded = [Vec::new(), Vec::new(), Vec::new()];
    for (slot, (name, size, count)) in [("4k", 4 * KIB, 16), ("1m", MIB, 4), ("large", large, 2)]
        .into_iter()
        .enumerate()
    {
        if size == 0 {
            continue;
        }
        let mut body = filled(size);
        for i in 0..count {
            stamp(&mut body, 9_999, i as u64);
            let key = format!("seed/{name}/{i}");
            let r = c
                .send(
                    "PUT",
                    &format!("/api/v1/storage/buckets/{BUCKET}/objects/{key}"),
                    Some(token),
                    &[],
                    Payload::Raw(&body, "application/octet-stream"),
                    false,
                )
                .await;
            assert_eq!(r.status, 201, "seed {key}");
            seeded[slot].push(key);
        }
    }
    let (addr, token_c) = (a.addr.clone(), token.to_owned());
    let seeded = std::sync::Arc::new(seeded);
    let has_large = large > 0;
    let (stats, secs) = run_workers(
        a.concurrency,
        a.duration,
        OPS.len(),
        move |w, stop: std::sync::Arc<AtomicBool>| {
            let (addr, token, seeded) = (addr.clone(), token_c.clone(), seeded.clone());
            async move {
                let mut c = Conn::new(&addr);
                let mut st: Vec<Stats> = OPS.iter().map(|_| Stats::default()).collect();
                let mut small = filled(4 * KIB);
                let mut mid = filled(MIB);
                let mut big = if has_large { filled(large) } else { Vec::new() };
                let mut rng = Lcg::new(w as u64 + 1);
                let mut n = 0_u64;
                while !stop.load(Ordering::Relaxed) {
                    n += 1;
                    let op = pick(n, has_large);
                    let t = std::time::Instant::now();
                    let (ok, bytes) = if op <= 2 {
                        let body: &mut Vec<u8> = [&mut small, &mut mid, &mut big][op];
                        stamp(body, w, n);
                        let r = c
                            .send(
                                "PUT",
                                &format!("/api/v1/storage/buckets/{BUCKET}/objects/w{w}/{n}"),
                                Some(&token),
                                &[],
                                Payload::Raw(body, "application/octet-stream"),
                                false,
                            )
                            .await;
                        (r.status == 201, body.len())
                    } else {
                        let keys = &seeded[op - 3];
                        let key = &keys[rng.below(keys.len() as u64) as usize];
                        let r = c
                            .send(
                                "GET",
                                &format!("/api/v1/storage/buckets/{BUCKET}/objects/{key}"),
                                Some(&token),
                                &[],
                                Payload::None,
                                false,
                            )
                            .await;
                        let want = [4 * KIB, MIB, big.len()][op - 3];
                        (r.status == 200 && r.body_len == want, r.body_len)
                    };
                    st[op].us.push(t.elapsed().as_micros() as u64);
                    st[op].bytes += bytes as u64;
                    if !ok {
                        st[op].errors += 1;
                    }
                }
                st
            }
        },
    )
    .await;
    for (s, op) in stats.into_iter().zip(OPS) {
        if s.us.is_empty() && s.errors == 0 {
            continue;
        }
        s.report("storage", op, secs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mix_matches_its_documentation() {
        let mut count = [0; 6];
        for n in 0..40 {
            count[pick(n, true)] += 1;
        }
        assert_eq!(count, [8, 6, 2, 16, 6, 2]);
        let mut count = [0; 6];
        for n in 0..40 {
            count[pick(n, false)] += 1;
        }
        assert_eq!(count[2] + count[5], 0);
    }

    #[test]
    fn stamps_make_bodies_unique() {
        let mut a = filled(64);
        let mut b = a.clone();
        stamp(&mut a, 1, 1);
        stamp(&mut b, 1, 2);
        assert_ne!(a, b);
    }
}
