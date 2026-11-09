/** Typed client for `/api/v1/jobs` (queues, jobs, schedules). The worker protocol (claim/heartbeat/complete/fail) is not for the dashboard. */
import { request, send } from "./client";

const J = "/api/v1/jobs";
const enc = encodeURIComponent;

export type JobStatus = "queued" | "running" | "succeeded" | "dead" | "cancelled";
export const JOB_STATUSES: JobStatus[] = ["queued", "running", "succeeded", "dead", "cancelled"];

export interface Job {
  id: string; queue: string; kind: string; payload: unknown; priority: number; status: JobStatus; run_at: string;
  attempts: number; max_attempts: number; backoff_base_ms: number; backoff_max_ms: number; visibility_timeout_ms: number;
  idempotency_key?: string | null; schedule_id?: string | null; worker_id?: string | null; visibility_until?: string | null;
  cancel_requested: boolean; has_checkpoint?: boolean; waiting_for?: number; result_stored?: boolean; result?: unknown; last_error?: string | null; created_by: string;
  created_at: string; updated_at: string; started_at?: string | null; finished_at?: string | null;
  /** Long-task fields (FB-64); absent on servers without them. */
  progress?: Progress | null; has_checkpoint?: boolean; checkpoint_at?: string | null; timeout_ms?: number | null; deadline_at?: string | null;
  heartbeat_at?: string | null; workflow_id?: string | null; step?: string | null; parent_id?: string | null; compensates?: string | null;
  waiting_for?: number; result_stored?: boolean; result_size?: number | null;
}
export interface Progress { percent?: number | null; stage?: string | null; message?: string | null; updated_at: string }
export interface Checkpoint { job_id: string; checkpoint?: unknown; saved_at?: string | null }
export interface Attempt {
  attempt: number; worker_id?: string | null; started_at: string; finished_at?: string | null; duration_ms: number; outcome?: string | null; error?: string | null; from_checkpoint: boolean;
}
export interface JobResult { job_id: string; stored: boolean; size?: number | null; result?: unknown }
export interface JobQueueInfo {
  name: string; configured: boolean; concurrency?: number | null; rate_limit?: number | null; rate_period: "second" | "minute"; paused: boolean;
  default_priority?: number | null; default_timeout_ms?: number | null; queued: number; waiting: number; running: number; succeeded: number; dead: number; cancelled: number;
}
export type WorkflowStatus = "running" | "succeeded" | "failed" | "compensating" | "compensated" | "cancelled";
export interface Workflow {
  id: string; name: string; status: WorkflowStatus; on_failure: "fail" | "compensate"; failed_job_id?: string | null; error?: string | null;
  created_by: string; created_at: string; updated_at: string; finished_at?: string | null;
}
export interface WorkflowStep { name?: string | null; role: string; job: Job; depends_on: string[] }
export interface WorkflowDetail extends Workflow { steps: WorkflowStep[] }
export interface JobPage { items: Job[]; total: number }
export interface JobEvent { id: number; at: string; event: string; attempt: number; worker_id?: string | null; message?: string | null }
export interface QueueStats { queue: string; queued: number; running: number; succeeded: number; dead: number; cancelled: number }
export interface JobStats { totals: Record<string, number>; queues: QueueStats[] }
export interface Schedule {
  id: string; name: string; cron: string; queue: string; kind: string; payload: unknown; priority: number; enabled: boolean;
  next_run_at: string; last_run_at?: string | null; last_job_id?: string | null; created_at: string; updated_at: string;
}
export interface JobFilter { status?: JobStatus; queue?: string; kind?: string; limit?: number; offset?: number; workflow_id?: string }

export const JOB_OPERATIONS = [
  "get /api/v1/jobs", "get /api/v1/jobs/stats", "get /api/v1/jobs/{id}", "get /api/v1/jobs/{id}/events",
  "post /api/v1/jobs/{id}/cancel", "post /api/v1/jobs/{id}/retry", "delete /api/v1/jobs/{id}", "get /api/v1/jobs/schedules",
] as const;

export const jobStats = (signal?: AbortSignal) => request<JobStats>(`${J}/stats`, { signal });
export function listJobs(filter: JobFilter = {}, signal?: AbortSignal) {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(filter)) if (v !== undefined && v !== "") q.set(k, String(v));
  const s = q.toString();
  return request<JobPage>(s ? `${J}?${s}` : J, { signal });
}
export const getJob = (id: string, signal?: AbortSignal) => request<Job>(`${J}/${enc(id)}`, { signal });
export const jobEvents = async (id: string, signal?: AbortSignal) => (await request<{ items: JobEvent[] }>(`${J}/${enc(id)}/events`, { signal })).items;
export const cancelJob = (id: string) => send<Job>("POST", `${J}/${enc(id)}/cancel`);
export const retryJob = (id: string) => send<Job>("POST", `${J}/${enc(id)}/retry`);
export const deleteJob = (id: string) => send<void>("DELETE", `${J}/${enc(id)}`);
export const jobAttempts = async (id: string, signal?: AbortSignal) => (await request<{ items: Attempt[] }>(`${J}/${enc(id)}/attempts`, { signal })).items;
export const jobCheckpoint = (id: string, signal?: AbortSignal) => request<Checkpoint>(`${J}/${enc(id)}/checkpoint`, { signal });
export const jobResult = (id: string, signal?: AbortSignal) => request<JobResult>(`${J}/${enc(id)}/result`, { signal });
export const listJobQueues = async (signal?: AbortSignal) => (await request<{ items: JobQueueInfo[] }>(`${J}/queues`, { signal })).items;
export const pauseJobQueue = (name: string) => send<JobQueueInfo>("POST", `${J}/queues/${enc(name)}/pause`);
export const resumeJobQueue = (name: string) => send<JobQueueInfo>("POST", `${J}/queues/${enc(name)}/resume`);
export function listWorkflows(filter: { status?: WorkflowStatus; limit?: number; offset?: number } = {}, signal?: AbortSignal) {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(filter)) if (v !== undefined) q.set(k, String(v));
  const s = q.toString();
  return request<{ items: Workflow[]; total: number }>(s ? `${J}/workflows?${s}` : `${J}/workflows`, { signal });
}
export const getWorkflow = (id: string, signal?: AbortSignal) => request<WorkflowDetail>(`${J}/workflows/${enc(id)}`, { signal });
export const cancelWorkflow = (id: string) => send<WorkflowDetail>("POST", `${J}/workflows/${enc(id)}/cancel`);
export const listSchedules = async (signal?: AbortSignal) => (await request<{ items: Schedule[] }>(`${J}/schedules`, { signal })).items;
