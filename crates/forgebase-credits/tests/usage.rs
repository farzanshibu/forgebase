//! Credits paying for FB-24 usage: pricing, shortfall, idempotent replay by
//! event id with the `seq` cursor, overage pricing, and statistics.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use forgebase_credits::{AmendInput, CreditError, Cursor, EntryKind, Interval, RateBasis, RateInput, SpendInput, Window};
use forgebase_entitlements::{Enforcement, FeatureInput, FeatureKind, GaugeRequest, PlanInput, QuotaPolicy};

async fn priced(env: &Env, feature: &str, credits_per_unit: i64, per_units: i64, basis: RateBasis) {
    env.credits
        .put_rate(
            &admin(),
            feature,
            RateInput {
                credits_per_unit,
                per_units: Some(per_units),
                basis: Some(basis),
            },
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn usage_events_of_priced_features_are_charged_in_credits() {
    let env = Env::new().await;
    env.metered(&["ai_tokens", "exports"]).await;
    priced(&env, "ai_tokens", 1, 1000, RateBasis::Units).await;
    let s = org_subject("acme");
    env.top_up(&s, 10, "t").await;
    env.use_units(&s, "ai_tokens", 2500, "u1").await;
    env.use_units(&s, "exports", 4, "u2").await; // not priced
    let run = env.credits.consume_usage().await.unwrap();
    assert_eq!((run.events_read, run.events_charged, run.credits_charged, run.shortfall), (2, 1, 3, 0));
    assert_eq!(env.balance(&s).await, 7);
    let page = env.credits.usage_charges(&sys(), Some(&s), false, None, None, false).await.unwrap();
    assert_eq!(page.items.len(), 1);
    let c = &page.items[0];
    assert_eq!((c.feature.as_str(), c.units, c.cost, c.charged, c.shortfall), ("ai_tokens", 2500, 3, 3, 0));
    let entry = env.credits.entry(&sys(), c.entry_id.as_ref().unwrap()).await.unwrap();
    assert_eq!((entry.kind, entry.amount), (EntryKind::Consumption, -3));
    assert_eq!(entry.usage_event_id.as_deref(), Some(c.event_id.as_str()));
    assert_eq!(entry.usage_seq, Some(c.event_seq));
    assert_eq!((entry.feature.as_deref(), entry.units), (Some("ai_tokens"), Some(2500)));
    assert_eq!(run.cursor, env.count("SELECT MAX(seq) FROM _forgebase_usage_events").await);
    assert_eq!(env.credits.usage_cursor(&admin()).await.unwrap().seq, run.cursor);
    // Nothing new: nothing read.
    assert_eq!(env.credits.consume_usage().await.unwrap().events_read, 0);
    env.assert_consistent(&s).await;
}

#[tokio::test]
async fn the_balance_never_goes_negative_the_rest_is_shortfall() {
    let env = Env::new().await;
    env.metered(&["ai_tokens"]).await;
    priced(&env, "ai_tokens", 2, 1, RateBasis::Units).await;
    let (s, broke) = (pk("alice"), pk("bob"));
    env.top_up(&s, 5, "t").await;
    env.use_units(&s, "ai_tokens", 4, "u1").await; // costs 8
    env.use_units(&broke, "ai_tokens", 1, "u2").await; // no account
    let run = env.credits.consume_usage().await.unwrap();
    assert_eq!((run.events_charged, run.credits_charged, run.shortfall), (2, 5, 5));
    assert_eq!(env.balance(&s).await, 0);
    let short = env.credits.usage_charges(&sys(), None, true, None, None, true).await.unwrap();
    let rows: Vec<_> = short
        .items
        .iter()
        .map(|c| (c.subject.as_str(), c.cost, c.charged, c.shortfall, c.entry_id.is_some()))
        .collect();
    assert_eq!(rows, vec![(s.as_str(), 8, 5, 3, true), (broke.as_str(), 2, 0, 2, false)]);
    // Only the subject (or billing:read) sees its charges; all needs privilege.
    assert_eq!(
        env.credits
            .usage_charges(&user("alice"), None, true, None, None, false)
            .await
            .unwrap()
            .items
            .len(),
        1
    );
    assert!(
        env.credits
            .usage_charges(&user("alice"), Some(&broke), false, None, None, false)
            .await
            .is_err()
    );
    env.assert_consistent(&s).await;
    env.assert_consistent(&broke).await;
}

#[tokio::test]
async fn replaying_the_usage_feed_never_charges_an_event_twice() {
    let env = Env::new().await;
    env.metered(&["ai_tokens"]).await;
    priced(&env, "ai_tokens", 1, 1, RateBasis::Units).await;
    let s = pk("alice");
    env.top_up(&s, 1000, "t").await;
    for i in 0..450 {
        env.use_units(&s, "ai_tokens", 1, &format!("u{i}")).await;
    }
    // Two consumers at once: the batches serialise, every event once.
    let (a, b) = tokio::join!(env.credits.consume_usage(), env.credits.consume_usage());
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.events_charged + b.events_charged, 450);
    assert_eq!(env.balance(&s).await, 550);
    // Back to the start of the feed: everything is read again, nothing charged.
    env.credits.set_usage_cursor(&admin(), Cursor { seq: 0 }).await.unwrap();
    let replay = env.credits.consume_usage().await.unwrap();
    assert_eq!((replay.events_read, replay.events_charged, replay.credits_charged), (450, 0, 0));
    assert_eq!(env.balance(&s).await, 550);
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_credit_ledger WHERE kind = 'consumption'").await,
        450
    );
    // A cursor ahead of the feed skips what lies before it.
    env.use_units(&s, "ai_tokens", 1, "late").await;
    let max = env.count("SELECT MAX(seq) FROM _forgebase_usage_events").await;
    env.credits.set_usage_cursor(&admin(), Cursor { seq: max }).await.unwrap();
    assert_eq!(env.credits.consume_usage().await.unwrap().events_read, 0);
    assert_eq!(env.balance(&s).await, 550);
    env.credits.set_usage_cursor(&admin(), Cursor { seq: max - 1 }).await.unwrap();
    assert_eq!(env.credits.consume_usage().await.unwrap().credits_charged, 1);
    assert!(env.credits.set_usage_cursor(&user("alice"), Cursor { seq: 0 }).await.is_err());
    env.assert_consistent(&s).await;
}

#[tokio::test]
async fn overage_pricing_charges_only_units_past_a_soft_limit_and_gauges_are_free() {
    let env = Env::new().await;
    for (key, kind) in [("calls", FeatureKind::Metered), ("storage", FeatureKind::Gauge)] {
        env.ent
            .put_feature(
                &admin(),
                key,
                FeatureInput {
                    name: None,
                    description: None,
                    kind,
                    unit: None,
                    permissions: Vec::new(),
                },
            )
            .await
            .unwrap();
    }
    env.ent
        .put_quota_policy(
            &admin(),
            "calls",
            QuotaPolicy {
                enforcement: Enforcement::Soft,
                ..QuotaPolicy::default()
            },
        )
        .await
        .unwrap();
    env.ent
        .put_plan(
            &admin(),
            "free",
            PlanInput {
                name: None,
                description: None,
                is_default: true,
                active: true,
                trial_days: 0,
                metadata: None,
                features: vec![
                    pf("calls", Some(10)),
                    forgebase_entitlements::PlanFeature {
                        period: None,
                        ..pf("storage", Some(1000))
                    },
                ],
            },
        )
        .await
        .unwrap();
    priced(&env, "calls", 2, 1, RateBasis::Overage).await;
    priced(&env, "storage", 1, 1, RateBasis::Units).await;
    let s = pk("alice");
    env.top_up(&s, 100, "t").await;
    env.use_units(&s, "calls", 8, "u1").await; // inside the plan
    env.use_units(&s, "calls", 5, "u2").await; // 3 overage
    env.ent
        .adjust_gauge(
            &forgebase_data::AccessContext::system(),
            GaugeRequest {
                feature: "storage".into(),
                subject: Some(s.clone()),
                scope_key: None,
                delta: Some(500),
                value: None,
                idempotency_key: Some("g".into()),
                metadata: None,
            },
        )
        .await
        .unwrap();
    let run = env.credits.consume_usage().await.unwrap();
    assert_eq!((run.events_read, run.events_charged, run.credits_charged), (3, 1, 6));
    assert_eq!(env.balance(&s).await, 94);
    env.assert_consistent(&s).await;
}

#[tokio::test]
async fn rates_are_managed_by_administrators_and_listed_to_callers() {
    let env = Env::new().await;
    priced(&env, "ai_tokens", 3, 1000, RateBasis::Units).await;
    priced(&env, "ai_tokens", 4, 1000, RateBasis::Units).await;
    let rates = env.credits.rates(&user("alice")).await.unwrap();
    assert_eq!(rates.len(), 1);
    assert_eq!((rates[0].credits_per_unit, rates[0].per_units), (4, 1000));
    assert!(matches!(
        env.credits
            .put_rate(
                &admin(),
                "ai_tokens",
                RateInput {
                    credits_per_unit: 0,
                    per_units: None,
                    basis: None
                }
            )
            .await,
        Err(CreditError::Invalid { code: "invalid_rate", .. })
    ));
    assert!(env.credits.delete_rate(&user("alice"), "ai_tokens").await.is_err());
    env.credits.delete_rate(&admin(), "ai_tokens").await.unwrap();
    assert!(matches!(
        env.credits.delete_rate(&admin(), "ai_tokens").await,
        Err(CreditError::NotFound(_))
    ));
    assert!(env.credits.rates(&forgebase_data::AccessContext::anonymous()).await.is_err());
}

#[tokio::test]
async fn refunds_of_usage_charges_are_for_operators() {
    let env = Env::new().await;
    env.metered(&["ai_tokens"]).await;
    priced(&env, "ai_tokens", 1, 1, RateBasis::Units).await;
    let org = env.org("alice", "acme").await;
    let s = org_subject(&org);
    env.top_up(&s, 50, "t").await;
    env.use_units(&s, "ai_tokens", 20, "u").await;
    env.credits.consume_usage().await.unwrap();
    let charge = env.credits.usage_charges(&sys(), Some(&s), false, None, None, false).await.unwrap().items[0]
        .entry_id
        .clone()
        .unwrap();
    // The org's owner cannot give its own usage charges back.
    let owner = user("alice").with_org(&org);
    assert!(env.credits.refund(&owner, AmendInput::new(&charge, "r")).await.is_err());
    let r = env.credits.refund(&admin(), AmendInput::new(&charge, "r").amount(5)).await.unwrap();
    assert_eq!((r.entry.amount, r.entry.feature.as_deref()), (5, Some("ai_tokens")));
    assert_eq!(env.balance(&s).await, 35);
    env.assert_consistent(&s).await;
}

#[tokio::test]
async fn statistics_show_balance_history_consumption_and_totals() {
    let env = Env::new().await;
    env.metered(&["ai_tokens", "images"]).await;
    priced(&env, "ai_tokens", 1, 10, RateBasis::Units).await;
    priced(&env, "images", 5, 1, RateBasis::Units).await;
    let s = pk("alice");
    let day0 = T0 - 12 * HOUR; // 2026-11-09 00:00 UTC
    env.top_up(&s, 100, "t").await; // day 0
    env.use_units(&s, "ai_tokens", 100, "u1").await; // 10 credits
    env.credits.consume_usage().await.unwrap();
    env.clock.advance(DAY); // day 1
    env.use_units(&s, "images", 2, "u2").await; // 10 credits
    env.use_units(&s, "ai_tokens", 50, "u3").await; // 5 credits
    env.credits.consume_usage().await.unwrap();
    let spend = env
        .credits
        .spend(&sys(), SpendInput::new(&s, 7, "s").for_feature("images", 1))
        .await
        .unwrap();
    env.credits.refund(&sys(), AmendInput::new(&spend.entry.id, "r").amount(2)).await.unwrap();
    env.clock.advance(2 * DAY); // day 3
    env.grant_until(&s, 20, "g", T0 + 4 * DAY).await;

    let w = Window {
        from: day0,
        to: day0 + 4 * DAY,
        interval: Interval::Day,
    };
    let h = env.credits.balance_history(&user("alice"), None, w).await.unwrap();
    assert_eq!(h.opening, 0);
    let pts: Vec<_> = h.points.iter().map(|p| (p.start, p.credited, p.debited, p.closing)).collect();
    assert_eq!(
        pts,
        vec![
            (day0, 100, 10, 90),
            (day0 + DAY, 2, 22, 70),
            (day0 + 2 * DAY, 0, 0, 70),
            (day0 + 3 * DAY, 20, 0, 90),
        ]
    );
    let later = env
        .credits
        .balance_history(
            &sys(),
            Some(&s),
            Window {
                from: day0 + DAY,
                to: day0 + 2 * DAY,
                interval: Interval::Hour,
            },
        )
        .await
        .unwrap();
    assert_eq!((later.opening, later.points.len()), (90, 24));
    assert_eq!(later.points.last().unwrap().closing, 70);

    let c = env.credits.consumption(&user("alice"), None, false, w).await.unwrap();
    let rows: Vec<_> = c
        .rows
        .iter()
        .map(|r| (r.start, r.feature.as_str(), r.debited, r.refunded, r.net, r.units, r.entries))
        .collect();
    assert_eq!(
        rows,
        vec![
            (day0, "ai_tokens", 10, 0, 10, 100, 1),
            (day0 + DAY, "ai_tokens", 5, 0, 5, 50, 1),
            (day0 + DAY, "images", 17, 2, 15, 3, 2),
        ]
    );
    assert_eq!(c.total_net, 30);
    let month = env
        .credits
        .consumption(
            &sys(),
            None,
            true,
            Window {
                interval: Interval::Month,
                ..w
            },
        )
        .await
        .unwrap();
    assert_eq!(month.rows.len(), 2);
    assert_eq!(month.rows.iter().map(|r| r.net).sum::<i64>(), 30);
    assert!(env.credits.consumption(&user("alice"), None, true, w).await.is_err());

    let sum = env.credits.summary(&sys(), Some(&s), false, day0, day0 + 4 * DAY).await.unwrap();
    assert_eq!(
        (sum.top_up, sum.grant, sum.consumption, sum.spend, sum.refund, sum.net, sum.balance),
        (100, 20, 25, 7, 2, 90, 90)
    );
    assert!(matches!(
        env.credits
            .balance_history(
                &sys(),
                Some(&s),
                Window {
                    from: day0,
                    to: day0 + 100 * DAY,
                    interval: Interval::Hour
                }
            )
            .await,
        Err(CreditError::Invalid { .. })
    ));
}
