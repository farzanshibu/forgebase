import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError, getHealth, getReady, getSystemInfo, request } from ".";

function mockFetch(status: number, body: unknown, raw = false) {
  const fn = vi.fn(async () => new Response(raw ? (body as string) : JSON.stringify(body), { status }));
  vi.stubGlobal("fetch", fn);
  return fn;
}

afterEach(() => vi.unstubAllGlobals());

describe("api client", () => {
  it("returns parsed JSON on success", async () => {
    const f = mockFetch(200, { status: "ok" });
    expect(await getHealth()).toEqual({ status: "ok" });
    expect(f).toHaveBeenCalledWith("/health", expect.objectContaining({ method: "GET" }));
  });

  it("maps the PLAN section 5 error model to ApiError", async () => {
    mockFetch(404, { error: { code: "not_found", message: "no such thing", request_id: "r-1" } });
    const err = await request("/api/x").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect(err).toMatchObject({ status: 404, code: "not_found", message: "no such thing", requestId: "r-1" });
  });

  it("falls back to http_error for non-model error bodies", async () => {
    mockFetch(500, "boom", true);
    await expect(request("/x")).rejects.toMatchObject({ status: 500, code: "http_error" });
  });

  it("reports network failure as status 0", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => { throw new TypeError("Failed to fetch"); }));
    const err = await getSystemInfo().catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).isNetwork).toBe(true);
    expect((err as ApiError).code).toBe("network_error");
  });

  it("rejects invalid JSON on a 200", async () => {
    mockFetch(200, "<html>", true);
    await expect(request("/x")).rejects.toMatchObject({ code: "invalid_response" });
  });

  it("sends JSON bodies with content-type", async () => {
    const f = mockFetch(200, {});
    await request("/x", { method: "POST", body: { a: 1 } });
    const init = (f.mock.calls[0] as unknown as [string, RequestInit])[1];
    expect(init.body).toBe('{"a":1}');
    expect((init.headers as Record<string, string>)["Content-Type"]).toBe("application/json");
  });

  it("treats a 503 readiness body with checks as data", async () => {
    mockFetch(503, { status: "not_ready", checks: { database: "ok", storage: "failed" } });
    expect(await getReady()).toEqual({ status: "not_ready", checks: { database: "ok", storage: "failed" } });
  });

  it("throws on a 503 that uses the error model", async () => {
    mockFetch(503, { error: { code: "unavailable", message: "down", request_id: "r" } });
    await expect(getReady()).rejects.toMatchObject({ status: 503, code: "unavailable" });
  });
});
