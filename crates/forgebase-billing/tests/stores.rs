//! FB-72 merchant-of-record stores: App Store, Google Play and Creem against
//! recorded-shape mock APIs (no real network). Signed payloads are made with
//! test-only keys and a test certificate chain (`tests/fixtures/stores`,
//! `gen.sh`); the App Store driver is given the test root instead of Apple's.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use common::*;
use forgebase_auth::AuthError;
use forgebase_auth::social::http_client::{BoxFuture, HttpClient, HttpRequest, HttpResponse};
use forgebase_billing::provider::app_store::{AppStoreConfig, AppStoreProvider};
use forgebase_billing::provider::creem::{self, CreemProvider};
use forgebase_billing::provider::google_play::{GooglePlayConfig, GooglePlayProvider};
use forgebase_billing::provider::jws;
use forgebase_billing::{
    BillingError, InvoiceStatus, OrderStatus, ProviderInput, StoreCheckoutInput, StoreProductInput, StoreVerifyInput,
    SubscriptionStatus,
};
use forgebase_entitlements::Clock as _;
use serde_json::{Value as Json, json};

const F: &str = "tests/fixtures/stores";
const BUNDLE: &str = "com.forgebase.app";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/{F}/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn der(pem: &str) -> Vec<u8> {
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD.decode(body).unwrap()
}

/// A recorded-response mock: answers by `METHOD path?query`, records calls.
#[derive(Debug, Default)]
struct Mock {
    routes: Mutex<HashMap<String, (u16, Json)>>,
    calls: Mutex<Vec<HttpRequest>>,
}

impl Mock {
    fn on(&self, method: &str, path: &str, status: u16, body: Json) {
        self.routes
            .lock()
            .unwrap()
            .insert(format!("{method} {path}"), (status, body));
    }

    fn calls(&self, needle: &str) -> Vec<HttpRequest> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.url.contains(needle))
            .cloned()
            .collect()
    }
}

impl HttpClient for Mock {
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, AuthError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(req.clone());
            let u = url::Url::parse(&req.url).unwrap();
            let key = match u.query() {
                Some(q) => format!("{} {}?{q}", req.method, u.path()),
                None => format!("{} {}", req.method, u.path()),
            };
            let (status, body) = self
                .routes
                .lock()
                .unwrap()
                .get(&key)
                .cloned()
                .unwrap_or((404, json!({"error": {"message": "no such route"}})));
            Ok(HttpResponse {
                status,
                headers: vec![("content-type".into(), "application/json".into())],
                body: body.to_string().into_bytes(),
            })
        })
    }
}

fn code(e: &BillingError) -> &'static str {
    match e {
        BillingError::Invalid { code, .. }
        | BillingError::Conflict { code, .. }
        | BillingError::Webhook { code, .. } => code,
        other => panic!("unexpected error {other:?}"),
    }
}

/// Catalog plus store product mappings.
async fn setup() -> Env {
    let env = Env::new().await;
    env.catalog().await;
    for (store, product, price) in [
        ("app_store", "com.forgebase.pro.monthly", "pro-m"),
        ("app_store", "com.forgebase.credits100", "credits-100"),
        ("google_play", "pro_monthly", "pro-m"),
        ("google_play", "credits_100", "credits-100"),
        ("creem", "prod_pro", "pro-m"),
        ("creem", "prod_credits", "credits-100"),
    ] {
        env.billing
            .put_store_product(&admin(), store, product, StoreProductInput { price: price.into() })
            .await
            .unwrap();
    }
    env
}

// ---------------------------------------------------------------------------
// App Store
// ---------------------------------------------------------------------------

struct Apple {
    mock: Arc<Mock>,
}

fn chain(names: &[&str]) -> Json {
    json!(
        names
            .iter()
            .map(|n| base64::engine::general_purpose::STANDARD.encode(der(&fixture(n))))
            .collect::<Vec<_>>()
    )
}

/// An Apple-style JWS signed by the test leaf with the test chain.
fn apple_jws(payload: &Json) -> String {
    apple_jws_with(payload, &["apple-leaf.pem", "apple-int.pem", "apple-root.pem"])
}

fn apple_jws_with(payload: &Json, certs: &[&str]) -> String {
    let key = der(&fixture("apple-leaf.key"));
    jws::sign_es256(&key, &json!({"alg": "ES256", "x5c": chain(certs)}), payload).unwrap()
}

fn apple_tx(
    txn: &str,
    original: &str,
    product: &str,
    kind: &str,
    start: i64,
    expires: Option<i64>,
    price: i64,
) -> Json {
    let mut t = json!({
        "transactionId": txn, "originalTransactionId": original, "bundleId": BUNDLE, "productId": product,
        "type": kind, "purchaseDate": start, "originalPurchaseDate": start, "quantity": 1,
        "inAppOwnershipType": "PURCHASED", "signedDate": start, "environment": "Sandbox",
        "transactionReason": "PURCHASE", "storefront": "USA", "price": price, "currency": "USD",
    });
    if let Some(e) = expires {
        t["expiresDate"] = json!(e);
    }
    t
}

fn renewal(original: &str, auto: i64, signed: i64) -> Json {
    json!({"originalTransactionId": original, "autoRenewProductId": "com.forgebase.pro.monthly",
           "productId": "com.forgebase.pro.monthly", "autoRenewStatus": auto, "signedDate": signed,
           "environment": "Sandbox"})
}

fn notification(uuid: &str, kind: &str, sub: Option<&str>, tx: &Json, renewal: Option<&Json>, signed: i64) -> Vec<u8> {
    let mut data = json!({"bundleId": BUNDLE, "environment": "Sandbox", "signedTransactionInfo": apple_jws(tx),
                          "status": 1});
    if let Some(r) = renewal {
        data["signedRenewalInfo"] = json!(apple_jws(r));
    }
    let mut n = json!({"notificationType": kind, "notificationUUID": uuid, "data": data, "version": "2.0",
                       "signedDate": signed});
    if let Some(s) = sub {
        n["subtype"] = json!(s);
    }
    json!({"signedPayload": apple_jws(&n)}).to_string().into_bytes()
}

impl Apple {
    fn install(env: &Env) -> Self {
        let mock = Arc::new(Mock::default());
        let cfg = AppStoreConfig {
            issuer_id: "57246542-96fe-1a63-e053-0824d011072a".into(),
            key_id: "2X9R4HXF34".into(),
            private_key: fixture("asc-key.p8"),
            bundle_id: BUNDLE.into(),
            environment: "sandbox".into(),
        };
        let p = AppStoreProvider::new(
            cfg,
            Some("http://apple.test"),
            Some(vec![der(&fixture("apple-root.pem"))]),
            mock.clone(),
            Arc::new(env.clock.clone()),
        )
        .unwrap();
        env.billing.register_store(Arc::new(p));
        Self { mock }
    }

    /// The subscription status the API returns.
    fn status(&self, original: &str, tx: &Json, renewal: &Json, status: i64) {
        self.mock.on(
            "GET",
            &format!("/inApps/v1/subscriptions/{original}"),
            200,
            json!({"environment": "Sandbox", "bundleId": BUNDLE, "data": [{"subscriptionGroupIdentifier": "21000000",
                   "lastTransactions": [{"originalTransactionId": original, "status": status,
                       "signedTransactionInfo": apple_jws(tx), "signedRenewalInfo": apple_jws(renewal)}]}]}),
        );
    }
}

fn verify_jws(signed: &str) -> StoreVerifyInput {
    StoreVerifyInput {
        signed_transaction: Some(signed.into()),
        ..StoreVerifyInput::default()
    }
}

#[tokio::test]
async fn app_store_subscription_purchase_renewal_replay_and_refund() {
    let env = setup().await;
    let apple = Apple::install(&env);
    let month = 30 * DAY;
    let tx1 = apple_tx(
        "2000000001",
        "2000000001",
        "com.forgebase.pro.monthly",
        "Auto-Renewable Subscription",
        T0,
        Some(T0 + month),
        29_990,
    );
    apple.status("2000000001", &tx1, &renewal("2000000001", 1, T0), 1);

    // The app sends the StoreKit 2 signed transaction.
    let rec = env
        .billing
        .verify_store_purchase(&user("ann"), "app_store", verify_jws(&apple_jws(&tx1)))
        .await
        .unwrap();
    assert_eq!(
        (rec.status.as_str(), rec.kind.as_str(), rec.price.as_str()),
        ("active", "subscription", "pro-m")
    );
    assert_eq!(rec.subject, pk("ann"));
    assert!(rec.auto_renew);
    let sub_id = rec.subscription_id.clone().unwrap();
    assert_eq!(env.plan_of(&pk("ann")).0.as_deref(), Some("pro"));
    let invs = env.billing.invoices(&user("ann"), None, None, None).await.unwrap();
    assert_eq!(invs.len(), 1);
    assert_eq!(
        (invs[0].status, invs[0].total, invs[0].currency.as_str()),
        (InvoiceStatus::Paid, 2999, "USD")
    );
    let pays = env.billing.payments(&user("ann"), None, None).await.unwrap();
    assert_eq!(pays[0].provider_ref.as_deref(), Some("2000000001"));

    // The App Store Server API was called with an ES256 bearer for the bundle.
    let call = &apple.mock.calls("/inApps/v1/subscriptions/2000000001")[0];
    let bearer = call
        .headers
        .iter()
        .find(|(k, _)| k == "authorization")
        .unwrap()
        .1
        .clone();
    let token = jws::Jws::parse(bearer.strip_prefix("Bearer ").unwrap()).unwrap();
    assert_eq!(token.header["alg"], "ES256");
    assert_eq!(token.header["kid"], "2X9R4HXF34");
    assert_eq!(
        (token.payload["aud"].as_str(), token.payload["bid"].as_str()),
        (Some("appstoreconnect-v1"), Some(BUNDLE))
    );

    // Verifying again changes nothing.
    env.billing
        .verify_store_purchase(&user("ann"), "app_store", verify_jws(&apple_jws(&tx1)))
        .await
        .unwrap();
    assert_eq!(
        env.billing
            .invoices(&user("ann"), None, None, None)
            .await
            .unwrap()
            .len(),
        1
    );

    // A store subscription is not ForgeBase's to change or renew.
    let e = env
        .billing
        .cancel(&user("ann"), &sub_id, Default::default())
        .await
        .unwrap_err();
    assert_eq!(code(&e), "store_managed");

    // Renewal notification a month later.
    env.clock.advance(month);
    let tx2 = apple_tx(
        "2000000002",
        "2000000001",
        "com.forgebase.pro.monthly",
        "Auto-Renewable Subscription",
        T0 + month,
        Some(T0 + 2 * month),
        29_990,
    );
    let body = notification(
        "6f6d1c3e-0001",
        "DID_RENEW",
        None,
        &tx2,
        Some(&renewal("2000000001", 1, T0 + month)),
        T0 + month,
    );
    let out = env.billing.handle_webhook("app_store", &[], &body).await.unwrap();
    assert_eq!(out.result, "processed", "{out:?}");
    let invs = env.billing.invoices(&user("ann"), None, None, None).await.unwrap();
    assert_eq!(invs.len(), 2);
    assert_eq!(invs[0].billing_reason, "store_renewal");
    let sub = env.billing.subscription(&user("ann"), &sub_id).await.unwrap();
    assert_eq!(
        (sub.status, sub.current_period_end),
        (SubscriptionStatus::Active, T0 + 2 * month)
    );
    // Maintenance does not renew or dun it.
    let rep = env.advance(DAY).await;
    assert_eq!(rep.renewed, 0);

    // The same notification again: duplicate, no effect.
    let out = env.billing.handle_webhook("app_store", &[], &body).await.unwrap();
    assert_eq!(out.result, "duplicate");
    assert_eq!(
        env.billing
            .invoices(&user("ann"), None, None, None)
            .await
            .unwrap()
            .len(),
        2
    );

    // An older notification (auto-renew off, signed before the renewal) arriving late is ignored.
    let late = notification(
        "6f6d1c3e-0000",
        "DID_CHANGE_RENEWAL_STATUS",
        Some("AUTO_RENEW_DISABLED"),
        &tx1,
        Some(&renewal("2000000001", 0, T0 + month - HOUR)),
        T0 + month - HOUR,
    );
    let out = env.billing.handle_webhook("app_store", &[], &late).await.unwrap();
    assert_eq!((out.result.as_str(), out.detail.as_deref()), ("ignored", Some("stale")));
    assert!(
        !env.billing
            .subscription(&user("ann"), &sub_id)
            .await
            .unwrap()
            .cancel_at_period_end
    );

    // Auto-renew turned off now: cancels at the period end.
    let off = notification(
        "6f6d1c3e-0002",
        "DID_CHANGE_RENEWAL_STATUS",
        Some("AUTO_RENEW_DISABLED"),
        &tx2,
        Some(&renewal("2000000001", 0, T0 + month + 2 * DAY)),
        T0 + month + 2 * DAY,
    );
    env.billing.handle_webhook("app_store", &[], &off).await.unwrap();
    assert!(
        env.billing
            .subscription(&user("ann"), &sub_id)
            .await
            .unwrap()
            .cancel_at_period_end
    );

    // Apple refunds the renewal: credit note with an external refund, access revoked.
    let mut refunded = tx2.clone();
    refunded["revocationDate"] = json!(T0 + month + 3 * DAY);
    refunded["revocationReason"] = json!(0);
    refunded["signedDate"] = json!(T0 + month + 3 * DAY);
    let body = notification("6f6d1c3e-0003", "REFUND", None, &refunded, None, T0 + month + 3 * DAY);
    let out = env.billing.handle_webhook("app_store", &[], &body).await.unwrap();
    assert_eq!(
        (out.result.as_str(), out.detail.as_deref()),
        ("processed", Some("refunded")),
        "{out:?}"
    );
    let notes = env.billing.credit_notes(&user("ann"), None, None).await.unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!((notes[0].amount, notes[0].refund_amount), (2999, 2999));
    let refunds = env.billing.refunds(&user("ann"), None, None).await.unwrap();
    assert_eq!(refunds[0].provider_ref.as_deref(), Some("2000000002:refund"));
    assert_eq!(refunds[0].status, forgebase_billing::RefundStatus::Succeeded);
    let sub = env.billing.subscription(&user("ann"), &sub_id).await.unwrap();
    assert_eq!(
        (sub.status, sub.cancel_reason.as_deref()),
        (SubscriptionStatus::Canceled, Some("store_refunded"))
    );
    assert_eq!(
        env.plan_of(&pk("ann")).1,
        Some(forgebase_entitlements::SubscriptionStatus::Canceled)
    );
    // Replaying the refund (another notification id) refunds nothing twice.
    let again = notification("6f6d1c3e-0004", "REFUND", None, &refunded, None, T0 + month + 3 * DAY);
    env.billing.handle_webhook("app_store", &[], &again).await.unwrap();
    assert_eq!(
        env.billing.credit_notes(&user("ann"), None, None).await.unwrap().len(),
        1
    );
    let purchases = env.billing.store_purchases(&user("ann"), None).await.unwrap();
    assert_eq!(purchases[0].status, "refunded");
}

#[tokio::test]
async fn maintenance_reads_back_store_subscriptions_past_their_period() {
    let env = setup().await;
    let apple = Apple::install(&env);
    let month = 30 * DAY;
    let tx = apple_tx(
        "2100000001",
        "2100000001",
        "com.forgebase.pro.monthly",
        "Auto-Renewable Subscription",
        T0,
        Some(T0 + month),
        29_990,
    );
    apple.status("2100000001", &tx, &renewal("2100000001", 1, T0), 1);
    let rec = env
        .billing
        .verify_store_purchase(&user("ann"), "app_store", verify_jws(&apple_jws(&tx)))
        .await
        .unwrap();
    // No notification arrives; the store says the subscription renewed.
    let tx2 = apple_tx(
        "2100000002",
        "2100000001",
        "com.forgebase.pro.monthly",
        "Auto-Renewable Subscription",
        T0 + month,
        Some(T0 + 2 * month),
        29_990,
    );
    apple.status("2100000001", &tx2, &renewal("2100000001", 1, T0 + month), 1);
    env.advance(month + HOUR).await;
    let sub = env
        .billing
        .subscription(&user("ann"), rec.subscription_id.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(
        (sub.status, sub.current_period_end),
        (SubscriptionStatus::Active, T0 + 2 * month)
    );
    assert_eq!(
        env.billing
            .invoices(&user("ann"), None, None, None)
            .await
            .unwrap()
            .len(),
        2
    );
    // Later it expired (status 2) and no notification came either.
    let mut ended = tx2.clone();
    ended["signedDate"] = json!(T0 + 2 * month + HOUR);
    apple.status(
        "2100000001",
        &ended,
        &renewal("2100000001", 0, T0 + 2 * month + HOUR),
        2,
    );
    env.advance(month + 2 * HOUR).await;
    let sub = env
        .billing
        .subscription(&user("ann"), rec.subscription_id.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(
        (sub.status, sub.cancel_reason.as_deref()),
        (SubscriptionStatus::Canceled, Some("store_expired"))
    );
    assert_eq!(
        env.plan_of(&pk("ann")).1,
        Some(forgebase_entitlements::SubscriptionStatus::Canceled)
    );
}

#[tokio::test]
async fn app_store_rejects_forged_foreign_live_and_reused_receipts() {
    let env = setup().await;
    let apple = Apple::install(&env);
    let tx = apple_tx(
        "3000000001",
        "3000000001",
        "com.forgebase.credits100",
        "Consumable",
        T0,
        None,
        9_990,
    );
    let verify = |who: &'static str, signed: String| {
        let b = env.billing.clone();
        async move {
            b.verify_store_purchase(&user(who), "app_store", verify_jws(&signed))
                .await
        }
    };

    // A chain that does not lead to the pinned root.
    let forged = apple_jws_with(&tx, &["forged-leaf.pem", "forged-int.pem", "forged-root.pem"]);
    assert_eq!(code(&verify("ann", forged).await.unwrap_err()), "invalid_signature");
    // A certificate of the right chain without Apple's receipt-signing marker.
    let rogue = apple_jws_with(&tx, &["apple-rogue-leaf.pem", "apple-int.pem", "apple-root.pem"]);
    assert_eq!(code(&verify("ann", rogue).await.unwrap_err()), "invalid_signature");
    // A genuine signature over another payload.
    let good = apple_jws(&tx);
    let mut parts: Vec<&str> = good.split('.').collect();
    let mut richer = tx.clone();
    richer["quantity"] = json!(50);
    let payload = jws::b64url(richer.to_string().as_bytes());
    parts[1] = &payload;
    assert_eq!(
        code(&verify("ann", parts.join(".")).await.unwrap_err()),
        "invalid_signature"
    );
    // No chain at all.
    let bare = jws::sign_es256(&der(&fixture("apple-leaf.key")), &json!({"alg": "ES256"}), &tx).unwrap();
    assert_eq!(code(&verify("ann", bare).await.unwrap_err()), "invalid_signature");
    // Another app's transaction, a production transaction.
    let mut other = tx.clone();
    other["bundleId"] = json!("com.someone.else");
    assert_eq!(code(&verify("ann", apple_jws(&other)).await.unwrap_err()), "wrong_app");
    let mut live = tx.clone();
    live["environment"] = json!("Production");
    assert_eq!(
        code(&verify("ann", apple_jws(&live)).await.unwrap_err()),
        "live_mode_refused"
    );
    // An unmapped product.
    let mut unknown = tx.clone();
    unknown["productId"] = json!("com.forgebase.gems");
    assert_eq!(
        code(&verify("ann", apple_jws(&unknown)).await.unwrap_err()),
        "unknown_store_product"
    );
    assert_eq!(env.count("SELECT COUNT(*) FROM _forgebase_billing_invoices").await, 0);

    // The genuine one works once per account: bob cannot redeem ann's receipt.
    verify("ann", apple_jws(&tx)).await.unwrap();
    let e = verify("bob", apple_jws(&tx)).await.unwrap_err();
    assert_eq!(code(&e), "purchase_owned");
    assert_eq!(env.count("SELECT COUNT(*) FROM _forgebase_billing_invoices").await, 1);

    // Notifications: forged chain refused before anything is recorded.
    let n = json!({"notificationType": "ONE_TIME_CHARGE", "notificationUUID": "u-1", "signedDate": T0,
                   "data": {"bundleId": BUNDLE, "environment": "Sandbox", "signedTransactionInfo": apple_jws(&tx)}});
    let body = json!({"signedPayload": apple_jws_with(&n, &["forged-leaf.pem", "forged-int.pem", "forged-root.pem"])});
    let e = env
        .billing
        .handle_webhook("app_store", &[], body.to_string().as_bytes())
        .await
        .unwrap_err();
    assert_eq!(code(&e), "invalid_signature");
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_billing_webhook_events")
            .await,
        0
    );
    // A test notification is recorded and ignored.
    let ping = json!({"notificationType": "TEST", "notificationUUID": "u-test", "signedDate": T0, "data": {"bundleId": BUNDLE}});
    let out = env
        .billing
        .handle_webhook(
            "app_store",
            &[],
            json!({"signedPayload": apple_jws(&ping)}).to_string().as_bytes(),
        )
        .await
        .unwrap();
    assert_eq!(
        (out.result.as_str(), out.detail.as_deref()),
        ("ignored", Some("test_notification"))
    );

    // A transaction id is looked up with the API; an unknown one is refused.
    let e = env
        .billing
        .verify_store_purchase(
            &user("ann"),
            "app_store",
            StoreVerifyInput {
                transaction_id: Some("999".into()),
                ..StoreVerifyInput::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(code(&e), "unknown_transaction");
    apple.mock.on(
        "GET",
        "/inApps/v1/transactions/3000000001",
        200,
        json!({"signedTransactionInfo": apple_jws(&tx)}),
    );
    let rec = env
        .billing
        .verify_store_purchase(
            &user("ann"),
            "app_store",
            StoreVerifyInput {
                transaction_id: Some("3000000001".into()),
                ..StoreVerifyInput::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rec.kind, "one_time");
}

#[tokio::test]
async fn app_store_consumable_tops_up_credits_and_refund_takes_them_back() {
    let env = setup().await;
    let _apple = Apple::install(&env);
    let tx = apple_tx(
        "3000000009",
        "3000000009",
        "com.forgebase.credits100",
        "Consumable",
        T0,
        None,
        9_990,
    );
    let rec = env
        .billing
        .verify_store_purchase(&user("ann"), "app_store", verify_jws(&apple_jws(&tx)))
        .await
        .unwrap();
    let order = env
        .billing
        .order(&user("ann"), rec.order_id.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(order.status, OrderStatus::Paid);
    assert_eq!(
        env.credits.balance(&sys(), Some(&pk("ann"))).await.unwrap().balance,
        100
    );

    let mut refunded = tx.clone();
    refunded["revocationDate"] = json!(T0 + HOUR);
    refunded["signedDate"] = json!(T0 + HOUR);
    let body = notification("c-1", "REFUND", None, &refunded, None, T0 + HOUR);
    env.clock.advance(HOUR);
    let out = env.billing.handle_webhook("app_store", &[], &body).await.unwrap();
    assert_eq!(out.detail.as_deref(), Some("refunded"), "{out:?}");
    assert_eq!(env.credits.balance(&sys(), Some(&pk("ann"))).await.unwrap().balance, 0);
    let order = env
        .billing
        .order(&user("ann"), rec.order_id.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(order.status, OrderStatus::Refunded);
}

#[tokio::test]
async fn store_accounts_are_sealed_and_test_mode_only() {
    let env = setup().await;
    let put = |p: &'static str, input: ProviderInput| {
        let b = env.billing.clone();
        async move { b.put_provider(&admin(), p, input).await }
    };
    let apple = |environment: &str| ProviderInput {
        config: Some(
            json!({"issuer_id": "iss", "key_id": "KID123", "private_key": fixture("asc-key.p8"),
                            "bundle_id": BUNDLE, "environment": environment}),
        ),
        ..ProviderInput::default()
    };
    assert_eq!(
        code(&put("app_store", apple("production")).await.unwrap_err()),
        "live_mode_refused"
    );
    let mut prod_api = apple("sandbox");
    prod_api.api_base = Some("https://api.storekit.itunes.apple.com".into());
    assert_eq!(
        code(&put("app_store", prod_api).await.unwrap_err()),
        "live_mode_refused"
    );
    let acct = put("app_store", apple("sandbox")).await.unwrap();
    assert_eq!(acct.webhook_path, "/api/v1/billing/webhooks/app_store");
    assert!(env.billing.store("app_store").is_some());
    // The config is sealed with the secrets.
    assert_eq!(
        env.count(
            "SELECT COUNT(*) FROM _forgebase_billing_providers WHERE secret LIKE '%PRIVATE%' OR secret LIKE '%bundle%'"
        )
        .await,
        0
    );

    let creem = |key: &str| ProviderInput {
        api_key: Some(key.into()),
        webhook_secret: Some("whsec_creem_test_1".into()),
        ..ProviderInput::default()
    };
    assert_eq!(
        code(&put("creem", creem("creem_7a8b9c0d1e2f3a4b")).await.unwrap_err()),
        "live_mode_refused"
    );
    put("creem", creem("creem_test_7a8b9c0d1e2f3a4b")).await.unwrap();

    let google = ProviderInput {
        config: Some(
            json!({"package_name": BUNDLE, "push_audience": "https://forgebase.test/hook",
            "service_account": {"client_email": "play@forgebase-test.iam.gserviceaccount.com",
                                "private_key": fixture("google-sa.key")}}),
        ),
        ..ProviderInput::default()
    };
    put("google_play", google).await.unwrap();
    let listed: Vec<String> = env
        .billing
        .providers(&admin())
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.provider)
        .collect();
    assert_eq!(listed, vec!["app_store", "creem", "google_play"]);

    // Store products map to recurring or one-time prices only.
    let e = env
        .billing
        .put_store_product(&admin(), "app_store", "x", StoreProductInput { price: "nope".into() })
        .await
        .unwrap_err();
    assert_eq!(code(&e), "unknown_price");
    let e = env
        .billing
        .put_store_product(
            &user("ann"),
            "app_store",
            "x",
            StoreProductInput { price: "pro-m".into() },
        )
        .await
        .unwrap_err();
    assert!(matches!(e, BillingError::Forbidden(_)), "{e:?}");
    assert_eq!(env.billing.store_products(&user("ann")).await.unwrap().len(), 6);
}

// ---------------------------------------------------------------------------
// Google Play
// ---------------------------------------------------------------------------

const AUDIENCE: &str = "https://forgebase.test/api/v1/billing/webhooks/google_play";
const PUSHER: &str = "pubsub-push@forgebase-test.iam.gserviceaccount.com";

struct Google {
    mock: Arc<Mock>,
}

/// `(n, e)` of a PKCS#1 RSAPublicKey (two short-form DER integers inside a sequence).
fn rsa_ne(pkcs1: &[u8]) -> (Vec<u8>, Vec<u8>) {
    fn tlv(b: &[u8]) -> (&[u8], &[u8]) {
        let (len, hdr) = match b[1] {
            l if l < 0x80 => (usize::from(l), 2),
            l => {
                let n = usize::from(l & 0x7f);
                (
                    b[2..2 + n].iter().fold(0usize, |a, x| (a << 8) | usize::from(*x)),
                    2 + n,
                )
            }
        };
        (&b[hdr..hdr + len], &b[hdr + len..])
    }
    let (seq, _) = tlv(pkcs1);
    let (n, rest) = tlv(seq);
    let (e, _) = tlv(rest);
    let strip = |x: &[u8]| x.iter().skip_while(|b| **b == 0).copied().collect::<Vec<u8>>();
    (strip(n), strip(e))
}

impl Google {
    fn install(env: &Env) -> Self {
        let mock = Arc::new(Mock::default());
        mock.on(
            "POST",
            "/token",
            200,
            json!({"access_token": "ya29.test-token", "expires_in": 3599, "token_type": "Bearer"}),
        );
        let signer = der(&fixture("google-signer.key"));
        let (n, e) = rsa_ne(&jws::rsa_public(&signer).unwrap());
        mock.on(
            "GET",
            "/oauth2/v3/certs",
            200,
            json!({"keys": [{"kty": "RSA", "alg": "RS256", "use": "sig", "kid": "k1", "n": jws::b64url(&n), "e": jws::b64url(&e)}]}),
        );
        let cfg = GooglePlayConfig {
            package_name: BUNDLE.into(),
            client_email: "play@forgebase-test.iam.gserviceaccount.com".into(),
            private_key: fixture("google-sa.key"),
            token_uri: "http://oauth.test/token".into(),
            push_audience: AUDIENCE.into(),
            push_service_account: Some(PUSHER.into()),
            certs_uri: "http://certs.test/oauth2/v3/certs".into(),
        };
        let p =
            GooglePlayProvider::new(cfg, Some("http://play.test"), mock.clone(), Arc::new(env.clock.clone())).unwrap();
        env.billing.register_store(Arc::new(p));
        Self { mock }
    }

    fn subscription(&self, token: &str, state: &str, order: &str, expiry: &str, test: bool) {
        let mut v = json!({
            "kind": "androidpublisher#subscriptionPurchaseV2", "regionCode": "US",
            "startTime": "2026-11-01T00:00:00.120Z", "subscriptionState": state, "latestOrderId": order,
            "acknowledgementState": "ACKNOWLEDGEMENT_STATE_PENDING",
            "lineItems": [{"productId": "pro_monthly", "expiryTime": expiry,
                "autoRenewingPlan": {"autoRenewEnabled": true, "recurringPrice": {"currencyCode": "USD", "units": "29", "nanos": 990_000_000}},
                "offerDetails": {"basePlanId": "monthly"}}],
        });
        if test {
            v["testPurchase"] = json!({});
        }
        self.mock.on(
            "GET",
            &format!("/androidpublisher/v3/applications/{BUNDLE}/purchases/subscriptionsv2/tokens/{token}"),
            200,
            v,
        );
        self.mock.on(
            "POST",
            &format!("/androidpublisher/v3/applications/{BUNDLE}/purchases/subscriptions/pro_monthly/tokens/{token}:acknowledge"),
            200,
            json!({}),
        );
    }
}

/// A Pub/Sub push: the OIDC token and the body.
fn push(env: &Env, key: &str, claims: Json, message_id: &str, data: &Json) -> (Vec<(String, String)>, Vec<u8>) {
    let now = env.clock.now_ms() / 1000;
    let mut c = json!({"iss": "https://accounts.google.com", "aud": AUDIENCE, "email": PUSHER, "email_verified": true,
                       "iat": now, "exp": now + 3600, "sub": "1234567890"});
    for (k, v) in claims.as_object().unwrap() {
        c[k] = v.clone();
    }
    let token = jws::sign_rs256(
        &der(&fixture(key)),
        &json!({"alg": "RS256", "kid": "k1", "typ": "JWT"}),
        &c,
    )
    .unwrap();
    let body = json!({"message": {"data": base64::engine::general_purpose::STANDARD.encode(data.to_string()),
                                  "messageId": message_id, "publishTime": "2026-11-01T00:00:00Z"},
                      "subscription": "projects/forgebase-test/subscriptions/play-rtdn"});
    (
        vec![("authorization".into(), format!("Bearer {token}"))],
        body.to_string().into_bytes(),
    )
}

fn rtdn(kind: i64, token: &str, at: i64) -> Json {
    json!({"version": "1.0", "packageName": BUNDLE, "eventTimeMillis": at.to_string(),
           "subscriptionNotification": {"version": "1.0", "notificationType": kind, "purchaseToken": token,
                                         "subscriptionId": "pro_monthly"}})
}

#[tokio::test]
async fn google_play_subscription_verify_rtdn_renewal_and_revocation() {
    let env = setup().await;
    let g = Google::install(&env);
    g.subscription(
        "tok-1",
        "SUBSCRIPTION_STATE_ACTIVE",
        "GPA.3344-5566-7788-99001",
        "2026-12-01T00:00:00Z",
        true,
    );
    let rec = env
        .billing
        .verify_store_purchase(
            &user("ann"),
            "google_play",
            StoreVerifyInput {
                purchase_token: Some("tok-1".into()),
                product_id: Some("pro_monthly".into()),
                ..StoreVerifyInput::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rec.status, "active");
    assert_eq!(env.plan_of(&pk("ann")).0.as_deref(), Some("pro"));
    let invs = env.billing.invoices(&user("ann"), None, None, None).await.unwrap();
    assert_eq!((invs.len(), invs[0].total), (1, 2999));
    // The purchase was acknowledged, with a service-account token.
    assert_eq!(g.mock.calls(":acknowledge").len(), 1);
    let token_call = &g.mock.calls("oauth.test/token")[0];
    let assertion = url::form_urlencoded::parse(&token_call.body)
        .find(|(k, _)| k == "assertion")
        .unwrap()
        .1
        .into_owned();
    let a = jws::Jws::parse(&assertion).unwrap();
    let (n, e) = rsa_ne(&jws::rsa_public(&der(&fixture("google-sa.key"))).unwrap());
    a.verify_rs256(&n, &e).unwrap();
    assert_eq!(a.payload["scope"], "https://www.googleapis.com/auth/androidpublisher");
    assert!(
        g.mock.calls("subscriptionsv2")[0]
            .headers
            .contains(&("authorization".into(), "Bearer ya29.test-token".into()))
    );

    // SUBSCRIPTION_RENEWED: the state is read back from the API.
    env.clock.advance(30 * DAY);
    g.subscription(
        "tok-1",
        "SUBSCRIPTION_STATE_ACTIVE",
        "GPA.3344-5566-7788-99001..0",
        "2027-01-01T00:00:00Z",
        true,
    );
    let (h, body) = push(
        &env,
        "google-signer.key",
        json!({}),
        "msg-1",
        &rtdn(2, "tok-1", T0 + 30 * DAY),
    );
    let out = env.billing.handle_webhook("google_play", &h, &body).await.unwrap();
    assert_eq!(out.result, "processed", "{out:?}");
    assert_eq!(
        env.billing
            .invoices(&user("ann"), None, None, None)
            .await
            .unwrap()
            .len(),
        2
    );
    // Pub/Sub redelivers the same message: duplicate.
    let out = env.billing.handle_webhook("google_play", &h, &body).await.unwrap();
    assert_eq!(out.result, "duplicate");
    // The access token is reused while valid: one before, one after 30 days.
    assert_eq!(g.mock.calls("oauth.test/token").len(), 2);

    // SUBSCRIPTION_REVOKED: refunded and ended at once.
    g.subscription(
        "tok-1",
        "SUBSCRIPTION_STATE_EXPIRED",
        "GPA.3344-5566-7788-99001..0",
        "2026-12-02T00:00:00Z",
        true,
    );
    let (h, body) = push(
        &env,
        "google-signer.key",
        json!({}),
        "msg-2",
        &rtdn(12, "tok-1", T0 + 30 * DAY + HOUR),
    );
    let out = env.billing.handle_webhook("google_play", &h, &body).await.unwrap();
    assert_eq!(out.detail.as_deref(), Some("refunded"), "{out:?}");
    let sub = env
        .billing
        .subscription(&user("ann"), rec.subscription_id.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(
        (sub.status, sub.cancel_reason.as_deref()),
        (SubscriptionStatus::Canceled, Some("store_revoked"))
    );
    assert_eq!(
        env.plan_of(&pk("ann")).1,
        Some(forgebase_entitlements::SubscriptionStatus::Canceled)
    );
    assert_eq!(
        env.billing.credit_notes(&user("ann"), None, None).await.unwrap()[0].amount,
        2999
    );
}

#[tokio::test]
async fn google_play_rejects_forged_pushes_foreign_apps_and_real_purchases() {
    let env = setup().await;
    let g = Google::install(&env);
    g.subscription(
        "tok-9",
        "SUBSCRIPTION_STATE_ACTIVE",
        "GPA.1",
        "2026-12-01T00:00:00Z",
        true,
    );
    let data = rtdn(4, "tok-9", T0);
    let deliver = |h: Vec<(String, String)>, b: Vec<u8>| {
        let bill = env.billing.clone();
        async move { bill.handle_webhook("google_play", &h, &b).await }
    };
    // No token.
    let (_, body) = push(&env, "google-signer.key", json!({}), "m-1", &data);
    assert_eq!(
        code(&deliver(Vec::new(), body.clone()).await.unwrap_err()),
        "invalid_signature"
    );
    // Signed by a key that is not Google's.
    let (h, body) = push(&env, "google-sa.key", json!({}), "m-2", &data);
    assert_eq!(code(&deliver(h, body).await.unwrap_err()), "invalid_signature");
    // Another audience, issuer or push account; an expired token.
    for (claims, want) in [
        (json!({"aud": "https://elsewhere.test"}), "invalid_signature"),
        (json!({"iss": "https://evil.test"}), "invalid_signature"),
        (json!({"email": "someone@evil.test"}), "invalid_signature"),
        (json!({"exp": T0 / 1000 - 10}), "stale_signature"),
    ] {
        let (h, body) = push(&env, "google-signer.key", claims, "m-3", &data);
        assert_eq!(code(&deliver(h, body).await.unwrap_err()), want);
    }
    // A tampered body under a valid token is still authenticated by Pub/Sub's
    // token only; another app's package is refused.
    let mut foreign = data.clone();
    foreign["packageName"] = json!("com.other.app");
    let (h, body) = push(&env, "google-signer.key", json!({}), "m-4", &foreign);
    assert_eq!(code(&deliver(h, body).await.unwrap_err()), "wrong_app");
    assert_eq!(env.count("SELECT COUNT(*) FROM _forgebase_billing_invoices").await, 0);

    // A notification for a purchase no account claimed yet is recorded and ignored.
    let (h, body) = push(&env, "google-signer.key", json!({}), "m-5", &data);
    let out = deliver(h, body).await.unwrap();
    assert_eq!(
        (out.result.as_str(), out.detail.as_deref()),
        ("ignored", Some("unknown_purchase"))
    );

    // A real (non-test) purchase is refused; an unknown token too.
    g.subscription(
        "tok-live",
        "SUBSCRIPTION_STATE_ACTIVE",
        "GPA.2",
        "2026-12-01T00:00:00Z",
        false,
    );
    let verify = |token: &'static str| {
        let b = env.billing.clone();
        async move {
            b.verify_store_purchase(
                &user("ann"),
                "google_play",
                StoreVerifyInput {
                    purchase_token: Some(token.into()),
                    product_id: Some("pro_monthly".into()),
                    ..StoreVerifyInput::default()
                },
            )
            .await
        }
    };
    assert_eq!(code(&verify("tok-live").await.unwrap_err()), "live_mode_refused");
    assert_eq!(code(&verify("tok-unknown").await.unwrap_err()), "unknown_purchase");
    assert_eq!(env.count("SELECT COUNT(*) FROM _forgebase_billing_invoices").await, 0);
}

#[tokio::test]
async fn google_play_one_time_product_and_voided_purchase() {
    let env = setup().await;
    let g = Google::install(&env);
    g.mock.on(
        "GET",
        &format!("/androidpublisher/v3/applications/{BUNDLE}/purchases/products/credits_100/tokens/otp-1"),
        200,
        json!({"kind": "androidpublisher#productPurchase", "purchaseTimeMillis": T0.to_string(), "purchaseState": 0,
               "consumptionState": 0, "orderId": "GPA.7777-0001", "purchaseType": 0, "acknowledgementState": 0,
               "quantity": 1, "regionCode": "US"}),
    );
    g.mock.on(
        "POST",
        &format!("/androidpublisher/v3/applications/{BUNDLE}/purchases/products/credits_100/tokens/otp-1:acknowledge"),
        200,
        json!({}),
    );
    let rec = env
        .billing
        .verify_store_purchase(
            &user("ann"),
            "google_play",
            StoreVerifyInput {
                purchase_token: Some("otp-1".into()),
                product_id: Some("credits_100".into()),
                kind: Some("one_time".into()),
                ..StoreVerifyInput::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(rec.kind, "one_time");
    assert_eq!(
        env.credits.balance(&sys(), Some(&pk("ann"))).await.unwrap().balance,
        100
    );
    assert_eq!(g.mock.calls("otp-1:acknowledge").len(), 1);
    // Catalog price (Play does not report it on the purchase).
    assert_eq!(
        env.billing.invoices(&user("ann"), None, None, None).await.unwrap()[0].total,
        900
    );

    let voided = json!({"version": "1.0", "packageName": BUNDLE, "eventTimeMillis": (T0 + HOUR).to_string(),
        "voidedPurchaseNotification": {"purchaseToken": "otp-1", "orderId": "GPA.7777-0001", "productType": 2, "refundType": 1}});
    env.clock.advance(HOUR);
    let (h, body) = push(&env, "google-signer.key", json!({}), "v-1", &voided);
    let out = env.billing.handle_webhook("google_play", &h, &body).await.unwrap();
    assert_eq!(out.detail.as_deref(), Some("refunded"), "{out:?}");
    assert_eq!(env.credits.balance(&sys(), Some(&pk("ann"))).await.unwrap().balance, 0);
}

// ---------------------------------------------------------------------------
// Creem
// ---------------------------------------------------------------------------

const CREEM_KEY: &str = "creem_test_3d5f7a9b1c2e4f6a";
const CREEM_WHSEC: &str = "whsec_creem_forgebase_test";

fn creem_event(id: &str, kind: &str, created: i64, object: Json) -> Vec<u8> {
    json!({"id": id, "eventType": kind, "created_at": created, "object": object})
        .to_string()
        .into_bytes()
}

fn creem_sub(id: &str, status: &str, start: &str, end: &str, txn: &str) -> Json {
    json!({"id": id, "object": "subscription", "product": {"id": "prod_pro", "price": 2900, "currency": "USD",
           "billing_type": "recurring", "billing_period": "every-month"},
           "customer": {"id": "cust_1", "email": "ann@example.test"}, "collection_method": "charge_automatically",
           "status": status, "last_transaction_id": txn, "current_period_start_date": start,
           "current_period_end_date": end, "canceled_at": null, "mode": "test",
           "metadata": {"forgebase_subject": pk("ann")}})
}

async fn creem_hook(env: &Env, body: &[u8]) -> forgebase_billing::Result<forgebase_billing::WebhookOutcome> {
    let h = vec![("creem-signature".to_owned(), creem::sign(CREEM_WHSEC, body))];
    env.billing.handle_webhook("creem", &h, body).await
}

#[tokio::test]
async fn creem_checkout_webhooks_renewal_refund_and_cancel() {
    let env = setup().await;
    let mock = Arc::new(Mock::default());
    mock.on(
        "POST",
        "/v1/checkouts",
        200,
        json!({"id": "ch_1", "object": "checkout", "status": "pending", "checkout_url": "https://creem.test/checkout/ch_1",
               "mode": "test"}),
    );
    let p = CreemProvider::new(
        CREEM_KEY,
        CREEM_WHSEC,
        Some("http://creem.test"),
        mock.clone(),
        Arc::new(env.clock.clone()),
    )
    .unwrap();
    env.billing.register_store(Arc::new(p));

    let page = env
        .billing
        .store_checkout(
            &user("ann"),
            "creem",
            StoreCheckoutInput {
                subject: None,
                price: "pro-m".into(),
                success_url: "https://app.test/billing/done".into(),
                idempotency_key: "buy-pro-1".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(page.url, "https://creem.test/checkout/ch_1");
    let req: Json = serde_json::from_slice(&mock.calls("/v1/checkouts")[0].body).unwrap();
    assert_eq!(req["product_id"], "prod_pro");
    assert_eq!(req["metadata"]["forgebase_subject"], pk("ann"));
    assert!(
        mock.calls("/v1/checkouts")[0]
            .headers
            .contains(&("x-api-key".into(), CREEM_KEY.into()))
    );

    // checkout.completed: the subject comes back in the signed metadata.
    let checkout = json!({"id": "ch_1", "object": "checkout", "status": "completed", "mode": "test",
        "order": {"id": "ord_1", "transaction": "tran_1", "amount": 2900, "currency": "USD", "status": "paid", "type": "recurring"},
        "product": {"id": "prod_pro"}, "customer": {"id": "cust_1", "email": "ann@example.test"},
        "subscription": creem_sub("sub_1", "active", "2026-11-01T00:00:00Z", "2026-12-01T00:00:00Z", "tran_1"),
        "metadata": {"forgebase_subject": pk("ann")}});
    let body = creem_event("evt_1", "checkout.completed", T0, checkout.clone());
    let out = creem_hook(&env, &body).await.unwrap();
    assert_eq!(out.result, "processed", "{out:?}");
    assert_eq!(env.plan_of(&pk("ann")).0.as_deref(), Some("pro"));
    let invs = env.billing.invoices(&user("ann"), None, None, None).await.unwrap();
    assert_eq!((invs.len(), invs[0].total), (1, 2900));
    // Replay and forgery.
    assert_eq!(creem_hook(&env, &body).await.unwrap().result, "duplicate");
    let forged = vec![("creem-signature".to_owned(), creem::sign("whsec_guess", &body))];
    let e = env.billing.handle_webhook("creem", &forged, &body).await.unwrap_err();
    assert_eq!(code(&e), "invalid_signature");
    let e = env.billing.handle_webhook("creem", &[], &body).await.unwrap_err();
    assert_eq!(code(&e), "invalid_signature");

    // subscription.active for the same first payment raises no second invoice.
    let body = creem_event(
        "evt_2",
        "subscription.active",
        T0 + MIN,
        creem_sub(
            "sub_1",
            "active",
            "2026-11-01T00:00:00Z",
            "2026-12-01T00:00:00Z",
            "tran_1",
        ),
    );
    creem_hook(&env, &body).await.unwrap();
    assert_eq!(
        env.billing
            .invoices(&user("ann"), None, None, None)
            .await
            .unwrap()
            .len(),
        1
    );

    // Renewal: subscription.paid with a new transaction.
    env.clock.advance(30 * DAY);
    let renewed = creem_sub(
        "sub_1",
        "active",
        "2026-12-01T00:00:00Z",
        "2027-01-01T00:00:00Z",
        "tran_2",
    );
    let out = creem_hook(&env, &creem_event("evt_3", "subscription.paid", T0 + 30 * DAY, renewed))
        .await
        .unwrap();
    assert_eq!(out.result, "processed");
    let invs = env.billing.invoices(&user("ann"), None, None, None).await.unwrap();
    assert_eq!(invs.len(), 2);

    // A refund of the renewal: credit note, the subscription stays (Creem
    // sends the cancellation as its own event).
    let refund = json!({"id": "ref_1", "object": "refund", "status": "succeeded", "refund_amount": 2900,
        "refund_currency": "USD", "reason": "requested_by_customer", "transaction": {"id": "tran_2", "amount": 2900},
        "subscription": {"id": "sub_1"}, "order": {"id": "ord_2"}, "mode": "test"});
    let out = creem_hook(
        &env,
        &creem_event("evt_4", "refund.created", T0 + 30 * DAY + HOUR, refund),
    )
    .await
    .unwrap();
    assert_eq!(out.detail.as_deref(), Some("refunded"), "{out:?}");
    let notes = env.billing.credit_notes(&user("ann"), None, None).await.unwrap();
    assert_eq!((notes.len(), notes[0].refund_amount), (1, 2900));
    assert_eq!(env.plan_of(&pk("ann")).0.as_deref(), Some("pro"));

    // subscription.canceled ends it.
    let canceled = creem_sub(
        "sub_1",
        "canceled",
        "2026-12-01T00:00:00Z",
        "2027-01-01T00:00:00Z",
        "tran_2",
    );
    creem_hook(
        &env,
        &creem_event("evt_5", "subscription.canceled", T0 + 30 * DAY + 2 * HOUR, canceled),
    )
    .await
    .unwrap();
    let rec = &env.billing.store_purchases(&user("ann"), None).await.unwrap()[0];
    assert_eq!(rec.status, "expired");
    let sub = env
        .billing
        .subscription(&user("ann"), rec.subscription_id.as_deref().unwrap())
        .await
        .unwrap();
    assert_eq!(sub.status, SubscriptionStatus::Canceled);

    // Server-side verification of the success redirect, bound to ann.
    mock.on("GET", "/v1/checkouts?checkout_id=ch_1", 200, checkout);
    let verify = |who: &'static str| {
        let b = env.billing.clone();
        async move {
            b.verify_store_purchase(
                &user(who),
                "creem",
                StoreVerifyInput {
                    checkout_id: Some("ch_1".into()),
                    ..StoreVerifyInput::default()
                },
            )
            .await
        }
    };
    assert_eq!(verify("ann").await.unwrap().purchase_id, "sub_1");
    assert_eq!(code(&verify("bob").await.unwrap_err()), "purchase_owned");
}

#[tokio::test]
async fn store_account_config_merges_and_reads_back_without_secrets() {
    let env = setup().await;
    let b = &env.billing;
    let apple = |config: serde_json::Value, clear: &[&str]| ProviderInput {
        config: Some(config),
        clear: clear.iter().map(|c| (*c).to_owned()).collect(),
        ..ProviderInput::default()
    };
    let acct = b
        .put_provider(
            &admin(),
            "app_store",
            apple(
                json!({"issuer_id": "iss", "key_id": "KID123", "private_key": fixture("asc-key.p8"),
                       "bundle_id": BUNDLE, "environment": "xcode"}),
                &[],
            ),
        )
        .await
        .unwrap();
    assert!(acct.test_mode);
    assert_eq!(acct.environment, "xcode");
    assert_eq!(acct.config["bundle_id"], BUNDLE);
    assert_eq!(acct.secrets_set, vec!["config.private_key"]);
    let shown = serde_json::to_string(&b.providers(&admin()).await.unwrap()).unwrap();
    assert!(
        !shown.contains("PRIVATE") && !shown.contains("private_key\":"),
        "{shown}"
    );

    // Only the changed field is sent; blanks and the missing private key keep
    // the stored values, so the driver still builds.
    let acct = b
        .put_provider(
            &admin(),
            "app_store",
            apple(
                json!({"issuer_id": "iss2", "bundle_id": "", "private_key": "", "environment": "sandbox"}),
                &[],
            ),
        )
        .await
        .unwrap();
    assert_eq!(acct.config["issuer_id"], "iss2");
    assert_eq!(acct.config["bundle_id"], BUNDLE);
    assert_eq!(acct.config["key_id"], "KID123");
    assert_eq!(acct.environment, "sandbox");
    assert_eq!(acct.secrets_set, vec!["config.private_key"]);
    assert!(b.store("app_store").is_some());

    // A config field is cleared by name (a required one cannot be).
    let e = b
        .put_provider(&admin(), "app_store", apple(json!({}), &["config.issuer_id"]))
        .await
        .unwrap_err();
    assert_eq!(code(&e), "invalid_config");
    let acct = b
        .put_provider(&admin(), "app_store", apple(json!({}), &["config.environment"]))
        .await
        .unwrap();
    assert!(acct.config.get("environment").is_none());
    assert_eq!(acct.environment, "sandbox");
    assert_eq!(acct.config["key_id"], "KID123");
}
