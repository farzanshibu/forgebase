//! Google Play in-app purchases (FB-72), licence-tester (test) purchases only.
//!
//! - **Verification** with the Play Developer API: subscriptions through
//!   `purchases.subscriptionsv2.get`, one-time products through
//!   `purchases.products.get`; verified purchases are acknowledged (Play
//!   refunds unacknowledged purchases after three days).
//! - **Authentication**: a service-account JWT (RS256) exchanged for an
//!   access token at Google's token endpoint, cached until it expires.
//! - **Real-time developer notifications** arrive as Pub/Sub push messages.
//!   The push carries a Google-signed OIDC token (`Authorization: Bearer`):
//!   RS256 against Google's JWKS, issuer `accounts.google.com`, the configured
//!   audience and push service account. The notification only names the
//!   purchase; its state is always read back from the Developer API.
//!   Deduplicated by the Pub/Sub message id.
//! - A purchase without `testPurchase` (subscriptions) or `purchaseType` 0
//!   (products) is a real one and is refused.

use std::sync::{Arc, Mutex};

use base64::Engine as _;
use forgebase_auth::social::http_client::{HttpClient, HttpRequest};
use forgebase_db::backend::BoxFuture;
use forgebase_entitlements::Clock;
use forgebase_entitlements::model::parse_iso;
use serde_json::{Value as Json, json};

use super::der;
use super::jws::{self, Jws};
use super::store::{
    PurchaseClaim, PurchaseKind, StoreNotification, StoreProvider, StorePurchase, StoreRefund, StoreState,
};
use super::{header, json_body, parse_ok, str_at};
use crate::error::{BillingError, Result};
use crate::money::exponent;

/// Developer API base.
pub const API_BASE: &str = "https://androidpublisher.googleapis.com";
/// OAuth token endpoint.
pub const TOKEN_URI: &str = "https://oauth2.googleapis.com/token";
/// Google's signing keys for OIDC tokens.
pub const CERTS_URI: &str = "https://www.googleapis.com/oauth2/v3/certs";
const SCOPE: &str = "https://www.googleapis.com/auth/androidpublisher";

/// Account settings.
#[derive(Debug, Clone, Default)]
pub struct GooglePlayConfig {
    /// Android package name; notifications of other apps are refused.
    pub package_name: String,
    /// Service account email.
    pub client_email: String,
    /// Service account private key (PEM PKCS#8).
    pub private_key: String,
    /// Token endpoint (default Google's).
    pub token_uri: String,
    /// Audience of the Pub/Sub push token (the push endpoint URL or the
    /// value set on the subscription).
    pub push_audience: String,
    /// Service account the push subscription authenticates as (optional).
    pub push_service_account: Option<String>,
    /// JWKS endpoint (default Google's).
    pub certs_uri: String,
}

impl GooglePlayConfig {
    /// From the sealed provider config: `package_name`, `service_account`
    /// (the JSON key file, as an object or a string), `push_audience`,
    /// `push_service_account`.
    pub fn from_json(v: &Json) -> Self {
        let s = |v: &Json, k: &str| v.get(k).and_then(Json::as_str).unwrap_or_default().trim().to_owned();
        let sa = match v.get("service_account") {
            Some(Json::String(t)) => serde_json::from_str(t).unwrap_or(Json::Null),
            Some(o) => o.clone(),
            None => Json::Null,
        };
        Self {
            package_name: s(v, "package_name"),
            client_email: s(&sa, "client_email"),
            private_key: s(&sa, "private_key"),
            token_uri: Some(s(&sa, "token_uri")).filter(|t| !t.is_empty()).unwrap_or_else(|| TOKEN_URI.into()),
            push_audience: s(v, "push_audience"),
            push_service_account: Some(s(v, "push_service_account")).filter(|t| !t.is_empty()),
            certs_uri: Some(s(v, "certs_uri")).filter(|t| !t.is_empty()).unwrap_or_else(|| CERTS_URI.into()),
        }
    }
}

type Keys = Vec<(String, Vec<u8>, Vec<u8>)>;

/// The Google Play driver.
pub struct GooglePlayProvider {
    cfg: GooglePlayConfig,
    key: Vec<u8>,
    api_base: String,
    http: Arc<dyn HttpClient>,
    clock: Arc<dyn Clock>,
    token: Mutex<Option<(String, i64)>>,
    jwks: Mutex<Option<(Keys, i64)>>,
}

impl std::fmt::Debug for GooglePlayProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GooglePlayProvider")
            .field("package_name", &self.cfg.package_name)
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

/// Google's money: `{"currencyCode", "units", "nanos"}` to minor units.
fn money(v: &Json) -> Option<(i64, String)> {
    let cur = str_at(v, &["currencyCode"])?.to_ascii_uppercase();
    let units: i64 = match v.get("units") {
        Some(Json::String(s)) => s.parse().ok()?,
        Some(n) => n.as_i64()?,
        None => 0,
    };
    let nanos = v.get("nanos").and_then(Json::as_i64).unwrap_or(0);
    let scale = 10i64.pow(exponent(&cur));
    Some((units * scale + crate::money::mul_div(nanos, scale, 1_000_000_000), cur))
}

impl GooglePlayProvider {
    /// A driver for test purchases.
    pub fn new(
        cfg: GooglePlayConfig,
        api_base: Option<&str>,
        http: Arc<dyn HttpClient>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        if cfg.package_name.is_empty() || cfg.package_name.len() > 200 {
            return Err(BillingError::invalid("invalid_config", "package_name is required"));
        }
        if cfg.client_email.is_empty() {
            return Err(BillingError::invalid(
                "invalid_config",
                "service_account (the JSON key) is required",
            ));
        }
        if cfg.push_audience.is_empty() {
            return Err(BillingError::invalid(
                "invalid_config",
                "push_audience (of the Pub/Sub push subscription) is required",
            ));
        }
        let key = der::pem_der(&cfg.private_key, "PRIVATE KEY")?;
        jws::rsa_public(&key)?;
        Ok(Self {
            cfg,
            key,
            api_base: api_base.unwrap_or(API_BASE).trim_end_matches('/').to_owned(),
            http,
            clock,
            token: Mutex::new(None),
            jwks: Mutex::new(None),
        })
    }

    fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    async fn access_token(&self) -> Result<String> {
        let now = self.now();
        if let Some((t, exp)) = self.token.lock().ok().and_then(|g| g.clone())
            && exp > now + 60_000
        {
            return Ok(t);
        }
        let iat = now / 1000;
        let assertion = jws::sign_rs256(
            &self.key,
            &json!({"alg": "RS256", "typ": "JWT"}),
            &json!({"iss": self.cfg.client_email, "scope": SCOPE, "aud": self.cfg.token_uri,
                    "iat": iat, "exp": iat + 3600}),
        )?;
        let req = HttpRequest::post_form(
            self.cfg.token_uri.clone(),
            &[
                ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                ("assertion", assertion.as_str()),
            ],
        );
        let v = parse_ok(&self.http.send(req).await?)?;
        let token = str_at(&v, &["access_token"])
            .ok_or_else(|| BillingError::Provider("Google returned no access token".into()))?
            .to_owned();
        let ttl = v.get("expires_in").and_then(Json::as_i64).unwrap_or(3600);
        if let Ok(mut g) = self.token.lock() {
            *g = Some((token.clone(), now + ttl * 1000));
        }
        Ok(token)
    }

    fn app_path(&self) -> String {
        format!(
            "{}/androidpublisher/v3/applications/{}",
            self.api_base,
            enc(&self.cfg.package_name)
        )
    }

    async fn call(&self, method: &'static str, url: String) -> Result<Json> {
        let token = self.access_token().await?;
        let mut req = HttpRequest::get(url).header("authorization", format!("Bearer {token}"));
        if method == "POST" {
            req.method = "POST";
            req.body = b"{}".to_vec();
            req = req.header("content-type", "application/json");
        }
        let res = self.http.send(req).await?;
        if res.status == 404 || res.status == 410 {
            return Err(BillingError::invalid(
                "unknown_purchase",
                "Google Play does not know this purchase token",
            ));
        }
        parse_ok(&res)
    }

    async fn subscription(&self, token: &str, product: Option<&str>) -> Result<StorePurchase> {
        let v = self
            .call(
                "GET",
                format!("{}/purchases/subscriptionsv2/tokens/{}", self.app_path(), enc(token)),
            )
            .await?;
        let item = v
            .get("lineItems")
            .and_then(Json::as_array)
            .and_then(|a| match product {
                Some(p) => a.iter().find(|i| str_at(i, &["productId"]) == Some(p)),
                None => a.first(),
            })
            .ok_or_else(|| BillingError::invalid("product_mismatch", "the purchase is not for this product"))?;
        let product_id = str_at(item, &["productId"]).unwrap_or_default().to_owned();
        let now = self.now();
        let expires = str_at(item, &["expiryTime"]).and_then(parse_iso);
        let auto = item
            .get("autoRenewingPlan")
            .and_then(|p| p.get("autoRenewEnabled"))
            .and_then(Json::as_bool)
            .unwrap_or(false);
        let state = match str_at(&v, &["subscriptionState"]).unwrap_or_default() {
            "SUBSCRIPTION_STATE_ACTIVE" => StoreState::Active,
            "SUBSCRIPTION_STATE_CANCELED" if expires.is_some_and(|e| e > now) => StoreState::Active,
            "SUBSCRIPTION_STATE_IN_GRACE_PERIOD" => StoreState::Grace,
            "SUBSCRIPTION_STATE_ON_HOLD" => StoreState::OnHold,
            "SUBSCRIPTION_STATE_PAUSED" => StoreState::Paused,
            "SUBSCRIPTION_STATE_PENDING" => StoreState::Pending,
            _ => StoreState::Expired,
        };
        let price = item
            .get("autoRenewingPlan")
            .and_then(|p| p.get("recurringPrice"))
            .and_then(money);
        let order = str_at(&v, &["latestOrderId"])
            .or_else(|| str_at(item, &["latestSuccessfulOrderId"]))
            .map(str::to_owned);
        Ok(StorePurchase {
            purchase_id: token.to_owned(),
            transaction_id: order,
            product_id,
            kind: PurchaseKind::Subscription,
            state,
            auto_renew: auto && state.live() && str_at(&v, &["subscriptionState"]) != Some("SUBSCRIPTION_STATE_CANCELED"),
            period_start: None,
            expires_at: expires,
            purchased_at: str_at(&v, &["startTime"]).and_then(parse_iso).unwrap_or(now),
            amount: price.as_ref().map(|p| p.0),
            currency: price.map(|p| p.1),
            quantity: 1,
            test: v.get("testPurchase").is_some(),
            subject_hint: None,
            replaces: str_at(&v, &["linkedPurchaseToken"]).map(str::to_owned),
            refund: None,
            refund_only: false,
            observed_at: now,
            needs_ack: str_at(&v, &["acknowledgementState"]) == Some("ACKNOWLEDGEMENT_STATE_PENDING"),
        })
    }

    async fn product(&self, token: &str, product: &str) -> Result<StorePurchase> {
        let v = self
            .call(
                "GET",
                format!(
                    "{}/purchases/products/{}/tokens/{}",
                    self.app_path(),
                    enc(product),
                    enc(token)
                ),
            )
            .await?;
        let now = self.now();
        let order = str_at(&v, &["orderId"]).map(str::to_owned);
        let state = match v.get("purchaseState").and_then(Json::as_i64) {
            Some(0) => StoreState::Active,
            Some(2) => StoreState::Pending,
            _ => StoreState::Refunded,
        };
        let refund = (state == StoreState::Refunded)
            .then(|| order.clone())
            .flatten()
            .map(|o| StoreRefund {
                refund_ref: format!("{o}:canceled"),
                transaction_id: o,
                amount: None,
            });
        let at = match v.get("purchaseTimeMillis") {
            Some(Json::String(s)) => s.parse().ok(),
            Some(n) => n.as_i64(),
            None => None,
        };
        Ok(StorePurchase {
            purchase_id: token.to_owned(),
            transaction_id: order,
            product_id: product.to_owned(),
            kind: PurchaseKind::OneTime,
            state,
            auto_renew: false,
            period_start: None,
            expires_at: None,
            purchased_at: at.unwrap_or(now),
            amount: None,
            currency: None,
            quantity: v.get("quantity").and_then(Json::as_i64).unwrap_or(1).max(1),
            test: v.get("purchaseType").and_then(Json::as_i64) == Some(0),
            subject_hint: None,
            replaces: None,
            refund,
            refund_only: false,
            observed_at: now,
            needs_ack: v.get("acknowledgementState").and_then(Json::as_i64) == Some(0),
        })
    }

    async fn keys(&self, kid: &str) -> Result<(Vec<u8>, Vec<u8>)> {
        let now = self.now();
        let find = |keys: &Keys| keys.iter().find(|k| k.0 == kid).map(|k| (k.1.clone(), k.2.clone()));
        if let Some((keys, at)) = self.jwks.lock().ok().and_then(|g| g.clone())
            && now - at < 3_600_000
            && let Some(k) = find(&keys)
        {
            return Ok(k);
        }
        let v = parse_ok(&self.http.send(HttpRequest::get(self.cfg.certs_uri.clone())).await?)?;
        let keys: Keys = v
            .get("keys")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter(|k| str_at(k, &["kty"]) == Some("RSA"))
            .filter_map(|k| {
                Some((
                    str_at(k, &["kid"])?.to_owned(),
                    jws::b64url_decode(str_at(k, &["n"])?)?,
                    jws::b64url_decode(str_at(k, &["e"])?)?,
                ))
            })
            .collect();
        let found = find(&keys);
        if let Ok(mut g) = self.jwks.lock() {
            *g = Some((keys, now));
        }
        found.ok_or_else(|| BillingError::webhook("invalid_signature", "the push token key is unknown"))
    }

    /// Verifies the Pub/Sub push OIDC token.
    async fn check_push(&self, headers: &[(String, String)], now: i64) -> Result<()> {
        let forged = |m: &str| BillingError::webhook("invalid_signature", m.to_owned());
        let token = header(headers, "authorization")
            .and_then(|h| h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer ")))
            .ok_or_else(|| forged("the push carries no bearer token"))?;
        let j = Jws::parse(token)?;
        let kid = j.kid().ok_or_else(|| forged("the push token has no kid"))?.to_owned();
        let (n, e) = self.keys(&kid).await?;
        j.verify_rs256(&n, &e)?;
        let c = &j.payload;
        if !matches!(
            str_at(c, &["iss"]),
            Some("accounts.google.com" | "https://accounts.google.com")
        ) {
            return Err(forged("the push token was not issued by Google"));
        }
        let aud_ok = match c.get("aud") {
            Some(Json::String(a)) => *a == self.cfg.push_audience,
            Some(Json::Array(a)) => a.iter().any(|x| x.as_str() == Some(self.cfg.push_audience.as_str())),
            _ => false,
        };
        if !aud_ok {
            return Err(forged("the push token is for another audience"));
        }
        let exp = c.get("exp").and_then(Json::as_i64).unwrap_or(0) * 1000;
        if exp <= now {
            return Err(BillingError::webhook("stale_signature", "the push token has expired"));
        }
        if c.get("iat").and_then(Json::as_i64).unwrap_or(0) * 1000 > now + 300_000 {
            return Err(forged("the push token is issued in the future"));
        }
        if let Some(sa) = &self.cfg.push_service_account
            && (str_at(c, &["email"]) != Some(sa.as_str()) || c.get("email_verified").and_then(Json::as_bool) != Some(true))
        {
            return Err(forged("the push token is not from the configured service account"));
        }
        Ok(())
    }
}

/// Names of subscription notification types.
fn sub_type(n: i64) -> &'static str {
    match n {
        1 => "SUBSCRIPTION_RECOVERED",
        2 => "SUBSCRIPTION_RENEWED",
        3 => "SUBSCRIPTION_CANCELED",
        4 => "SUBSCRIPTION_PURCHASED",
        5 => "SUBSCRIPTION_ON_HOLD",
        6 => "SUBSCRIPTION_IN_GRACE_PERIOD",
        7 => "SUBSCRIPTION_RESTARTED",
        8 => "SUBSCRIPTION_PRICE_CHANGE_CONFIRMED",
        9 => "SUBSCRIPTION_DEFERRED",
        10 => "SUBSCRIPTION_PAUSED",
        11 => "SUBSCRIPTION_PAUSE_SCHEDULE_CHANGED",
        12 => "SUBSCRIPTION_REVOKED",
        13 => "SUBSCRIPTION_EXPIRED",
        19 => "SUBSCRIPTION_PRICE_CHANGE_UPDATED",
        20 => "SUBSCRIPTION_PENDING_PURCHASE_CANCELED",
        _ => "SUBSCRIPTION_UNKNOWN",
    }
}

impl StoreProvider for GooglePlayProvider {
    fn name(&self) -> &'static str {
        "google_play"
    }

    fn verify_purchase<'a>(&'a self, claim: &'a PurchaseClaim) -> BoxFuture<'a, Result<StorePurchase>> {
        Box::pin(async move {
            let token = claim
                .purchase_token
                .as_deref()
                .filter(|t| !t.is_empty() && t.len() <= 4096)
                .ok_or_else(|| BillingError::invalid("purchase_token_required", "send purchase_token"))?;
            let product = claim
                .product_id
                .as_deref()
                .filter(|p| !p.is_empty())
                .ok_or_else(|| BillingError::invalid("product_required", "send product_id"))?;
            match claim.kind.unwrap_or(PurchaseKind::Subscription) {
                PurchaseKind::Subscription => self.subscription(token, Some(product)).await,
                PurchaseKind::OneTime => self.product(token, product).await,
            }
        })
    }

    fn refresh<'a>(
        &'a self,
        purchase_id: &'a str,
        product_id: &'a str,
        kind: PurchaseKind,
    ) -> BoxFuture<'a, Result<StorePurchase>> {
        Box::pin(async move {
            match kind {
                PurchaseKind::Subscription => self.subscription(purchase_id, None).await,
                PurchaseKind::OneTime => self.product(purchase_id, product_id).await,
            }
        })
    }

    fn verify_notification<'a>(
        &'a self,
        headers: &'a [(String, String)],
        body: &'a [u8],
        now_ms: i64,
    ) -> BoxFuture<'a, Result<StoreNotification>> {
        Box::pin(async move {
            self.check_push(headers, now_ms).await?;
            let v = json_body(body)?;
            let msg = v.get("message").cloned().unwrap_or(Json::Null);
            let id = str_at(&msg, &["messageId"])
                .or_else(|| str_at(&msg, &["message_id"]))
                .ok_or_else(|| BillingError::webhook("invalid_payload", "no Pub/Sub message id"))?
                .to_owned();
            let data = str_at(&msg, &["data"])
                .and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok())
                .and_then(|d| serde_json::from_slice::<Json>(&d).ok())
                .ok_or_else(|| BillingError::webhook("invalid_payload", "the message data is not base64 JSON"))?;
            if str_at(&data, &["packageName"]) != Some(self.cfg.package_name.as_str()) {
                return Err(BillingError::invalid("wrong_app", "the notification belongs to another app"));
            }
            let created_at = match data.get("eventTimeMillis") {
                Some(Json::String(s)) => s.parse().unwrap_or(now_ms),
                Some(n) => n.as_i64().unwrap_or(now_ms),
                None => now_ms,
            };
            let done = |event_type: &str, purchase| StoreNotification {
                id: id.clone(),
                event_type: event_type.to_owned(),
                created_at,
                purchase,
            };
            if let Some(n) = data.get("subscriptionNotification") {
                let kind = sub_type(n.get("notificationType").and_then(Json::as_i64).unwrap_or(0));
                let token = str_at(n, &["purchaseToken"])
                    .ok_or_else(|| BillingError::webhook("invalid_payload", "no purchaseToken"))?;
                let mut p = self.subscription(token, None).await?;
                if kind == "SUBSCRIPTION_REVOKED" {
                    p.state = StoreState::Revoked;
                    p.auto_renew = false;
                    p.refund = p.transaction_id.clone().map(|o| StoreRefund {
                        refund_ref: format!("{o}:revoked"),
                        transaction_id: o,
                        amount: None,
                    });
                }
                return Ok(done(kind, Some(p)));
            }
            if let Some(n) = data.get("oneTimeProductNotification") {
                let token = str_at(n, &["purchaseToken"])
                    .ok_or_else(|| BillingError::webhook("invalid_payload", "no purchaseToken"))?;
                let sku = str_at(n, &["sku"]).unwrap_or_default();
                let kind = match n.get("notificationType").and_then(Json::as_i64) {
                    Some(1) => "ONE_TIME_PRODUCT_PURCHASED",
                    Some(2) => "ONE_TIME_PRODUCT_CANCELED",
                    _ => "ONE_TIME_PRODUCT_UNKNOWN",
                };
                let p = self.product(token, sku).await?;
                return Ok(done(kind, Some(p)));
            }
            if let Some(n) = data.get("voidedPurchaseNotification") {
                let token = str_at(n, &["purchaseToken"])
                    .ok_or_else(|| BillingError::webhook("invalid_payload", "no purchaseToken"))?;
                let order = str_at(n, &["orderId"]).unwrap_or_default().to_owned();
                let kind = if n.get("productType").and_then(Json::as_i64) == Some(1) {
                    PurchaseKind::Subscription
                } else {
                    PurchaseKind::OneTime
                };
                // The product is the one on record for the token.
                let p = StorePurchase {
                    purchase_id: token.to_owned(),
                    transaction_id: Some(order.clone()),
                    product_id: String::new(),
                    kind,
                    state: StoreState::Refunded,
                    auto_renew: false,
                    period_start: None,
                    expires_at: None,
                    purchased_at: created_at,
                    amount: None,
                    currency: None,
                    quantity: 1,
                    test: true,
                    subject_hint: None,
                    replaces: None,
                    refund: Some(StoreRefund {
                        refund_ref: format!("{order}:voided"),
                        transaction_id: order,
                        amount: None,
                    }),
                    refund_only: false,
                    observed_at: created_at,
                    needs_ack: false,
                };
                return Ok(done("VOIDED_PURCHASE", Some(p)));
            }
            Ok(done(
                if data.get("testNotification").is_some() {
                    "TEST_NOTIFICATION"
                } else {
                    "UNKNOWN"
                },
                None,
            ))
        })
    }

    fn acknowledge<'a>(&'a self, p: &'a StorePurchase) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if !p.needs_ack {
                return Ok(());
            }
            let kind = match p.kind {
                PurchaseKind::Subscription => "subscriptions",
                PurchaseKind::OneTime => "products",
            };
            self.call(
                "POST",
                format!(
                    "{}/purchases/{kind}/{}/tokens/{}:acknowledge",
                    self.app_path(),
                    enc(&p.product_id),
                    enc(&p.purchase_id)
                ),
            )
            .await?;
            Ok(())
        })
    }
}
