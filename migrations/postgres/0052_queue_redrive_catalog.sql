-- PostgreSQL twin of migrations/system/0052_queue_redrive_catalog.sql
-- (ADR-0031 §§3, 6): dead-letter redrive marks, subscription-addressed
-- copies of redriven messages, and topic descriptions and examples.

ALTER TABLE _forgebase_queue_dead_letters ADD COLUMN redriven_at bigint;
ALTER TABLE _forgebase_queue_dead_letters ADD COLUMN redriven_by text;
ALTER TABLE _forgebase_queue_messages ADD COLUMN only_subscription text;
ALTER TABLE _forgebase_queue_topics ADD COLUMN description text;
ALTER TABLE _forgebase_queue_topics ADD COLUMN example text;
