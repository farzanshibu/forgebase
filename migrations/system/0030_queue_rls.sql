-- 0030_queue_rls: queue tenancy, subscription owners and record views
-- (FB-141, rls-coverage G2 and G16).
--
-- * `tenant` on topics, subscriptions and snapshots: NULL for an
--   instance-wide resource (every one before FB-141), else the tenant that
--   owns it. A tenant resource is stored under the name `<tenant>/<name>`;
--   queue names never hold '/', so names are unique per tenant and never
--   meet an instance-wide name.
-- * `_forgebase_queue_subscriptions.owner_context`: the creator's access
--   context (principal key, realm, organization, tenant, key scopes) as
--   JSON. Record-source messages are shown to a subscription as this owner
--   sees the record when they are delivered. NULL (subscriptions before
--   FB-141): the creator (`created_by`) in the default realm.
-- * `_forgebase_queue_messages.source_row`: for a message relayed from a
--   record change, the changed row (collection id, writer's tenant and the
--   outbox row snapshot) as JSON; the stored `data.record` is null and each
--   reader gets their own view. Record-source messages stored before FB-141
--   carry the full record of the system view: they are marked 'legacy' and
--   shown only to privileged owners and readers.

ALTER TABLE _forgebase_queue_topics ADD COLUMN tenant TEXT;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN tenant TEXT;
ALTER TABLE _forgebase_queue_subscriptions ADD COLUMN owner_context TEXT;
ALTER TABLE _forgebase_queue_snapshots ADD COLUMN tenant TEXT;
ALTER TABLE _forgebase_queue_messages ADD COLUMN source_row TEXT;

UPDATE _forgebase_queue_messages SET source_row = 'legacy'
    WHERE topic IN (SELECT name FROM _forgebase_queue_topics WHERE source_collection IS NOT NULL);

CREATE INDEX _forgebase_queue_topics_tenant ON _forgebase_queue_topics (tenant) WHERE tenant IS NOT NULL;
CREATE INDEX _forgebase_queue_subscriptions_tenant ON _forgebase_queue_subscriptions (tenant)
    WHERE tenant IS NOT NULL;
CREATE INDEX _forgebase_queue_snapshots_tenant ON _forgebase_queue_snapshots (tenant) WHERE tenant IS NOT NULL;
