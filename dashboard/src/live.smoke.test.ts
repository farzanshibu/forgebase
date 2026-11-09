// @vitest-environment node
/**
 * Live smoke test (FB-23): boots the real `forgebase` binary, creates an admin with the
 * CLI, signs in through the dashboard's own API client (cookie session + CSRF) and loads
 * users, roles and the Database Studio collection list.
 *
 * Opt in with `FB_LIVE_BIN=/path/to/target/debug/forgebase npm run test:live`
 * (skipped otherwise, so plain `npm test` stays hermetic).
 */
import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { listRealms, listRoles } from "./api/access";
import { searchUsers, listAdmins, listSuspensions } from "./api/authAdmin";
import { setCsrfToken } from "./api/token";
import { isMfaChallenge, login, logout, me } from "./api/session";
import { listCollections, createCollection } from "./api/v1";
import {
  createVectorCollection, deleteVectorCollection, listVectorCollections, measureRecall, rebuildVectorIndex, upsertVectors,
  vectorCollectionStats, vectorHybrid, vectorSearch, vectorStats,
} from "./api/vector";
import { getMetrics, readingFrom, pointFrom } from "./api/metrics";
import { getBillingSettings, isBillingDisabled } from "./api/billing";
import {
  addDomain, createClient, deleteClient, getBranding, listAgreements, listClients, listDeliveries, listDomains, listRealmIdentities, listTemplates, previewEmail,
  publishAgreement, putBranding, putRealmIdentity, putTemplate, rotateClientSecret, rotateOidcKeys, deleteDomain, listConsents, realmInfo,
} from "./api";
import {
  copyObject, createBucket, deleteBucket, deleteObject, downloadObject, getBucketAcl, listBuckets, listObjects, listVersions, putBucketAcl, restoreVersion,
  signUrl, storageStats, updateBucket, uploadObject, reconcileStorage,
} from "./api/storage";
import { kvDelete, kvExec, kvGet, kvHashSet, kvInfo, kvListPush, kvNamespaces, kvScan, kvSetString, kvSetTtl, kvStreamAdd, kvZAdd, kvSetAdd, kvPublish } from "./api/kv";
import { createEdge, createNode, graphStats, listEdges, listNodes, neighbors, shortestPath, traverse } from "./api/graph";

const BIN = process.env.FB_LIVE_BIN;
const EMAIL = "smoke-admin@example.com";
const PASSWORD = "correct horse battery staple 42";

const freePort = () =>
  new Promise<number>((resolve, reject) => {
    const s = createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address() as { port: number };
      s.close(() => resolve(port));
    });
  });

describe.skipIf(!BIN)("live server", () => {
  let dir = "";
  let server: ChildProcess | undefined;
  let base = "";
  const jar = new Map<string, string>();
  const realFetch = globalThis.fetch;

  beforeAll(async () => {
    dir = mkdtempSync(join(tmpdir(), "fb23-live-"));
    const port = await freePort();
    base = `http://127.0.0.1:${port}`;
    const env = { ...process.env, FORGEBASE_DATA_DIR: join(dir, "data"), FORGEBASE_HOST: "127.0.0.1", FORGEBASE_PORT: String(port) };
    const run = (args: string[], input?: string) => {
      const r = spawnSync(BIN!, args, { cwd: dir, env, input, encoding: "utf8" });
      if (r.status !== 0) throw new Error(`forgebase ${args.join(" ")} failed: ${r.stderr}${r.stdout}`);
    };
    run(["init", dir]);
    run(["admin", "create", "--email", EMAIL, "--password-stdin"], `${PASSWORD}\n`);
    server = spawn(BIN!, ["serve"], { cwd: dir, env, stdio: "ignore" });
    for (let i = 0; i < 100; i++) {
      try {
        if ((await realFetch(`${base}/health`)).ok) break;
      } catch {
        /* not up yet */
      }
      await new Promise((r) => setTimeout(r, 100));
    }
    // The dashboard client uses relative URLs and the browser's cookie jar: emulate both.
    globalThis.fetch = (async (input: RequestInfo | URL, init: RequestInit = {}) => {
      const url = typeof input === "string" && input.startsWith("/") ? base + input : input;
      const headers = new Headers(init.headers);
      if (jar.size) headers.set("Cookie", [...jar].map(([k, v]) => `${k}=${v}`).join("; "));
      const res = await realFetch(url, { ...init, headers });
      for (const c of res.headers.getSetCookie()) {
        const pair = c.split(";")[0] ?? "";
        const eq = pair.indexOf("=");
        const [k, v] = [pair.slice(0, eq).trim(), pair.slice(eq + 1)];
        if (/max-age=0/i.test(c) || v === "") jar.delete(k);
        else jar.set(k, v);
      }
      return res;
    }) as typeof fetch;
  }, 60_000);

  afterAll(() => {
    globalThis.fetch = realFetch;
    server?.kill("SIGTERM");
    if (dir) rmSync(dir, { recursive: true, force: true });
  });

  it("rejects bad credentials", async () => {
    await expect(login(EMAIL, "wrong password")).rejects.toMatchObject({ status: 401 });
  });

  it("signs in with a cookie session and loads Studio data", async () => {
    const res = await login(EMAIL, PASSWORD);
    expect(isMfaChallenge(res)).toBe(false);
    if (isMfaChallenge(res)) return;
    expect(jar.has("fb_session")).toBe(true);
    setCsrfToken(res.csrf_token ?? jar.get("fb_csrf") ?? null);

    const who = await me();
    expect(who.admin?.email).toBe(EMAIL);

    // Database Studio: collection list, plus a write that needs the CSRF header.
    const before = await listCollections();
    expect(Array.isArray(before)).toBe(true);
    await createCollection({ name: "smoke_notes", type: "base", fields: [] });
    expect((await listCollections()).some((c) => c.name === "smoke_notes")).toBe(true);

    // Auth Studio: users (auth collection), admins, suspensions.
    const users = await searchUsers("users", {});
    expect(users).toHaveProperty("items");
    expect((await listAdmins()).map((a) => a.email)).toContain(EMAIL);
    expect(Array.isArray(await listSuspensions())).toBe(true);

    // Access Studio: roles and realms.
    const roles = await listRoles();
    expect(roles.length).toBeGreaterThan(0);
    expect(Array.isArray(await listRealms())).toBe(true);
  });

  it("drives Vector Studio: collections, records, search, hybrid, index", async () => {
    await createVectorCollection({ name: "smoke_vec", description: "live", fields: [{ name: "embedding", dimension: 3, metric: "l2" }] });
    expect((await listVectorCollections()).map((c) => c.name)).toContain("smoke_vec");
    const up = await upsertVectors("smoke_vec", [
      { id: "a", vectors: { embedding: [1, 0, 0] }, metadata: { tag: "x" }, text: "red apple" },
      { id: "b", vectors: { embedding: [0, 1, 0] }, metadata: { tag: "y" }, text: "green pear" },
      { id: "c", vectors: { embedding: [0.9, 0.1, 0] }, metadata: { tag: "x" }, text: "red cherry" },
    ]);
    expect(up.created).toBe(3);
    const hits = await vectorSearch("smoke_vec", { field: "embedding", vector: [1, 0, 0], k: 5, filter: 'tag = "x"' });
    expect(hits.hits.map((h) => h.id)).toEqual(["a", "c"]);
    const hyb = await vectorHybrid("smoke_vec", { field: "embedding", vector: [1, 0, 0], text: "red", k: 5 });
    expect(hyb.hits.length).toBeGreaterThan(0);
    expect(hyb.hits[0]).toHaveProperty("score");
    const rebuilt = await rebuildVectorIndex("smoke_vec", "embedding");
    expect(rebuilt.indexes[0]?.field).toBe("embedding");
    expect((await vectorCollectionStats("smoke_vec")).items).toBe(3);
    expect((await vectorStats()).collections).toBeGreaterThan(0);
    expect((await measureRecall("smoke_vec", { field: "embedding", samples: 3, k: 2 })).samples).toBeGreaterThan(0);
    await deleteVectorCollection("smoke_vec");
    expect((await listVectorCollections()).map((c) => c.name)).not.toContain("smoke_vec");
  });

  it("drives Graph Studio: nodes, edges, neighbours, traversal, path", async () => {
    for (const id of ["alice", "bob", "carol"]) await createNode({ id, label: "Person", properties: { name: id } });
    await createEdge({ type: "KNOWS", source: "alice", target: "bob" });
    await createEdge({ type: "KNOWS", source: "bob", target: "carol" });
    expect((await listNodes({ label: "Person" })).items).toHaveLength(3);
    expect((await listEdges({ type: "KNOWS" })).items).toHaveLength(2);
    expect((await neighbors("bob", { direction: "both" })).items).toHaveLength(2);
    const t = await traverse({ start: "alice", direction: "out", max_depth: 2, include_start: true });
    expect(t.nodes.map((n) => n.node.id).sort()).toEqual(["alice", "bob", "carol"]);
    const p = await shortestPath({ from: "alice", to: "carol" });
    expect(p.found).toBe(true);
    expect(p.nodes.map((n) => n.id)).toEqual(["alice", "bob", "carol"]);
    const st = await graphStats();
    expect(st.nodes).toBe(3);
    expect(st.edge_types[0]).toMatchObject({ type: "KNOWS", count: 2 });
  });

  it("reports billing as disabled while the server has it off (default): the routes answer 404", async () => {
    const err = await getBillingSettings().then(() => undefined, (e: unknown) => e);
    // A binary built before FB-70 has no billing routes either; both answer 404.
    expect(isBillingDisabled(err)).toBe(true);
  });

  it("drives Storage Studio: buckets, upload, folders, versions, copy, signed URL, ACL, reconcile", async () => {
    await createBucket({ name: "smoke-files", versioning: true });
    expect((await listBuckets()).map((b) => b.name)).toContain("smoke-files");
    await updateBucket("smoke-files", { visibility: "authenticated" });
    await uploadObject("smoke-files", "docs/a.txt", new Blob(["one"], { type: "text/plain" }));
    await uploadObject("smoke-files", "docs/a.txt", new Blob(["two!"], { type: "text/plain" }));
    const big = new Uint8Array(300);
    await uploadObject("smoke-files", "docs/big.bin", new Blob([big]), { threshold: 100, chunkSize: 128 });
    const root = await listObjects("smoke-files", { delimiter: "/" });
    expect(root.prefixes).toEqual(["docs/"]);
    const docs = await listObjects("smoke-files", { prefix: "docs/", delimiter: "/" });
    expect(docs.objects.map((o) => o.key).sort()).toEqual(["docs/a.txt", "docs/big.bin"]);
    expect(docs.objects.find((o) => o.key === "docs/big.bin")!.size).toBe(300);
    const versions = await listVersions("smoke-files", "docs/a.txt");
    expect(versions).toHaveLength(2);
    const old = versions.find((v) => !v.is_latest)!;
    expect(await (await downloadObject("smoke-files", "docs/a.txt", old.version_id)).text()).toBe("one");
    await restoreVersion("smoke-files", "docs/a.txt", old.version_id);
    expect(await (await downloadObject("smoke-files", "docs/a.txt")).text()).toBe("one");
    await copyObject("smoke-files", "docs/a.txt", "copy/a.txt");
    expect((await listObjects("smoke-files", { prefix: "copy/" })).objects).toHaveLength(1);
    const signed = await signUrl("smoke-files", { key: "docs/a.txt", expires_in: 300 });
    expect(signed.url).toContain("/signed/smoke-files/docs/a.txt");
    await putBucketAcl("smoke-files", { entries: [{ prefix: "docs/", grantee: "role:viewer", permission: "read" }] });
    expect((await getBucketAcl("smoke-files")).entries).toHaveLength(1);
    expect((await storageStats()).buckets).toBeGreaterThan(0);
    expect((await reconcileStorage()).missing_blobs).toEqual([]);
    await deleteObject("smoke-files", "copy/a.txt");
    await deleteBucket("smoke-files", true);
    expect((await listBuckets()).map((b) => b.name)).not.toContain("smoke-files");
  });

  it("drives Redis Studio: keys of every type, TTL, scan, console, stats", async () => {
    const ns = "smoke";
    await kvSetString(ns, "greeting", "hello");
    await kvHashSet(ns, "user:1", { name: "Ann" });
    await kvListPush(ns, "todo", "right", ["a", "b"]);
    await kvSetAdd(ns, "tags", ["red"]);
    await kvZAdd(ns, "board", [{ member: "ann", score: 10 }]);
    await kvStreamAdd(ns, "events", { kind: "login" });
    const all = await kvScan(ns, { count: 100 });
    expect(all.keys.sort()).toEqual(["board", "events", "greeting", "tags", "todo", "user:1"]);
    expect((await kvScan(ns, { match: "user:*" })).keys).toEqual(["user:1"]);
    expect((await kvScan(ns, { type: "zset" })).keys).toEqual(["board"]);
    for (const [k, t] of [["greeting", "string"], ["user:1", "hash"], ["todo", "list"], ["tags", "set"], ["board", "zset"], ["events", "stream"]] as const) expect((await kvGet(ns, k)).type).toBe(t);
    await kvSetTtl(ns, "greeting", 60_000);
    expect((await kvGet(ns, "greeting")).ttl_ms).toBeGreaterThan(0);
    expect(await kvExec(ns, ["GET", "greeting"])).toBe("hello");
    expect((await kvNamespaces()).map((n) => n.name)).toContain(ns);
    expect((await kvInfo()).keys).toBeGreaterThan(0);
    expect(typeof (await kvPublish(ns, "news", "x")).receivers).toBe("number");
    expect((await kvDelete(ns, "greeting")).deleted).toBe(true);
  });

  it("drives Identity Studio: realms, client apps, keys, branding, domains, email, agreements", async () => {
    const realms = await listRealmIdentities();
    expect(realms.map((r) => r.realm)).toContain("default");
    await putRealmIdentity("default", { config: realms.find((r) => r.realm === "default")!.config });
    expect((await realmInfo("default")).realm).toBe("default");

    const made = await createClient({ name: "Smoke app", type: "confidential", redirect_uris: ["https://app.example.com/cb"], realm: "default" });
    expect(made.client_secret).toBeTruthy();
    const id = made.client.client_id;
    expect((await listClients("default")).map((c) => c.client_id)).toContain(id);
    const rotated = await rotateClientSecret(id, 3600);
    expect(rotated.client_secret).not.toBe(made.client_secret);
    expect(rotated.client.previous_secret_expires_at).toBeTruthy();
    // Consents are per user: an administrator session has none to list (an admin-wide list is a missing endpoint).
    await expect(listConsents()).rejects.toMatchObject({ message: "only users have consents" });
    expect((await rotateOidcKeys("default")).kid).toBeTruthy();
    await deleteClient(id);

    expect((await getBranding("default")).realm).toBe("default");
    expect((await putBranding({ display_name: "Smoke", primary_color: "#123456" }, "default")).primary_color).toBe("#123456");
    const d = await addDomain("login.smoke.example.com", "default");
    expect(d.verification?.record_type).toBe("TXT");
    expect((await listDomains("default")).map((x) => x.host)).toContain("login.smoke.example.com");
    await deleteDomain(d.id);

    const tpls = await listTemplates("default");
    expect(tpls.length).toBeGreaterThan(0);
    const t = tpls[0]!;
    const pv = await previewEmail({ draft: { subject: t.subject, text: t.text }, variables: {} }, "default");
    expect(Array.isArray(pv.missing)).toBe(true);
    await putTemplate(t.key, { subject: "Smoke subject", text: t.text }, "default");
    expect(Array.isArray(await listDeliveries({ realm: "default" }))).toBe(true);

    const a = await publishAgreement({ kind: "terms", title: "Smoke terms", body: "Be nice." }, "default");
    expect(a.version).toBeGreaterThanOrEqual(1);
    expect((await listAgreements("default")).map((x) => x.id)).toContain(a.id);
  });

  it("feeds the Dashboard overview: /metrics parses, stats endpoints answer", async () => {
    const a = readingFrom(await getMetrics(), Date.now());
    expect(a.uptime).toBeDefined();
    expect(a.dbSize).toBeGreaterThan(0);
    expect(a.requests).toBeGreaterThan(0);
    await storageStats();
    await vectorStats();
    const b = readingFrom(await getMetrics(), Date.now() + 1000);
    expect(pointFrom(a, b).rps).toBeGreaterThanOrEqual(0);
  });

  it("ends the session on sign-out", async () => {
    await logout();
    expect(jar.has("fb_session")).toBe(false);
    await expect(me()).rejects.toMatchObject({ status: 401 });
  });
});
