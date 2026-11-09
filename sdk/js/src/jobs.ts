import { ForgeBaseError } from "./errors";
import type { Transport } from "./http";
import type { RealtimeClient, Subscription } from "./realtime";

/*
 * Background jobs and long-running tasks (`/api/v1/jobs`, ADR-0023): `client.jobs`.
 * Wire types are written by hand (snake_case, as the server sends them) until the OpenAPI fixture carries the long-task schemas.
 */

const J = "/api/v1/jobs";
const enc = encodeURIComponent;

export type JobStatus = "queued" | "running" | "succeeded" | "dead" | "cancelled";
export type WorkflowStatus = "running" | "succeeded" | "failed" | "compensating" | "compensated" | "cancelled";

export interface Progress {
  percent?: number | null;
  stage?: string | null;
  message?: string | null;
  updated_at: string;
}

export interface Job {
  id: string;
  queue: string;
  kind: string;
  payload: unknown;
  priority: number;
  status: JobStatus;
  attempts: number;
  max_attempts: number;
  result?: unknown;
  last_error?: string | null;
  progress?: Progress | null;
  has_checkpoint?: boolean;
  cancel_requested?: boolean;
  timeout_ms?: number | null;
  workflow_id?: string | null;
  step?: string | null;
  parent_id?: string | null;
  waiting_for?: number;
  created_at?: string;
  updated_at?: string;
  finished_at?: string | null;
  [k: string]: unknown;
}

/** A live update delivered on `jobs:<id>`. */
export interface JobUpdate {
  /** `progress` (a report or checkpoint) or `status` (a state change). */
  event: "progress" | "status" | (string & {});
  job_id: string;
  queue: string;
  kind: string;
  status: JobStatus;
  attempts: number;
  progress?: Progress | null;
  has_checkpoint: boolean;
  cancel_requested: boolean;
  waiting_for: number;
  workflow_id?: string | null;
  step?: string | null;
  last_error?: string | null;
  updated_at: string;
  finished_at?: string | null;
}

export interface Workflow {
  id: string;
  name: string;
  status: WorkflowStatus;
  on_failure: "fail" | "compensate";
  failed_job_id?: string | null;
  error?: string | null;
  created_at?: string;
  updated_at?: string;
  finished_at?: string | null;
  [k: string]: unknown;
}

export interface WorkflowStep {
  name?: string | null;
  role: string;
  job: Job;
  depends_on: string[];
}

export interface WorkflowDetail extends Workflow {
  steps: WorkflowStep[];
}

export interface Checkpoint {
  job_id: string;
  checkpoint?: unknown;
  saved_at?: string | null;
}

export interface JobResult<T = unknown> {
  job_id: string;
  stored: boolean;
  size?: number | null;
  result?: T;
}

export interface JobQueueInfo {
  name: string;
  paused?: boolean;
  [k: string]: unknown;
}

export interface EnqueueOptions {
  queue?: string;
  priority?: number;
  /** RFC 3339 start time. */
  runAt?: string;
  delayMs?: number;
  maxAttempts?: number;
  backoffBaseMs?: number;
  backoffMaxMs?: number;
  visibilityTimeoutMs?: number;
  /** A repeat with the same key returns the existing job (and is retried safely). */
  idempotencyKey?: string;
  /** Longest run of one attempt, in ms (1 s to 30 days). */
  timeoutMs?: number;
  /** Job ids that must succeed first. */
  dependsOn?: string[];
}

export interface ListJobsOptions {
  status?: JobStatus;
  queue?: string;
  kind?: string;
  limit?: number;
  offset?: number;
  workflowId?: string;
}

export interface StepOptions {
  queue?: string;
  priority?: number;
  maxAttempts?: number;
  backoffBaseMs?: number;
  backoffMaxMs?: number;
  visibilityTimeoutMs?: number;
  timeoutMs?: number;
  /** Runs when the workflow fails after this step succeeded (`onFailure: "compensate"`). */
  compensate?: { kind: string; payload?: unknown; queue?: string };
}

export interface StepSpec {
  name: string;
  kind: string;
  payload?: unknown;
  depends_on: string[];
  queue?: string;
  priority?: number;
  max_attempts?: number;
  backoff_base_ms?: number;
  backoff_max_ms?: number;
  visibility_timeout_ms?: number;
  timeout_ms?: number;
  compensate?: { kind: string; payload?: unknown; queue?: string };
}

export interface WorkflowSpec {
  name: string;
  on_failure?: "fail" | "compensate";
  steps: StepSpec[];
}

export interface WaitOptions {
  /** Poll period (default 1000 ms). */
  pollMs?: number;
  /** Give up after this long (default 5 min). */
  timeoutMs?: number;
  signal?: AbortSignal;
}

export interface ProgressOptions {
  /** Called when the stream ends because the job reached a final state. */
  onDone?: (job: Job) => void;
  /** Ignore `status` updates and deliver only progress reports. */
  progressOnly?: boolean;
}

const FINAL: ReadonlySet<string> = new Set(["succeeded", "dead", "cancelled"]);
const WF_FINAL: ReadonlySet<string> = new Set(["succeeded", "failed", "compensated", "cancelled"]);

/** True once a job can no longer change by itself (`dead` can still be retried by hand). */
export const isFinalJob = (s: JobStatus): boolean => FINAL.has(s);
/** True once a workflow has settled. */
export const isFinalWorkflow = (s: WorkflowStatus): boolean => WF_FINAL.has(s);

/** A job did not finish in time or ended unsuccessfully (`wait`). */
export class JobFailedError extends ForgeBaseError {
  constructor(
    readonly job: Job,
    message?: string,
  ) {
    super({ status: 0, code: "job_failed", message: message ?? `job ${job.id} ${job.status}${job.last_error ? `: ${job.last_error}` : ""}` });
    this.name = "JobFailedError";
  }
}

const jobErr = (message: string) => new ForgeBaseError({ status: 0, code: "invalid_workflow", message });

const compact = <T extends Record<string, unknown>>(o: T): Partial<T> =>
  Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as Partial<T>;

/**
 * Builds a workflow (a DAG of jobs): `workflow("etl").step("fetch", "http.get", {...}).step("load", "db.load", {...}, { after: ["fetch"] })`.
 * A step without `after` depends on the step before it (a pipeline); pass `after: []` for a root or several names for fan-in.
 */
export class WorkflowBuilder {
  private readonly steps: StepSpec[] = [];
  private policy?: "fail" | "compensate";

  constructor(private readonly name: string) {}

  /** `compensate` runs the steps' `compensate` jobs (newest first) when a step fails; the default `fail` just stops. */
  onFailure(policy: "fail" | "compensate"): this {
    this.policy = policy;
    return this;
  }

  step(name: string, kind: string, payload?: unknown, opts: StepOptions & { after?: string[] } = {}): this {
    const { after, compensate, queue, priority, maxAttempts, backoffBaseMs, backoffMaxMs, visibilityTimeoutMs, timeoutMs } = opts;
    if (this.steps.some((s) => s.name === name)) throw jobErr(`workflow step "${name}" is defined twice`);
    const prev = this.steps[this.steps.length - 1];
    this.steps.push({
      name,
      kind,
      payload: payload ?? null,
      depends_on: after ?? (prev ? [prev.name] : []),
      ...compact({
        queue,
        priority,
        max_attempts: maxAttempts,
        backoff_base_ms: backoffBaseMs,
        backoff_max_ms: backoffMaxMs,
        visibility_timeout_ms: visibilityTimeoutMs,
        timeout_ms: timeoutMs,
        compensate,
      }),
    });
    return this;
  }

  /** The request body, validated locally (known dependencies, no cycles). */
  build(): WorkflowSpec {
    if (this.steps.length === 0) throw jobErr("a workflow needs at least one step");
    const names = new Set(this.steps.map((s) => s.name));
    for (const s of this.steps) {
      for (const d of s.depends_on) {
        if (!names.has(d)) throw jobErr(`step "${s.name}" depends on unknown step "${d}"`);
        if (d === s.name) throw jobErr(`step "${s.name}" depends on itself`);
      }
    }
    // Kahn's algorithm: every step must be reachable in topological order.
    const left = new Map(this.steps.map((s) => [s.name, s.depends_on.length]));
    const queue = this.steps.filter((s) => s.depends_on.length === 0).map((s) => s.name);
    let seen = 0;
    while (queue.length) {
      const n = queue.pop()!;
      seen++;
      for (const s of this.steps) {
        if (s.depends_on.includes(n)) {
          const c = left.get(s.name)! - 1;
          left.set(s.name, c);
          if (c === 0) queue.push(s.name);
        }
      }
    }
    if (seen !== this.steps.length) throw jobErr("workflow steps contain a dependency cycle");
    return { name: this.name, ...(this.policy ? { on_failure: this.policy } : {}), steps: this.steps.map((s) => ({ ...s })) };
  }
}

/** Starts a workflow definition. */
export const workflow = (name: string): WorkflowBuilder => new WorkflowBuilder(name);

/** Jobs and long tasks: enqueue, watch progress live, checkpoints, results, queues and workflows: `client.jobs`. */
export class JobsApi {
  constructor(
    private readonly t: Transport,
    private readonly rt: RealtimeClient,
    private readonly sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
  ) {}

  // ------------------------------------------------------------------- jobs

  enqueue(kind: string, payload: unknown = null, o: EnqueueOptions = {}): Promise<Job> {
    const body = {
      kind,
      payload,
      ...compact({
        queue: o.queue,
        priority: o.priority,
        run_at: o.runAt,
        delay_ms: o.delayMs,
        max_attempts: o.maxAttempts,
        backoff_base_ms: o.backoffBaseMs,
        backoff_max_ms: o.backoffMaxMs,
        visibility_timeout_ms: o.visibilityTimeoutMs,
        idempotency_key: o.idempotencyKey,
        timeout_ms: o.timeoutMs,
        depends_on: o.dependsOn,
      }),
    };
    return this.t.request<Job>(J, { method: "POST", body, retry: o.idempotencyKey ? undefined : false });
  }

  get(id: string): Promise<Job> {
    return this.t.request(`${J}/${enc(id)}`);
  }

  async list(o: ListJobsOptions = {}): Promise<{ items: Job[]; total: number }> {
    const query = compact({ status: o.status, queue: o.queue, kind: o.kind, limit: o.limit, offset: o.offset, workflow_id: o.workflowId });
    return this.t.request(J, { query: query as Record<string, string | number> });
  }

  cancel(id: string): Promise<Job> {
    return this.t.request(`${J}/${enc(id)}/cancel`, { method: "POST" });
  }

  retry(id: string): Promise<Job> {
    return this.t.request(`${J}/${enc(id)}/retry`, { method: "POST" });
  }

  async delete(id: string): Promise<void> {
    await this.t.request(`${J}/${enc(id)}`, { method: "DELETE" });
  }

  stats(): Promise<{ totals: Record<string, number>; queues: Record<string, unknown>[] }> {
    return this.t.request(`${J}/stats`);
  }

  async attempts(id: string): Promise<Record<string, unknown>[]> {
    return (await this.t.request<{ items: Record<string, unknown>[] }>(`${J}/${enc(id)}/attempts`)).items;
  }

  checkpoint(id: string): Promise<Checkpoint> {
    return this.t.request(`${J}/${enc(id)}/checkpoint`);
  }

  result<T = unknown>(id: string): Promise<JobResult<T>> {
    return this.t.request(`${J}/${enc(id)}/result`);
  }

  async children(id: string): Promise<Job[]> {
    return (await this.t.request<{ items: Job[] }>(`${J}/${enc(id)}/children`)).items;
  }

  // ------------------------------------------------------- live progress

  /**
   * Watches one job: `handler` gets the stored state first (so a late subscriber sees the current progress), then every live update from
   * the `jobs:<id>` realtime channel. Resolves with a subscription; `unsubscribe()` stops it.
   */
  async onProgress(id: string, handler: (u: JobUpdate) => void, o: ProgressOptions = {}): Promise<Subscription> {
    const seenAt = { t: "" };
    const deliver = (u: JobUpdate) => {
      if (o.progressOnly && u.event !== "progress") return;
      if (u.updated_at && seenAt.t && u.updated_at < seenAt.t) return; // older than what the snapshot already showed
      seenAt.t = u.updated_at || seenAt.t;
      handler(u);
    };
    const sub = await this.rt.subscribe({ channel: `jobs:${id}` }, (e) => {
      if (e.type === "message") deliver(e.data as JobUpdate);
    });
    try {
      const job = await this.get(id);
      deliver(toUpdate(job));
      if (isFinalJob(job.status)) o.onDone?.(job);
    } catch (err) {
      await sub.unsubscribe();
      throw err;
    }
    return sub;
  }

  /**
   * Resolves when the job succeeds (with the job); rejects with `JobFailedError` when it ends `dead` or `cancelled`, or when `timeoutMs` passes.
   * Uses plain polling, so it works without a realtime connection.
   */
  async wait(id: string, o: WaitOptions = {}): Promise<Job> {
    const pollMs = o.pollMs ?? 1000;
    const deadline = Date.now() + (o.timeoutMs ?? 300_000);
    for (;;) {
      o.signal?.throwIfAborted();
      const job = await this.get(id);
      if (job.status === "succeeded") return job;
      if (job.status === "dead" || job.status === "cancelled") throw new JobFailedError(job);
      if (Date.now() >= deadline) throw new JobFailedError(job, `job ${id} still ${job.status} after ${o.timeoutMs ?? 300_000} ms`);
      await this.sleep(pollMs);
    }
  }

  // ------------------------------------------------------------------ queues

  async queues(): Promise<JobQueueInfo[]> {
    return (await this.t.request<{ items: JobQueueInfo[] }>(`${J}/queues`)).items;
  }

  pauseQueue(name: string): Promise<JobQueueInfo> {
    return this.t.request(`${J}/queues/${enc(name)}/pause`, { method: "POST" });
  }

  resumeQueue(name: string): Promise<JobQueueInfo> {
    return this.t.request(`${J}/queues/${enc(name)}/resume`, { method: "POST" });
  }

  // --------------------------------------------------------------- workflows

  /** Creates a workflow from a builder (`workflow("x").step(...)`) or a ready spec. */
  createWorkflow(wf: WorkflowBuilder | WorkflowSpec): Promise<WorkflowDetail> {
    const body = wf instanceof WorkflowBuilder ? wf.build() : wf;
    return this.t.request(`${J}/workflows`, { method: "POST", body, retry: false });
  }

  workflow(id: string): Promise<WorkflowDetail> {
    return this.t.request(`${J}/workflows/${enc(id)}`);
  }

  listWorkflows(o: { status?: WorkflowStatus; limit?: number; offset?: number } = {}): Promise<{ items: Workflow[]; total: number }> {
    return this.t.request(`${J}/workflows`, { query: compact(o) as Record<string, string | number> });
  }

  cancelWorkflow(id: string): Promise<WorkflowDetail> {
    return this.t.request(`${J}/workflows/${enc(id)}/cancel`, { method: "POST" });
  }

  /** Watches a workflow's live updates (`jobs:<workflow id>`); `handler` gets the current state first. */
  async onWorkflowProgress(id: string, handler: (wf: Workflow) => void): Promise<Subscription> {
    const sub = await this.rt.subscribe({ channel: `jobs:${id}` }, (e) => {
      if (e.type !== "message") return;
      const d = e.data as { event?: string; workflow?: Workflow };
      if (d.event === "workflow" && d.workflow) handler(d.workflow);
    });
    try {
      handler(await this.workflow(id));
    } catch (err) {
      await sub.unsubscribe();
      throw err;
    }
    return sub;
  }

  /** Resolves when the workflow settles; rejects when it ends `failed` or `cancelled` (or `compensated`). */
  async waitWorkflow(id: string, o: WaitOptions = {}): Promise<WorkflowDetail> {
    const pollMs = o.pollMs ?? 1000;
    const deadline = Date.now() + (o.timeoutMs ?? 300_000);
    for (;;) {
      o.signal?.throwIfAborted();
      const wf = await this.workflow(id);
      if (wf.status === "succeeded") return wf;
      if (isFinalWorkflow(wf.status)) throw jobErr(`workflow ${id} ${wf.status}${wf.error ? `: ${wf.error}` : ""}`);
      if (Date.now() >= deadline) throw jobErr(`workflow ${id} still ${wf.status} after ${o.timeoutMs ?? 300_000} ms`);
      await this.sleep(pollMs);
    }
  }
}

function toUpdate(j: Job): JobUpdate {
  return {
    event: "status",
    job_id: j.id,
    queue: j.queue,
    kind: j.kind,
    status: j.status,
    attempts: j.attempts,
    progress: j.progress ?? null,
    has_checkpoint: j.has_checkpoint ?? false,
    cancel_requested: j.cancel_requested ?? false,
    waiting_for: j.waiting_for ?? 0,
    workflow_id: j.workflow_id ?? null,
    step: j.step ?? null,
    last_error: j.last_error ?? null,
    updated_at: j.updated_at ?? "",
    finished_at: j.finished_at ?? null,
  };
}
