//! App Store in-app purchases (FB-72), sandbox only.
//!
//! - **Verification**: a transaction id is looked up with the App Store
//!   Server API (`GET /inApps/v1/transactions/{id}`, ES256 JWT of the App
//!   Store Connect key), a StoreKit 2 signed transaction is taken as is;
//!   subscriptions also read their status and renewal info
//!   (`GET /inApps/v1/subscriptions/{originalTransactionId}`).
//! - **Signatures**: every JWS Apple signs (transactions, renewal info,
//!   notifications) is verified against its `x5c` chain, which must lead to
//!   the pinned **Apple Root CA - G3** (SHA-256 `63:34:3A:BF:…:91:79`), with
//!   Apple's receipt-signing marker on the leaf (`1.2.840.113635.100.6.11.1`)
//!   and the WWDR marker on the intermediate (`1.2.840.113635.100.6.2.1`).
//! - **Notifications**: App Store Server Notifications V2 (`{"signedPayload"}`),
//!   deduplicated by `notificationUUID`.
//! - Production transactions and the production API are refused.

use std::sync::Arc;

use forgebase_auth::social::http_client::{HttpClient, HttpRequest};
use forgebase_db::backend::BoxFuture;
use forgebase_entitlements::Clock;
use serde_json::{Value as Json, json};

use super::der;
use super::jws::{self, Jws};
use super::store::{
    PurchaseClaim, PurchaseKind, StoreNotification, StoreProvider, StorePurchase, StoreRefund, StoreState,
};
use super::{json_body, parse_ok, str_at};
use crate::error::{BillingError, Result};
use crate::money::exponent;

/// Sandbox API base.
pub const SANDBOX_API: &str = "https://api.storekit-sandbox.itunes.apple.com";
/// Production API base (refused).
pub const PRODUCTION_API: &str = "https://api.storekit.itunes.apple.com";
/// Apple Root CA - G3 (pinned).
pub const APPLE_ROOT_CA_G3: &str = include_str!("apple_root_ca_g3.pem");
/// Marker extension of Apple's receipt-signing leaf certificates.
pub const LEAF_OID: &str = "1.2.840.113635.100.6.11.1";
/// Marker extension of Apple's WWDR intermediate certificates.
pub const INTERMEDIATE_OID: &str = "1.2.840.113635.100.6.2.1";

/// Account settings.
#[derive(Debug, Clone, Default)]
pub struct AppStoreConfig {
    /// App Store Connect API issuer id.
    pub issuer_id: String,
    /// API key id.
    pub key_id: String,
    /// The `.p8` private key (PEM PKCS#8, P-256).
    pub private_key: String,
    /// Bundle id of the app; transactions of other apps are refused.
    pub bundle_id: String,
    /// `sandbox` (default), `xcode` or `local_testing`. `production` is refused.
    pub environment: String,
}

impl AppStoreConfig {
    /// From the sealed provider config (`issuer_id`, `key_id`, `private_key`,
    /// `bundle_id`, `environment`).
    pub fn from_json(v: &Json) -> Self {
        let s = |k: &str| v.get(k).and_then(Json::as_str).unwrap_or_default().trim().to_owned();
        Self {
            issuer_id: s("issuer_id"),
            key_id: s("key_id"),
            private_key: s("private_key"),
            bundle_id: s("bundle_id"),
            environment: s("environment"),
        }
    }
}

/// The App Store driver.
#[derive(Clone)]
pub struct AppStoreProvider {
    cfg: AppStoreConfig,
    key: Vec<u8>,
    api_base: String,
    roots: Vec<Vec<u8>>,
    http: Arc<dyn HttpClient>,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for AppStoreProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppStoreProvider")
            .field("bundle_id", &self.cfg.bundle_id)
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

impl AppStoreProvider {
    /// A sandbox driver. `roots` replaces the pinned Apple root (tests sign
    /// with their own chain).
    pub fn new(
        cfg: AppStoreConfig,
        api_base: Option<&str>,
        roots: Option<Vec<Vec<u8>>>,
        http: Arc<dyn HttpClient>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        match cfg.environment.to_ascii_lowercase().as_str() {
            "" | "sandbox" | "xcode" | "local_testing" => {}
            "production" => {
                return Err(BillingError::invalid(
                    "live_mode_refused",
                    "only the App Store sandbox is accepted",
                ));
            }
            other => {
                return Err(BillingError::invalid(
                    "invalid_config",
                    format!("unknown App Store environment {other:?}"),
                ));
            }
        }
        let api_base = api_base.unwrap_or(SANDBOX_API).trim_end_matches('/').to_owned();
        if api_base == PRODUCTION_API {
            return Err(BillingError::invalid(
                "live_mode_refused",
                "the production App Store Server API is refused",
            ));
        }
        for (v, name) in [
            (&cfg.issuer_id, "issuer_id"),
            (&cfg.key_id, "key_id"),
            (&cfg.bundle_id, "bundle_id"),
        ] {
            if v.is_empty() || v.len() > 200 {
                return Err(BillingError::invalid("invalid_config", format!("{name} is required")));
            }
        }
        let key = der::pem_der(&cfg.private_key, "PRIVATE KEY")?;
        jws::es256_public(&key)?;
        let roots = match roots {
            Some(r) => r,
            None => der::pem_blocks(APPLE_ROOT_CA_G3).into_iter().map(|(_, d)| d).collect(),
        };
        Ok(Self {
            cfg,
            key,
            api_base,
            roots,
            http,
            clock,
        })
    }

    fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    /// The App Store Server API bearer token.
    fn token(&self) -> Result<String> {
        let iat = self.now() / 1000;
        jws::sign_es256(
            &self.key,
            &json!({"alg": "ES256", "kid": self.cfg.key_id, "typ": "JWT"}),
            &json!({"iss": self.cfg.issuer_id, "iat": iat, "exp": iat + 600, "aud": "appstoreconnect-v1",
                    "bid": self.cfg.bundle_id}),
        )
    }

    async fn get(&self, path: &str) -> Result<Json> {
        let req = HttpRequest::get(format!("{}{path}", self.api_base))
            .header("authorization", format!("Bearer {}", self.token()?))
            .header("accept", "application/json");
        let res = self.http.send(req).await?;
        if res.status == 404 {
            return Err(BillingError::invalid(
                "unknown_transaction",
                "the App Store does not know this transaction",
            ));
        }
        parse_ok(&res)
    }

    /// Verifies one JWS Apple signed and returns its payload.
    pub fn decode(&self, signed: &str) -> Result<Json> {
        let j = Jws::parse(signed)?;
        j.verify_x5c(&self.roots, self.now(), Some(LEAF_OID), Some(INTERMEDIATE_OID))?;
        Ok(j.payload)
    }

    fn check_app(&self, bundle: Option<&str>) -> Result<()> {
        if bundle != Some(self.cfg.bundle_id.as_str()) {
            return Err(BillingError::invalid(
                "wrong_app",
                "the transaction belongs to another app",
            ));
        }
        Ok(())
    }

    /// A purchase from a verified transaction, its renewal info and the
    /// subscription status (1 active, 2 expired, 3 billing retry, 4 grace,
    /// 5 revoked) when known.
    fn purchase(&self, tx: &Json, renewal: Option<&Json>, status: Option<i64>) -> Result<StorePurchase> {
        self.check_app(str_at(tx, &["bundleId"]))?;
        let s = |k: &str| str_at(tx, &[k]).map(str::to_owned);
        let i = |k: &str| tx.get(k).and_then(Json::as_i64);
        let txn = s("transactionId").ok_or_else(|| BillingError::invalid("invalid_transaction", "no transactionId"))?;
        let original = s("originalTransactionId").unwrap_or_else(|| txn.clone());
        let product = s("productId").ok_or_else(|| BillingError::invalid("invalid_transaction", "no productId"))?;
        let kind = match str_at(tx, &["type"]) {
            Some("Auto-Renewable Subscription") => PurchaseKind::Subscription,
            _ => PurchaseKind::OneTime,
        };
        let env = str_at(tx, &["environment"]).unwrap_or("Production");
        let now = self.now();
        let expires = i("expiresDate");
        let revoked = i("revocationDate");
        let trial = str_at(tx, &["offerDiscountType"]) == Some("FREE_TRIAL");
        let state = if revoked.is_some() {
            StoreState::Refunded
        } else if kind == PurchaseKind::OneTime {
            StoreState::Active
        } else {
            match status {
                Some(1) | None if expires.is_some_and(|e| e > now) || status == Some(1) => {
                    if trial {
                        StoreState::Trialing
                    } else {
                        StoreState::Active
                    }
                }
                Some(3) => StoreState::OnHold,
                Some(4) => StoreState::Grace,
                Some(5) => StoreState::Revoked,
                _ => StoreState::Expired,
            }
        };
        let amount = i("price").map(|milli| {
            let scale = 10i64.pow(exponent(&s("currency").unwrap_or_default()));
            crate::money::mul_div(milli, scale, 1000)
        });
        let auto_renew = match renewal {
            Some(r) => r.get("autoRenewStatus").and_then(Json::as_i64) == Some(1),
            None => kind == PurchaseKind::Subscription,
        };
        Ok(StorePurchase {
            purchase_id: original,
            transaction_id: Some(txn.clone()),
            product_id: product,
            kind,
            state,
            auto_renew: auto_renew && state.live(),
            period_start: i("purchaseDate"),
            expires_at: expires,
            purchased_at: i("purchaseDate").unwrap_or(now),
            amount,
            currency: s("currency").map(|c| c.to_ascii_uppercase()),
            quantity: i("quantity").unwrap_or(1).max(1),
            test: env != "Production",
            subject_hint: None,
            replaces: None,
            refund: revoked.map(|at| StoreRefund {
                transaction_id: txn.clone(),
                refund_ref: format!("{txn}:revoked:{at}"),
                amount: None,
            }),
            refund_only: false,
            observed_at: i("signedDate").unwrap_or(now).max(
                renewal
                    .and_then(|r| r.get("signedDate"))
                    .and_then(Json::as_i64)
                    .unwrap_or(0),
            ),
            needs_ack: false,
        })
    }

    /// Status and renewal info of a subscription, with its newest transaction.
    async fn status(&self, original: &str) -> Result<(Json, Option<Json>, Option<i64>)> {
        let v = self.get(&format!("/inApps/v1/subscriptions/{}", enc(original))).await?;
        let last = v
            .get("data")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(|g| g.get("lastTransactions").and_then(Json::as_array))
            .flatten()
            .find(|t| str_at(t, &["originalTransactionId"]) == Some(original))
            .cloned()
            .ok_or_else(|| BillingError::invalid("unknown_transaction", "no subscription status"))?;
        let tx = self.decode(
            str_at(&last, &["signedTransactionInfo"])
                .ok_or_else(|| BillingError::Provider("App Store status without a transaction".into()))?,
        )?;
        let renewal = match str_at(&last, &["signedRenewalInfo"]) {
            Some(r) => Some(self.decode(r)?),
            None => None,
        };
        Ok((tx, renewal, last.get("status").and_then(Json::as_i64)))
    }
}

fn enc(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

impl StoreProvider for AppStoreProvider {
    fn name(&self) -> &'static str {
        "app_store"
    }

    fn verify_purchase<'a>(&'a self, claim: &'a PurchaseClaim) -> BoxFuture<'a, Result<StorePurchase>> {
        Box::pin(async move {
            let tx = match (&claim.signed_transaction, &claim.transaction_id) {
                (Some(jws), _) => self.decode(jws)?,
                (None, Some(id)) => {
                    let v = self.get(&format!("/inApps/v1/transactions/{}", enc(id))).await?;
                    self.decode(
                        str_at(&v, &["signedTransactionInfo"])
                            .ok_or_else(|| BillingError::Provider("App Store returned no transaction".into()))?,
                    )?
                }
                (None, None) => {
                    return Err(BillingError::invalid(
                        "transaction_required",
                        "send transaction_id or signed_transaction",
                    ));
                }
            };
            self.check_app(str_at(&tx, &["bundleId"]))?;
            if str_at(&tx, &["type"]) == Some("Auto-Renewable Subscription") {
                let original = str_at(&tx, &["originalTransactionId"]).unwrap_or_default().to_owned();
                let (latest, renewal, status) = self.status(&original).await?;
                return self.purchase(&latest, renewal.as_ref(), status);
            }
            self.purchase(&tx, None, None)
        })
    }

    fn refresh<'a>(
        &'a self,
        purchase_id: &'a str,
        _product_id: &'a str,
        kind: PurchaseKind,
    ) -> BoxFuture<'a, Result<StorePurchase>> {
        Box::pin(async move {
            if kind == PurchaseKind::Subscription {
                let (tx, renewal, status) = self.status(purchase_id).await?;
                return self.purchase(&tx, renewal.as_ref(), status);
            }
            let v = self
                .get(&format!("/inApps/v1/transactions/{}", enc(purchase_id)))
                .await?;
            let tx = self.decode(
                str_at(&v, &["signedTransactionInfo"])
                    .ok_or_else(|| BillingError::Provider("App Store returned no transaction".into()))?,
            )?;
            self.purchase(&tx, None, None)
        })
    }

    fn verify_notification<'a>(
        &'a self,
        _headers: &'a [(String, String)],
        body: &'a [u8],
        now_ms: i64,
    ) -> BoxFuture<'a, Result<StoreNotification>> {
        Box::pin(async move {
            let v = json_body(body)?;
            let signed = str_at(&v, &["signedPayload"])
                .ok_or_else(|| BillingError::webhook("invalid_payload", "no signedPayload"))?;
            let j = Jws::parse(signed)?;
            j.verify_x5c(&self.roots, now_ms, Some(LEAF_OID), Some(INTERMEDIATE_OID))?;
            let n = j.payload;
            let data = n.get("data").cloned().unwrap_or(Json::Null);
            let kind = str_at(&n, &["notificationType"]).unwrap_or_default().to_owned();
            let sub = str_at(&n, &["subtype"]).map(str::to_owned);
            let id = str_at(&n, &["notificationUUID"])
                .ok_or_else(|| BillingError::webhook("invalid_payload", "no notificationUUID"))?
                .to_owned();
            let created_at = n.get("signedDate").and_then(Json::as_i64).unwrap_or(now_ms);
            let event_type = match &sub {
                Some(s) => format!("{kind}.{s}"),
                None => kind.clone(),
            };
            if kind == "TEST" {
                return Ok(StoreNotification {
                    id,
                    event_type,
                    created_at,
                    purchase: None,
                });
            }
            if let Some(b) = str_at(&data, &["bundleId"]) {
                self.check_app(Some(b))?;
            }
            if str_at(&data, &["environment"]) == Some("Production") {
                return Err(BillingError::invalid(
                    "live_mode_refused",
                    "only App Store sandbox notifications are accepted",
                ));
            }
            let Some(signed_tx) = str_at(&data, &["signedTransactionInfo"]) else {
                return Ok(StoreNotification {
                    id,
                    event_type,
                    created_at,
                    purchase: None,
                });
            };
            let tx = self.decode(signed_tx)?;
            let renewal = match str_at(&data, &["signedRenewalInfo"]) {
                Some(r) => Some(self.decode(r)?),
                None => None,
            };
            let status = data.get("status").and_then(Json::as_i64);
            let mut p = self.purchase(&tx, renewal.as_ref(), status)?;
            p.observed_at = p.observed_at.max(created_at);
            let txn = p.transaction_id.clone().unwrap_or_default();
            match (kind.as_str(), sub.as_deref()) {
                ("CONSUMPTION_REQUEST", _) => {
                    return Ok(StoreNotification {
                        id,
                        event_type,
                        created_at,
                        purchase: None,
                    });
                }
                ("DID_FAIL_TO_RENEW", Some("GRACE_PERIOD")) => p.state = StoreState::Grace,
                ("DID_FAIL_TO_RENEW", _) | ("GRACE_PERIOD_EXPIRED", _) => p.state = StoreState::OnHold,
                ("EXPIRED", _) => p.state = StoreState::Expired,
                ("REFUND", _) | ("REVOKE", _) => {
                    p.state = if kind == "REFUND" {
                        StoreState::Refunded
                    } else {
                        StoreState::Revoked
                    };
                    p.refund = Some(StoreRefund {
                        transaction_id: txn.clone(),
                        refund_ref: format!("{txn}:{}", kind.to_ascii_lowercase()),
                        amount: None,
                    });
                }
                _ => {}
            }
            if !p.state.live() {
                p.auto_renew = false;
            }
            Ok(StoreNotification {
                id,
                event_type,
                created_at,
                purchase: Some(p),
            })
        })
    }
}
