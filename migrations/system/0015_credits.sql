-- FB-73 (ADR-0016): credits. An append-only ledger per subject (a user, a
-- service account or an organization): every top-up, grant, consumption,
-- spend, refund, reversal and expiry is one ledger row. Balances are derived
-- from the ledger and kept on the account row for the single-writer balance
-- check; `_forgebase_credit_lots` holds what is left of each top-up or grant
-- (expiry works per lot) and `_forgebase_credit_allocations` records every
-- change to a lot, so that
--
--   account.balance = SUM(ledger.amount) = SUM(lots.remaining)
--   lot.remaining   = SUM(allocations.amount of the lot)
--   entry.amount    = SUM(allocations.amount of the entry)
--
-- hold at every commit (CreditService::audit checks them). Times are epoch
-- milliseconds. Amounts are whole credits.

-- One row per subject holding credits. `balance` never goes below zero: a
-- debit is `UPDATE ... SET balance = balance - n WHERE balance >= n`.
CREATE TABLE _forgebase_credit_accounts (
    subject    TEXT    NOT NULL PRIMARY KEY,
    balance    INTEGER NOT NULL DEFAULT 0 CHECK (balance >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

-- The ledger. Immutable: triggers refuse UPDATE and DELETE. `seq` is the
-- cursor for readers (statistics, FB-70/71). Credits are positive, debits
-- negative; `balance_after` is the account balance right after the entry.
CREATE TABLE _forgebase_credit_ledger (
    seq             INTEGER PRIMARY KEY AUTOINCREMENT,
    id              TEXT    NOT NULL UNIQUE,
    at              INTEGER NOT NULL,
    subject         TEXT    NOT NULL,
    kind            TEXT    NOT NULL CHECK (kind IN ('top_up', 'grant', 'consumption', 'spend', 'refund', 'reversal', 'expiry')),
    amount          INTEGER NOT NULL,
    balance_after   INTEGER NOT NULL CHECK (balance_after >= 0),
    -- top_up / grant: the lot it created; reversal / expiry: the lot it took from.
    lot_id          TEXT,
    -- refund: the debit it gives back; reversal: the top-up or grant it takes back.
    ref_entry       TEXT,
    -- consumption / spend (and their refunds): what the credits paid for.
    feature_key     TEXT,
    units           INTEGER,
    -- consumption: the FB-24 usage event it charges (one charge per event).
    usage_event_id  TEXT    UNIQUE,
    usage_seq       INTEGER,
    -- top_up / grant: when its credits lapse (NULL: never).
    expires_at      INTEGER,
    idempotency_key TEXT    NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 300),
    actor           TEXT    NOT NULL,
    reason          TEXT,
    -- FB-70: order, payment or invoice the entry belongs to.
    external_ref    TEXT,
    request_id      TEXT,
    metadata        TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(metadata)),
    -- The request as received, to tell a replay from a conflicting reuse of the key.
    request         TEXT    NOT NULL DEFAULT '{}' CHECK (json_valid(request)),
    UNIQUE (subject, idempotency_key),
    CHECK ((kind IN ('top_up', 'grant', 'refund') AND amount > 0)
        OR (kind IN ('consumption', 'spend', 'reversal', 'expiry') AND amount < 0))
) STRICT;
CREATE INDEX _forgebase_credit_ledger_subject ON _forgebase_credit_ledger (subject, seq);
CREATE INDEX _forgebase_credit_ledger_at ON _forgebase_credit_ledger (subject, at);
CREATE INDEX _forgebase_credit_ledger_kind_at ON _forgebase_credit_ledger (kind, at);
CREATE INDEX _forgebase_credit_ledger_ref ON _forgebase_credit_ledger (ref_entry) WHERE ref_entry IS NOT NULL;

CREATE TRIGGER _forgebase_credit_ledger_no_update BEFORE UPDATE ON _forgebase_credit_ledger
BEGIN
    SELECT RAISE(ABORT, 'credit ledger entries are immutable');
END;
CREATE TRIGGER _forgebase_credit_ledger_no_delete BEFORE DELETE ON _forgebase_credit_ledger
BEGIN
    SELECT RAISE(ABORT, 'credit ledger entries are immutable');
END;

-- What is left of each top-up or grant. Debits draw from the lots that
-- expire first (then grants before top-ups, then oldest first); refunds give
-- units back to the lots they came from; expiry empties a lot.
CREATE TABLE _forgebase_credit_lots (
    id         TEXT    NOT NULL PRIMARY KEY,
    subject    TEXT    NOT NULL,
    entry_id   TEXT    NOT NULL UNIQUE,
    kind       TEXT    NOT NULL CHECK (kind IN ('top_up', 'grant')),
    amount     INTEGER NOT NULL CHECK (amount > 0),
    remaining  INTEGER NOT NULL CHECK (remaining >= 0 AND remaining <= amount),
    expires_at INTEGER,
    created_at INTEGER NOT NULL
) STRICT;
CREATE INDEX _forgebase_credit_lots_open ON _forgebase_credit_lots (subject, expires_at) WHERE remaining > 0;
CREATE INDEX _forgebase_credit_lots_expiring ON _forgebase_credit_lots (expires_at)
    WHERE remaining > 0 AND expires_at IS NOT NULL;

-- Every change to a lot, signed (draws and expiries negative, the initial
-- amount and refunds positive). Immutable.
CREATE TABLE _forgebase_credit_allocations (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    entry_id TEXT    NOT NULL,
    lot_id   TEXT    NOT NULL,
    amount   INTEGER NOT NULL CHECK (amount <> 0)
) STRICT;
CREATE INDEX _forgebase_credit_allocations_entry ON _forgebase_credit_allocations (entry_id);
CREATE INDEX _forgebase_credit_allocations_lot ON _forgebase_credit_allocations (lot_id);

CREATE TRIGGER _forgebase_credit_allocations_no_update BEFORE UPDATE ON _forgebase_credit_allocations
BEGIN
    SELECT RAISE(ABORT, 'credit allocations are immutable');
END;
CREATE TRIGGER _forgebase_credit_allocations_no_delete BEFORE DELETE ON _forgebase_credit_allocations
BEGIN
    SELECT RAISE(ABORT, 'credit allocations are immutable');
END;

-- Prices of metered features in credits: `ceil(units * credits_per_unit /
-- per_units)` per usage event; `basis` `units` charges every unit, `overage`
-- only the units past a soft limit.
CREATE TABLE _forgebase_credit_rates (
    feature_key      TEXT    NOT NULL PRIMARY KEY,
    credits_per_unit INTEGER NOT NULL CHECK (credits_per_unit > 0),
    per_units        INTEGER NOT NULL DEFAULT 1 CHECK (per_units > 0),
    basis            TEXT    NOT NULL DEFAULT 'units' CHECK (basis IN ('units', 'overage')),
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL
) STRICT;

-- FB-24 usage events charged in credits, one row per event (the event id is
-- the idempotency key). `shortfall` is the part of the cost the balance could
-- not cover (the balance never goes negative); FB-70 may bill it.
CREATE TABLE _forgebase_credit_usage (
    event_id    TEXT    NOT NULL PRIMARY KEY,
    event_seq   INTEGER NOT NULL,
    event_at    INTEGER NOT NULL,
    at          INTEGER NOT NULL,
    subject     TEXT    NOT NULL,
    feature_key TEXT    NOT NULL,
    units       INTEGER NOT NULL,
    rate        INTEGER NOT NULL,
    per_units   INTEGER NOT NULL,
    cost        INTEGER NOT NULL CHECK (cost > 0),
    charged     INTEGER NOT NULL CHECK (charged >= 0),
    shortfall   INTEGER NOT NULL CHECK (shortfall >= 0),
    entry_id    TEXT,
    CHECK (charged + shortfall = cost)
) STRICT;
CREATE INDEX _forgebase_credit_usage_subject ON _forgebase_credit_usage (subject, event_seq);
CREATE INDEX _forgebase_credit_usage_shortfall ON _forgebase_credit_usage (subject, event_seq) WHERE shortfall > 0;

CREATE TRIGGER _forgebase_credit_usage_no_update BEFORE UPDATE ON _forgebase_credit_usage
BEGIN
    SELECT RAISE(ABORT, 'credit usage charges are immutable');
END;
CREATE TRIGGER _forgebase_credit_usage_no_delete BEFORE DELETE ON _forgebase_credit_usage
BEGIN
    SELECT RAISE(ABORT, 'credit usage charges are immutable');
END;

-- Read positions of feed consumers (`usage`: the FB-24 usage event `seq`).
CREATE TABLE _forgebase_credit_cursors (
    name       TEXT    NOT NULL PRIMARY KEY,
    seq        INTEGER NOT NULL CHECK (seq >= 0),
    updated_at INTEGER NOT NULL
) STRICT;
