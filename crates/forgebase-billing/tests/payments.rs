//! Money movement against real persistence: credit notes and refunds,
//! dunning to past_due then cancel, payment-to-entitlement mapping (plans,
//! add-ons, credits, usage packs), credit top-up purchases, manual billing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use forgebase_billing::{
    BillingError, CreditNoteInput, InvoiceStatus, ItemInput, MarkPaidInput, OrderInput, OrderStatus, PaymentStatus,
    RefundStatus, SubscribeInput, SubscriptionStatus,
};
use forgebase_entitlements::SubscriptionStatus as EntStatus;
use serde_json::json;

async fn env_with_card(who: &str) -> Env {
    let env = Env::new().await;
    env.catalog().await;
    env.stripe().await;
    env.customer(who, "USD").await;
    env.save_card(who).await;
    env
}

fn note(amount: Option<i64>, key: &str) -> CreditNoteInput {
    CreditNoteInput {
        amount,
        to_balance: false,
        reason: Some("requested_by_customer".into()),
        idempotency_key: key.into(),
    }
}

#[tokio::test]
async fn refund_and_credit_note() {
    let env = env_with_card("ann").await;
    let inv = env.subscribe("ann", &["pro-m"], "k").await.invoice.unwrap();
    // Customers and org billing managers cannot refund themselves.
    let e = env.billing.credit_invoice(&user("ann"), &inv.id, note(None, "cn1")).await.unwrap_err();
    assert!(matches!(e, BillingError::Policy(_) | BillingError::Forbidden(_)), "{e:?}");

    let cn = env.billing.credit_invoice(&admin(), &inv.id, note(Some(1000), "cn1")).await.unwrap();
    assert_eq!(cn.number, "CN-000001");
    assert_eq!((cn.amount, cn.refund_amount, cn.balance_amount), (1000, 1000, 0));
    let r = cn.refund.clone().unwrap();
    assert_eq!((r.amount, r.status), (1000, RefundStatus::Succeeded));
    let calls = env.http.calls("/v1/refunds");
    assert_eq!(calls.len(), 1);
    assert_eq!(MockHttp::field(&calls[0], "amount").as_deref(), Some("1000"));
    // Idempotent.
    let again = env.billing.credit_invoice(&admin(), &inv.id, note(Some(1000), "cn1")).await.unwrap();
    assert_eq!(again.id, cn.id);
    assert_eq!(env.http.calls("/v1/refunds").len(), 1);
    // The rest, then nothing is left.
    let rest = env.billing.credit_invoice(&admin(), &inv.id, note(None, "cn2")).await.unwrap();
    assert_eq!((rest.number.as_str(), rest.amount), ("CN-000002", 2000));
    let e = env.billing.credit_invoice(&admin(), &inv.id, note(Some(1), "cn3")).await.unwrap_err();
    assert!(matches!(e, BillingError::Invalid { code: "invalid_amount", .. }), "{e:?}");
    let inv = env.billing.invoice(&admin(), &inv.id).await.unwrap();
    assert_eq!(inv.amount_refunded, 3000);
    let pays = env.billing.payments(&user("ann"), None, None).await.unwrap();
    assert_eq!(pays[0].amount_refunded, 3000);
    // The refunds appear in the statistics.
    let s = env
        .billing
        .stats(&admin(), T0 - DAY, T0 + DAY, forgebase_billing::Bucket::Day)
        .await
        .unwrap();
    assert_eq!(s.revenue.iter().map(|r| r.net).sum::<i64>(), 0);
}

#[tokio::test]
async fn credit_note_to_balance_and_refused_refund_settles_to_balance() {
    let env = env_with_card("ann").await;
    let inv = env.subscribe("ann", &["pro-m"], "k").await.invoice.unwrap();
    let mut n = note(Some(500), "b1");
    n.to_balance = true;
    let cn = env.billing.credit_invoice(&admin(), &inv.id, n).await.unwrap();
    assert_eq!((cn.refund_amount, cn.balance_amount), (0, 500));
    assert!(cn.refund.is_none());
    assert_eq!(env.billing.customer(&user("ann"), None).await.unwrap().balance, 500);
    // The provider refuses a refund: the credit note settles to the balance.
    *env.http.refund_status.lock().unwrap() = "failed";
    let cn = env.billing.credit_invoice(&admin(), &inv.id, note(Some(700), "b2")).await.unwrap();
    assert_eq!(cn.refund.unwrap().status, RefundStatus::Failed);
    assert_eq!((cn.refund_amount, cn.balance_amount), (0, 700));
    assert_eq!(env.billing.customer(&user("ann"), None).await.unwrap().balance, 1200);
    let pays = env.billing.payments(&user("ann"), None, None).await.unwrap();
    assert_eq!(pays[0].amount_refunded, 0);
}

#[tokio::test]
async fn dunning_to_past_due_then_cancel() {
    let env = env_with_card("ann").await;
    let sub = env.subscribe("ann", &["pro-m"], "k").await.subscription.unwrap();
    env.http.set_mode(ChargeMode::Decline);
    let rep = env.advance(30 * DAY).await;
    assert_eq!(rep.renewed, 1);
    let s = env.billing.subscription(&user("ann"), &sub.id).await.unwrap();
    assert_eq!(s.status, SubscriptionStatus::PastDue);
    // Still entitled during dunning.
    assert_eq!(env.plan_of(&pk("ann")), (Some("pro".into()), Some(EntStatus::PastDue), true));
    let inv = &env.billing.invoices(&user("ann"), Some(&pk("ann")), Some(InvoiceStatus::Open), None).await.unwrap()[0];
    assert_eq!(inv.attempt_count, 1);
    assert_eq!(inv.next_attempt_at, Some(inv.finalized_at.unwrap() + DAY));
    let p = &env.billing.payments(&user("ann"), None, None).await.unwrap()[0];
    assert_eq!((p.status, p.failure_code.as_deref()), (PaymentStatus::Failed, Some("insufficient_funds")));

    // Retries on days 1, 3 and 5; the last failure cancels.
    for (days, attempt) in [(1, 2), (2, 3)] {
        let rep = env.advance(days * DAY).await;
        assert_eq!(rep.retries, 1);
        let i = env.billing.invoice(&user("ann"), &inv.id).await.unwrap();
        assert_eq!((i.attempt_count, i.status), (attempt, InvoiceStatus::Open));
        assert_eq!(
            env.billing.subscription(&user("ann"), &sub.id).await.unwrap().status,
            SubscriptionStatus::PastDue
        );
    }
    let rep = env.advance(2 * DAY).await;
    assert_eq!(rep.retries, 1);
    let i = env.billing.invoice(&user("ann"), &inv.id).await.unwrap();
    assert_eq!((i.attempt_count, i.status), (4, InvoiceStatus::Uncollectible));
    let s = env.billing.subscription(&user("ann"), &sub.id).await.unwrap();
    assert_eq!((s.status, s.cancel_reason.as_deref()), (SubscriptionStatus::Canceled, Some("payment_failed")));
    assert_eq!(env.plan_of(&pk("ann")).1, Some(EntStatus::Canceled));
    assert_eq!(env.count("SELECT COUNT(*) FROM _forgebase_billing_payments WHERE status = 'failed'").await, 4);
    // Nothing is retried any more.
    assert_eq!(env.advance(10 * DAY).await.retries, 0);
}

#[tokio::test]
async fn dunning_recovers_when_a_retry_succeeds() {
    let env = env_with_card("ann").await;
    let sub = env.subscribe("ann", &["pro-m"], "k").await.subscription.unwrap();
    env.http.set_mode(ChargeMode::Decline);
    env.advance(30 * DAY).await;
    env.http.set_mode(ChargeMode::Succeed);
    let rep = env.advance(DAY).await;
    assert_eq!(rep.retries, 1);
    let s = env.billing.subscription(&user("ann"), &sub.id).await.unwrap();
    assert_eq!(s.status, SubscriptionStatus::Active);
    assert_eq!(env.plan_of(&pk("ann")).1, Some(EntStatus::Active));
    let i = &env.billing.invoices(&user("ann"), None, None, None).await.unwrap()[0];
    assert_eq!((i.status, i.next_attempt_at), (InvoiceStatus::Paid, None));
}

#[tokio::test]
async fn a_charge_with_unknown_outcome_is_retried_with_the_same_key() {
    let env = env_with_card("ann").await;
    env.subscribe("ann", &["pro-m"], "k").await;
    env.http.set_mode(ChargeMode::Unreachable);
    env.advance(30 * DAY).await;
    let p = &env.billing.payments(&user("ann"), None, None).await.unwrap()[0];
    assert_eq!(p.status, PaymentStatus::Pending);
    env.http.set_mode(ChargeMode::Succeed);
    env.advance(DAY).await;
    let charges = env.http.calls("/v1/payment_intents");
    let keys: Vec<String> = charges
        .iter()
        .skip(1)
        .map(|r| r.headers.iter().find(|(k, _)| k == "idempotency-key").unwrap().1.clone())
        .collect();
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0], keys[1], "the retry reuses the attempt's idempotency key");
    assert_eq!(env.billing.invoices(&user("ann"), None, None, None).await.unwrap()[0].status, InvoiceStatus::Paid);
    assert_eq!(env.count("SELECT COUNT(*) FROM _forgebase_billing_payments").await, 2);
}

#[tokio::test]
async fn payment_to_entitlement_mapping() {
    let env = Env::new().await;
    env.catalog().await;
    env.stripe().await;
    env.customer("ann", "USD").await;
    // No card: the first invoice is paid on the hosted page.
    let c = env
        .billing
        .subscribe(
            &user("ann"),
            SubscribeInput {
                items: vec![ItemInput::new("pro-m"), ItemInput { price: "seat-m".into(), quantity: 2 }],
                idempotency_key: "k".into(),
                return_url: Some("https://app.example.test/billing".into()),
                ..SubscribeInput::default()
            },
        )
        .await
        .unwrap();
    let sub = c.subscription.unwrap();
    let inv = c.invoice.unwrap();
    assert_eq!(sub.status, SubscriptionStatus::Incomplete);
    assert_eq!(inv.total, 3000 + 2 * 500);
    let url = c.payment_url.unwrap();
    assert!(url.starts_with("https://checkout.stripe.test/"), "{url}");
    let session = env.http.calls("/v1/checkout/sessions").pop().unwrap();
    assert_eq!(MockHttp::field(&session, "payment_intent_data[metadata][forgebase_invoice]"), Some(inv.id.clone()));
    // Not entitled before payment.
    assert_ne!(env.plan_of(&pk("ann")).0.as_deref(), Some("pro"));

    let ev = stripe_event(
        "evt_paid_1",
        "payment_intent.succeeded",
        env.clock_secs(),
        json!({"id": "pi_hosted_1", "amount": 4000, "amount_received": 4000, "currency": "usd",
               "customer": "cus_x", "payment_method": "pm_saved_1",
               "metadata": {"forgebase_invoice": inv.id}}),
    );
    let out = env.stripe_webhook(&ev, env.clock_secs()).await.unwrap();
    assert_eq!((out.result.as_str(), out.detail.as_deref()), ("processed", Some("invoice_paid")));
    let s = env.billing.subscription(&user("ann"), &sub.id).await.unwrap();
    assert_eq!(s.status, SubscriptionStatus::Active);
    let e = env.ent.entitlements_of(&pk("ann"), None);
    assert_eq!(e.plan.as_deref(), Some("pro"));
    assert_eq!(e.features["projects"].limit, Some(100 + 2 * 10), "plan plus two add-ons");
    // The card from the hosted page is kept for renewals.
    let cust = env.billing.customer(&user("ann"), None).await.unwrap();
    assert!(cust.has_payment_method);
    env.advance(30 * DAY).await;
    assert_eq!(env.http.calls("/v1/payment_intents").len(), 1);
    assert_eq!(env.billing.invoices(&user("ann"), None, None, None).await.unwrap()[0].status, InvoiceStatus::Paid);

    // An unpaid first invoice expires the subscription.
    env.customer("bob", "USD").await;
    let b = env.subscribe("bob", &["basic-m"], "k").await.subscription.unwrap();
    assert_eq!(b.status, SubscriptionStatus::Incomplete);
    let rep = env.advance(DAY + MIN).await;
    assert_eq!(rep.ended, 1);
    let b = env.billing.subscription(&user("bob"), &b.id).await.unwrap();
    assert_eq!((b.status, b.cancel_reason.as_deref()), (SubscriptionStatus::Canceled, Some("incomplete_expired")));
}

#[tokio::test]
async fn credit_top_up_purchase_and_refund() {
    let env = env_with_card("ann").await;
    let order = |key: &str, qty: i64| OrderInput {
        items: vec![ItemInput { price: "credits-100".into(), quantity: qty }],
        idempotency_key: key.into(),
        ..OrderInput::default()
    };
    let c = env.billing.create_order(&user("ann"), order("o1", 3)).await.unwrap();
    let o = c.order.unwrap();
    let inv = c.invoice.unwrap();
    assert_eq!(o.status, OrderStatus::Paid);
    assert_eq!((inv.total, inv.status), (2700, InvoiceStatus::Paid));
    assert!(inv.fulfilled_at.is_some());
    assert_eq!(env.credits.balance(&sys(), Some(&pk("ann"))).await.unwrap().balance, 300);
    // Replays neither charge nor credit again.
    let again = env.billing.create_order(&user("ann"), order("o1", 3)).await.unwrap();
    assert!(again.replayed);
    assert_eq!(env.http.calls("/v1/payment_intents").len(), 1);
    // Re-running fulfillment is idempotent.
    assert!(!env.billing.fulfill(&inv.id).await.unwrap());
    env.billing.run_maintenance().await.unwrap();
    assert_eq!(env.credits.balance(&sys(), Some(&pk("ann"))).await.unwrap().balance, 300);
    let ledger = env.credits.audit(&sys(), &pk("ann")).await.unwrap();
    assert!(ledger.consistent);

    // A full refund takes the credits back.
    let cn = env.billing.credit_invoice(&admin(), &inv.id, note(None, "r1")).await.unwrap();
    assert_eq!(cn.credits_reversed, 300);
    assert_eq!(env.credits.balance(&sys(), Some(&pk("ann"))).await.unwrap().balance, 0);
    assert_eq!(env.billing.order(&user("ann"), &o.id).await.unwrap().status, OrderStatus::Refunded);

    // A usage pack purchase grants FB-24 units.
    let c = env
        .billing
        .create_order(
            &user("ann"),
            OrderInput {
                items: vec![ItemInput::new("pack-1k")],
                idempotency_key: "p1".into(),
                ..OrderInput::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(c.invoice.unwrap().total, 400);
    let packs = env.ent.pack_grants(&sys(), &pk("ann")).await.unwrap();
    assert_eq!((packs.len(), packs[0].remaining), (1, 1000));
}

#[tokio::test]
async fn order_paid_on_hosted_page_is_fulfilled_by_the_webhook() {
    let env = Env::new().await;
    env.catalog().await;
    env.stripe().await;
    env.customer("ann", "USD").await;
    let c = env
        .billing
        .create_order(
            &user("ann"),
            OrderInput {
                items: vec![ItemInput::new("credits-100")],
                idempotency_key: "o".into(),
                ..OrderInput::default()
            },
        )
        .await
        .unwrap();
    assert!(c.payment_url.is_some());
    let inv = c.invoice.unwrap();
    assert_eq!(env.credits.balance(&sys(), Some(&pk("ann"))).await.unwrap().balance, 0);
    let ev = stripe_event(
        "evt_o",
        "payment_intent.succeeded",
        env.clock_secs(),
        json!({"id": "pi_o", "amount": 900, "currency": "usd", "metadata": {"forgebase_invoice": inv.id}}),
    );
    env.stripe_webhook(&ev, env.clock_secs()).await.unwrap();
    assert_eq!(env.credits.balance(&sys(), Some(&pk("ann"))).await.unwrap().balance, 100);
}

#[tokio::test]
async fn manual_billing_and_voiding() {
    let env = Env::new().await;
    env.catalog().await;
    env.billing
        .put_provider(&admin(), "manual", forgebase_billing::ProviderInput::default())
        .await
        .unwrap();
    env.customer("ann", "USD").await;
    let mut cust = forgebase_billing::CustomerInput::default();
    cust.provider = Some("manual".into());
    env.billing.put_customer(&user("ann"), cust).await.unwrap();
    // Manual billing: the invoice stays open until an operator records the payment.
    let c = env.subscribe("ann", &["pro-m"], "k").await;
    assert!(c.payment_url.is_none());
    assert_eq!(c.invoice.as_ref().unwrap().status, InvoiceStatus::Open);
    let subs = env.billing.subscriptions(&user("ann"), None).await.unwrap();
    let inv_id = subs[0].latest_invoice_id.clone().unwrap();
    // Members cannot mark their own invoices paid.
    assert!(env.billing.mark_paid(&user("ann"), &inv_id, MarkPaidInput::default()).await.is_err());
    let inv = env.billing.mark_paid(&admin(), &inv_id, MarkPaidInput { reference: Some("wire 42".into()) }).await.unwrap();
    assert_eq!(inv.status, InvoiceStatus::Paid);
    assert_eq!(env.billing.subscription(&user("ann"), &subs[0].id).await.unwrap().status, SubscriptionStatus::Active);
    assert_eq!(env.plan_of(&pk("ann")).0.as_deref(), Some("pro"));
    // Voiding needs an unpaid invoice.
    let e = env.billing.void_invoice(&admin(), &inv_id).await.unwrap_err();
    assert!(matches!(e, BillingError::Conflict { .. }), "{e:?}");
}
