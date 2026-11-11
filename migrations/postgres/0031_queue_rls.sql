-- PostgreSQL twin of migrations/system/0031_queue_rls.sql (FB-141,
-- rls-coverage G2 and G16): tenant of topics, subscriptions and snapshots
-- (a tenant resource is stored as `<tenant>/<name>`), the recorded owner
-- context of a subscription, and the source row of record-change messages
-- (pre-FB-141 record-change messages are marked 'legacy').

ALTER TABLE _forgebase_queue_topics ADD COLUMN tenant text;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN tenant text;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN owner_context text;
ALTER TABLE _forgebase_queue_snapshots ADD COLUMN tenant text;
ALTER TABLE _forgebase_queue_messages ADD COLUMN source_row text;

UPDATE _forgebase_queue_messages SET source_row = 'legacy'
    WHERE topic IN (SELECT name FROM _forgebase_queue_topics WHERE source_collection IS NOT NULL);

CREATE INDEX _forgebase_queue_topics_tenant ON _forgebase_queue_topics (tenant) WHERE tenant IS NOT NULL;
CREATE INDEX _forgebase_queue_subscriptions_tenant ON _forgebase_queue_subscriptions (tenant)
    WHERE tenant IS NOT NULL;
CREATE INDEX _forgebase_queue_snapshots_tenant ON _forgebase_queue_snapshots (tenant) WHERE tenant IS NOT NULL;
