-- ADR-0032: the inbound billing webhook inspector. One row per webhook
-- request that reached `POST /api/v1/billing/webhooks/{provider}`, verified
-- or not, next to the per-event dedup table `_forgebase_billing_webhook_events`
-- (which it links to once a delivery is verified).
--
-- Unverified deliveries are only stored within a per-provider budget (count
-- and body bytes per minute, a total cap), so forged traffic cannot fill the
-- disk; `body` is the raw request body up to the configured cap (`body_truncated`
-- when longer, NULL when the budget dropped it). `headers` is a JSON object of
-- allowlisted headers only (never `authorization` or cookies); `stored_bytes`
-- is what the row keeps of both (the total cap counts it). `claim` /
-- `claimed_at` let one operator re-verification run at a time. Rows older than
-- `[billing] webhook_log_retention_days` are pruned by the billing maintenance job.
CREATE TABLE _forgebase_billing_webhook_deliveries (
    id                TEXT    NOT NULL PRIMARY KEY,
    provider          TEXT    NOT NULL,
    account           TEXT,
    received_at       INTEGER NOT NULL,
    client_addr       TEXT,
    headers           TEXT    NOT NULL DEFAULT '{}',
    body              BLOB,
    body_size         INTEGER NOT NULL,
    body_sha256       TEXT    NOT NULL,
    body_truncated    INTEGER NOT NULL DEFAULT 0 CHECK (body_truncated IN (0, 1)),
    stored_bytes      INTEGER NOT NULL DEFAULT 0,
    verification      TEXT    NOT NULL CHECK (verification IN ('verified', 'signature_invalid',
                          'timestamp_out_of_tolerance', 'unknown_provider', 'malformed', 'rejected', 'error')),
    reason            TEXT,
    event_id          TEXT,
    event_type        TEXT,
    processing        TEXT    CHECK (processing IN ('applied', 'ignored', 'duplicate', 'failed')),
    outcome           TEXT    NOT NULL,
    detail            TEXT,
    webhook_event_id  TEXT,
    customer_id       TEXT,
    subscription_id   TEXT,
    order_id          TEXT,
    invoice_id        TEXT,
    payment_id        TEXT,
    refund_id         TEXT,
    store_purchase_id TEXT,
    latency_us        INTEGER,
    replays           INTEGER NOT NULL DEFAULT 0,
    reverify_attempts INTEGER NOT NULL DEFAULT 0,
    reverified_at     INTEGER,
    claim             TEXT,
    claimed_at        INTEGER,
    updated_at        INTEGER NOT NULL
) STRICT;
CREATE INDEX _forgebase_billing_webhook_deliveries_received ON _forgebase_billing_webhook_deliveries (received_at);
CREATE INDEX _forgebase_billing_webhook_deliveries_provider ON _forgebase_billing_webhook_deliveries (provider, received_at);
CREATE INDEX _forgebase_billing_webhook_deliveries_outcome ON _forgebase_billing_webhook_deliveries (outcome, received_at);
CREATE INDEX _forgebase_billing_webhook_deliveries_verification ON _forgebase_billing_webhook_deliveries (verification, received_at);
CREATE INDEX _forgebase_billing_webhook_deliveries_event ON _forgebase_billing_webhook_deliveries (provider, event_id);
