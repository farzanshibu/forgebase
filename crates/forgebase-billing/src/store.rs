//! Store purchases (FB-72, ADR-0021 §9): App Store, Google Play and Creem.
//!
//! The store is the merchant of record: it took the money and renews the
//! subscription. ForgeBase verifies what the store reports, server side, and
//! mirrors it:
//!
//! | Store says | Billing does |
//! |---|---|
//! | subscription bought / renewed (new transaction) | subscription created or moved to the store's period, a **paid invoice** with a payment whose `provider_ref` is the transaction id, FB-24 sync |
//! | one-time product bought | a paid order and invoice; credits and usage packs are delivered (FB-73, FB-24) |
//! | auto-renew off | `cancel_at_period_end` |
//! | grace period / on hold / paused | `past_due` (access kept) / `paused` (access suspended) |
//! | expired | `canceled` at the store's time |
//! | refunded / revoked | a credit note with an **external** refund (already made by the store), credits taken back, the subscription ended at once |
//!
//! A purchase (`_forgebase_billing_store_purchases`) belongs to one subject:
//! the first verified claim binds it and any other account gets `409
//! purchase_owned`. Store-managed subscriptions are never renewed, dunned,
//! changed or cancelled by ForgeBase. Out-of-order reports are ignored by
//! store time (`last_event_at`); every effect is idempotent by transaction
//! id or refund reference, so replays change nothing.

use std::sync::Arc;

use forgebase_data::AccessContext;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::error::{BillingError, Result};
use crate::model::{
    CreditNoteInput, HostedPage, Price, PriceType, ProductKind, StoreCheckoutInput, StoreProduct, StoreProductInput,
    StorePurchaseRecord, StoreVerifyInput, WebhookOutcome,
};
use crate::money::hex;
use crate::provider::store::{
    PurchaseClaim, PurchaseKind, StoreCheckoutRequest, StoreProvider, StorePurchase, StoreState, require_test,
};
use crate::service::{
    BillingService, Tx, add_months, check_idem, check_subject, check_url, int, new_id, opt_int, opt_text,
    period_months, realm_clause, text,
};

/// Stores ForgeBase knows.
pub const STORES: [&str; 3] = ["app_store", "google_play", "creem"];

const COLUMNS: &str = "sp.id, sp.provider, sp.purchase_id, sp.subject, sp.product_id, p.key, sp.kind, sp.status,
    sp.auto_renew, sp.expires_at, sp.environment, sp.subscription_id, sp.order_id, sp.last_transaction_id,
    sp.created_at, sp.updated_at, sp.customer_id, sp.last_event_at, sp.price_id";
const FROM: &str = "_forgebase_billing_store_purchases sp JOIN _forgebase_billing_prices p ON p.id = sp.price_id";

/// A stored purchase.
#[derive(Debug, Clone)]
struct Link {
    rec: StorePurchaseRecord,
    last_event_at: i64,
}

fn link_row(r: &[forgebase_db::Value]) -> Link {
    Link {
        rec: StorePurchaseRecord {
            id: text(r, 0),
            provider: text(r, 1),
            purchase_id: text(r, 2),
            subject: text(r, 3),
            product_id: text(r, 4),
            price: text(r, 5),
            kind: text(r, 6),
            status: text(r, 7),
            auto_renew: int(r, 8) != 0,
            expires_at: opt_int(r, 9),
            environment: text(r, 10),
            subscription_id: opt_text(r, 11),
            order_id: opt_text(r, 12),
            last_transaction_id: opt_text(r, 13),
            created_at: int(r, 14),
            updated_at: int(r, 15),
        },
        last_event_at: int(r, 17),
    }
}

/// What applying a store report did.
#[derive(Debug, Clone)]
pub(crate) struct StoreOutcome {
    pub status: &'static str,
    pub detail: String,
    pub record: Option<StorePurchaseRecord>,
}

impl StoreOutcome {
    fn ignored(detail: &str) -> Self {
        Self {
            status: "ignored",
            detail: detail.to_owned(),
            record: None,
        }
    }
}

fn check_store(provider: &str) -> Result<()> {
    if STORES.contains(&provider) {
        Ok(())
    } else {
        Err(BillingError::invalid(
            "unknown_provider",
            format!("{provider:?} is not a store (app_store, google_play, creem)"),
        ))
    }
}

fn check_product_id(id: &str) -> Result<String> {
    let id = id.trim();
    if id.is_empty() || id.len() > 200 || id.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(BillingError::invalid(
            "invalid_product",
            "a store product id is 1 to 200 characters without spaces",
        ));
    }
    Ok(id.to_owned())
}

/// Billing status of a store state (`None`: leave the subscription alone).
fn sub_status(s: StoreState) -> Option<&'static str> {
    Some(match s {
        StoreState::Active => "active",
        StoreState::Trialing => "trialing",
        StoreState::Grace => "past_due",
        StoreState::OnHold | StoreState::Paused => "paused",
        StoreState::Expired | StoreState::Refunded | StoreState::Revoked => "canceled",
        StoreState::Pending => return None,
    })
}

impl BillingService {
    fn store_driver(&self, provider: &str) -> Result<Arc<dyn StoreProvider>> {
        check_store(provider)?;
        self.store(provider)
            .ok_or_else(|| BillingError::NotConfigured(format!("store {provider:?} is not configured")))
    }

    /// Whether a store owns the subscription.
    pub(crate) async fn store_managed(&self, sub_id: &str) -> Result<bool> {
        let rows = self
            .fetch(
                "SELECT COUNT(*) FROM _forgebase_billing_store_purchases WHERE subscription_id = ?1",
                vec![sub_id.into()],
            )
            .await?;
        Ok(rows.scalar().and_then(forgebase_db::Value::as_i64).unwrap_or_default() > 0)
    }

    // -----------------------------------------------------------------------
    // Store products
    // -----------------------------------------------------------------------

    /// Store products and their prices (any caller).
    pub async fn store_products(&self, ctx: &AccessContext) -> Result<Vec<StoreProduct>> {
        self.require_caller(ctx)?;
        let rows = self
            .fetch(
                "SELECT sp.provider, sp.product_id, p.key, p.type, sp.updated_at FROM _forgebase_billing_store_products sp
                 JOIN _forgebase_billing_prices p ON p.id = sp.price_id ORDER BY sp.provider, sp.product_id",
                Vec::new(),
            )
            .await?;
        Ok(rows
            .rows
            .iter()
            .map(|r| StoreProduct {
                provider: text(r, 0),
                product_id: text(r, 1),
                price: text(r, 2),
                kind: if text(r, 3) == "recurring" {
                    "subscription"
                } else {
                    "one_time"
                }
                .into(),
                updated_at: int(r, 4),
            })
            .collect())
    }

    /// Maps a store product to a price (platform administrators).
    pub async fn put_store_product(
        &self,
        ctx: &AccessContext,
        provider: &str,
        product_id: &str,
        input: StoreProductInput,
    ) -> Result<StoreProduct> {
        self.require_admin(ctx)?;
        check_store(provider)?;
        let product_id = check_product_id(product_id)?;
        let price = match self.price_by("key", input.price.trim()).await {
            Err(BillingError::NotFound(_)) => {
                return Err(BillingError::invalid("unknown_price", "no price with this key"));
            }
            r => r?,
        };
        let kind = match price.price_type {
            PriceType::Recurring => "subscription",
            PriceType::OneTime => "one_time",
            PriceType::Metered => {
                return Err(BillingError::invalid(
                    "invalid_price",
                    "stores sell recurring or one-time prices, not metered ones",
                ));
            }
        };
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            t.exec(
                "INSERT INTO _forgebase_billing_store_products (provider, product_id, price_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT (provider, product_id) DO UPDATE SET price_id = excluded.price_id, updated_at = excluded.updated_at",
                vec![provider.into(), product_id.clone().into(), price.id.clone().into(), now.into()],
            )
            .await?;
            t.audit(
                &ctx.principal_key(),
                "store_product.put",
                "store_product",
                &format!("{provider}:{product_id}"),
                json!({"price": price.key}),
                now,
            )
            .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        Ok(StoreProduct {
            provider: provider.to_owned(),
            product_id,
            price: price.key,
            kind: kind.into(),
            updated_at: now,
        })
    }

    /// Removes a store product mapping (platform administrators). Purchases
    /// already verified keep their price.
    pub async fn delete_store_product(&self, ctx: &AccessContext, provider: &str, product_id: &str) -> Result<()> {
        self.require_admin(ctx)?;
        let now = self.now();
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let n = t
                .exec(
                    "DELETE FROM _forgebase_billing_store_products WHERE provider = ?1 AND product_id = ?2",
                    vec![provider.into(), product_id.into()],
                )
                .await?;
            if n == 0 {
                return Err(BillingError::NotFound("store product".into()));
            }
            t.audit(
                &ctx.principal_key(),
                "store_product.delete",
                "store_product",
                &format!("{provider}:{product_id}"),
                json!({}),
                now,
            )
            .await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await
    }

    async fn store_price(&self, provider: &str, product_id: &str) -> Result<Price> {
        let rows = self
            .fetch(
                "SELECT price_id FROM _forgebase_billing_store_products WHERE provider = ?1 AND product_id = ?2",
                vec![provider.into(), product_id.into()],
            )
            .await?;
        let Some(r) = rows.rows.first() else {
            return Err(BillingError::invalid(
                "unknown_store_product",
                format!("store product {product_id:?} is not mapped to a price"),
            ));
        };
        self.price_by("id", &text(r, 0)).await
    }

    // -----------------------------------------------------------------------
    // Purchases
    // -----------------------------------------------------------------------

    async fn link(&self, provider: &str, purchase_id: &str) -> Result<Option<Link>> {
        let rows = self
            .fetch(
                &format!("SELECT {COLUMNS} FROM {FROM} WHERE sp.provider = ?1 AND sp.purchase_id = ?2"),
                vec![provider.into(), purchase_id.into()],
            )
            .await?;
        Ok(rows.rows.first().map(|r| link_row(r)))
    }

    /// Store purchases of a subject (instance-wide readers may omit it).
    pub async fn store_purchases(
        &self,
        ctx: &AccessContext,
        subject: Option<&str>,
    ) -> Result<Vec<StorePurchaseRecord>> {
        let rows = if subject.is_none()
            && let Ok(realm) = self.can_read_all(ctx)
        {
            let mut params = Vec::new();
            let only = realm_clause(realm.as_deref(), "sp.subject", &mut params);
            self.fetch(
                &format!("SELECT {COLUMNS} FROM {FROM} WHERE 1 = 1{only} ORDER BY sp.created_at DESC LIMIT 500"),
                params,
            )
            .await?
        } else {
            let s = self.subject_for(ctx, subject)?;
            self.can_read(ctx, &s)?;
            self.fetch(
                &format!("SELECT {COLUMNS} FROM {FROM} WHERE sp.subject = ?1 ORDER BY sp.created_at DESC"),
                vec![s.into()],
            )
            .await?
        };
        Ok(rows.rows.iter().map(|r| link_row(r).rec).collect())
    }

    /// Verifies a purchase an app received (server side, with the store),
    /// binds it to the subject and applies it. Idempotent.
    pub async fn verify_store_purchase(
        &self,
        ctx: &AccessContext,
        provider: &str,
        input: StoreVerifyInput,
    ) -> Result<StorePurchaseRecord> {
        let subject = self.subject_for(ctx, input.subject.as_deref())?;
        self.can_manage(ctx, &subject)?;
        let store = self.store_driver(provider)?;
        let kind = match input.kind.as_deref() {
            None | Some("") => None,
            Some(k) => Some(
                PurchaseKind::parse(k)
                    .ok_or_else(|| BillingError::invalid("invalid_kind", "kind is subscription or one_time"))?,
            ),
        };
        let claim = PurchaseClaim {
            transaction_id: input.transaction_id,
            signed_transaction: input.signed_transaction,
            purchase_token: input.purchase_token,
            product_id: input.product_id,
            kind,
            checkout_id: input.checkout_id,
        };
        let p = store.verify_purchase(&claim).await.map_err(|e| match e {
            // A forged signed transaction is a bad request, not a webhook.
            BillingError::Webhook { code, message } => BillingError::invalid(code, message),
            e => e,
        })?;
        require_test(&p, provider)?;
        let out = self.apply_store(&store, Some(&subject), p).await?;
        match out.record {
            Some(r) => Ok(r),
            None => Err(BillingError::invalid(
                "unknown_purchase",
                format!("the purchase cannot be applied ({})", out.detail),
            )),
        }
    }

    /// Opens a hosted checkout at a store that sells outside apps (Creem)
    /// for a price mapped to one of its products.
    pub async fn store_checkout(
        &self,
        ctx: &AccessContext,
        provider: &str,
        input: StoreCheckoutInput,
    ) -> Result<HostedPage> {
        let subject = self.subject_for(ctx, input.subject.as_deref())?;
        self.can_manage(ctx, &subject)?;
        let store = self.store_driver(provider)?;
        let idem = check_idem(&input.idempotency_key)?;
        let success_url = check_url(&input.success_url)?;
        let price = self.price_by("key", input.price.trim()).await?;
        let rows = self
            .fetch(
                "SELECT product_id FROM _forgebase_billing_store_products WHERE provider = ?1 AND price_id = ?2
                 ORDER BY product_id LIMIT 1",
                vec![provider.into(), price.id.clone().into()],
            )
            .await?;
        let product_id = rows.rows.first().map(|r| text(r, 0)).ok_or_else(|| {
            BillingError::invalid(
                "unknown_store_product",
                "the price is not mapped to a product of this store",
            )
        })?;
        let cust = self.ensure_customer(&subject).await?;
        // The request id is stable per subject and key (store idempotency).
        let request_id = hex(&Sha256::digest(format!("{subject}\n{idem}").as_bytes()))[..32].to_owned();
        store
            .checkout(&StoreCheckoutRequest {
                product_id,
                subject,
                email: cust.email,
                success_url,
                request_id,
            })
            .await
    }

    /// A store server notification (from `handle_webhook`): verified by the
    /// driver, deduplicated by (store, notification id), applied.
    pub(crate) async fn handle_store_notification(
        &self,
        store: Arc<dyn StoreProvider>,
        headers: &[(String, String)],
        body: &[u8],
    ) -> Result<WebhookOutcome> {
        let provider = store.name();
        let now = self.now();
        let settings = self.settings().await?;
        let ev = store.verify_notification(headers, body, now).await?;
        let hash = hex(&Sha256::digest(body));
        if let Some(dup) = self.replayed(provider, &ev.id, &hash).await? {
            return Ok(dup);
        }
        let too_old = ev.created_at < now - settings.webhook_max_age_secs * 1000;
        let applied = match (&ev.purchase, too_old) {
            (_, true) => Ok(StoreOutcome::ignored("too_old")),
            (None, _) => Ok(StoreOutcome::ignored(if ev.event_type.contains("TEST") {
                "test_notification"
            } else {
                "unhandled_type"
            })),
            (Some(p), _) => match require_test(p, provider) {
                Err(e) => Err(e),
                Ok(()) => match self.apply_store(&store, None, p.clone()).await {
                    // Purchases ForgeBase cannot place are recorded, not retried.
                    Err(BillingError::Invalid { code, .. } | BillingError::Conflict { code, .. })
                        if matches!(
                            code,
                            "unknown_store_product" | "store_product_kind" | "purchase_owned" | "already_subscribed"
                        ) =>
                    {
                        Ok(StoreOutcome::ignored(code))
                    }
                    r => r,
                },
            },
        };
        let event = crate::provider::ProviderEvent {
            id: ev.id.clone(),
            event_type: ev.event_type.clone(),
            created_at: ev.created_at,
            action: crate::provider::EventAction::Other,
        };
        let (status, detail) = match &applied {
            Ok(o) => (o.status, Some(o.detail.clone())),
            Err(e) => ("failed", Some(e.to_string())),
        };
        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            crate::webhook::record(&mut t, provider, &event, &hash, status, detail.as_deref(), now).await?;
            Ok(((), true))
        }
        .await;
        self.end(tx, res).await?;
        let o = applied?;
        Ok(WebhookOutcome {
            result: o.status.to_owned(),
            detail: Some(o.detail),
            event_id: ev.id,
        })
    }

    /// Reads store subscriptions past their period end back from the store
    /// (maintenance): a missed renewal or expiry notification heals here.
    pub(crate) async fn refresh_stores(&self) {
        let now = self.now();
        let due = match self
            .fetch(
                &format!(
                    "SELECT {COLUMNS} FROM {FROM} WHERE sp.kind = 'subscription'
                     AND sp.status IN ('active', 'trialing', 'grace', 'on_hold')
                     AND sp.expires_at IS NOT NULL AND sp.expires_at <= ?1
                     AND (sp.refreshed_at IS NULL OR sp.refreshed_at <= ?2)
                     ORDER BY sp.expires_at LIMIT 50"
                ),
                vec![now.into(), (now - 3_600_000).into()],
            )
            .await
        {
            Ok(r) => r.rows.iter().map(|r| link_row(r)).collect::<Vec<_>>(),
            Err(e) => {
                tracing::warn!(error = %e, "store refresh query failed");
                return;
            }
        };
        for l in due {
            let Some(store) = self.store(&l.rec.provider) else {
                continue;
            };
            let mut tx = match self.begin().await {
                Ok(t) => t,
                Err(_) => return,
            };
            let res = async {
                let mut t = Tx(tx.as_mut());
                t.exec(
                    "UPDATE _forgebase_billing_store_purchases SET refreshed_at = ?2 WHERE id = ?1",
                    vec![l.rec.id.clone().into(), now.into()],
                )
                .await?;
                Ok(((), true))
            }
            .await;
            if self.end(tx, res).await.is_err() {
                continue;
            }
            let r = match store
                .refresh(&l.rec.purchase_id, &l.rec.product_id, PurchaseKind::Subscription)
                .await
            {
                Ok(p) if p.test => self.apply_store(&store, Some(&l.rec.subject), p).await.map(|_| ()),
                Ok(_) => Err(BillingError::invalid("live_mode_refused", "not a test purchase")),
                Err(e) => Err(e),
            };
            if let Err(e) = r {
                tracing::warn!(purchase = %l.rec.id, error = %e, "store refresh failed");
            }
        }
    }

    /// Applies a store report (see the module docs).
    pub(crate) async fn apply_store(
        &self,
        store: &Arc<dyn StoreProvider>,
        subject: Option<&str>,
        mut p: StorePurchase,
    ) -> Result<StoreOutcome> {
        let provider = store.name();
        let link = self.link(provider, &p.purchase_id).await?;
        let old = match (&link, &p.replaces) {
            (None, Some(r)) => self.link(provider, r).await?,
            _ => None,
        };
        let known = link.as_ref().or(old.as_ref());
        if p.product_id.is_empty() {
            match known {
                Some(l) => p.product_id = l.rec.product_id.clone(),
                None => return Ok(StoreOutcome::ignored("unknown_purchase")),
            }
        }
        let subject = match (known, subject) {
            (Some(l), Some(s)) if l.rec.subject != s => {
                return Err(BillingError::conflict(
                    "purchase_owned",
                    "this purchase is bound to another account",
                ));
            }
            (Some(l), _) => l.rec.subject.clone(),
            (None, Some(s)) => s.to_owned(),
            (None, None) => match p.subject_hint.as_deref().map(check_subject) {
                Some(Ok(s)) => s,
                _ => return Ok(StoreOutcome::ignored("unknown_purchase")),
            },
        };
        let price = match link.as_ref() {
            // A purchase keeps its price unless the store reports another product.
            Some(l) if l.rec.product_id == p.product_id => self.price_by("key", &l.rec.price).await?,
            _ => self.store_price(provider, &p.product_id).await?,
        };
        let recurring = price.price_type == PriceType::Recurring;
        if recurring != (p.kind == PurchaseKind::Subscription) {
            return Err(BillingError::invalid(
                "store_product_kind",
                "a store subscription maps to a recurring price, a one-time product to a one-time price",
            ));
        }
        let product = self.product_by("id", &price.product_id).await?;
        let cust = self.ensure_customer(&subject).await?;
        let settings = self.settings().await?;
        let now = self.now();
        let txn = p.transaction_id.clone().or_else(|| match p.kind {
            PurchaseKind::OneTime => Some(p.purchase_id.clone()),
            PurchaseKind::Subscription => None,
        });
        let currency = p
            .currency
            .clone()
            .filter(|c| p.amount.is_some() && !c.is_empty())
            .unwrap_or_else(|| price.currency.clone());
        let amount = match (p.amount, p.currency.is_some()) {
            (Some(a), true) => a.max(0),
            _ => price
                .amounts(&currency)
                .map(|(u, _)| u)
                .unwrap_or(price.unit_amount)
                .saturating_mul(p.quantity.max(1)),
        };
        let months = period_months(
            price.interval.unwrap_or(crate::model::Interval::Month),
            price.interval_count,
        );

        let mut tx = self.begin().await?;
        let res = async {
            let mut t = Tx(tx.as_mut());
            let cur = t
                .fetch(
                    &format!("SELECT {COLUMNS} FROM {FROM} WHERE sp.provider = ?1 AND sp.purchase_id = ?2"),
                    vec![provider.into(), p.purchase_id.clone().into()],
                )
                .await?
                .rows
                .first()
                .map(|r| link_row(r));
            let stale = cur.as_ref().is_some_and(|l| p.observed_at < l.last_event_at);
            let apply_state = !stale && !p.refund_only;
            let link_id = cur.as_ref().map(|l| l.rec.id.clone()).unwrap_or_else(|| new_id("sp"));
            let mut sub_id = cur.as_ref().and_then(|l| l.rec.subscription_id.clone());
            let mut order_id = cur.as_ref().and_then(|l| l.rec.order_id.clone());
            let mut detail = if stale { "stale" } else { "processed" };
            let mut paid_invoice = None;
            let mut sub_changed = false;
            if cur.is_none() && p.refund_only {
                return Ok((None, false));
            }
            // An upgrade or plan change replaces an older purchase: its
            // subscription moves over.
            if cur.is_none()
                && let Some(o) = &old
            {
                sub_id = o.rec.subscription_id.clone();
                t.exec(
                    "UPDATE _forgebase_billing_store_purchases SET status = 'replaced', subscription_id = NULL,
                         replaced_by = ?2, auto_renew = 0, updated_at = ?3 WHERE id = ?1",
                    vec![o.rec.id.clone().into(), link_id.clone().into(), now.into()],
                )
                .await?;
            }
            let txn_known = match &txn {
                Some(x) => t
                    .fetch(
                        "SELECT COUNT(*) FROM _forgebase_billing_payments WHERE provider = ?1 AND provider_ref = ?2",
                        vec![provider.into(), x.clone().into()],
                    )
                    .await?
                    .scalar()
                    .and_then(forgebase_db::Value::as_i64)
                    .unwrap_or_default()
                    > 0,
                None => true,
            };
            // A transaction is money the store took: it is invoiced even when
            // its state report is older than the newest one applied.
            let billable = !p.refund_only
                && !txn_known
                && amount > 0
                && matches!(
                    p.state,
                    StoreState::Active | StoreState::Trialing | StoreState::Grace | StoreState::Expired
                );

            let start = p
                .period_start
                .or_else(|| p.expires_at.map(|e| add_months(e, -months)))
                .unwrap_or(p.purchased_at);
            let end = p
                .expires_at
                .unwrap_or_else(|| add_months(start, months))
                .max(start + 1);
            if p.kind == PurchaseKind::Subscription && apply_state {
                let status = sub_status(p.state);
                if sub_id.is_none()
                    && matches!(status, Some("active" | "trialing" | "past_due" | "paused"))
                {
                    if product.kind == ProductKind::Plan {
                        let live = t
                            .fetch(
                                "SELECT COUNT(*) FROM _forgebase_billing_subscriptions s
                                 JOIN _forgebase_billing_subscription_items i ON i.subscription_id = s.id
                                 JOIN _forgebase_billing_prices pr ON pr.id = i.price_id
                                 JOIN _forgebase_billing_products pd ON pd.id = pr.product_id
                                 WHERE s.subject = ?1 AND s.status <> 'canceled' AND pd.kind = 'plan'",
                                vec![subject.clone().into()],
                            )
                            .await?;
                        if live.scalar().and_then(forgebase_db::Value::as_i64).unwrap_or_default() > 0 {
                            return Err(BillingError::conflict(
                                "already_subscribed",
                                "the subject already has a plan from another subscription",
                            ));
                        }
                    }
                    let id = new_id("sub");
                    t.exec(
                        "INSERT INTO _forgebase_billing_subscriptions (id, customer_id, subject, status, currency, interval,
                             interval_count, cycle_anchor, current_period_start, current_period_end, provider,
                             idempotency_key, metadata, created_at, updated_at)
                         VALUES (?1, ?2, ?3, 'incomplete', ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
                        vec![
                            id.clone().into(),
                            cust.id.clone().into(),
                            subject.clone().into(),
                            currency.clone().into(),
                            price.interval.unwrap_or(crate::model::Interval::Month).as_str().into(),
                            price.interval_count.into(),
                            start.into(),
                            end.into(),
                            provider.into(),
                            format!("store:{provider}:{}", p.purchase_id).into(),
                            json!({"store": provider, "purchase_id": p.purchase_id}).to_string().into(),
                            now.into(),
                        ],
                    )
                    .await?;
                    t.exec(
                        "INSERT INTO _forgebase_billing_subscription_items (id, subscription_id, price_id, quantity, created_at)
                         VALUES (?1, ?2, ?3, 1, ?4)",
                        vec![new_id("si").into(), id.clone().into(), price.id.clone().into(), now.into()],
                    )
                    .await?;
                    t.audit("system", "subscription.create", "subscription", &id,
                        json!({"subject": subject, "store": provider, "purchase_id": p.purchase_id}), now)
                        .await?;
                    sub_id = Some(id);
                }
                if let (Some(sid), Some(status)) = (&sub_id, status) {
                    let ended = status == "canceled";
                    let reason = match p.state {
                        StoreState::Refunded => Some("store_refunded"),
                        StoreState::Revoked => Some("store_revoked"),
                        StoreState::Expired => Some("store_expired"),
                        _ => None,
                    };
                    let ended_at = if ended {
                        Some(match p.state {
                            StoreState::Expired => end.min(now),
                            _ => now,
                        })
                    } else {
                        None
                    };
                    let trial_end = (p.state == StoreState::Trialing).then_some(end);
                    let cancel_at_end = !ended && !p.auto_renew;
                    let n = t
                        .exec(
                            "UPDATE _forgebase_billing_subscriptions SET status = ?2, current_period_start = ?3,
                                 current_period_end = ?4, trial_ends_at = ?5, cancel_at_period_end = ?6,
                                 ended_at = ?7, canceled_at = CASE WHEN ?6 = 1 OR CAST(?7 AS BIGINT) IS NOT NULL THEN COALESCE(canceled_at, ?9) END,
                                 cancel_reason = CASE WHEN CAST(?7 AS BIGINT) IS NOT NULL THEN ?8 ELSE NULL END, provider = ?10,
                                 version = version + 1, updated_at = CASE WHEN CAST(?9 AS BIGINT) > updated_at + 1 THEN CAST(?9 AS BIGINT) ELSE updated_at + 1 END
                             WHERE id = ?1 AND NOT (status = ?2 AND current_period_start = ?3 AND current_period_end = ?4
                                 AND trial_ends_at IS NOT DISTINCT FROM ?5 AND cancel_at_period_end = ?6 AND ended_at IS NOT DISTINCT FROM ?7)",
                            vec![
                                sid.clone().into(),
                                status.into(),
                                start.into(),
                                end.into(),
                                trial_end.into(),
                                cancel_at_end.into(),
                                ended_at.into(),
                                reason.into(),
                                now.into(),
                                provider.into(),
                            ],
                        )
                        .await?;
                    // A store plan change (same purchase, other product).
                    let items = t
                        .exec(
                            "UPDATE _forgebase_billing_subscription_items SET price_id = ?2
                             WHERE subscription_id = ?1 AND price_id <> ?2",
                            vec![sid.clone().into(), price.id.clone().into()],
                        )
                        .await?;
                    if items > 0 && n == 0 {
                        t.exec(
                            "UPDATE _forgebase_billing_subscriptions SET version = version + 1,
                                 updated_at = CASE WHEN CAST(?2 AS BIGINT) > updated_at + 1 THEN CAST(?2 AS BIGINT) ELSE updated_at + 1 END WHERE id = ?1",
                            vec![sid.clone().into(), now.into()],
                        )
                        .await?;
                    }
                    sub_changed = n > 0 || items > 0;
                    if sub_changed {
                        detail = match p.state {
                            StoreState::Active | StoreState::Trialing => "subscription_active",
                            StoreState::Grace => "subscription_grace",
                            StoreState::OnHold | StoreState::Paused => "subscription_paused",
                            StoreState::Expired => "subscription_expired",
                            StoreState::Refunded | StoreState::Revoked => "subscription_revoked",
                            StoreState::Pending => "processed",
                        };
                    }
                }
            }
            if p.kind == PurchaseKind::Subscription
                && billable
                && let Some(sid) = &sub_id
            {
                    let first = t
                        .fetch(
                            "SELECT COUNT(*) FROM _forgebase_billing_invoices WHERE subscription_id = ?1",
                            vec![sid.clone().into()],
                        )
                        .await?
                        .scalar()
                        .and_then(forgebase_db::Value::as_i64)
                        .unwrap_or_default()
                        == 0;
                    let inv = self
                        .store_invoice(
                            &mut t,
                            StoreInvoice {
                                provider,
                                customer_id: &cust.id,
                                subject: &subject,
                                subscription_id: Some(sid),
                                order_id: None,
                                price: &price,
                                quantity: 1,
                                amount,
                                currency: &currency,
                                period: Some((start, end)),
                                reason: if first { "store_purchase" } else { "store_renewal" },
                                transaction_id: txn.as_deref().unwrap_or_default(),
                                event_at: p.observed_at,
                                prefix: &settings.invoice_prefix,
                            },
                            now,
                        )
                        .await?;
                    if !sub_changed {
                        detail = "renewed";
                    }
                    paid_invoice = Some(inv);
            }

            if p.kind == PurchaseKind::OneTime && order_id.is_none() && billable {
                let oid = new_id("ord");
                t.exec(
                    "INSERT INTO _forgebase_billing_orders (id, customer_id, subject, status, currency, idempotency_key,
                         created_at, updated_at)
                     VALUES (?1, ?2, ?3, 'paid', ?4, ?5, ?6, ?6)",
                    vec![
                        oid.clone().into(),
                        cust.id.clone().into(),
                        subject.clone().into(),
                        currency.clone().into(),
                        format!("store:{provider}:{}", p.purchase_id).into(),
                        now.into(),
                    ],
                )
                .await?;
                let inv = self
                    .store_invoice(
                        &mut t,
                        StoreInvoice {
                            provider,
                            customer_id: &cust.id,
                            subject: &subject,
                            subscription_id: None,
                            order_id: Some(&oid),
                            price: &price,
                            quantity: p.quantity.max(1),
                            amount,
                            currency: &currency,
                            period: None,
                            reason: "store_purchase",
                            transaction_id: txn.as_deref().unwrap_or_default(),
                            event_at: p.observed_at,
                            prefix: &settings.invoice_prefix,
                        },
                        now,
                    )
                    .await?;
                t.exec(
                    "UPDATE _forgebase_billing_orders SET invoice_id = ?2 WHERE id = ?1",
                    vec![oid.clone().into(), inv.clone().into()],
                )
                .await?;
                order_id = Some(oid);
                paid_invoice = Some(inv);
                detail = "order_paid";
            }

            // The purchase row.
            let state = if apply_state {
                p.state.as_str().to_owned()
            } else {
                cur.as_ref().map(|l| l.rec.status.clone()).unwrap_or_else(|| p.state.as_str().into())
            };
            t.exec(
                "INSERT INTO _forgebase_billing_store_purchases (id, provider, purchase_id, subject, customer_id, product_id,
                     price_id, kind, status, auto_renew, expires_at, environment, subscription_id, order_id,
                     last_transaction_id, last_event_at, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'test', ?12, ?13, ?14, ?15, ?16, ?16)
                 ON CONFLICT (provider, purchase_id) DO UPDATE SET
                     product_id = CASE WHEN CAST(?17 AS BIGINT) = 1 THEN excluded.product_id ELSE _forgebase_billing_store_purchases.product_id END,
                     price_id = CASE WHEN CAST(?17 AS BIGINT) = 1 THEN excluded.price_id ELSE _forgebase_billing_store_purchases.price_id END,
                     status = excluded.status,
                     auto_renew = CASE WHEN CAST(?17 AS BIGINT) = 1 THEN excluded.auto_renew ELSE _forgebase_billing_store_purchases.auto_renew END,
                     expires_at = CASE WHEN CAST(?17 AS BIGINT) = 1 THEN COALESCE(excluded.expires_at, _forgebase_billing_store_purchases.expires_at) ELSE _forgebase_billing_store_purchases.expires_at END,
                     subscription_id = excluded.subscription_id, order_id = excluded.order_id,
                     last_transaction_id = COALESCE(excluded.last_transaction_id, _forgebase_billing_store_purchases.last_transaction_id),
                     last_event_at = CASE WHEN excluded.last_event_at > _forgebase_billing_store_purchases.last_event_at THEN excluded.last_event_at ELSE _forgebase_billing_store_purchases.last_event_at END, updated_at = excluded.updated_at",
                vec![
                    link_id.clone().into(),
                    provider.into(),
                    p.purchase_id.clone().into(),
                    subject.clone().into(),
                    cust.id.clone().into(),
                    p.product_id.clone().into(),
                    price.id.clone().into(),
                    p.kind.as_str().into(),
                    state.into(),
                    (p.auto_renew && apply_state).into(),
                    p.expires_at.into(),
                    sub_id.clone().into(),
                    order_id.clone().into(),
                    if apply_state { p.transaction_id.clone() } else { None }.into(),
                    if p.refund_only { 0 } else { p.observed_at }.into(),
                    now.into(),
                    apply_state.into(),
                ],
            )
            .await?;
            if cur.is_none() {
                t.audit("system", "store.purchase", "store_purchase", &link_id,
                    json!({"provider": provider, "purchase_id": p.purchase_id, "subject": subject, "state": p.state.as_str()}), now)
                    .await?;
            }
            Ok((Some((link_id, detail, paid_invoice, sub_id.filter(|_| sub_changed))), true))
        }
        .await;
        let Some((link_id, mut detail, paid_invoice, changed_sub)) = self.end(tx, res).await? else {
            return Ok(StoreOutcome::ignored("unknown_purchase"));
        };
        if let Some(inv) = &paid_invoice {
            self.after_paid(inv).await;
        }
        if let Some(s) = &changed_sub {
            self.sync_logged(s).await;
        }
        if let Some(r) = &p.refund
            && self.store_refund(provider, r).await?
        {
            detail = "refunded";
        }
        if p.needs_ack
            && let Err(e) = store.acknowledge(&p).await
        {
            tracing::warn!(provider, purchase = %p.purchase_id, error = %e, "store acknowledgement failed");
        }
        let rows = self
            .fetch(
                &format!("SELECT {COLUMNS} FROM {FROM} WHERE sp.id = ?1"),
                vec![link_id.into()],
            )
            .await?;
        Ok(StoreOutcome {
            status: if detail == "stale" { "ignored" } else { "processed" },
            detail: detail.to_owned(),
            record: rows.rows.first().map(|r| link_row(r).rec),
        })
    }

    /// Records a refund the store made: a credit note settled by an
    /// external refund (idempotent by the store's refund reference). Credits
    /// bought with the transaction are taken back. Returns whether something
    /// was refunded now.
    async fn store_refund(&self, provider: &str, r: &crate::provider::store::StoreRefund) -> Result<bool> {
        let rows = self
            .fetch(
                "SELECT p.invoice_id, i.amount_paid + i.balance_applied - i.amount_refunded,
                        (SELECT COUNT(*) FROM _forgebase_billing_credit_notes WHERE idempotency_key = ?3)
                 FROM _forgebase_billing_payments p JOIN _forgebase_billing_invoices i ON i.id = p.invoice_id
                 WHERE p.provider = ?1 AND p.provider_ref = ?2",
                vec![
                    provider.into(),
                    r.transaction_id.clone().into(),
                    format!("store:{provider}:{}", r.refund_ref).into(),
                ],
            )
            .await?;
        let Some(row) = rows.rows.first() else {
            return Ok(false);
        };
        let (invoice, left, seen) = (text(row, 0), int(row, 1), int(row, 2));
        if seen > 0 || left <= 0 {
            return Ok(false);
        }
        self.issue_credit_note(
            "system",
            &invoice,
            CreditNoteInput {
                amount: Some(r.amount.map_or(left, |a| a.clamp(1, left))),
                to_balance: false,
                reason: Some(format!("{provider} refund")),
                idempotency_key: format!("store:{provider}:{}", r.refund_ref),
            },
            Some(&r.refund_ref),
        )
        .await?;
        Ok(true)
    }
}

/// A paid invoice for one store transaction.
struct StoreInvoice<'a> {
    provider: &'a str,
    customer_id: &'a str,
    subject: &'a str,
    subscription_id: Option<&'a str>,
    order_id: Option<&'a str>,
    price: &'a Price,
    quantity: i64,
    amount: i64,
    currency: &'a str,
    period: Option<(i64, i64)>,
    reason: &'static str,
    transaction_id: &'a str,
    event_at: i64,
    prefix: &'a str,
}

impl BillingService {
    /// Writes a paid invoice (one line, no ForgeBase tax: the store is the
    /// merchant of record) and its succeeded payment inside `t`.
    async fn store_invoice(&self, t: &mut Tx<'_>, i: StoreInvoice<'_>, now: i64) -> Result<String> {
        let id = new_id("in");
        t.exec(
            "INSERT INTO _forgebase_billing_invoices (id, customer_id, subject, subscription_id, order_id, status,
                 billing_reason, currency, period_start, period_end, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 'draft', ?6, ?7, ?8, ?9, ?10, ?10)",
            vec![
                id.clone().into(),
                i.customer_id.into(),
                i.subject.into(),
                i.subscription_id.map(str::to_owned).into(),
                i.order_id.map(str::to_owned).into(),
                i.reason.into(),
                i.currency.into(),
                i.period.map(|p| p.0).into(),
                i.period.map(|p| p.1).into(),
                now.into(),
            ],
        )
        .await?;
        t.exec(
            "INSERT INTO _forgebase_billing_invoice_lines (id, invoice_id, position, kind, description, product_key,
                 price_key, quantity, unit_amount, amount, period_start, period_end, metadata)
             VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            vec![
                new_id("il").into(),
                id.clone().into(),
                if i.subscription_id.is_some() {
                    "subscription"
                } else {
                    "one_time"
                }
                .into(),
                format!("{} ({})", i.price.product, i.provider).into(),
                i.price.product.clone().into(),
                i.price.key.clone().into(),
                i.quantity.into(),
                (i.quantity == 1).then_some(i.amount).into(),
                i.amount.into(),
                i.period.map(|p| p.0).into(),
                i.period.map(|p| p.1).into(),
                json!({"store": i.provider, "transaction_id": i.transaction_id})
                    .to_string()
                    .into(),
            ],
        )
        .await?;
        let number = t.next_number(i.prefix).await?;
        t.exec(
            "UPDATE _forgebase_billing_invoices SET number = ?2, status = 'paid', subtotal = ?3, total = ?3,
                 amount_due = ?3, amount_paid = ?3, finalized_at = ?4, paid_at = ?4, updated_at = ?4
             WHERE id = ?1",
            vec![id.clone().into(), number.into(), i.amount.into(), now.into()],
        )
        .await?;
        t.exec(
            "INSERT INTO _forgebase_billing_payments (id, invoice_id, customer_id, subject, provider, provider_ref,
                 status, amount, currency, attempt, provider_event_at, idempotency_key, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'succeeded', ?7, ?8, 1, ?9, ?10, ?11, ?11)",
            vec![
                new_id("pay").into(),
                id.clone().into(),
                i.customer_id.into(),
                i.subject.into(),
                i.provider.into(),
                i.transaction_id.into(),
                i.amount.into(),
                i.currency.into(),
                i.event_at.into(),
                format!("store:{}:{}", i.provider, i.transaction_id).into(),
                now.into(),
            ],
        )
        .await?;
        if let Some(s) = i.subscription_id {
            t.exec(
                "UPDATE _forgebase_billing_subscriptions SET latest_invoice_id = ?2 WHERE id = ?1",
                vec![s.into(), id.clone().into()],
            )
            .await?;
        }
        t.audit(
            "system",
            "invoice.paid",
            "invoice",
            &id,
            json!({"amount": i.amount, "currency": i.currency, "store": i.provider, "transaction_id": i.transaction_id}),
            now,
        )
        .await?;
        Ok(id)
    }
}
