/**
 * In-memory fake of the PostgreSQL parts of the Database studio (FB-155): `/api/system/info`, direct logins,
 * extensions, and the SQL endpoint's answers for `pg_policies` and `pg_stat_statements`. These routes are newer than
 * the OpenAPI fixture, so they are not contract-checked until god regenerates it.
 */
import type { DbLogin, ExtensionInfo } from "../api";
import { envelope, type MockApi, type MockReply, type MockRequest } from "./mockApi";
import { simpleView } from "./topologyMock";

export interface PgState {
  backend: "postgres" | "sqlite";
  defaultBackend: string;
  features: Record<string, { engine: string; state: string }>;
  logins: DbLogin[];
  extensions: ExtensionInfo[];
  policies: [string, string, string, string, string, string, string, string][];
  top: [string, number, number, number, number][];
  /** Every SQL body, in order. */
  sql: Record<string, unknown>[];
  /** Overrides the SQL endpoint's answer for a body. */
  sqlHandler?: (b: Record<string, unknown>) => MockReply | undefined;
  rotations: string[];
  created: Record<string, unknown>[];
  /** Calls to DELETE: `login:<name>` or `ext:<name>`. */
  deleted: string[];
}

export const login = (name: string, extra: Partial<DbLogin> = {}): DbLogin => ({
  name, role: `fbu_${name}`, principal: "service:reporting", tenant: null, readonly: true, admin: false, connection_limit: 10,
  statement_timeout_ms: 30000, idle_timeout_ms: 60000, valid_until: null, has_claims: true, claims_at: "2026-11-11T10:00:00Z",
  created_by: "admin", created_at: "2026-11-10T10:00:00Z", rotated_at: null, ...extra,
});

export const extension = (name: string, managed_by: string, installed: string | null, extra: Partial<ExtensionInfo> = {}): ExtensionInfo => ({
  name, managed_by, available: true, default_version: "1.0", installed_version: installed, schema: installed ? "public" : null, comment: `${name} module`, ...extra,
});

export function installPg(mock: MockApi, init: Partial<PgState> = {}): PgState {
  const st: PgState = {
    backend: "postgres", defaultBackend: "postgres",
    features: { vector: { engine: "pgvector 0.8.7", state: "active" }, trigram: { engine: "pg_trgm 1.6", state: "active" }, graph: { engine: "portable", state: "portable" } },
    logins: [login("reporting"), login("ghost", { principal: "user:users/gone", has_claims: false })],
    extensions: [extension("pg_stat_statements", "api", "1.11"), extension("hstore", "api", null), extension("vector", "feature:vector", "0.8.7"), extension("citext", "schema", "1.6")],
    policies: [
      ["public", "orders", "fb_rule_select", "PERMISSIVE", "SELECT", "{forgebase_login}", "(owner = 1)", ""],
      ["public", "orders", "orders_everyone", "PERMISSIVE", "SELECT", "{public}", "true", ""],
      ["public", "orders", "orders_tenant_only", "RESTRICTIVE", "ALL", "{public}", "(tenant = 'a')", ""],
    ],
    top: [["SELECT * FROM orders WHERE id = $1", 120, 340.5, 2.84, 120]],
    sql: [], rotations: [], created: [], deleted: [],
    ...init,
  };
  mock.on("GET", "/api/system/info", () => ({
    status: 200,
    body: {
      name: "forgebase", version: "0.3.0", mode: "embedded", data_dir: "/d", started_at: "2026-11-11T00:00:00Z", uptime_secs: 5,
      database: {
        backend: st.backend, mode: st.backend === "postgres" ? "managed" : null, path: "/d/db", size_bytes: 1, journal_mode: "wal", schema_version: 40,
        server_version: st.backend === "postgres" ? "18.1" : "3.46", connections: st.backend === "postgres" ? 3 : null,
        engine: st.backend === "postgres" ? { state: "ready", version: "18.1" } : null,
        default_backend: st.defaultBackend, bundled_postgres: st.defaultBackend === "postgres",
        features: st.backend === "postgres" ? Object.fromEntries(Object.entries(st.features).map(([k, v]) => [k, { ...v, mode: "auto" }])) : {},
      },
    },
  }));
  // The SQL editor asks whether the server is sharded (FB-176); a test that cares installs test/topologyMock.ts, which wins.
  mock.onFallback("GET", "/api/system/database/topology", () => ({ status: 200, body: simpleView() }));
  mock.onFallback("GET", "/api/auth/service-accounts", () => ({ status: 200, body: { items: [] } }));
  const unavailable = (what: string): MockReply | undefined => st.backend === "postgres" ? undefined : envelope(400, `${what}_unavailable`, `${what} need PostgreSQL`);
  const route = (m: string, t: string, h: (r: MockRequest) => MockReply) => mock.on(m, t, (r) => unavailable(t.includes("extensions") ? "extensions" : "db_logins") ?? h(r));
  route("GET", "/api/v1/system/db-logins", () => ({ status: 200, body: { items: st.logins } }));
  route("POST", "/api/v1/system/db-logins", ({ body }) => {
    const b = body as Record<string, unknown> & { name: string; principal?: string };
    st.created.push(b);
    if (b.principal === "service:nobody") return envelope(404, "not_found", "no such service account");
    const l = login(b.name, { principal: String(b.principal ?? "user:users/me"), readonly: !!b.readonly, tenant: (b.tenant as string) ?? null });
    st.logins.push(l);
    return { status: 201, body: { login: l, password: "s3cret-PASSWORD-once", hint: `host=/run/pg port=5432 dbname=forgebase user=${l.role}` } };
  });
  route("POST", "/api/v1/system/db-logins/{name}/rotate", ({ params }) => {
    st.rotations.push(params.name!);
    return { status: 200, body: { login: st.logins.find((l) => l.name === params.name), password: "rotated-PASSWORD-once", hint: `user=fbu_${params.name}` } };
  });
  route("DELETE", "/api/v1/system/db-logins/{name}", ({ params }) => { st.deleted.push(`login:${params.name}`); st.logins = st.logins.filter((l) => l.name !== params.name); return { status: 204 }; });
  route("GET", "/api/v1/system/extensions", () => ({ status: 200, body: { items: st.extensions } }));
  route("POST", "/api/v1/system/extensions", ({ body }) => {
    const name = (body as { name: string }).name;
    const e = st.extensions.find((x) => x.name === name)!;
    e.installed_version = e.default_version;
    return { status: 201, body: e };
  });
  route("DELETE", "/api/v1/system/extensions/{name}", ({ params }) => {
    st.deleted.push(`ext:${params.name}`);
    const e = st.extensions.find((x) => x.name === params.name)!;
    e.installed_version = null;
    return { status: 204 };
  });
  mock.on("POST", "/api/v1/sql", ({ body }) => {
    const b = body as Record<string, unknown>;
    st.sql.push(b);
    const own = st.sqlHandler?.(b);
    if (own) return own;
    const sql = String(b.sql);
    const base = { kind: "read", rows_affected: 0, truncated: false, duration_ms: 1.2 };
    if (sql.includes("pg_policies")) return { status: 200, body: { ...base, columns: ["schemaname", "tablename", "policyname", "permissive", "cmd", "roles", "qual", "with_check"], rows: st.policies } };
    if (sql.includes("pg_stat_statements")) return { status: 200, body: { ...base, columns: ["query", "calls", "total_exec_time", "mean_exec_time", "rows"], rows: st.top } };
    return { status: 200, body: { ...base, columns: ["one"], rows: [[1]] } };
  });
  return st;
}
