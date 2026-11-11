import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import App from "../App";
import { setCsrfToken } from "../api";
import { ADMIN, signedIn } from "../test/helpers";
import { MockApi } from "../test/mockApi";

const UNAUTH = { status: 401, body: { error: { code: "invalid_credentials", message: "invalid email or password", request_id: "r" } } };

/** A server where `me` answers as a realm administrator once the user signs in through /api/auth/login. */
function realmServer() {
  const mock = signedIn(new MockApi().install(), false);
  // The first matching route wins: put the realm administrator's `me` ahead of signedIn's.
  mock.custom.unshift({ method: "GET", template: "/api/auth/me", handler: () =>
    mock.cookieSession
      ? { status: 200, body: { subject: { type: "user", id: "u1" }, admin_scope: { realm: "acme" } } }
      : { status: 401, body: { error: { code: "unauthenticated", message: "authentication required", request_id: "r" } } } });
  mock.on("POST", "/api/auth/login", ({ body }) => {
    if ((body as { password: string }).password !== "correct-horse") return UNAUTH;
    mock.cookieSession = true;
    return { status: 200, body: { token_type: "Bearer", expires_in: 900, session_id: "s2", csrf_token: "csrf-2", subject: { type: "user", id: "u1" } } };
  });
  return mock;
}

beforeEach(() => {
  setCsrfToken(null);
  document.cookie = "fb_csrf=; Max-Age=0";
});
afterEach(() => vi.unstubAllGlobals());

function mount() {
  render(<MemoryRouter initialEntries={["/"]}><App /></MemoryRouter>);
}

describe("LoginPage: which sign-in to use", () => {
  it("says which kind of administrator signs in how, and defaults to the instance administrator", async () => {
    mount();
    const group = await screen.findByRole("group", { name: "Sign in as" });
    const instance = within(group).getByRole("radio", { name: /Instance administrator/ });
    expect(instance).toBeChecked();
    expect(group).toHaveTextContent(/administrator account made when the server was set up/);
    expect(within(group).getByRole("radio", { name: /Realm administrator/ })).not.toBeChecked();
    expect(group).toHaveTextContent(/you do not need an instance administrator account/);
    expect(screen.queryByLabelText("Realm")).not.toBeInTheDocument();
  });

  it("an instance administrator signs in at /api/auth/admins/login", async () => {
    const mock = signedIn(new MockApi().install(), false);
    mount();
    const u = userEvent.setup();
    await u.type(await screen.findByLabelText("Email"), ADMIN.email);
    await u.type(screen.getByLabelText("Password"), "correct-horse");
    await u.click(screen.getByRole("button", { name: "Sign in" }));
    expect(await screen.findByText(ADMIN.email)).toBeInTheDocument();
    expect(mock.bodies["post /api/auth/admins/login"]).toHaveLength(1);
  });

  it("a realm administrator signs in as a user of their realm, and the nav hides System and Billing", async () => {
    const mock = realmServer();
    mount();
    const u = userEvent.setup();
    await u.click(await screen.findByRole("radio", { name: /Realm administrator/ }));
    await u.type(screen.getByLabelText("Realm"), "acme");
    await u.type(screen.getByLabelText("Email"), "ops@acme.test");
    await u.type(screen.getByLabelText("Password"), "correct-horse");
    await u.click(screen.getByRole("button", { name: "Sign in" }));
    const nav = await screen.findByRole("navigation", { name: "Studios" });
    expect(mock.bodies["post /api/auth/login"]?.[0]).toMatchObject({ email: "ops@acme.test", cookie: true });
    expect(mock.bodies["post /api/auth/admins/login"]).toBeUndefined();
    expect(mock.headers.some((h) => h["X-ForgeBase-Realm"] === "acme")).toBe(true);
    expect(within(nav).getByRole("link", { name: "Identity" })).toBeInTheDocument();
    expect(within(nav).queryByRole("link", { name: "System" })).not.toBeInTheDocument();
    expect(within(nav).queryByRole("link", { name: "Billing" })).not.toBeInTheDocument();
    expect(within(nav).queryByRole("link", { name: "Features and settings" })).not.toBeInTheDocument();
  });

  it("a user who is not a realm administrator is told so, not that they are not an administrator", async () => {
    const mock = signedIn(new MockApi().install(), false);
    mock.on("POST", "/api/auth/login", () => {
      mock.cookieSession = false;
      return { status: 200, body: { token_type: "Bearer", expires_in: 900, session_id: "s3", csrf_token: "c", subject: { type: "user", id: "u9" } } };
    });
    mount();
    const u = userEvent.setup();
    await u.click(await screen.findByRole("radio", { name: /Realm administrator/ }));
    await u.type(screen.getByLabelText("Email"), "pat@acme.test");
    await u.type(screen.getByLabelText("Password"), "correct-horse");
    await u.click(screen.getByRole("button", { name: "Sign in" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/not a realm administrator/);
  });

  it("an instance administrator sees System and Billing in the nav", async () => {
    signedIn(new MockApi().install(), true);
    mount();
    const nav = await screen.findByRole("navigation", { name: "Studios" });
    expect(within(nav).getByRole("link", { name: "System" })).toBeInTheDocument();
  });
});
