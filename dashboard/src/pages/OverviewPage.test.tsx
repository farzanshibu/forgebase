import { afterEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, within } from "@testing-library/react";
import { ServerHealth } from "./overview/ServerHealth";
import { MockApi, makeCollection } from "../test/mockApi";
import { renderStudio, signedIn } from "../test/helpers";
import { auditEntry, fn, installOverview, key } from "../test/overviewMock";
import userEvent from "@testing-library/user-event";
import { resetServerVersion } from "../hooks";

const info = { name: "forgebase", version: "0.1.0", mode: "embedded", data_dir: "/d", started_at: "x", uptime_secs: 65, database: { path: "/d/db", size_bytes: 2048, journal_mode: "wal", schema_version: 3 } };
const metrics = (n: number, extra = "") => `forgebase_http_requests_total{status="200"} ${n}
forgebase_http_requests_total{status="500"} ${n / 10}
forgebase_http_requests_in_flight 2
forgebase_realtime_connections 4
process_resident_memory_bytes 1048576
process_cpu_seconds_total 3
forgebase_db_size_bytes 4096
forgebase_http_request_duration_seconds_bucket{le="0.01"} ${n}
forgebase_http_request_duration_seconds_bucket{le="+Inf"} ${n}
${extra}`;

const j = (body: unknown, status = 200) => new Response(JSON.stringify(body), { status });
function server(routes: Record<string, () => Response>) {
  vi.stubGlobal("fetch", vi.fn(async (url: string) => {
    const h = routes[url];
    return h ? h() : j({ error: { code: "not_found", message: "no", request_id: "r" } }, 404);
  }));
}
const full = (text: () => string): Record<string, () => Response> => ({
  "/metrics": () => new Response(text(), { status: 200 }),
  "/api/system/info": () => j(info),
  "/api/v1/storage/stats": () => j({ buckets: 2, objects: 5, versions: 5, logical_bytes: 1024, stored_bytes: 900, open_uploads: 1 }),
  "/api/v1/entitlements": () => j({ features: { "storage.bytes": { enabled: true, limit: 2048 } } }),
  "/api/v1/vector/stats": () => j({ collections: 1, indexes: 2, building: 0, indexed_vectors: 77, memory_bytes: 4096 }),
  "/api/v1/jobs/stats": () => j({ totals: { queued: 3, running: 1, dead: 2, succeeded: 9 }, queues: [] }),
  "/api/v1/kv/_info": () => j({ keys: 12, used_memory_bytes: 2048, max_memory_bytes: 0, eviction_policy: "lru", hits: 5, misses: 1, expired_keys: 0, evicted_keys: 0, channels: 0, unsaved_keys: 0, persistent: true }),
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  resetServerVersion();
});

describe("Server health", () => {
  it("shows version, database, connections, latency, storage vs quota, vector, jobs and kv", async () => {
    server(full(() => metrics(100)));
    render(<ServerHealth />);
    const db = await screen.findByRole("region", { name: "Database" });
    expect(within(db).getByText("4.00 KiB")).toBeInTheDocument();
    expect(within(db).getByText(/wal · schema 3/)).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Server" })).toHaveTextContent(/ForgeBase 0\.1\.0.*embedded · up 1m/);
    const conn = screen.getByRole("region", { name: "Connections" });
    expect(within(conn).getByText("2")).toBeInTheDocument();
    expect(within(conn).getByText("4")).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Request latency" })).getAllByText(/ms/).length).toBe(2);
    expect(screen.getByLabelText("Storage quota usage")).toHaveTextContent("1.00 KiB of 2.00 KiB quota");
    expect(screen.getByRole("progressbar", { name: "Storage quota" })).toHaveAttribute("aria-valuenow", "50");
    expect(within(screen.getByRole("region", { name: "Vector indexes" })).getByText("77")).toBeInTheDocument();
    const jobs = screen.getByRole("region", { name: "Jobs" });
    for (const v of ["3", "1", "2", "9"]) expect(within(jobs).getByText(v)).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Key-value store" })).getByText("12")).toBeInTheDocument();
    expect(screen.queryByText(/does not report/)).not.toBeInTheDocument();
  });

  it("collapses what the server does not report into one muted line, and never says 'not reported' beside 'collecting…'", async () => {
    server({ "/metrics": () => new Response("forgebase_uptime_seconds 5\n", { status: 200 }) });
    render(<ServerHealth />);
    expect(await screen.findByRole("region", { name: "Database" })).toBeInTheDocument();
    for (const name of ["Requests per second", "Error rate", "Request latency", "Connections", "Memory and CPU", "Storage usage", "Vector indexes", "Jobs", "Key-value store"]) {
      expect(screen.queryByRole("region", { name })).not.toBeInTheDocument();
    }
    expect(screen.getByText(/^This server does not report requests per second, the error rate, request latency, connections, memory and CPU, storage usage, vector indexes, jobs and the key-value store\./)).toBeInTheDocument();
    expect(screen.queryByText(/not reported/)).not.toBeInTheDocument();
    expect(screen.queryByText("collecting…")).not.toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Server" })).toHaveTextContent("This server does not report its version.");
  });

  it("drops only the part of a card the server does not report", async () => {
    server(full(() => metrics(100).replace(/forgebase_realtime_connections 4\n/, "").replace(/process_cpu_seconds_total 3\n/, "")));
    render(<ServerHealth />);
    const conn = await screen.findByRole("region", { name: "Connections" });
    expect(within(conn).getByText("Active requests")).toBeInTheDocument();
    expect(within(conn).queryByText("Realtime")).not.toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Memory and CPU" })).queryByText("CPU")).not.toBeInTheDocument();
    expect(screen.getByText(/does not report realtime connections and CPU\./)).toBeInTheDocument();
  });

  it("polls, keeps history and derives requests per second from two scrapes", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let n = 100;
    server(full(() => metrics(n)));
    render(<ServerHealth />);
    const rps = await screen.findByRole("region", { name: "Requests per second" });
    // One reading so far: the rate needs a second one.
    expect(within(rps).getByText("—")).toBeInTheDocument();
    expect(within(rps).getByText("collecting…")).toBeInTheDocument();
    n = 600;
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    // 500 + 50 extra requests in ~5 s of fake time
    await vi.waitFor(() => expect(within(rps).getByText(/^\d+\.\d\d$/)).toBeInTheDocument());
    expect(within(rps).getByText("660 total")).toBeInTheDocument();
  });

  it("shows the /metrics failure in plain words with the request id, and keeps the other cards", async () => {
    server({ ...full(() => ""), "/metrics": () => j({ error: { code: "internal", message: "boom", request_id: "req-9" } }, 500) });
    render(<ServerHealth />);
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Something went wrong on the server");
    expect(within(alert).getByRole("button", { name: "Copy request id" })).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Jobs" })).getByText("9")).toBeInTheDocument();
  });
});

describe("OverviewPage", () => {
  it("opens with Getting started, Connect your app, Server health and Recent activity read from the server", async () => {
    const mock = signedIn(new MockApi({ collections: [{ ...makeCollection("users", []), type: "auth" }, makeCollection("notes", [{ name: "title", type: "text" }])] }).install());
    installOverview(mock, { keys: [key()], buckets: 1, objects: 0, audit: [auditEntry(2, "admin.created", { actor: "cli" }), auditEntry(1, "signing_key.rotated", { actor: "system" })] });
    renderStudio("/");
    expect(await screen.findByRole("heading", { name: "Overview", level: 1 })).toBeInTheDocument();

    const gs = await screen.findByRole("region", { name: "Getting started" });
    expect(await within(gs).findByText(/3 of 5 done/)).toBeInTheDocument();
    expect(within(gs).getByRole("link", { name: "Open notes" })).toHaveAttribute("href", "/database/c/notes");
    expect(within(gs).getByRole("link", { name: "Upload a file" })).toHaveAttribute("href", "/storage");

    const connect = screen.getByRole("region", { name: "Connect your app" });
    expect(within(connect).getByLabelText("JavaScript snippet")).toHaveTextContent('fb.collection("notes")');

    expect(screen.getByRole("heading", { name: "Server health", level: 2 })).toBeInTheDocument();
    const activity = screen.getByRole("region", { name: "Recent activity" });
    expect(await within(activity).findByText("Administrator created")).toBeInTheDocument();
    expect(within(activity).getByText("by command line")).toBeInTheDocument();
    expect(within(activity).getByRole("link", { name: "Open the audit log" })).toHaveAttribute("href", "/auth/audit");
    expect(mock.problems).toEqual([]);
  });

  it("hides the upload and function steps when their features are switched off", async () => {
    const mock = signedIn(new MockApi().install());
    installOverview(mock);
    const feature = (id: string, path: string, enabled: boolean) => ({ id, name: id, summary: "", always_on: false, can_disable: true, enabled, studio_path: path, docs: null, settings: [] });
    mock.on("GET", "/api/system/features", () => ({ status: 200, body: { config_file: "forgebase.toml", restart_pending: false, features: [feature("storage", "/storage", false), feature("functions", "/functions", false)] } }));
    renderStudio("/");
    const gs = await screen.findByRole("region", { name: "Getting started" });
    expect(await within(gs).findByText(/1 of 3 done/)).toBeInTheDocument();
    expect(within(gs).queryByText(/Upload a file/)).not.toBeInTheDocument();
    expect(within(gs).queryByText(/Deploy a function/)).not.toBeInTheDocument();
  });

  it("folds a finished checklist into one line, and Show again brings the steps back", async () => {
    const mock = signedIn(new MockApi({ collections: [makeCollection("notes", [])] }).install());
    installOverview(mock, { keys: [key()], objects: 2, functions: [fn("hello")] });
    renderStudio("/");
    const gs = await screen.findByRole("region", { name: "Getting started" });
    expect(await within(gs).findByText("All 5 steps are done.")).toBeInTheDocument();
    await userEvent.setup().click(within(gs).getByRole("button", { name: "Show again" }));
    expect(within(screen.getByRole("region", { name: "Getting started" })).getAllByRole("listitem")).toHaveLength(5);
    expect(mock.problems).toEqual([]);
  });
});
