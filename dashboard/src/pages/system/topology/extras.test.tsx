/** FB-176 extras: replica and sharding settings, the SQL editor's "Run on" selector, and the "View topology" link. */
import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../../test/mockApi";
import { renderStudio, seeded, signedIn } from "../../../test/helpers";
import { installPg } from "../../../test/pgMock";
import { installSetup, sampleFeatures } from "../../../test/setupMock";
import { installTopology, shardedView } from "../../../test/topologyMock";
import type { FeatureSetting } from "../../../api";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

const T = { timeout: 15_000 };

const s = (key: string, o: Partial<FeatureSetting> & Pick<FeatureSetting, "label" | "type" | "value">): FeatureSetting => ({
  key, help: `Help for ${o.label}.`, unit: null, options: null, default: o.value, source: "default", editable: true, restart_required: false, secret: false, ...o,
});

describe("replica and sharding settings in Features and settings", () => {
  function setup() {
    const f = sampleFeatures();
    f.find((x) => x.id === "database")!.settings = [
      s("database.replicas.read_routing", { label: "Reads from replicas", type: "enum", options: ["primary", "replicas"], value: "replicas", help: "Where reads that may be slightly behind go." }),
      s("database.replicas.max_lag_ms", { label: "Largest replica lag", type: "int", unit: "ms", value: 5000, help: "A replica further behind than this stops answering reads." }),
      s("database.sharding.max_parallel_moves", { label: "Tenant moves at once", type: "int", value: 2, help: "How many tenant moves may run at the same time." }),
      s("database.nodes", { label: "Servers declared in the file", type: "list", value: [{ name: "eu-read-1", set: "control", role: "replica", weight: 1 }] as never, editable: false, source: "file", help: "Edit them in the file." }),
    ];
    const mock = new MockApi().install();
    const st = installSetup(mock, { features: f });
    signedIn(mock);
    return { mock, st };
  }

  it("groups them as Read replicas and Sharding with their plain help, editable, and links to the topology screen", async () => {
    setup();
    renderStudio("/system/features");
    const cfg = await screen.findByRole("region", { name: "Settings" }, T);
    expect(within(cfg).getByRole("heading", { name: "Read replicas" })).toBeInTheDocument();
    expect(within(cfg).getByRole("heading", { name: "Sharding" })).toBeInTheDocument();
    expect(within(cfg).getByText("A replica further behind than this stops answering reads.")).toBeInTheDocument();
    expect(within(cfg).getByLabelText("Largest replica lag")).toBeEnabled();
    expect(within(cfg).getByLabelText("Tenant moves at once")).toBeEnabled();
    expect(within(cfg).getAllByRole("link", { name: /Database topology/ })[0]).toHaveAttribute("href", "/system/topology");
  });

  it("saves a changed replica setting with one PATCH", async () => {
    const { st } = setup();
    renderStudio("/system/features");
    const cfg = await screen.findByRole("region", { name: "Settings" }, T);
    const lag = within(cfg).getByLabelText("Largest replica lag");
    await userEvent.clear(lag);
    await userEvent.type(lag, "8000");
    await userEvent.click(within(screen.getByRole("region", { name: "Save changes" })).getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(st.patches).toEqual([{ set: { "database.replicas.max_lag_ms": 8000 } }]), T);
  });

  it("shows database.nodes read-only, one row per declared server", async () => {
    setup();
    renderStudio("/system/features");
    const cfg = await screen.findByRole("region", { name: "Settings" }, T);
    const nodes = within(cfg).getByLabelText("Servers declared in the file");
    expect(nodes).toHaveTextContent("name = eu-read-1");
    expect(nodes).toHaveTextContent("set = control");
    expect(cfg.querySelector("[data-setting='database.nodes']")).toHaveTextContent(/Read-only: This setting is fixed while the server runs/);
  });
});

describe("SQL editor: Run on", () => {
  function setup(view = shardedView(), backend: "postgres" | "sqlite" = "postgres") {
    const mock = seeded();
    const pg = installPg(mock, { backend });
    installTopology(mock, { view });
    return { mock, pg };
  }

  it("is shown only when the server is sharded, and sends the top-level set", async () => {
    const { pg } = setup();
    renderStudio("/database/sql");
    const u = userEvent.setup();
    const pick = await screen.findByRole("combobox", { name: "Run on" }, T);
    await u.click(await screen.findByRole("button", { name: "Run" }));
    await screen.findByRole("table", { name: "Result" });
    expect(pg.sql.at(-1)).toEqual({ sql: "SELECT 1 AS one" });
    await u.click(pick);
    await u.click(await screen.findByRole("option", { name: "eu-1" }, T));
    await u.click(screen.getByRole("button", { name: "Run" }));
    await waitFor(() => expect(pg.sql).toHaveLength(2));
    expect(pg.sql.at(-1)).toEqual({ sql: "SELECT 1 AS one", set: "s1" });
  });

  it("is hidden with one server, and on SQLite", async () => {
    const { mock } = setup(undefined, "postgres");
    mock.custom = mock.custom.filter((c) => !c.template.endsWith("/topology"));
    mock.on("GET", "/api/system/database/topology", () => ({ status: 200, body: { backend: "postgres", epoch: 1, sets: [{ id: "control", name: "Control", kind: "control", number: 0, state: "active", weight: 1, hosts_tenants: true, tenants: 0, est_bytes: 0 }], nodes: [], operations: [] } }));
    renderStudio("/database/sql");
    await screen.findByRole("checkbox", { name: "Read from replica" }, T);
    await new Promise((r) => setTimeout(r, 50));
    expect(screen.queryByRole("combobox", { name: "Run on" })).not.toBeInTheDocument();
  });

  it("is hidden on SQLite and asks nothing of the topology API", async () => {
    const { mock } = setup(undefined, "sqlite");
    renderStudio("/database/sql");
    await screen.findByRole("button", { name: "Run" }, T);
    expect(screen.queryByRole("combobox", { name: "Run on" })).not.toBeInTheDocument();
    expect(mock.calls).not.toContain("get /api/system/database/topology");
  });
});

describe("the Database studio's View topology link", () => {
  it("leads to the topology tab of System", async () => {
    const mock = seeded();
    installPg(mock);
    installTopology(mock);
    renderStudio("/database");
    const link = await screen.findByRole("link", { name: "View topology" }, T);
    await userEvent.click(link);
    expect(await screen.findByRole("heading", { level: 1, name: /Database topology/ }, T)).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Database topology", selected: true })).toBeInTheDocument();
  });
});
