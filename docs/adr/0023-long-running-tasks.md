# ADR-0023: Long-running background tasks

- Status: accepted
- Date: 2026-11-09
- Work package: FB-64 (MDF-56), addendum `docs/requirements/queues-pubsub-long-tasks.md` §B
- Scope: progress, checkpoints, timeouts, named queue settings, workflows, stored results, attempt history.
- Builds on: ADR-0007 (durable jobs), ADR-0011 (realtime), ADR-0018 (object storage).

## Decisions

1. **Extend, do not replace.** Everything lives in `forgebase-jobs` and migration
   `0022_jobs_long_tasks.sql`. New columns on `_forgebase_jobs` are nullable or defaulted; new tables
   `_forgebase_job_queues`, `_forgebase_job_workflows`, `_forgebase_job_deps`, `_forgebase_job_attempts`.
   No existing route, status code or payload field changed; responses only gained fields, and
   `JobStatus` kept its five values (a job waiting for parents is `queued` with `waiting_for > 0`,
   because the 0007 `CHECK` on `status` cannot be altered without rebuilding the table).
2. **Effects after commit.** Every state change runs in one transaction as before; side effects
   (live updates, waking the pool, in-process cancel signals, deleting stored results) are collected
   in `Fx` and applied only after the commit, so nobody hears about a change that rolled back.
3. **Progress** (`percent` 0-100, `stage`, `message`; fields left out keep their value) is stored on the
   job and published to the FB-40 channel `jobs:<job id>` (and `jobs:<workflow id>` for workflow
   jobs) as a `JobUpdate` (`event: progress | status`). Channel messages are ephemeral; the stored
   progress is the state a late subscriber reads first. Reporting progress or saving a checkpoint
   renews the lease like a heartbeat. In-process handlers call `JobContext::progress`; external
   workers `POST /jobs/{id}/progress`.
4. **Who may watch.** `JobChannelAuthorizer` wraps the process authorizer (server: around the policy
   engine): subscribing to `jobs:*` needs the channel rule **and** `ReadJobs`; publishing there is
   refused to everyone but the system, so progress cannot be forged. Other channels are untouched.
5. **Checkpoints**: JSON up to 256 KiB, saved with the lease (`save_checkpoint`). Kept across crash,
   lease expiry, timeout, release, restart and manual retry; handed to the next attempt in
   `ClaimedJob.checkpoint` / `JobContext::checkpoint()`. The attempt row records `from_checkpoint`.
6. **Timeouts**: `timeout_ms` per job (1 s to 30 days; default from the queue). The claim sets
   `deadline_at`; the reaper (claims and the pool tick) treats an attempt past it like a retryable
   failure (backoff, dead-letter when attempts are used up), however alive the worker is. The lease
   (visibility timeout, max 1 day) is separate and renewed automatically by the pool every third of
   it (`PoolConfig::heartbeat_interval` overrides). A timed-out or lost job fires the handler's cancel
   token.
7. **Cooperative cancellation.** `JobContext::cancel_token()` (`CancelToken`) fires on cancel, timeout
   or lost lease. A cancel in the same process signals the handler at once (registry of running
   handlers in `JobService`); across processes it arrives with the next heartbeat (`cancel_requested`).
   Cancelling a job cancels its unfinished child jobs.
8. **Named queues.** Concurrency (running jobs over all workers), rate limit per second or minute
   (fixed window stored on the queue row), pause/resume, default priority and timeout. Enforced inside
   the claim transaction on the single writer, so the in-process pool and every external worker share
   one budget. A claim works in rounds: a queue that runs out of budget is excluded and the claim goes
   on, so a saturated queue does not starve others. The cheap pre-claim read excludes blocked queues
   too, so idle polls of a paused queue never take the writer.
9. **Dependencies and workflows.** `NewJob.depends_on` (job ids) or a workflow (`POST
   /jobs/workflows`: named steps with `depends_on` by step name; chain, fan-out, fan-in) stores edges in
   `_forgebase_job_deps` and `waiting_for` = parents not succeeded. A parent's success counts its
   dependants down; a parent ending `dead`/`cancelled` cancels its waiting dependants transitively.
   Steps are inserted in topological order; cycles and unknown names are `400 invalid_workflow`.
   Parents' results reach the child in `ClaimedJob.parents` (inline results) and
   `JobContext::parent_results()` (also stored ones). Child jobs (`spawn_child`, `POST
   /jobs/{id}/children`, lease required) join the parent's workflow.
10. **Failure policy.** `fail`: the first failed step fails the workflow and cancels the rest.
    `compensate`: the workflow becomes `compensating`, the rest is cancelled, and the `compensate` job
    of every succeeded step runs, newest first, as a chain (each compensation gets the compensated
    step as a parent); all succeed → `compensated`, one fails → `failed`.
11. **Results.** Up to 256 KiB (the old limit) stay inline, so existing clients see no change. Larger
    results (to 16 MiB) go to a `ResultStore`; the server backs it with FB-30 storage (private bucket
    `forgebase-job-results`, `forgebase_server::StorageResults`), because `forgebase-storage` depends on
    `forgebase-jobs`. The upload happens before the transaction under a per-lease key after a lease
    check, and is deleted if the completion fails; deleting or purging the job deletes the object.
    `GET /jobs/{id}/result` reads either. Without a store, larger results stay `payload_too_large`.
12. **History.** `_forgebase_job_attempts` keeps one row per attempt (worker, start, end, duration,
    outcome `succeeded|failed|retry|timed_out|lease_expired|released|cancelled`, error,
    `from_checkpoint`) next to the 0007 event log. `GET /jobs/{id}/attempts`.
13. **API.** 16 new operations under `/api/v1/jobs` with operationIds prefixed `jobs_`; `GET /jobs`
    gained `workflow_id` and `parent_id` filters. `POST /jobs/{id}/complete` accepts bodies up to the
    result limit; all other routes keep the 1 MiB limit.

## Known limits / follow-ups

- Rate limits use fixed windows (a burst of 2× the limit across a window edge is possible).
- Compensations start without waiting for still-running sibling steps to acknowledge their cancel.
- Retrying a dead step does not revive a failed workflow (retry the workflow's jobs by hand).
- `jobs:` channel messages are not replayable (realtime ephemeral messages); read the job first.
- Studio, SDK and CLI for all of this are FB-65.
- Dependencies: none new (`forgebase-realtime` was already a transitive dependency through `forgebase-api`).
