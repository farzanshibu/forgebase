/**
 * In-memory ForgeBase API behind a stubbed `fetch`.
 *
 * Every request must match a route of the real /api/openapi.json (else the mock
 * answers 599 and records it in `problems`), and every JSON response is
 * validated against the schema the document declares for it.
 */
import { vi } from "vitest";
import type { CollectionDef, IndexInfo, RecordObject } from "../api";
import { hasResponse, matchRoute, responseSchemaName, validateBody } from "./contract";

const NOW = "2026-11-07T10:00:00Z";

export function envelope(status: number, code: string, message: string, details?: unknown) {
  return { status, body: { error: { code, message, request_id: "req-test", ...(details ? { details } : {}) } } };
}

export interface MockOptions {
  requireAuth?: boolean;
  collections?: CollectionDef[];
  records?: Record<string, RecordObject[]>;
}

export function sysFields(): CollectionDef["fields"] {
  return ["id", "created", "updated", "_version"].map((name) => ({ id: `sys_${name}`, name, type: "text", required: false, unique: false, hidden: name === "_version", system: true }));
}

export function makeCollection(name: string, fields: { name: string; type: string; required?: boolean }[]): CollectionDef {
  return {
    id: `col_${name}`, name, type: "base", system: false, schema_mode: "strict", versioned: false,
    fields: [...sysFields(), ...fields.map((f) => ({ id: `f_${name}_${f.name}`, required: false, unique: false, hidden: false, system: false, ...f }))],
    view_query: null, options: {}, version: 1, created: NOW, updated: NOW,
  };
}

/** A request as seen by a custom route handler (`MockApi.on`). */
export interface MockRequest {
  method: string;
  path: string;
  params: Record<string, string>;
  query: URLSearchParams;
  body: unknown;
  headers: Record<string, string>;
}
export type MockReply = { status: number; body?: unknown };
export type MockHandler = (req: MockRequest) => MockReply | Promise<MockReply>;

export class MockApi {
  /** Routes outside the OpenAPI document (`/api/auth/*`) and anything a test wants to stub. Matched before the OpenAPI routes. */
  custom: { method: string; template: string; handler: MockHandler }[] = [];
  /** Register `method template` (`{name}` segments are params), e.g. `mock.on("GET", "/api/auth/users/{collection}/{id}", ...)`. */
  on(method: string, template: string, handler: MockHandler): this {
    this.custom.push({ method: method.toUpperCase(), template, handler });
    return this;
  }
  private matchCustom(method: string, pathname: string) {
    const parts = pathname.split("/");
    for (const c of this.custom) {
      if (c.method !== method) continue;
      const tp = c.template.split("/");
      const wild = tp[tp.length - 1]?.startsWith("{*") ?? false;
      if (wild ? parts.length < tp.length : tp.length !== parts.length) continue;
      const params: Record<string, string> = {};
      const ok = tp.every((seg, i) => {
        if (wild && i === tp.length - 1) { params[seg.slice(2, -1)] = parts.slice(i).map((x) => decodeURIComponent(x)).join("/"); return true; }
        if (seg.startsWith("{")) { params[seg.slice(1, -1)] = decodeURIComponent(parts[i] ?? ""); return true; }
        return seg === parts[i];
      });
      if (ok) return { c, params };
    }
    return undefined;
  }

  collections: CollectionDef[];
  records: Record<string, RecordObject[]>;
  indexes: IndexInfo[] = [];
  slow = { threshold_ms: 100, items: [{ sql: "SELECT * FROM t WHERE x = ?", param_count: 1, duration_ms: 250.5, at: NOW, rows: 3, label: null }] };
  /** `method path` of every request received. */
  calls: string[] = [];
  /** Request bodies by `method path`. */
  bodies: Record<string, unknown[]> = {};
  headers: Record<string, string>[] = [];
  /** Things that would make the real server or the OpenAPI document disagree with the dashboard. */
  problems: string[] = [];
  requireAuth: boolean;
  /** A cookie session is active (set by `signedIn`): satisfies `requireAuth` like a real `fb_session` cookie. */
  cookieSession = false;
  private seq = 100;

  constructor(opts: MockOptions = {}) {
    this.requireAuth = !!opts.requireAuth;
    this.collections = opts.collections ?? [];
    this.records = opts.records ?? {};
  }

  install() {
    vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL, init?: RequestInit) => this.handle(String(input), init ?? {})));
    return this;
  }

  private respond(status: number, body: unknown, schemaName?: string, text?: string) {
    return { status, body, schemaName, text };
  }

  private async handle(url: string, init: RequestInit): Promise<Response> {
    const u = new URL(url, "http://localhost");
    const method = (init.method ?? "GET").toUpperCase();
    const headers = (init.headers ?? {}) as Record<string, string>;
    this.calls.push(`${method.toLowerCase()} ${u.pathname}`);
    this.headers.push(headers);
    const custom = this.matchCustom(method, u.pathname);
    if (custom) {
      let cb: unknown;
      if (typeof init.body === "string" && init.body) {
        try { cb = JSON.parse(init.body); } catch { cb = init.body; }
      } else if (init.body) {
        cb = init.body; // raw bytes (storage uploads): Blob / Uint8Array
      }
      (this.bodies[`${method.toLowerCase()} ${custom.c.template}`] ??= []).push(cb);
      const reply = await custom.c.handler({ method, path: u.pathname, params: custom.params, query: u.searchParams, body: cb, headers });
      return new Response(reply.body === undefined ? null : JSON.stringify(reply.body), { status: reply.status });
    }
    const route = matchRoute(method, u.pathname);
    if (!route) {
      this.problems.push(`not in openapi.json: ${method} ${u.pathname}`);
      return new Response(JSON.stringify(envelope(404, "not_found", "no route").body), { status: 599 });
    }
    let reqBody: unknown;
    if (typeof init.body === "string" && init.body && (headers["Content-Type"] ?? "").includes("json")) reqBody = JSON.parse(init.body);
    const key = `${method.toLowerCase()} ${route.template}`;
    (this.bodies[key] ??= []).push(typeof init.body === "string" && reqBody === undefined ? init.body : reqBody);

    let out: { status: number; body?: unknown; schemaName?: string; text?: string };
    if (this.requireAuth && !headers.Authorization && !this.cookieSession) out = envelope(401, "unauthenticated", "authentication required");
    else out = this.route(method, route.template, route.params, u.searchParams, reqBody, init.body, headers);

    if (out.status < 400) {
      if (!hasResponse(route.op, out.status)) this.problems.push(`${key}: status ${out.status} not documented`);
      const name = responseSchemaName(route.op, out.status);
      if (name && out.body !== undefined) {
        for (const p of validateBody(name, out.body)) this.problems.push(`${key} -> ${name}: ${p}`);
      }
    } else {
      for (const p of validateBody("ErrorEnvelope", out.body)) this.problems.push(`error envelope: ${p}`);
    }
    if (out.text !== undefined) return new Response(out.text, { status: out.status });
    return new Response(out.body === undefined ? null : JSON.stringify(out.body), { status: out.status });
  }

  private coll(name: string) {
    return this.collections.find((c) => c.name === name);
  }

  private route(method: string, t: string, p: Record<string, string>, q: URLSearchParams, body: unknown, rawBody: unknown, headers: Record<string, string>) {
    const r = this.respond.bind(this);
    const c = p.collection ? this.coll(p.collection) : undefined;
    if (p.collection && !c) return envelope(404, "not_found", `no such collection ${p.collection}`);
    const rows = c ? (this.records[c.name] ??= []) : [];
    switch (`${method} ${t}`) {
      case "GET /api/v1/collections": return r(200, { items: this.collections }, "CollectionList");
      case "POST /api/v1/collections": {
        const def = body as CollectionDef;
        if (!def.name) return envelope(400, "validation_failed", "invalid collection", { fields: { name: { code: "required", message: "name is required" } } });
        if (this.coll(def.name)) return envelope(409, "conflict", "name taken");
        const made = { ...makeCollection(def.name, []), ...def, fields: [...sysFields(), ...def.fields.map((f, i) => ({ id: `f${i}`, required: false, unique: false, hidden: false, system: false, ...f }))] };
        this.collections.push(made);
        return r(201, made, "CollectionSchema");
      }
      case "PUT /api/v1/collections/{collection}": {
        const def = body as CollectionDef;
        Object.assign(c!, def, { fields: def.fields.map((f, i) => ({ id: `f${i}`, required: false, unique: false, hidden: false, system: false, ...f })), version: (c!.version ?? 1) + 1 });
        return r(200, c, "CollectionSchema");
      }
      case "DELETE /api/v1/collections/{collection}":
        this.collections = this.collections.filter((x) => x !== c);
        return r(204, undefined);
      case "GET /api/v1/collections/{collection}/records": {
        const page = Number(q.get("page") ?? 1), per = Number(q.get("per_page") ?? 25);
        let items = [...rows];
        const f = q.get("filter");
        if (f) {
          const m = /^(\w+)\s*(=|~)\s*"?(.*?)"?$/.exec(f);
          if (!m) return envelope(400, "invalid_filter", `cannot parse filter: ${f}`);
          items = items.filter((x) => (m[2] === "=" ? String(x[m[1]!]) === m[3] : String(x[m[1]!]).includes(m[3]!)));
        }
        const sort = q.get("sort");
        if (sort) {
          const desc = sort.startsWith("-"), k = sort.replace(/^-/, "");
          items.sort((a, b) => (String(a[k]) < String(b[k]) ? -1 : 1) * (desc ? -1 : 1));
        }
        return r(200, { items: items.slice((page - 1) * per, page * per), page, per_page: per, total_items: items.length, total_pages: Math.max(1, Math.ceil(items.length / per)), next_cursor: null }, "ListResponse");
      }
      case "POST /api/v1/collections/{collection}/records": {
        const rec = body as RecordObject;
        const fields: Record<string, { code: string; message: string }> = {};
        for (const f of c!.fields) if (f.required && !f.system && (rec[f.name] === undefined || rec[f.name] === "")) fields[f.name] = { code: "required", message: `${f.name} is required` };
        if (Object.keys(fields).length) return envelope(400, "validation_failed", "record is invalid", { fields });
        const made = { id: `rec${++this.seq}`, created: NOW, updated: NOW, _version: 1, ...rec };
        rows.push(made);
        return r(201, made, undefined);
      }
      case "GET /api/v1/collections/{collection}/records/{id}": {
        const rec = rows.find((x) => x.id === p.id);
        return rec ? r(200, rec) : envelope(404, "not_found", "no such record");
      }
      case "PATCH /api/v1/collections/{collection}/records/{id}":
      case "PUT /api/v1/collections/{collection}/records/{id}": {
        const rec = rows.find((x) => x.id === p.id);
        if (!rec) return envelope(404, "not_found", "no such record");
        const want = headers["If-Match"]?.replaceAll('"', "");
        if (want && Number(want) !== rec._version) return envelope(409, "version_conflict", "stale version");
        Object.assign(rec, body, { _version: (rec._version ?? 1) + 1 });
        return r(200, rec);
      }
      case "DELETE /api/v1/collections/{collection}/records/{id}":
        this.records[c!.name] = rows.filter((x) => x.id !== p.id);
        return r(204, undefined);
      case "GET /api/v1/collections/{collection}/export":
        return r(200, undefined, undefined, JSON.stringify(rows));
      case "POST /api/v1/collections/{collection}/import": {
        const parsed = JSON.parse(String(rawBody)) as RecordObject[];
        for (const x of parsed) rows.push({ id: `rec${++this.seq}`, created: NOW, updated: NOW, _version: 1, ...x });
        return r(200, { imported: parsed.length, format: q.get("format") ?? "json" }, "ImportResult");
      }
      case "GET /api/v1/indexes": {
        const coll = q.get("collection");
        return r(200, { items: this.indexes.filter((i) => !coll || i.collection === coll) }, "IndexList");
      }
      case "POST /api/v1/indexes": {
        const def = body as { name?: string; collection: string; keys: { path: string }[]; unique?: boolean };
        const info: IndexInfo = {
          name: def.name || `idx_${def.collection}_${def.keys.map((k) => k.path).join("_")}`, table: `c_${def.collection}`, collection: def.collection,
          origin: "manual", kind: "btree", unique: !!def.unique, partial: false, columns: def.keys.map((k) => k.path),
          definition: { collection: def.collection, keys: def.keys }, sql: null, stats: { stat: null, rows_estimate: 0, size_bytes: 4096 },
        };
        this.indexes.push(info);
        return r(201, info, "IndexInfoSchema");
      }
      case "GET /api/v1/indexes/recommendations":
        return r(200, { items: [{ definition: { collection: "products", keys: [{ path: "price" }] }, reason: "range filter in slow queries", occurrences: 4, max_duration_ms: 310.2 }] }, "IndexRecommendations");
      case "POST /api/v1/indexes/analyze": return r(204, undefined);
      case "DELETE /api/v1/indexes/{name}":
        this.indexes = this.indexes.filter((i) => i.name !== p.name);
        return r(204, undefined);
      case "POST /api/v1/indexes/{name}/rebuild": return r(200, this.indexes.find((i) => i.name === p.name), "IndexInfoSchema");
      case "GET /api/v1/indexes/{name}/stats": return r(200, { stat: "10 2", rows_estimate: 10, size_bytes: 8192 }, "IndexStatsSchema");
      case "GET /api/v1/slow-queries": return r(200, this.slow, "SlowQueryLog");
      case "DELETE /api/v1/slow-queries":
        this.slow.items = [];
        return r(204, undefined);
      case "PUT /api/v1/slow-queries/threshold":
        this.slow.threshold_ms = (body as { threshold_ms: number }).threshold_ms;
        return r(200, body, "ThresholdBody");
      case "POST /api/v1/sql": {
        const sql = (body as { sql: string }).sql;
        if (/^\s*select 1 as one/i.test(sql)) return r(200, { kind: "read", columns: ["one"], rows: [[1]], rows_affected: 0, last_insert_id: null, truncated: false, duration_ms: 0.4 }, "SqlResultSchema");
        return envelope(400, "sql_error", `near "${sql.slice(0, 8)}": syntax error`);
      }
      case "POST /api/v1/sql/explain":
        return r(200, { sql: (body as { sql: string }).sql, steps: [{ id: 2, parent: 0, detail: "SCAN products" }], indexes_used: [], full_scans: ["products"] }, "QueryPlanSchema");
      case "GET /api/v1/sql/tables":
        return r(200, { items: this.collections.map((x) => ({ name: `c_${x.name}`, kind: "table", collection: x.name, columns: [], indexes: [], sql: null })) }, "TableList");
      default:
        this.problems.push(`mock has no handler for ${method} ${t}`);
        return envelope(501, "not_implemented", "mock");
    }
  }
}
