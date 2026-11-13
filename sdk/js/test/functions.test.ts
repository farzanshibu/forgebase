import { describe, expect, it } from "vitest";
import { ForgeBase, InvalidResponseError, NotFoundError, PermissionError, ValidationError } from "../src/index";
import { FunctionsApi } from "../src/functions";
import { errBody, json, scripted } from "./helpers";

const info = { id: "f1", name: "hello", description: "", access: "public", active_version: 2, latest_version: 2, principal: "service:function.hello", created_at: "t", updated_at: "t", created_by: "u" };
const version = { version: 2, sha256: "ab", size: 10, active: true, created_at: "t", created_by: "u", capabilities: [], limits: { fuel: 1, memory_bytes: 2, timeout_ms: 3, max_concurrency: 4 }, runtime: "js", language: "typescript" };

const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  const fb = new ForgeBase("http://x", { fetch: f.fetch, retry: { maxRetries: 0 } });
  return { f, fn: new FunctionsApi(fb.transport) };
};
const text = (status: number, body: string, type = "text/plain") => new Response(body, { status, headers: { "content-type": type } });

describe("functions: management", () => {
  it("list unwraps items; get encodes the name", async () => {
    const { f, fn } = setup(json(200, { items: [info] }), json(200, info));
    expect(await fn.list()).toEqual([info]);
    await fn.get("a b/c");
    expect(f.calls[0]).toMatchObject({ method: "GET", url: "http://x/api/v1/functions" });
    expect(f.calls[1]!.url).toBe("http://x/api/v1/functions/a%20b%2Fc");
  });

  it("deploy maps camelCase options to the wire and leaves unset fields out", async () => {
    const { f, fn } = setup(json(201, { function: info, version }), json(201, { function: info, version }));
    const out = await fn.deploy({
      name: "hello",
      source: "export default 1",
      language: "javascript",
      access: "authenticated",
      activate: false,
      capabilities: ["kv", "fetch"],
      fetchHosts: ["*.example.com"],
      env: { A: "1" },
      hooks: [{ collection: "posts", events: ["after_create"] }],
      limits: { timeoutMs: 500, maxConcurrency: 2 },
    });
    expect(out.version.version).toBe(2);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/functions" });
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({
      name: "hello",
      source: "export default 1",
      language: "javascript",
      access: "authenticated",
      activate: false,
      capabilities: ["kv", "fetch"],
      fetch_hosts: ["*.example.com"],
      env: { A: "1" },
      hooks: [{ collection: "posts", events: ["after_create"] }],
      limits: { timeout_ms: 500, max_concurrency: 2 },
    });
    await fn.deploy({ name: "bare", source: "x" });
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ name: "bare", source: "x" });
  });

  it("deploy base64-encodes wasm bytes and passes a base64 string through", async () => {
    const { f, fn } = setup(json(201, { function: info, version }));
    await fn.deploy({ name: "w", wasm: new Uint8Array([0, 0x61, 0x73, 0x6d]) });
    await fn.deploy({ name: "w", wasm: new Uint8Array([1, 2, 3]).buffer });
    await fn.deploy({ name: "w", wasm: "AGFzbQ==" });
    expect(JSON.parse(f.calls[0]!.body!).wasm).toBe("AGFzbQ==");
    expect(JSON.parse(f.calls[1]!.body!).wasm).toBe("AQID");
    expect(JSON.parse(f.calls[2]!.body!).wasm).toBe("AGFzbQ==");
  });

  it("a source that does not compile is a ValidationError with the diagnostics", async () => {
    const details = { line: 3, column: 7, message: "unexpected token", errors: [{ line: 3, column: 7, message: "unexpected token" }] };
    const { fn } = setup(json(400, errBody("function_compile_failed", "does not compile", details)));
    const err = await fn.deploy({ name: "bad", source: "(" }).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ValidationError);
    expect((err as ValidationError).code).toBe("function_compile_failed");
    expect((err as ValidationError).details).toMatchObject({ line: 3, errors: [{ message: "unexpected token" }] });
  });

  it("a non-admin asking for service_role is a PermissionError", async () => {
    const { fn } = setup(json(403, errBody("service_role_requires_admin")));
    await expect(fn.deploy({ name: "x", source: "x", capabilities: ["service_role"] })).rejects.toBeInstanceOf(PermissionError);
  });

  it("delete, versions, versionSource, rollback", async () => {
    const { f, fn } = setup(
      json(204, null),
      json(200, { items: [version] }),
      json(200, { language: "typescript", source: "src" }),
      json(200, info),
      json(200, info),
    );
    await fn.delete("hello");
    expect(await fn.versions("hello")).toEqual([version]);
    expect(await fn.versionSource("hello", 2)).toEqual({ language: "typescript", source: "src" });
    await fn.rollback("hello");
    await fn.rollback("hello", 1);
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "DELETE http://x/api/v1/functions/hello",
      "GET http://x/api/v1/functions/hello/versions",
      "GET http://x/api/v1/functions/hello/versions/2/source",
      "POST http://x/api/v1/functions/hello/rollback",
      "POST http://x/api/v1/functions/hello/rollback",
    ]);
    expect(JSON.parse(f.calls[3]!.body!)).toEqual({});
    expect(JSON.parse(f.calls[4]!.body!)).toEqual({ version: 1 });
  });

  it("versionSource of a WebAssembly version is a NotFoundError", async () => {
    const { fn } = setup(json(404, errBody("function_source_not_found")));
    await expect(fn.versionSource("w", 1)).rejects.toBeInstanceOf(NotFoundError);
  });

  it("logs passes the limit; types returns the declarations as text", async () => {
    const log = { id: 1, function: "hello", version: 2, trigger: "http", status: "ok", duration_ms: 1, fuel_used: 1, logs: [], created_at: "t" };
    const { f, fn } = setup(json(200, { items: [log] }), json(200, { items: [] }), text(200, "declare module 'forgebase' {}", "application/typescript; charset=utf-8"));
    expect(await fn.logs("hello", { limit: 5 })).toEqual([log]);
    await fn.logs("hello");
    expect(await fn.types()).toBe("declare module 'forgebase' {}");
    expect(f.calls[0]!.url).toBe("http://x/api/v1/functions/hello/logs?limit=5");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/functions/hello/logs");
    expect(f.calls[2]!.url).toBe("http://x/api/v1/functions/types");
  });
});

describe("functions: secrets and kv rules", () => {
  it("secrets list / set / delete", async () => {
    const secret = { name: "API KEY", created_at: "t", updated_at: "t" };
    const { f, fn } = setup(json(200, { items: [secret] }), json(200, secret), json(204, null));
    expect(await fn.secrets.list("hello")).toEqual([secret]);
    expect(await fn.secrets.set("hello", "API KEY", "s3cret")).toEqual(secret);
    await fn.secrets.delete("hello", "API KEY");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/functions/hello/secrets",
      "PUT http://x/api/v1/functions/hello/secrets/API%20KEY",
      "DELETE http://x/api/v1/functions/hello/secrets/API%20KEY",
    ]);
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ value: "s3cret" });
  });

  it("kvRules get / set wrap and unwrap items; a non-admin set is a PermissionError", async () => {
    const rules = [{ namespace: "app.*", access: "read" as const }];
    const { f, fn } = setup(json(200, { items: rules }), json(200, { items: rules }), json(403, errBody("kv_rules_require_admin")));
    expect(await fn.kvRules.get("hello")).toEqual(rules);
    expect(await fn.kvRules.set("hello", rules)).toEqual(rules);
    expect(f.calls[1]).toMatchObject({ method: "PUT", url: "http://x/api/v1/functions/hello/kv-rules" });
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ items: rules });
    await expect(fn.kvRules.set("hello", [])).rejects.toBeInstanceOf(PermissionError);
  });
});

describe("functions: invoke", () => {
  it("POSTs JSON to /invoke and parses a JSON answer", async () => {
    const { f, fn } = setup(json(200, { hello: "ada" }));
    expect(await fn.invoke<{ hello: string }>("hello", { name: "ada" })).toEqual({ hello: "ada" });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/functions/hello/invoke" });
    expect(f.calls[0]!.headers["Content-Type"]).toBe("application/json");
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ name: "ada" });
  });

  it("sub-path, method, query, version and headers", async () => {
    const { f, fn } = setup(json(200, {}));
    await fn.invoke("api", undefined, { method: "get", path: "/users/42/a b", query: { q: "x y", skip: undefined }, version: 3, headers: { "X-Trace": "1" } });
    expect(f.calls[0]).toMatchObject({ method: "GET", url: "http://x/api/v1/functions/api/invoke/users/42/a%20b?q=x+y&version=3" });
    expect(f.calls[0]!.headers["X-Trace"]).toBe("1");
    expect(f.calls[0]!.body).toBeUndefined();
  });

  it("an empty path stays on /invoke; no version leaves the query alone", async () => {
    const { f, fn } = setup(json(200, {}));
    await fn.invoke("api", undefined, { path: "/" });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/functions/api/invoke");
  });

  it("a string body goes out as is, not as a JSON string", async () => {
    const { f, fn } = setup(text(200, "pong"));
    expect(await fn.invoke("echo", "ping", { headers: { "Content-Type": "text/plain" } })).toBe("pong");
    expect(f.calls[0]!.body).toBe("ping");
    expect(f.calls[0]!.headers["Content-Type"]).toBe("text/plain");
  });

  it("204 resolves undefined; an empty body too", async () => {
    const { fn } = setup(json(204, null), text(200, ""));
    expect(await fn.invoke("x")).toBeUndefined();
    expect(await fn.invoke("x")).toBeUndefined();
  });

  it("a +json content type is parsed, broken JSON is an InvalidResponseError", async () => {
    const { fn } = setup(text(200, '{"a":1}', "application/vnd.api+json"), text(200, "{", "application/json"));
    expect(await fn.invoke("x")).toEqual({ a: 1 });
    await expect(fn.invoke("x")).rejects.toBeInstanceOf(InvalidResponseError);
  });

  it("invokeRaw returns the Response with its headers", async () => {
    const { fn } = setup(new Response("bytes", { status: 201, headers: { "x-custom": "yes", "content-type": "application/octet-stream" } }));
    const res = await fn.invokeRaw("x", { a: 1 });
    expect(res.status).toBe(201);
    expect(res.headers.get("x-custom")).toBe("yes");
    expect(await res.text()).toBe("bytes");
  });

  it("a failing function answer raises the matching typed error", async () => {
    const { fn } = setup(json(404, errBody("nope")), json(502, errBody("function_trap")), json(504, errBody("function_timeout")));
    await expect(fn.invoke("x")).rejects.toBeInstanceOf(NotFoundError);
    await expect(fn.invoke("x")).rejects.toMatchObject({ status: 502, code: "function_trap" });
    await expect(fn.invoke("x")).rejects.toMatchObject({ status: 504, code: "function_timeout" });
  });
});
