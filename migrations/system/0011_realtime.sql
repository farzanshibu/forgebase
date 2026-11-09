-- 0011_realtime: transactional outbox for realtime events (FB-40, ADR-0011).
--
-- Every record change made through the data service inserts one row here in
-- the same transaction as the change, so an event exists if and only if its
-- change committed. `seq` is the event cursor: AUTOINCREMENT never reuses a
-- value, even after old rows are pruned, and with SQLite's single writer seq
-- order is commit order, so "every event after cursor N" has no gaps.
-- `snapshot` is the full row ({"c": columns, "v": values}; password columns
-- left out) after the change, or before it for deletes; delivery re-checks it
-- against each subscriber's current access.

CREATE TABLE _forgebase_realtime_outbox (
    seq           INTEGER PRIMARY KEY AUTOINCREMENT,
    op            TEXT NOT NULL CHECK (op IN ('create', 'update', 'delete')),
    collection_id TEXT NOT NULL,
    collection    TEXT NOT NULL,
    record_id     TEXT NOT NULL,
    tenant        TEXT,
    actor         TEXT NOT NULL,
    snapshot      TEXT NOT NULL CHECK (json_valid(snapshot)),
    created_at    TEXT NOT NULL
) STRICT;

CREATE INDEX _forgebase_realtime_outbox_created ON _forgebase_realtime_outbox (created_at);
