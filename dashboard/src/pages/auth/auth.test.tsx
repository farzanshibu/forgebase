import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AuthFake, renderAuth } from "./authMock";

const { startImpersonation } = vi.hoisted(() => ({ startImpersonation: vi.fn(async () => {}) }));
vi.mock("../../auth/session", () => ({
  useSession: () => ({ admin: { id: "adm1", email: "root@example.com" }, impersonation: null, startImpersonation }),
}));

let fake: AuthFake;
beforeEach(() => {
  fake = new AuthFake();
  startImpersonation.mockClear();
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("Users", () => {
  it("lists users of the auth collection and searches", async () => {
    renderAuth("/auth");
    expect(await screen.findByText("ann@example.com")).toBeInTheDocument();
    expect(screen.getByText("bob@example.com")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Search users"), "bob{enter}");
    await waitFor(() => expect(screen.queryByText("ann@example.com")).not.toBeInTheDocument());
    expect(screen.getByText("bob@example.com")).toBeInTheDocument();
    expect(fake.mock.calls.some((c) => c.includes("/records"))).toBe(true);
  });
  it("shows a 403 as sign-in required", async () => {
    fake.forbidden = true;
    renderAuth("/auth/users/users/u1");
    expect(await screen.findByRole("alert")).toBeInTheDocument();
  });
});

describe("User detail", () => {
  it("shows MFA, sessions and history; revokes one and all sessions", async () => {
    renderAuth("/auth/users/users/u1");
    expect(await screen.findByText("enabled")).toBeInTheDocument();
    expect(await screen.findByText("Laptop", { exact: false })).toBeInTheDocument();
    expect(await screen.findByText("login.failed")).toBeInTheDocument();
    await userEvent.click(screen.getByLabelText("Revoke session s1"));
    await waitFor(() => expect(screen.queryByText("Laptop", { exact: false })).not.toBeInTheDocument());
    expect(fake.sessions.map((s) => s.id)).toEqual(["s2"]);
    await userEvent.click(screen.getByRole("button", { name: "Revoke all" }));
    await userEvent.click(screen.getByRole("button", { name: /confirm revoke all/i }));
    expect(await screen.findByText("No active sessions.")).toBeInTheDocument();
    expect(fake.sessions).toHaveLength(0);
  });
  it("suspends and unsuspends", async () => {
    renderAuth("/auth/users/users/u1");
    await screen.findByText("Active");
    const btn = screen.getByRole("button", { name: "Suspend" });
    expect(btn).toBeDisabled();
    await userEvent.type(screen.getByLabelText("Suspension reason"), "abuse");
    await userEvent.click(btn);
    expect(await screen.findByText("Suspended")).toBeInTheDocument();
    expect(fake.suspensions[0]).toMatchObject({ principal: "user:users/u1", reason: "abuse" });
    await userEvent.click(screen.getByRole("button", { name: "Unsuspend" }));
    await screen.findByText("Active");
    expect(fake.suspensions).toHaveLength(0);
  });
  it("impersonation needs a reason", async () => {
    renderAuth("/auth/users/users/u1");
    await userEvent.click(await screen.findByRole("button", { name: "Impersonate" }));
    const dlg = screen.getByRole("dialog");
    const go = within(dlg).getByRole("button", { name: "Start impersonation" });
    expect(go).toBeDisabled();
    await userEvent.type(within(dlg).getByLabelText("Reason"), "support ticket 42");
    await userEvent.click(go);
    await waitFor(() => expect(startImpersonation).toHaveBeenCalledWith("user:users/u1", "support ticket 42", 15));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });
});

describe("Service accounts", () => {
  it("creates, shows the key secret once, revokes, deletes", async () => {
    renderAuth("/auth/service-accounts");
    expect(await screen.findByText("ci-bot")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Service account name"), "etl");
    await userEvent.click(screen.getByRole("button", { name: "Create" }));
    expect(await screen.findByText("etl")).toBeInTheDocument();

    await userEvent.click(screen.getByLabelText("Keys of ci-bot"));
    expect(await screen.findByText("deploy")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Key name"), "ci");
    await userEvent.type(screen.getByLabelText("Scopes"), "collections:read, kv:read");
    await userEvent.click(screen.getByRole("button", { name: "Create key" }));
    const dlg = await screen.findByRole("dialog");
    expect(within(dlg).getByLabelText("Secret")).toHaveTextContent("fbk_k2_SECRETVALUE");
    expect(fake.mock.bodies["post /api/auth/service-accounts/{id}/keys"]?.[0]).toMatchObject({ scopes: ["collections:read", "kv:read"] });
    await userEvent.click(within(dlg).getByRole("button", { name: "Done" }));
    expect(screen.queryByText(/SECRETVALUE/)).not.toBeInTheDocument();

    const row = screen.getByText("deploy").closest("tr")!;
    await userEvent.click(within(row).getByRole("button", { name: "Revoke" }));
    await userEvent.click(within(row).getByRole("button", { name: /confirm revoke/i }));
    await waitFor(() => expect(fake.keys.find((k) => k.id === "k1")?.revoked).toBe(true));

    const sarow = screen.getByText("etl").closest("tr")!;
    await userEvent.click(within(sarow).getByRole("button", { name: "Delete" }));
    await userEvent.click(within(sarow).getByRole("button", { name: /confirm delete/i }));
    await waitFor(() => expect(screen.queryByText("etl")).not.toBeInTheDocument());
  });
});

describe("API keys and PATs", () => {
  it("lists every key, creates a PAT with a one-time secret, revokes", async () => {
    renderAuth("/auth/keys");
    expect(await screen.findByText("deploy")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Token name"), "cli");
    await userEvent.type(screen.getByLabelText("Token scopes"), "collections:read");
    await userEvent.click(screen.getByRole("button", { name: "Create token" }));
    const dlg = await screen.findByRole("dialog");
    expect(within(dlg).getByLabelText("Secret")).toHaveTextContent("fbp_p2_PATSECRET");
    await userEvent.click(within(dlg).getByRole("button", { name: "Done" }));
    expect(screen.queryByText(/PATSECRET/)).not.toBeInTheDocument();
    const row = (await screen.findByText("cli")).closest("tr")!;
    await userEvent.click(within(row).getByRole("button", { name: "Revoke" }));
    await userEvent.click(within(row).getByRole("button", { name: /confirm revoke/i }));
    expect(await screen.findByText("revoked")).toBeInTheDocument();
  });
});

describe("Audit log", () => {
  it("filters by event prefix, pages older entries, highlights the impersonator", async () => {
    renderAuth("/auth/audit");
    expect(await screen.findByText("adm1")).toBeInTheDocument();
    expect(screen.getAllByRole("row")).toHaveLength(51);
    await userEvent.click(screen.getByRole("button", { name: "Load older" }));
    await waitFor(() => expect(screen.getAllByRole("row")).toHaveLength(61));
    expect(fake.requests.some((r) => r.includes("before=11"))).toBe(true);
    await userEvent.type(screen.getByLabelText("Event prefix"), "login.failed");
    await userEvent.click(screen.getByRole("button", { name: "Filter" }));
    await waitFor(() => expect(screen.queryAllByText("success")).toHaveLength(0));
    expect(fake.requests.some((r) => r.includes("event=login.failed"))).toBe(true);
  });
  it("authorization tab shows the failed pipeline step", async () => {
    renderAuth("/auth/audit");
    await userEvent.click(await screen.findByRole("tab", { name: "Authorization" }));
    expect(await screen.findByText("permission", { selector: "span" })).toBeInTheDocument();
    expect(screen.getByText("data:posts:delete")).toBeInTheDocument();
    expect(screen.getByText("adm1")).toBeInTheDocument();
  });
});

describe("Admins", () => {
  it("lists administrators", async () => {
    renderAuth("/auth/admins");
    expect(await screen.findByText("root@example.com")).toBeInTheDocument();
  });
});
