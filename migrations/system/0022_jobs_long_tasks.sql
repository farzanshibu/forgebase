-- 0022_jobs_long_tasks: long-running background tasks (FB-64, ADR-0023).
--
-- Extends the durable job queue of 0007 without changing what is there:
-- progress, checkpoints, per-job timeouts, named queue settings, workflows
-- (dependencies, child jobs, compensation), results kept in object storage
-- and a per-attempt history. Every new column is nullable or has a default,
-- so rows written before this migration stay valid. Times are Unix epoch
-- milliseconds.

-- Progress reported by the handler or external worker.
ALTER TABLE _forgebase_jobs ADD COLUMN progress_percent REAL
    CHECK (progress_percent IS NULL OR (progress_percent >= 0 AND progress_percent <= 100));
ALTER TABLE _forgebase_jobs ADD COLUMN progress_stage TEXT CHECK (progress_stage IS NULL OR length(progress_stage) <= 128);
ALTER TABLE _forgebase_jobs ADD COLUMN progress_message TEXT CHECK (progress_message IS NULL OR length(progress_message) <= 2000);
ALTER TABLE _forgebase_jobs ADD COLUMN progress_at INTEGER;
-- Last checkpoint; the next attempt starts from it.
ALTER TABLE _forgebase_jobs ADD COLUMN checkpoint TEXT CHECK (checkpoint IS NULL OR json_valid(checkpoint));
ALTER TABLE _forgebase_jobs ADD COLUMN checkpoint_at INTEGER;
-- Longest run of one attempt; `deadline_at` is set at claim (claim + timeout).
ALTER TABLE _forgebase_jobs ADD COLUMN timeout_ms INTEGER CHECK (timeout_ms IS NULL OR timeout_ms >= 1);
ALTER TABLE _forgebase_jobs ADD COLUMN deadline_at INTEGER;
-- Workflows: the workflow, the step name, the job that spawned this one
-- (child jobs), the step this job compensates, and how many parents
-- (dependencies) have not succeeded yet. A job with `waiting_for > 0` is
-- `queued` but cannot be claimed.
ALTER TABLE _forgebase_jobs ADD COLUMN workflow_id TEXT;
ALTER TABLE _forgebase_jobs ADD COLUMN step TEXT CHECK (step IS NULL OR length(step) BETWEEN 1 AND 128);
ALTER TABLE _forgebase_jobs ADD COLUMN parent_id TEXT;
ALTER TABLE _forgebase_jobs ADD COLUMN compensates TEXT;
ALTER TABLE _forgebase_jobs ADD COLUMN compensate TEXT CHECK (compensate IS NULL OR json_valid(compensate));
ALTER TABLE _forgebase_jobs ADD COLUMN waiting_for INTEGER NOT NULL DEFAULT 0 CHECK (waiting_for >= 0);
-- A large result lives in object storage under this key; `result` is NULL then.
ALTER TABLE _forgebase_jobs ADD COLUMN result_ref TEXT;
ALTER TABLE _forgebase_jobs ADD COLUMN result_size INTEGER;

-- Running jobs past their deadline.
CREATE INDEX _forgebase_jobs_deadline ON _forgebase_jobs (deadline_at) WHERE status = 'running' AND deadline_at IS NOT NULL;
-- Running count per queue (concurrency limits) and listings by queue.
CREATE INDEX _forgebase_jobs_by_queue ON _forgebase_jobs (queue, status);
CREATE INDEX _forgebase_jobs_by_workflow ON _forgebase_jobs (workflow_id) WHERE workflow_id IS NOT NULL;
CREATE INDEX _forgebase_jobs_by_parent ON _forgebase_jobs (parent_id) WHERE parent_id IS NOT NULL;

-- Named queue settings. A queue without a row has no limits.
CREATE TABLE _forgebase_job_queues (
    name               TEXT    NOT NULL PRIMARY KEY CHECK (length(name) BETWEEN 1 AND 64),
    -- Most jobs of the queue running at once, over every worker.
    concurrency        INTEGER CHECK (concurrency IS NULL OR concurrency >= 1),
    -- Most claims per window of `rate_period_ms` (1 s or 1 min).
    rate_limit         INTEGER CHECK (rate_limit IS NULL OR rate_limit >= 1),
    rate_period_ms     INTEGER NOT NULL DEFAULT 1000 CHECK (rate_period_ms IN (1000, 60000)),
    paused             INTEGER NOT NULL DEFAULT 0 CHECK (paused IN (0, 1)),
    -- Applied at enqueue when the job sets none.
    default_priority   INTEGER CHECK (default_priority IS NULL OR default_priority BETWEEN -1000 AND 1000),
    default_timeout_ms INTEGER CHECK (default_timeout_ms IS NULL OR default_timeout_ms >= 1),
    -- Fixed rate window: its start and the claims made in it.
    window_start       INTEGER NOT NULL DEFAULT 0,
    window_count       INTEGER NOT NULL DEFAULT 0,
    created_at         INTEGER NOT NULL,
    updated_at         INTEGER NOT NULL
) STRICT;

CREATE TABLE _forgebase_job_workflows (
    id            TEXT    NOT NULL PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 64),
    name          TEXT    NOT NULL CHECK (length(name) BETWEEN 1 AND 128),
    status        TEXT    NOT NULL CHECK (status IN ('running', 'succeeded', 'failed', 'compensating', 'compensated', 'cancelled')),
    on_failure    TEXT    NOT NULL DEFAULT 'fail' CHECK (on_failure IN ('fail', 'compensate')),
    failed_job_id TEXT,
    error         TEXT,
    created_by    TEXT    NOT NULL,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    finished_at   INTEGER
) STRICT;

CREATE INDEX _forgebase_job_workflows_by_status ON _forgebase_job_workflows (status, created_at);

-- Dependencies: `job_id` runs after `parent_id` succeeded. No foreign key on
-- the parent, which may be purged long after its dependants ran.
CREATE TABLE _forgebase_job_deps (
    job_id    TEXT NOT NULL REFERENCES _forgebase_jobs (id) ON DELETE CASCADE,
    parent_id TEXT NOT NULL,
    PRIMARY KEY (job_id, parent_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX _forgebase_job_deps_by_parent ON _forgebase_job_deps (parent_id);

-- One row per attempt: who ran it, when, how long and how it ended.
CREATE TABLE _forgebase_job_attempts (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id          TEXT    NOT NULL REFERENCES _forgebase_jobs (id) ON DELETE CASCADE,
    attempt         INTEGER NOT NULL,
    worker_id       TEXT,
    started_at      INTEGER NOT NULL,
    finished_at     INTEGER,
    outcome         TEXT    CHECK (outcome IS NULL OR outcome IN (
                        'succeeded', 'failed', 'retry', 'timed_out', 'lease_expired', 'released', 'cancelled')),
    error           TEXT,
    from_checkpoint INTEGER NOT NULL DEFAULT 0 CHECK (from_checkpoint IN (0, 1))
) STRICT;

CREATE INDEX _forgebase_job_attempts_by_job ON _forgebase_job_attempts (job_id, id);
