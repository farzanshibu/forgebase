-- PostgreSQL twin of migrations/system/0051_queue_push_health.sql
-- (ADR-0031 §§4, 5, 7): secret rotation, endpoint health, the circuit
-- breaker and expiry of subscriptions.

ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN push_secret_previous bytea;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN push_secret_previous_until bigint;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN pause_after_failures bigint;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN paused_at bigint;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN paused_reason text;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN consecutive_failures bigint NOT NULL DEFAULT 0;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN last_success_at bigint;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN last_failure_at bigint;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN last_error text;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN expires_at bigint;

CREATE INDEX _forgebase_queue_subscriptions_expiry ON _forgebase_queue_subscriptions (expires_at)
    WHERE expires_at IS NOT NULL;
