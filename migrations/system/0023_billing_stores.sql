-- FB-72 (ADR-0021 §10): merchant-of-record stores (App Store, Google Play,
-- Creem). The store owns the payment and its renewals; ForgeBase verifies
-- what the store reports and mirrors it as subscriptions, orders, paid
-- invoices, refunds and FB-24 entitlements. WeChat Pay is an ordinary
-- payment provider and needs no table of its own.

-- Which billing price a store product is. A subscription product maps to a
-- recurring price, a one-time product (consumable, non-consumable) to a
-- one-time price; the price's product kind decides what the purchase
-- delivers (plan, add-on, usage pack, credits).
CREATE TABLE _forgebase_billing_store_products (
    provider   TEXT    NOT NULL,
    product_id TEXT    NOT NULL,
    price_id   TEXT    NOT NULL REFERENCES _forgebase_billing_prices (id),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (provider, product_id)
) STRICT;

-- One store purchase bound to a billing subject. `purchase_id` is stable
-- across renewals (App Store original transaction id, Google purchase token,
-- Creem subscription or order id) and belongs to one subject only: a receipt
-- cannot be redeemed by a second account. `status` is the store's state;
-- `last_event_at` is the store time of the newest state applied, so an older
-- report arriving late is ignored. A subscription purchase drives
-- `subscription_id` (never renewed or dunned by ForgeBase), a one-time
-- purchase `order_id`. Each store transaction is a paid invoice whose
-- payment carries the transaction id as `provider_ref`.
CREATE TABLE _forgebase_billing_store_purchases (
    id                  TEXT    NOT NULL PRIMARY KEY,
    provider            TEXT    NOT NULL,
    purchase_id         TEXT    NOT NULL,
    subject             TEXT    NOT NULL,
    customer_id         TEXT    NOT NULL REFERENCES _forgebase_billing_customers (id),
    product_id          TEXT    NOT NULL,
    price_id            TEXT    NOT NULL,
    kind                TEXT    NOT NULL CHECK (kind IN ('subscription', 'one_time')),
    status              TEXT    NOT NULL CHECK (status IN ('active', 'trialing', 'grace', 'on_hold', 'paused',
                                                           'expired', 'refunded', 'revoked', 'pending', 'replaced')),
    auto_renew          INTEGER NOT NULL DEFAULT 0,
    expires_at          INTEGER,
    environment         TEXT    NOT NULL DEFAULT 'test',
    subscription_id     TEXT,
    order_id            TEXT,
    last_transaction_id TEXT,
    replaced_by         TEXT,
    last_event_at       INTEGER NOT NULL,
    refreshed_at        INTEGER,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL,
    UNIQUE (provider, purchase_id)
) STRICT;
CREATE INDEX _forgebase_billing_store_purchases_subject ON _forgebase_billing_store_purchases (subject, created_at);
CREATE INDEX _forgebase_billing_store_purchases_subscription ON _forgebase_billing_store_purchases (subscription_id);
CREATE INDEX _forgebase_billing_store_purchases_due ON _forgebase_billing_store_purchases (kind, status, expires_at);
