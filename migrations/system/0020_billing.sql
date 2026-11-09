-- FB-70 (ADR-0021): billing. A catalog of products and prices (recurring
-- monthly or yearly, one-time, metered; flat, per unit, graduated or volume
-- tiers; one price set per product in several currencies), customers (users
-- or organizations), subscriptions with their lifecycle, one-time orders,
-- invoices with line items and gapless numbers, payments, refunds and credit
-- notes, coupons, tax rates, metered usage taken from FB-24 usage events,
-- payment provider accounts (secrets sealed with the instance keyring) and
-- the webhook events received from them.
--
-- Money is always an INTEGER amount in minor units (cents, paise) next to an
-- ISO 4217 currency code; there are no floating point amounts. Times are
-- epoch milliseconds.

-- Products: what is sold. `kind` says what a paid line delivers
-- (payment-to-entitlement mapping): `plan` sets the FB-24 plan `plan_key`,
-- `addon` adds the FB-24 add-on `addon_key`, `usage_pack` grants the FB-24
-- pack `pack_key`, `credits` tops up `credits` FB-73 credits per unit,
-- `service` delivers nothing automatically.
CREATE TABLE _forgebase_billing_products (
    id          TEXT    NOT NULL PRIMARY KEY,
    key         TEXT    NOT NULL UNIQUE,
    name        TEXT    NOT NULL,
    description TEXT,
    kind        TEXT    NOT NULL CHECK (kind IN ('plan', 'addon', 'usage_pack', 'credits', 'service')),
    plan_key    TEXT,
    addon_key   TEXT,
    pack_key    TEXT,
    credits     INTEGER CHECK (credits IS NULL OR credits > 0),
    active      INTEGER NOT NULL DEFAULT 1,
    metadata    TEXT    NOT NULL DEFAULT '{}',
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
) STRICT;

-- Prices. `currency` and `unit_amount` / `tiers` are the primary amounts;
-- `currency_options` holds the same price in other currencies
-- ({"EUR": {"unit_amount": 900, "tiers": null}}). `type`: `recurring`
-- (charged in advance each interval), `one_time`, `metered` (charged in
-- arrears each interval from usage of `feature_key`). `scheme`: `per_unit`
-- (unit_amount x quantity), `graduated` (each unit at its tier's price) or
-- `volume` (every unit at the price of the tier the total falls in).
CREATE TABLE _forgebase_billing_prices (
    id               TEXT    NOT NULL PRIMARY KEY,
    key              TEXT    NOT NULL UNIQUE,
    product_id       TEXT    NOT NULL REFERENCES _forgebase_billing_products (id),
    type             TEXT    NOT NULL CHECK (type IN ('recurring', 'one_time', 'metered')),
    interval         TEXT    CHECK (interval IS NULL OR interval IN ('month', 'year')),
    interval_count   INTEGER NOT NULL DEFAULT 1 CHECK (interval_count BETWEEN 1 AND 36),
    scheme           TEXT    NOT NULL DEFAULT 'per_unit' CHECK (scheme IN ('per_unit', 'graduated', 'volume')),
    currency         TEXT    NOT NULL,
    unit_amount      INTEGER NOT NULL DEFAULT 0 CHECK (unit_amount >= 0),
    tiers            TEXT,
    currency_options TEXT    NOT NULL DEFAULT '{}',
    feature_key      TEXT,
    meter_basis      TEXT    NOT NULL DEFAULT 'units' CHECK (meter_basis IN ('units', 'overage')),
    trial_days       INTEGER NOT NULL DEFAULT 0 CHECK (trial_days BETWEEN 0 AND 730),
    tax_behavior     TEXT    NOT NULL DEFAULT 'exclusive' CHECK (tax_behavior IN ('exclusive', 'inclusive')),
    active           INTEGER NOT NULL DEFAULT 1,
    metadata         TEXT    NOT NULL DEFAULT '{}',
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
) STRICT;
CREATE INDEX _forgebase_billing_prices_product ON _forgebase_billing_prices (product_id);

-- Tax rates per region (`IN`, `DE`, `US-CA`): a fixed rate in basis points
-- (1800 = 18 %). No tax engine: the operator keeps the rates.
CREATE TABLE _forgebase_billing_tax_rates (
    id         TEXT    NOT NULL PRIMARY KEY,
    region     TEXT    NOT NULL UNIQUE,
    name       TEXT    NOT NULL,
    rate_bp    INTEGER NOT NULL CHECK (rate_bp BETWEEN 0 AND 10000),
    active     INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

-- Coupons: `percent` (basis points) or `amount` (minor units of `currency`)
-- off, for one invoice (`once`), `duration_cycles` invoices (`repeating`) or
-- every invoice (`forever`).
CREATE TABLE _forgebase_billing_coupons (
    id              TEXT    NOT NULL PRIMARY KEY,
    code            TEXT    NOT NULL UNIQUE,
    name            TEXT,
    kind            TEXT    NOT NULL CHECK (kind IN ('percent', 'amount')),
    percent_bp      INTEGER CHECK (percent_bp IS NULL OR percent_bp BETWEEN 1 AND 10000),
    amount_off      INTEGER CHECK (amount_off IS NULL OR amount_off > 0),
    currency        TEXT,
    duration        TEXT    NOT NULL CHECK (duration IN ('once', 'repeating', 'forever')),
    duration_cycles INTEGER,
    max_redemptions INTEGER,
    redeemed        INTEGER NOT NULL DEFAULT 0,
    expires_at      INTEGER,
    products        TEXT    NOT NULL DEFAULT '[]',
    active          INTEGER NOT NULL DEFAULT 1,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    CHECK (max_redemptions IS NULL OR redeemed <= max_redemptions)
) STRICT;

-- Customers: one per billing subject (`org:<id>` or a principal key).
-- `balance` is credit owed to the customer (minor units of `currency`, from
-- downgrades and credit notes) and applied to the next invoices. Payment
-- methods are provider references only; card numbers never reach ForgeBase.
CREATE TABLE _forgebase_billing_customers (
    id                    TEXT    NOT NULL PRIMARY KEY,
    subject               TEXT    NOT NULL UNIQUE,
    email                 TEXT,
    name                  TEXT,
    currency              TEXT    NOT NULL,
    region                TEXT,
    tax_exempt            INTEGER NOT NULL DEFAULT 0,
    provider              TEXT,
    provider_customer_id  TEXT,
    payment_method        TEXT,
    payment_method_label  TEXT,
    balance               INTEGER NOT NULL DEFAULT 0 CHECK (balance >= 0),
    metadata              TEXT    NOT NULL DEFAULT '{}',
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
) STRICT;

-- Subscriptions. `version` increases on every change; `synced_version` is
-- the last version applied to FB-24 (`BillingSync`), so a lost sync is
-- retried by the maintenance job. `pending_price_id` is a downgrade
-- scheduled for the end of the period.
CREATE TABLE _forgebase_billing_subscriptions (
    id                   TEXT    NOT NULL PRIMARY KEY,
    customer_id          TEXT    NOT NULL REFERENCES _forgebase_billing_customers (id),
    subject              TEXT    NOT NULL,
    status               TEXT    NOT NULL CHECK (status IN ('incomplete', 'trialing', 'active', 'past_due', 'paused', 'canceled')),
    currency             TEXT    NOT NULL,
    interval             TEXT    NOT NULL CHECK (interval IN ('month', 'year')),
    interval_count       INTEGER NOT NULL DEFAULT 1,
    cycle_anchor         INTEGER NOT NULL,
    current_period_start INTEGER NOT NULL,
    current_period_end   INTEGER NOT NULL,
    trial_ends_at        INTEGER,
    cancel_at_period_end INTEGER NOT NULL DEFAULT 0,
    canceled_at          INTEGER,
    ended_at             INTEGER,
    cancel_reason        TEXT,
    paused_at            INTEGER,
    coupon_id            TEXT    REFERENCES _forgebase_billing_coupons (id),
    coupon_cycles_left   INTEGER,
    pending_price_id     TEXT,
    provider             TEXT,
    latest_invoice_id    TEXT,
    idempotency_key      TEXT,
    version              INTEGER NOT NULL DEFAULT 1,
    synced_version       INTEGER NOT NULL DEFAULT 0,
    metadata             TEXT    NOT NULL DEFAULT '{}',
    created_at           INTEGER NOT NULL,
    updated_at           INTEGER NOT NULL,
    UNIQUE (customer_id, idempotency_key)
) STRICT;
CREATE INDEX _forgebase_billing_subscriptions_subject ON _forgebase_billing_subscriptions (subject, status);
CREATE INDEX _forgebase_billing_subscriptions_due ON _forgebase_billing_subscriptions (status, current_period_end);

CREATE TABLE _forgebase_billing_subscription_items (
    id              TEXT    NOT NULL PRIMARY KEY,
    subscription_id TEXT    NOT NULL REFERENCES _forgebase_billing_subscriptions (id),
    price_id        TEXT    NOT NULL REFERENCES _forgebase_billing_prices (id),
    quantity        INTEGER NOT NULL DEFAULT 1 CHECK (quantity >= 1),
    created_at      INTEGER NOT NULL,
    UNIQUE (subscription_id, price_id)
) STRICT;

-- One-time orders (checkout of one-time prices: credit top-ups, packs).
CREATE TABLE _forgebase_billing_orders (
    id              TEXT    NOT NULL PRIMARY KEY,
    customer_id     TEXT    NOT NULL REFERENCES _forgebase_billing_customers (id),
    subject         TEXT    NOT NULL,
    status          TEXT    NOT NULL CHECK (status IN ('open', 'paid', 'canceled', 'refunded')),
    currency        TEXT    NOT NULL,
    invoice_id      TEXT,
    idempotency_key TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    UNIQUE (customer_id, idempotency_key)
) STRICT;

-- Invoices. A `number` is assigned when the draft is finalized (gapless per
-- prefix, `_forgebase_billing_sequences`). Amounts: subtotal (lines before
-- discount), discount, tax (exclusive tax added; inclusive tax is part of
-- the lines; negative on a downgrade credit), total, balance applied (customer credit), amount_due, paid,
-- refunded (credit notes). `fulfilled_at`: the paid lines were delivered
-- (entitlements, credits, packs); NULL on a paid invoice means retry.
CREATE TABLE _forgebase_billing_invoices (
    id               TEXT    NOT NULL PRIMARY KEY,
    number           TEXT    UNIQUE,
    customer_id      TEXT    NOT NULL REFERENCES _forgebase_billing_customers (id),
    subject          TEXT    NOT NULL,
    subscription_id  TEXT,
    order_id         TEXT,
    status           TEXT    NOT NULL CHECK (status IN ('draft', 'open', 'paid', 'void', 'uncollectible')),
    billing_reason   TEXT    NOT NULL,
    currency         TEXT    NOT NULL,
    subtotal         INTEGER NOT NULL DEFAULT 0,
    discount         INTEGER NOT NULL DEFAULT 0 CHECK (discount >= 0),
    tax              INTEGER NOT NULL DEFAULT 0,
    total            INTEGER NOT NULL DEFAULT 0,
    balance_applied  INTEGER NOT NULL DEFAULT 0 CHECK (balance_applied >= 0),
    amount_due       INTEGER NOT NULL DEFAULT 0 CHECK (amount_due >= 0),
    amount_paid      INTEGER NOT NULL DEFAULT 0 CHECK (amount_paid >= 0),
    amount_refunded  INTEGER NOT NULL DEFAULT 0 CHECK (amount_refunded >= 0 AND amount_refunded <= amount_paid + balance_applied),
    coupon_id        TEXT,
    tax_region       TEXT,
    period_start     INTEGER,
    period_end       INTEGER,
    attempt_count    INTEGER NOT NULL DEFAULT 0,
    next_attempt_at  INTEGER,
    hosted_url       TEXT,
    finalized_at     INTEGER,
    paid_at          INTEGER,
    voided_at        INTEGER,
    fulfilled_at     INTEGER,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
) STRICT;
CREATE INDEX _forgebase_billing_invoices_subject ON _forgebase_billing_invoices (subject, created_at);
CREATE INDEX _forgebase_billing_invoices_dunning ON _forgebase_billing_invoices (status, next_attempt_at);
CREATE INDEX _forgebase_billing_invoices_subscription ON _forgebase_billing_invoices (subscription_id);

-- Invoice lines. `amount` is signed (proration credits and applied
-- discounts are negative); `tax_amount` is the tax of the line, included in
-- `amount` when `tax_inclusive`.
CREATE TABLE _forgebase_billing_invoice_lines (
    id              TEXT    NOT NULL PRIMARY KEY,
    invoice_id      TEXT    NOT NULL REFERENCES _forgebase_billing_invoices (id),
    position        INTEGER NOT NULL,
    kind            TEXT    NOT NULL CHECK (kind IN ('subscription', 'proration', 'usage', 'one_time', 'adjustment')),
    description     TEXT    NOT NULL,
    product_id      TEXT,
    price_id        TEXT,
    feature_key     TEXT,
    quantity        INTEGER NOT NULL DEFAULT 1,
    unit_amount     INTEGER,
    amount          INTEGER NOT NULL,
    discount_amount INTEGER NOT NULL DEFAULT 0,
    tax_rate_bp     INTEGER NOT NULL DEFAULT 0,
    tax_amount      INTEGER NOT NULL DEFAULT 0,
    tax_inclusive   INTEGER NOT NULL DEFAULT 0,
    period_start    INTEGER,
    period_end      INTEGER,
    metadata        TEXT    NOT NULL DEFAULT '{}'
) STRICT;
CREATE INDEX _forgebase_billing_invoice_lines_invoice ON _forgebase_billing_invoice_lines (invoice_id, position);

-- Gapless number sequences (`INV`, `CN`).
CREATE TABLE _forgebase_billing_sequences (
    prefix TEXT    NOT NULL PRIMARY KEY,
    next   INTEGER NOT NULL
) STRICT;

-- Payments: one attempt to collect an invoice. `provider_ref` is the
-- provider's payment id (Stripe PaymentIntent, Razorpay payment).
-- `provider_event_at` is the provider time of the newest webhook applied, so
-- an older event arriving late is ignored.
CREATE TABLE _forgebase_billing_payments (
    id                TEXT    NOT NULL PRIMARY KEY,
    invoice_id        TEXT    NOT NULL REFERENCES _forgebase_billing_invoices (id),
    customer_id       TEXT    NOT NULL,
    subject           TEXT    NOT NULL,
    provider          TEXT    NOT NULL,
    provider_ref      TEXT,
    status            TEXT    NOT NULL CHECK (status IN ('pending', 'succeeded', 'failed', 'canceled')),
    amount            INTEGER NOT NULL CHECK (amount >= 0),
    currency          TEXT    NOT NULL,
    amount_refunded   INTEGER NOT NULL DEFAULT 0 CHECK (amount_refunded >= 0 AND amount_refunded <= amount),
    failure_code      TEXT,
    failure_message   TEXT,
    attempt           INTEGER NOT NULL DEFAULT 1,
    provider_event_at INTEGER,
    idempotency_key   TEXT    NOT NULL UNIQUE,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
) STRICT;
CREATE UNIQUE INDEX _forgebase_billing_payments_ref ON _forgebase_billing_payments (provider, provider_ref) WHERE provider_ref IS NOT NULL;
CREATE INDEX _forgebase_billing_payments_invoice ON _forgebase_billing_payments (invoice_id);
CREATE INDEX _forgebase_billing_payments_at ON _forgebase_billing_payments (status, updated_at);

-- Credit notes: a numbered document reducing what an invoice charged,
-- settled by a refund to the payment method and/or a credit to the
-- customer balance.
CREATE TABLE _forgebase_billing_credit_notes (
    id              TEXT    NOT NULL PRIMARY KEY,
    number          TEXT    NOT NULL UNIQUE,
    invoice_id      TEXT    NOT NULL REFERENCES _forgebase_billing_invoices (id),
    customer_id     TEXT    NOT NULL,
    subject         TEXT    NOT NULL,
    currency        TEXT    NOT NULL,
    amount          INTEGER NOT NULL CHECK (amount > 0),
    refund_amount   INTEGER NOT NULL DEFAULT 0 CHECK (refund_amount >= 0),
    balance_amount  INTEGER NOT NULL DEFAULT 0 CHECK (balance_amount >= 0),
    reason          TEXT,
    credits_reversed INTEGER NOT NULL DEFAULT 0,
    idempotency_key TEXT    NOT NULL UNIQUE,
    created_at      INTEGER NOT NULL,
    CHECK (refund_amount + balance_amount = amount)
) STRICT;

CREATE TABLE _forgebase_billing_refunds (
    id                TEXT    NOT NULL PRIMARY KEY,
    payment_id        TEXT    NOT NULL REFERENCES _forgebase_billing_payments (id),
    invoice_id        TEXT    NOT NULL,
    credit_note_id    TEXT,
    provider          TEXT    NOT NULL,
    provider_ref      TEXT,
    status            TEXT    NOT NULL CHECK (status IN ('pending', 'succeeded', 'failed')),
    amount            INTEGER NOT NULL CHECK (amount > 0),
    currency          TEXT    NOT NULL,
    reason            TEXT,
    provider_event_at INTEGER,
    idempotency_key   TEXT    NOT NULL UNIQUE,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
) STRICT;

-- Metered usage: one row per FB-24 usage event of a metered price's feature
-- (idempotent by event id), billed by the invoice of the period it fell in.
CREATE TABLE _forgebase_billing_usage_records (
    event_id        TEXT    NOT NULL PRIMARY KEY,
    seq             INTEGER NOT NULL,
    subscription_id TEXT    NOT NULL,
    price_id        TEXT    NOT NULL,
    subject         TEXT    NOT NULL,
    feature_key     TEXT    NOT NULL,
    quantity        INTEGER NOT NULL,
    at              INTEGER NOT NULL,
    invoice_id      TEXT
) STRICT;
CREATE INDEX _forgebase_billing_usage_unbilled ON _forgebase_billing_usage_records (subscription_id, price_id, invoice_id, at);

CREATE TABLE _forgebase_billing_cursors (
    name       TEXT    NOT NULL PRIMARY KEY,
    seq        INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

-- Provider accounts (test mode only). `secret` is the sealed JSON of the
-- API secret and the webhook signing secret; it is never returned.
CREATE TABLE _forgebase_billing_providers (
    provider   TEXT    NOT NULL PRIMARY KEY,
    key_id     TEXT,
    secret     TEXT    NOT NULL,
    api_base   TEXT,
    hint       TEXT,
    enabled    INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

-- Webhook events received. Unique per (provider, event id): a replay is
-- answered from this row and changes nothing. `status`: `processed`,
-- `ignored` (unknown type, stale, unknown object) or `failed` (retried when
-- the provider redelivers).
CREATE TABLE _forgebase_billing_webhook_events (
    id           TEXT    NOT NULL PRIMARY KEY,
    provider     TEXT    NOT NULL,
    event_id     TEXT    NOT NULL,
    event_type   TEXT    NOT NULL,
    event_at     INTEGER NOT NULL,
    received_at  INTEGER NOT NULL,
    payload_hash TEXT    NOT NULL,
    status       TEXT    NOT NULL CHECK (status IN ('processed', 'ignored', 'failed')),
    result       TEXT,
    attempts     INTEGER NOT NULL DEFAULT 1,
    processed_at INTEGER,
    UNIQUE (provider, event_id)
) STRICT;

-- Operator settings (dunning schedule, number prefixes).
CREATE TABLE _forgebase_billing_settings (
    key        TEXT    NOT NULL PRIMARY KEY,
    value      TEXT    NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

-- Append-only audit of billing actions.
CREATE TABLE _forgebase_billing_audit (
    seq       INTEGER PRIMARY KEY AUTOINCREMENT,
    at        INTEGER NOT NULL,
    actor     TEXT    NOT NULL,
    action    TEXT    NOT NULL,
    object    TEXT    NOT NULL,
    object_id TEXT    NOT NULL,
    detail    TEXT    NOT NULL DEFAULT '{}'
) STRICT;
CREATE INDEX _forgebase_billing_audit_object ON _forgebase_billing_audit (object, object_id);
CREATE TRIGGER _forgebase_billing_audit_no_update BEFORE UPDATE ON _forgebase_billing_audit
BEGIN
    SELECT RAISE(ABORT, 'billing audit entries are immutable');
END;
CREATE TRIGGER _forgebase_billing_audit_no_delete BEFORE DELETE ON _forgebase_billing_audit
BEGIN
    SELECT RAISE(ABORT, 'billing audit entries are immutable');
END;
-- Lines of a finalized invoice are fixed.
CREATE TRIGGER _forgebase_billing_lines_frozen_update BEFORE UPDATE ON _forgebase_billing_invoice_lines
WHEN (SELECT status FROM _forgebase_billing_invoices WHERE id = OLD.invoice_id) <> 'draft'
BEGIN
    SELECT RAISE(ABORT, 'lines of a finalized invoice are immutable');
END;
CREATE TRIGGER _forgebase_billing_lines_frozen_delete BEFORE DELETE ON _forgebase_billing_invoice_lines
WHEN (SELECT status FROM _forgebase_billing_invoices WHERE id = OLD.invoice_id) <> 'draft'
BEGIN
    SELECT RAISE(ABORT, 'lines of a finalized invoice are immutable');
END;
CREATE TRIGGER _forgebase_billing_lines_frozen_insert BEFORE INSERT ON _forgebase_billing_invoice_lines
WHEN (SELECT status FROM _forgebase_billing_invoices WHERE id = NEW.invoice_id) <> 'draft'
BEGIN
    SELECT RAISE(ABORT, 'lines of a finalized invoice are immutable');
END;
