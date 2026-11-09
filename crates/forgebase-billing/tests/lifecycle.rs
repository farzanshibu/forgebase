//! Subscriptions end to end against real persistence: lifecycle, trials,
//! proration (upgrade and downgrade mid-cycle), metered invoices from FB-24
//! usage events, tiered pricing, coupons, tax, multi-currency, the
//! entitlement sync.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use forgebase_billing::{
    BillingError, CancelInput, ChangePlanInput, CouponDuration, CouponInput, CouponKind, CustomerInput, InvoiceStatus,
    ItemInput, LineKind, MeterBasis, PriceType, Proration, Scheme, SubscribeInput, SubscriptionStatus, TaxBehavior,
    TaxRateInput, Tier,
};
use forgebase_entitlements::SubscriptionStatus as EntStatus;

async fn env_with_card(who: &str) -> Env {
    let env = Env::new().await;
    env.catalog().await;
    env.stripe().await;
    env.customer(who, "USD").await;
    env.save_card(who).await;
    env
}

#[tokio::test]
async fn full_lifecycle_subscribe_renew_cancel_at_period_end() {
    let env = env_with_card("ann").await;
    let c = env.subscribe("ann", &["pro-m"], "sub-1").await;
    let sub = c.subscription.unwrap();
    let inv = c.invoice.unwrap();
    assert_eq!(sub.status, SubscriptionStatus::Active);
    assert_eq!(inv.status, InvoiceStatus::Paid);
    assert_eq!(inv.number.as_deref(), Some("INV-000001"));
    assert_eq!((inv.total, inv.amount_paid), (3000, 3000));
    assert_eq!(sub.current_period_end, T0 + 30 * DAY, "November has 30 days");
    let charges = env.http.calls("/v1/payment_intents");
    assert_eq!(charges.len(), 1);
    assert_eq!(MockHttp::field(&charges[0], "amount").as_deref(), Some("3000"));
    assert_eq!(
        MockHttp::field(&charges[0], "payment_method").as_deref(),
        Some("pm_card_ann")
    );
    assert_eq!(
        env.plan_of(&pk("ann")),
        (Some("pro".into()), Some(EntStatus::Active), true)
    );

    // Idempotent subscribe; one plan per subject.
    let again = env.subscribe("ann", &["pro-m"], "sub-1").await;
    assert!(again.replayed);
    assert_eq!(again.subscription.unwrap().id, sub.id);
    assert_eq!(env.http.calls("/v1/payment_intents").len(), 1);
    let dup = env
        .billing
        .subscribe(
            &user("ann"),
            SubscribeInput {
                items: vec![ItemInput::new("basic-m")],
                idempotency_key: "sub-2".into(),
                ..SubscribeInput::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            dup,
            BillingError::Conflict {
                code: "already_subscribed",
                ..
            }
        ),
        "{dup:?}"
    );

    // Renewal on 1 December.
    let rep = env.advance(30 * DAY).await;
    assert_eq!(rep.renewed, 1);
    let s = env.billing.subscription(&user("ann"), &sub.id).await.unwrap();
    assert_eq!(s.current_period_start, T0 + 30 * DAY);
    assert_eq!(s.current_period_end, T0 + 61 * DAY, "December has 31 days");
    let invs = env.billing.invoices(&user("ann"), None, None, None).await.unwrap();
    assert_eq!(invs.len(), 2);
    assert_eq!(invs[0].number.as_deref(), Some("INV-000002"));
    assert_eq!(invs[0].status, InvoiceStatus::Paid);
    assert_eq!(invs[0].billing_reason, "subscription_cycle");

    // Cancel at period end: entitled until then.
    let c = env
        .billing
        .cancel(
            &user("ann"),
            &sub.id,
            CancelInput {
                at_period_end: true,
                reason: None,
            },
        )
        .await
        .unwrap();
    let s = c.subscription.unwrap();
    assert_eq!(s.status, SubscriptionStatus::Active);
    assert!(s.cancel_at_period_end);
    assert_eq!(env.plan_of(&pk("ann")).0.as_deref(), Some("pro"));
    let rep = env.advance(31 * DAY).await;
    assert_eq!((rep.ended, rep.renewed), (1, 0));
    let s = env.billing.subscription(&user("ann"), &sub.id).await.unwrap();
    assert_eq!(s.status, SubscriptionStatus::Canceled);
    assert_eq!(s.ended_at, Some(T0 + 61 * DAY));
    let (plan, status, _) = env.plan_of(&pk("ann"));
    assert_eq!(status, Some(EntStatus::Canceled));
    assert_ne!(plan.as_deref(), Some("pro"));
    assert_eq!(
        env.http.calls("/v1/payment_intents").len(),
        2,
        "no charge after cancellation"
    );
    // Every row of the audit trail stays.
    assert!(env.count("SELECT COUNT(*) FROM _forgebase_billing_audit").await >= 5);
}

#[tokio::test]
async fn cancel_now_and_pause_resume() {
    let env = env_with_card("ann").await;
    let sub = env.subscribe("ann", &["basic-m"], "k").await.subscription.unwrap();
    let p = env.billing.pause(&user("ann"), &sub.id).await.unwrap();
    assert_eq!(p.status, SubscriptionStatus::Paused);
    assert_eq!(env.plan_of(&pk("ann")).1, Some(EntStatus::Paused));
    // No renewal while paused.
    let rep = env.advance(40 * DAY).await;
    assert_eq!(rep.renewed, 0);
    let r = env.billing.resume(&user("ann"), &sub.id).await.unwrap();
    let s = r.subscription.unwrap();
    assert_eq!(s.status, SubscriptionStatus::Active);
    assert_eq!(s.current_period_start, T0 + 40 * DAY);
    assert_eq!(r.invoice.unwrap().status, InvoiceStatus::Paid);
    // Another user cannot cancel it.
    let e = env
        .billing
        .cancel(&user("bob"), &sub.id, CancelInput::default())
        .await
        .unwrap_err();
    assert!(matches!(e, BillingError::NotFound(_)), "{e:?}");
    let c = env
        .billing
        .cancel(
            &user("ann"),
            &sub.id,
            CancelInput {
                at_period_end: false,
                reason: Some("too expensive".into()),
            },
        )
        .await
        .unwrap();
    let s = c.subscription.unwrap();
    assert_eq!(s.status, SubscriptionStatus::Canceled);
    assert_eq!(s.cancel_reason.as_deref(), Some("too expensive"));
    assert_eq!(env.plan_of(&pk("ann")).1, Some(EntStatus::Canceled));
}

#[tokio::test]
async fn trial_then_conversion() {
    let env = env_with_card("ann").await;
    let mut p = recurring("pro", "USD", 3000);
    p.trial_days = 14;
    env.billing.put_price(&admin(), "pro-trial", p).await.unwrap();
    let c = env.subscribe("ann", &["pro-trial"], "t").await;
    let sub = c.subscription.unwrap();
    assert_eq!(sub.status, SubscriptionStatus::Trialing);
    assert_eq!(sub.trial_ends_at, Some(T0 + 14 * DAY));
    assert!(c.invoice.is_none());
    assert!(env.http.calls("/v1/payment_intents").is_empty());
    assert_eq!(
        env.plan_of(&pk("ann")),
        (Some("pro".into()), Some(EntStatus::Trialing), true)
    );
    let rep = env.advance(14 * DAY).await;
    assert_eq!(rep.trials_ended, 1);
    let s = env.billing.subscription(&user("ann"), &sub.id).await.unwrap();
    assert_eq!(s.status, SubscriptionStatus::Active);
    assert_eq!(
        (s.current_period_start, s.current_period_end),
        (T0 + 14 * DAY, T0 + 44 * DAY)
    );
    let inv = env.billing.invoices(&user("ann"), None, None, None).await.unwrap();
    assert_eq!((inv.len(), inv[0].total, inv[0].status), (1, 3000, InvoiceStatus::Paid));
    assert_eq!(env.plan_of(&pk("ann")).1, Some(EntStatus::Active));
}

#[tokio::test]
async fn proration_upgrade_mid_cycle_charges_the_difference_now() {
    let env = env_with_card("ann").await;
    let sub = env.subscribe("ann", &["basic-m"], "k").await.subscription.unwrap();
    env.clock.advance(15 * DAY); // half of the 30-day period
    let c = env
        .billing
        .change_plan(
            &user("ann"),
            &sub.id,
            ChangePlanInput {
                price: "pro-m".into(),
                quantity: None,
                proration: None,
                at_period_end: false,
            },
        )
        .await
        .unwrap();
    let inv = c.invoice.unwrap();
    assert_eq!(inv.billing_reason, "subscription_update");
    let amounts: Vec<(LineKind, i64)> = inv.lines.iter().map(|l| (l.kind, l.amount)).collect();
    assert_eq!(amounts, vec![(LineKind::Proration, -500), (LineKind::Proration, 1500)]);
    assert_eq!((inv.total, inv.status), (1000, InvoiceStatus::Paid));
    let charges = env.http.calls("/v1/payment_intents");
    assert_eq!(
        MockHttp::field(charges.last().unwrap(), "amount").as_deref(),
        Some("1000")
    );
    assert_eq!(c.subscription.unwrap().items[0].price, "pro-m");
    assert_eq!(env.plan_of(&pk("ann")).0.as_deref(), Some("pro"));
    // The renewal charges the new price in full.
    env.advance(15 * DAY).await;
    let invs = env.billing.invoices(&user("ann"), None, None, None).await.unwrap();
    assert_eq!(invs[0].total, 3000);
}

#[tokio::test]
async fn proration_downgrade_mid_cycle_credits_the_balance() {
    let env = env_with_card("ann").await;
    let sub = env.subscribe("ann", &["pro-m"], "k").await.subscription.unwrap();
    env.clock.advance(15 * DAY);
    let c = env
        .billing
        .change_plan(
            &user("ann"),
            &sub.id,
            ChangePlanInput {
                price: "basic-m".into(),
                quantity: None,
                proration: Some(Proration::AlwaysInvoice),
                at_period_end: false,
            },
        )
        .await
        .unwrap();
    let inv = c.invoice.unwrap();
    assert_eq!(inv.lines.iter().map(|l| l.amount).collect::<Vec<_>>(), vec![-1500, 500]);
    assert_eq!((inv.total, inv.amount_due, inv.status), (-1000, 0, InvoiceStatus::Paid));
    let cust = env.billing.customer(&user("ann"), None).await.unwrap();
    assert_eq!(cust.balance, 1000, "the net credit is owed to the customer");
    assert_eq!(env.http.calls("/v1/payment_intents").len(), 1, "nothing charged");
    assert_eq!(env.plan_of(&pk("ann")).0.as_deref(), Some("basic"));
    // The balance pays the next invoice.
    env.advance(15 * DAY).await;
    let next = &env.billing.invoices(&user("ann"), None, None, None).await.unwrap()[0];
    assert_eq!((next.total, next.balance_applied, next.amount_due), (1000, 1000, 0));
    assert_eq!(next.status, InvoiceStatus::Paid);
    assert_eq!(env.billing.customer(&user("ann"), None).await.unwrap().balance, 0);
    assert_eq!(env.http.calls("/v1/payment_intents").len(), 1);
}

#[tokio::test]
async fn downgrade_at_period_end_and_prorations_on_next_invoice() {
    let env = env_with_card("ann").await;
    let sub = env.subscribe("ann", &["pro-m"], "k").await.subscription.unwrap();
    let c = env
        .billing
        .change_plan(
            &user("ann"),
            &sub.id,
            ChangePlanInput {
                price: "basic-m".into(),
                quantity: None,
                proration: None,
                at_period_end: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(c.subscription.unwrap().pending_price.as_deref(), Some("basic-m"));
    assert_eq!(env.plan_of(&pk("ann")).0.as_deref(), Some("pro"));
    env.advance(30 * DAY).await;
    let s = env.billing.subscription(&user("ann"), &sub.id).await.unwrap();
    assert_eq!(
        (s.items[0].price.as_str(), s.pending_price.as_deref()),
        ("basic-m", None)
    );
    assert_eq!(
        env.billing.invoices(&user("ann"), None, None, None).await.unwrap()[0].total,
        1000
    );
    assert_eq!(env.plan_of(&pk("ann")).0.as_deref(), Some("basic"));

    // create_prorations: the lines wait for the renewal.
    env.clock.advance(31 * DAY / 2);
    let c = env
        .billing
        .change_plan(
            &user("ann"),
            &sub.id,
            ChangePlanInput {
                price: "pro-m".into(),
                quantity: None,
                proration: Some(Proration::CreateProrations),
                at_period_end: false,
            },
        )
        .await
        .unwrap();
    assert!(
        c.invoice
            .as_ref()
            .is_none_or(|i| i.billing_reason != "subscription_update")
    );
    let rep = env.advance(31 * DAY / 2 + 1).await;
    assert_eq!(rep.renewed, 1);
    let inv = &env.billing.invoices(&user("ann"), None, None, None).await.unwrap()[0];
    let kinds: Vec<LineKind> = inv.lines.iter().map(|l| l.kind).collect();
    assert_eq!(
        kinds,
        vec![LineKind::Proration, LineKind::Proration, LineKind::Subscription]
    );
    // -500 (half of basic) + 1500 (half of pro) + 3000 for January.
    assert_eq!(inv.total, 4000);
}

async fn metered_env(scheme: Scheme, tiers: Option<Vec<Tier>>, basis: MeterBasis) -> Env {
    let env = env_with_card("ann").await;
    let mut p = recurring("api", "USD", 2);
    p.price_type = PriceType::Metered;
    p.feature = Some("api_calls".into());
    p.scheme = Some(scheme);
    if tiers.is_some() {
        p.unit_amount = 0;
    }
    p.tiers = tiers;
    p.meter_basis = Some(basis);
    env.billing.put_price(&admin(), "api-m", p).await.unwrap();
    env
}

#[tokio::test]
async fn metered_invoice_from_usage_events() {
    let env = metered_env(Scheme::PerUnit, None, MeterBasis::Units).await;
    let sub = env
        .subscribe("ann", &["basic-m", "api-m"], "k")
        .await
        .subscription
        .unwrap();
    let first = &env.billing.invoices(&user("ann"), None, None, None).await.unwrap()[0];
    assert_eq!(first.total, 1000, "metered prices are billed in arrears");
    env.clock.advance(HOUR);
    env.use_units(&pk("ann"), "api_calls", 300, "u1").await;
    env.use_units(&pk("ann"), "api_calls", 200, "u2").await;
    let rep = env.advance(MIN).await;
    assert_eq!(rep.usage_recorded, 2);
    // Re-reading the feed from the start stores nothing twice.
    env.exec("UPDATE _forgebase_billing_cursors SET seq = 0").await;
    assert_eq!(env.advance(MIN).await.usage_recorded, 0);
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_billing_usage_records").await,
        2
    );
    env.clock.set(T0 + 30 * DAY);
    env.billing.run_maintenance().await.unwrap();
    let inv = &env.billing.invoices(&user("ann"), None, None, None).await.unwrap()[0];
    let usage: Vec<_> = inv.lines.iter().filter(|l| l.kind == LineKind::Usage).collect();
    assert_eq!(usage.len(), 1);
    assert_eq!((usage[0].quantity, usage[0].amount), (500, 1000));
    assert_eq!(
        (usage[0].period_start, usage[0].period_end),
        (Some(T0), Some(T0 + 30 * DAY))
    );
    assert_eq!(inv.total, 1000 + 1000);
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_billing_usage_records WHERE invoice_id IS NULL")
            .await,
        0
    );
    // The next period has no usage yet.
    env.advance(31 * DAY).await;
    let inv = &env.billing.invoices(&user("ann"), None, None, None).await.unwrap()[0];
    assert!(inv.lines.iter().all(|l| l.kind != LineKind::Usage));
    let _ = sub;
}

#[tokio::test]
async fn tiered_pricing_graduated_and_volume() {
    let tiers = vec![
        Tier {
            up_to: Some(1000),
            unit_amount: 0,
            flat_amount: 0,
        },
        Tier {
            up_to: Some(5000),
            unit_amount: 2,
            flat_amount: 0,
        },
        Tier {
            up_to: None,
            unit_amount: 1,
            flat_amount: 500,
        },
    ];
    for (scheme, expected) in [(Scheme::Graduated, 8000 + 1000 + 500), (Scheme::Volume, 6000 + 500)] {
        let env = metered_env(scheme, Some(tiers.clone()), MeterBasis::Units).await;
        env.subscribe("ann", &["basic-m", "api-m"], "k").await;
        env.use_units(&pk("ann"), "api_calls", 6000, "u").await;
        env.advance(30 * DAY).await;
        let inv = &env.billing.invoices(&user("ann"), None, None, None).await.unwrap()[0];
        let line = inv.lines.iter().find(|l| l.kind == LineKind::Usage).unwrap();
        assert_eq!((line.quantity, line.amount), (6000, expected), "{scheme:?}");
    }
    // Invalid tiers are refused.
    let env = Env::new().await;
    env.catalog().await;
    let mut p = recurring("api", "USD", 0);
    p.price_type = PriceType::Metered;
    p.feature = Some("api_calls".into());
    p.scheme = Some(Scheme::Graduated);
    p.tiers = Some(vec![Tier {
        up_to: Some(10),
        unit_amount: 1,
        flat_amount: 0,
    }]);
    let e = env.billing.put_price(&admin(), "bad", p).await.unwrap_err();
    assert!(
        matches!(
            e,
            BillingError::Invalid {
                code: "invalid_tiers",
                ..
            }
        ),
        "{e:?}"
    );
}

#[tokio::test]
async fn coupons_percent_repeating_and_amount_once() {
    let env = env_with_card("ann").await;
    env.billing
        .put_coupon(
            &admin(),
            "save20",
            CouponInput {
                name: None,
                kind: CouponKind::Percent,
                percent_bp: Some(2000),
                amount_off: None,
                currency: None,
                duration: Some(CouponDuration::Repeating),
                duration_cycles: Some(2),
                max_redemptions: Some(1),
                expires_at: None,
                products: vec![],
                active: true,
            },
        )
        .await
        .unwrap();
    let c = env
        .billing
        .subscribe(
            &user("ann"),
            SubscribeInput {
                items: vec![ItemInput::new("pro-m")],
                coupon: Some("SAVE20".into()),
                idempotency_key: "k".into(),
                ..SubscribeInput::default()
            },
        )
        .await
        .unwrap();
    let inv = c.invoice.unwrap();
    assert_eq!((inv.subtotal, inv.discount, inv.total), (3000, 600, 2400));
    assert_eq!(inv.coupon.as_deref(), Some("SAVE20"));
    env.advance(30 * DAY).await;
    env.advance(31 * DAY).await;
    let invs = env.billing.invoices(&user("ann"), None, None, None).await.unwrap();
    let totals: Vec<i64> = invs.iter().rev().map(|i| i.total).collect();
    assert_eq!(
        totals,
        vec![2400, 2400, 3000],
        "two discounted invoices, then the full price"
    );
    // The coupon was redeemed once and is capped at one redemption.
    env.customer("bob", "USD").await;
    let e = env
        .billing
        .subscribe(
            &user("bob"),
            SubscribeInput {
                items: vec![ItemInput::new("pro-m")],
                coupon: Some("save20".into()),
                idempotency_key: "k".into(),
                ..SubscribeInput::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            BillingError::Invalid {
                code: "coupon_exhausted",
                ..
            }
        ),
        "{e:?}"
    );

    // An amount coupon on a one-time order.
    env.billing
        .put_coupon(
            &admin(),
            "tenoff",
            CouponInput {
                name: None,
                kind: CouponKind::Amount,
                percent_bp: None,
                amount_off: Some(1000),
                currency: Some("USD".into()),
                duration: None,
                duration_cycles: None,
                max_redemptions: None,
                expires_at: None,
                products: vec!["credits".into()],
                active: true,
            },
        )
        .await
        .unwrap();
    let o = env
        .billing
        .create_order(
            &user("ann"),
            forgebase_billing::OrderInput {
                items: vec![ItemInput {
                    price: "credits-100".into(),
                    quantity: 2,
                }],
                coupon: Some("TENOFF".into()),
                idempotency_key: "o1".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let inv = o.invoice.unwrap();
    assert_eq!((inv.subtotal, inv.discount, inv.total), (1800, 1000, 800));
}

#[tokio::test]
async fn tax_exclusive_and_inclusive_by_region() {
    let env = Env::new().await;
    env.catalog().await;
    env.stripe().await;
    env.billing
        .put_tax_rate(
            &admin(),
            "IN",
            TaxRateInput {
                name: "GST".into(),
                rate_bp: 1800,
                active: true,
            },
        )
        .await
        .unwrap();
    env.billing
        .put_tax_rate(
            &admin(),
            "DE",
            TaxRateInput {
                name: "MwSt".into(),
                rate_bp: 1900,
                active: true,
            },
        )
        .await
        .unwrap();
    let mut incl = recurring("pro", "EUR", 11_900);
    incl.tax_behavior = Some(TaxBehavior::Inclusive);
    env.billing.put_price(&admin(), "pro-de", incl).await.unwrap();
    for (who, cur, region) in [("ravi", "INR", "IN-KA"), ("lena", "EUR", "DE")] {
        env.billing
            .put_customer(
                &user(who),
                CustomerInput {
                    currency: Some(cur.into()),
                    region: Some(region.into()),
                    ..CustomerInput::default()
                },
            )
            .await
            .unwrap();
        env.save_card(who).await;
    }
    // Exclusive: 18 % on top (the country rate applies to IN-KA).
    let inv = env.subscribe("ravi", &["pro-m"], "k").await.invoice.unwrap();
    assert_eq!(
        (inv.currency.as_str(), inv.subtotal, inv.tax, inv.total),
        ("INR", 250_000, 45_000, 295_000)
    );
    assert_eq!(inv.tax_region.as_deref(), Some("IN"));
    assert_eq!(inv.lines[0].tax_rate_bp, 1800);
    // Inclusive: 19 % inside the price.
    let inv = env.subscribe("lena", &["pro-de"], "k").await.invoice.unwrap();
    assert_eq!((inv.subtotal, inv.tax, inv.total), (11_900, 1_900, 11_900));
    assert!(inv.lines[0].tax_inclusive);
}

#[tokio::test]
async fn multi_currency_price_sets() {
    let env = Env::new().await;
    env.catalog().await;
    env.stripe().await;
    env.customer("eva", "EUR").await;
    env.save_card("eva").await;
    let inv = env.subscribe("eva", &["pro-m"], "k").await.invoice.unwrap();
    assert_eq!((inv.currency.as_str(), inv.total), ("EUR", 2800));
    let charge = env.http.calls("/v1/payment_intents").pop().unwrap();
    assert_eq!(MockHttp::field(&charge, "currency").as_deref(), Some("eur"));
    env.customer("ken", "JPY").await;
    let e = env
        .billing
        .subscribe(
            &user("ken"),
            SubscribeInput {
                items: vec![ItemInput::new("pro-m")],
                idempotency_key: "k".into(),
                ..SubscribeInput::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            BillingError::Invalid {
                code: "currency_not_supported",
                ..
            }
        ),
        "{e:?}"
    );
    // The currency is fixed once invoiced.
    let e = env
        .billing
        .put_customer(
            &user("eva"),
            CustomerInput {
                currency: Some("USD".into()),
                ..CustomerInput::default()
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            BillingError::Conflict {
                code: "currency_locked",
                ..
            }
        ),
        "{e:?}"
    );
}

#[tokio::test]
async fn statistics_mrr_arr_churn_revenue() {
    let env = env_with_card("ann").await;
    env.customer("bob", "USD").await;
    env.save_card("bob").await;
    env.subscribe("ann", &["pro-m"], "k").await;
    let b = env
        .subscribe("bob", &["basic-m", "seat-m"], "k")
        .await
        .subscription
        .unwrap();
    let s = env
        .billing
        .stats(&admin(), T0 - DAY, T0 + DAY, forgebase_billing::Bucket::Day)
        .await
        .unwrap();
    let usd = s.recurring.iter().find(|r| r.currency == "USD").unwrap();
    assert_eq!((usd.mrr, usd.arr, usd.subscriptions), (3000 + 1500, 54_000, 2));
    assert_eq!(s.new_subscriptions, 2);
    let rev: i64 = s.revenue.iter().map(|r| r.net).sum();
    assert_eq!(rev, 4500);
    env.clock.advance(2 * HOUR);
    env.billing
        .cancel(
            &user("bob"),
            &b.id,
            CancelInput {
                at_period_end: false,
                reason: None,
            },
        )
        .await
        .unwrap();
    let s = env
        .billing
        .stats(&admin(), T0 + HOUR, T0 + DAY, forgebase_billing::Bucket::Month)
        .await
        .unwrap();
    assert_eq!((s.active_at_start, s.churned, s.churn_rate_bp), (2, 1, Some(5000)));
    assert_eq!(s.recurring[0].mrr, 3000);
    // Members cannot read instance-wide statistics.
    assert!(
        env.billing
            .stats(&user("ann"), T0, T0 + DAY, forgebase_billing::Bucket::Day)
            .await
            .is_err()
    );
}
