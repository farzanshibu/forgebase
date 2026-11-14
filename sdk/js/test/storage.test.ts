import { createHash } from "node:crypto";
import { describe, expect, it } from "vitest";
import { ConflictError, NotFoundError, Transport } from "../src/index";
import { StorageApi, sha256Hex } from "../src/storage";
import type { FetchLike } from "../src/index";
import { errBody, json, scripted } from "./helpers";

const obj = (key: string, extra: Record<string, unknown> = {}) => ({
  bucket: "b", key, version_id: "v1", size: 3, content_type: "text/plain", metadata: {}, is_latest: true, delete_marker: false, created_by: "u", created_at: "t", ...extra,
});
const bucket = { name: "avatars", visibility: "private", versioning: false, owner: "u", created_at: "t", updated_at: "t" };
const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  return { f, s: new StorageApi(new Transport({ baseUrl: "http://x", fetch: f.fetch })) };
};

describe("storage.buckets", () => {
  it("lists, gets, creates and updates with snake_case bodies", async () => {
    const { f, s } = setup(json(200, { items: [bucket] }), json(200, bucket), json(201, bucket), json(200, bucket));
    expect(await s.buckets.list()).toHaveLength(1);
    expect((await s.buckets.get("my bucket")).name).toBe("avatars");
    await s.buckets.create({ name: "avatars", visibility: "public", versioning: true, allowedContentTypes: ["image/*"], maxObjectBytes: 100 });
    await s.buckets.update("avatars", { maxObjectBytes: 0, versioning: false });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/storage/buckets");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/storage/buckets/my%20bucket");
    expect(f.calls[2]).toMatchObject({ method: "POST", url: "http://x/api/v1/storage/buckets" });
    expect(JSON.parse(f.calls[2]!.body!)).toEqual({ name: "avatars", visibility: "public", versioning: true, allowed_content_types: ["image/*"], max_object_bytes: 100 });
    expect(f.calls[3]).toMatchObject({ method: "PATCH", url: "http://x/api/v1/storage/buckets/avatars" });
    expect(JSON.parse(f.calls[3]!.body!)).toEqual({ max_object_bytes: 0, versioning: false });
  });

  it("deletes with force, reads and replaces the ACL, reads usage", async () => {
    const acl = { entries: [{ grantee: "public", permission: "read", prefix: "pub/" }] };
    const { f, s } = setup(json(204, null), json(204, null), json(200, acl), json(200, acl), json(200, { bucket: "a", objects: 1, versions: 2, logical_bytes: 3, version_bytes: 4 }));
    await s.buckets.delete("a");
    await s.buckets.delete("a", { force: true });
    expect(await s.buckets.acl("a")).toEqual(acl);
    await s.buckets.setAcl("a", acl.entries as never);
    expect((await s.buckets.usage("a")).versions).toBe(2);
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "DELETE http://x/api/v1/storage/buckets/a",
      "DELETE http://x/api/v1/storage/buckets/a?force=true",
      "GET http://x/api/v1/storage/buckets/a/acl",
      "PUT http://x/api/v1/storage/buckets/a/acl",
      "GET http://x/api/v1/storage/buckets/a/usage",
    ]);
    expect(JSON.parse(f.calls[3]!.body!)).toEqual(acl);
  });

  it("maps a missing bucket to NotFoundError", async () => {
    const { s } = setup(json(404, errBody("not_found")));
    await expect(s.buckets.get("nope")).rejects.toBeInstanceOf(NotFoundError);
  });
});

describe("storage.from(bucket) objects", () => {
  it("puts with content type, metadata and checksum headers; keys keep their slashes", async () => {
    const { f, s } = setup(json(201, obj("a b/c#d.txt")));
    const out = await s.from("b").put("a b/c#d.txt", "hey", { contentType: "text/plain", metadata: { owner: "me" }, checksumSha256: "ab" });
    expect(out.key).toBe("a b/c#d.txt");
    expect(f.calls[0]).toMatchObject({ method: "PUT", url: "http://x/api/v1/storage/buckets/b/objects/a%20b/c%23d.txt", body: "hey" });
    expect(f.calls[0]!.headers).toMatchObject({ "Content-Type": "text/plain", "X-Forgebase-Checksum-Sha256": "ab", "X-Forgebase-Meta-owner": "me" });
  });

  it("sends binary bodies as they are", async () => {
    let seen: unknown;
    const fetch: FetchLike = async (_u, init) => {
      seen = init?.body;
      return json(201, obj("k"));
    };
    const s = new StorageApi(new Transport({ baseUrl: "http://x", fetch }));
    const bytes = new Uint8Array([1, 2, 3]);
    await s.from("b").put("k", bytes);
    expect(seen).toBe(bytes);
  });

  it("maps checksum_mismatch (422) to ValidationError", async () => {
    const { s } = setup(json(422, errBody("checksum_mismatch", "bad", { expected: "a", actual: "b" })));
    await expect(s.from("b").put("k", "x", { checksumSha256: "a" })).rejects.toMatchObject({ status: 422, code: "checksum_mismatch", details: { actual: "b" } });
  });

  it("downloads: raw response, ranges, version and download name; helpers read the body", async () => {
    const res = () => new Response("hello", { status: 200 });
    const { f, s } = setup(res(), new Response("ell", { status: 206 }), res(), res(), res(), new Response('{"a":1}'), res());
    const b = s.from("b");
    expect((await b.download("x/y.txt")).status).toBe(200);
    const part = await b.download("x/y.txt", { range: { start: 1, end: 3 }, version: "v9", downloadAs: "my file.txt" });
    expect(part.status).toBe(206);
    expect(await b.text("t")).toBe("hello");
    expect(await b.bytes("t")).toEqual(new TextEncoder().encode("hello"));
    expect(await (await b.blob("t")).text()).toBe("hello");
    expect(await b.json("j")).toEqual({ a: 1 });
    await b.download("t", { range: { suffix: 2 } });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/storage/buckets/b/objects/x/y.txt");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/storage/buckets/b/objects/x/y.txt?version=v9&download=my+file.txt");
    expect(f.calls[1]!.headers.Range).toBe("bytes=1-3");
    expect(f.calls[6]!.headers.Range).toBe("bytes=-2");
  });

  it("accepts a literal Range string", async () => {
    const { f, s } = setup(new Response("x", { status: 206 }));
    await s.from("b").download("t", { range: "bytes=5-" });
    expect(f.calls[0]!.headers.Range).toBe("bytes=5-");
  });

  it("lists with prefix, delimiter and cursor, and iterates every page", async () => {
    const page1 = { objects: [obj("a"), obj("b")], prefixes: [], next_cursor: "c1" };
    const page2 = { objects: [obj("c")], prefixes: [] };
    const { f, s } = setup(json(200, page1), json(200, page1), json(200, page2));
    expect((await s.from("b").list({ prefix: "p/", delimiter: "/", limit: 2 })).next_cursor).toBe("c1");
    expect(f.calls[0]!.url).toBe("http://x/api/v1/storage/buckets/b/objects?prefix=p%2F&delimiter=%2F&limit=2");
    const keys: string[] = [];
    for await (const o of s.from("b").iterate({ prefix: "p/" })) keys.push(o.key);
    expect(keys).toEqual(["a", "b", "c"]);
    expect(f.calls[1]!.url).toBe("http://x/api/v1/storage/buckets/b/objects?prefix=p%2F");
    expect(f.calls[2]!.url).toBe("http://x/api/v1/storage/buckets/b/objects?prefix=p%2F&cursor=c1");
  });

  it("meta, delete, copy, move, versions and restore", async () => {
    const { f, s } = setup(json(200, obj("k")), json(200, { version_id: "v2", delete_marker: true }), json(201, obj("k2")), json(200, obj("k3")), json(200, { items: [obj("k")] }), json(201, obj("k")));
    const b = s.from("b");
    expect((await b.meta("d/k", { version: "v1" })).version_id).toBe("v1");
    expect(await b.delete("k", { version: "v1" })).toEqual({ version_id: "v2", delete_marker: true });
    await b.copy("k", "k2", { destBucket: "other", versionId: "v1" });
    await b.move("k", "k3");
    expect(await b.versions("a/k")).toHaveLength(1);
    await b.restore("k", "v1");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/storage/buckets/b/meta/d/k?version=v1",
      "DELETE http://x/api/v1/storage/buckets/b/objects/k?version=v1",
      "POST http://x/api/v1/storage/buckets/b/copy",
      "POST http://x/api/v1/storage/buckets/b/move",
      "GET http://x/api/v1/storage/buckets/b/versions/a/k",
      "POST http://x/api/v1/storage/buckets/b/restore",
    ]);
    expect(JSON.parse(f.calls[2]!.body!)).toEqual({ key: "k", dest_key: "k2", dest_bucket: "other", version_id: "v1" });
    expect(JSON.parse(f.calls[3]!.body!)).toEqual({ key: "k", dest_key: "k3" });
    expect(JSON.parse(f.calls[5]!.body!)).toEqual({ key: "k", version_id: "v1" });
  });

  it("signs download and upload URLs", async () => {
    const su = { url: "/api/v1/storage/signed/b/k?x=1", method: "GET", expires_at: "t" };
    const { f, s } = setup(json(200, su), json(200, { ...su, method: "PUT" }), json(200, su));
    const b = s.from("b");
    expect(await b.createSignedUrl("k", { expiresIn: 60, downloadAs: "f.txt", versionId: "v1" })).toEqual(su);
    await b.createSignedUploadUrl("k", { contentType: "image/png" });
    await b.sign("k");
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ key: "k", method: "GET", expires_in: 60, download_as: "f.txt", version_id: "v1" });
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ key: "k", method: "PUT", content_type: "image/png" });
    expect(JSON.parse(f.calls[2]!.body!)).toEqual({ key: "k" });
  });
});

describe("storage.signed", () => {
  it("resolves URLs and uses them without credentials", async () => {
    const f = scripted(new Response("data"), json(201, obj("k")));
    const t = new Transport({ baseUrl: "http://x/", fetch: f.fetch });
    t.credentials = { headers: async () => ({ Authorization: "Bearer fb_secret" }), onUnauthorized: async () => false };
    const fb = { storage: new StorageApi(t) };
    const rel = "/api/v1/storage/signed/b/k?v=1&sig=ab";
    expect(fb.storage.signed.resolve({ url: rel, method: "GET", expires_at: "t" })).toBe(`http://x${rel}`);
    expect(fb.storage.signed.resolve("https://cdn/x")).toBe("https://cdn/x");
    expect(await (await fb.storage.signed.download(`http://x${rel}`, { range: { start: 0, end: 1 } })).text()).toBe("data");
    await fb.storage.signed.upload(rel, "abc", { contentType: "text/plain" });
    expect(f.calls[0]).toMatchObject({ method: "GET", url: `http://x${rel}` });
    expect(f.calls[0]!.headers.Range).toBe("bytes=0-1");
    expect(Object.keys(f.calls[0]!.headers).map((k) => k.toLowerCase())).not.toContain("authorization");
    expect(f.calls[1]).toMatchObject({ method: "PUT", url: `http://x${rel}`, body: "abc" });
    expect(Object.keys(f.calls[1]!.headers).map((k) => k.toLowerCase())).not.toContain("authorization");
  });
});

describe("storage.uploads", () => {
  const up = (extra: Record<string, unknown> = {}) => ({ id: "up1", bucket: "b", key: "k", kind: "multipart", content_type: "x", received_bytes: 0, parts: [], expires_at: "t", ...extra });

  it("creates, reads and aborts sessions; sends parts and completes", async () => {
    const { f, s } = setup(json(201, up()), json(200, up()), json(200, { part_number: 2, size: 3, checksum_sha256: "h" }), json(201, obj("k")), json(204, null));
    const u = s.uploads;
    await u.create("b", "k", { kind: "multipart", contentType: "video/mp4", metadata: { a: "1" }, size: 10, checksumSha256: "h" });
    await u.get("up1");
    expect((await u.putPart("up1", 2, "abc", { checksumSha256: "h" })).part_number).toBe(2);
    await u.complete("up1", { parts: [{ partNumber: 1, checksumSha256: "a" }, { partNumber: 2 }] });
    await u.abort("up1");
    expect(JSON.parse(f.calls[0]!.body!)).toEqual({ key: "k", kind: "multipart", content_type: "video/mp4", metadata: { a: "1" }, size: 10, checksum_sha256: "h" });
    expect(f.calls[0]!.url).toBe("http://x/api/v1/storage/buckets/b/uploads");
    expect(f.calls[2]).toMatchObject({ method: "PUT", url: "http://x/api/v1/storage/uploads/up1/parts/2", body: "abc" });
    expect(f.calls[2]!.headers["X-Forgebase-Checksum-Sha256"]).toBe("h");
    expect(JSON.parse(f.calls[3]!.body!)).toEqual({ parts: [{ part_number: 1, checksum_sha256: "a" }, { part_number: 2 }] });
    expect(f.calls[4]).toMatchObject({ method: "DELETE", url: "http://x/api/v1/storage/uploads/up1" });
  });

  it("upload() splits the data into parts, reports progress and completes", async () => {
    const { f, s } = setup(json(201, up()), json(200, { part_number: 1, size: 4, checksum_sha256: "h" }), json(201, obj("k")));
    const seen: number[] = [];
    await s.uploads.upload("b", "k", new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]), { partSize: 4, onProgress: (p) => seen.push(p.uploadedBytes) });
    expect(f.calls.map((c) => c.url.replace("http://x/api/v1/storage/", ""))).toEqual(["buckets/b/uploads", "uploads/up1/parts/1", "uploads/up1/parts/2", "uploads/up1/parts/3", "uploads/up1/complete"]);
    expect(seen).toEqual([4, 8, 10]);
  });

  it("upload() aborts the session when a part fails", async () => {
    const { f, s } = setup(json(201, up()), json(500, errBody("internal")), json(204, null));
    await expect(s.uploads.upload("b", "k", "abcdef", { partSize: 3 })).rejects.toMatchObject({ status: 500 });
    expect(f.calls.at(-1)).toMatchObject({ method: "DELETE", url: "http://x/api/v1/storage/uploads/up1" });
  });

  it("append() sends the offset and tells an open session from a finished object", async () => {
    const { f, s } = setup(
      json(200, up({ kind: "resumable", received_bytes: 3 }), { "Upload-Offset": "3" }),
      json(201, obj("k"), { "Upload-Offset": "6" }),
    );
    const a = await s.uploads.append("up1", 0, "abc");
    expect(a).toMatchObject({ offset: 3, upload: { id: "up1" } });
    const b = await s.uploads.append("up1", 3, "def");
    expect(b).toMatchObject({ offset: 6, object: { key: "k" } });
    expect(f.calls[0]).toMatchObject({ method: "PATCH", url: "http://x/api/v1/storage/uploads/up1", body: "abc" });
    expect(f.calls[0]!.headers["Upload-Offset"]).toBe("0");
  });

  it("append() with a wrong offset is a ConflictError carrying the server offset", async () => {
    const { s } = setup(json(409, errBody("offset_mismatch", "x", { upload_offset: 3 })));
    const err = await s.uploads.append("up1", 9, "abc").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ConflictError);
    expect((err as ConflictError).details).toEqual({ upload_offset: 3 });
  });

  it("resume() continues from the server's offset until the object is finished", async () => {
    const { f, s } = setup(
      json(200, up({ kind: "resumable", received_bytes: 4 })),
      json(200, up({ kind: "resumable", received_bytes: 8 }), { "Upload-Offset": "8" }),
      json(201, obj("k"), { "Upload-Offset": "10" }),
    );
    const out = await s.uploads.resume("up1", new Uint8Array(10), { chunkSize: 4 });
    expect(out.key).toBe("k");
    expect(f.calls.map((c) => c.headers["Upload-Offset"])).toEqual([undefined, "4", "8"]);
  });
});

describe("storage admin", () => {
  it("stats, reconcile and rotateSigningKey", async () => {
    const stats = { buckets: 1, objects: 2, versions: 3, logical_bytes: 4, stored_bytes: 5, open_uploads: 0, scope: "engine" };
    const report = { uploads_aborted: 0, staging_deleted: 0, orphan_blobs_deleted: 1, unreferenced_blobs_deleted: 0, missing_blobs: [] };
    const { f, s } = setup(json(200, stats), json(200, report), json(204, null));
    expect((await s.stats()).scope).toBe("engine");
    expect((await s.reconcile()).orphan_blobs_deleted).toBe(1);
    await s.rotateSigningKey();
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/v1/storage/stats",
      "POST http://x/api/v1/storage/maintenance/reconcile",
      "POST http://x/api/v1/storage/maintenance/rotate-signing-key",
    ]);
  });
});

describe("sha256Hex", () => {
  it("matches node:crypto for strings, bytes, buffers and blobs", async () => {
    const want = createHash("sha256").update("héllo").digest("hex");
    expect(await sha256Hex("héllo")).toBe(want);
    const bytes = new TextEncoder().encode("héllo");
    expect(await sha256Hex(bytes)).toBe(want);
    expect(await sha256Hex(bytes.buffer as ArrayBuffer)).toBe(want);
    expect(await sha256Hex(new Blob([bytes]))).toBe(want);
  });
});

describe("storage conditional reads", () => {
  const notModified = () => new Response(null, { status: 304, headers: { etag: '"abc"' } });
  const file = (body: string, etag: string) => new Response(body, { status: 200, headers: { "content-type": "text/plain", etag } });

  it("download({ ifNoneMatch }) sends the header and hands back the 304 Response", async () => {
    const { f, s } = setup(notModified(), file("hello", '"def"'));
    const res = await s.from("b").download("k", { ifNoneMatch: '"abc"' });
    expect(res.status).toBe(304);
    expect(f.calls[0]!.headers["If-None-Match"]).toBe('"abc"');
    const fresh = await s.from("b").download("k", { ifNoneMatch: '"zzz"' });
    expect(fresh.status).toBe(200);
    expect(await fresh.text()).toBe("hello");
  });

  it("without ifNoneMatch a 404 still throws", async () => {
    const { s } = setup(json(404, errBody("not_found", "no such object")));
    await expect(s.from("b").download("k")).rejects.toBeInstanceOf(NotFoundError);
  });

  it("text({ conditional }) and json({ conditional }) answer a 304 from memory and follow changes", async () => {
    const { f, s } = setup(file("v1", '"a"'), notModified(), file("v2", '"b"'), file('{"n":1}', '"j"'), new Response(null, { status: 304, headers: { etag: '"j"' } }));
    const bucket = s.from("b");
    expect(await bucket.text("t", { conditional: true })).toBe("v1");
    expect(await bucket.text("t", { conditional: true })).toBe("v1");
    expect(f.calls[1]!.headers["If-None-Match"]).toBe('"a"');
    expect(await bucket.text("t", { conditional: true })).toBe("v2");
    expect(await bucket.json<{ n: number }>("j", { conditional: true })).toEqual({ n: 1 });
    expect(await bucket.json<{ n: number }>("j", { conditional: true })).toEqual({ n: 1 });
    expect(f.calls[4]!.headers["If-None-Match"]).toBe('"j"');
  });
});
