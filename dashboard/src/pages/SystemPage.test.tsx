import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import SystemPage from "./SystemPage";

const info = {
  name: "forgebase",
  version: "0.1.0",
  mode: "embedded",
  data_dir: "/data",
  started_at: "2026-11-07T10:00:00Z",
  uptime_secs: 3725,
  database: { path: "/data/forgebase.db", size_bytes: 2048, journal_mode: "wal", schema_version: 3 },
};

function route(handlers: Record<string, () => Response>) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string) => {
      const h = handlers[url];
      if (!h) throw new Error(`unmocked ${url}`);
      return h();
    }),
  );
}
const json = (status: number, body: unknown) => () => new Response(JSON.stringify(body), { status });

afterEach(() => vi.unstubAllGlobals());

describe("SystemPage", () => {
  it("shows status, version, uptime, db size, journal mode, schema version", async () => {
    route({
      "/health": json(200, { status: "ok" }),
      "/health/ready": json(200, { status: "ready", checks: { database: "ok", storage: "ok" } }),
      "/api/system/info": json(200, info),
    });
    render(<SystemPage />);
    expect(await screen.findByText("0.1.0")).toBeInTheDocument();
    expect(screen.getAllByText("ok")).toHaveLength(3);
    expect(screen.getByText("ready")).toBeInTheDocument();
    expect(screen.getByText("1h 2m 5s")).toBeInTheDocument();
    expect(screen.getByText("2.00 KiB")).toBeInTheDocument();
    expect(screen.getByText("wal")).toBeInTheDocument();
    expect(screen.getByText("3")).toBeInTheDocument();
  });

  it("shows a clear error state when the server is down", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => { throw new TypeError("Failed to fetch"); }));
    render(<SystemPage />);
    expect(await screen.findByText("Server unreachable")).toBeInTheDocument();
    expect(screen.queryByText("Database")).not.toBeInTheDocument();
  });

  it("shows the failing readiness check on 503", async () => {
    route({
      "/health": json(200, { status: "ok" }),
      "/health/ready": json(503, { status: "not_ready", checks: { database: "ok", storage: "failed" } }),
      "/api/system/info": json(200, info),
    });
    render(<SystemPage />);
    expect(await screen.findByText("failed")).toBeInTheDocument();
    expect(screen.getByText("not_ready")).toBeInTheDocument();
  });

  it("surfaces an API error from one endpoint without hiding the rest", async () => {
    route({
      "/health": json(200, { status: "ok" }),
      "/health/ready": json(200, { status: "ready", checks: {} }),
      "/api/system/info": json(500, { error: { code: "internal", message: "db locked", request_id: "abc" } }),
    });
    render(<SystemPage />);
    expect(await screen.findByText(/internal: db locked \(request abc\)/)).toBeInTheDocument();
    expect(screen.getByText("ready")).toBeInTheDocument();
  });
});
