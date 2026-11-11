-- PostgreSQL twin of migrations/system/0038_schedule_fire.sql (FB-165,
-- ADR-0029 §1.7).
--
-- forgebase.fire_schedule(id [, due_ms]) fires one schedule the way the Rust
-- tick (`fire_tx` and `insert_job` in forgebase-jobs) does: it locks the
-- schedule row, takes the due time from the clock (the start of the minute,
-- or of the N-second slot for `*/N * * * * *`), inserts the job with the
-- idempotency key `schedule:<id>:<due ms>` (an existing job with that key is
-- kept), applies the queue's default timeout (`<tenant>/<queue>` settings),
-- writes the `enqueued` event, records last_run_at/last_job_id and runs
-- NOTIFY forgebase_jobs so the server wakes its workers.
--
-- pg_cron calls it as `SELECT forgebase.fire_schedule('<id>')`; the Rust
-- scheduler calls it with `due_ms` for the start-up catch-up. It returns the
-- id of the job (new or existing), or NULL when the schedule is gone or
-- disabled. The function is created by the migrating role, which is also
-- the role pg_cron's jobs run as.
--
-- `next_run_at` is left alone: for a pg_cron-owned schedule the server
-- computes it from the expression when it reads the schedule.

CREATE SCHEMA IF NOT EXISTS forgebase;

CREATE OR REPLACE FUNCTION forgebase.fire_schedule(p_id text, p_due bigint DEFAULT NULL)
RETURNS text
LANGUAGE plpgsql
SET search_path = forgebase, public
AS $fn$
DECLARE
    s        _forgebase_job_schedules%ROWTYPE;
    now_ms   bigint := (extract(epoch FROM clock_timestamp()) * 1000)::bigint;
    grid_ms  bigint := 60000;
    step     text[];
    due_ms   bigint;
    ikey     text;
    job_id   text;
    qkey     text;
    timeout  bigint;
BEGIN
    SELECT * INTO s FROM _forgebase_job_schedules WHERE id = p_id FOR UPDATE;
    IF NOT FOUND OR s.enabled = 0 THEN
        RETURN NULL;
    END IF;

    step := regexp_match(regexp_replace(btrim(s.cron), '[[:space:]]+', ' ', 'g'),
                         '^\*/([0-9]+) \* \* \* \* \*$');
    IF step IS NOT NULL AND step[1]::bigint BETWEEN 1 AND 59 THEN
        grid_ms := step[1]::bigint * 1000;
    END IF;
    due_ms := COALESCE(p_due, (now_ms / grid_ms) * grid_ms);
    ikey := 'schedule:' || s.id || ':' || due_ms;

    SELECT j.id INTO job_id FROM _forgebase_jobs j
     WHERE j.queue = s.queue AND j.idempotency_key = ikey AND COALESCE(j.tenant, '') = COALESCE(s.tenant, '');

    IF job_id IS NULL THEN
        qkey := CASE WHEN s.tenant IS NULL THEN s.queue ELSE s.tenant || '/' || s.queue END;
        SELECT q.default_timeout_ms INTO timeout FROM _forgebase_job_queues q WHERE q.name = qkey;
        job_id := 'job-' || replace(gen_random_uuid()::text, '-', '');
        INSERT INTO _forgebase_jobs (
            id, queue, kind, payload, priority, status, run_at, max_attempts, backoff_base_ms, backoff_max_ms,
            visibility_timeout_ms, idempotency_key, schedule_id, created_by, created_at, updated_at, timeout_ms,
            waiting_for, tenant, context)
        VALUES (
            job_id, s.queue, s.kind, s.payload, s.priority, 'queued', now_ms, s.max_attempts, s.backoff_base_ms,
            s.backoff_max_ms, s.visibility_timeout_ms, ikey, s.id, 'schedule:' || s.name, now_ms, now_ms, timeout,
            0, s.tenant, s.context)
        ON CONFLICT DO NOTHING;
        IF NOT FOUND THEN
            -- Lost a race on the idempotency key: the other firing is the job.
            SELECT j.id INTO job_id FROM _forgebase_jobs j
             WHERE j.queue = s.queue AND j.idempotency_key = ikey AND COALESCE(j.tenant, '') = COALESCE(s.tenant, '');
        ELSE
            INSERT INTO _forgebase_job_events (job_id, at, event, attempt) VALUES (job_id, now_ms, 'enqueued', 0);
            PERFORM pg_notify('forgebase_jobs', job_id);
        END IF;
    END IF;

    UPDATE _forgebase_job_schedules
       SET last_run_at = now_ms, last_job_id = job_id, updated_at = now_ms
     WHERE id = s.id;
    RETURN job_id;
END;
$fn$;
