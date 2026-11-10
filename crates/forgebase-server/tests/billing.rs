//! SPEC §27 scenario 20, "process a billing webhook idempotently", through
//! the real server: billing is on by default and off only when configured off; a signed Stripe
//! webhook sent over HTTP pays the invoice once, a redelivery changes
//! nothing, and after a restart the stored event still makes it a duplicate
//! (the provider account, with its sealed secret, is loaded from the
//! database). No real provider is called: outbound calls go to a mock.
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;

use forgebase_auth::AuthError;
use forgebase_auth::social::http_client::{BoxFuture, HttpClient, HttpRequest, HttpResponse};
use forgebase_billing::provider::{StripeProvider, stripe};
use forgebase_billing::{
    CustomerInput, Interval, InvoiceStatus, ItemInput, PriceInput, PriceType, ProductInput, ProductKind, ProviderInput,
    SubscribeInput, SubscriptionStatus,
};
use forgebase_core::Config;
use forgebase_data::AccessContext;
use forgebase_entitlements::{FeatureInput, FeatureKind, PlanFeature, PlanInput};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const KEY: &str = "sk_test_51serverscenario20";
const WHSEC: &str = "whsec_server_scenario_20";

/// Answers like Stripe test mode; never leaves the process.
#[derive(Debug, Default)]
struct Mock;

impl HttpClient for Mock {
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, AuthError>> {
        Box::pin(async move {
            let body = if req.url.ends_with("/v1/customers") {
                r#"{"id":"cus_s20"}"#
            } else if req.url.ends_with("/v1/checkout/sessions") {
                r#"{"id":"cs_s20","url":"https://checkout.stripe.test/cs_s20"}"#
            } else {
                r#"{"error":{"message":"unexpected"}}"#
            };
            Ok(HttpResponse {
                status: 200,
                headers: Vec::new(),
                body: body.as_bytes().to_vec(),
            })
        })
    }
}

async fn http(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, String)], body: &str) -> (u16, String) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: forgebase\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status = out.split(' ').nth(1).unwrap().parse().unwrap();
    let body = out
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

fn config(dir: &std::path::Path, billing: bool) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg.billing.enabled = billing;
    cfg
}

fn now_secs() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap()
}

#[tokio::test]
async fn billing_is_on_by_default() {
    let dir = tempfile::tempdir().unwrap();
    // No billing setting, no provider keys: the stock defaults.
    let mut cfg = Config::defaults(dir.path());
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    let server = forgebase_server::start(&cfg).await.unwrap();
    assert!(server.billing().is_some());
    let (s, body) = http(server.local_addr(), "GET", "/api/openapi.json", &[], "").await;
    assert_eq!(s, 200);
    assert!(body.contains("/api/v1/billing/"), "billing is documented by default");
    let (s, _) = http(server.local_addr(), "GET", "/api/v1/billing/settings", &[], "").await;
    assert_eq!(s, 401, "mounted: needs an admin, not 404");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn billing_can_be_turned_off() {
    let dir = tempfile::tempdir().unwrap();
    let server = forgebase_server::start(&config(dir.path(), false)).await.unwrap();
    assert!(server.billing().is_none());
    let (s, body) = http(server.local_addr(), "GET", "/api/openapi.json", &[], "").await;
    assert_eq!(s, 200);
    assert!(!body.contains("/api/v1/billing/"), "billing is not documented when off");
    let (s, _) = http(
        server.local_addr(),
        "POST",
        "/api/v1/billing/webhooks/stripe",
        &[],
        "{}",
    )
    .await;
    assert_eq!(s, 404);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn scenario_20_billing_webhook_is_processed_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path(), true);
    let server = forgebase_server::start(&cfg).await.unwrap();
    let addr = server.local_addr();
    let admin = AccessContext::admin("root");
    let ent = server.entitlements();
    ent.put_feature(
        &admin,
        "projects",
        FeatureInput {
            name: None,
            description: None,
            kind: FeatureKind::Metered,
            unit: None,
            permissions: Vec::new(),
        },
    )
    .await
    .unwrap();
    for (plan, default) in [("free", true), ("pro", false)] {
        ent.put_plan(
            &admin,
            plan,
            PlanInput {
                name: None,
                description: None,
                is_default: default,
                active: true,
                trial_days: 0,
                metadata: None,
                features: vec![PlanFeature {
                    feature: "projects".into(),
                    enabled: true,
                    limit: Some(if default { 3 } else { 100 }),
                    unit: String::new(),
                    period: None,
                    window_secs: None,
                    trial_limit: None,
                }],
            },
        )
        .await
        .unwrap();
    }
    let billing = server.billing().unwrap().clone();
    billing
        .put_provider(
            &admin,
            "stripe",
            ProviderInput {
                api_key: Some(KEY.into()),
                webhook_secret: Some(WHSEC.into()),
                ..ProviderInput::default()
            },
        )
        .await
        .unwrap();
    // Outbound calls of this test go to the mock, never to Stripe.
    billing.register_provider(Arc::new(StripeProvider::new(KEY, WHSEC, None, Arc::new(Mock)).unwrap()));
    billing
        .put_product(
            &admin,
            "pro",
            ProductInput {
                name: "Pro".into(),
                description: None,
                kind: ProductKind::Plan,
                plan_key: Some("pro".into()),
                addon_key: None,
                pack_key: None,
                credits: None,
                active: true,
                metadata: None,
            },
        )
        .await
        .unwrap();
    billing
        .put_price(
            &admin,
            "pro-m",
            PriceInput {
                product: "pro".into(),
                price_type: PriceType::Recurring,
                interval: Some(Interval::Month),
                interval_count: 1,
                scheme: None,
                currency: "USD".into(),
                unit_amount: 2900,
                tiers: None,
                currency_options: Default::default(),
                feature: None,
                meter_basis: None,
                trial_days: 0,
                tax_behavior: None,
                active: true,
                metadata: None,
            },
        )
        .await
        .unwrap();
    let ann = AccessContext::user("users", "ann");
    let subject = "user:users/ann";
    billing
        .put_customer(
            &ann,
            CustomerInput {
                email: Some("ann@example.test".into()),
                ..CustomerInput::default()
            },
        )
        .await
        .unwrap();
    let c = billing
        .subscribe(
            &ann,
            SubscribeInput {
                items: vec![ItemInput::new("pro-m")],
                idempotency_key: "s20".into(),
                ..SubscribeInput::default()
            },
        )
        .await
        .unwrap();
    let sub = c.subscription.unwrap().id;
    let inv = c.invoice.unwrap().id;
    assert_eq!(c.payment_url.as_deref(), Some("https://checkout.stripe.test/cs_s20"));

    let t = now_secs();
    let body = serde_json::json!({
        "id": "evt_s20", "object": "event", "type": "payment_intent.succeeded", "created": t, "livemode": false,
        "data": {"object": {"id": "pi_s20", "amount": 2900, "amount_received": 2900, "currency": "usd",
            "payment_method": "pm_s20", "metadata": {"forgebase_invoice": inv}}}
    })
    .to_string();
    let sig = stripe::sign(WHSEC, t, body.as_bytes());
    let headers = [
        ("Content-Type", "application/json".to_owned()),
        ("Stripe-Signature", sig.clone()),
    ];
    // A forged delivery is refused and changes nothing.
    let forged = [(
        "Stripe-Signature",
        stripe::sign("whsec_not_the_secret", t, body.as_bytes()),
    )];
    let (s, b) = http(addr, "POST", "/api/v1/billing/webhooks/stripe", &forged, &body).await;
    assert_eq!(s, 400, "{b}");
    assert!(b.contains("invalid_signature"));

    let (s, b) = http(addr, "POST", "/api/v1/billing/webhooks/stripe", &headers, &body).await;
    assert_eq!(s, 200, "{b}");
    assert!(b.contains("\"processed\""), "{b}");
    let (s, b) = http(addr, "POST", "/api/v1/billing/webhooks/stripe", &headers, &body).await;
    assert_eq!(s, 200, "{b}");
    assert!(b.contains("\"duplicate\""), "{b}");

    let invoice = billing.invoice(&admin, &inv).await.unwrap();
    assert_eq!((invoice.status, invoice.amount_paid), (InvoiceStatus::Paid, 2900));
    assert_eq!(billing.payments(&admin, Some(subject), None).await.unwrap().len(), 1);
    assert_eq!(
        billing.subscription(&admin, &sub).await.unwrap().status,
        SubscriptionStatus::Active
    );
    assert_eq!(ent.entitlements_of(subject, None).plan.as_deref(), Some("pro"));
    let schedules = server.jobs().schedules(&AccessContext::system()).await.unwrap();
    assert!(
        schedules
            .iter()
            .any(|s| s.name == forgebase_billing::MAINTENANCE_SCHEDULE)
    );
    server.shutdown().await.unwrap();

    // Restart: the event, the payment and the provider account persisted.
    let server = forgebase_server::start(&cfg).await.unwrap();
    let addr = server.local_addr();
    let (s, b) = http(addr, "POST", "/api/v1/billing/webhooks/stripe", &headers, &body).await;
    assert_eq!(s, 200, "{b}");
    assert!(b.contains("\"duplicate\""), "{b}");
    let billing = server.billing().unwrap();
    assert_eq!(billing.payments(&admin, Some(subject), None).await.unwrap().len(), 1);
    let events = billing.webhook_events(&admin, None).await.unwrap();
    assert_eq!(
        (events.len(), events[0].attempts, events[0].status.as_str()),
        (1, 3, "processed")
    );
    assert_eq!(
        server.entitlements().entitlements_of(subject, None).plan.as_deref(),
        Some("pro")
    );
    server.shutdown().await.unwrap();
}
