/**
 * In-memory fakes of the long-task routes of `/api/v1/jobs` (ADR-0023): queue settings, workflows, attempts, checkpoints, results.
 * Plain handlers: the OpenAPI fixture does not describe them yet. Register before the `/jobs/{id}` routes (the matcher takes the first hit).
 */
import type { Attempt, Job, JobQueueInfo, WorkflowDetail } from "../api";
import { envelope, type MockApi } from "./mockApi";

export interface TaskState {
  queues: JobQueueInfo[];
  workflows: WorkflowDetail[];
  attempts: Record<string, Attempt[]>;
  checkpoints: Record<string, unknown>;
  results: Record<string, unknown>;
}

const NOW = "2026-11-09T10:00:00Z";

export const jobQueue = (name: string, extra: Partial<JobQueueInfo> = {}): JobQueueInfo => ({
  name, configured: true, rate_period: "second", paused: false, queued: 0, waiting: 0, running: 0, succeeded: 0, dead: 0, cancelled: 0, ...extra,
});

export function installTasks(mock: MockApi, jobs: () => Job[]): TaskState {
  const st: TaskState = { queues: [jobQueue("mail", { concurrency: 2, rate_limit: 10, tenant: "acme", created_by: "admin" }), jobQueue("default")], workflows: [], attempts: {}, checkpoints: {}, results: {} };
  const J = "/api/v1/jobs";
  const find = (id: string) => jobs().find((j) => j.id === id);
  const nf = () => envelope(404, "job_not_found", "no such job");
  const wfNf = () => envelope(404, "workflow_not_found", "no such workflow");

  mock.on("GET", `${J}/queues`, () => ({ status: 200, body: { items: st.queues } }));
  for (const [verb, paused] of [["pause", true], ["resume", false]] as const) {
    mock.on("POST", `${J}/queues/{name}/${verb}`, ({ params }) => {
      const q = st.queues.find((x) => x.name === params.name);
      if (!q) return envelope(404, "queue_not_found", "no such queue");
      q.paused = paused;
      return { status: 200, body: q };
    });
  }
  mock.on("GET", `${J}/workflows`, ({ query }) => {
    const s = query.get("status");
    const items = st.workflows.filter((w) => !s || w.status === s).map((w) => Object.fromEntries(Object.entries(w).filter(([k]) => k !== "steps")));
    return { status: 200, body: { items, total: items.length } };
  });
  mock.on("GET", `${J}/workflows/{id}`, ({ params }) => st.workflows.find((w) => w.id === params.id) ? { status: 200, body: st.workflows.find((w) => w.id === params.id) } : wfNf());
  mock.on("POST", `${J}/workflows/{id}/cancel`, ({ params }) => {
    const w = st.workflows.find((x) => x.id === params.id);
    if (!w) return wfNf();
    if (w.status !== "running" && w.status !== "compensating") return envelope(409, "workflow_finished", "already finished");
    w.status = "cancelled";
    for (const s of w.steps) if (s.job.status === "queued") s.job.status = "cancelled";
    return { status: 200, body: w };
  });
  mock.on("GET", `${J}/{id}/attempts`, ({ params }) => (find(params.id!) ? { status: 200, body: { items: st.attempts[params.id!] ?? [] } } : nf()));
  mock.on("GET", `${J}/{id}/checkpoint`, ({ params }) => (find(params.id!) ? { status: 200, body: { job_id: params.id, checkpoint: st.checkpoints[params.id!] ?? null, saved_at: NOW } } : nf()));
  mock.on("GET", `${J}/{id}/result`, ({ params }) => {
    const j = find(params.id!);
    if (!j) return nf();
    if (j.status !== "succeeded") return envelope(409, "job_not_succeeded", "not succeeded");
    return { status: 200, body: { job_id: j.id, stored: !!j.result_stored, size: j.result_size ?? null, result: st.results[j.id] ?? j.result ?? null } };
  });
  return st;
}
