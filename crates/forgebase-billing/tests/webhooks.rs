//! Webhooks: forged, tampered and replayed deliveries are refused;
//! redeliveries are idempotent; out-of-order and stale events are ignored;
//! Stripe and Razorpay schemes; test-mode keys only, secrets sealed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use forgebase_billing::provider::stripe;
use forgebase_billing::{BillingError, InvoiceStatus, PaymentStatus, ProviderInput, SubscriptionStatus};
use serde_json::{Value as Json, json};

/// A customer without a card, subscribed: an open first invoice.
async fn open_invoice(env: &Env) -> (String, String) {
    env.catalog().await;
    env.stripe().await;
    env.customer("ann", "USD").await;
    let c = env.subscribe("ann", &["pro-m"], "k").await;
    (c.subscription.unwrap().id, c.invoice.unwrap().id)
}

fn paid(id: &str, created: i64, pi: &str, invoice: &str) -> Json {
    stripe_event(
        id,
        "payment_intent.succeeded",
        created,
        json!({"id": pi, "amount": 3000, "amount_received": 3000, "currency": "usd", "metadata": {"forgebase_invoice": invoice}}),
    )
}

fn code(e: &BillingError) -> &str {
    match e {
        BillingError::Webhook { code, .. } => code,
        _ => "",
    }
}

#[tokio::test]
async fn forged_and_tampered_signatures_are_refused() {
    let env = Env::new().await;
    let (_, inv) = open_invoice(&env).await;
    let now = env.clock_secs();
    let body = paid("evt_1", now, "pi_1", &inv).to_string();
    // Signed with another secret.
    let forged = vec![("stripe-signature".to_owned(), stripe::sign("whsec_attacker_secret", now, body.as_bytes()))];
    let e = env.billing.handle_webhook("stripe", &forged, body.as_bytes()).await.unwrap_err();
    assert_eq!(code(&e), "invalid_signature");
    // Signed, then the amount changed.
    let good = stripe::sign(STRIPE_WHSEC, now, body.as_bytes());
    let tampered = body.replace("3000", "1");
    let e = env
        .billing
        .handle_webhook("stripe", &[("stripe-signature".into(), good.clone())], tampered.as_bytes())
        .await
        .unwrap_err();
    assert_eq!(code(&e), "invalid_signature");
    // No signature.
    let e = env.billing.handle_webhook("stripe", &[], body.as_bytes()).await.unwrap_err();
    assert_eq!(code(&e), "invalid_signature");
    // Nothing was recorded or paid.
    assert_eq!(env.count("SELECT COUNT(*) FROM _forgebase_billing_webhook_events").await, 0);
    assert_eq!(env.billing.invoice(&admin(), &inv).await.unwrap().status, InvoiceStatus::Open);
    // An unconfigured provider is not found.
    let e = env.billing.handle_webhook("razorpay", &[], b"{}").await.unwrap_err();
    assert!(matches!(e, BillingError::NotConfigured(_)), "{e:?}");
}

#[tokio::test]
async fn replayed_old_delivery_is_refused() {
    let env = Env::new().await;
    let (_, inv) = open_invoice(&env).await;
    let then = env.clock_secs() - 600; // ten minutes ago, outside the 300 s tolerance
    let body = paid("evt_old", then, "pi_1", &inv).to_string();
    let headers = vec![("stripe-signature".to_owned(), stripe::sign(STRIPE_WHSEC, then, body.as_bytes()))];
    let e = env.billing.handle_webhook("stripe", &headers, body.as_bytes()).await.unwrap_err();
    assert_eq!(code(&e), "stale_signature");
    assert_eq!(env.billing.invoice(&admin(), &inv).await.unwrap().status, InvoiceStatus::Open);
    // A fresh signature over an event older than the replay window is ignored.
    let old = paid("evt_ancient", env.clock_secs() - 4 * 86_400, "pi_1", &inv);
    let out = env.stripe_webhook(&old, env.clock_secs()).await.unwrap();
    assert_eq!((out.result.as_str(), out.detail.as_deref()), ("ignored", Some("too_old")));
    assert_eq!(env.billing.invoice(&admin(), &inv).await.unwrap().status, InvoiceStatus::Open);
}

#[tokio::test]
async fn redelivery_is_idempotent() {
    let env = Env::new().await;
    let (sub, inv) = open_invoice(&env).await;
    let ev = paid("evt_dup", env.clock_secs(), "pi_dup", &inv);
    let first = env.stripe_webhook(&ev, env.clock_secs()).await.unwrap();
    assert_eq!(first.result, "processed");
    for _ in 0..3 {
        let again = env.stripe_webhook(&ev, env.clock_secs()).await.unwrap();
        assert_eq!(again.result, "duplicate");
    }
    let i = env.billing.invoice(&admin(), &inv).await.unwrap();
    assert_eq!((i.status, i.amount_paid), (InvoiceStatus::Paid, 3000));
    assert_eq!(env.count("SELECT COUNT(*) FROM _forgebase_billing_payments").await, 1);
    assert_eq!(env.count("SELECT attempts FROM _forgebase_billing_webhook_events").await, 4);
    assert_eq!(env.count("SELECT COUNT(*) FROM _forgebase_billing_audit WHERE action = 'invoice.paid'").await, 1);
    assert_eq!(
        env.billing.subscription(&admin(), &sub).await.unwrap().status,
        SubscriptionStatus::Active
    );
    // Another event for the same payment changes nothing either.
    let other = paid("evt_dup_2", env.clock_secs(), "pi_dup", &inv);
    let out = env.stripe_webhook(&other, env.clock_secs()).await.unwrap();
    assert_eq!((out.result.as_str(), out.detail.as_deref()), ("ignored", Some("already_succeeded")));
    let events = env.billing.webhook_events(&admin(), None).await.unwrap();
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn out_of_order_events_are_ignored() {
    let env = Env::new().await;
    env.catalog().await;
    env.stripe().await;
    env.customer("ann", "USD").await;
    env.save_card("ann").await;
    // The charge is processing: the webhook decides.
    env.http.set_mode(ChargeMode::Processing);
    let c = env.subscribe("ann", &["pro-m"], "k").await;
    let inv = c.invoice.unwrap();
    assert_eq!(inv.status, InvoiceStatus::Open);
    let p = &env.billing.payments(&admin(), Some(&pk("ann")), None).await.unwrap()[0];
    assert_eq!(p.status, PaymentStatus::Pending);
    let pi = p.provider_ref.clone().unwrap();
    let t = env.clock_secs();
    // succeeded (t) arrives before payment_failed (t - 30).
    let ok = paid("evt_ok", t, &pi, &inv.id);
    assert_eq!(env.stripe_webhook(&ok, t).await.unwrap().result, "processed");
    let late = stripe_event(
        "evt_late_fail",
        "payment_intent.payment_failed",
        t - 30,
        json!({"id": pi, "last_payment_error": {"code": "card_declined"}, "metadata": {"forgebase_invoice": inv.id}}),
    );
    let out = env.stripe_webhook(&late, t).await.unwrap();
    assert_eq!(out.result, "ignored");
    assert!(matches!(out.detail.as_deref(), Some("stale" | "already_succeeded")), "{out:?}");
    assert_eq!(env.billing.invoice(&admin(), &inv.id).await.unwrap().status, InvoiceStatus::Paid);
    let p = &env.billing.payments(&admin(), Some(&pk("ann")), None).await.unwrap()[0];
    assert_eq!(p.status, PaymentStatus::Succeeded);

    // A failure, then an older event for the same payment: stale.
    env.customer("bob", "USD").await;
    env.save_card("bob").await;
    let c = env.subscribe("bob", &["basic-m"], "k").await;
    let inv = c.invoice.unwrap();
    let pi = env.billing.payments(&admin(), Some(&pk("bob")), None).await.unwrap()[0]
        .provider_ref
        .clone()
        .unwrap();
    let fail = stripe_event(
        "evt_fail",
        "payment_intent.payment_failed",
        t + 10,
        json!({"id": pi, "last_payment_error": {"decline_code": "expired_card"}, "metadata": {"forgebase_invoice": inv.id}}),
    );
    assert_eq!(env.stripe_webhook(&fail, t).await.unwrap().result, "processed");
    let older = paid("evt_older", t + 5, &pi, &inv.id);
    let out = env.stripe_webhook(&older, t).await.unwrap();
    assert_eq!((out.result.as_str(), out.detail.as_deref()), ("ignored", Some("stale")));
    assert_eq!(env.billing.invoice(&admin(), &inv.id).await.unwrap().status, InvoiceStatus::Open);
    // Unknown types and unknown payments are ignored, not errors.
    let other = stripe_event("evt_x", "customer.updated", t, json!({"id": "cus_1"}));
    assert_eq!(env.stripe_webhook(&other, t).await.unwrap().detail.as_deref(), Some("unhandled_type"));
    let stray = paid("evt_stray", t, "pi_unknown", "in_unknown");
    assert_eq!(env.stripe_webhook(&stray, t).await.unwrap().result, "ignored");
}

#[tokio::test]
async fn razorpay_webhooks() {
    let env = Env::new().await;
    env.catalog().await;
    env.razorpay().await;
    env.customer("ravi", "INR").await;
    let c = env.subscribe("ravi", &["pro-m"], "k").await;
    let inv = c.invoice.unwrap();
    assert!(c.payment_url.unwrap().starts_with("https://rzp.test/"));
    let link = env.http.calls("/v1/payment_links").pop().unwrap();
    let auth = link.headers.iter().find(|(k, _)| k == "authorization").unwrap().1.clone();
    assert!(auth.starts_with("Basic "));
    let body = json!({
        "entity": "event", "event": "payment.captured", "created_at": env.clock_secs(),
        "payload": {"payment": {"entity": {"id": "pay_rzp_1", "amount": 250000, "currency": "INR", "status": "captured",
            "token_id": "token_rzp_1", "customer_id": "cust_rzp_1", "method": "card",
            "notes": {"forgebase_invoice": inv.id}}}}
    });
    // Forged.
    let raw = body.to_string();
    let bad = vec![
        ("x-razorpay-signature".to_owned(), forgebase_billing::provider::razorpay::sign("wrong_secret_1", raw.as_bytes())),
        ("x-razorpay-event-id".to_owned(), "evt_r1".to_owned()),
    ];
    let e = env.billing.handle_webhook("razorpay", &bad, raw.as_bytes()).await.unwrap_err();
    assert_eq!(code(&e), "invalid_signature");
    // Genuine, then redelivered.
    assert_eq!(env.razorpay_webhook("evt_r1", &body).await.unwrap().result, "processed");
    assert_eq!(env.razorpay_webhook("evt_r1", &body).await.unwrap().result, "duplicate");
    let i = env.billing.invoice(&admin(), &inv.id).await.unwrap();
    assert_eq!((i.status, i.currency.as_str(), i.amount_paid), (InvoiceStatus::Paid, "INR", 250_000));
    assert_eq!(env.plan_of(&pk("ravi")).0.as_deref(), Some("pro"));
    // The token is saved: the renewal is charged as a recurring payment.
    env.advance(30 * DAY).await;
    assert_eq!(env.http.calls("/v1/payments/create/recurring").len(), 1);
    let renewal = &env.billing.invoices(&admin(), Some(&pk("ravi")), Some(InvoiceStatus::Open), None).await.unwrap()[0];
    let pay = env.billing.payments(&admin(), Some(&pk("ravi")), None).await.unwrap()[0].clone();
    assert_eq!(pay.status, PaymentStatus::Pending);
    let captured = json!({
        "entity": "event", "event": "payment.captured", "created_at": env.clock_secs(),
        "payload": {"payment": {"entity": {"id": pay.provider_ref, "amount": 250000, "currency": "INR",
            "notes": {"forgebase_invoice": renewal.id}}}}
    });
    assert_eq!(env.razorpay_webhook("evt_r2", &captured).await.unwrap().result, "processed");
    assert_eq!(env.billing.invoice(&admin(), &renewal.id).await.unwrap().status, InvoiceStatus::Paid);
    // A missing event id is refused.
    let raw = captured.to_string();
    let h = vec![("x-razorpay-signature".to_owned(), forgebase_billing::provider::razorpay::sign(RZP_WHSEC, raw.as_bytes()))];
    let e = env.billing.handle_webhook("razorpay", &h, raw.as_bytes()).await.unwrap_err();
    assert_eq!(code(&e), "invalid_payload");
    // Refund through Razorpay, confirmed by its webhook.
    let cn = env
        .billing
        .credit_invoice(
            &admin(),
            &inv.id,
            forgebase_billing::CreditNoteInput {
                amount: Some(50_000),
                to_balance: false,
                reason: None,
                idempotency_key: "r".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(cn.refund.unwrap().status, forgebase_billing::RefundStatus::Succeeded);
    assert_eq!(env.http.calls("/v1/payments/pay_rzp_1/refund").len(), 1);
}

#[tokio::test]
async fn only_test_mode_keys_and_secrets_stay_sealed() {
    let env = Env::new().await;
    let put = |p: &'static str, key: &'static str, id: Option<&'static str>, wh: &'static str| {
        let b = env.billing.clone();
        async move {
            b.put_provider(
                &admin(),
                p,
                ProviderInput {
                    api_key: Some(key.into()),
                    key_id: id.map(Into::into),
                    webhook_secret: Some(wh.into()),
                    ..ProviderInput::default()
                },
            )
            .await
        }
    };
    let e = put("stripe", "sk_live_51realkey000", None, "whsec_x_123456").await.unwrap_err();
    assert!(matches!(e, BillingError::Invalid { code: "live_mode_refused", .. }), "{e:?}");
    let e = put("razorpay", "secret_123456", Some("rzp_live_abcdef"), "whsec_1234").await.unwrap_err();
    assert!(matches!(e, BillingError::Invalid { code: "live_mode_refused", .. }), "{e:?}");
    let e = put("paypal", "x", None, "y").await.unwrap_err();
    assert!(matches!(e, BillingError::Invalid { code: "unknown_provider", .. }), "{e:?}");
    // Members cannot configure providers.
    let e = env
        .billing
        .put_provider(&user("ann"), "stripe", ProviderInput::default())
        .await
        .unwrap_err();
    assert!(matches!(e, BillingError::Forbidden(_)), "{e:?}");
    env.stripe().await;
    let accounts = env.billing.providers(&admin()).await.unwrap();
    assert_eq!(accounts[0].hint.as_deref(), Some("…tkey"));
    assert_eq!(accounts[0].webhook_path, "/api/v1/billing/webhooks/stripe");
    let stored = env
        .db
        .fetch_all(&forgebase_db::Statement::raw("SELECT secret FROM _forgebase_billing_providers"))
        .await
        .unwrap();
    let sealed = stored.rows[0][0].as_str().unwrap().to_owned();
    assert!(!sealed.contains("sk_test") && !sealed.contains("whsec"), "secrets are sealed");
    assert!(!serde_json::to_string(&accounts).unwrap().contains(STRIPE_KEY));
}
