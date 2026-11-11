import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import SystemPage from "./SystemPage";

const renderPage = () => render(<MemoryRouter><SystemPage /></MemoryRouter>);

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
    renderPage();
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
    renderPage();
    expect(await screen.findByText("Server unreachable")).toBeInTheDocument();
    expect(screen.queryByText("Database")).not.toBeInTheDocument();
  });

  it("shows the failing readiness check on 503", async () => {
    route({
      "/health": json(200, { status: "ok" }),
      "/health/ready": json(503, { status: "not_ready", checks: { database: "ok", storage: "failed" } }),
      "/api/system/info": json(200, info),
    });
    renderPage();
    expect(await screen.findByText("failed")).toBeInTheDocument();
    expect(screen.getByText("not_ready")).toBeInTheDocument();
  });

  it("surfaces an API error from one endpoint without hiding the rest", async () => {
    route({
      "/health": json(200, { status: "ok" }),
      "/health/ready": json(200, { status: "ready", checks: {} }),
      "/api/system/info": json(500, { error: { code: "internal", message: "db locked", request_id: "abc" } }),
    });
    renderPage();
    expect(await screen.findByText(/internal: db locked \(request abc\)/)).toBeInTheDocument();
    expect(screen.getByText("ready")).toBeInTheDocument();
  });

  it("links to Backups, the Audit log and Features and settings, with no phase copy", async () => {
    route({
      "/health": json(200, { status: "ok" }),
      "/health/ready": json(200, { status: "ready", checks: {} }),
      "/api/system/info": json(200, info),
    });
    renderPage();
    const more = (await screen.findByText("More about this server")).closest("section")!;
    expect(within(more).getByRole("link", { name: "Backups" })).toHaveAttribute("href", "/system/backups");
    expect(within(more).getByRole("link", { name: "Audit log" })).toHaveAttribute("href", "/auth/audit");
    expect(within(more).getByRole("link", { name: "Features and settings" })).toHaveAttribute("href", "/system/features");
    expect(document.body).not.toHaveTextContent(/phase 9|coming later/i);
  });

  it("shows the PostgreSQL backend and each extension feature as active, portable or off (FB-148, FB-161)", async () => {
    route({
      "/health": json(200, { status: "ok" }),
      "/health/ready": json(200, { status: "ready", checks: {} }),
      "/api/system/info": json(200, {
        ...info,
        database: {
          schema_version: 33, backend: "postgres", mode: "managed", server_version: "18.3", connections: 4,
          features: {
            vector: { engine: "pgvector 0.8.7", state: "active", mode: "auto", extension: "vector" },
            queue: { engine: "portable", state: "portable", mode: "auto", reason: "pgmq is not installed" },
            graph: { engine: "portable", state: "off", mode: "off" },
          },
        },
      }),
    });
    renderPage();
    const db = (await screen.findByText("Database")).closest("section")!;
    expect(within(db).getByText("PostgreSQL")).toBeInTheDocument();
    expect(within(db).getByText("managed")).toBeInTheDocument();
    expect(within(db).getByText("18.3")).toBeInTheDocument();
    const list = await screen.findByRole("list", { name: "Extension features" });
    const state = (id: string) => within(list.querySelector<HTMLElement>(`[data-feature="${id}"]`)!);
    expect(state("vector").getByText("active")).toBeInTheDocument();
    expect(list.querySelector('[data-feature="vector"]')).toHaveTextContent("pgvector 0.8.7");
    expect(state("queue").getByText("portable")).toBeInTheDocument();
    expect(list.querySelector('[data-feature="queue"]')).toHaveTextContent("pgmq is not installed");
    expect(state("graph").getByText("off")).toBeInTheDocument();
  });
});
