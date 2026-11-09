import { afterEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, within } from "@testing-library/react";
import OverviewPage from "./OverviewPage";

const info = { name: "forgebase", version: "0.1.0", mode: "embedded", data_dir: "/d", started_at: "x", uptime_secs: 65, database: { path: "/d/db", size_bytes: 2048, journal_mode: "wal", schema_version: 3 } };
const metrics = (n: number, extra = "") => `forgebase_http_requests_total{status="200"} ${n}
forgebase_http_requests_total{status="500"} ${n / 10}
forgebase_http_requests_in_flight 2
forgebase_realtime_connections 4
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
});

describe("OverviewPage", () => {
  it("shows database, connections, latency, storage vs quota, vector, jobs and kv cards", async () => {
    server(full(() => metrics(100)));
    render(<OverviewPage />);
    const db = await screen.findByRole("region", { name: "Database" });
    expect(within(db).getByText("4.00 KiB")).toBeInTheDocument();
    expect(within(db).getByText(/wal · schema 3/)).toBeInTheDocument();
    const conn = screen.getByRole("region", { name: "Connections" });
    expect(within(conn).getByText("2")).toBeInTheDocument();
    expect(within(conn).getByText("4")).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Query latency" })).getAllByText(/ms/).length).toBe(2);
    expect(screen.getByLabelText("Storage quota usage")).toHaveTextContent("1.00 KiB of 2.00 KiB quota");
    expect(screen.getByRole("progressbar", { name: "Storage quota" })).toHaveAttribute("aria-valuenow", "50");
    expect(within(screen.getByRole("region", { name: "Vector indexes" })).getByText("77")).toBeInTheDocument();
    const jobs = screen.getByRole("region", { name: "Jobs" });
    for (const v of ["3", "1", "2", "9"]) expect(within(jobs).getByText(v)).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Key-value store" })).getByText("12")).toBeInTheDocument();
  });

  it("says 'not reported by this server' for absent metrics and endpoints", async () => {
    server({ "/metrics": () => new Response("forgebase_uptime_seconds 5\n", { status: 200 }) });
    render(<OverviewPage />);
    const cpu = await screen.findByRole("region", { name: "Memory and CPU" });
    expect(within(cpu).getAllByText("not reported by this server")).toHaveLength(2);
    expect(within(screen.getByRole("region", { name: "Jobs" })).getByText("not reported by this server")).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Vector indexes" })).getByText("not reported by this server")).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Connections" })).getAllByText("not reported by this server")).toHaveLength(2);
  });

  it("polls, keeps history and derives requests per second from two scrapes", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let n = 100;
    server(full(() => metrics(n)));
    render(<OverviewPage />);
    await screen.findByRole("region", { name: "Database" });
    n = 600;
    await act(async () => { await vi.advanceTimersByTimeAsync(5000); });
    const rps = screen.getByRole("region", { name: "Requests per second" });
    // 500 + 50 extra requests in ~5 s of fake time
    await vi.waitFor(() => expect(within(rps).getByText(/^\d+\.\d\d$/)).toBeInTheDocument());
    expect(within(rps).getByText("660 total")).toBeInTheDocument();
  });

  it("shows an error when /metrics fails but keeps the other cards", async () => {
    server({ ...full(() => ""), "/metrics": () => j({ error: { code: "internal", message: "boom", request_id: "r" } }, 500) });
    render(<OverviewPage />);
    expect(await screen.findByText(/Could not read \/metrics: internal: boom/)).toBeInTheDocument();
    expect(within(screen.getByRole("region", { name: "Jobs" })).getByText("9")).toBeInTheDocument();
  });
});
