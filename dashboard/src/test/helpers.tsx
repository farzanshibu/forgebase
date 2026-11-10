import { render } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import App from "../App";
import { setTokenSource } from "../api";
import { MockApi, makeCollection } from "./mockApi";

export const ADMIN = { id: "adm1", email: "root@example.com" };

/** Teach the mock the admin sign-in routes and start with a live cookie session (`signedIn(mock, false)` = anonymous). */
export function signedIn<T extends MockApi>(mock: T, active = true): T {
  mock.cookieSession = active;
  const me = () => (mock.cookieSession ? { status: 200, body: { subject: { type: "admin", id: ADMIN.id }, admin: ADMIN, credential: "session_cookie", mfa: false } } : { status: 401, body: { error: { code: "unauthenticated", message: "authentication required", request_id: "r" } } });
  mock.on("GET", "/api/auth/me", me);
  mock.on("POST", "/api/auth/admins/login", ({ body }) => {
    const b = body as { email: string; password: string; cookie?: boolean };
    if (b.password !== "correct-horse") return { status: 401, body: { error: { code: "invalid_credentials", message: "invalid email or password", request_id: "r" } } };
    mock.cookieSession = true;
    return { status: 200, body: { token_type: "Bearer", expires_in: 900, session_id: "s1", csrf_token: "csrf-1", session_expires_at: "2026-11-10T00:00:00Z", subject: { type: "admin", id: ADMIN.id } } };
  });
  // The Dashboard overview opens at "/": answer its probes with "not found" (cards read "not reported") so they are not counted as problems.
  // First-run setup: the server has an administrator, and no feature is switched off unless a test says so.
  mock.onFallback("GET", "/api/setup/status", () => ({ status: 200, body: { setup_required: false } }));
  const none = () => ({ status: 404, body: { error: { code: "not_found", message: "not found", request_id: "r" } } });
  for (const path of ["/metrics", "/api/system/info", "/api/system/features", "/api/v1/storage/stats", "/api/v1/entitlements", "/api/v1/vector/stats", "/api/v1/jobs/stats", "/api/v1/kv/_info"]) mock.onFallback("GET", path, none);
  mock.on("POST", "/api/auth/logout", ({ headers }) => {
    if (headers.Authorization) return { status: 204 };
    mock.cookieSession = false;
    return { status: 204 };
  });
  return mock;
}

export function seeded(opts: { requireAuth?: boolean } = {}) {
  const products = makeCollection("products", [
    { name: "title", type: "text", required: true },
    { name: "price", type: "number" },
    { name: "meta", type: "json" },
  ]);
  const mock = new MockApi({
    ...opts,
    collections: [products],
    records: {
      products: [
        { id: "p1", created: "2026-11-01T00:00:00Z", updated: "2026-11-01T00:00:00Z", _version: 1, title: "Pen", price: 3, meta: { color: "blue" } },
        { id: "p2", created: "2026-11-02T00:00:00Z", updated: "2026-11-02T00:00:00Z", _version: 1, title: "Notebook", price: 12, meta: {} },
      ],
    },
  });
  return signedIn(mock.install());
}

export function renderStudio(path: string) {
  setTokenSource(() => null);
  return render(
    <MemoryRouter initialEntries={[path]}>
      <App />
    </MemoryRouter>,
  );
}
