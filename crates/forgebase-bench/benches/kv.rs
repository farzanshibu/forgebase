//! Key-value store (in-memory with write-behind persistence): GET and SET.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use forgebase_bench::Rig;
use forgebase_data::{AccessContext, DefaultAuthorizer};
use forgebase_kv::{KvConfig, KvService};

fn cmd(parts: &[&str]) -> Vec<Vec<u8>> {
    parts.iter().map(|p| p.as_bytes().to_vec()).collect()
}

fn bench(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let rig = rt.block_on(Rig::open());
    let kv = rt
        .block_on(KvService::open(
            KvConfig::default(),
            Arc::new(DefaultAuthorizer),
            Some(rig.backend()),
        ))
        .unwrap();
    let sys = AccessContext::system();
    for i in 0..10_000 {
        kv.exec(&sys, "bench", &cmd(&["SET", &format!("key:{i}"), "value-0123456789"]))
            .unwrap();
    }
    let mut g = c.benchmark_group("kv");
    g.throughput(Throughput::Elements(1));
    let mut i = 0_usize;
    g.bench_function("get", |b| {
        b.iter(|| {
            i = (i + 7919) % 10_000;
            kv.exec(&sys, "bench", &cmd(&["GET", &format!("key:{i}")])).unwrap();
        });
    });
    g.bench_function("set", |b| {
        b.iter(|| {
            i = (i + 7919) % 10_000;
            kv.exec(&sys, "bench", &cmd(&["SET", &format!("key:{i}"), "value-9876543210"]))
                .unwrap();
        });
    });
    g.bench_function("incr", |b| {
        b.iter(|| {
            kv.exec(&sys, "bench", &cmd(&["INCR", "counter"])).unwrap();
        });
    });
    g.finish();
    rt.block_on(kv.shutdown()).unwrap();
}

criterion_group!(benches, bench);
criterion_main!(benches);
