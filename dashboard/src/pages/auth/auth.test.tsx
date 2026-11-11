import { cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AuthFake, renderAuth } from "./authMock";

const { startImpersonation } = vi.hoisted(() => ({ startImpersonation: vi.fn(async () => {}) }));
const sessionState = vi.hoisted(() => ({ adminScope: "platform" as "platform" | { realm: string } }));
vi.mock("../../auth/session", () => {
  const session = () => ({ admin: { id: "adm1", email: "root@example.com" }, adminScope: sessionState.adminScope, impersonation: null, startImpersonation });
  return { useSession: session, useOptionalSession: session };
});

let fake: AuthFake;
beforeEach(() => {
  fake = new AuthFake();
  sessionState.adminScope = "platform";
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
  it("creates one in a sheet, makes a key for it, revokes the key and deletes the account by typing its name", async () => {
    renderAuth("/auth/service-accounts");
    expect(await screen.findByText("ci-bot")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "New service account" }));
    const form = await screen.findByRole("form", { name: "New service account" });
    await userEvent.type(within(form).getByLabelText(/^Name/), "etl");
    await userEvent.click(within(form).getByRole("button", { name: "Create service account" }));
    expect(await screen.findByText("etl")).toBeInTheDocument();

    await userEvent.click(screen.getByLabelText("Keys of ci-bot"));
    expect(await screen.findByText("deploy")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Create key" }));
    const keyForm = await screen.findByRole("form", { name: "Create API key" });
    // the sheet opens on this account
    expect(within(keyForm).getByRole("combobox", { name: /Service account/ })).toHaveTextContent("ci-bot");
    await userEvent.type(within(keyForm).getByLabelText(/^App name/), "ci");
    await userEvent.click(within(within(keyForm).getByRole("group", { name: "Key-value" })).getByRole("checkbox", { name: "Read keys" }));
    await userEvent.click(within(keyForm).getByRole("button", { name: "Create key" }));
    const dlg = await screen.findByRole("dialog");
    expect(within(dlg).getByLabelText("Secret")).toHaveTextContent("fbk_k2_SECRETVALUE");
    expect(fake.mock.bodies["post /api/auth/service-accounts/{id}/keys"]?.[0]).toMatchObject({ scopes: ["kv:read"] });
    await userEvent.click(within(dlg).getByRole("button", { name: "Done" }));
    expect(screen.queryByText(/SECRETVALUE/)).not.toBeInTheDocument();

    const row = screen.getByText("deploy").closest("tr")!;
    await userEvent.click(within(row).getByRole("button", { name: "Revoke deploy" }));
    await userEvent.click(await screen.findByRole("button", { name: "Revoke key" }));
    await waitFor(() => expect(fake.keys.find((k) => k.id === "k1")?.revoked).toBe(true));

    const sarow = screen.getByText("etl").closest("tr")!;
    await userEvent.click(within(sarow).getByRole("button", { name: "Delete etl" }));
    const confirm = await screen.findByRole("button", { name: "Delete service account" });
    expect(confirm).toBeDisabled();
    await userEvent.type(screen.getByRole("textbox"), "etl");
    await userEvent.click(confirm);
    await waitFor(() => expect(screen.queryByText("etl")).not.toBeInTheDocument());
  });

  it("explains an empty list", async () => {
    fake.sas = [];
    renderAuth("/auth/service-accounts");
    const empty = await screen.findByRole("region", { name: "No service accounts yet" });
    expect(empty).toHaveTextContent("owns the API keys");
  });
});

describe("Create API key", () => {
  async function openDialog(path = "/auth/keys?new=1") {
    renderAuth(path);
    return screen.findByRole("form", { name: "Create API key" });
  }
  const area = (form: HTMLElement, name: string) => within(within(form).getByRole("group", { name }));

  it("opens from /auth/keys?new=1, builds collections:read:<name> from the checkboxes, shows the secret once beside a curl example", async () => {
    const form = await openDialog();
    await userEvent.type(within(form).getByLabelText(/^App name/), "web");
    const records = area(form, "Records");
    expect(within(form).getByText("collections:read")).toBeInTheDocument(); // the scope string shows under the checkbox
    await userEvent.click(records.getByRole("checkbox", { name: "Read records" }));
    await userEvent.click(records.getByRole("checkbox", { name: "All collections" }));
    expect(records.getByText(/Pick at least one collection/)).toBeInTheDocument();
    await userEvent.click(records.getByRole("checkbox", { name: "posts" }));
    // under the checkbox and in the summary line
    expect(within(form).getAllByText("collections:read:posts", { selector: "code" })).toHaveLength(2);
    await userEvent.click(within(form).getByRole("button", { name: "Create key" }));

    const dlg = await screen.findByRole("dialog");
    expect(within(dlg).getByLabelText("Secret")).toHaveTextContent(/fbk_k\d+_SECRETVALUE/);
    expect(within(dlg).getByLabelText("curl example")).toHaveTextContent("<YOUR_API_KEY>");
    expect(within(dlg).getByLabelText("curl example")).toHaveTextContent("/api/v1/collections/posts/records");
    expect(within(dlg).getByLabelText("curl example")).not.toHaveTextContent("SECRETVALUE");
    // a new service account named after the app owns the key
    expect(fake.mock.bodies["post /api/auth/service-accounts"]?.[0]).toMatchObject({ name: "web", role: "service" });
    expect(fake.mock.bodies["post /api/auth/service-accounts/{id}/keys"]?.[0]).toEqual({ name: "web", scopes: ["collections:read:posts"], expires_in_days: 90 });
    await userEvent.click(within(dlg).getByRole("button", { name: "Done" }));
    expect(screen.queryByText(/SECRETVALUE/)).not.toBeInTheDocument();
  });

  it("grants nothing until something is ticked, and write does not tick read", async () => {
    const form = await openDialog();
    await userEvent.type(within(form).getByLabelText(/^App name/), "web");
    expect(within(form).getByRole("button", { name: "Create key" })).toBeDisabled();
    const files = area(form, "Files");
    await userEvent.click(files.getByRole("checkbox", { name: "Upload and delete files" }));
    expect(files.getByRole("checkbox", { name: "Download and list files" })).not.toBeChecked();
    expect(within(form).getByRole("button", { name: "Create key" })).toBeEnabled();
    await userEvent.click(within(form).getByRole("button", { name: "Create key" }));
    await screen.findByRole("dialog");
    expect(fake.mock.bodies["post /api/auth/service-accounts/{id}/keys"]?.[0]).toMatchObject({ scopes: ["storage:write"] });
  });

  it("binds the key to a tenant, with an expiry of never", async () => {
    const form = await openDialog();
    await userEvent.type(within(form).getByLabelText(/^App name/), "tenant-app");
    await userEvent.click(area(form, "Records").getByRole("checkbox", { name: "Read records" }));
    await userEvent.type(within(form).getByLabelText(/Tenant/), "acme");
    await userEvent.click(within(form).getByRole("combobox", { name: /Expires/ }));
    await userEvent.click(await screen.findByRole("option", { name: "Never expires" }));
    await userEvent.click(within(form).getByRole("button", { name: "Create key" }));
    await screen.findByRole("dialog");
    expect(fake.mock.bodies["post /api/auth/service-accounts/{id}/keys"]?.[0]).toEqual({ name: "tenant-app", scopes: ["collections:read"], tenant: "acme" });
  });

  it("puts a server's tenant refusal next to the tenant field and reuses the service account on retry", async () => {
    fake.rejectTenant = true;
    const form = await openDialog();
    await userEvent.type(within(form).getByLabelText(/^App name/), "web");
    await userEvent.click(area(form, "Records").getByRole("checkbox", { name: "Read records" }));
    await userEvent.type(within(form).getByLabelText(/Tenant/), "nope");
    await userEvent.click(within(form).getByRole("button", { name: "Create key" }));
    expect(await within(form).findByText("This tenant does not exist")).toBeInTheDocument();
    expect(within(form).getByLabelText(/Tenant/)).toHaveAccessibleDescription(/This tenant does not exist/);
    fake.rejectTenant = false;
    await userEvent.click(within(form).getByRole("button", { name: "Create key" }));
    await screen.findByRole("dialog");
    expect(fake.mock.bodies["post /api/auth/service-accounts"]).toHaveLength(1);
  });

  it("makes a personal token without a service account", async () => {
    renderAuth("/auth/keys");
    await userEvent.click(await screen.findByRole("button", { name: "New personal token" }));
    const form = await screen.findByRole("form", { name: "New personal token" });
    expect(within(form).queryByRole("combobox", { name: /Service account/ })).not.toBeInTheDocument();
    await userEvent.type(within(form).getByLabelText(/^Name/), "cli");
    await userEvent.click(within(within(form).getByRole("group", { name: "Records" })).getByRole("checkbox", { name: "Read records" }));
    await userEvent.click(within(form).getByRole("button", { name: "Create token" }));
    const dlg = await screen.findByRole("dialog");
    expect(within(dlg).getByLabelText("Secret")).toHaveTextContent("fbp_p2_PATSECRET");
    await userEvent.click(within(dlg).getByRole("button", { name: "Done" }));
    expect(screen.queryByText(/PATSECRET/)).not.toBeInTheDocument();
  });
});

describe("API keys", () => {
  it("lists keys with their tenant, keeps personal tokens in a second section, and revokes", async () => {
    fake.keys.push({ id: "p9", kind: "pat", name: "my-script", owner_type: "admin", collection: null, owner_id: "adm1", scopes: ["collections:read"], tenant: "acme", created_at: "2026-11-01T00:00:00Z", expires_at: null, last_used_at: null, revoked: false });
    renderAuth("/auth/keys");
    const keys = await screen.findByRole("region", { name: "API keys" });
    expect(within(keys).getByText("deploy")).toBeInTheDocument();
    expect(within(keys).queryByText("my-script")).not.toBeInTheDocument();
    const tokens = screen.getByRole("region", { name: "Personal tokens" });
    expect(tokens).toHaveTextContent("Personal tokens (for scripts you run yourself)");
    expect(within(tokens).getByText("my-script")).toBeInTheDocument();
    expect(within(tokens).getByText("acme")).toBeInTheDocument();
    await userEvent.click(within(keys).getByRole("button", { name: "Revoke deploy" }));
    await userEvent.click(await screen.findByRole("button", { name: "Revoke key" }));
    await waitFor(() => expect(fake.keys.find((k) => k.id === "k1")?.revoked).toBe(true));
  });

  it("explains an empty list and offers the dialog", async () => {
    fake.keys = [];
    renderAuth("/auth/keys");
    const empty = await screen.findByRole("region", { name: "No API keys yet" });
    await userEvent.click(within(empty).getByRole("button", { name: "Create API key" }));
    expect(await screen.findByRole("form", { name: "Create API key" })).toBeInTheDocument();
  });

  it("names the tab API keys, and the header's primary action opens the dialog", async () => {
    renderAuth("/auth");
    expect(await screen.findByRole("tab", { name: "API keys" })).toBeInTheDocument();
    expect(screen.queryByRole("tab", { name: /tokens/i })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Create API key" }));
    expect(await screen.findByRole("form", { name: "Create API key" })).toBeInTheDocument();
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
  it("is for platform administrators: a realm administrator gets no tab and an explanation", async () => {
    sessionState.adminScope = { realm: "acme" };
    renderAuth("/auth/admins");
    expect(await screen.findByRole("region", { name: "Administrators is for platform administrators" })).toHaveTextContent("acme");
    expect(screen.queryByRole("tab", { name: "Admins" })).not.toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "API keys" })).toBeInTheDocument();
    expect(screen.queryByText("root@example.com")).not.toBeInTheDocument();
  });
});
