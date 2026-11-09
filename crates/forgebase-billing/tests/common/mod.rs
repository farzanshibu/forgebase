//! Test harness: a migrated SQLite database, policy, entitlements (FB-24),
//! credits (FB-73), the job queue, the billing service on a manual clock and
//! a mock provider HTTP client. **No real provider is ever called**: the
//! drivers talk to [`MockHttp`], which answers like Stripe and Razorpay test
//! mode (and replays a response for a repeated idempotency key, as Stripe
//! does).
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use forgebase_auth::AuthError;
use forgebase_auth::crypto::Keyring;
use forgebase_auth::social::http_client::{BoxFuture, HttpClient, HttpRequest, HttpResponse};
use forgebase_billing::provider::{razorpay, stripe};
use forgebase_billing::{
    BillingDeps, BillingService, Checkout, CustomerInput, Interval, ItemInput, PriceInput, PriceType, ProductInput,
    ProductKind, ProviderInput, SubscribeInput,
};
use forgebase_core::DataLayout;
use forgebase_credits::CreditService;
use forgebase_data::{AccessContext, DataService};
use forgebase_db::{Backend, Database, DbOptions, Statement};
use forgebase_entitlements::Clock as _;
use forgebase_entitlements::{
    AddonFeature, AddonInput, EntitlementService, FeatureInput, FeatureKind, ManualClock, PlanFeature, PlanInput,
    UsagePackInput, UsageRequest,
};
use forgebase_jobs::JobService;
use forgebase_policy::PolicyService;
use serde_json::{Value as Json, json};

/// 2026-11-01 00:00 UTC.
pub const T0: i64 = 1_793_491_200_000;
pub const MIN: i64 = 60_000;
pub const HOUR: i64 = 3_600_000;
pub const DAY: i64 = 86_400_000;

pub const STRIPE_KEY: &str = "sk_test_51forgebasetestkey";
pub const STRIPE_WHSEC: &str = "whsec_forgebase_test_secret";
pub const RZP_KEY: &str = "rzp_test_forgebase1";
pub const RZP_SECRET: &str = "rzp_secret_forgebase";
pub const RZP_WHSEC: &str = "rzp_webhook_secret_1";

/// How the mock answers charges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeMode {
    Succeed,
    Decline,
    Processing,
    /// Transport failure (timeout).
    Unreachable,
}

/// A mock of the provider APIs.
#[derive(Debug)]
pub struct MockHttp {
    pub requests: Mutex<Vec<HttpRequest>>,
    pub mode: Mutex<ChargeMode>,
    pub refund_status: Mutex<&'static str>,
    idem: Mutex<HashMap<String, HttpResponse>>,
    n: AtomicU64,
}

impl Default for MockHttp {
    fn default() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            mode: Mutex::new(ChargeMode::Succeed),
            refund_status: Mutex::new("succeeded"),
            idem: Mutex::new(HashMap::new()),
            n: AtomicU64::new(0),
        }
    }
}

fn resp(status: u16, body: Json) -> HttpResponse {
    HttpResponse {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: body.to_string().into_bytes(),
    }
}

impl MockHttp {
    pub fn set_mode(&self, m: ChargeMode) {
        *self.mode.lock().unwrap() = m;
    }

    /// Requests whose URL contains `path`.
    pub fn calls(&self, path: &str) -> Vec<HttpRequest> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.url.contains(path))
            .cloned()
            .collect()
    }

    /// Form field of a request body.
    pub fn field(req: &HttpRequest, name: &str) -> Option<String> {
        url::form_urlencoded::parse(&req.body)
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    }

    fn answer(&self, req: &HttpRequest) -> Result<HttpResponse, AuthError> {
        let n = self.n.fetch_add(1, Ordering::SeqCst);
        let path = url::Url::parse(&req.url).unwrap().path().to_owned();
        let mode = *self.mode.lock().unwrap();
        Ok(match path.as_str() {
            "/v1/customers" => resp(200, json!({"id": format!("cus_test_{n}")})),
            "/v1/payment_intents" => match mode {
                ChargeMode::Succeed => resp(200, json!({"id": format!("pi_test_{n}"), "status": "succeeded"})),
                ChargeMode::Processing => resp(200, json!({"id": format!("pi_test_{n}"), "status": "processing"})),
                ChargeMode::Decline => resp(
                    402,
                    json!({"error": {"type": "card_error", "code": "card_declined", "decline_code": "insufficient_funds",
                        "message": "Your card has insufficient funds.", "payment_intent": {"id": format!("pi_test_{n}")}}}),
                ),
                ChargeMode::Unreachable => {
                    return Err(AuthError::Provider("request to the remote server timed out".into()));
                }
            },
            "/v1/refunds" => resp(
                200,
                json!({"id": format!("re_test_{n}"), "status": *self.refund_status.lock().unwrap()}),
            ),
            "/v1/checkout/sessions" => resp(
                200,
                json!({"id": format!("cs_test_{n}"), "url": format!("https://checkout.stripe.test/c/pay/cs_test_{n}")}),
            ),
            "/v1/orders" => resp(200, json!({"id": format!("order_test_{n}")})),
            "/v1/payments/create/recurring" => match mode {
                ChargeMode::Decline => resp(
                    400,
                    json!({"error": {"code": "BAD_REQUEST_ERROR", "description": "Payment failed"}}),
                ),
                _ => resp(
                    200,
                    json!({"razorpay_payment_id": format!("pay_test_{n}"), "razorpay_order_id": "order"}),
                ),
            },
            "/v1/payment_links" => resp(
                200,
                json!({"id": format!("plink_{n}"), "short_url": format!("https://rzp.test/{n}")}),
            ),
            "/v1/subscription_registration/auth_links" => resp(
                200,
                json!({"id": format!("inv_{n}"), "short_url": format!("https://rzp.test/auth/{n}")}),
            ),
            p if p.starts_with("/v1/payments/") && p.ends_with("/refund") => {
                resp(200, json!({"id": format!("rfnd_test_{n}"), "status": "processed"}))
            }
            _ => resp(404, json!({"error": {"message": "no such route"}})),
        })
    }
}

impl HttpClient for MockHttp {
    fn send(&self, req: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, AuthError>> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(req.clone());
            let key = req
                .headers
                .iter()
                .find(|(k, _)| k == "idempotency-key" || k == "x-razorpay-idempotency-key")
                .map(|(_, v)| format!("{}|{v}", req.url));
            if let Some(k) = &key
                && let Some(r) = self.idem.lock().unwrap().get(k)
            {
                return Ok(r.clone());
            }
            let r = self.answer(&req)?;
            if let Some(k) = key {
                self.idem.lock().unwrap().insert(k, r.clone());
            }
            Ok(r)
        })
    }
}

pub struct Env {
    pub db: Database,
    pub policy: PolicyService,
    pub data: DataService,
    pub ent: EntitlementService,
    pub credits: CreditService,
    pub jobs: JobService,
    pub billing: BillingService,
    pub clock: ManualClock,
    pub http: Arc<MockHttp>,
    pub tmp: tempfile::TempDir,
}

pub fn admin() -> AccessContext {
    AccessContext::admin("root")
}

pub fn sys() -> AccessContext {
    AccessContext::system()
}

pub fn user(id: &str) -> AccessContext {
    AccessContext::user("people", id)
}

pub fn pk(id: &str) -> String {
    format!("user:people/{id}")
}

pub fn pf(feature: &str, limit: Option<i64>) -> PlanFeature {
    PlanFeature {
        feature: feature.into(),
        enabled: true,
        limit,
        unit: String::new(),
        period: Some(serde_json::from_value(json!("month")).unwrap()),
        window_secs: None,
        trial_limit: None,
    }
}

pub fn recurring(product: &str, currency: &str, amount: i64) -> PriceInput {
    PriceInput {
        product: product.into(),
        price_type: PriceType::Recurring,
        interval: Some(Interval::Month),
        interval_count: 1,
        scheme: None,
        currency: currency.into(),
        unit_amount: amount,
        tiers: None,
        currency_options: Default::default(),
        feature: None,
        meter_basis: None,
        trial_days: 0,
        tax_behavior: None,
        active: true,
        metadata: None,
    }
}

pub fn product(name: &str, kind: ProductKind) -> ProductInput {
    ProductInput {
        name: name.into(),
        description: None,
        kind,
        plan_key: None,
        addon_key: None,
        pack_key: None,
        credits: None,
        active: true,
        metadata: None,
    }
}

impl Env {
    pub async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(tmp.path().join("data"));
        layout.ensure().unwrap();
        let cfg = forgebase_core::Config::defaults(tmp.path());
        let db = Database::open(DbOptions::from_config(&cfg.database, layout.database_path()))
            .await
            .unwrap();
        db.migrate(Some(&layout.backups_dir())).await.unwrap();
        let backend: Arc<dyn Backend> = Arc::new(db.clone());
        let policy = PolicyService::open(backend.clone()).await.unwrap();
        let data = DataService::new(backend.clone(), policy.authorizer()).await.unwrap();
        policy.attach_data(data.clone());
        let clock = ManualClock::at(T0);
        let ent = EntitlementService::open(backend.clone(), policy.clone(), Arc::new(clock.clone()))
            .await
            .unwrap();
        policy.set_hooks(ent.hooks());
        let jobs = JobService::new(data.clone());
        ent.attach_jobs(jobs.clone());
        let credits = CreditService::open(backend.clone(), policy.clone(), ent.clone(), Arc::new(clock.clone()))
            .await
            .unwrap();
        let http = Arc::new(MockHttp::default());
        let billing = BillingService::open(
            backend,
            BillingDeps {
                policy: policy.clone(),
                entitlements: ent.clone(),
                credits: Arc::new(credits.clone()),
                clock: Arc::new(clock.clone()),
                keyring: Keyring::from_master(&[7u8; 32]),
                http: http.clone(),
            },
        )
        .await
        .unwrap();
        billing.attach_jobs(jobs.clone());
        let env = Self {
            db,
            policy,
            data,
            ent,
            credits,
            jobs,
            billing,
            clock,
            http,
            tmp,
        };
        env.data
            .create_collection(
                &sys(),
                serde_json::from_value(json!({"name": "people", "fields": [{"name": "email", "type": "text"}]}))
                    .unwrap(),
            )
            .await
            .unwrap();
        env
    }

    /// Stripe (test mode) as the provider.
    pub async fn stripe(&self) {
        self.billing
            .put_provider(
                &admin(),
                "stripe",
                ProviderInput {
                    api_key: Some(STRIPE_KEY.into()),
                    webhook_secret: Some(STRIPE_WHSEC.into()),
                    ..ProviderInput::default()
                },
            )
            .await
            .unwrap();
    }

    /// Razorpay (test mode) as the provider.
    pub async fn razorpay(&self) {
        self.billing
            .put_provider(
                &admin(),
                "razorpay",
                ProviderInput {
                    key_id: Some(RZP_KEY.into()),
                    api_key: Some(RZP_SECRET.into()),
                    webhook_secret: Some(RZP_WHSEC.into()),
                    ..ProviderInput::default()
                },
            )
            .await
            .unwrap();
    }

    /// FB-24: features `projects` (metered, limit per plan) and `api_calls`
    /// (metered, unlimited), plans free (default), basic, pro; an add-on
    /// `extra_projects`; a usage pack `api_1k`. Billing: products `basic`,
    /// `pro` (plans), `seats` (add-on), `api` (metered service), `credits`
    /// (100 credits per unit), `pack` (usage pack); USD monthly prices
    /// `basic-m` 1000, `pro-m` 3000 (EUR 2800, INR 250000), `seat-m` 500,
    /// one-time `credits-100` 900, `pack-1k` 400.
    pub async fn catalog(&self) {
        let a = admin();
        for (f, kind) in [("projects", FeatureKind::Metered), ("api_calls", FeatureKind::Metered)] {
            self.ent
                .put_feature(
                    &a,
                    f,
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
        for (plan, default, limit) in [("free", true, 3), ("basic", false, 20), ("pro", false, 100)] {
            self.ent
                .put_plan(
                    &a,
                    plan,
                    PlanInput {
                        name: None,
                        description: None,
                        is_default: default,
                        active: true,
                        trial_days: 0,
                        metadata: None,
                        features: vec![pf("projects", Some(limit)), pf("api_calls", None)],
                    },
                )
                .await
                .unwrap();
        }
        self.ent
            .put_addon(
                &a,
                "extra_projects",
                AddonInput {
                    name: None,
                    description: None,
                    active: true,
                    features: vec![AddonFeature {
                        feature: "projects".into(),
                        limit: Some(10),
                        period: None,
                        window_secs: None,
                    }],
                },
            )
            .await
            .unwrap();
        self.ent
            .put_usage_pack(
                &a,
                "api_1k",
                UsagePackInput {
                    name: None,
                    feature: "api_calls".into(),
                    units: 1000,
                    valid_days: None,
                    active: true,
                },
            )
            .await
            .unwrap();
        let b = &self.billing;
        for (key, plan) in [("basic", "basic"), ("pro", "pro")] {
            let mut p = product(key, ProductKind::Plan);
            p.plan_key = Some(plan.into());
            b.put_product(&a, key, p).await.unwrap();
        }
        let mut seats = product("Extra projects", ProductKind::Addon);
        seats.addon_key = Some("extra_projects".into());
        b.put_product(&a, "seats", seats).await.unwrap();
        b.put_product(&a, "api", product("API calls", ProductKind::Service))
            .await
            .unwrap();
        let mut credits = product("Credits", ProductKind::Credits);
        credits.credits = Some(100);
        b.put_product(&a, "credits", credits).await.unwrap();
        let mut pack = product("1k API calls", ProductKind::UsagePack);
        pack.pack_key = Some("api_1k".into());
        b.put_product(&a, "pack", pack).await.unwrap();

        b.put_price(&a, "basic-m", recurring("basic", "USD", 1000))
            .await
            .unwrap();
        let mut pro = recurring("pro", "USD", 3000);
        pro.currency_options = serde_json::from_value(json!({
            "EUR": {"unit_amount": 2800},
            "INR": {"unit_amount": 250000},
        }))
        .unwrap();
        b.put_price(&a, "pro-m", pro).await.unwrap();
        b.put_price(&a, "seat-m", recurring("seats", "USD", 500)).await.unwrap();
        let mut one = recurring("credits", "USD", 900);
        one.price_type = PriceType::OneTime;
        one.interval = None;
        b.put_price(&a, "credits-100", one.clone()).await.unwrap();
        one.product = "pack".into();
        one.unit_amount = 400;
        b.put_price(&a, "pack-1k", one).await.unwrap();
    }

    pub async fn customer(&self, who: &str, currency: &str) {
        self.billing
            .put_customer(
                &user(who),
                CustomerInput {
                    email: Some(format!("{who}@example.test")),
                    name: Some(who.into()),
                    currency: Some(currency.into()),
                    ..CustomerInput::default()
                },
            )
            .await
            .unwrap();
    }

    /// Saves a card for `who` the way production does: the hosted setup
    /// page, then Stripe's signed `setup_intent.succeeded` webhook.
    pub async fn save_card(&self, who: &str) {
        let c = self.billing.customer(&user(who), None).await.unwrap();
        let ev = stripe_event(
            &format!("evt_setup_{who}"),
            "setup_intent.succeeded",
            self.clock_secs(),
            json!({"id": format!("seti_{who}"), "customer": c.provider_customer_id, "payment_method": format!("pm_card_{who}"),
                   "metadata": {"forgebase_customer": c.id}}),
        );
        let out = self.stripe_webhook(&ev, self.clock_secs()).await.unwrap();
        assert_eq!(out.result, "processed", "{out:?}");
    }

    pub fn clock_secs(&self) -> i64 {
        self.clock.now_ms() / 1000
    }

    /// Delivers a signed Stripe webhook.
    pub async fn stripe_webhook(
        &self,
        body: &Json,
        t_secs: i64,
    ) -> forgebase_billing::Result<forgebase_billing::WebhookOutcome> {
        let raw = body.to_string();
        let headers = vec![(
            "stripe-signature".to_owned(),
            stripe::sign(STRIPE_WHSEC, t_secs, raw.as_bytes()),
        )];
        self.billing.handle_webhook("stripe", &headers, raw.as_bytes()).await
    }

    /// Delivers a signed Razorpay webhook.
    pub async fn razorpay_webhook(
        &self,
        id: &str,
        body: &Json,
    ) -> forgebase_billing::Result<forgebase_billing::WebhookOutcome> {
        let raw = body.to_string();
        let headers = vec![
            (
                "x-razorpay-signature".to_owned(),
                razorpay::sign(RZP_WHSEC, raw.as_bytes()),
            ),
            ("x-razorpay-event-id".to_owned(), id.to_owned()),
        ];
        self.billing.handle_webhook("razorpay", &headers, raw.as_bytes()).await
    }

    pub async fn subscribe(&self, who: &str, prices: &[&str], key: &str) -> Checkout {
        self.billing
            .subscribe(
                &user(who),
                SubscribeInput {
                    items: prices.iter().map(|p| ItemInput::new(*p)).collect(),
                    idempotency_key: key.into(),
                    ..SubscribeInput::default()
                },
            )
            .await
            .unwrap()
    }

    /// Records FB-24 usage.
    pub async fn use_units(&self, subject: &str, feature: &str, units: i64, key: &str) {
        self.ent
            .consume(&sys(), UsageRequest::new(feature, units, key).for_subject(subject))
            .await
            .unwrap();
    }

    pub fn plan_of(&self, subject: &str) -> (Option<String>, Option<forgebase_entitlements::SubscriptionStatus>, bool) {
        let e = self.ent.entitlements_of(subject, None);
        (e.plan, e.status, e.entitled)
    }

    pub async fn count(&self, sql: &str) -> i64 {
        self.db
            .fetch_all(&Statement::new(sql, Vec::new()))
            .await
            .unwrap()
            .scalar()
            .and_then(forgebase_db::Value::as_i64)
            .unwrap()
    }

    /// Runs one write statement.
    pub async fn exec(&self, sql: &str) {
        let mut tx = self.db.begin(forgebase_db::TxMode::Normal).await.unwrap();
        tx.execute(&Statement::new(sql, Vec::new())).await.unwrap();
        tx.commit().await.unwrap();
    }

    /// Advances the clock and runs maintenance.
    pub async fn advance(&self, ms: i64) -> forgebase_billing::MaintenanceReport {
        self.clock.advance(ms);
        self.billing.run_maintenance().await.unwrap()
    }
}

/// A Stripe event body.
pub fn stripe_event(id: &str, kind: &str, created_secs: i64, object: Json) -> Json {
    json!({"id": id, "object": "event", "type": kind, "created": created_secs, "livemode": false,
           "data": {"object": object}})
}
