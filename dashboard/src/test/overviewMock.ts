/**
 * Overview's project probes on a MockApi: API keys, functions, storage totals and the audit log. Collections come
 * from the MockApi itself (`new MockApi({ collections })`).
 *
 * Usage:
 *   const mock = signedIn(new MockApi({ collections: [makeCollection("notes", [])] }).install());
 *   const st = installOverview(mock, { keys: [key()], objects: 2 });
 *   st.functions.push(fn("hello"));     // later probes see it
 */
import type { ApiKeyView, AuditEntry, FunctionInfo } from "../api";
import type { MockApi } from "./mockApi";

export interface OverviewState {
  keys: ApiKeyView[];
  functions: FunctionInfo[];
  buckets: number;
  objects: number;
  audit: AuditEntry[];
}

export function key(over: Partial<ApiKeyView> = {}): ApiKeyView {
  return {
    id: "k1", kind: "api_key", name: "app", owner_type: "service_account", collection: null, owner_id: "sa1",
    scopes: ["collections:read"], created_at: "2026-11-07T10:00:00Z", expires_at: null, last_used_at: null, revoked: false, ...over,
  };
}

export function fn(name: string, over: Partial<FunctionInfo> = {}): FunctionInfo {
  return {
    id: `fn_${name}`, name, description: "", access: "public", active_version: 1, latest_version: 1, principal: `function:${name}`,
    created_at: "2026-11-07T10:00:00Z", updated_at: "2026-11-07T10:00:00Z", created_by: "admin:adm1", ...over,
  } as FunctionInfo;
}

export function auditEntry(id: number, event: string, over: Partial<AuditEntry> = {}): AuditEntry {
  return {
    id, at: "2026-11-07T10:00:00Z", event, outcome: "success", actor: "admin:adm1", subject_type: "admin", collection: null,
    subject_id: "adm1", ip: "127.0.0.1", user_agent: "test", request_id: `req-${id}`, detail: {}, ...over,
  };
}

export function installOverview(mock: MockApi, init: Partial<OverviewState> = {}): OverviewState {
  const st: OverviewState = { keys: [], functions: [], buckets: 1, objects: 0, audit: [], ...init };
  mock.on("GET", "/api/auth/api-keys", () => ({ status: 200, body: { items: st.keys } }));
  mock.on("GET", "/api/v1/functions", () => ({ status: 200, body: { items: st.functions } }));
  mock.on("GET", "/api/v1/storage/stats", () => ({
    status: 200,
    body: { buckets: st.buckets, objects: st.objects, versions: st.objects, logical_bytes: st.objects * 10, stored_bytes: st.objects * 10, open_uploads: 0 },
  }));
  mock.on("GET", "/api/auth/audit", ({ query }) => ({ status: 200, body: { items: st.audit.slice(0, Number(query.get("limit") ?? 50)) } }));
  return st;
}
