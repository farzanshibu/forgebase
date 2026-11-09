-- FB-63: durable topics and queues with publish/subscribe (ADR-0022).
--
-- A topic keeps every published message for its retention (time and size).
-- A subscription has its own cursor over the topic's message sequence and a
-- delivery row per message it still owes (ready or leased); consumers of one
-- subscription compete for those rows. Times are Unix epoch milliseconds.

CREATE TABLE _forgebase_queue_topics (
    name              TEXT PRIMARY KEY,
    retention_ms      INTEGER NOT NULL,
    retention_bytes   INTEGER,
    max_message_bytes INTEGER NOT NULL,
    dedup_window_ms   INTEGER NOT NULL,
    message_schema    TEXT,
    source_collection TEXT,
    source_ops        TEXT,
    source_after_seq  INTEGER NOT NULL DEFAULT 0,
    labels            TEXT NOT NULL DEFAULT '{}',
    published_total   INTEGER NOT NULL DEFAULT 0,
    published_bytes   INTEGER NOT NULL DEFAULT 0,
    created_by        TEXT NOT NULL,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL
);

CREATE INDEX _forgebase_queue_topics_source ON _forgebase_queue_topics (source_collection)
    WHERE source_collection IS NOT NULL;

CREATE TABLE _forgebase_queue_messages (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    topic        TEXT NOT NULL REFERENCES _forgebase_queue_topics (name) ON DELETE CASCADE,
    id           TEXT NOT NULL UNIQUE,
    data         TEXT NOT NULL,
    attributes   TEXT NOT NULL DEFAULT '{}',
    ordering_key TEXT,
    dedup_id     TEXT,
    bytes        INTEGER NOT NULL,
    publisher    TEXT NOT NULL,
    subject      TEXT,
    -- 1: metered (or not billable), 0: to be metered after commit, 2: refused.
    metered      INTEGER NOT NULL DEFAULT 1,
    -- Delivery history of a dead-lettered message republished to a dead-letter topic.
    history      TEXT,
    published_at INTEGER NOT NULL
);

CREATE INDEX _forgebase_queue_messages_topic ON _forgebase_queue_messages (topic, seq);
CREATE INDEX _forgebase_queue_messages_time ON _forgebase_queue_messages (topic, published_at);
CREATE INDEX _forgebase_queue_messages_unmetered ON _forgebase_queue_messages (seq) WHERE metered = 0;

CREATE TABLE _forgebase_queue_dedup (
    topic      TEXT NOT NULL REFERENCES _forgebase_queue_topics (name) ON DELETE CASCADE,
    dedup_id   TEXT NOT NULL,
    message_id TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (topic, dedup_id)
);

CREATE INDEX _forgebase_queue_dedup_expiry ON _forgebase_queue_dedup (expires_at);

CREATE TABLE _forgebase_queue_subscriptions (
    name                  TEXT PRIMARY KEY,
    topic                 TEXT NOT NULL REFERENCES _forgebase_queue_topics (name) ON DELETE CASCADE,
    filter                TEXT,
    ack_deadline_ms       INTEGER NOT NULL,
    max_delivery_attempts INTEGER,
    backoff_base_ms       INTEGER NOT NULL,
    backoff_max_ms        INTEGER NOT NULL,
    dead_letter_topic     TEXT,
    ordering              INTEGER NOT NULL DEFAULT 0,
    push_kind             TEXT,
    push_endpoint         TEXT,
    push_secret           BLOB,
    -- Highest topic sequence number already turned into delivery rows.
    cursor_seq            INTEGER NOT NULL,
    delivered_total       INTEGER NOT NULL DEFAULT 0,
    acked_total           INTEGER NOT NULL DEFAULT 0,
    nacked_total          INTEGER NOT NULL DEFAULT 0,
    expired_total         INTEGER NOT NULL DEFAULT 0,
    dead_lettered_total   INTEGER NOT NULL DEFAULT 0,
    labels                TEXT NOT NULL DEFAULT '{}',
    created_by            TEXT NOT NULL,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
);

CREATE INDEX _forgebase_queue_subscriptions_topic ON _forgebase_queue_subscriptions (topic);

CREATE TABLE _forgebase_queue_deliveries (
    subscription TEXT NOT NULL REFERENCES _forgebase_queue_subscriptions (name) ON DELETE CASCADE,
    seq          INTEGER NOT NULL REFERENCES _forgebase_queue_messages (seq) ON DELETE CASCADE,
    ordering_key TEXT,
    -- 'ready' (waiting, possibly until available_at) or 'leased'.
    state        TEXT NOT NULL,
    attempts     INTEGER NOT NULL DEFAULT 0,
    available_at INTEGER NOT NULL,
    lease_id     TEXT,
    lease_until  INTEGER,
    consumer     TEXT,
    history      TEXT NOT NULL DEFAULT '[]',
    PRIMARY KEY (subscription, seq)
);

CREATE UNIQUE INDEX _forgebase_queue_deliveries_lease ON _forgebase_queue_deliveries (lease_id)
    WHERE lease_id IS NOT NULL;
CREATE INDEX _forgebase_queue_deliveries_seq ON _forgebase_queue_deliveries (seq);
CREATE INDEX _forgebase_queue_deliveries_ready ON _forgebase_queue_deliveries (subscription, state, available_at);
CREATE INDEX _forgebase_queue_deliveries_key ON _forgebase_queue_deliveries (subscription, ordering_key, seq)
    WHERE ordering_key IS NOT NULL;
CREATE INDEX _forgebase_queue_deliveries_consumer ON _forgebase_queue_deliveries (consumer)
    WHERE state = 'leased';

CREATE TABLE _forgebase_queue_dead_letters (
    id                   INTEGER PRIMARY KEY AUTOINCREMENT,
    subscription         TEXT NOT NULL REFERENCES _forgebase_queue_subscriptions (name) ON DELETE CASCADE,
    topic                TEXT NOT NULL,
    message_id           TEXT NOT NULL,
    data                 TEXT NOT NULL,
    attributes           TEXT NOT NULL,
    ordering_key         TEXT,
    published_at         INTEGER NOT NULL,
    attempts             INTEGER NOT NULL,
    history              TEXT NOT NULL,
    reason               TEXT NOT NULL,
    dead_letter_topic    TEXT,
    forwarded_message_id TEXT,
    at                   INTEGER NOT NULL
);

CREATE INDEX _forgebase_queue_dead_letters_sub ON _forgebase_queue_dead_letters (subscription, id);
CREATE INDEX _forgebase_queue_dead_letters_at ON _forgebase_queue_dead_letters (at);

CREATE TABLE _forgebase_queue_snapshots (
    name         TEXT PRIMARY KEY,
    subscription TEXT NOT NULL REFERENCES _forgebase_queue_subscriptions (name) ON DELETE CASCADE,
    topic        TEXT NOT NULL,
    cursor_seq   INTEGER NOT NULL,
    created_by   TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL
);

CREATE TABLE _forgebase_queue_snapshot_items (
    snapshot TEXT NOT NULL REFERENCES _forgebase_queue_snapshots (name) ON DELETE CASCADE,
    seq      INTEGER NOT NULL,
    PRIMARY KEY (snapshot, seq)
);

-- Per-minute counters for publish and ack rates.
CREATE TABLE _forgebase_queue_rates (
    scope  TEXT NOT NULL,
    name   TEXT NOT NULL,
    metric TEXT NOT NULL,
    minute INTEGER NOT NULL,
    count  INTEGER NOT NULL,
    PRIMARY KEY (scope, name, metric, minute)
);

-- Small key/value state (the realtime-outbox relay cursor).
CREATE TABLE _forgebase_queue_state (
    key   TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
