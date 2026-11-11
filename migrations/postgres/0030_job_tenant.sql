-- PostgreSQL twin of migrations/system/0030_job_tenant.sql (FB-142,
-- rls-coverage G3, G4, G5): the enqueuer's tenant on jobs, schedules and
-- workflows (NULL = none), and the recorded invocation context of jobs and
-- schedules as jsonb (NULL = the system).

ALTER TABLE _forgebase_jobs ADD COLUMN tenant text;
ALTER TABLE _forgebase_jobs ADD COLUMN context jsonb;
ALTER TABLE _forgebase_job_schedules ADD COLUMN tenant text;
ALTER TABLE _forgebase_job_schedules ADD COLUMN context jsonb;
ALTER TABLE _forgebase_job_workflows ADD COLUMN tenant text;

CREATE INDEX _forgebase_jobs_by_tenant ON _forgebase_jobs (tenant, created_at);

-- Idempotency keys are per tenant: the same key in two tenants names two jobs.
DROP INDEX _forgebase_jobs_idempotency;
CREATE UNIQUE INDEX _forgebase_jobs_idempotency ON _forgebase_jobs (queue, COALESCE(tenant, ''), idempotency_key)
    WHERE idempotency_key IS NOT NULL;
