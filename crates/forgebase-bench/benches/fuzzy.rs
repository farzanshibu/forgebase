//! Fuzzy search on the portable trigram engine (ADR-0034, FB-166): a
//! `fuzzy` list query on a trigram-indexed text field at 10 000 and
//! 100 000 rows (500 and 2 000 at smoke scale).
//!
//! The rig runs no extension setup, so the engine is the portable one on
//! both backends (side table of trigrams on SQLite and on PostgreSQL with
//! `trigram = off`). The engine in use is printed. Each size is its own
//! collection in one database; names are drawn from a fixed word list with a
//! fixed seed, so the data (and the match counts) repeat run to run.
#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use forgebase_bench::{Lcg, Rig, scaled};
use forgebase_data::schema::{Collection, Field, FieldKind};
use forgebase_data::{IndexDefinition, IndexKind, ListQuery};
use serde_json::json;

const WORDS: [&str; 40] = [
    "laptop", "keyboard", "monitor", "mouse", "speaker", "camera", "tablet", "router", "printer", "scanner", "cable",
    "adapter", "charger", "battery", "headset", "webcam", "dock", "stand", "sleeve", "backpack", "pro", "max", "mini",
    "ultra", "air", "slim", "wireless", "gaming", "office", "travel", "black", "silver", "blue", "red", "green",
    "compact", "classic", "premium", "basic", "studio",
];

fn name(rng: &mut Lcg, i: usize) -> String {
    let n = 1 + rng.below(2) as usize;
    let words: Vec<&str> = (0..n).map(|_| WORDS[rng.below(WORDS.len() as u64) as usize]).collect();
    format!("{} {i}", words.join(" "))
}

fn bench(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let rig = rt.block_on(Rig::open());
    let data = rt.block_on(rig.data());
    let sys = Rig::system();
    eprintln!(
        "fuzzy bench: backend {:?}, trigram engine {}",
        rig.db.kind(),
        data.trigram_engine().name()
    );
    let sizes = [("10k", scaled(10_000, 500)), ("100k", scaled(100_000, 2_000))];
    let mut rng = Lcg::new(99);
    for (label, rows) in sizes {
        let coll = format!("products_{label}");
        rt.block_on(async {
            data.create_collection(
                &sys,
                Collection::base(&coll).with_field(Field::new(
                    "name",
                    FieldKind::Text {
                        min_length: None,
                        max_length: None,
                    },
                )),
            )
            .await
            .unwrap();
            let mut made = 0;
            while made < rows {
                let n = (rows - made).min(1_000);
                let batch = (made..made + n).map(|i| json!({"name": name(&mut rng, i)})).collect();
                data.create_records(&sys, &coll, batch).await.unwrap();
                made += n;
            }
            // The index is built over the loaded rows (the backfill's cost is
            // not benchmarked; maintenance on writes is, below).
            data.create_index(
                &sys,
                IndexDefinition {
                    name: format!("{coll}_name_trgm"),
                    kind: IndexKind::Trigram,
                    ..IndexDefinition::btree(&coll, &["name"])
                },
            )
            .await
            .unwrap();
        });
    }

    let mut g = c.benchmark_group("fuzzy");
    g.sample_size(20);
    g.throughput(Throughput::Elements(1));
    for (label, rows) in sizes {
        let coll = format!("products_{label}");
        // A typo of a word that is in the data, and a two-word query.
        for (qname, text) in [("typo_one_word", "laptpo"), ("two_words", "wireles keybord")] {
            g.bench_function(format!("{label}_{qname}_top_20"), |b| {
                b.to_async(&rt).iter(|| {
                    let (data, sys, coll) = (data.clone(), sys.clone(), coll.clone());
                    async move {
                        let r = data
                            .list_records(
                                &sys,
                                &coll,
                                &ListQuery {
                                    fuzzy: Some(text.into()),
                                    per_page: Some(20),
                                    fuzzy_threshold: Some(0.2),
                                    skip_total: true,
                                    ..ListQuery::default()
                                },
                            )
                            .await
                            .unwrap();
                        assert!(!r.items.is_empty());
                    }
                });
            });
        }
        let mut n = rows as i64;
        g.bench_function(format!("{label}_insert_with_trigram_maintenance"), |b| {
            b.to_async(&rt).iter(|| {
                n += 1;
                let (data, sys, coll) = (data.clone(), sys.clone(), coll.clone());
                async move {
                    data.create_record(&sys, &coll, json!({"name": format!("wireless laptop sleeve {n}")}))
                        .await
                        .unwrap();
                }
            });
        });
    }
    g.finish();
    drop(data);
    rt.block_on(rig.close());
}

criterion_group!(benches, bench);
criterion_main!(benches);
