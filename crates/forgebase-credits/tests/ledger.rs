//! The credit ledger against real persistence: balances, lots, top-ups,
//! grants, spends, refunds, reversals, expiry (lazy and by job),
//! idempotency, authorization, append-only storage, concurrency and the
//! ledger-sum property.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::time::Duration;

use common::*;
use forgebase_credits::{
    AmendInput, CreditError, CreditInput, CreditSink, EntryKind, LedgerQuery, MAINTENANCE_JOB, PurchaseReversal,
    SpendInput,
};
use forgebase_db::{Backend, Statement, TxMode};
use forgebase_entitlements::Clock;
use forgebase_jobs::{JobRegistry, NewJob, PoolConfig, start_pool};
use serde_json::json;

fn insufficient(r: &Result<forgebase_credits::Posted, CreditError>) -> Option<(i64, i64)> {
    match r {
        Err(CreditError::InsufficientCredits { balance, requested, .. }) => Some((*balance, *requested)),
        _ => None,
    }
}

fn conflict_code(r: &Result<impl std::fmt::Debug, CreditError>) -> &'static str {
    match r {
        Err(CreditError::Conflict { code, .. }) => code,
        _ => "",
    }
}

#[tokio::test]
async fn top_ups_grants_and_spends_draw_expiring_credits_first() {
    let env = Env::new().await;
    let s = pk("alice");
    let top = env.top_up(&s, 100, "t1").await;
    let grant = env.grant_until(&s, 50, "g1", T0 + DAY).await;
    let b = env.credits.balance(&sys(), Some(&s)).await.unwrap();
    assert_eq!(b.balance, 150);
    assert_eq!(b.expiring.len(), 1);
    assert_eq!((b.expiring[0].expires_at, b.expiring[0].amount), (T0 + DAY, 50));

    let spent = env
        .credits
        .spend(&sys(), SpendInput::new(&s, 70, "s1").for_feature("ai_tokens", 7))
        .await
        .unwrap();
    assert!(!spent.replayed);
    assert_eq!((spent.entry.kind, spent.entry.amount, spent.entry.balance_after), (EntryKind::Spend, -70, 80));
    assert_eq!(spent.entry.feature.as_deref(), Some("ai_tokens"));
    // The expiring grant is used up first.
    let lots = env.credits.lots(&sys(), Some(&s), true).await.unwrap();
    let left = |entry: &str| lots.iter().find(|l| l.entry_id == entry).unwrap().remaining;
    assert_eq!((left(&grant), left(&top)), (0, 80));
    assert_eq!(env.credits.lots(&sys(), Some(&s), false).await.unwrap().len(), 1);

    let page = env
        .credits
        .ledger(
            &sys(),
            &LedgerQuery {
                subject: Some(s.clone()),
                ..LedgerQuery::default()
            },
        )
        .await
        .unwrap();
    let kinds: Vec<_> = page.items.iter().map(|e| (e.kind, e.amount, e.balance_after)).collect();
    assert_eq!(
        kinds,
        vec![(EntryKind::TopUp, 100, 100), (EntryKind::Grant, 50, 150), (EntryKind::Spend, -70, 80)]
    );
    assert_eq!(env.credits.entry(&sys(), &top).await.unwrap().kind, EntryKind::TopUp);
    env.assert_consistent(&s).await;
}

#[tokio::test]
async fn a_spend_is_all_or_nothing_and_never_overdraws() {
    let env = Env::new().await;
    let s = org_subject("acme");
    env.top_up(&s, 10, "t").await;
    let r = env.credits.spend(&sys(), SpendInput::new(&s, 11, "s")).await;
    assert_eq!(insufficient(&r), Some((10, 11)));
    assert_eq!(env.balance(&s).await, 10);
    // No account at all: a zero balance.
    let r = env.credits.spend(&sys(), SpendInput::new(pk("nobody"), 1, "s")).await;
    assert_eq!(insufficient(&r), Some((0, 1)));
    // Denials are not stored: the same key works once there is enough.
    env.top_up(&s, 1, "t2").await;
    env.credits.spend(&sys(), SpendInput::new(&s, 11, "s")).await.unwrap();
    assert_eq!(env.balance(&s).await, 0);
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_credit_ledger WHERE kind = 'spend'").await,
        1
    );
    env.assert_consistent(&s).await;
}

#[tokio::test]
async fn writes_are_idempotent_and_reused_keys_conflict() {
    let env = Env::new().await;
    let s = pk("alice");
    let first = env
        .credits
        .top_up(&sys(), CreditInput::new(&s, 25, "pay_1"))
        .await
        .unwrap();
    let again = env
        .credits
        .top_up(&sys(), CreditInput::new(&s, 25, "pay_1"))
        .await
        .unwrap();
    assert!(again.replayed && !first.replayed);
    assert_eq!(again.entry, first.entry);
    assert_eq!(env.balance(&s).await, 25);
    let other = env.credits.top_up(&sys(), CreditInput::new(&s, 26, "pay_1")).await;
    assert_eq!(conflict_code(&other), "idempotency_conflict");
    // Keys are per subject.
    env.top_up(&pk("bob"), 5, "pay_1").await;

    let sp = env.credits.spend(&sys(), SpendInput::new(&s, 5, "op")).await.unwrap();
    let sp2 = env.credits.spend(&sys(), SpendInput::new(&s, 5, "op")).await.unwrap();
    assert!(sp2.replayed);
    assert_eq!(sp2.entry.id, sp.entry.id);
    assert_eq!(env.balance(&s).await, 20);
    // A spend key cannot be reused for a top-up either.
    assert_eq!(
        conflict_code(&env.credits.top_up(&sys(), CreditInput::new(&s, 5, "op")).await),
        "idempotency_conflict"
    );
    for key in ["usage:x", "expiry:y", "", "   "] {
        assert!(matches!(
            env.credits.top_up(&sys(), CreditInput::new(&s, 5, key)).await,
            Err(CreditError::Invalid { .. })
        ));
    }
    for amount in [0, -5, forgebase_credits::MAX_AMOUNT + 1] {
        assert!(matches!(
            env.credits.top_up(&sys(), CreditInput::new(&s, amount, "a")).await,
            Err(CreditError::Invalid { code: "invalid_amount", .. })
        ));
    }
    assert!(matches!(
        env.credits.grant(&sys(), CreditInput::new(&s, 5, "past").expiring(T0)).await,
        Err(CreditError::Invalid { code: "invalid_expiry", .. })
    ));
    assert!(matches!(
        env.credits.top_up(&sys(), CreditInput::new("bogus", 5, "b")).await,
        Err(CreditError::Invalid { code: "invalid_subject", .. })
    ));
    env.assert_consistent(&s).await;
}

#[tokio::test]
async fn refunds_return_credits_to_their_lots_and_are_capped() {
    let env = Env::new().await;
    let s = pk("alice");
    let grant = env.grant_until(&s, 30, "g", T0 + HOUR).await;
    let top = env.top_up(&s, 100, "t").await;
    let spend = env.spend(&s, 50, "s").await; // 30 from the grant, 20 from the top-up
    let r1 = env
        .credits
        .refund(&sys(), AmendInput::new(&spend, "r1").amount(10))
        .await
        .unwrap();
    assert_eq!((r1.entry.kind, r1.entry.amount, r1.entry.balance_after), (EntryKind::Refund, 10, 90));
    assert_eq!(r1.entry.ref_entry.as_deref(), Some(spend.as_str()));
    assert_eq!(
        conflict_code(
            &env.credits
                .refund(&sys(), AmendInput::new(&spend, "r2").amount(41))
                .await
        ),
        "refund_exceeds_debit"
    );
    // The rest (40) goes back; then nothing is left to refund.
    let r2 = env.credits.refund(&sys(), AmendInput::new(&spend, "r2")).await.unwrap();
    assert_eq!(r2.entry.amount, 40);
    assert_eq!(
        conflict_code(&env.credits.refund(&sys(), AmendInput::new(&spend, "r3")).await),
        "already_refunded"
    );
    let lots = env.credits.lots(&sys(), Some(&s), true).await.unwrap();
    let left = |entry: &str| lots.iter().find(|l| l.entry_id == entry).unwrap().remaining;
    assert_eq!((left(&grant), left(&top)), (30, 100));
    assert_eq!(env.balance(&s).await, 130);
    // Only debits can be refunded.
    assert!(matches!(
        env.credits.refund(&sys(), AmendInput::new(&top, "x")).await,
        Err(CreditError::Invalid { code: "not_refundable", .. })
    ));
    env.assert_consistent(&s).await;

    // Credits refunded into a lot that lapsed meanwhile lapse again at once.
    let spend2 = env.spend(&s, 30, "s2").await; // all from the grant
    env.clock.advance(2 * HOUR);
    let r = env.credits.refund(&sys(), AmendInput::new(&spend2, "r4")).await.unwrap();
    assert_eq!(r.entry.amount, 30);
    let page = env
        .credits
        .ledger(
            &sys(),
            &LedgerQuery {
                subject: Some(s.clone()),
                after: Some(r.entry.seq),
                ..LedgerQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!((page.items[0].kind, page.items[0].amount), (EntryKind::Expiry, -30));
    assert_eq!(env.balance(&s).await, 100);
    env.assert_consistent(&s).await;
}

#[tokio::test]
async fn reversals_take_back_only_what_is_left_of_a_purchase() {
    let env = Env::new().await;
    let s = org_subject("acme");
    let purchase = env
        .credits
        .credit_purchase(CreditInput {
            external_ref: Some("order_1".into()),
            ..CreditInput::new(&s, 100, "evt_pay_1")
        })
        .await
        .unwrap();
    assert_eq!(purchase.entry.external_ref.as_deref(), Some("order_1"));
    env.spend(&s, 70, "use").await;
    let rev = env
        .credits
        .reverse_purchase(PurchaseReversal {
            subject: s.clone(),
            purchase_key: "evt_pay_1".into(),
            idempotency_key: "evt_refund_1".into(),
            ..PurchaseReversal::default()
        })
        .await
        .unwrap();
    assert_eq!((rev.reversed, rev.unrecovered, rev.replayed), (30, 70, false));
    let e = rev.entry.unwrap();
    assert_eq!((e.kind, e.amount, e.balance_after), (EntryKind::Reversal, -30, 0));
    assert_eq!(e.ref_entry.as_deref(), Some(purchase.entry.id.as_str()));
    let replay = env
        .credits
        .reverse(&sys(), AmendInput::new(&purchase.entry.id, "evt_refund_1"))
        .await
        .unwrap();
    assert!(replay.replayed);
    assert_eq!(replay.reversed, 30);
    // Nothing left: no entry, all unrecovered.
    let none = env
        .credits
        .reverse(&sys(), AmendInput::new(&purchase.entry.id, "again"))
        .await
        .unwrap();
    assert_eq!((none.entry.is_none(), none.reversed, none.unrecovered), (true, 0, 70));
    assert_eq!(
        conflict_code(
            &env.credits
                .reverse(&sys(), AmendInput::new(&purchase.entry.id, "big").amount(71))
                .await
        ),
        "reversal_exceeds_credit"
    );
    assert!(matches!(
        env.credits
            .reverse_purchase(PurchaseReversal {
                subject: s.clone(),
                purchase_key: "unknown".into(),
                idempotency_key: "k".into(),
                ..PurchaseReversal::default()
            })
            .await,
        Err(CreditError::NotFound(_))
    ));
    assert_eq!(env.balance(&s).await, 0);
    env.assert_consistent(&s).await;
    // Plan allowances through the sink, lapsing at the end of the cycle.
    let g = env
        .credits
        .grant_credits(CreditInput::new(&s, 500, "cycle_2026_11").expiring(T0 + 30 * DAY))
        .await
        .unwrap();
    assert_eq!((g.entry.kind, g.entry.expires_at), (EntryKind::Grant, Some(T0 + 30 * DAY)));
}

#[tokio::test]
async fn lapsed_credits_are_not_spendable_and_expire_by_maintenance() {
    let env = Env::new().await;
    let s = pk("alice");
    env.grant_until(&s, 40, "g", T0 + HOUR).await;
    env.top_up(&s, 5, "t").await;
    env.clock.advance(2 * HOUR);
    // The read shows only what can be spent, before any sweep ran.
    assert_eq!(env.balance(&s).await, 5);
    assert!(insufficient(&env.credits.spend(&sys(), SpendInput::new(&s, 6, "s")).await).is_some());
    let a = env.audit(&s).await;
    assert!(a.consistent && a.balance == 45, "{a:?}");
    let report = env.credits.expire_due().await.unwrap();
    assert_eq!((report.lots, report.credits), (1, 40));
    assert_eq!(env.credits.expire_due().await.unwrap().lots, 0);
    let e = env
        .credits
        .ledger(
            &sys(),
            &LedgerQuery {
                subject: Some(s.clone()),
                kind: Some(EntryKind::Expiry),
                ..LedgerQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(e.items.len(), 1);
    assert_eq!((e.items[0].amount, e.items[0].balance_after), (-40, 5));
    env.assert_consistent(&s).await;
    // A write sweeps the subject's lapsed lots in its own transaction.
    env.grant_until(&s, 7, "g2", T0 + 3 * HOUR).await;
    env.clock.advance(2 * HOUR);
    env.spend(&s, 5, "s2").await;
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_credit_ledger WHERE kind = 'expiry'").await,
        2
    );
    assert_eq!(env.balance(&s).await, 0);
    env.assert_consistent(&s).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_maintenance_job_expires_lapsed_credits() {
    let env = Env::new().await;
    let (a, b) = (pk("alice"), org_subject("acme"));
    env.grant_until(&a, 10, "g", T0 + MIN).await;
    env.grant_until(&b, 20, "g", T0 + 2 * MIN).await;
    env.grant_until(&b, 30, "later", T0 + DAY).await;
    env.credits.ensure_schedule().await.unwrap();
    env.credits.ensure_schedule().await.unwrap();
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_job_schedules WHERE name = 'credits-maintenance'")
            .await,
        1
    );
    env.clock.advance(HOUR);
    env.jobs
        .enqueue(&sys(), NewJob::new(MAINTENANCE_JOB, json!({})))
        .await
        .unwrap();
    let registry = forgebase_credits::register_jobs(JobRegistry::new(), env.credits.clone());
    let pool = start_pool(
        env.jobs.clone(),
        registry,
        PoolConfig {
            poll_interval: Duration::from_millis(20),
            maintenance_interval: Duration::from_millis(50),
            ..PoolConfig::default()
        },
    );
    for _ in 0..250 {
        if env
            .count("SELECT COUNT(*) FROM _forgebase_credit_ledger WHERE kind = 'expiry'")
            .await
            == 2
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    pool.shutdown().await;
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_credit_ledger WHERE kind = 'expiry'").await,
        2
    );
    assert_eq!(
        env.count(&format!(
            "SELECT COUNT(*) FROM _forgebase_jobs WHERE kind = '{MAINTENANCE_JOB}' AND status = 'succeeded'"
        ))
        .await,
        1
    );
    assert_eq!(env.balance(&a).await, 0);
    assert_eq!(env.balance(&b).await, 30);
    env.assert_consistent(&a).await;
    env.assert_consistent(&b).await;
}

#[tokio::test]
async fn the_ledger_allocations_and_usage_charges_are_append_only() {
    let env = Env::new().await;
    let s = pk("alice");
    env.top_up(&s, 10, "t").await;
    env.spend(&s, 3, "s").await;
    for sql in [
        "UPDATE _forgebase_credit_ledger SET amount = 1000",
        "DELETE FROM _forgebase_credit_ledger",
        "UPDATE _forgebase_credit_allocations SET amount = 1000",
        "DELETE FROM _forgebase_credit_allocations",
        "INSERT INTO _forgebase_credit_usage (event_id, event_seq, event_at, at, subject, feature_key, units, rate,
             per_units, cost, charged, shortfall) VALUES ('e', 1, 0, 0, 's', 'f', 1, 1, 1, 1, 1, 0)",
        "UPDATE _forgebase_credit_usage SET charged = 0",
        "DELETE FROM _forgebase_credit_usage",
    ] {
        let mut tx = Backend::begin(&env.db, TxMode::Normal).await.unwrap();
        let r = tx.execute(&Statement::new(sql, vec![])).await;
        if sql.starts_with("INSERT") {
            r.unwrap();
        } else {
            let e = r.expect_err(sql).to_string();
            assert!(e.contains("immutable"), "{sql}: {e}");
        }
        tx.commit().await.unwrap();
    }
    // The schema refuses a negative balance and a debit stored as a credit.
    let mut tx = Backend::begin(&env.db, TxMode::Normal).await.unwrap();
    assert!(
        tx.execute(&Statement::new(
            "UPDATE _forgebase_credit_accounts SET balance = -1",
            vec![]
        ))
        .await
        .is_err()
    );
    assert!(
        tx.execute(&Statement::new(
            "INSERT INTO _forgebase_credit_ledger (id, at, subject, kind, amount, balance_after, idempotency_key, actor)
             VALUES ('x', 0, 's', 'spend', 5, 0, 'k', 'a')",
            vec![]
        ))
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
    env.assert_consistent(&s).await;
}

#[tokio::test]
async fn authorization_follows_subjects_and_never_lets_an_org_mint_its_own_credits() {
    let env = Env::new().await;
    let org = env.org("alice", "acme").await;
    let s = org_subject(&org);
    env.member("alice", &org, "bob", &["member"]).await;
    env.member("alice", &org, "bill", &["billing_manager"]).await;
    let alice = user("alice").with_org(&org);
    let bob = user("bob").with_org(&org);
    let bill = user("bill").with_org(&org);
    env.top_up(&s, 100, "t").await;
    env.top_up(&pk("bob"), 10, "t").await;

    // Reads: the subject itself, or billing:read in the organization.
    assert_eq!(env.credits.balance(&alice, None).await.unwrap().balance, 100);
    assert_eq!(env.credits.balance(&bill, None).await.unwrap().balance, 100);
    assert!(env.credits.balance(&bob, None).await.is_err());
    assert_eq!(env.credits.balance(&user("bob"), None).await.unwrap().balance, 10);
    assert!(env.credits.balance(&user("bob"), Some(&pk("alice"))).await.is_err());
    assert!(env.credits.balance(&user("carol"), Some(&s)).await.is_err());
    assert!(matches!(
        env.credits
            .balance(&forgebase_data::AccessContext::anonymous(), Some(&s))
            .await,
        Err(CreditError::Unauthenticated)
    ));

    // Minting: neither the org's owner nor its billing manager.
    for who in [&alice, &bill] {
        assert!(env.credits.top_up(who, CreditInput::new(&s, 1, "m")).await.is_err());
        assert!(env.credits.grant(who, CreditInput::new(&s, 1, "m")).await.is_err());
    }
    assert!(
        env.credits
            .top_up(&user("bob"), CreditInput::new(pk("bob"), 1, "m"))
            .await
            .is_err()
    );
    // Spending: billing:write or usage:write in the org; never one's own
    // personal credits without them.
    let spent = env.credits.spend(&bill, SpendInput::new(&s, 5, "b1")).await.unwrap();
    env.credits.spend(&alice, SpendInput::new(&s, 5, "a1")).await.unwrap();
    assert!(env.credits.spend(&bob, SpendInput::new(&s, 5, "x")).await.is_err());
    assert!(
        env.credits
            .spend(&user("bob"), SpendInput::new(pk("bob"), 5, "x"))
            .await
            .is_err()
    );
    // Refunds of spends: whoever may spend; reversals: operators only.
    env.credits
        .refund(&bill, AmendInput::new(&spent.entry.id, "r"))
        .await
        .unwrap();
    let top = env.top_up(&s, 1, "t2").await;
    assert!(env.credits.reverse(&bill, AmendInput::new(&top, "v")).await.is_err());
    // Another subject's entries are invisible.
    assert!(matches!(
        env.credits.entry(&user("carol"), &top).await,
        Err(CreditError::NotFound(_))
    ));
    assert!(matches!(
        env.credits.refund(&user("carol"), AmendInput::new(&spent.entry.id, "r9")).await,
        Err(CreditError::NotFound(_))
    ));
    // Rates and maintenance: platform administrators.
    assert!(
        env.credits
            .put_rate(
                &alice,
                "ai",
                forgebase_credits::RateInput {
                    credits_per_unit: 1,
                    per_units: None,
                    basis: None
                }
            )
            .await
            .is_err()
    );
    assert!(env.credits.usage_cursor(&alice).await.is_err());

    // A realm-level billing manager (an operator) may mint for anyone.
    env.realm_roles("olga", &["billing_manager"]).await;
    let olga = user("olga");
    env.credits.top_up(&olga, CreditInput::new(&s, 50, "o1")).await.unwrap();
    env.credits
        .grant(&olga, CreditInput::new(pk("bob"), 5, "o2"))
        .await
        .unwrap();
    assert_eq!(env.credits.balance(&olga, Some(&s)).await.unwrap().balance, 146);
    env.credits.reverse(&olga, AmendInput::new(&top, "v")).await.unwrap();
    env.assert_consistent(&s).await;
    env.assert_consistent(&pk("bob")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_spends_never_overdraw() {
    let env = Env::new().await;
    let s = org_subject("acme");
    env.top_up(&s, 100, "t").await;
    let mut tasks = Vec::new();
    for i in 0..40 {
        let credits = env.credits.clone();
        let s = s.clone();
        tasks.push(tokio::spawn(async move {
            credits
                .spend(&sys(), SpendInput::new(s, 7, format!("spend-{i}")))
                .await
        }));
    }
    let mut ok = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => ok += 1,
            Err(CreditError::InsufficientCredits { .. }) => {}
            Err(e) => panic!("{e}"),
        }
    }
    assert_eq!(ok, 14);
    assert_eq!(env.balance(&s).await, 2);
    env.assert_consistent(&s).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_mixed_writes_keep_every_account_consistent() {
    let env = Env::new().await;
    env.metered(&["ai_tokens"]).await;
    env.credits
        .put_rate(
            &admin(),
            "ai_tokens",
            forgebase_credits::RateInput {
                credits_per_unit: 3,
                per_units: None,
                basis: None,
            },
        )
        .await
        .unwrap();
    let subjects = [pk("a"), pk("b"), org_subject("o")];
    for s in &subjects {
        env.top_up(s, 50, "seed").await;
        env.grant_until(s, 30, "seed-grant", T0 + 30 * MIN).await;
    }
    let mut tasks = Vec::new();
    for i in 0..60usize {
        let env_c = env.credits.clone();
        let ent = env.ent.clone();
        let clock = env.clock.clone();
        let s = subjects[i % 3].clone();
        tasks.push(tokio::spawn(async move {
            match i % 6 {
                0 => {
                    let _ = env_c.top_up(&sys(), CreditInput::new(&s, 4, format!("t{i}"))).await;
                }
                1 | 2 => {
                    let _ = env_c.spend(&sys(), SpendInput::new(&s, 9, format!("s{i}"))).await;
                }
                3 => {
                    ent.consume(
                        &sys(),
                        forgebase_entitlements::UsageRequest::new("ai_tokens", 2, format!("u{i}")).for_subject(&s),
                    )
                    .await
                    .unwrap();
                    env_c.consume_usage().await.unwrap();
                }
                4 => {
                    clock.advance(MIN);
                    env_c.expire_due().await.unwrap();
                }
                _ => {
                    let _ = env_c.consume_usage().await.unwrap();
                }
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    env.credits.consume_usage().await.unwrap();
    for s in &subjects {
        let a = env.audit(s).await;
        assert!(a.consistent && a.balance >= 0, "{a:?}");
    }
    // Every usage event was charged once.
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_credit_usage").await,
        env.count("SELECT COUNT(*) FROM _forgebase_usage_events").await
    );
}

/// xorshift64*: deterministic pseudo-random numbers for the property test.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: i64) -> i64 {
        i64::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }
}

/// Walks a subject's ledger: every `balance_after` is the running sum, and
/// the last one is the balance.
async fn assert_chain(env: &Env, subject: &str) {
    let mut after = None;
    let mut sum = 0;
    loop {
        let page = env
            .credits
            .ledger(
                &sys(),
                &LedgerQuery {
                    subject: Some(subject.to_owned()),
                    after,
                    limit: Some(1000),
                    ..LedgerQuery::default()
                },
            )
            .await
            .unwrap();
        for e in &page.items {
            sum += e.amount;
            assert_eq!(e.balance_after, sum, "{subject} at seq {}", e.seq);
            assert!(e.balance_after >= 0);
        }
        match page.next {
            Some(n) => after = Some(n),
            None => break,
        }
    }
    let a = env.audit(subject).await;
    assert!(a.consistent, "{a:?}");
    assert_eq!(a.ledger_sum, sum);
}

/// Property: under any sequence of top-ups, grants (with and without
/// expiry), spends, refunds, reversals, usage charges, clock moves and
/// expiry runs, the ledger sums to the balance, which equals what the lots
/// hold, and never goes negative.
#[tokio::test]
async fn property_ledger_sum_always_equals_balance() {
    let env = Env::new().await;
    env.metered(&["ai_tokens"]).await;
    env.credits
        .put_rate(
            &admin(),
            "ai_tokens",
            forgebase_credits::RateInput {
                credits_per_unit: 1,
                per_units: Some(3),
                basis: None,
            },
        )
        .await
        .unwrap();
    let subjects = [pk("p1"), pk("p2"), org_subject("po")];
    let mut debits: Vec<String> = Vec::new();
    let mut credits: Vec<String> = Vec::new();
    for seed in 1..=12u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        for step in 0..40 {
            let s = &subjects[rng.below(3) as usize];
            let key = format!("k{seed}-{step}");
            match rng.below(9) {
                0 => {
                    let p = env
                        .credits
                        .top_up(&sys(), CreditInput::new(s, 1 + rng.below(60), &key))
                        .await
                        .unwrap();
                    credits.push(p.entry.id);
                }
                1 => {
                    let mut input = CreditInput::new(s, 1 + rng.below(60), &key);
                    if rng.below(2) == 0 {
                        input = input.expiring(env.clock.now_ms() + 1 + rng.below(3 * HOUR));
                    }
                    credits.push(env.credits.grant(&sys(), input).await.unwrap().entry.id);
                }
                2 | 3 => match env.credits.spend(&sys(), SpendInput::new(s, 1 + rng.below(50), &key)).await {
                    Ok(p) => debits.push(p.entry.id),
                    Err(CreditError::InsufficientCredits { .. }) => {}
                    Err(e) => panic!("{e}"),
                },
                4 if !debits.is_empty() => {
                    let id = debits[rng.below(debits.len() as i64) as usize].clone();
                    let mut input = AmendInput::new(id, &key);
                    if rng.below(2) == 0 {
                        input = input.amount(1 + rng.below(30));
                    }
                    match env.credits.refund(&sys(), input).await {
                        Ok(_) | Err(CreditError::Conflict { .. }) => {}
                        Err(e) => panic!("{e}"),
                    }
                }
                5 if !credits.is_empty() => {
                    let id = credits[rng.below(credits.len() as i64) as usize].clone();
                    let mut input = AmendInput::new(id, &key);
                    if rng.below(2) == 0 {
                        input = input.amount(1 + rng.below(30));
                    }
                    match env.credits.reverse(&sys(), input).await {
                        Ok(_) | Err(CreditError::Conflict { .. }) => {}
                        Err(e) => panic!("{e}"),
                    }
                }
                6 => {
                    env.use_units(s, "ai_tokens", 1 + rng.below(40), &key).await;
                    let run = env.credits.consume_usage().await.unwrap();
                    assert_eq!(run.events_charged, 1);
                }
                7 => env.clock.advance(rng.below(90 * MIN)),
                _ => {
                    env.credits.expire_due().await.unwrap();
                }
            }
            for s in &subjects {
                let a = env.audit(s).await;
                assert!(a.consistent, "seed {seed} step {step}: {a:?}");
                let b = env.credits.balance(&sys(), Some(s)).await.unwrap();
                let open: i64 = env
                    .credits
                    .lots(&sys(), Some(s), false)
                    .await
                    .unwrap()
                    .iter()
                    .map(|l| l.remaining)
                    .sum();
                assert_eq!(b.balance, open, "spendable balance = open lots");
            }
        }
    }
    for s in &subjects {
        assert_chain(&env, s).await;
    }
    // A pass that touched every operation kind.
    for kind in EntryKind::ALL {
        let n = env
            .count(&format!(
                "SELECT COUNT(*) FROM _forgebase_credit_ledger WHERE kind = '{}'",
                kind.as_str()
            ))
            .await;
        assert!(n > 0, "no {kind:?} entries");
    }
}
