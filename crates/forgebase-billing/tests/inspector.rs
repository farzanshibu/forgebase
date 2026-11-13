//! The inbound webhook inspector (ADR-0032): every delivery is recorded and
//! linked; forged ones are recorded and never processed; the unverified
//! budget stops storing without changing the answer; replay is idempotent;
//! re-verification after a secret fix processes exactly once, also under
//! concurrent calls; retention prune; administrators only. Runs on the
//! backend `FORGEBASE_TEST_BACKEND` selects.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use common::*;
use forgebase_api::{AppState, RouterOptions};
use forgebase_billing::provider::{razorpay, stripe};
use forgebase_billing::{
    BillingError, InvoiceStatus, ProviderInput, WebhookDeliveryFilter, WebhookLogConfig, WebhookProcessing,
    WebhookVerification,
};
use forgebase_core::DataLayout;
use forgebase_data::AccessContext;
use serde_json::{Value as Json, json};
use tower::ServiceExt;

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

/// Stripe headers signed with `secret` at `t`, plus headers that must never
/// be stored.
fn stripe_headers(secret: &str, t: i64, body: &[u8]) -> Vec<(String, String)> {
    vec![
        ("stripe-signature".to_owned(), stripe::sign(secret, t, body)),
        ("content-type".to_owned(), "application/json".to_owned()),
        (
            "user-agent".to_owned(),
            "Stripe/1.0 (+https://stripe.com/docs/webhooks)".to_owned(),
        ),
        (
            "authorization".to_owned(),
            "Bearer sk_test_must_not_be_stored".to_owned(),
        ),
        ("cookie".to_owned(), "fb_session=must_not_be_stored".to_owned()),
    ]
}

fn all() -> WebhookDeliveryFilter {
    WebhookDeliveryFilter::default()
}

fn conflict(e: &BillingError) -> &'static str {
    match e {
        BillingError::Conflict { code, .. } => code,
        other => panic!("expected a conflict, got {other:?}"),
    }
}

fn webhook_code(e: &BillingError) -> &'static str {
    match e {
        BillingError::Webhook { code, .. } => code,
        other => panic!("expected a webhook error, got {other:?}"),
    }
}

#[tokio::test]
async fn verified_stripe_and_razorpay_deliveries_are_recorded_and_linked() {
    let env = Env::new().await;
    let (sub, inv) = open_invoice(&env).await;
    let raw = paid("evt_ok", env.clock_secs(), "pi_ok", &inv).to_string();
    let h = stripe_headers(STRIPE_WHSEC, env.clock_secs(), raw.as_bytes());
    let out = env
        .billing
        .receive_webhook("stripe", &h, raw.as_bytes(), Some("198.51.100.20".into()))
        .await
        .unwrap();
    assert_eq!(out.result, "processed");
    let page = env.billing.webhook_deliveries(&admin(), all()).await.unwrap();
    assert_eq!(page.total, 1);
    let d = &page.items[0];
    assert_eq!(
        (d.provider.as_str(), d.verification, d.processing, d.outcome.as_str()),
        (
            "stripe",
            WebhookVerification::Verified,
            Some(WebhookProcessing::Applied),
            "applied"
        )
    );
    assert_eq!(
        (d.event_id.as_deref(), d.event_type.as_deref(), d.detail.as_deref()),
        (Some("evt_ok"), Some("payment_intent.succeeded"), Some("invoice_paid"))
    );
    assert_eq!(d.client_addr.as_deref(), Some("198.51.100.20"));
    assert_eq!(d.received_at, T0);
    assert!(d.body_stored && !d.body_truncated);
    assert_eq!(d.body_size, i64::try_from(raw.len()).unwrap());
    assert!(d.latency_us.is_some());
    // Linked to the event row, the invoice, its payment, customer and subscription.
    let payment = env.billing.payments(&user("ann"), None, None).await.unwrap()[0]
        .id
        .clone();
    assert_eq!(d.links.invoice.as_deref(), Some(inv.as_str()));
    assert_eq!(d.links.subscription.as_deref(), Some(sub.as_str()));
    assert_eq!(d.links.payment.as_deref(), Some(payment.as_str()));
    assert!(d.links.customer.is_some());
    let events = env.billing.webhook_events(&admin(), None).await.unwrap();
    assert_eq!(d.links.webhook_event.as_deref(), Some(events[0].id.as_str()));
    assert_eq!(d.body_sha256.len(), 64);

    // Detail: the body as received, allowlisted headers only.
    let detail = env.billing.webhook_delivery(&admin(), &d.id).await.unwrap();
    assert_eq!(detail.body.as_deref(), Some(raw.as_str()));
    assert_eq!(detail.body_encoding.as_deref(), Some("utf8"));
    assert!(detail.headers["stripe-signature"].as_str().unwrap().starts_with("t="));
    assert_eq!(detail.headers["content-type"], "application/json");
    assert!(detail.headers.get("authorization").is_none() && detail.headers.get("cookie").is_none());
    let text = serde_json::to_string(&detail).unwrap();
    assert!(
        !text.contains("must_not_be_stored") && !text.contains(STRIPE_WHSEC),
        "{text}"
    );

    // A redelivery is recorded as a duplicate of the same event.
    env.billing.handle_webhook("stripe", &h, raw.as_bytes()).await.unwrap();
    let dup = &env
        .billing
        .webhook_deliveries(
            &admin(),
            WebhookDeliveryFilter {
                outcome: Some("duplicate".into()),
                ..all()
            },
        )
        .await
        .unwrap()
        .items[0];
    assert_eq!(dup.links.webhook_event, d.links.webhook_event);

    // Razorpay: the account is the key id.
    env.razorpay().await;
    env.billing
        .put_customer(
            &user("ravi"),
            forgebase_billing::CustomerInput {
                currency: Some("INR".into()),
                provider: Some("razorpay".into()),
                ..forgebase_billing::CustomerInput::default()
            },
        )
        .await
        .unwrap();
    let c = env.subscribe("ravi", &["pro-m"], "kr").await;
    let rinv = c.invoice.unwrap().id;
    let body = json!({
        "entity": "event", "event": "payment.captured", "created_at": env.clock_secs(),
        "payload": {"payment": {"entity": {"id": "pay_rzp_1", "amount": 250000, "currency": "INR", "status": "captured",
            "token_id": "token_rzp_1", "customer_id": "cust_rzp_1", "method": "card",
            "notes": {"forgebase_invoice": rinv}}}}
    });
    assert_eq!(env.razorpay_webhook("evt_r1", &body).await.unwrap().result, "processed");
    let r = &env
        .billing
        .webhook_deliveries(
            &admin(),
            WebhookDeliveryFilter {
                provider: Some("razorpay".into()),
                ..all()
            },
        )
        .await
        .unwrap()
        .items[0];
    assert_eq!(
        (r.account.as_deref(), r.outcome.as_str(), r.links.invoice.as_deref()),
        (Some(RZP_KEY), "applied", Some(rinv.as_str()))
    );
    assert!(r.links.payment.is_some() && r.links.subscription.is_some());
    // Filters.
    let f = |p: Option<&str>, o: Option<&str>, t: Option<&str>| WebhookDeliveryFilter {
        provider: p.map(str::to_owned),
        outcome: o.map(str::to_owned),
        event_type: t.map(str::to_owned),
        ..all()
    };
    let n = |w: WebhookDeliveryFilter| {
        let b = env.billing.clone();
        async move { b.webhook_deliveries(&admin(), w).await.unwrap().total }
    };
    assert_eq!(n(f(Some("stripe"), None, None)).await, 2);
    assert_eq!(n(f(None, Some("applied"), None)).await, 2);
    assert_eq!(n(f(None, None, Some("payment.captured"))).await, 1);
    let late = WebhookDeliveryFilter {
        from: Some(T0 + 1),
        ..all()
    };
    assert_eq!(n(late).await, 0);
    let paged = env
        .billing
        .webhook_deliveries(
            &admin(),
            WebhookDeliveryFilter {
                limit: Some(1),
                offset: Some(1),
                ..all()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (paged.items.len(), paged.total, paged.limit, paged.offset),
        (1, 3, 1, 1)
    );
    let e = env
        .billing
        .webhook_deliveries(&admin(), f(None, Some("bogus"), None))
        .await
        .unwrap_err();
    assert!(
        matches!(
            e,
            BillingError::Invalid {
                code: "invalid_parameter",
                ..
            }
        ),
        "{e:?}"
    );
}

#[tokio::test]
async fn forged_stale_malformed_and_unknown_deliveries_are_recorded_not_processed() {
    let env = Env::new().await;
    let (_, inv) = open_invoice(&env).await;
    let now = env.clock_secs();
    let raw = paid("evt_f", now, "pi_f", &inv).to_string();
    // Forged signature: same answer as before.
    let forged = stripe_headers("whsec_attacker_secret", now, raw.as_bytes());
    let e = env
        .billing
        .handle_webhook("stripe", &forged, raw.as_bytes())
        .await
        .unwrap_err();
    assert_eq!(webhook_code(&e), "invalid_signature");
    // Signed long ago (replayed): timestamp out of tolerance.
    let old = stripe_headers(STRIPE_WHSEC, now - 600, raw.as_bytes());
    let e = env
        .billing
        .handle_webhook("stripe", &old, raw.as_bytes())
        .await
        .unwrap_err();
    assert_eq!(webhook_code(&e), "stale_signature");
    // Validly signed garbage: malformed.
    let junk = b"not json".to_vec();
    let e = env
        .billing
        .handle_webhook("stripe", &stripe_headers(STRIPE_WHSEC, now, &junk), &junk)
        .await
        .unwrap_err();
    assert_eq!(webhook_code(&e), "invalid_payload");
    // No such provider.
    let e = env
        .billing
        .handle_webhook("../paypal\"x", &[], b"{}")
        .await
        .unwrap_err();
    assert!(matches!(e, BillingError::NotConfigured(_)), "{e:?}");

    // Nothing was processed.
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_billing_webhook_events")
            .await,
        0
    );
    assert_eq!(
        env.billing.invoice(&admin(), &inv).await.unwrap().status,
        InvoiceStatus::Open
    );
    let page = env.billing.webhook_deliveries(&admin(), all()).await.unwrap();
    assert_eq!(page.total, 4);
    let by = |o: &str| page.items.iter().find(|d| d.outcome == o).unwrap().clone();
    let f = by("signature_invalid");
    assert_eq!(
        (f.verification, f.processing, f.event_id.as_deref()),
        (WebhookVerification::SignatureInvalid, None, None)
    );
    assert_eq!(
        f.reason.as_deref(),
        Some("invalid_signature: the webhook signature does not match")
    );
    assert!(f.links.webhook_event.is_none() && f.links.invoice.is_none());
    assert_eq!(
        by("timestamp_out_of_tolerance").verification,
        WebhookVerification::TimestampOutOfTolerance
    );
    assert_eq!(by("malformed").verification, WebhookVerification::Malformed);
    let u = by("unknown_provider");
    assert_eq!(u.provider, ".._paypal_x");
    // No secret or expected signature anywhere in what is stored.
    let expected = stripe::sign(STRIPE_WHSEC, now, raw.as_bytes());
    let v1 = expected.split("v1=").nth(1).unwrap().to_owned();
    for d in &page.items {
        let detail = serde_json::to_string(&env.billing.webhook_delivery(&admin(), &d.id).await.unwrap()).unwrap();
        assert!(
            !detail.contains(STRIPE_WHSEC) && !detail.contains("must_not_be_stored"),
            "{detail}"
        );
        if d.outcome == "signature_invalid" {
            assert!(!detail.contains(&v1), "the expected signature leaked: {detail}");
        }
    }
    // An unverified delivery cannot be replayed.
    let e = env.billing.replay_webhook_delivery(&admin(), &f.id).await.unwrap_err();
    assert_eq!(conflict(&e), "delivery_not_verified");
}

#[tokio::test]
async fn unverified_budget_stops_storing_and_answers_the_same() {
    let env = Env::new().await;
    let (_, inv) = open_invoice(&env).await;
    let _ = forgebase_observability::metrics_handle();
    let now = env.clock_secs();
    let raw = paid("evt_b", now, "pi_b", &inv).to_string();
    let forged = stripe_headers("whsec_attacker_secret", now, raw.as_bytes());
    // What one stored row keeps: the body and the allowlisted headers.
    let kept: serde_json::Map<String, Json> = forged
        .iter()
        .filter(|(k, _)| matches!(k.as_str(), "stripe-signature" | "content-type" | "user-agent"))
        .map(|(k, v)| (k.clone(), Json::String(v.clone())))
        .collect();
    let one = raw.len() + Json::Object(kept).to_string().len();
    env.billing.set_webhook_log(WebhookLogConfig {
        unverified_per_minute: 4,
        unverified_bytes_per_minute: 2 * one + one / 2,
        unverified_max_rows: 6,
        ..WebhookLogConfig::default()
    });
    let mut answers = Vec::new();
    for _ in 0..7 {
        let e = env
            .billing
            .handle_webhook("stripe", &forged, raw.as_bytes())
            .await
            .unwrap_err();
        answers.push(e.to_string());
    }
    assert!(answers.iter().all(|a| *a == answers[0]), "{answers:?}");
    // 4 rows this minute: 2 with their body, 2 without; 3 not stored at all.
    let page = env.billing.webhook_deliveries(&admin(), all()).await.unwrap();
    assert_eq!(page.total, 4);
    assert_eq!(page.items.iter().filter(|d| d.body_stored).count(), 2);
    let bare = page.items.iter().find(|d| !d.body_stored).unwrap();
    let detail = env.billing.webhook_delivery(&admin(), &bare.id).await.unwrap();
    assert_eq!((detail.body, detail.headers), (None, json!({})));
    assert_eq!(bare.body_size, i64::try_from(raw.len()).unwrap());
    // A verified delivery is not budgeted.
    let ok = paid("evt_ok", now, "pi_ok", &inv);
    assert_eq!(env.stripe_webhook(&ok, now).await.unwrap().result, "processed");
    let verified = env
        .billing
        .webhook_deliveries(
            &admin(),
            WebhookDeliveryFilter {
                outcome: Some("applied".into()),
                ..all()
            },
        )
        .await
        .unwrap();
    assert!(verified.items[0].body_stored);
    // The next minute has a new budget; the total cap (6 unverified rows)
    // evicts the oldest unverified rows, never verified ones.
    env.clock.advance(MIN);
    for _ in 0..4 {
        env.billing
            .handle_webhook("stripe", &forged, raw.as_bytes())
            .await
            .unwrap_err();
    }
    let unverified = env
        .billing
        .webhook_deliveries(
            &admin(),
            WebhookDeliveryFilter {
                outcome: Some("signature_invalid".into()),
                ..all()
            },
        )
        .await
        .unwrap();
    assert_eq!(unverified.total, 6);
    assert_eq!(
        unverified.items.iter().filter(|d| d.received_at == T0 + MIN).count(),
        4,
        "the newest are kept"
    );
    assert_eq!(env.billing.webhook_deliveries(&admin(), all()).await.unwrap().total, 7);
    // Metrics: fixed labels, counted whether stored or not.
    let text = forgebase_observability::metrics_handle().render();
    assert!(
        text.contains(r#"forgebase_billing_webhook_deliveries_total{provider="stripe",outcome="signature_invalid"}"#),
        "{text}"
    );
    assert!(
        text.contains(r#"forgebase_billing_webhook_log_dropped_total{provider="stripe",kind="delivery"}"#),
        "{text}"
    );
    assert!(
        text.contains(r#"forgebase_billing_webhook_log_dropped_total{provider="stripe",kind="body"}"#),
        "{text}"
    );
    assert!(
        text.contains(r#"forgebase_billing_webhook_processing_seconds_bucket{provider="stripe""#),
        "{text}"
    );
}

#[tokio::test]
async fn replay_of_an_applied_event_is_a_reported_no_op() {
    let env = Env::new().await;
    let (_, inv) = open_invoice(&env).await;
    let ev = paid("evt_r", env.clock_secs(), "pi_r", &inv);
    assert_eq!(
        env.stripe_webhook(&ev, env.clock_secs()).await.unwrap().result,
        "processed"
    );
    let d = env.billing.webhook_deliveries(&admin(), all()).await.unwrap().items[0].clone();
    // An hour later (far outside the signature tolerance): checked against
    // the time it was received.
    env.clock.advance(60 * MIN);
    let r = env.billing.replay_webhook_delivery(&admin(), &d.id).await.unwrap();
    assert_eq!(r.result, "already_applied", "{r:?}");
    assert_eq!(r.delivery.replays, 1);
    assert_eq!(r.delivery.outcome, "duplicate");
    assert_eq!(env.count("SELECT COUNT(*) FROM _forgebase_billing_payments").await, 1);
    let i = env.billing.invoice(&admin(), &inv).await.unwrap();
    assert_eq!((i.status, i.amount_paid), (InvoiceStatus::Paid, 3000));
    assert_eq!(
        env.count("SELECT attempts FROM _forgebase_billing_webhook_events")
            .await,
        1,
        "a replay is not a delivery"
    );
    assert_eq!(
        env.count(
            "SELECT COUNT(*) FROM _forgebase_billing_audit WHERE action = 'webhook_delivery.replay' AND actor = 'admin:root'"
        )
        .await,
        1
    );
    // An event whose processing failed is processed again by a replay (here
    // billing already holds the payment, so it changes nothing more).
    env.exec("UPDATE _forgebase_billing_webhook_events SET status = 'failed'")
        .await;
    let r = env.billing.replay_webhook_delivery(&admin(), &d.id).await.unwrap();
    assert_eq!(
        (r.result.as_str(), r.detail.as_deref()),
        ("ignored", Some("already_succeeded"))
    );
    assert_eq!(r.delivery.replays, 2);
    assert_eq!(env.count("SELECT COUNT(*) FROM _forgebase_billing_payments").await, 1);
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_billing_webhook_events WHERE status = 'ignored'")
            .await,
        1
    );
    // A rotated secret: the stored delivery no longer verifies and is not processed.
    env.billing
        .put_provider(
            &admin(),
            "stripe",
            ProviderInput {
                webhook_secret: Some("whsec_rotated_secret".into()),
                ..ProviderInput::default()
            },
        )
        .await
        .unwrap();
    let r = env.billing.replay_webhook_delivery(&admin(), &d.id).await.unwrap();
    assert_eq!(r.result, "unverified");
    assert_eq!(
        r.delivery.verification,
        WebhookVerification::Verified,
        "the record is kept"
    );
    // Unknown delivery.
    let e = env
        .billing
        .replay_webhook_delivery(&admin(), "whd_nope")
        .await
        .unwrap_err();
    assert!(matches!(e, BillingError::NotFound(_)), "{e:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reverify_after_a_secret_fix_processes_exactly_once_under_concurrency() {
    let env = Env::new().await;
    env.catalog().await;
    // The operator pasted the wrong webhook secret.
    env.billing
        .put_provider(
            &admin(),
            "stripe",
            ProviderInput {
                api_key: Some(STRIPE_KEY.into()),
                webhook_secret: Some("whsec_misconfigured_secret".into()),
                ..ProviderInput::default()
            },
        )
        .await
        .unwrap();
    env.customer("ann", "USD").await;
    let inv = env.subscribe("ann", &["pro-m"], "k").await.invoice.unwrap().id;
    // Stripe delivers (and retries) the genuine event: refused each time.
    let raw = paid("evt_fix", env.clock_secs(), "pi_fix", &inv).to_string();
    for _ in 0..3 {
        let h = stripe_headers(STRIPE_WHSEC, env.clock_secs(), raw.as_bytes());
        let e = env
            .billing
            .handle_webhook("stripe", &h, raw.as_bytes())
            .await
            .unwrap_err();
        assert_eq!(webhook_code(&e), "invalid_signature");
        env.clock.advance(MIN);
    }
    let ids: Vec<String> = env
        .billing
        .webhook_deliveries(&admin(), all())
        .await
        .unwrap()
        .items
        .iter()
        .map(|d| d.id.clone())
        .collect();
    assert_eq!(ids.len(), 3);
    // Still wrong: re-verify reports it and processes nothing.
    let r = env.billing.reverify_webhook_delivery(&admin(), &ids[0]).await.unwrap();
    assert_eq!(r.result, "unverified");
    assert_eq!(r.delivery.reverify_attempts, 1);
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_billing_webhook_events")
            .await,
        0
    );
    // The operator fixes the secret, an hour later.
    env.clock.advance(60 * MIN);
    env.billing
        .put_provider(
            &admin(),
            "stripe",
            ProviderInput {
                webhook_secret: Some(STRIPE_WHSEC.into()),
                ..ProviderInput::default()
            },
        )
        .await
        .unwrap();
    // Eight concurrent re-verifications of every stored delivery.
    let mut set = tokio::task::JoinSet::new();
    for i in 0..24 {
        let (b, id) = (env.billing.clone(), ids[i % 3].clone());
        set.spawn(async move { b.reverify_webhook_delivery(&admin(), &id).await });
    }
    let (mut applied, mut already, mut refused) = (0, 0, 0);
    while let Some(r) = set.join_next().await {
        match r.unwrap() {
            Ok(r) if r.result == "applied" => applied += 1,
            Ok(r) if r.result == "already_applied" => already += 1,
            Ok(r) => panic!("unexpected {r:?}"),
            Err(e) => {
                assert!(
                    matches!(conflict(&e), "delivery_already_verified" | "reverify_in_progress"),
                    "{e:?}"
                );
                refused += 1;
            }
        }
    }
    assert_eq!((applied, already, refused), (1, 2, 21));
    // Applied exactly once.
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_billing_payments WHERE status = 'succeeded'")
            .await,
        1
    );
    let i = env.billing.invoice(&admin(), &inv).await.unwrap();
    assert_eq!((i.status, i.amount_paid), (InvoiceStatus::Paid, 3000));
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_billing_webhook_events WHERE status = 'processed'")
            .await,
        1
    );
    let page = env.billing.webhook_deliveries(&admin(), all()).await.unwrap();
    assert!(
        page.items
            .iter()
            .all(|d| d.verification == WebhookVerification::Verified)
    );
    assert!(
        page.items
            .iter()
            .all(|d| d.reverified_at.is_some() && d.reason.is_none())
    );
    assert_eq!(page.items.iter().filter(|d| d.outcome == "applied").count(), 1);
    assert_eq!(page.items.iter().filter(|d| d.outcome == "duplicate").count(), 2);
    assert!(page.items.iter().all(|d| d.links.webhook_event.is_some()));
    // Verified now: re-verify refuses, replay is a no-op.
    let e = env
        .billing
        .reverify_webhook_delivery(&admin(), &ids[0])
        .await
        .unwrap_err();
    assert_eq!(conflict(&e), "delivery_already_verified");
    let r = env.billing.replay_webhook_delivery(&admin(), &ids[0]).await.unwrap();
    assert_eq!(r.result, "already_applied");
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_billing_audit WHERE action = 'webhook_delivery.reverify'")
            .await,
        4,
        "the failed attempt and the three that ran"
    );
}

#[tokio::test]
async fn retention_prunes_old_deliveries_in_maintenance() {
    let env = Env::new().await;
    let (_, inv) = open_invoice(&env).await;
    env.billing.set_webhook_log(WebhookLogConfig {
        retention_days: 7,
        ..WebhookLogConfig::default()
    });
    let ev = paid("evt_old", env.clock_secs(), "pi_old", &inv);
    env.stripe_webhook(&ev, env.clock_secs()).await.unwrap();
    env.billing.handle_webhook("stripe", &[], b"{}").await.unwrap_err();
    let rep = env.advance(6 * DAY).await;
    assert_eq!(rep.webhook_deliveries_pruned, 0);
    env.billing.handle_webhook("stripe", &[], b"{}").await.unwrap_err();
    let rep = env.advance(DAY + MIN).await;
    assert_eq!(rep.webhook_deliveries_pruned, 2);
    let left = env.billing.webhook_deliveries(&admin(), all()).await.unwrap();
    assert_eq!(left.total, 1);
    assert_eq!(left.items[0].received_at, T0 + 6 * DAY);
    // The event dedup outlives the log: a redelivery is still a duplicate.
    env.clock.advance(-7 * DAY);
    assert_eq!(
        env.stripe_webhook(&ev, env.clock_secs()).await.unwrap().result,
        "duplicate"
    );
}

#[tokio::test]
async fn only_platform_administrators_inspect() {
    let env = Env::new().await;
    let (_, inv) = open_invoice(&env).await;
    let ev = paid("evt_a", env.clock_secs(), "pi_a", &inv);
    env.stripe_webhook(&ev, env.clock_secs()).await.unwrap();
    let id = env.billing.webhook_deliveries(&admin(), all()).await.unwrap().items[0]
        .id
        .clone();
    for ctx in [user("ann"), AccessContext::anonymous()] {
        let anon = ctx.principal == forgebase_data::Principal::Anonymous;
        let check = |e: BillingError| {
            if anon {
                assert!(matches!(e, BillingError::Unauthenticated), "{e:?}");
            } else {
                assert!(matches!(e, BillingError::Forbidden(_)), "{e:?}");
            }
        };
        check(env.billing.webhook_deliveries(&ctx, all()).await.unwrap_err());
        check(env.billing.webhook_delivery(&ctx, &id).await.unwrap_err());
        check(env.billing.replay_webhook_delivery(&ctx, &id).await.unwrap_err());
        check(env.billing.reverify_webhook_delivery(&ctx, &id).await.unwrap_err());
    }
}

// ---------------------------------------------------------------------------
// HTTP
// ---------------------------------------------------------------------------

async fn app(env: &Env) -> Router {
    let layout = DataLayout::new(env.tmp.data_dir().join("data"));
    let state = AppState::from_data(env.db.clone(), layout, env.data.clone());
    forgebase_api::router(
        state,
        RouterOptions {
            extensions: vec![forgebase_billing::extension(env.billing.clone())],
            ..RouterOptions::default()
        },
    )
}

async fn send(
    app: &Router,
    ctx: Option<AccessContext>,
    method: Method,
    uri: &str,
    headers: &[(String, String)],
    body: Vec<u8>,
) -> (StatusCode, Json) {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    let mut req = req.body(Body::from(body)).unwrap();
    if let Some(ctx) = ctx {
        req.extensions_mut().insert(ctx);
    }
    let peer: SocketAddr = "203.0.113.50:44321".parse().unwrap();
    req.extensions_mut().insert(ConnectInfo(peer));
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
}

#[tokio::test]
async fn http_inspector_routes() {
    let env = Env::new().await;
    let (_, inv) = open_invoice(&env).await;
    let app = app(&env).await;
    let now = env.clock_secs();
    let raw = paid("evt_h", now, "pi_h", &inv).to_string().into_bytes();
    // A forged delivery: the provider gets the same 400 as before.
    let forged = stripe_headers("whsec_attacker_secret", now, &raw);
    let (s, b) = send(
        &app,
        None,
        Method::POST,
        "/api/v1/billing/webhooks/stripe",
        &forged,
        raw.clone(),
    )
    .await;
    assert_eq!(
        (s, b["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_signature"))
    );
    let good = stripe_headers(STRIPE_WHSEC, now, &raw);
    let (s, b) = send(
        &app,
        None,
        Method::POST,
        "/api/v1/billing/webhooks/stripe",
        &good,
        raw.clone(),
    )
    .await;
    assert_eq!((s, b["result"].as_str()), (StatusCode::OK, Some("processed")));

    let (s, b) = send(
        &app,
        Some(admin()),
        Method::GET,
        "/api/v1/billing/webhook-deliveries?provider=stripe&outcome=applied",
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["total"], 1);
    let d = &b["items"][0];
    assert_eq!(
        (d["verification"].as_str(), d["outcome"].as_str()),
        (Some("verified"), Some("applied"))
    );
    assert_eq!(d["client_addr"], "203.0.113.50");
    assert_eq!(d["links"]["invoice_id"], inv.as_str());
    assert!(d.get("body").is_none(), "the list carries no bodies");
    let id = d["id"].as_str().unwrap().to_owned();
    let (s, all) = send(
        &app,
        Some(admin()),
        Method::GET,
        &format!("/api/v1/billing/webhook-deliveries?from={}", "2026-11-01T00:00:00Z"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!((s, all["total"].as_i64()), (StatusCode::OK, Some(2)));

    let (s, b) = send(
        &app,
        Some(admin()),
        Method::GET,
        &format!("/api/v1/billing/webhook-deliveries/{id}"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["body"].as_str().map(str::as_bytes), Some(raw.as_slice()));
    assert!(b["headers"].get("authorization").is_none());
    let (s, b) = send(
        &app,
        Some(admin()),
        Method::POST,
        &format!("/api/v1/billing/webhook-deliveries/{id}/replay"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        (s, b["result"].as_str()),
        (StatusCode::OK, Some("already_applied")),
        "{b}"
    );
    let (s, b) = send(
        &app,
        Some(admin()),
        Method::POST,
        &format!("/api/v1/billing/webhook-deliveries/{id}/reverify"),
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        (s, b["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("delivery_already_verified"))
    );
    let (s, b) = send(
        &app,
        Some(admin()),
        Method::GET,
        "/api/v1/billing/webhook-deliveries/whd_missing",
        &[],
        Vec::new(),
    )
    .await;
    assert_eq!(
        (s, b["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("not_found"))
    );

    // Non-administrators: 403 (401 anonymous), before anything is read.
    for (uri, method) in [
        ("/api/v1/billing/webhook-deliveries".to_owned(), Method::GET),
        (
            "/api/v1/billing/webhook-deliveries?from=not-a-time".to_owned(),
            Method::GET,
        ),
        (format!("/api/v1/billing/webhook-deliveries/{id}"), Method::GET),
        (format!("/api/v1/billing/webhook-deliveries/{id}/replay"), Method::POST),
        (
            format!("/api/v1/billing/webhook-deliveries/{id}/reverify"),
            Method::POST,
        ),
    ] {
        let (s, b) = send(&app, Some(user("ann")), method.clone(), &uri, &[], Vec::new()).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{uri}: {b}");
        let (s, _) = send(&app, None, method, &uri, &[], Vec::new()).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "{uri}");
    }
    // The OpenAPI fragment names the operations.
    let doc = serde_json::to_value(forgebase_billing::openapi()).unwrap();
    for op in [
        "billing_webhook_deliveries",
        "billing_webhook_delivery",
        "billing_replay_webhook_delivery",
        "billing_reverify_webhook_delivery",
    ] {
        assert!(doc.to_string().contains(op), "{op}");
    }
    // Razorpay deliveries through HTTP record the signature headers needed to
    // verify them again, and nothing else.
    env.razorpay().await;
    let body =
        json!({"entity": "event", "event": "order.paid", "created_at": env.clock_secs(), "payload": {}}).to_string();
    let h = vec![
        (
            "x-razorpay-signature".to_owned(),
            razorpay::sign(RZP_WHSEC, body.as_bytes()),
        ),
        ("x-razorpay-event-id".to_owned(), "evt_rh".to_owned()),
        ("x-api-key".to_owned(), "must_not_be_stored".to_owned()),
    ];
    let (s, _) = send(
        &app,
        None,
        Method::POST,
        "/api/v1/billing/webhooks/razorpay",
        &h,
        body.into_bytes(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, b) = send(
        &app,
        Some(admin()),
        Method::GET,
        "/api/v1/billing/webhook-deliveries?provider=razorpay",
        &[],
        Vec::new(),
    )
    .await;
    let rid = b["items"][0]["id"].as_str().unwrap().to_owned();
    let (_, b) = send(
        &app,
        Some(admin()),
        Method::GET,
        &format!("/api/v1/billing/webhook-deliveries/{rid}"),
        &[],
        Vec::new(),
    )
    .await;
    let names: Vec<&str> = b["headers"].as_object().unwrap().keys().map(String::as_str).collect();
    assert!(
        names.contains(&"x-razorpay-signature") && names.contains(&"x-razorpay-event-id"),
        "{names:?}"
    );
    assert!(!names.contains(&"x-api-key"), "{names:?}");
}
