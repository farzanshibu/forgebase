-- 0052_queue_redrive_catalog: dead-letter redrive and the event catalog
-- (ADR-0031 §§3, 6).
--
-- * `_forgebase_queue_dead_letters.redriven_at` / `redriven_by`: set once,
--   in the transaction that delivers the dead letter to its subscription
--   again, so a record is never redriven twice.
-- * `_forgebase_queue_messages.only_subscription`: a redriven dead letter
--   whose message retention already dropped is stored again for that one
--   subscription (same id, data, attributes and ordering key); other
--   subscriptions of the topic skip it. NULL for every published message.
-- * `_forgebase_queue_topics.description` / `example`: what the topic's
--   events are and an example of their data (JSON text), for the catalog.

ALTER TABLE _forgebase_queue_dead_letters ADD COLUMN redriven_at INTEGER;
ALTER TABLE _forgebase_queue_dead_letters ADD COLUMN redriven_by TEXT;
ALTER TABLE _forgebase_queue_messages ADD COLUMN only_subscription TEXT;
ALTER TABLE _forgebase_queue_topics ADD COLUMN description TEXT;
ALTER TABLE _forgebase_queue_topics ADD COLUMN example TEXT;
