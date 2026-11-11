-- FB-142 (rls-coverage G3, G4, G5): job tenancy and recorded invocation
-- contexts.
--
-- `tenant`: the tenant (project environment) of the enqueuer, stamped when the
-- job, schedule or workflow is created; NULL = none (system jobs, callers
-- without a tenant). Non-privileged callers see only jobs of their own tenant,
-- and only those they created unless they hold `engine:read_jobs:all`.
--
-- `context`: the enqueuer's recorded invocation context as JSON (principal,
-- realm, organization, tenant, scopes, impersonator); NULL = the system.
-- Handlers that act for the enqueuer (`function.invoke`) run with it. Jobs
-- fired by a schedule take the schedule's tenant and context.
ALTER TABLE _forgebase_jobs ADD COLUMN tenant TEXT;
ALTER TABLE _forgebase_jobs ADD COLUMN context TEXT CHECK (context IS NULL OR json_valid(context));
ALTER TABLE _forgebase_job_schedules ADD COLUMN tenant TEXT;
ALTER TABLE _forgebase_job_schedules ADD COLUMN context TEXT CHECK (context IS NULL OR json_valid(context));
ALTER TABLE _forgebase_job_workflows ADD COLUMN tenant TEXT;

CREATE INDEX _forgebase_jobs_by_tenant ON _forgebase_jobs (tenant, created_at);

-- Idempotency keys are per tenant: the same key in two tenants names two jobs.
DROP INDEX _forgebase_jobs_idempotency;
CREATE UNIQUE INDEX _forgebase_jobs_idempotency ON _forgebase_jobs (queue, COALESCE(tenant, ''), idempotency_key)
    WHERE idempotency_key IS NOT NULL;
