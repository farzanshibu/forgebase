//! FB-72 WeChat Pay API v3 against a mock that signs like WeChat Pay with a
//! test platform key: signed requests, verified responses and notifications,
//! AES-256-GCM resources, platform key rotation by serial, refunds.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use common::*;
use forgebase_auth::AuthError;
use forgebase_auth::social::http_client::{BoxFuture, HttpClient, HttpRequest, HttpResponse};
use forgebase_billing::provider::wechat_pay::{self, WechatPayConfig, WechatPayProvider};
use forgebase_billing::provider::{HostedPaymentRequest, jws};
use forgebase_billing::{
    BillingError, CreditNoteInput, PaymentProvider as _, ProviderInput, RefundStatus, SubscriptionStatus,
};
use forgebase_entitlements::{Clock as _, ManualClock};
use serde_json::{Value as Json, json};

const F: &str = "tests/fixtures/stores";
const MCHID: &str = "1900000109";
const APIV3: &str = "forgebaseTestApiV3Key0123456789a";
const CERT_SERIAL: &str = "5157F09EFDC096DE15EBE81A47057A7232F1B8E1";
const PUBKEY_SERIAL: &str = "PUB_KEY_ID_0119000001092026110100";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{}/{F}/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn der(pem: &str) -> Vec<u8> {
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD.decode(body).unwrap()
}

/// Wechatpay-* headers over `ts\nnonce\nbody\n` with a platform key.
fn platform_headers(key: &str, serial: &str, ts: i64, body: &[u8]) -> Vec<(String, String)> {
    let nonce = "593BEC0C930BF1AFEB40B4A08C8FB242";
    let msg = format!("{ts}\n{nonce}\n{}\n", String::from_utf8_lossy(body));
    vec![
        ("wechatpay-timestamp".into(), ts.to_string()),
        ("wechatpay-nonce".into(), nonce.into()),
        (
            "wechatpay-signature".into(),
            wechat_pay::sign_message(&fixture(key), &msg).unwrap(),
        ),
        ("wechatpay-serial".into(), serial.into()),
    ]
}

#[derive(Debug)]
struct Mock {
    clock: ManualClock,
    routes: Mutex<HashMap<String, (u16, Json)>>,
    calls: Mutex<Vec<HttpRequest>>,
    unsigned: Mutex<bool>,
}

impl Mock {
    fn on(&self, key: &str, status: u16, body: Json) {
        self.routes.lock().unwrap().insert(key.into(), (status, body));
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
            let key = format!("{} {}", req.method, u.path());
            let (status, body) = self
                .routes
                .lock()
                .unwrap()
                .get(&key)
                .cloned()
                .unwrap_or((404, json!({"code": "RESOURCE_NOT_EXISTS", "message": "no such route"})));
            let body = body.to_string().into_bytes();
            let mut headers = vec![("content-type".to_owned(), "application/json".to_owned())];
            if !*self.unsigned.lock().unwrap() {
                headers.extend(platform_headers(
                    "wechat-platform.key",
                    CERT_SERIAL,
                    self.clock.now_ms() / 1000,
                    &body,
                ));
            }
            Ok(HttpResponse { status, headers, body })
        })
    }
}

fn config() -> WechatPayConfig {
    WechatPayConfig {
        mchid: MCHID.into(),
        appid: "wxd678efh567hg6787".into(),
        merchant_serial: "3775B6A45ACD588826D15E583A95F5DD67D93D9A".into(),
        private_key: fixture("wechat-merchant.key"),
        api_v3_key: APIV3.into(),
        platform_keys: vec![
            (CERT_SERIAL.into(), fixture("wechat-platform.pem")),
            (PUBKEY_SERIAL.into(), fixture("wechat-platform-pub.pem")),
        ],
        notify_url: "https://forgebase.test/api/v1/billing/webhooks/wechat_pay".into(),
    }
}

fn install(env: &Env) -> (Arc<Mock>, WechatPayProvider) {
    let mock = Arc::new(Mock {
        clock: env.clock.clone(),
        routes: Mutex::new(HashMap::new()),
        calls: Mutex::new(Vec::new()),
        unsigned: Mutex::new(false),
    });
    mock.on(
        "POST /v3/pay/transactions/native",
        200,
        json!({"code_url": "weixin://wxpay/bizpayurl?pr=fb72Test"}),
    );
    let p = WechatPayProvider::new(
        config(),
        Some("http://wechat.test"),
        mock.clone(),
        Arc::new(env.clock.clone()),
    )
    .unwrap();
    env.billing.register_provider(Arc::new(p.clone()));
    (mock, p)
}

/// A notification body with an encrypted resource.
fn notice(id: &str, event: &str, created: &str, kind: &str, plain: &Json) -> Vec<u8> {
    let nonce = "fb72nonce123";
    let aad = kind;
    json!({"id": id, "create_time": created, "resource_type": "encrypt-resource", "event_type": event,
           "summary": "ok",
           "resource": {"original_type": kind, "algorithm": "AEAD_AES_256_GCM", "associated_data": aad, "nonce": nonce,
                        "ciphertext": wechat_pay::encrypt_resource(APIV3, nonce, aad, plain.to_string().as_bytes()).unwrap()}})
    .to_string()
    .into_bytes()
}

fn paid(invoice: &str, txn: &str, amount: i64) -> Json {
    json!({"mchid": MCHID, "appid": "wxd678efh567hg6787", "out_trade_no": "1217752501201407033233368018",
           "transaction_id": txn, "trade_type": "NATIVE", "trade_state": "SUCCESS", "trade_state_desc": "支付成功",
           "bank_type": "CMC", "attach": invoice, "success_time": "2026-11-01T08:00:05+08:00",
           "payer": {"openid": "oUpF8uMuAJO_M2pxb1Q9zNjWeS6o"},
           "amount": {"total": amount, "payer_total": amount, "currency": "CNY", "payer_currency": "CNY"}})
}

fn code(e: &BillingError) -> &'static str {
    match e {
        BillingError::Invalid { code, .. } | BillingError::Webhook { code, .. } => code,
        other => panic!("unexpected error {other:?}"),
    }
}

async fn setup() -> (Env, Arc<Mock>, WechatPayProvider) {
    let env = Env::new().await;
    env.catalog().await;
    let mut p = recurring("pro", "CNY", 19_900);
    p.currency_options = Default::default();
    env.billing.put_price(&admin(), "pro-cny", p).await.unwrap();
    let (mock, driver) = install(&env);
    env.customer("ann", "CNY").await;
    (env, mock, driver)
}

#[tokio::test]
async fn wechat_pay_native_payment_signed_notification_and_refund() {
    let (env, mock, _) = setup().await;
    let co = env.subscribe("ann", &["pro-cny"], "sub-wx-1").await;
    assert_eq!(co.payment_url.as_deref(), Some("weixin://wxpay/bizpayurl?pr=fb72Test"));
    let inv = co.invoice.clone().unwrap();

    // The order request is signed with the merchant key.
    let req = &mock.calls("/v3/pay/transactions/native")[0];
    let auth = req
        .headers
        .iter()
        .find(|(k, _)| k == "authorization")
        .unwrap()
        .1
        .clone();
    assert!(auth.starts_with("WECHATPAY2-SHA256-RSA2048 "), "{auth}");
    let field = |name: &str| {
        let start = auth.find(&format!("{name}=\"")).unwrap() + name.len() + 2;
        auth[start..start + auth[start..].find('"').unwrap()].to_owned()
    };
    assert_eq!(field("mchid"), MCHID);
    let msg = format!(
        "POST\n/v3/pay/transactions/native\n{}\n{}\n{}\n",
        field("timestamp"),
        field("nonce_str"),
        String::from_utf8_lossy(&req.body)
    );
    let sig = base64::engine::general_purpose::STANDARD
        .decode(field("signature"))
        .unwrap();
    let merchant_pub = jws::rsa_public(&der(&fixture("wechat-merchant.key"))).unwrap();
    assert!(jws::rsa_verify(&merchant_pub, msg.as_bytes(), &sig));
    let body: Json = serde_json::from_slice(&req.body).unwrap();
    assert_eq!(
        (body["amount"]["total"].as_i64(), body["attach"].as_str()),
        (Some(19_900), Some(inv.id.as_str()))
    );

    // TRANSACTION.SUCCESS, signed by the platform certificate.
    let ts = env.clock.now_ms() / 1000;
    let raw = notice(
        "EV-2018022511",
        "TRANSACTION.SUCCESS",
        "2026-11-01T08:00:06+08:00",
        "transaction",
        &paid(&inv.id, "4200000001202611010000000001", 19_900),
    );
    let h = platform_headers("wechat-platform.key", CERT_SERIAL, ts, &raw);
    let out = env.billing.handle_webhook("wechat_pay", &h, &raw).await.unwrap();
    assert_eq!(
        (out.result.as_str(), out.detail.as_deref()),
        ("processed", Some("invoice_paid")),
        "{out:?}"
    );
    let sub = env
        .billing
        .subscription(&user("ann"), &co.subscription.unwrap().id)
        .await
        .unwrap();
    assert_eq!(sub.status, SubscriptionStatus::Active);
    assert_eq!(env.plan_of(&pk("ann")).0.as_deref(), Some("pro"));
    // Replay.
    assert_eq!(
        env.billing.handle_webhook("wechat_pay", &h, &raw).await.unwrap().result,
        "duplicate"
    );

    // Refund: the original amount is read back, then the refund requested.
    mock.on(
        "GET /v3/pay/transactions/id/4200000001202611010000000001",
        200,
        json!({"transaction_id": "4200000001202611010000000001", "trade_state": "SUCCESS",
               "amount": {"total": 19_900, "currency": "CNY"}}),
    );
    mock.on(
        "POST /v3/refund/domestic/refunds",
        200,
        json!({"refund_id": "50000000382019052709732678859", "out_refund_no": "x", "status": "PROCESSING",
               "amount": {"refund": 5_000, "total": 19_900, "currency": "CNY"}}),
    );
    let note = env
        .billing
        .credit_invoice(
            &admin(),
            &inv.id,
            CreditNoteInput {
                amount: Some(5_000),
                to_balance: false,
                reason: Some("goodwill".into()),
                idempotency_key: "cn-wx-1".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(note.refund_amount, 5_000);
    let refund_req: Json = serde_json::from_slice(&mock.calls("/v3/refund/domestic/refunds")[0].body).unwrap();
    assert_eq!(
        refund_req["amount"],
        json!({"refund": 5_000, "total": 19_900, "currency": "CNY"})
    );
    assert!(
        mock.calls("/v3/pay/transactions/id/4200000001202611010000000001")[0]
            .url
            .contains("mchid=1900000109")
    );
    let r = &env.billing.refunds(&user("ann"), None, None).await.unwrap()[0];
    assert_eq!(r.status, RefundStatus::Pending);

    // REFUND.SUCCESS, signed with the platform public key (rotation).
    let raw = notice(
        "EV-2018022512",
        "REFUND.SUCCESS",
        "2026-11-01T09:00:00+08:00",
        "refund",
        &json!({"mchid": MCHID, "transaction_id": "4200000001202611010000000001", "out_trade_no": "x",
                "refund_id": "50000000382019052709732678859", "out_refund_no": "x", "refund_status": "SUCCESS",
                "success_time": "2026-11-01T09:00:00+08:00",
                "amount": {"total": 19_900, "refund": 5_000, "payer_total": 19_900, "payer_refund": 5_000}}),
    );
    let h = platform_headers("wechat-platform.key", PUBKEY_SERIAL, ts, &raw);
    let out = env.billing.handle_webhook("wechat_pay", &h, &raw).await.unwrap();
    assert_eq!(out.detail.as_deref(), Some("refund_succeeded"), "{out:?}");
    assert_eq!(
        env.billing.refunds(&user("ann"), None, None).await.unwrap()[0].status,
        RefundStatus::Succeeded
    );
}

#[tokio::test]
async fn wechat_pay_rejects_forged_stale_and_foreign_notifications() {
    let (env, mock, driver) = setup().await;
    let co = env.subscribe("ann", &["pro-cny"], "sub-wx-2").await;
    let inv = co.invoice.unwrap();
    let ts = env.clock.now_ms() / 1000;
    let raw = notice(
        "EV-1",
        "TRANSACTION.SUCCESS",
        "2026-11-01T08:00:00+08:00",
        "transaction",
        &paid(&inv.id, "4200000002", 19_900),
    );
    let deliver = |h: Vec<(String, String)>, b: Vec<u8>| {
        let bill = env.billing.clone();
        async move { bill.handle_webhook("wechat_pay", &h, &b).await }
    };
    // Signed by a key that is not the platform's, under the platform serial.
    let h = platform_headers("wechat-other.key", CERT_SERIAL, ts, &raw);
    assert_eq!(code(&deliver(h, raw.clone()).await.unwrap_err()), "invalid_signature");
    // An unknown serial.
    let h = platform_headers("wechat-platform.key", "DEADBEEF", ts, &raw);
    assert_eq!(code(&deliver(h, raw.clone()).await.unwrap_err()), "invalid_signature");
    // A replayed old delivery outside the tolerance.
    let h = platform_headers("wechat-platform.key", CERT_SERIAL, ts - 3_600, &raw);
    assert_eq!(code(&deliver(h, raw.clone()).await.unwrap_err()), "stale_signature");
    // A body changed after signing.
    let h = platform_headers("wechat-platform.key", CERT_SERIAL, ts, &raw);
    let mut changed = raw.clone();
    let last = changed.len() - 1;
    changed.insert(last, b' ');
    assert_eq!(code(&deliver(h, changed).await.unwrap_err()), "invalid_signature");
    // A resource that does not decrypt with our APIv3 key.
    let mut bad: Json = serde_json::from_slice(&raw).unwrap();
    bad["resource"]["ciphertext"] = json!(
        wechat_pay::encrypt_resource("someoneElsesApiV3Key0123456789ab", "fb72nonce123", "transaction", b"{}").unwrap()
    );
    let bad = bad.to_string().into_bytes();
    let h = platform_headers("wechat-platform.key", CERT_SERIAL, ts, &bad);
    assert_eq!(code(&deliver(h, bad).await.unwrap_err()), "invalid_signature");
    // Another merchant's payment.
    let mut foreign = paid(&inv.id, "4200000003", 19_900);
    foreign["mchid"] = json!("1230000109");
    let raw2 = notice(
        "EV-2",
        "TRANSACTION.SUCCESS",
        "2026-11-01T08:00:00+08:00",
        "transaction",
        &foreign,
    );
    let h = platform_headers("wechat-platform.key", CERT_SERIAL, ts, &raw2);
    assert_eq!(code(&deliver(h, raw2).await.unwrap_err()), "invalid_signature");
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_billing_webhook_events")
            .await,
        0
    );
    assert_eq!(
        env.count("SELECT COUNT(*) FROM _forgebase_billing_invoices WHERE status = 'paid'")
            .await,
        0
    );

    // An unsigned API response is not trusted.
    *mock.unsigned.lock().unwrap() = true;
    let e = driver
        .hosted_payment(&HostedPaymentRequest {
            invoice_id: inv.id.clone(),
            description: "x".into(),
            amount: 100,
            currency: "CNY".into(),
            idempotency_key: "k-unsigned".into(),
            ..HostedPaymentRequest::default()
        })
        .await
        .unwrap_err();
    assert!(e.to_string().contains("unverified"), "{e}");
    // Native payments are CNY only.
    let e = driver
        .hosted_payment(&HostedPaymentRequest {
            currency: "USD".into(),
            idempotency_key: "k-usd".into(),
            ..HostedPaymentRequest::default()
        })
        .await
        .unwrap_err();
    assert_eq!(code(&e), "currency_not_supported");
}

#[tokio::test]
async fn wechat_pay_accounts_refuse_production_and_bad_keys() {
    let env = Env::new().await;
    let input = |api_base: Option<&str>, apiv3: &str| ProviderInput {
        key_id: Some(MCHID.into()),
        api_key: Some(apiv3.into()),
        api_base: api_base.map(Into::into),
        config: Some(json!({"appid": "wxd678efh567hg6787", "merchant_serial": "3775B6A4",
            "private_key": fixture("wechat-merchant.key"), "notify_url": "https://forgebase.test/hook",
            "platform_keys": [{"serial": CERT_SERIAL, "pem": fixture("wechat-platform.pem")}]})),
        ..ProviderInput::default()
    };
    let put = |i: ProviderInput| {
        let b = env.billing.clone();
        async move { b.put_provider(&admin(), "wechat_pay", i).await }
    };
    assert_eq!(code(&put(input(None, APIV3)).await.unwrap_err()), "live_mode_refused");
    assert_eq!(
        code(
            &put(input(Some("https://api.mch.weixin.qq.com"), APIV3))
                .await
                .unwrap_err()
        ),
        "live_mode_refused"
    );
    assert_eq!(
        code(
            &put(input(Some("https://wechat-sandbox.test"), "short"))
                .await
                .unwrap_err()
        ),
        "invalid_key"
    );
    // A certificate whose serial is not the one given.
    let mut wrong = input(Some("https://wechat-sandbox.test"), APIV3);
    wrong.config.as_mut().unwrap()["platform_keys"][0]["serial"] = json!("0123");
    assert_eq!(code(&put(wrong).await.unwrap_err()), "invalid_config");
    let acct = put(input(Some("https://wechat-sandbox.test"), APIV3)).await.unwrap();
    assert_eq!(acct.webhook_path, "/api/v1/billing/webhooks/wechat_pay");
    assert!(env.billing.provider("wechat_pay").is_ok());
}

#[tokio::test]
async fn wechat_pay_notification_is_recorded_linked_and_replayed_after_the_tolerance() {
    let (env, _, _) = setup().await;
    let co = env.subscribe("ann", &["pro-cny"], "sub-wx-i").await;
    let inv = co.invoice.unwrap();
    let ts = env.clock.now_ms() / 1000;
    let raw = notice(
        "EV-INSPECT-1",
        "TRANSACTION.SUCCESS",
        "2026-11-01T08:00:06+08:00",
        "transaction",
        &paid(&inv.id, "4200000009202611010000000009", 19_900),
    );
    let h = platform_headers("wechat-platform.key", CERT_SERIAL, ts, &raw);
    let out = env.billing.handle_webhook("wechat_pay", &h, &raw).await.unwrap();
    assert_eq!(out.result, "processed", "{out:?}");
    let d = env
        .billing
        .webhook_deliveries(&admin(), forgebase_billing::WebhookDeliveryFilter::default())
        .await
        .unwrap()
        .items
        .remove(0);
    assert_eq!(
        (d.provider.as_str(), d.account.as_deref(), d.outcome.as_str()),
        ("wechat_pay", Some(MCHID), "applied")
    );
    assert_eq!(d.links.invoice.as_deref(), Some(inv.id.as_str()));
    assert_eq!(d.links.subscription, co.subscription.map(|s| s.id));
    assert!(d.links.payment.is_some() && d.links.customer.is_some());
    let detail = env.billing.webhook_delivery(&admin(), &d.id).await.unwrap();
    for k in [
        "wechatpay-timestamp",
        "wechatpay-nonce",
        "wechatpay-signature",
        "wechatpay-serial",
    ] {
        assert!(detail.headers[k].is_string(), "{k}");
    }
    // Ten minutes later (outside the 300 s signature tolerance) a replay is
    // verified against the time it was received, then deduplicated.
    env.clock.advance(10 * MIN);
    let r = env.billing.replay_webhook_delivery(&admin(), &d.id).await.unwrap();
    assert_eq!(r.result, "already_applied", "{r:?}");
    // Signed with a key that is not the platform's: recorded, not processed.
    let raw = notice(
        "EV-INSPECT-2",
        "TRANSACTION.SUCCESS",
        "2026-11-01T08:10:00+08:00",
        "transaction",
        &paid(&inv.id, "4200000009202611010000000010", 19_900),
    );
    let ts = env.clock.now_ms() / 1000;
    let h = platform_headers("wechat-merchant.key", CERT_SERIAL, ts, &raw);
    let e = env.billing.handle_webhook("wechat_pay", &h, &raw).await.unwrap_err();
    assert_eq!(code(&e), "invalid_signature");
    let f = env
        .billing
        .webhook_deliveries(
            &admin(),
            forgebase_billing::WebhookDeliveryFilter {
                outcome: Some("signature_invalid".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!((f.total, f.items[0].processing), (1, None));
}
