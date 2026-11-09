import { render } from "@testing-library/react";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import type { ApiKeyView, AuditEntry, AuthzAuditRow, ServiceAccountView, SessionView, Suspension } from "../../api";
import { MockApi, makeCollection } from "../../test/mockApi";
import AuthPage from "./AuthPage";


/** In-memory fake of the `/api/auth/*` admin routes (outside the OpenAPI document). */
export class AuthFake {
  mock: MockApi;
  sessions: SessionView[] = [
    { id: "s1", auth_method: "password", mfa: true, cookie: false, ip: "10.0.0.1", user_agent: "UA", device_name: "Laptop", created_at: "2026-11-09T09:00:00Z", last_seen_at: "2026-11-09T10:00:00Z", expires_at: "2026-11-10T09:00:00Z" },
    { id: "s2", auth_method: "magic_link", mfa: false, cookie: false, ip: "10.0.0.2", user_agent: "UA2", device_name: "Phone", created_at: "2026-11-09T09:00:00Z", last_seen_at: "2026-11-09T10:00:00Z", expires_at: "2026-11-10T09:00:00Z" },
  ];
  suspensions: Suspension[] = [];
  sas: ServiceAccountView[] = [
    { id: "sa1", name: "ci-bot", description: "CI", role: "service", created_by: "admin:a", created_at: "2026-11-01T00:00:00Z", disabled: false },
  ];
  keys: ApiKeyView[] = [
    { id: "k1", kind: "api_key", name: "deploy", owner_type: "service_account", collection: null, owner_id: "sa1", scopes: ["collections:read"], created_at: "2026-11-01T00:00:00Z", expires_at: null, last_used_at: null, revoked: false },
  ];
  audit: AuditEntry[] = Array.from({ length: 60 }, (_, i) => ({
    id: 60 - i, at: "2026-11-09T10:00:00Z", event: i % 2 ? "login" : "login.failed", outcome: i % 2 ? "success" : "failure",
    actor: "user:users/u1", subject_type: "user", collection: "users", subject_id: "u1", ip: "1.1.1.1", user_agent: null, request_id: null, detail: {},
    impersonator: i === 0 ? "adm1" : null,
  }));
  authz: AuthzAuditRow[] = [
    { at: "2026-11-09T10:00:00Z", realm_id: "default", principal: "user:users/u1", tenant: null, org_id: null, permission: "data:posts:delete", resource: null, step: "permission", reason: "missing permission", request_id: null, impersonator: "adm1" },
  ];
  users = [
    { id: "u1", email: "ann@example.com", verified: true, created: "2026-11-01" },
    { id: "u2", email: "bob@example.com", verified: false, created: "2026-11-02" },
  ];
  requests: string[] = [];
  forbidden = false;

  constructor() {
    const users = makeCollection("users", [{ name: "email", type: "email" }]);
    users.type = "auth";
    this.mock = new MockApi({ collections: [users, makeCollection("posts", [])] }).install();
    const m = this.mock;
    const guard = (f: () => { status: number; body?: unknown }) => () =>
      this.forbidden ? { status: 403, body: { error: { code: "forbidden", message: "admins only", request_id: "r" } } } : f();
    m.on("GET", "/api/v1/collections/{collection}/records", ({ query }) => {
      const s = (query.get("search") ?? "").toLowerCase();
      const items = this.users.filter((u) => u.email.includes(s));
      return { status: 200, body: { items, page: 1, per_page: 25, total_items: items.length, total_pages: 1, next_cursor: null } };
    });
    m.on("GET", "/api/auth/users/{collection}/{id}", guard(() => ({ status: 200, body: { user: { id: "u1", email: "ann@example.com" }, mfa_enabled: true, active_sessions: this.sessions.length, last_login_at: "2026-11-09T10:00:00Z" } })));
    m.on("GET", "/api/auth/users/{collection}/{id}/sessions", () => ({ status: 200, body: { items: this.sessions } }));
    m.on("DELETE", "/api/auth/sessions/{id}", ({ params }) => {
      this.sessions = this.sessions.filter((s) => s.id !== params.id);
      return { status: 204 };
    });
    m.on("POST", "/api/auth/users/{collection}/{id}/sessions/revoke", () => {
      const revoked = this.sessions.length;
      this.sessions = [];
      return { status: 200, body: { revoked } };
    });
    m.on("GET", "/api/auth/users/{collection}/{id}/login-history", () => ({ status: 200, body: { items: this.audit.slice(0, 2) } }));
    m.on("GET", "/api/v1/suspensions", () => ({ status: 200, body: this.suspensions }));
    m.on("POST", "/api/v1/suspensions", ({ body }) => {
      const b = body as Suspension;
      this.suspensions.push({ principal: b.principal, reason: b.reason, until: b.until ?? null });
      return { status: 204 };
    });
    m.on("POST", "/api/v1/suspensions/lift", ({ body }) => {
      this.suspensions = this.suspensions.filter((s) => s.principal !== (body as Suspension).principal);
      return { status: 204 };
    });
    m.on("GET", "/api/auth/service-accounts", guard(() => ({ status: 200, body: { items: this.sas } })));
    m.on("POST", "/api/auth/service-accounts", ({ body }) => {
      const b = body as { name: string; description?: string; role?: string };
      const sa = { id: `sa${this.sas.length + 1}`, name: b.name, description: b.description ?? "", role: b.role ?? "service", created_by: "admin:a", created_at: "2026-11-09T00:00:00Z", disabled: false };
      this.sas.push(sa);
      return { status: 201, body: sa };
    });
    m.on("DELETE", "/api/auth/service-accounts/{id}", ({ params }) => {
      this.sas = this.sas.filter((s) => s.id !== params.id);
      return { status: 204 };
    });
    m.on("GET", "/api/auth/service-accounts/{id}/keys", ({ params }) => ({ status: 200, body: { items: this.keys.filter((k) => k.owner_id === params.id) } }));
    m.on("POST", "/api/auth/service-accounts/{id}/keys", ({ params, body }) => {
      const b = body as { name: string; scopes: string[] };
      const k: ApiKeyView = { id: `k${this.keys.length + 1}`, kind: "api_key", name: b.name, owner_type: "service_account", collection: null, owner_id: params.id!, scopes: b.scopes, created_at: "2026-11-09T00:00:00Z", expires_at: null, last_used_at: null, revoked: false };
      this.keys.push(k);
      return { status: 201, body: { ...k, key: `fbk_${k.id}_SECRETVALUE` } };
    });
    m.on("GET", "/api/auth/api-keys", () => ({ status: 200, body: { items: this.keys } }));
    m.on("DELETE", "/api/auth/api-keys/{id}", ({ params }) => {
      const k = this.keys.find((x) => x.id === params.id);
      if (k) k.revoked = true;
      return { status: 204 };
    });
    m.on("POST", "/api/auth/tokens", ({ body }) => {
      const b = body as { name: string; scopes: string[] };
      const k: ApiKeyView = { id: `p${this.keys.length + 1}`, kind: "pat", name: b.name, owner_type: "admin", collection: null, owner_id: "adm1", scopes: b.scopes, created_at: "2026-11-09T00:00:00Z", expires_at: null, last_used_at: null, revoked: false };
      this.keys.push(k);
      return { status: 201, body: { ...k, key: `fbp_${k.id}_PATSECRET` } };
    });
    m.on("GET", "/api/auth/audit", ({ query }) => {
      this.requests.push(query.toString());
      const ev = query.get("event");
      const before = query.get("before");
      let rows = this.audit.filter((r) => (!ev || r.event.startsWith(ev)) && (!before || r.id < Number(before)));
      rows = rows.slice(0, Number(query.get("limit") ?? 50));
      return { status: 200, body: { items: rows } };
    });
    m.on("GET", "/api/v1/authz/audit", () => ({ status: 200, body: this.authz }));
    m.on("GET", "/api/auth/admins", () => ({ status: 200, body: { items: [{ id: "adm1", email: "root@example.com", created_at: "2026-10-01T00:00:00Z", last_login_at: null }] } }));
  }
}

export function renderAuth(path = "/auth") {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route path="auth/*" element={<AuthPage />} />
      </Routes>
    </MemoryRouter>,
  );
}
