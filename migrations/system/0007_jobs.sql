-- 0007_jobs: durable background jobs (FB-61, ADR-0007).
--
-- A job is a row. It survives restarts because every state change is a
-- committed transaction: a job is `queued` until a worker claims it, `running`
-- while a lease is held, and ends `succeeded`, `dead` (dead-letter queue) or
-- `cancelled`. A crashed worker's lease expires (`visibility_until`) and the
-- job is claimed again. Times are Unix epoch milliseconds.

-- Recurring jobs. Each firing enqueues one job; `next_run_at` moves in the
-- same transaction, so a firing happens once even with several processes.
CREATE TABLE _forgebase_job_schedules (
    id                    TEXT    NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    name                  TEXT    NOT NULL UNIQUE CHECK (length(name) BETWEEN 1 AND 128),
    cron                  TEXT    NOT NULL CHECK (length(cron) BETWEEN 1 AND 256),
    queue                 TEXT    NOT NULL DEFAULT 'default' CHECK (length(queue) BETWEEN 1 AND 64),
    kind                  TEXT    NOT NULL CHECK (length(kind) BETWEEN 1 AND 128),
    payload               TEXT    NOT NULL DEFAULT 'null' CHECK (json_valid(payload)),
    priority              INTEGER NOT NULL DEFAULT 0,
    max_attempts          INTEGER NOT NULL DEFAULT 5 CHECK (max_attempts >= 1),
    backoff_base_ms       INTEGER NOT NULL DEFAULT 1000 CHECK (backoff_base_ms >= 0),
    backoff_max_ms        INTEGER NOT NULL DEFAULT 3600000 CHECK (backoff_max_ms >= 0),
    visibility_timeout_ms INTEGER NOT NULL DEFAULT 60000 CHECK (visibility_timeout_ms >= 1),
    enabled               INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    next_run_at           INTEGER NOT NULL,
    last_run_at           INTEGER,
    last_job_id           TEXT,
    created_by            TEXT    NOT NULL,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL
) STRICT;

CREATE INDEX _forgebase_job_schedules_due ON _forgebase_job_schedules (next_run_at) WHERE enabled = 1;

CREATE TABLE _forgebase_jobs (
    id                    TEXT    NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    queue                 TEXT    NOT NULL DEFAULT 'default' CHECK (length(queue) BETWEEN 1 AND 64),
    kind                  TEXT    NOT NULL CHECK (length(kind) BETWEEN 1 AND 128),
    payload               TEXT    NOT NULL DEFAULT 'null' CHECK (json_valid(payload)),
    -- Higher runs first.
    priority              INTEGER NOT NULL DEFAULT 0,
    status                TEXT    NOT NULL CHECK (status IN ('queued', 'running', 'succeeded', 'dead', 'cancelled')),
    -- Earliest start (delayed jobs, retry backoff).
    run_at                INTEGER NOT NULL,
    -- Claims so far; a lost lease counts as an attempt.
    attempts              INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts          INTEGER NOT NULL DEFAULT 5 CHECK (max_attempts >= 1),
    backoff_base_ms       INTEGER NOT NULL DEFAULT 1000 CHECK (backoff_base_ms >= 0),
    backoff_max_ms        INTEGER NOT NULL DEFAULT 3600000 CHECK (backoff_max_ms >= 0),
    visibility_timeout_ms INTEGER NOT NULL DEFAULT 60000 CHECK (visibility_timeout_ms >= 1),
    idempotency_key       TEXT    CHECK (idempotency_key IS NULL OR length(idempotency_key) BETWEEN 1 AND 256),
    schedule_id           TEXT    REFERENCES _forgebase_job_schedules (id) ON DELETE SET NULL,
    -- Lease: set exactly while running. `lease_id` is a secret fencing token;
    -- complete / fail / heartbeat must present it, so a worker that lost its
    -- lease cannot finish the job a second time.
    worker_id             TEXT,
    lease_id              TEXT,
    visibility_until      INTEGER,
    heartbeat_at          INTEGER,
    cancel_requested      INTEGER NOT NULL DEFAULT 0 CHECK (cancel_requested IN (0, 1)),
    result                TEXT    CHECK (result IS NULL OR json_valid(result)),
    last_error            TEXT,
    created_by            TEXT    NOT NULL,
    created_at            INTEGER NOT NULL,
    updated_at            INTEGER NOT NULL,
    started_at            INTEGER,
    finished_at           INTEGER,
    CHECK ((status = 'running') = (lease_id IS NOT NULL AND visibility_until IS NOT NULL))
) STRICT;

-- Claim order: priority, then due time.
CREATE INDEX _forgebase_jobs_claim ON _forgebase_jobs (priority DESC, run_at) WHERE status = 'queued';
-- Expired leases.
CREATE INDEX _forgebase_jobs_lease ON _forgebase_jobs (visibility_until) WHERE status = 'running';
-- Listings and retention.
CREATE INDEX _forgebase_jobs_by_status ON _forgebase_jobs (status, finished_at);
CREATE INDEX _forgebase_jobs_by_kind ON _forgebase_jobs (kind, status);
-- Idempotency: one job per (queue, key).
CREATE UNIQUE INDEX _forgebase_jobs_idempotency ON _forgebase_jobs (queue, idempotency_key) WHERE idempotency_key IS NOT NULL;

-- Job history: one row per state change.
CREATE TABLE _forgebase_job_events (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id    TEXT    NOT NULL REFERENCES _forgebase_jobs (id) ON DELETE CASCADE,
    at        INTEGER NOT NULL,
    event     TEXT    NOT NULL CHECK (event IN (
                  'enqueued', 'claimed', 'succeeded', 'retry_scheduled', 'dead', 'cancel_requested',
                  'cancelled', 'lease_expired', 'released', 'requeued')),
    attempt   INTEGER NOT NULL DEFAULT 0,
    worker_id TEXT,
    message   TEXT
) STRICT;

CREATE INDEX _forgebase_job_events_by_job ON _forgebase_job_events (job_id, id);

-- Workers that registered or claimed jobs.
CREATE TABLE _forgebase_job_workers (
    id            TEXT    NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 128),
    name          TEXT,
    queues        TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(queues)),
    kinds         TEXT    NOT NULL DEFAULT '[]' CHECK (json_valid(kinds)),
    concurrency   INTEGER NOT NULL DEFAULT 1 CHECK (concurrency >= 1),
    registered_at INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL
) STRICT;
