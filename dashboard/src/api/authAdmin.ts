/** Typed client for the Auth Studio (docs/adr/0014-admin-studio.md section 2). */
import { request, send } from "./client";
import type { ListResponse } from "./types";

const AUTH = "/api/auth";
const V1 = "/api/v1";
const enc = encodeURIComponent;

export interface SessionView {
  id: string;
  auth_method: string;
  mfa: boolean;
  cookie: boolean;
  ip: string | null;
  user_agent: string | null;
  device_name: string | null;
  created_at: string;
  last_seen_at: string;
  expires_at: string;
  impersonator?: string | null;
  current?: boolean;
}

export interface AuditEntry {
  id: number;
  at: string;
  event: string;
  outcome: string;
  actor: string;
  subject_type: string | null;
  collection: string | null;
  subject_id: string | null;
  ip: string | null;
  user_agent: string | null;
  request_id: string | null;
  detail: unknown;
  impersonator?: string | null;
}

export interface AuthzAuditRow {
  at: string;
  realm_id: string;
  principal: string;
  tenant: string | null;
  org_id: string | null;
  permission: string;
  resource: string | null;
  step: string;
  reason: string;
  request_id: string | null;
  impersonator?: string | null;
}

export interface UserDetail {
  user: Record<string, unknown>;
  mfa_enabled: boolean;
  active_sessions: number;
  last_login_at: string | null;
}

export interface ApiKeyView {
  id: string;
  kind: "api_key" | "pat" | string;
  name: string;
  owner_type: string;
  collection: string | null;
  owner_id: string;
  scopes: string[];
  created_at: string;
  expires_at: string | null;
  last_used_at: string | null;
  revoked: boolean;
}
export type CreatedKey = ApiKeyView & { key: string };

export interface ServiceAccountView {
  id: string;
  name: string;
  description: string;
  role: string;
  created_by: string;
  created_at: string;
  disabled: boolean;
}

export interface AdminView {
  id: string;
  email: string;
  created_at: string;
  last_login_at: string | null;
}

export interface Suspension {
  principal: string;
  reason: string;
  until: string | number | null;
  created_at?: string | number;
  created_by?: string;
}

export interface AuditFilter {
  event?: string;
  subject_id?: string;
  before?: number;
  limit?: number;
}

const items = async <T>(p: Promise<{ items: T[] }>) => (await p).items;
const userBase = (c: string, id: string) => `${AUTH}/users/${enc(c)}/${enc(id)}`;

export const getUserDetail = (c: string, id: string, signal?: AbortSignal) => request<UserDetail>(userBase(c, id), { signal });
export const userSessions = (c: string, id: string, signal?: AbortSignal) =>
  items(request<{ items: SessionView[] }>(`${userBase(c, id)}/sessions`, { signal }));
export const revokeSession = (sessionId: string) => send<void>("DELETE", `${AUTH}/sessions/${enc(sessionId)}`);
export const revokeAllSessions = (c: string, id: string) =>
  send<{ revoked: number }>("POST", `${userBase(c, id)}/sessions/revoke`, {});
export const userLoginHistory = (c: string, id: string, limit = 50, signal?: AbortSignal) =>
  items(request<{ items: AuditEntry[] }>(`${userBase(c, id)}/login-history?limit=${limit}`, { signal }));

export const searchUsers = (c: string, q: { search?: string; page?: number; per_page?: number }, signal?: AbortSignal) => {
  const p = new URLSearchParams();
  if (q.search) p.set("search", q.search);
  p.set("page", String(q.page ?? 1));
  p.set("per_page", String(q.per_page ?? 25));
  return request<ListResponse>(`${V1}/collections/${enc(c)}/records?${p}`, { signal });
};

export const listSuspensions = async (signal?: AbortSignal) => {
  const r = await request<Suspension[] | { items: Suspension[] }>(`${V1}/suspensions`, { signal });
  return Array.isArray(r) ? r : r.items;
};
export const suspend = (principal: string, reason: string, until?: string | null) =>
  send<void>("POST", `${V1}/suspensions`, { principal, reason, until: until ?? null });
export const unsuspend = (principal: string) => send<void>("POST", `${V1}/suspensions/lift`, { principal });

export const listServiceAccounts = (signal?: AbortSignal) =>
  items(request<{ items: ServiceAccountView[] }>(`${AUTH}/service-accounts`, { signal }));
export const createServiceAccount = (b: { name: string; description?: string; role?: string }) =>
  send<ServiceAccountView>("POST", `${AUTH}/service-accounts`, b);
export const deleteServiceAccount = (id: string) => send<void>("DELETE", `${AUTH}/service-accounts/${enc(id)}`);
export const serviceAccountKeys = (id: string, signal?: AbortSignal) =>
  items(request<{ items: ApiKeyView[] }>(`${AUTH}/service-accounts/${enc(id)}/keys`, { signal }));
export const createServiceAccountKey = (id: string, b: { name: string; scopes: string[]; expires_in_days?: number }) =>
  send<CreatedKey>("POST", `${AUTH}/service-accounts/${enc(id)}/keys`, b);

export const listApiKeys = (signal?: AbortSignal) => items(request<{ items: ApiKeyView[] }>(`${AUTH}/api-keys`, { signal }));
export const createPat = (b: { name: string; scopes: string[]; expires_in_days?: number }) =>
  send<CreatedKey>("POST", `${AUTH}/tokens`, b);
export const revokeKey = (id: string) => send<void>("DELETE", `${AUTH}/api-keys/${enc(id)}`);

export const listAdmins = (signal?: AbortSignal) => items(request<{ items: AdminView[] }>(`${AUTH}/admins`, { signal }));

export const authAudit = (f: AuditFilter = {}, signal?: AbortSignal) => {
  const p = new URLSearchParams();
  if (f.event) p.set("event", f.event);
  if (f.subject_id) p.set("subject_id", f.subject_id);
  if (f.before !== undefined) p.set("before", String(f.before));
  p.set("limit", String(f.limit ?? 50));
  return items(request<{ items: AuditEntry[] }>(`${AUTH}/audit?${p}`, { signal }));
};
export const authzAudit = async (limit = 100, signal?: AbortSignal) => {
  const r = await request<AuthzAuditRow[] | { items: AuthzAuditRow[] }>(`${V1}/authz/audit?limit=${limit}`, { signal });
  return Array.isArray(r) ? r : r.items;
};
