-- 0050_queue_push_attempts: the delivery attempt log of push subscriptions
-- (ADR-0031 §1).
--
-- One row per webhook or function call a push subscription made, including
-- test sends (`test = 1`, `message_id` 'test-…'): HTTP status, latency,
-- error class, the request headers sent (signature masked), allowlisted
-- response headers and the response body truncated to `[queue]
-- push_response_body_bytes`. Kept per subscription for the last
-- `push_attempts_per_subscription` rows and at most
-- `push_attempts_retention_secs`; the maintenance pass prunes the rest.
-- Times are Unix epoch milliseconds; flags are 0/1.

CREATE TABLE _forgebase_queue_push_attempts (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    subscription       TEXT NOT NULL REFERENCES _forgebase_queue_subscriptions (name) ON DELETE CASCADE,
    message_id         TEXT NOT NULL,
    attempt            INTEGER NOT NULL,
    test               INTEGER NOT NULL DEFAULT 0,
    kind               TEXT NOT NULL,
    endpoint           TEXT NOT NULL,
    at                 INTEGER NOT NULL,
    latency_ms         INTEGER NOT NULL,
    outcome            TEXT NOT NULL CHECK (outcome IN ('success', 'failure')),
    status             INTEGER,
    error_class        TEXT,
    error              TEXT,
    request_headers    TEXT NOT NULL DEFAULT '{}',
    response_headers   TEXT NOT NULL DEFAULT '{}',
    response_body      TEXT,
    response_bytes     INTEGER,
    response_truncated INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX _forgebase_queue_push_attempts_sub ON _forgebase_queue_push_attempts (subscription, id);
CREATE INDEX _forgebase_queue_push_attempts_message ON _forgebase_queue_push_attempts (subscription, message_id);
CREATE INDEX _forgebase_queue_push_attempts_at ON _forgebase_queue_push_attempts (at);
