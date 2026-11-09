import { describe, expect, it } from "vitest";
import { ForgeBase, JobFailedError, workflow, type JobUpdate } from "../src/index";
import { json, noSleep, scripted } from "./helpers";

const job = (o: Record<string, unknown> = {}) => ({
  id: "j1", queue: "default", kind: "k", payload: null, priority: 0, status: "queued", attempts: 0, max_attempts: 5, ...o,
});

type Handler = (e: { type: string; data: unknown }) => void;

/** A ForgeBase whose realtime client is replaced by a recorder, so tests push channel messages by hand. */
const client = (...replies: Parameters<typeof scripted>) => {
  const s = scripted(...replies);
  const fb = new ForgeBase("http://x", { apiKey: "k", fetch: s.fetch, retry: { ...noSleep, statuses: [] } });
  const handlers: { channel?: string; fn: Handler }[] = [];
  let unsubscribed = 0;
  (fb.realtime as unknown as { subscribe: unknown }).subscribe = async (o: { channel?: string }, fn: Handler) => {
    handlers.push({ channel: o.channel, fn });
    return { id: "s", cursor: 0, unsubscribe: async () => void unsubscribed++ };
  };
  return { fb, calls: s.calls, handlers, unsub: () => unsubscribed };
};
const msg = (data: unknown) => ({ type: "message", data });
const upd = (o: Partial<JobUpdate>): JobUpdate => ({
  event: "progress", job_id: "j1", queue: "default", kind: "k", status: "running", attempts: 1, has_checkpoint: false, cancel_requested: false, waiting_for: 0, updated_at: "2026-01-01T00:00:10Z", ...o,
});

describe("jobs", () => {
  it("enqueues with options mapped to the wire, retrying only with an idempotency key", async () => {
    const { fb, calls } = client(json(200, job()));
    await fb.jobs.enqueue("resize", { id: 1 }, { queue: "img", timeoutMs: 60_000, maxAttempts: 2, delayMs: 5, dependsOn: ["p"], idempotencyKey: "i1" });
    expect(JSON.parse(calls[0]!.body!)).toEqual({ kind: "resize", payload: { id: 1 }, queue: "img", timeout_ms: 60_000, max_attempts: 2, delay_ms: 5, depends_on: ["p"], idempotency_key: "i1" });
  });

  it("lists with a workflow filter and reads stats, attempts, checkpoint and result", async () => {
    const { fb, calls } = client(
      json(200, { items: [job()], total: 1 }),
      json(200, { items: [{ attempt: 1 }] }),
      json(200, { job_id: "j1", checkpoint: { page: 3 }, saved_at: "t" }),
      json(200, { job_id: "j1", stored: true, size: 10, result: { ok: true } }),
    );
    expect((await fb.jobs.list({ status: "running", workflowId: "w1", limit: 5 })).total).toBe(1);
    expect(calls[0]!.url).toContain("workflow_id=w1");
    expect(calls[0]!.url).toContain("status=running");
    expect(await fb.jobs.attempts("j1")).toEqual([{ attempt: 1 }]);
    expect((await fb.jobs.checkpoint("j1")).checkpoint).toEqual({ page: 3 });
    expect((await fb.jobs.result<{ ok: boolean }>("j1")).result?.ok).toBe(true);
  });

  it("cancels and retries", async () => {
    const { fb, calls } = client(json(200, job({ status: "cancelled" })), json(200, job()));
    expect((await fb.jobs.cancel("j1")).status).toBe("cancelled");
    await fb.jobs.retry("j1");
    expect(calls.map((c) => `${c.method} ${c.url}`)).toEqual(["POST http://x/api/v1/jobs/j1/cancel", "POST http://x/api/v1/jobs/j1/retry"]);
  });
});

describe("jobs.onProgress", () => {
  it("delivers the stored progress first, then live updates, and drops stale ones", async () => {
    const { fb, handlers, unsub } = client(json(200, job({ status: "running", updated_at: "2026-01-01T00:00:05Z", progress: { percent: 20, stage: "load", updated_at: "t" } })));
    const got: JobUpdate[] = [];
    const sub = await fb.jobs.onProgress("j1", (u) => got.push(u));
    expect(handlers[0]!.channel).toBe("jobs:j1");
    expect(got[0]).toMatchObject({ job_id: "j1", status: "running", progress: { percent: 20 } });
    handlers[0]!.fn(msg(upd({ progress: { percent: 60, updated_at: "t" } })));
    handlers[0]!.fn(msg(upd({ updated_at: "2026-01-01T00:00:01Z", progress: { percent: 1, updated_at: "t" } }))); // older than the snapshot
    handlers[0]!.fn({ type: "presence.join", data: null }); // not a channel message
    expect(got.map((u) => u.progress?.percent)).toEqual([20, 60]);
    await sub.unsubscribe();
    expect(unsub()).toBe(1);
  });

  it("filters to progress events and reports a final job through onDone", async () => {
    const { fb, handlers } = client(json(200, job({ status: "succeeded", updated_at: "2026-01-01T00:00:00Z" })));
    const got: JobUpdate[] = [];
    let done: string | undefined;
    await fb.jobs.onProgress("j1", (u) => got.push(u), { progressOnly: true, onDone: (j) => (done = j.status) });
    expect(done).toBe("succeeded");
    handlers[0]!.fn(msg(upd({ event: "status", status: "succeeded" })));
    expect(got).toHaveLength(0);
  });

  it("unsubscribes when the initial read fails", async () => {
    const { fb, unsub } = client(json(404, { error: { code: "not_found", message: "no job" } }));
    await expect(fb.jobs.onProgress("nope", () => undefined)).rejects.toMatchObject({ status: 404 });
    expect(unsub()).toBe(1);
  });
});

describe("jobs.wait", () => {
  it("polls until the job succeeds", async () => {
    const { fb, calls } = client(json(200, job({ status: "queued" })), json(200, job({ status: "running" })), json(200, job({ status: "succeeded", result: 7 })));
    const j = await fb.jobs.wait("j1", { pollMs: 1 });
    expect(j.result).toBe(7);
    expect(calls).toHaveLength(3);
  });

  it("rejects with JobFailedError when the job dies", async () => {
    const { fb } = client(json(200, job({ status: "dead", last_error: "boom" })));
    const e = await fb.jobs.wait("j1").catch((x: unknown) => x);
    expect(e).toBeInstanceOf(JobFailedError);
    expect((e as JobFailedError).job.status).toBe("dead");
    expect((e as Error).message).toContain("boom");
  });

  it("gives up after the timeout", async () => {
    const { fb } = client(json(200, job({ status: "running" })));
    await expect(fb.jobs.wait("j1", { pollMs: 1, timeoutMs: 5 })).rejects.toThrow(/still running/);
  });
});

describe("workflow helpers", () => {
  it("builds a pipeline, fan-out and fan-in with compensation", () => {
    const spec = workflow("etl")
      .onFailure("compensate")
      .step("fetch", "http.get", { u: 1 }, { timeoutMs: 1000, compensate: { kind: "undo" } })
      .step("a", "k")
      .step("b", "k", null, { after: ["fetch"] })
      .step("join", "k", null, { after: ["a", "b"], queue: "q" })
      .build();
    expect(spec.on_failure).toBe("compensate");
    expect(spec.steps.map((s) => [s.name, s.depends_on])).toEqual([["fetch", []], ["a", ["fetch"]], ["b", ["fetch"]], ["join", ["a", "b"]]]);
    expect(spec.steps[0]).toMatchObject({ timeout_ms: 1000, compensate: { kind: "undo" }, payload: { u: 1 } });
    expect(spec.steps[3]).toMatchObject({ queue: "q" });
  });

  it("rejects duplicates, unknown dependencies, cycles and empty workflows", () => {
    expect(() => workflow("w").step("a", "k").step("a", "k")).toThrow(/twice/);
    expect(() => workflow("w").step("a", "k", null, { after: ["zz"] }).build()).toThrow(/unknown step/);
    expect(() => workflow("w").step("a", "k", null, { after: ["b"] }).step("b", "k", null, { after: ["a"] }).build()).toThrow(/cycle/);
    expect(() => workflow("w").build()).toThrow(/at least one/);
  });

  it("creates, reads, lists and cancels a workflow", async () => {
    const wf = { id: "w1", name: "etl", status: "running", on_failure: "fail", steps: [] };
    const { fb, calls } = client(json(201, wf), json(200, wf), json(200, { items: [wf], total: 1 }), json(200, { ...wf, status: "cancelled" }));
    const made = await fb.jobs.createWorkflow(workflow("etl").step("a", "k"));
    expect(made.id).toBe("w1");
    expect(JSON.parse(calls[0]!.body!)).toEqual({ name: "etl", steps: [{ name: "a", kind: "k", payload: null, depends_on: [] }] });
    await fb.jobs.workflow("w1");
    expect((await fb.jobs.listWorkflows({ status: "running" })).total).toBe(1);
    expect((await fb.jobs.cancelWorkflow("w1")).status).toBe("cancelled");
    expect(calls[3]!.url).toBe("http://x/api/v1/jobs/workflows/w1/cancel");
  });

  it("waits for a workflow and fails when it ends failed", async () => {
    const base = { id: "w1", name: "x", on_failure: "fail", steps: [] };
    const { fb } = client(json(200, { ...base, status: "running" }), json(200, { ...base, status: "succeeded" }));
    expect((await fb.jobs.waitWorkflow("w1", { pollMs: 1 })).status).toBe("succeeded");
    const bad = client(json(200, { ...base, status: "failed", error: "step b" }));
    await expect(bad.fb.jobs.waitWorkflow("w1")).rejects.toThrow(/failed: step b/);
  });

  it("delivers workflow updates from the channel", async () => {
    const wf = { id: "w1", name: "x", status: "running", on_failure: "fail", steps: [] };
    const { fb, handlers } = client(json(200, wf));
    const got: string[] = [];
    await fb.jobs.onWorkflowProgress("w1", (w) => got.push(w.status));
    handlers[0]!.fn(msg({ event: "workflow", workflow: { ...wf, status: "succeeded" } }));
    handlers[0]!.fn(msg({ event: "progress" }));
    expect(got).toEqual(["running", "succeeded"]);
  });
});
