import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import App from "../App";
import { listCollections, setCsrfToken } from "../api";
import Layout from "../components/Layout";
import { ADMIN, signedIn } from "../test/helpers";
import { MockApi } from "../test/mockApi";
import { SessionProvider, useSession } from "./session";

const fail = (status: number, code: string, message: string) => ({ status, body: { error: { code, message, request_id: "r" } } });

function mount(mock: MockApi, path = "/") {
  render(<MemoryRouter initialEntries={[path]}><App /></MemoryRouter>);
  return mock;
}

beforeEach(() => {
  setCsrfToken(null);
  document.cookie = "fb_csrf=; Max-Age=0";
});
afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

describe("sign-in", () => {
  it("signs in, shows the admin and loads the studios", async () => {
    const mock = signedIn(new MockApi().install(), false);
    mount(mock);
    const u = userEvent.setup();
    await u.type(await screen.findByLabelText("Email"), ADMIN.email);
    await u.type(screen.getByLabelText("Password"), "correct-horse");
    await u.click(screen.getByRole("button", { name: "Sign in" }));
    expect(await screen.findByText(ADMIN.email)).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Studios" })).toBeInTheDocument();
    expect(mock.bodies["post /api/auth/admins/login"]?.[0]).toMatchObject({ email: ADMIN.email, cookie: true });
  });

  it("shows the server's error for a wrong password and stays on the form", async () => {
    const mock = signedIn(new MockApi().install(), false);
    mount(mock);
    const u = userEvent.setup();
    await u.type(await screen.findByLabelText("Email"), ADMIN.email);
    await u.type(screen.getByLabelText("Password"), "nope");
    await u.click(screen.getByRole("button", { name: "Sign in" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("invalid email or password");
    expect(screen.getByRole("form", { name: "Sign in" })).toBeInTheDocument();
  });

  it("asks for the MFA code when required and finishes sign-in", async () => {
    const mock = new MockApi().install();
    mock.cookieSession = false;
    mock.on("GET", "/api/auth/me", () =>
      mock.cookieSession ? { status: 200, body: { subject: { type: "admin", id: "a" }, admin: ADMIN } } : fail(401, "unauthenticated", "no"));
    mock.on("POST", "/api/auth/admins/login", () => ({ status: 200, body: { mfa_required: true, mfa_token: "mt1", expires_in: 300 } }));
    mock.on("POST", "/api/auth/mfa/verify", ({ body }) => {
      if ((body as { code: string }).code !== "123456") return fail(401, "invalid_code", "invalid code");
      mock.cookieSession = true;
      return { status: 200, body: { session_id: "s", csrf_token: "c", subject: { type: "admin", id: "a" } } };
    });
    mount(mock);
    const u = userEvent.setup();
    await u.type(await screen.findByLabelText("Email"), ADMIN.email);
    await u.type(screen.getByLabelText("Password"), "x");
    await u.click(screen.getByRole("button", { name: "Sign in" }));
    const code = await screen.findByLabelText("Authentication code");
    await u.type(code, "000000");
    await u.click(screen.getByRole("button", { name: "Verify" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("invalid code");
    await u.type(screen.getByLabelText("Authentication code"), "123456");
    await u.click(screen.getByRole("button", { name: "Verify" }));
    expect(await screen.findByText(ADMIN.email)).toBeInTheDocument();
    expect(mock.bodies["post /api/auth/mfa/verify"]?.[1]).toEqual({ mfa_token: "mt1", code: "123456" });
  });
});

describe("session lifecycle", () => {
  it("sends X-CSRF-Token on unsafe requests only", async () => {
    const mock = signedIn(new MockApi().install(), false);
    mount(mock);
    const u = userEvent.setup();
    await u.type(await screen.findByLabelText("Email"), ADMIN.email);
    await u.type(screen.getByLabelText("Password"), "correct-horse");
    await u.click(screen.getByRole("button", { name: "Sign in" }));
    await screen.findByText(ADMIN.email);
    await u.click(screen.getByRole("button", { name: "Sign out" }));
    await screen.findByRole("form", { name: "Sign in" });
    const post = mock.calls.indexOf("post /api/auth/logout");
    expect(mock.headers[post]?.["X-CSRF-Token"]).toBe("csrf-1");
    expect(mock.headers[mock.calls.indexOf("get /api/auth/me")]?.["X-CSRF-Token"]).toBeUndefined();
  });

  it("prefers the fb_csrf cookie", async () => {
    document.cookie = "fb_csrf=from-cookie";
    const mock = signedIn(new MockApi().install());
    mount(mock);
    await userEvent.click(await screen.findByRole("button", { name: "Sign out" }));
    await waitFor(() => expect(mock.calls).toContain("post /api/auth/logout"));
    expect(mock.headers[mock.calls.indexOf("post /api/auth/logout")]?.["X-CSRF-Token"]).toBe("from-cookie");
  });

  it("signs out: calls logout and shows the form without an expiry notice", async () => {
    const mock = signedIn(new MockApi().install());
    mount(mock);
    await userEvent.click(await screen.findByRole("button", { name: "Sign out" }));
    expect(await screen.findByRole("form", { name: "Sign in" })).toBeInTheDocument();
    expect(mock.cookieSession).toBe(false);
    expect(screen.queryByText(/session expired/i)).not.toBeInTheDocument();
  });
});

/** Mount the shell with a probe route that exposes `startImpersonation`. */
async function mountWithStart() {
  let start!: ReturnType<typeof useSession>["startImpersonation"];
  function Tap() {
    start = useSession().startImpersonation;
    return <p>tap</p>;
  }
  render(
    <MemoryRouter>
      <SessionProvider>
        <Routes>
          <Route element={<Layout />}>
            <Route path="*" element={<Tap />} />
          </Route>
        </Routes>
      </SessionProvider>
    </MemoryRouter>,
  );
  await screen.findByText("tap");
  await screen.findByText(ADMIN.email);
  return (...a: Parameters<typeof start>) => start(...a);
}

const grant = (ms: number, reason = "r") => ({
  status: 200,
  body: {
    access_token: "imp-tok", expires_in: Math.round(ms / 1000), expires_at: new Date(Date.now() + ms).toISOString(), session_id: "imps1",
    impersonator: ADMIN.id, subject: { type: "user", collection: "users", id: "u1" }, reason,
  },
});

describe("impersonation", () => {
  it("shows the banner, uses the bearer, and ends with one click", async () => {
    const mock = signedIn(new MockApi().install());
    mock.on("POST", "/api/auth/impersonate", ({ body }) => grant(600_000, (body as { reason: string }).reason));
    const start = await mountWithStart();
    await act(() => start("user:users/u1", "support ticket 42", 10));
    const banner = await screen.findByRole("alert", { name: "Impersonation active" });
    expect(banner).toHaveTextContent("Impersonating user:users/u1");
    expect(banner).toHaveTextContent("support ticket 42");
    expect(mock.bodies["post /api/auth/impersonate"]?.[0]).toEqual({ principal: "user:users/u1", reason: "support ticket 42", ttl_minutes: 10 });
    await listCollections();
    expect(mock.headers.at(-1)?.Authorization).toBe("Bearer imp-tok");
    await userEvent.click(screen.getByRole("button", { name: "End impersonation" }));
    await waitFor(() => expect(screen.queryByRole("alert", { name: "Impersonation active" })).not.toBeInTheDocument());
    expect(mock.headers[mock.calls.lastIndexOf("post /api/auth/logout")]?.Authorization).toBe("Bearer imp-tok");
    expect(mock.cookieSession).toBe(true); // the admin session is untouched
    await listCollections();
    expect(mock.headers.at(-1)?.Authorization).toBeUndefined();
    expect(screen.getByText(ADMIN.email)).toBeInTheDocument();
  });

  it("ends itself at expiry and returns to the admin session", async () => {
    const mock = signedIn(new MockApi().install());
    mock.on("POST", "/api/auth/impersonate", () => grant(1500));
    const start = await mountWithStart();
    await act(() => start("user:users/u1", "r"));
    expect(await screen.findByRole("alert", { name: "Impersonation active" })).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole("alert", { name: "Impersonation active" })).not.toBeInTheDocument(), { timeout: 4000 });
    await listCollections();
    expect(mock.headers.at(-1)?.Authorization).toBeUndefined();
    expect(screen.getByText(ADMIN.email)).toBeInTheDocument();
  });

  it("surfaces the server's refusal for an administrator target", async () => {
    const mock = signedIn(new MockApi().install());
    mock.on("POST", "/api/auth/impersonate", () => fail(403, "cannot_impersonate_admin", "administrators cannot be impersonated"));
    const start = await mountWithStart();
    await expect(start("admin:other", "why")).rejects.toMatchObject({ code: "cannot_impersonate_admin" });
    expect(screen.queryByRole("alert", { name: "Impersonation active" })).not.toBeInTheDocument();
  });
});

describe("session expiry", () => {
  it("returns to sign-in with a notice when an authenticated request gets 401", async () => {
    const mock = signedIn(new MockApi({ requireAuth: true }).install());
    mount(mock);
    await screen.findByText(ADMIN.email);
    mock.cookieSession = false;
    await act(async () => {
      await listCollections().catch(() => undefined);
    });
    expect(await screen.findByText("Your session expired. Sign in again.")).toBeInTheDocument();
    expect(screen.getByRole("form", { name: "Sign in" })).toBeInTheDocument();
  });
});
