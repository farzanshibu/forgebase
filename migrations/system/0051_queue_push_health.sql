-- 0051_queue_push_health: secret rotation, endpoint health, the circuit
-- breaker and expiry of subscriptions (ADR-0031 §§4, 5, 7).
--
-- * `push_secret_previous` / `push_secret_previous_until`: the webhook
--   secret a rotation replaced (sealed like `push_secret`) and when its
--   grace window ends. Until then deliveries carry both signatures.
-- * `pause_after_failures`: auto-pause after this many consecutive failed
--   push attempts (NULL: never). `paused_at` / `paused_reason`: a paused
--   push subscription is not delivered (messages accumulate) until resumed.
-- * `consecutive_failures`, `last_success_at`, `last_failure_at`,
--   `last_error`: endpoint health, updated with every push attempt.
-- * `expires_at`: the maintenance pass deletes the subscription after this
--   time (`ttl_ms`; NULL: never). `forgebase queues listen` uses it so a
--   crashed session does not leak its subscription.

ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN push_secret_previous BLOB;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN push_secret_previous_until INTEGER;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN pause_after_failures INTEGER;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN paused_at INTEGER;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN paused_reason TEXT;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN consecutive_failures INTEGER NOT NULL DEFAULT 0;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN last_success_at INTEGER;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN last_failure_at INTEGER;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN last_error TEXT;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN expires_at INTEGER;

CREATE INDEX _forgebase_queue_subscriptions_expiry ON _forgebase_queue_subscriptions (expires_at)
    WHERE expires_at IS NOT NULL;
