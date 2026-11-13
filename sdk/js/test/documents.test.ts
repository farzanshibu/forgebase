import { describe, expect, it } from "vitest";
import { ConflictError, NotFoundError, Transport } from "../src/index";
import { DocumentsApi } from "../src/documents";
import { errBody, json, scripted } from "./helpers";

const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  return { f, d: new DocumentsApi(new Transport({ baseUrl: "http://x", fetch: f.fetch })).collection<{ id: string; title?: string; _version?: number }>("my notes") };
};

describe("documents", () => {
  it("creates one document and many", async () => {
    const { f, d } = setup(json(201, { id: "d1", title: "a" }), json(201, { items: [{ id: "d1" }, { id: "d2" }] }));
    expect((await d.create({ title: "a" })).id).toBe("d1");
    expect(await d.createMany([{ title: "a" }, { title: "b" }])).toHaveLength(2);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/documents/my%20notes" });
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ title: "a" });
    expect(JSON.parse(f.calls[1]!.body!)).toEqual([{ title: "a" }, { title: "b" }]);
  });

  it("gets with expand and fields", async () => {
    const { f, d } = setup(json(200, { id: "a/b" }));
    await d.get("a/b", { expand: ["owner", "tags"], fields: ["id", "title"] });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/documents/my%20notes/a%2Fb?expand=owner%2Ctags&fields=id%2Ctitle");
  });

  it("patches and replaces, sending If-Match for a version", async () => {
    const { f, d } = setup(json(200, { id: "d1", _version: 4 }), json(200, { id: "d1" }), json(200, { id: "d1" }));
    await d.patch("d1", { title: "b" }, { version: 3 });
    await d.replace("d1", { title: "c" });
    await d.patch("d1", { title: null } as never);
    expect(f.calls[0]).toMatchObject({ method: "PATCH", url: "http://x/api/v1/documents/my%20notes/d1" });
    expect(f.calls[0]!.headers["If-Match"]).toBe('"3"');
    expect(f.calls[1]).toMatchObject({ method: "PUT" });
    expect(f.calls[1]!.headers["If-Match"]).toBeUndefined();
    expect(JSON.parse(f.calls[2]!.body!)).toEqual({ title: null });
  });

  it("a stale write is a ConflictError", async () => {
    const { d } = setup(json(412, errBody("version_mismatch")));
    await expect(d.replace("d1", {}, { version: 1 })).rejects.toBeInstanceOf(ConflictError);
  });

  it("deletes, honouring If-Match", async () => {
    const { f, d } = setup(json(204, null));
    await d.delete("d1", { version: 2 });
    expect(f.calls[0]).toMatchObject({ method: "DELETE", url: "http://x/api/v1/documents/my%20notes/d1" });
    expect(f.calls[0]!.headers["If-Match"]).toBe('"2"');
  });

  it("lists versions", async () => {
    const { f, d } = setup(json(200, { items: [{ version: 1, op: "create", actor: "u", changed_at: "t", data: { title: "a" } }] }));
    expect((await d.versions("d1"))[0]!.version).toBe(1);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/documents/my%20notes/d1/versions");
  });

  it("a missing document is a NotFoundError", async () => {
    const { d } = setup(json(404, errBody("not_found")));
    await expect(d.get("zz")).rejects.toBeInstanceOf(NotFoundError);
  });
});
