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

  it("ends the session on sign-out", async () => {
    await logout();
    expect(jar.has("fb_session")).toBe(false);
    await expect(me()).rejects.toMatchObject({ status: 401 });
  });
});
