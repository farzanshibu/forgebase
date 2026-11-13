-- ADR-0032: the inbound billing webhook inspector (see the SQLite twin
-- `system/0053_billing_webhook_deliveries.sql` for the column notes). One row
-- per webhook request, verified or not; unverified ones only within a
-- per-provider budget; raw body up to a cap; allowlisted headers only.
CREATE TABLE _forgebase_billing_webhook_deliveries (
    id                text   NOT NULL PRIMARY KEY,
    provider          text   NOT NULL,
    account           text,
    received_at       bigint NOT NULL,
    client_addr       text,
    headers           text   NOT NULL DEFAULT '{}',
    body              bytea,
    body_size         bigint NOT NULL,
    body_sha256       text   NOT NULL,
    body_truncated    smallint NOT NULL DEFAULT 0 CHECK (body_truncated IN (0, 1)),
    stored_bytes      bigint NOT NULL DEFAULT 0,
    verification      text   NOT NULL CHECK (verification IN ('verified', 'signature_invalid',
                          'timestamp_out_of_tolerance', 'unknown_provider', 'malformed', 'rejected', 'error')),
    reason            text,
    event_id          text,
    event_type        text,
    processing        text   CHECK (processing IN ('applied', 'ignored', 'duplicate', 'failed')),
    outcome           text   NOT NULL,
    detail            text,
    webhook_event_id  text,
    customer_id       text,
    subscription_id   text,
    order_id          text,
    invoice_id        text,
    payment_id        text,
    refund_id         text,
    store_purchase_id text,
    latency_us        bigint,
    replays           bigint NOT NULL DEFAULT 0,
    reverify_attempts bigint NOT NULL DEFAULT 0,
    reverified_at     bigint,
    claim             text,
    claimed_at        bigint,
    updated_at        bigint NOT NULL
);
CREATE INDEX _forgebase_billing_webhook_deliveries_received ON _forgebase_billing_webhook_deliveries (received_at);
CREATE INDEX _forgebase_billing_webhook_deliveries_provider ON _forgebase_billing_webhook_deliveries (provider, received_at);
CREATE INDEX _forgebase_billing_webhook_deliveries_outcome ON _forgebase_billing_webhook_deliveries (outcome, received_at);
CREATE INDEX _forgebase_billing_webhook_deliveries_verification ON _forgebase_billing_webhook_deliveries (verification, received_at);
CREATE INDEX _forgebase_billing_webhook_deliveries_event ON _forgebase_billing_webhook_deliveries (provider, event_id);

-- System tables created after 0040 enable row-level security themselves
-- (FB-152: no policies, so only trusted sessions read them directly).
ALTER TABLE _forgebase_billing_webhook_deliveries ENABLE ROW LEVEL SECURITY;
