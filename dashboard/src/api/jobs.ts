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
  cancel_requested: boolean; result?: unknown; last_error?: string | null; created_by: string;
  created_at: string; updated_at: string; started_at?: string | null; finished_at?: string | null;
}
export interface JobPage { items: Job[]; total: number }
export interface JobEvent { id: number; at: string; event: string; attempt: number; worker_id?: string | null; message?: string | null }
export interface QueueStats { queue: string; queued: number; running: number; succeeded: number; dead: number; cancelled: number }
export interface JobStats { totals: Record<string, number>; queues: QueueStats[] }
export interface Schedule {
  id: string; name: string; cron: string; queue: string; kind: string; payload: unknown; priority: number; enabled: boolean;
  next_run_at: string; last_run_at?: string | null; last_job_id?: string | null; created_at: string; updated_at: string;
}
export interface JobFilter { status?: JobStatus; queue?: string; kind?: string; limit?: number; offset?: number }

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
export const listSchedules = async (signal?: AbortSignal) => (await request<{ items: Schedule[] }>(`${J}/schedules`, { signal })).items;
