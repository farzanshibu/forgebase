import { describe, expect, it } from "vitest";
import { ForgeBase, isMfaChallenge, memoryStorage, type Session } from "../src/index";
import { errBody, json, scripted } from "./helpers";

const wireSession = (n: number, extra: object = {}) => ({
  token_type: "Bearer", access_token: `at${n}`, expires_in: 900, refresh_token: `rt${n}`, session_id: "s1",
  session_expires_at: "2027-01-01T00:00:00Z", subject: { kind: "user", id: "u1" }, ...extra,
});
const retry = { sleep: () => Promise.resolve(), random: () => 0 };

describe("auth", () => {
  it("signs in with a password and sends the bearer token afterwards", async () => {
    let clock = 1_000;
    const f = scripted(json(200, wireSession(1, { user: { id: "u1" } })), json(200, { ok: true }));
    const fb = new ForgeBase("http://x", { fetch: f.fetch, retry, now: () => clock });
    const s = (await fb.auth.signIn({ email: "a@b.c", password: "pw", deviceName: "cli" })) as Session;
    expect(s.accessToken).toBe("at1");
    expect(s.expiresAt).toBe(1_000 + 900_000);
    expect(JSON.parse(f.calls[0]!.body!)).toMatchObject({ email: "a@b.c", password: "pw", device_name: "cli" });
    expect(f.calls[0]!.headers.Authorization).toBeUndefined();
    clock += 1;
    await fb.request("/api/v1/collections");
    expect(f.calls[1]!.headers.Authorization).toBe("Bearer at1");
  });

  it("returns an MFA challenge and completes it", async () => {
    const f = scripted(json(200, { mfa_required: true, mfa_token: "m1", expires_in: 300 }), json(200, wireSession(1)));
    const fb = new ForgeBase("http://x", { fetch: f.fetch, retry });
    const r = await fb.auth.signIn({ email: "a@b.c", password: "pw" });
    expect(isMfaChallenge(r) && r.mfaToken).toBe("m1");
    expect(await fb.auth.getSession()).toBeNull();
    const s = await fb.auth.verifyMfa("m1", "123456");
    expect(s.accessToken).toBe("at1");
    expect(JSON.parse(f.calls[1]!.body!)).toEqual({ mfa_token: "m1", code: "123456" });
  });

  it("refreshes before expiry, once for concurrent requests, and rotates the refresh token", async () => {
    let clock = 0;
    const f = scripted(json(200, wireSession(1)), json(200, wireSession(2)), json(200, { ok: 1 }));
    const fb = new ForgeBase("http://x", { fetch: f.fetch, retry, now: () => clock });
    await fb.auth.signIn({ email: "a", password: "b" });
    clock = 900_000 - 10_000; // inside the 30 s skew
    const seen: string[] = [];
    f.calls.length = 0;
    await Promise.all([fb.request("/a"), fb.request("/b"), fb.request("/c")]);
    const refreshes = f.calls.filter((c) => c.url.endsWith("/api/auth/refresh"));
    expect(refreshes).toHaveLength(1);
    expect(JSON.parse(refreshes[0]!.body!)).toEqual({ refresh_token: "rt1" });
    for (const c of f.calls.filter((c) => !c.url.includes("/auth/"))) seen.push(c.headers.Authorization!);
    expect(seen).toEqual(["Bearer at2", "Bearer at2", "Bearer at2"]);
  });

  it("refreshes and retries once after a 401", async () => {
    const f = scripted(json(200, wireSession(1)), json(401, errBody("invalid_token")), json(200, wireSession(2)), json(200, { who: "me" }));
    const fb = new ForgeBase("http://x", { fetch: f.fetch, retry });
    await fb.auth.signIn({ email: "a", password: "b" });
    expect(await fb.auth.me()).toEqual({ who: "me" });
    expect(f.calls.map((c) => c.url.replace("http://x", ""))).toEqual(["/api/auth/login", "/api/auth/me", "/api/auth/refresh", "/api/auth/me"]);
    expect(f.calls[3]!.headers.Authorization).toBe("Bearer at2");
  });

  it("signs out locally when the refresh token is rejected", async () => {
    const f = scripted(json(200, wireSession(1)), json(401, errBody("invalid_token")), json(401, errBody("invalid_refresh_token")));
    const fb = new ForgeBase("http://x", { fetch: f.fetch, retry });
    const events: string[] = [];
    fb.auth.onChange((e) => events.push(e));
    await fb.auth.signIn({ email: "a", password: "b" });
    await expect(fb.auth.me()).rejects.toMatchObject({ status: 401 });
    expect(await fb.auth.getSession()).toBeNull();
    expect(events).toEqual(["signed_in", "signed_out"]);
  });

  it("sends an API key as X-API-Key and never refreshes", async () => {
    const f = scripted(json(401, errBody("invalid_token")));
    const fb = new ForgeBase("http://x", { fetch: f.fetch, retry, apiKey: "fbk_secret" });
    await expect(fb.request("/api/v1/collections")).rejects.toMatchObject({ status: 401 });
    expect(f.calls).toHaveLength(1);
    expect(f.calls[0]!.headers["X-API-Key"]).toBe("fbk_secret");
  });

  it("persists and restores the session through the storage adapter", async () => {
    const storage = memoryStorage();
    const f = scripted(json(200, wireSession(1)));
    await new ForgeBase("http://x", { fetch: f.fetch, retry, storage }).auth.signIn({ email: "a", password: "b" });
    const g = scripted(json(200, {}));
    const fb2 = new ForgeBase("http://x", { fetch: g.fetch, retry, storage });
    await fb2.request("/x");
    expect(g.calls[0]!.headers.Authorization).toBe("Bearer at1");
  });

  it("signOut revokes server side and clears local state even when the call fails", async () => {
    const f = scripted(json(200, wireSession(1)), json(500, errBody("internal")));
    const fb = new ForgeBase("http://x", { fetch: f.fetch, retry: { ...retry, maxRetries: 0 } });
    await fb.auth.signIn({ email: "a", password: "b" });
    await expect(fb.auth.signOut()).rejects.toMatchObject({ status: 500 });
    expect(await fb.auth.getSession()).toBeNull();
  });

  it("cookie mode: includes credentials and sends the CSRF header on writes only", async () => {
    const seen: { credentials?: RequestCredentials }[] = [];
    const f = scripted(
      json(200, { token_type: "Bearer", expires_in: 900, session_id: "s", csrf_token: "c1", session_expires_at: "x", subject: {} }),
      json(200, {}),
      json(200, {}),
    );
    const fb = new ForgeBase("http://x", { fetch: (u, i) => (seen.push(i ?? {}), f.fetch(u, i)), retry });
    await fb.auth.signIn({ email: "a", password: "b", cookie: true });
    await fb.request("/r");
    await fb.request("/w", { method: "POST", body: {} });
    expect(f.calls[1]!.headers["X-CSRF-Token"]).toBeUndefined();
    expect(f.calls[2]!.headers["X-CSRF-Token"]).toBe("c1");
    expect(seen[2]!.credentials).toBe("include");
  });

  it("builds OAuth URLs and completes from the redirect", async () => {
    const f = scripted(json(200, { authorization_url: "https://idp/auth?x=1", state: "st" }), json(200, wireSession(1)));
    const fb = new ForgeBase("http://x/", { fetch: f.fetch, retry });
    expect(fb.auth.oauth.redirectUrl("google", { cookie: true, collection: "members" })).toBe("http://x/api/auth/oauth/google/start?collection=members&cookie=true");
    expect(await fb.auth.oauth.start("google")).toEqual({ authorizationUrl: "https://idp/auth?x=1", state: "st" });
    const s = await fb.auth.oauth.completeFromUrl("google", "http://app/cb?code=C&state=st");
    expect(isMfaChallenge(s)).toBe(false);
    expect(f.calls[1]!.url).toBe("http://x/api/auth/oauth/google/callback?code=C&state=st");
    await expect(fb.auth.oauth.completeFromUrl("google", "http://app/cb?error=access_denied")).rejects.toThrow(/access_denied/);
  });

  it("magic link and OTP send the right bodies", async () => {
    const f = scripted(json(202, { status: "accepted" }), json(200, wireSession(1)), json(202, {}), json(200, wireSession(2)));
    const fb = new ForgeBase("http://x", { fetch: f.fetch, retry });
    await fb.auth.requestMagicLink("a@b.c");
    await fb.auth.confirmMagicLink("tok");
    await fb.auth.requestOtp("a@b.c");
    await fb.auth.verifyOtp({ email: "a@b.c", code: "999999" });
    expect(f.calls.map((c) => c.url.replace("http://x/api/auth", ""))).toEqual(["/magic-link/request", "/magic-link/confirm", "/otp/request", "/otp/verify"]);
    expect(JSON.parse(f.calls[3]!.body!)).toMatchObject({ email: "a@b.c", code: "999999" });
  });
});
