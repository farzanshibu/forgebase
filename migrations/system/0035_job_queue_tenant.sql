-- FB-146 (FB-141/FB-142 follow-up): named-queue settings and workers belong
-- to a tenant, like jobs (0030_job_tenant).
--
-- `tenant`: the tenant (project environment) of the caller that created the
-- row; NULL = none (the system, callers without a tenant, rows created before
-- this migration). `created_by`: the principal that created it (NULL before
-- this migration). Non-privileged callers see and change only rows of their
-- own tenant, and only those they created unless they hold
-- `engine:read_jobs:all`.
--
-- Queue settings of a tenant are stored under the name `<tenant>/<queue>`
-- (queue names never hold '/'), so two tenants can configure the same queue
-- name; the settings govern only the jobs of that tenant.
ALTER TABLE _forgebase_job_queues ADD COLUMN tenant TEXT;
ALTER TABLE _forgebase_job_queues ADD COLUMN created_by TEXT;
ALTER TABLE _forgebase_job_workers ADD COLUMN tenant TEXT;
ALTER TABLE _forgebase_job_workers ADD COLUMN created_by TEXT;

CREATE INDEX _forgebase_job_queues_by_tenant ON _forgebase_job_queues (tenant, name);
CREATE INDEX _forgebase_job_workers_by_tenant ON _forgebase_job_workers (tenant, last_seen_at);
