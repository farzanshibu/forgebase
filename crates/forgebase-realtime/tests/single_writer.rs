//! FB-146: the realtime dispatcher tails `_forgebase_realtime_outbox` by
//! `seq` and never looks back, so "sequence order is commit order" (ADR-0011,
//! ADR-0028 §7.3) must hold on both backends: exactly one writer connection,
//! a cursor that tails the outbox while many writers commit never skips an
//! event, and on PostgreSQL no other session writes except the two ADR-0029
//! allows. Fails if a second writer is configured.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use common::writers::{census, writer_pid};
use common::{Stack, admin, fast_config};
use forgebase_db::{Backend, BackendKind};
use forgebase_realtime::outbox;
use serde_json::json;

const WRITERS: usize = 8;
const EACH: usize = 20;

/// Every outbox `seq` after `after`, in order (what the dispatcher reads).
async fn after(db: &dyn Backend, mut after: i64) -> Vec<i64> {
    let mut out = Vec::new();
    loop {
        let page = outbox::read_after(db, after, 500).await.unwrap();
        if page.is_empty() {
            return out;
        }
        for e in page {
            assert!(e.seq > after, "outbox order");
            after = e.seq;
            out.push(e.seq);
        }
    }
}

#[tokio::test]
async fn one_writer_keeps_outbox_sequence_order_equal_to_commit_order() {
    let s = Stack::start(fast_config()).await;
    assert_eq!(
        s.db.pool_stats().writer.max,
        1,
        "the database has exactly one writer connection (ADR-0028 §7.3)"
    );
    s.collection(json!({"name": "notes", "fields": [{"name": "body", "type": "text"}]}))
        .await;
    let start = after(&s.db, 0).await.last().copied().unwrap_or(0);
    let writer = match s.db.kind() {
        BackendKind::Postgres => Some(writer_pid(&s.db).await),
        BackendKind::Sqlite => None,
    };

    let done = Arc::new(AtomicBool::new(false));
    let tail = {
        let (db, done) = (s.db.clone(), done.clone());
        tokio::spawn(async move {
            let (mut cursor, mut seen) = (start, Vec::new());
            loop {
                let last_round = done.load(Ordering::SeqCst);
                let page = after(&db, cursor).await;
                if let Some(&last) = page.last() {
                    cursor = last;
                }
                seen.extend(page);
                if last_round {
                    return seen;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
    };
    let sampler = {
        let (db, done) = (s.db.clone(), done.clone());
        tokio::spawn(async move {
            let mut others = Vec::new();
            if let Some(pid) = writer {
                while !done.load(Ordering::SeqCst) {
                    others.extend(census(&db, pid).await);
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            }
            others
        })
    };
    let writers: Vec<_> = (0..WRITERS)
        .map(|w| {
            let data = s.data.clone();
            tokio::spawn(async move {
                for i in 0..EACH {
                    data.create_record(&admin(), "notes", json!({"body": format!("{w}-{i}")}))
                        .await
                        .unwrap();
                }
            })
        })
        .collect();
    for w in writers {
        w.await.unwrap();
    }
    done.store(true, Ordering::SeqCst);
    let seen = tail.await.unwrap();
    let others = sampler.await.unwrap();

    let committed = after(&s.db, start).await;
    assert_eq!(committed.len(), WRITERS * EACH);
    assert_eq!(
        seen, committed,
        "the tailing cursor read every committed change once, in sequence order"
    );
    let second: BTreeSet<String> = others
        .into_iter()
        .filter(|(_, w)| !w.allowed())
        .map(|(pid, w)| format!("{pid}: {w:?}"))
        .collect();
    assert!(second.is_empty(), "second writers: {second:?}");
}
