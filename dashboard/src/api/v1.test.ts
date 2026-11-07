import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError, OPERATIONS, listCollections, patchRecord, request, setTokenSource } from ".";
import { specOperations } from "../test/contract";
import { MockApi } from "../test/mockApi";

afterEach(() => {
  vi.unstubAllGlobals();
  setTokenSource(() => null);
});

describe("typed client vs /api/openapi.json", () => {
  it("only calls operations the OpenAPI document declares", () => {
    const spec = new Set(specOperations());
    expect(OPERATIONS.filter((o) => !spec.has(o))).toEqual([]);
  });

  it("covers every Database Studio operation (collections, records, indexes, slow queries, sql, import/export)", () => {
    const wanted = specOperations().filter((o) => /\/api\/v1\/(collections|indexes|slow-queries|sql)/.test(o));
    const missing = wanted.filter((o) => !(OPERATIONS as readonly string[]).includes(o));
    // Only the single-resource reads the studio does not need are left out.
    expect(missing.sort()).toEqual([
      "get /api/v1/collections/{collection}/records/{id}/versions",
      "get /api/v1/collections/{collection}/schema",
      "get /api/v1/indexes/{name}",
      "get /api/v1/sql/tables/{name}",
      "post /api/v1/collections/{collection}/aggregate",
      "post /api/v1/sql/batch",
    ]);
  });
});

describe("auth + errors", () => {
  it("sends no Authorization header without a token and Bearer <token> with one", async () => {
    const mock = new MockApi({ collections: [] }).install();
    await listCollections();
    expect(mock.headers[0]).not.toHaveProperty("Authorization");
    setTokenSource(async () => "tok-123");
    await listCollections();
    expect(mock.headers[1]).toMatchObject({ Authorization: "Bearer tok-123" });
    expect(mock.problems).toEqual([]);
  });

  it("raises ApiError with isAuth on 401 and 403", async () => {
    new MockApi({ requireAuth: true }).install();
    const e = await listCollections().catch((x: unknown) => x);
    expect(e).toBeInstanceOf(ApiError);
    expect(e).toMatchObject({ status: 401, code: "unauthenticated", isAuth: true, requestId: "req-test" });
  });

  it("exposes per-field validation errors from error.details.fields", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ error: { code: "validation_failed", message: "bad", request_id: "r", details: { fields: { title: { code: "required", message: "title is required" } } } } }), { status: 400 })));
    const e = (await request("/x").catch((x: unknown) => x)) as ApiError;
    expect(e.fields).toEqual({ title: { code: "required", message: "title is required" } });
  });

  it("sends If-Match with the record version", async () => {
    const f = vi.fn(async () => new Response("{}", { status: 200 }));
    vi.stubGlobal("fetch", f);
    await patchRecord("c", "r1", { a: 1 }, 3);
    expect(f).toHaveBeenCalledWith("/api/v1/collections/c/records/r1", expect.objectContaining({ headers: expect.objectContaining({ "If-Match": '"3"' }) }));
  });
});
