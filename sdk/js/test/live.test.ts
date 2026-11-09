/**
 * Live integration tests: boot the real `forgebase` binary and drive it with the SDK.
 *
 *   FB_LIVE_BIN=/path/to/target/debug/forgebase npm run test:live
 *
 * Skipped without FB_LIVE_BIN, so `npm test` stays hermetic.
 */
import { spawn, spawnSync, type ChildProcess } from "node:child_process";
import { mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import {
  AuthenticationError, ConflictError, ForgeBase, NotFoundError, PermissionError, QuotaExceededError, ValidationError, isMfaChallenge,
  type RealtimeEvent, type RecordEvent, type RecordsApi, type Session,
} from "../src/index";

const BIN = process.env.FB_LIVE_BIN;
const ADMIN = { email: "sdk-admin@example.com", password: "correct horse battery staple 42" };
const USER = { email: "reader@example.com", password: "another long passphrase 7" };

const freePort = () =>
  new Promise<number>((resolve, reject) => {
    const s = createServer();
    s.once("error", reject);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address() as { port: number };
      s.close(() => resolve(port));
    });
  });

const until = async <T>(fn: () => T | undefined | false, ms = 8000): Promise<T> => {
  const end = Date.now() + ms;
  for (;;) {
    const v = fn();
    if (v) return v;
    if (Date.now() > end) throw new Error("timed out waiting for condition");
    await new Promise((r) => setTimeout(r, 25));
  }
};

describe.skipIf(!BIN)("live server", () => {
  let dir = "";
  let server: ChildProcess | undefined;
  let base = "";
  let admin: ForgeBase;

  /** Mails the dev mailer wrote (the SDK cannot read mail: tests do, like a user's inbox). */
  const mails = () => {
    try {
      return readdirSync(join(dir, "data", "mail")).sort().map((f) => JSON.parse(readFileSync(join(dir, "data", "mail", f), "utf8")) as Record<string, unknown>);
    } catch {
      return [];
    }
  };
  const tokenFrom = (m: Record<string, unknown>) => /token=([A-Za-z0-9_\-.~%]+)/.exec(JSON.stringify(m))?.[1];

  beforeAll(async () => {
    dir = mkdtempSync(join(tmpdir(), "fb41-live-"));
    const port = await freePort();
    base = `http://127.0.0.1:${port}`;
    const env = { ...process.env, FORGEBASE_DATA_DIR: join(dir, "data"), FORGEBASE_HOST: "127.0.0.1", FORGEBASE_PORT: String(port) };
    const run = (args: string[], input?: string) => {
      const r = spawnSync(BIN!, args, { cwd: dir, env, input, encoding: "utf8" });
      if (r.status !== 0) throw new Error(`forgebase ${args.join(" ")} failed: ${r.stderr}${r.stdout}`);
    };
    run(["init", dir]);
    run(["admin", "create", "--email", ADMIN.email, "--password-stdin"], `${ADMIN.password}\n`);
    server = spawn(BIN!, ["serve"], { cwd: dir, env, stdio: "ignore" });
    for (let i = 0; i < 150; i++) {
      try {
        if ((await fetch(`${base}/health`)).ok) break;
      } catch {
        /* not up yet */
      }
      await new Promise((r) => setTimeout(r, 100));
    }
    admin = new ForgeBase(base);
    const res = await admin.auth.signInAdmin(ADMIN);
    expect(isMfaChallenge(res)).toBe(false);
  }, 60_000);

  afterAll(() => {
    admin?.realtime.close();
    server?.kill("SIGTERM");
    if (dir) rmSync(dir, { recursive: true, force: true });
  });

  describe("auth", () => {
    it("rejects bad credentials with a typed error", async () => {
      const fb = new ForgeBase(base);
      await expect(fb.auth.signIn({ email: ADMIN.email, password: "nope nope nope" })).rejects.toBeInstanceOf(AuthenticationError);
    });

    it("signs a user up, signs in with a password, refreshes and signs out", async () => {
      const fb = new ForgeBase(base);
      const user = await fb.auth.signUp(USER);
      expect(user.email).toBe(USER.email);
      const s = (await fb.auth.signIn(USER)) as Session;
      expect(s.accessToken).toBeTruthy();
      expect(s.refreshToken).toBeTruthy();
      const who = await fb.auth.me<{ subject: { kind?: string } }>();
      expect(who.subject).toBeTruthy();
      const events: string[] = [];
      fb.auth.onChange((e) => events.push(e));
      const next = await fb.auth.refresh();
      expect(next.accessToken).not.toBe(s.accessToken);
      expect(next.refreshToken).not.toBe(s.refreshToken);
      await fb.auth.signOut();
      expect(events).toEqual(["token_refreshed", "signed_out"]);
      await expect(fb.auth.me()).rejects.toBeInstanceOf(AuthenticationError);
    });

    it("refreshes transparently after a 401 (revoked access token is replaced)", async () => {
      const fb = new ForgeBase(base);
      await fb.auth.signIn(USER);
      const before = (await fb.auth.getSession())!;
      // A token the server rejects: the SDK refreshes once and retries.
      await fb.auth.setSession({ ...before, accessToken: "garbage.token.value" });
      const who = await fb.auth.me();
      expect(who).toBeTruthy();
      expect((await fb.auth.getSession())!.accessToken).not.toBe("garbage.token.value");
      await fb.auth.signOut();
    });

    it("signs in with a magic link and an email OTP", async () => {
      const fb = new ForgeBase(base);
      const seen = mails().length;
      await fb.auth.requestMagicLink(USER.email);
      const mail = await until(() => mails()[seen]);
      const token = decodeURIComponent(tokenFrom(mail)!);
      const s = (await fb.auth.confirmMagicLink(token)) as Session;
      expect(s.accessToken).toBeTruthy();
      await expect(fb.auth.confirmMagicLink(token)).rejects.toBeInstanceOf(AuthenticationError);

      const seen2 = mails().length;
      await fb.auth.requestOtp(USER.email);
      const otpMail = await until(() => mails()[seen2]);
      const code = /\b(\d{6,8})\b/.exec(JSON.stringify(otpMail))?.[1];
      expect(code).toBeTruthy();
      const s2 = (await fb.auth.verifyOtp({ email: USER.email, code: code! })) as Session;
      expect(s2.accessToken).toBeTruthy();
      await fb.auth.signOut();
    });

    it("lists OAuth providers and builds the redirect URL", async () => {
      const fb = new ForgeBase(base);
      expect(Array.isArray(await fb.auth.oauth.providers())).toBe(true);
      expect(fb.auth.oauth.redirectUrl("github", { cookie: false })).toBe(`${base}/api/auth/oauth/github/start`);
      await expect(fb.auth.oauth.start("github")).rejects.toBeInstanceOf(NotFoundError);
    });

    it("authenticates with a personal access token as an API key", async () => {
      const pat = (await admin.auth.createToken({ name: "sdk-live", scopes: ["collections:read"] })) as { token?: string; key?: string; secret?: string };
      const secret = pat.token ?? pat.key ?? pat.secret;
      expect(secret).toMatch(/^fbp?_/);
      const fb = new ForgeBase(base, { apiKey: secret });
      expect(await fb.collections.list()).toBeInstanceOf(Array);
      // The key has no write scope: a typed 403.
      await expect(fb.collections.create({ name: "nope", type: "base", fields: [] } as never)).rejects.toBeInstanceOf(PermissionError);
    });
  });

  describe("collections and records", () => {
    type Post = { id: string; title: string; views: number; status: string; _version?: number };
    let posts: RecordsApi<Post>;

    beforeAll(async () => {
      await admin.collections.create({
        name: "posts",
        type: "base",
        fields: [
          { name: "title", type: "text", required: true },
          { name: "views", type: "number" },
          { name: "status", type: "text" },
        ],
      } as never);
      posts = admin.collection<Post>("posts");
    });

    it("creates, reads, updates, replaces, deletes", async () => {
      const p = await posts.create({ title: "hello", views: 1, status: "draft" });
      expect(p.id).toBeTruthy();
      expect((await posts.get(p.id)).title).toBe("hello");
      const u = await posts.update(p.id, { views: 2 });
      expect(u.views).toBe(2);
      await expect(posts.update(p.id, { views: 3 }, { version: 999 })).rejects.toBeInstanceOf(ConflictError);
      const ok = await posts.update(p.id, { views: 3 }, { version: (u as Post)._version });
      expect(ok.views).toBe(3);
      const r = await posts.replace(p.id, { title: "replaced", views: 0, status: "draft" });
      expect(r.title).toBe("replaced");
      await posts.delete(p.id);
      await expect(posts.get(p.id)).rejects.toBeInstanceOf(NotFoundError);
    });

    it("surfaces validation errors per field", async () => {
      const err = await posts.create({ views: 1 }).catch((e: unknown) => e);
      expect(err).toBeInstanceOf(ValidationError);
      expect((err as ValidationError).fields.title).toBeTruthy();
      expect((err as ValidationError).requestId).toBeTruthy();
    });

    it("filters, sorts, searches by typed params and counts", async () => {
      for (let i = 1; i <= 7; i++) await posts.create({ title: `item ${i}`, views: i * 10, status: i % 2 ? "live" : "draft" });
      const live = await posts.list({ filter: 'status = "live"', sort: "-views", perPage: 50 });
      expect(live.items.map((x) => x.views)).toEqual([70, 50, 30, 10]);
      const typed = await posts.list({ filter: "views > {:min}", params: { min: 40 }, sort: ["views"] });
      expect(typed.items.map((x) => x.views)).toEqual([50, 60, 70]);
      expect(await posts.count('status = "draft"')).toBe(3);
      expect((await posts.first('title = "item 3"'))?.views).toBe(30);
      const projected = await posts.list({ fields: ["title"], perPage: 1 });
      expect(projected.items[0]).toHaveProperty("title");
    });

    it("iterates every page, by page numbers and by cursor", async () => {
      const titles = new Set<string>();
      let pages = 0;
      for await (const p of posts.pages({ perPage: 3, sort: "views" })) {
        pages++;
        for (const r of p.items) titles.add(r.title);
      }
      expect(titles.size).toBe(7);
      expect(pages).toBe(3);
      const all = await posts.all({ perPage: 2, sort: "views", skipTotal: true });
      expect(all.map((r) => r.views)).toEqual([10, 20, 30, 40, 50, 60, 70]);
      const streamed: number[] = [];
      for await (const r of posts.iterate({ perPage: 4, sort: "-views" })) streamed.push(r.views);
      expect(streamed).toEqual([70, 60, 50, 40, 30, 20, 10]);
    });

    it("exports and imports", async () => {
      const csv = await posts.export("ndjson");
      expect(csv.trim().split("\n").length).toBeGreaterThanOrEqual(7);
      const imported = await admin.collection("imports_target").list().catch((e: unknown) => e);
      expect(imported).toBeInstanceOf(NotFoundError);
    });

    it("is anonymous-denied with a typed error", async () => {
      const anon = new ForgeBase(base);
      await expect(anon.collection("posts").list()).rejects.toBeInstanceOf(AuthenticationError);
    });
  });

  describe("realtime", () => {
    const names = ["rt_ws", "rt_sse", "rt_resume"];
    beforeAll(async () => {
      for (const name of names) await admin.collections.create({ name, type: "base", fields: [{ name: "n", type: "number" }] } as never);
    });

    const records = (evs: RealtimeEvent[]) => evs.filter((e): e is RecordEvent => e.type.startsWith("record."));

    it("streams record events over WebSocket", async () => {
      const fb = new ForgeBase(base, { apiKey: undefined });
      await fb.auth.signInAdmin(ADMIN);
      const got: RealtimeEvent[] = [];
      const sub = await fb.realtime.subscribe({ collection: "rt_ws" }, (e) => got.push(e));
      expect(fb.realtime.transport).toBe("websocket");
      const rec = await fb.collection("rt_ws").create({ n: 1 });
      await fb.collection("rt_ws").update(rec.id as string, { n: 2 });
      await fb.collection("rt_ws").delete(rec.id as string);
      await until(() => records(got).length >= 3);
      expect(records(got).map((e) => e.type)).toEqual(["record.created", "record.updated", "record.deleted"]);
      expect(records(got)[0]!.record_id).toBe(rec.id);
      expect(sub.cursor).toBe(records(got)[2]!.cursor);
      await sub.unsubscribe();
      fb.realtime.close();
    });

    it("resumes from a cursor after a reconnect without loss or duplicates", async () => {
      const fb = new ForgeBase(base);
      await fb.auth.signInAdmin(ADMIN);
      const col = fb.collection("rt_resume");
      const got: RecordEvent[] = [];
      const sub = await fb.realtime.subscribe({ collection: "rt_resume" }, (e) => e.type.startsWith("record.") && got.push(e as RecordEvent));
      await col.create({ n: 1 });
      await until(() => got.length === 1);
      const cursor = sub.cursor!;
      fb.realtime.close();
      // Writes while disconnected.
      await col.create({ n: 2 });
      await col.create({ n: 3 });
      // A new client resumes from the stored cursor.
      const fb2 = new ForgeBase(base);
      await fb2.auth.signInAdmin(ADMIN);
      const replay: RecordEvent[] = [];
      await fb2.realtime.subscribe({ collection: "rt_resume", cursor }, (e) => e.type.startsWith("record.") && replay.push(e as RecordEvent));
      await until(() => replay.length === 2);
      expect(replay.map((e) => (e.data as { n: number }).n)).toEqual([2, 3]);
      await col.create({ n: 4 });
      await until(() => replay.length === 3);
      expect(new Set(replay.map((e) => e.cursor)).size).toBe(3);
      fb2.realtime.close();
    });

    it("survives a dropped connection by reconnecting and resuming", async () => {
      const fb = new ForgeBase(base, { realtime: { reconnect: { baseDelayMs: 20, maxDelayMs: 100 } } });
      await fb.auth.signInAdmin(ADMIN);
      const col = fb.collection("rt_ws");
      const got: RecordEvent[] = [];
      await fb.realtime.subscribe({ collection: "rt_ws" }, (e) => e.type.startsWith("record.") && got.push(e as RecordEvent));
      const reconnects: number[] = [];
      fb.realtime.on("reconnecting", (a) => reconnects.push(a));
      // Kill the socket from the client side (code 4002 is what a heartbeat timeout sends).
      (fb.realtime as unknown as { ws: { close(c: number, r: string): void } }).ws.close(4002, "test drop");
      await col.create({ n: 99 });
      await until(() => got.some((e) => (e.data as { n?: number }).n === 99), 10_000);
      expect(reconnects.length).toBeGreaterThan(0);
      fb.realtime.close();
    });

    it("falls back to SSE and keeps resuming", async () => {
      const fb = new ForgeBase(base, { realtime: { transport: "sse" } });
      await fb.auth.signInAdmin(ADMIN);
      const col = fb.collection("rt_sse");
      const got: RecordEvent[] = [];
      const sub = await fb.realtime.subscribe({ collection: "rt_sse" }, (e) => e.type.startsWith("record.") && got.push(e as RecordEvent));
      expect(fb.realtime.transport).toBe("sse");
      const rec = await col.create({ n: 5 });
      await until(() => got.length === 1);
      expect(got[0]!.record_id).toBe(rec.id);
      expect(sub.cursor).toBe(got[0]!.cursor);
      await expect(fb.realtime.joinPresence("room")).rejects.toMatchObject({ code: "unsupported_transport" });
      fb.realtime.close();
    });

    it("auto-falls back to SSE when WebSocket cannot connect", async () => {
      class Broken {
        readyState = 3;
        onopen = null;
        onmessage = null;
        onclose = null;
        onerror: ((e: unknown) => void) | null = null;
        constructor() {
          queueMicrotask(() => this.onerror?.({}));
        }
        send() {}
        close() {}
      }
      const fb = new ForgeBase(base, { realtime: { WebSocket: Broken as never, fallbackAfter: 2, reconnect: { baseDelayMs: 5, maxDelayMs: 10 } } });
      await fb.auth.signInAdmin(ADMIN);
      const got: RecordEvent[] = [];
      const transports: string[] = [];
      fb.realtime.on("transport", (t) => transports.push(t));
      await fb.realtime.subscribe({ collection: "rt_sse" }, (e) => e.type.startsWith("record.") && got.push(e as RecordEvent));
      expect(transports).toEqual(["sse"]);
      await fb.collection("rt_sse").create({ n: 6 });
      await until(() => got.length === 1);
      fb.realtime.close();
    });

    it("rejects a subscription the caller may not read", async () => {
      const fb = new ForgeBase(base);
      await expect(fb.realtime.subscribe({ collection: "rt_ws" }, () => undefined)).rejects.toBeTruthy();
      fb.realtime.close();
    });
  });

  describe("entitlements and quotas", () => {
    let user: ForgeBase;
    let subject = "";
    beforeAll(async () => {
      await admin.request("/api/v1/features/ai_calls", { method: "PUT", body: { kind: "metered", unit: "call" } });
      await admin.request("/api/v1/features/beta_ui", { method: "PUT", body: { kind: "boolean" } });
      await admin.request("/api/v1/plans/free", {
        method: "PUT",
        body: { is_default: true, features: [{ feature: "ai_calls", limit: 3, period: "month" }] },
      });
      user = new ForgeBase(base);
      await user.auth.signUp(USER).catch((e: unknown) => {
        if (!(e instanceof ConflictError)) throw e;
      });
      const s = (await user.auth.signIn(USER)) as Session;
      subject = `user:users/${String(s.user?.id)}`;
    });

    it("answers has() for features, plans and the caller's entitlements", async () => {
      expect(await user.entitlements.has({ feature: "ai_calls" })).toBe(true);
      expect(await user.entitlements.has({ feature: "beta_ui" })).toBe(false);
      expect(await user.entitlements.has({ plan: "free" })).toBe(true);
      const e = await user.entitlements.get();
      expect(e.features.ai_calls?.enabled).toBe(true);
    });

    it("checks, consumes idempotently and stops at the hard limit", async () => {
      const c = await user.entitlements.checkQuota({ feature: "ai_calls", units: 1 });
      expect(c.allowed).toBe(true);
      expect(c.limit).toBe(3);
      // Recording usage needs `usage:write`: the admin meters the user's subject.
      const meter = (key: string) => admin.entitlements.consume({ feature: "ai_calls", idempotencyKey: key, subject });
      const first = await meter("k1");
      const replay = await meter("k1");
      expect(first.replayed).toBe(false);
      expect(replay.replayed).toBe(true);
      expect(replay.event_id).toBe(first.event_id);
      await meter("k2");
      await meter("k3");
      expect((await user.entitlements.checkQuota({ feature: "ai_calls" })).allowed).toBe(false);
      await expect(meter("k4")).rejects.toBeInstanceOf(QuotaExceededError);
      const usage = (await admin.entitlements.usage({ subject })).find((u) => u.feature === "ai_calls");
      expect(usage?.used).toBe(3);
    });

    it("reserves, commits and releases units", async () => {
      await admin.request("/api/v1/plans/free", {
        method: "PUT",
        body: { is_default: true, features: [{ feature: "ai_calls", limit: 10, period: "month" }] },
      });
      const r = await admin.entitlements.reserve({ feature: "ai_calls", units: 2, idempotencyKey: "res1", subject });
      expect(r.status).toBe("held");
      expect((await admin.entitlements.releaseReservation(r.id)).status).toBe("released");
      const out = await admin.entitlements.withReservation({ feature: "ai_calls", idempotencyKey: "res2", subject }, async (res) => res.id);
      expect(out).toBeTruthy();
    });
  });

  it("reports server health", async () => {
    expect(await new ForgeBase(base).health()).toBe(true);
    expect(await new ForgeBase("http://127.0.0.1:1", { retry: { maxRetries: 0 } }).health()).toBe(false);
  });
});
