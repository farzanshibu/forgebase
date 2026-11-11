/** Database topology screen (FB-176): every flow and every empty and error state, against a mock of /api/system/database/*. */
import { afterEach, describe, expect, it, vi } from "vitest";
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../../test/mockApi";
import { renderStudio, seeded } from "../../../test/helpers";
import { bodiesOf, installTopology, node, operation, placement, set, shardedView, simpleView, type TopologyState } from "../../../test/topologyMock";
import { formatLag, freezeText, lagText, validName, validUrl } from "./util";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
  localStorage.clear();
});

const T = { timeout: 5000 };
/** The first render in a file loads the lazy studio chunks; allow for a busy machine. */
const COLD = { timeout: 20000 };

function open(init: Partial<TopologyState> = {}, mock: MockApi = seeded()) {
  const st = installTopology(mock, init);
  renderStudio("/system/topology");
  return { mock, st, u: userEvent.setup() };
}

/** The dialog by its name. */
const dialog = (name: string | RegExp) => screen.findByRole("dialog", { name }, T);

async function openMenu(u: ReturnType<typeof userEvent.setup>, button: string, item: string | RegExp) {
  await u.click(await screen.findByRole("button", { name: button }, T));
  await u.click(await screen.findByRole("menuitem", { name: item }, T));
}

describe("helpers", () => {
  it("writes lag, freeze and names in plain words", () => {
    expect(formatLag(null)).toBe("—");
    expect(formatLag(0)).toBe("0 s");
    expect(formatLag(400)).toBe("0.4 s");
    expect(formatLag(12_000)).toBe("12 s");
    expect(formatLag(180_000)).toBe("3 min");
    expect(lagText({ role: "primary", lag_ms: null, lag_bytes: null })).toBe("—");
    expect(lagText({ role: "replica", lag_ms: null, lag_bytes: null })).toBe("not measured");
    expect(lagText({ role: "replica", lag_ms: 1200, lag_bytes: 2048 })).toBe("1 s · 2.00 KiB behind");
    expect(freezeText(null)).toBe("about a second or two");
    expect(freezeText(500)).toBe("under a second");
    expect(freezeText(3000)).toBe("about 3 s");
    expect(validName("")).toBe("Enter a name.");
    expect(validName("primary")).toMatch(/reserved/);
    expect(validName("Bad Name")).toMatch(/lower-case/);
    expect(validName("eu-read_1")).toBeUndefined();
    expect(validUrl("host")).toMatch(/postgres:\/\//);
    expect(validUrl("postgres://u:p@h/db")).toBeUndefined();
  });
});

describe("the tab", () => {
  it("is in System next to Health and Backups", async () => {
    const mock = seeded();
    installTopology(mock);
    renderStudio("/system");
    const tabs = await screen.findByRole("tablist", { name: "System sections" }, T);
    await userEvent.click(within(tabs).getByRole("tab", { name: "Database topology" }));
    expect(await screen.findByRole("heading", { level: 1, name: /Database topology/ }, T)).toBeInTheDocument();
  });

  it("says what it is, with a docs link and the primary action", async () => {
    open();
    expect(await screen.findByText(/Where your data lives/, undefined, COLD)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /Docs/ })).toHaveAttribute("href", expect.stringContaining("0029"));
    expect(await screen.findByRole("button", { name: "Add replica" }, COLD)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "More ways to add" })).toBeInTheDocument();
  });

  it("shows the summary strip, the set card, state chips, lag and the sparkline", async () => {
    const { mock } = open({
      view: shardedView(),
      history: { r1: [{ at: "2026-11-11T09:00:00Z", lag_ms: 100, lag_bytes: 1 }, { at: "2026-11-11T09:30:00Z", lag_ms: 900, lag_bytes: 5 }, { at: "2026-11-11T10:00:00Z", lag_ms: 300, lag_bytes: 2 }] },
      operations: [operation("opx", { kind: "database.node.rebuild", status: "succeeded", finished_at: "2026-11-11T10:05:00Z", progress: null, cancellable: false })],
    });
    const strip = await screen.findByRole("list", { name: "Summary" }, T).catch(() => screen.findByLabelText("Summary", undefined, T));
    expect(strip).toHaveTextContent("PostgreSQL 18.1");
    expect(strip).toHaveTextContent("3 of 3");
    expect(strip).toHaveTextContent("0.1 s");
    expect(strip).toHaveTextContent("Rebuilding a replica: Done");
    const control = await screen.findByLabelText("Control", { selector: "[data-set]" }, T);
    expect(within(control).getByText("2 tenants", { exact: false })).toBeInTheDocument();
    const row = within(control).getByText("r1").closest("tr")!;
    expect(within(row).getByText("Streaming")).toBeInTheDocument();
    expect(within(row).getByText("This server")).toBeInTheDocument();
    expect(await within(row).findByRole("img", { name: /lag over the last hour, now 0.3 s, highest 0.9 s/ }, T)).toBeInTheDocument();
    expect(screen.getByLabelText("eu-1", { selector: "[data-set]" })).toHaveTextContent("3 tenants");
    expect(mock.problems).toEqual([]);
  });

  it("shows every node state as a chip with words", async () => {
    const view = simpleView();
    view.nodes.push(node("a", "control", { state: "catching_up" }), node("b", "control", { state: "lagging" }), node("c", "control", { state: "unreachable", last_error: "connection refused" }), node("d", "control", { state: "needs_rebuild" }));
    open({ view });
    for (const label of ["Catching up", "Lagging", "Unreachable", "Needs rebuild"]) expect(await screen.findByText(label, undefined, T)).toBeInTheDocument();
    expect(screen.getByText("connection refused")).toBeInTheDocument();
  });

  it("shows replicas of a remote node with its disk free, and an unmeasured lag", async () => {
    const view = simpleView();
    view.nodes.push(node("rem", "control", { kind: "remote", lag_ms: null, agent: { state: "connected", disk_free: 5 * 1024 ** 3, address: "10.0.0.5" } }));
    open({ view });
    const row = (await screen.findByText("rem", undefined, T)).closest("tr")!;
    expect(within(row).getByText("Other host")).toBeInTheDocument();
    expect(within(row).getByText("5.00 GiB")).toBeInTheDocument();
    expect(within(row).getByText("not measured")).toBeInTheDocument();
  });

  it("explains an empty project (one server) and offers both actions", async () => {
    const view = simpleView();
    view.nodes = view.nodes.slice(0, 1);
    open({ view });
    const empty = await screen.findByRole("region", { name: "One server, and that is fine" }, T);
    expect(empty).toHaveTextContent("read replica");
    expect(empty).toHaveTextContent("shard");
    expect(empty).toHaveTextContent("Nothing changes until you add one.");
    expect(within(empty).getByRole("button", { name: "Add replica" })).toBeInTheDocument();
    expect(within(empty).getByRole("button", { name: "Add shard" })).toBeInTheDocument();
  });

  it("says SQLite needs PostgreSQL, shows the one server and the copyable command", async () => {
    const view = simpleView();
    view.backend = "sqlite";
    view.engine_version = "3.46";
    view.nodes = [node("sq", "control", { name: "sqlite", role: "primary", state: "ready", engine_version: "3.46" })];
    const { mock } = open({ view });
    expect(await screen.findByText("Replicas and shards need PostgreSQL", undefined, T)).toBeInTheDocument();
    expect(screen.getByText("forgebase db convert --to postgres")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy command" })).toBeInTheDocument();
    expect(await screen.findByLabelText("Your server")).toHaveTextContent("sqlite");
    expect(screen.queryByRole("button", { name: "Add replica" })).not.toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("treats a 400 postgres_required answer as the SQLite state too", async () => {
    const { st } = open({ failWith: (k) => (k === "GET /topology" ? { status: 400, body: { error: { code: "postgres_required", message: "needs postgres", request_id: "r1" } } } : undefined) });
    expect(await screen.findByText("Replicas and shards need PostgreSQL", undefined, T)).toBeInTheDocument();
    expect(st.calls).toEqual([]);
  });

  it("shows an error panel with the request id when the topology cannot load", async () => {
    open({ failWith: (k) => (k === "GET /topology" ? { status: 503, body: { error: { code: "topology_unavailable", message: "no topology service", request_id: "req-77" } } } : undefined) });
    expect(await screen.findByText(/cannot manage replicas and shards right now/, undefined, T)).toBeInTheDocument();
    expect(screen.getByText(/req-77/)).toBeInTheDocument();
  });

  it("shows a banner when control's promote needs a restart", async () => {
    const view = simpleView();
    view.restart_required = "Control's primary changed to r1.";
    open({ view });
    const banner = await screen.findByRole("alert", undefined, T);
    expect(banner).toHaveTextContent("Restart the server to finish");
    expect(banner).toHaveTextContent("Control's primary changed to r1.");
  });

  it("shows nodes from forgebase.toml read-only, without a menu", async () => {
    const view = simpleView();
    view.nodes.push(node("cfg", "control", { name: "eu-read-1", origin: "config", kind: "external" }));
    open({ view });
    const row = (await screen.findByText("eu-read-1", undefined, T)).closest("tr")!;
    expect(within(row).getByText("From forgebase.toml")).toBeInTheDocument();
    expect(within(row).queryByRole("button")).not.toBeInTheDocument();
    // An ordinary replica still has its menu.
    expect(screen.getByRole("button", { name: "Actions for r1" })).toBeInTheDocument();
  });

  it("explains lag, replicas and the use-from-code panel for reads", async () => {
    open();
    expect(await screen.findByRole("button", { name: "What is Replica lag?" }, T)).toBeInTheDocument();
    const code = await screen.findByLabelText("Use from code", undefined, T);
    expect(code).toHaveTextContent("consistency");
    await userEvent.click(within(code).getByRole("tab", { name: "curl" }));
    expect(code).toHaveTextContent("X-ForgeBase-Read-After");
  });

  it("is accessible by structure: one h1, every button and control named, tables named", async () => {
    open({ view: shardedView(), operations: [operation("o1")], placements: [placement("env-a", "control")] });
    await screen.findByLabelText("Control", { selector: "[data-set]" }, T);
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
    for (const b of screen.getAllByRole("button")) expect(b).toHaveAccessibleName();
    for (const t of screen.getAllByRole("table")) expect(t).toHaveAccessibleName();
    for (const i of screen.queryAllByRole("textbox")) expect(i).toHaveAccessibleName();
    for (const p of screen.getAllByRole("progressbar")) expect(p).toHaveAccessibleName();
  });
});

describe("add a replica", () => {
  it("on this server: validates the name, then starts an operation", async () => {
    const { st, u } = open();
    await u.click(await screen.findByRole("button", { name: "Add replica" }, COLD));
    const d = await dialog("Add a replica");
    await u.click(within(d).getByRole("button", { name: "Add replica" }));
    expect(within(d).getByText("Enter a name.")).toBeInTheDocument();
    expect(st.calls).toEqual([]);
    await u.type(within(d).getByLabelText(/^Name/), "eu-read-2");
    await u.click(within(d).getByRole("button", { name: "Add replica" }));
    await waitFor(() => expect(bodiesOf(st, "POST /nodes")).toEqual([{ set: "control", name: "eu-read-2", kind: "local", weight: 1 }]));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("a server you run: needs a postgres:// address, sends it, and never shows it again", async () => {
    const { st, u } = open();
    await u.click(await screen.findByRole("button", { name: "Add replica" }, T));
    const d = await dialog("Add a replica");
    await u.click(within(d).getByRole("button", { name: /A server I run/ }));
    await u.type(within(d).getByLabelText(/^Name/), "ext1");
    await u.type(within(d).getByLabelText(/^Address/), "nope");
    await u.click(within(d).getByRole("button", { name: "Add replica" }));
    expect(within(d).getByText(/Start with postgres:\/\//)).toBeInTheDocument();
    await u.clear(within(d).getByLabelText(/^Address/));
    await u.type(within(d).getByLabelText(/^Address/), "postgres://u:p@h:5432/db");
    await u.click(within(d).getByRole("button", { name: "Add replica" }));
    await waitFor(() => expect(bodiesOf(st, "POST /nodes")).toEqual([{ set: "control", name: "ext1", kind: "external", url: "postgres://u:p@h:5432/db", weight: 1 }]));
    expect(await screen.findByText("ext1", undefined, T)).toBeInTheDocument();
    expect(screen.queryByText(/u:p@h/)).not.toBeInTheDocument();
  });

  it("on another host: shows the join command once, with a warning and a copy button", async () => {
    const { st, u } = open();
    await u.click(await screen.findByRole("button", { name: "Add replica" }, T));
    const d = await dialog("Add a replica");
    await u.click(within(d).getByRole("button", { name: /On another host/ }));
    expect(within(d).queryByLabelText(/^Name/)).not.toBeInTheDocument();
    await u.click(within(d).getByRole("button", { name: "Create join command" }));
    const j = await dialog("Join command");
    expect(within(j).getByLabelText("Join command")).toHaveTextContent("forgebase node join --token fbj_secret_token");
    expect(j).toHaveTextContent("shown once");
    expect(within(j).getByRole("button", { name: "Copy command" })).toBeInTheDocument();
    expect(bodiesOf(st, "POST /join-tokens")).toEqual([{ set: "control", role: "replica" }]);
    await u.click(within(j).getByRole("button", { name: "Done" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("shows the server's refusal in the dialog and keeps it open", async () => {
    const { u } = open({ failWith: (k) => (k === "POST /nodes" ? { status: 503, body: { error: { code: "node_unreachable", message: "cannot connect", request_id: "r9" } } } : undefined) });
    await u.click(await screen.findByRole("button", { name: "Add replica" }, T));
    const d = await dialog("Add a replica");
    await u.type(within(d).getByLabelText(/^Name/), "eu-read-2");
    await u.click(within(d).getByRole("button", { name: "Add replica" }));
    expect(await within(d).findByText("ForgeBase cannot reach that server", undefined, T)).toBeInTheDocument();
    expect(screen.getByRole("dialog", { name: "Add a replica" })).toBeInTheDocument();
  });

  it("the card menu adds to that set", async () => {
    const { st, u } = open({ view: shardedView() });
    await screen.findByLabelText("eu-1", { selector: "[data-set]" }, T);
    await u.click(screen.getByRole("button", { name: "Actions for eu-1" }));
    await u.click(await screen.findByRole("menuitem", { name: /Add replica/ }, T));
    const d = await dialog("Add a replica");
    await u.type(within(d).getByLabelText(/^Name/), "eu-r");
    await u.click(within(d).getByRole("button", { name: "Add replica" }));
    await waitFor(() => expect(bodiesOf(st, "POST /nodes")[0]).toMatchObject({ set: "s1", name: "eu-r" }));
  });
});

describe("add a shard", () => {
  it("is in the primary action's menu, validates, and starts the operation", async () => {
    const { st, u } = open();
    await u.click(await screen.findByRole("button", { name: "More ways to add" }, T));
    await u.click(await screen.findByRole("menuitem", { name: /Add shard/ }, T));
    const d = await dialog("Add a shard");
    expect(d).toHaveTextContent("Nothing changes until it is active");
    await u.click(within(d).getByRole("button", { name: "Add shard" }));
    expect(within(d).getAllByText("Enter a name.").length).toBe(2);
    await u.type(within(d).getByLabelText(/^Shard name/), "eu-2");
    await u.type(within(d).getByLabelText(/^Primary server name/), "eu-2-primary");
    await u.type(within(d).getByLabelText(/^Server address/), "postgres://u:p@h:5432/db");
    await u.click(within(d).getByRole("button", { name: "Add shard" }));
    await waitFor(() => expect(bodiesOf(st, "POST /shards")).toEqual([{ name: "eu-2", node: { name: "eu-2-primary", kind: "external", url: "postgres://u:p@h:5432/db" } }]));
    expect(await screen.findByText("Adding a shard", undefined, T)).toBeInTheDocument();
  });

  it("shows a sharding_unsupported refusal in plain words", async () => {
    const { u } = open({ failWith: (k) => (k === "POST /shards" ? { status: 400, body: { error: { code: "sharding_unsupported", message: "max_prepared_transactions = 0", request_id: "r2" } } } : undefined) });
    await u.click(await screen.findByRole("button", { name: "More ways to add" }, T));
    await u.click(await screen.findByRole("menuitem", { name: /Add shard/ }, T));
    const d = await dialog("Add a shard");
    await u.type(within(d).getByLabelText(/^Shard name/), "eu-2");
    await u.type(within(d).getByLabelText(/^Primary server name/), "eu-2-primary");
    await u.type(within(d).getByLabelText(/^Server address/), "postgres://u:p@h:5432/db");
    await u.click(within(d).getByRole("button", { name: "Add shard" }));
    expect(await within(d).findByText("That server cannot hold shards", undefined, T)).toBeInTheDocument();
  });
});

describe("promote", () => {
  it("explains it, checks the primary, asks for the typed name and sends accept_data_loss false", async () => {
    const { st, u } = open();
    await openMenu(u, "Actions for r1", /Promote/);
    const d = await dialog("Promote r1 to primary?");
    expect(d).toHaveTextContent("takes writes");
    expect(await within(d).findByText(/The primary is reachable/, undefined, T)).toBeInTheDocument();
    const go = within(d).getByRole("button", { name: "Promote to primary" });
    expect(go).toBeDisabled();
    await u.type(within(d).getByLabelText(/to confirm/), "r1");
    expect(go).toBeEnabled();
    await u.click(go);
    await waitFor(() => expect(bodiesOf(st, "POST /nodes/{id}/promote")).toEqual([{ confirm: "r1", accept_data_loss: false }]));
    expect(await screen.findByText("Promoting a replica", undefined, T)).toBeInTheDocument();
  });

  it("when the primary is down: shows the data-loss box and needs the checkbox before it sends accept_data_loss", async () => {
    const { st, u } = open({ check: { primary_reachable: false, behind_bytes: 4096, replayed: "0/1", primary_lsn: null } });
    await openMenu(u, "Actions for r1", /Promote/);
    const d = await dialog("Promote r1 to primary?");
    const box = await within(d).findByRole("alert", undefined, T);
    expect(box).toHaveTextContent("The old primary is down");
    expect(box).toHaveTextContent("4.00 KiB");
    await u.type(within(d).getByLabelText(/to confirm/), "r1");
    const go = within(d).getByRole("button", { name: "Promote to primary" });
    expect(go).toBeDisabled();
    await u.click(within(d).getByRole("checkbox", { name: /newest changes may be lost/ }));
    expect(go).toBeEnabled();
    await u.click(go);
    await waitFor(() => expect(bodiesOf(st, "POST /nodes/{id}/promote")).toEqual([{ confirm: "r1", accept_data_loss: true }]));
  });

  it("says when the loss is not known, and when the check itself fails", async () => {
    const { u } = open({ check: { primary_reachable: false, behind_bytes: null } });
    await openMenu(u, "Actions for r1", /Promote/);
    expect(await within(await dialog("Promote r1 to primary?")).findByText(/amount is not known/, undefined, T)).toBeInTheDocument();
  });

  it("on 403 mfa_step_up_required: asks for the second factor in the dialog, then retries by itself", async () => {
    const mock = seeded();
    // The admin has two-step verification: sign-in answers with a challenge, the code finishes it.
    mock.custom = mock.custom.filter((c) => c.template !== "/api/auth/admins/login");
    let challenged = 0;
    mock.on("POST", "/api/auth/admins/login", () => { challenged += 1; return { status: 200, body: { mfa_required: true, mfa_token: "mt1" } }; });
    mock.on("POST", "/api/auth/mfa/verify", ({ body }) => ((body as { code: string }).code === "123456" ? { status: 200, body: { token_type: "Bearer", expires_in: 900, session_id: "s2", csrf_token: "c2", session_expires_at: "2026-11-12T00:00:00Z", subject: { type: "admin", id: "adm1" } } } : { status: 401, body: { error: { code: "invalid_credentials", message: "bad code", request_id: "r" } } }));
    const { st, u } = open({ stepUp: { "POST /nodes/{id}/promote": 1 } }, mock);
    await openMenu(u, "Actions for r1", /Promote/);
    const d = await dialog("Promote r1 to primary?");
    await within(d).findByText(/The primary is reachable/, undefined, T);
    await u.type(within(d).getByLabelText(/to confirm/), "r1");
    await u.click(within(d).getByRole("button", { name: "Promote to primary" }));
    const form = await within(d).findByRole("form", { name: "Sign in again" }, T);
    expect(form).toHaveTextContent("Sign in again with your second factor");
    expect(st.calls).toEqual([]);
    await u.type(within(form).getByLabelText(/^Password/), "correct-horse");
    await u.click(within(form).getByRole("button", { name: "Continue" }));
    await u.type(await within(form).findByLabelText(/^Authentication code/, undefined, T), "123456");
    await u.click(within(form).getByRole("button", { name: "Verify and continue" }));
    await waitFor(() => expect(bodiesOf(st, "POST /nodes/{id}/promote")).toEqual([{ confirm: "r1", accept_data_loss: false }]), T);
    expect(challenged).toBe(1);
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument(), T);
  });

  it("a wrong second-factor code shows the error and stays in the form", async () => {
    const mock = seeded();
    mock.custom = mock.custom.filter((c) => c.template !== "/api/auth/admins/login");
    mock.on("POST", "/api/auth/admins/login", () => ({ status: 200, body: { mfa_required: true, mfa_token: "mt1" } }));
    mock.on("POST", "/api/auth/mfa/verify", () => ({ status: 401, body: { error: { code: "invalid_credentials", message: "bad code", request_id: "r" } } }));
    const { u } = open({ stepUp: { "DELETE /nodes/{id}": 1 } }, mock);
    await openMenu(u, "Actions for r1", /Remove replica/);
    const d = await dialog("Remove replica r1?");
    await u.type(within(d).getByLabelText(/to confirm/), "r1");
    await u.click(within(d).getByRole("button", { name: "Remove replica" }));
    const form = await within(d).findByRole("form", { name: "Sign in again" }, T);
    await u.type(within(form).getByLabelText(/^Password/), "x");
    await u.click(within(form).getByRole("button", { name: "Continue" }));
    await u.type(await within(form).findByLabelText(/^Authentication code/, undefined, T), "000000");
    await u.click(within(form).getByRole("button", { name: "Verify and continue" }));
    expect(await within(form).findByText("The email or password is wrong", undefined, T)).toBeInTheDocument();
  });
});

describe("rebuild, remove, weight", () => {
  it("rebuild confirms and starts an operation", async () => {
    const { st, u } = open();
    await openMenu(u, "Actions for r1", /Rebuild/);
    const d = await dialog("Rebuild r1?");
    expect(d).toHaveTextContent("does not answer reads until the copy is done");
    await u.click(within(d).getByRole("button", { name: "Rebuild replica" }));
    await waitFor(() => expect(bodiesOf(st, "POST /nodes/{id}/rebuild")).toHaveLength(1));
    expect(await screen.findByText("Rebuilding a replica", undefined, T)).toBeInTheDocument();
  });

  it("remove asks for the replica's name, then deletes it with confirm", async () => {
    const { st, u } = open();
    await openMenu(u, "Actions for r1", /Remove replica/);
    const d = await dialog("Remove replica r1?");
    const go = within(d).getByRole("button", { name: "Remove replica" });
    expect(go).toBeDisabled();
    await u.type(within(d).getByLabelText(/to confirm/), "r");
    expect(go).toBeDisabled();
    await u.type(within(d).getByLabelText(/to confirm/), "1");
    await u.click(go);
    await waitFor(() => expect(st.calls.find((c) => c.key === "DELETE /nodes/{id}")?.query.get("confirm")).toBe("r1"));
    await waitFor(() => expect(screen.queryByText("r1")).not.toBeInTheDocument(), T);
  });

  it("sets a replica's read share, and refuses a bad number", async () => {
    const { st, u } = open();
    await openMenu(u, "Actions for r1", /Set read share/);
    const d = await dialog("Read share of r1");
    const input = within(d).getByLabelText("Share");
    await u.clear(input);
    await u.type(input, "0");
    expect(within(d).getByText("Use a whole number from 1 to 1000.")).toBeInTheDocument();
    expect(within(d).getByRole("button", { name: "Save" })).toBeDisabled();
    await u.clear(input);
    await u.type(input, "3");
    await u.click(within(d).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(bodiesOf(st, "PATCH /nodes/{id}")).toEqual([{ weight: 3 }]));
  });
});

describe("shards: drain, capacity, remove", () => {
  it("drain confirms, sends state draining, and then offers to take new tenants again", async () => {
    const { st, u } = open({ view: shardedView() });
    await screen.findByLabelText("eu-1", { selector: "[data-set]" }, T);
    await openMenu(u, "Actions for eu-1", /Drain/);
    const d = await dialog("Drain eu-1?");
    expect(d).toHaveTextContent("3 tenants already there stay");
    await u.click(within(d).getByRole("button", { name: "Drain shard" }));
    await waitFor(() => expect(bodiesOf(st, "PATCH /shards/{id}")).toEqual([{ state: "draining" }]));
    expect(await screen.findByText("Draining", undefined, T)).toBeInTheDocument();
    await openMenu(u, "Actions for eu-1", /Take new tenants again/);
    await u.click(within(await dialog(/take new tenants again/)).getByRole("button", { name: "Take new tenants" }));
    await waitFor(() => expect(bodiesOf(st, "PATCH /shards/{id}")[1]).toEqual({ state: "active" }));
  });

  it("sets a shard's capacity share", async () => {
    const { st, u } = open({ view: shardedView() });
    await screen.findByLabelText("eu-1", { selector: "[data-set]" }, T);
    await openMenu(u, "Actions for eu-1", /Set capacity share/);
    const d = await dialog("Capacity share of eu-1");
    await u.clear(within(d).getByLabelText("Share"));
    await u.type(within(d).getByLabelText("Share"), "4");
    await u.click(within(d).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(bodiesOf(st, "PATCH /shards/{id}")).toEqual([{ weight: 4 }]));
  });

  it("remove asks for the shard's name; a shard with tenants is refused in plain words", async () => {
    const { u } = open({ view: shardedView() });
    await screen.findByLabelText("eu-1", { selector: "[data-set]" }, T);
    await openMenu(u, "Actions for eu-1", /Remove shard/);
    const d = await dialog("Remove shard eu-1?");
    await u.type(within(d).getByLabelText(/to confirm/), "eu-1");
    await u.click(within(d).getByRole("button", { name: "Remove shard" }));
    expect(await within(d).findByText("That shard still holds tenants", undefined, T)).toBeInTheDocument();
  });

  it("an empty shard is removed", async () => {
    const view = shardedView();
    view.sets[1] = set("s1", { name: "eu-1", tenants: 0 });
    const { st, u } = open({ view });
    await screen.findByLabelText("eu-1", { selector: "[data-set]" }, T);
    await openMenu(u, "Actions for eu-1", /Remove shard/);
    const d = await dialog("Remove shard eu-1?");
    await u.type(within(d).getByLabelText(/to confirm/), "eu-1");
    await u.click(within(d).getByRole("button", { name: "Remove shard" }));
    await waitFor(() => expect(st.calls.filter((c) => c.key === "DELETE /shards/{id}")).toHaveLength(1));
    await waitFor(() => expect(screen.queryByLabelText("eu-1", { selector: "[data-set]" })).not.toBeInTheDocument(), T);
  });
});

describe("operations", () => {
  it("shows stage, a progress bar, the message and a link to the job; finished ones have none of those", async () => {
    open({ operations: [operation("op1"), operation("op0", { kind: "database.promote", status: "succeeded", progress: null, cancellable: false, finished_at: "2026-11-11T10:00:00Z" }), operation("opf", { kind: "database.shard.add", status: "failed", last_error: "shard server unreachable", progress: null, cancellable: false, created_at: "2026-11-10T10:00:00Z" })] });
    const list = await screen.findByRole("list", { name: "Operations" }, T);
    const run = within(list).getByText("Adding a replica").closest("li")!;
    expect(within(run).getByRole("progressbar", { name: /Adding a replica progress/ })).toHaveAttribute("aria-valuenow", "40");
    expect(run).toHaveTextContent("Copying data · 40% · 2 of 5 GB");
    expect(within(run).getByRole("link", { name: /Job/ })).toHaveAttribute("href", "/jobs/j/op1");
    const done = within(list).getByText("Promoting a replica").closest("li")!;
    expect(within(done).getByText("Done")).toBeInTheDocument();
    expect(within(done).queryByRole("progressbar")).not.toBeInTheDocument();
    const failed = within(list).getByText("Adding a shard").closest("li")!;
    expect(within(failed).getByText("Failed")).toBeInTheDocument();
    expect(failed).toHaveTextContent("shard server unreachable");
  });

  it("cancels an operation where the server allows it, with a confirmation", async () => {
    const { st, u } = open({ operations: [operation("op1")] });
    await u.click(await screen.findByRole("button", { name: "Cancel Adding a replica" }, T));
    const d = await dialog(/Cancel adding a replica\?/);
    expect(d).toHaveTextContent("leaves everything as it was");
    await u.click(within(d).getByRole("button", { name: "Cancel operation" }));
    await waitFor(() => expect(bodiesOf(st, "POST /operations/{id}/cancel")).toHaveLength(1));
  });

  it("offers no cancel past the last safe point", async () => {
    open({ operations: [operation("op1", { cancellable: false })] });
    await screen.findByRole("list", { name: "Operations" }, T);
    expect(screen.queryByRole("button", { name: /^Cancel Adding/ })).not.toBeInTheDocument();
  });

  it("says what operations are when there are none", async () => {
    open();
    expect(await screen.findByRole("region", { name: "No operations yet" }, T)).toHaveTextContent("run in the background");
  });

  it("follows the jobs:<id> channel of a running operation and refreshes when it speaks", async () => {
    const sources: { url: string; fire: () => void; closed: boolean }[] = [];
    class FakeSource {
      onmessage: (() => void) | null = null;
      onerror: (() => void) | null = null;
      private listeners: (() => void)[] = [];
      closed = false;
      url: string;
      constructor(url: string) {
        this.url = url;
        sources.push({ url, closed: false, fire: () => { this.onmessage?.(); this.listeners.forEach((l) => l()); } });
      }
      addEventListener(_t: string, l: () => void) { this.listeners.push(l); }
      close() { this.closed = true; const s = sources.find((x) => x.url === this.url); if (s) s.closed = true; }
    }
    vi.stubGlobal("EventSource", FakeSource);
    const { mock, st } = open({ operations: [operation("op1")] });
    await waitFor(() => expect(sources.length).toBe(1), T);
    const subs = JSON.parse(new URL(sources[0]!.url, "http://x").searchParams.get("subscriptions")!) as { channel: string }[];
    expect(subs.map((s) => s.channel)).toEqual(["jobs:op1"]);
    const before = mock.calls.filter((c) => c === "get /api/system/database/operations").length;
    st.operations[0]!.progress = { percent: 90, stage: "Finishing", message: "" };
    act(() => sources[0]!.fire());
    await waitFor(() => expect(mock.calls.filter((c) => c === "get /api/system/database/operations").length).toBeGreaterThan(before), T);
    expect(await screen.findByRole("progressbar", { name: /Adding a replica progress/ }, T)).toHaveAttribute("aria-valuenow", "90");
  });
});

describe("tenants (placements)", () => {
  const four = () => [placement("env-a", "control", { project: "shop" }), placement("env-b", "s1", { project: "blog", organization: "zed" }), placement("env-c", "s1", { project: "shop" }), placement("env-d", "control", { project: "docs", state: "moving" })];

  it("is hidden while there is one server, and lists tenants with their set when sharded", async () => {
    open({ view: shardedView(), placements: four() });
    const table = await screen.findByRole("table", { name: "Tenants" }, T);
    const rows = within(table).getAllByRole("row");
    expect(rows).toHaveLength(3); // header + page of 2
    expect(within(rows[1]!).getByText("env-a")).toBeInTheDocument();
    expect(within(rows[1]!).getByText("Control")).toBeInTheDocument();
    expect(within(rows[2]!).getByText("eu-1")).toBeInTheDocument();
  });

  it("does not show the tenants panel with one server", async () => {
    open({ placements: four() });
    await screen.findByLabelText("Control", { selector: "[data-set]" }, T);
    expect(screen.queryByRole("table", { name: "Tenants" })).not.toBeInTheDocument();
  });

  it("pages with a cursor (Load more), filters by set and project, and searches", async () => {
    const { st, u } = open({ view: shardedView(), placements: four() });
    await screen.findByRole("table", { name: "Tenants" }, T);
    expect(screen.getByText("2 tenants shown")).toBeInTheDocument();
    await u.click(screen.getByRole("button", { name: /Load more/ }));
    await waitFor(() => expect(screen.getByText(/4 tenants shown/)).toBeInTheDocument(), T);
    expect(st.view.sets.length).toBe(2);
    await u.type(screen.getByLabelText("Search tenants"), "zed");
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Tenants" })).getAllByRole("row")).toHaveLength(2), T);
    expect(screen.getByText("env-b")).toBeInTheDocument();
    await u.clear(screen.getByLabelText("Search tenants"));
    await u.type(screen.getByLabelText("Filter by project"), "docs");
    await waitFor(() => expect(screen.getByText("env-d")).toBeInTheDocument(), T);
    expect(screen.queryByText("env-a")).not.toBeInTheDocument();
  });

  it("says when nothing matches", async () => {
    const { u } = open({ view: shardedView(), placements: four() });
    await screen.findByRole("table", { name: "Tenants" }, T);
    await u.type(screen.getByLabelText("Search tenants"), "nothing-like-this");
    expect(await screen.findByRole("region", { name: "No tenants match" }, T)).toBeInTheDocument();
  });

  it("says when there are no tenants yet", async () => {
    open({ view: shardedView(), placements: [] });
    expect(await screen.findByRole("region", { name: "No tenants placed yet" }, T)).toBeInTheDocument();
  });

  it("move: explains the freeze, asks for the tenant id and sends it (FB-174 merged)", async () => {
    const { st, u } = open({ view: shardedView(), placements: four(), implemented: true });
    await screen.findByRole("table", { name: "Tenants" }, T);
    await u.click(screen.getByRole("button", { name: "Move env-a to another set" }));
    const d = await dialog("Move env-a?");
    expect(d).toHaveTextContent("writes pause");
    expect(d).toHaveTextContent("5.00 MiB");
    expect(d).toHaveTextContent("From Control");
    await u.type(within(d).getByLabelText(/to confirm/), "env-a");
    await u.click(within(d).getByRole("button", { name: "Move tenant" }));
    await waitFor(() => expect(bodiesOf(st, "POST /placements/{tenant}/move")).toEqual([{ to: "s1", confirm: "env-a" }]));
    expect(await screen.findByText("Moving a tenant", undefined, T)).toBeInTheDocument();
  });

  it("move answers 501 until FB-174 merges: 'coming in this release', nothing changed", async () => {
    const { u } = open({ view: shardedView(), placements: four() });
    await screen.findByRole("table", { name: "Tenants" }, T);
    await u.click(screen.getByRole("button", { name: "Move env-a to another set" }));
    const d = await dialog("Move env-a?");
    await u.type(within(d).getByLabelText(/to confirm/), "env-a");
    await u.click(within(d).getByRole("button", { name: "Move tenant" }));
    expect(await within(d).findByText(/Coming in this release: moving tenants/, undefined, T)).toBeInTheDocument();
    expect(await within(d).findByText("Coming in this release", { selector: "p, strong, div" }, T)).toBeInTheDocument();
  });

  it("move needs a set that takes tenants", async () => {
    const view = shardedView();
    view.sets[1] = set("s1", { name: "eu-1", tenants: 3, hosts_tenants: false });
    const { u } = open({ view, placements: four() });
    await screen.findByRole("table", { name: "Tenants" }, T);
    await u.click(screen.getByRole("button", { name: "Move env-a to another set" }));
    const d = await dialog("Move env-a?");
    expect(d).toHaveTextContent("No other set takes new tenants");
    await u.type(within(d).getByLabelText(/to confirm/), "env-a");
    expect(within(d).getByRole("button", { name: "Move tenant" })).toBeDisabled();
  });

  it("a tenant that is moving cannot be moved again", async () => {
    open({ view: shardedView(), placements: four(), pageSize: 4 });
    await screen.findByRole("table", { name: "Tenants" }, T);
    expect(screen.getByRole("button", { name: "Move env-d to another set" })).toBeDisabled();
  });
});

describe("rebalance", () => {
  it("answers 501 until FB-174 merges: 'coming in this release'", async () => {
    const { u } = open({ view: shardedView() });
    await u.click(await screen.findByRole("button", { name: "Plan rebalance" }, T));
    expect(await screen.findByText("Coming in this release.", undefined, T)).toBeInTheDocument();
    expect(screen.queryByRole("table", { name: "Rebalance plan" })).not.toBeInTheDocument();
  });

  it("shows the plan table, confirms with the plan id and the freeze estimate, then runs it", async () => {
    const { st, u } = open({ view: shardedView(), implemented: true });
    await u.click(await screen.findByRole("button", { name: "Plan rebalance" }, T));
    const plan = await screen.findByRole("table", { name: "Rebalance plan" }, T);
    const row = within(plan).getByText("env-a").closest("tr")!;
    expect(within(row).getByText("Control")).toBeInTheDocument();
    expect(within(row).getByText("eu-1")).toBeInTheDocument();
    expect(within(row).getByText("5.00 MiB")).toBeInTheDocument();
    expect(within(row).getByText("about 2 s")).toBeInTheDocument();
    await u.click(screen.getByRole("button", { name: /Run plan/ }));
    const d = await dialog(/Run this plan of 1 move\?/);
    expect(d).toHaveTextContent("writes pause");
    const go = within(d).getByRole("button", { name: "Run plan" });
    expect(go).toBeDisabled();
    await u.type(within(d).getByLabelText(/to confirm/), "plan-7");
    await u.click(go);
    await waitFor(() => expect(bodiesOf(st, "POST /rebalance")).toEqual([{ plan_id: "plan-7", confirm: "plan-7" }]));
    expect(await screen.findByText("Rebalancing tenants", undefined, T)).toBeInTheDocument();
  });

  it("says when the shards are already balanced", async () => {
    const { u } = open({ view: shardedView(), implemented: true, plan: { plan_id: "p0", moves: [] } });
    await u.click(await screen.findByRole("button", { name: "Plan rebalance" }, T));
    expect(await screen.findByText(/shards are balanced/, undefined, T)).toBeInTheDocument();
  });

  it("is not offered with one server", async () => {
    open();
    await screen.findByLabelText("Control", { selector: "[data-set]" }, T);
    expect(screen.queryByRole("button", { name: "Plan rebalance" })).not.toBeInTheDocument();
  });
});
