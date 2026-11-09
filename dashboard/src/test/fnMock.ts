/**
 * In-memory fakes of the functions and jobs REST APIs for component tests.
 * Jobs routes are checked against the OpenAPI fixture; the functions routes are not in the fixture yet, so they are plain handlers.
 */
import type { ExecutionLog, FunctionInfo, Job, JobEvent, JobStatus, SecretInfo, VersionInfo } from "../api";
import { envelope, type MockApi, type MockReply, type MockRequest } from "./mockApi";
import { hasResponse, matchRoute, responseSchemaName, validateBody } from "./contract";

const NOW = "2026-11-09T10:00:00Z";
type Handler = (r: MockRequest) => MockReply;

function route(mock: MockApi, method: string, template: string, h: Handler, checked = false) {
  mock.on(method, template, (req) => {
    const out = h(req);
    if (!checked) return out;
    const spec = matchRoute(method, req.path);
    if (!spec) mock.problems.push(`not in openapi.json: ${method} ${req.path}`);
    else if (out.status < 400) {
      if (!hasResponse(spec.op, out.status)) mock.problems.push(`${method} ${template}: status ${out.status} not documented`);
      const name = responseSchemaName(spec.op, out.status);
      if (name && out.body !== undefined) for (const p of validateBody(name, out.body)) mock.problems.push(`${method} ${template} -> ${name}: ${p}`);
    }
    return out;
  });
}

const cfg = (caps: string[] = []) => ({ capabilities: caps, fetch_hosts: [], limits: { fuel: 1000, memory_bytes: 65536, timeout_ms: 5000, max_concurrency: 8 }, env: { MODE: "x" }, hooks: [] });

export interface FnState {
  functions: FunctionInfo[];
  versions: Record<string, VersionInfo[]>;
  secrets: Record<string, SecretInfo[]>;
  secretValues: Record<string, string>;
  logs: Record<string, ExecutionLog[]>;
}

export function installFunctions(mock: MockApi): FnState {
  const st: FnState = {
    functions: [{ id: "f1", name: "hello", description: "Says hi", access: "permission", active_version: 2, latest_version: 2, principal: "service:function.hello", created_at: NOW, updated_at: NOW, created_by: "admin" }],
    versions: {
      hello: [
        { version: 2, size: 120, config: cfg(["log"]), active: true, created_at: NOW, created_by: "admin" },
        { version: 1, size: 100, config: cfg([]), active: false, created_at: NOW, created_by: "admin" },
      ],
    },
    secrets: { hello: [{ name: "API_KEY", created_at: NOW, updated_at: NOW }] },
    secretValues: {},
    logs: {
      hello: [
        { id: 2, function: "hello", version: 2, trigger: "http", status: "trap", duration_ms: 9, fuel_used: 55, error: "unreachable executed", logs: [], request_id: "r2", created_at: NOW },
        { id: 1, function: "hello", version: 2, trigger: "http", status: "ok", duration_ms: 3, fuel_used: 20, error: null, logs: [{ level: "info", message: "hi there" }], request_id: "r1", created_at: NOW },
      ],
    },
  };
  const F = "/api/v1/functions";
  const nf = () => envelope(404, "not_found", "no such function");
  const find = (n: string) => st.functions.find((f) => f.name === n);
  route(mock, "GET", F, () => ({ status: 200, body: { items: st.functions } }));
  route(mock, "POST", F, ({ body }) => {
    const b = body as { name: string; wasm: string; description?: string; access?: FunctionInfo["access"]; capabilities?: string[]; activate?: boolean };
    if (!/^[a-z][a-z0-9_-]*$/.test(b.name)) return envelope(400, "invalid_function_name", "bad name");
    let fn = find(b.name);
    const vs = (st.versions[b.name] ??= []);
    const version = (vs[0]?.version ?? 0) + 1;
    const activate = b.activate !== false;
    if (!fn) {
      fn = { id: `f_${b.name}`, name: b.name, description: b.description ?? "", access: b.access ?? "permission", active_version: 0, latest_version: 0, principal: `service:function.${b.name}`, created_at: NOW, updated_at: NOW, created_by: "admin" };
      st.functions.push(fn);
    }
    fn.latest_version = version;
    if (activate) { fn.active_version = version; vs.forEach((v) => (v.active = false)); }
    const v: VersionInfo = { version, size: atob(b.wasm).length, config: cfg(b.capabilities), active: activate, created_at: NOW, created_by: "admin" };
    vs.unshift(v);
    return { status: 201, body: { function: fn, version: v } };
  });
  route(mock, "GET", `${F}/{name}`, ({ params }) => { const f = find(params.name!); return f ? { status: 200, body: f } : nf(); });
  route(mock, "DELETE", `${F}/{name}`, ({ params }) => {
    if (!find(params.name!)) return nf();
    st.functions = st.functions.filter((f) => f.name !== params.name);
    return { status: 204 };
  });
  route(mock, "GET", `${F}/{name}/versions`, ({ params }) => (find(params.name!) ? { status: 200, body: { items: st.versions[params.name!] ?? [] } } : nf()));
  route(mock, "POST", `${F}/{name}/rollback`, ({ params, body }) => {
    const fn = find(params.name!);
    if (!fn) return nf();
    const vs = st.versions[fn.name] ?? [];
    const want = (body as { version?: number } | undefined)?.version ?? vs.find((v) => v.version < fn.active_version)?.version;
    const target = vs.find((v) => v.version === want);
    if (!target) return envelope(400, "no_previous_version", "no earlier version");
    vs.forEach((v) => (v.active = v.version === target.version));
    fn.active_version = target.version;
    return { status: 200, body: fn };
  });
  route(mock, "GET", `${F}/{name}/logs`, ({ params, query }) => {
    const limit = Number(query.get("limit") ?? 50);
    return { status: 200, body: { items: (st.logs[params.name!] ?? []).slice(0, limit) } };
  });
  route(mock, "GET", `${F}/{name}/secrets`, ({ params }) => ({ status: 200, body: { items: st.secrets[params.name!] ?? [] } }));
  route(mock, "PUT", `${F}/{name}/secrets/{key}`, ({ params, body }) => {
    const list = (st.secrets[params.name!] ??= []);
    const s: SecretInfo = { name: params.key!, created_at: NOW, updated_at: NOW };
    st.secrets[params.name!] = [...list.filter((x) => x.name !== s.name), s].sort((a, b) => a.name.localeCompare(b.name));
    st.secretValues[`${params.name}/${params.key}`] = (body as { value: string }).value;
    return { status: 200, body: s };
  });
  route(mock, "DELETE", `${F}/{name}/secrets/{key}`, ({ params }) => {
    st.secrets[params.name!] = (st.secrets[params.name!] ?? []).filter((x) => x.name !== params.key);
    return { status: 204 };
  });
  return st;
}

const job = (id: string, status: JobStatus, extra: Partial<Job> = {}): Job => ({
  id, queue: "default", kind: "email.send", payload: { to: "a@b.c" }, priority: 0, status, run_at: NOW, attempts: status === "queued" ? 0 : 1, max_attempts: 3,
  backoff_base_ms: 1000, backoff_max_ms: 60000, visibility_timeout_ms: 30000, cancel_requested: false, created_by: "admin", created_at: NOW, updated_at: NOW, ...extra,
});

export interface JobsState { jobs: Job[]; events: Record<string, JobEvent[]> }

export function installJobs(mock: MockApi): JobsState {
  const st: JobsState = {
    jobs: [
      job("job_q", "queued"),
      job("job_r", "running", { queue: "mail", worker_id: "w1" }),
      job("job_d", "dead", { last_error: "smtp refused", attempts: 3 }),
      job("job_s", "succeeded", { result: { sent: true }, finished_at: NOW }),
    ],
    events: { job_d: [{ id: 1, at: NOW, event: "enqueued", attempt: 0 }, { id: 2, at: NOW, event: "dead", attempt: 3, message: "smtp refused" }] },
  };
  const J = "/api/v1/jobs";
  const nf = () => envelope(404, "not_found", "no such job");
  const find = (id: string) => st.jobs.find((j) => j.id === id);
  const stats = () => {
    const totals = { queued: 0, running: 0, succeeded: 0, dead: 0, cancelled: 0 };
    const queues = new Map<string, typeof totals & { queue: string }>();
    for (const j of st.jobs) {
      totals[j.status]++;
      const q = queues.get(j.queue) ?? { queue: j.queue, queued: 0, running: 0, succeeded: 0, dead: 0, cancelled: 0 };
      q[j.status]++;
      queues.set(j.queue, q);
    }
    return { totals, queues: [...queues.values()] };
  };
  route(mock, "GET", `${J}/stats`, () => ({ status: 200, body: stats() }), true);
  route(mock, "GET", J, ({ query }) => {
    const s = query.get("status"), q = query.get("queue"), k = query.get("kind");
    const all = st.jobs.filter((j) => (!s || j.status === s) && (!q || j.queue === q) && (!k || j.kind === k));
    const off = Number(query.get("offset") ?? 0), lim = Number(query.get("limit") ?? 50);
    return { status: 200, body: { items: all.slice(off, off + lim), total: all.length } };
  }, true);
  route(mock, "GET", `${J}/schedules`, () => ({ status: 200, body: { items: [{ id: "s1", name: "nightly", cron: "0 3 * * *", queue: "default", kind: "report", payload: {}, priority: 0, max_attempts: 3, backoff_base_ms: 1000, backoff_max_ms: 60000, visibility_timeout_ms: 30000, enabled: true, next_run_at: NOW, created_by: "admin", created_at: NOW, updated_at: NOW }] } }), true);
  route(mock, "GET", `${J}/{id}`, ({ params }) => { const j = find(params.id!); return j ? { status: 200, body: j } : nf(); }, true);
  route(mock, "GET", `${J}/{id}/events`, ({ params }) => (find(params.id!) ? { status: 200, body: { items: st.events[params.id!] ?? [] } } : nf()), true);
  route(mock, "POST", `${J}/{id}/retry`, ({ params }) => {
    const j = find(params.id!);
    if (!j) return nf();
    if (j.status !== "dead" && j.status !== "cancelled") return envelope(409, "job_not_retryable", "not dead or cancelled");
    j.status = "queued"; j.attempts = 0; j.last_error = null;
    return { status: 200, body: j };
  }, true);
  route(mock, "POST", `${J}/{id}/cancel`, ({ params }) => {
    const j = find(params.id!);
    if (!j) return nf();
    if (j.status === "queued") j.status = "cancelled";
    else if (j.status === "running") j.cancel_requested = true;
    else return envelope(409, "job_finished", "already finished");
    return { status: 200, body: j };
  }, true);
  route(mock, "DELETE", `${J}/{id}`, ({ params }) => {
    const j = find(params.id!);
    if (!j) return nf();
    if (j.status === "queued" || j.status === "running") return envelope(409, "job_active", "not finished");
    st.jobs = st.jobs.filter((x) => x.id !== j.id);
    return { status: 204 };
  }, true);
  return st;
}
