import { describe, expect, it } from "vitest";
import { CommitTokens, ForgeBase } from "../src/index";
import { json, noSleep, scripted } from "./helpers";

describe("commit tokens", () => {
  it("keep the highest position per set and ignore junk", () => {
    const t = new CommitTokens();
    expect(t.header()).toBeUndefined();
    t.note("control/0/20, s1/1/0, junk, control/0/1F, /0/1, s2/x/1, s3/0/1/2");
    t.note(null);
    t.note("control/0/10");
    expect(t.get("control")).toBe("0/20");
    expect(t.get("s1")).toBe("1/0");
    expect(t.header()).toBe("control/0/20, s1/1/0");
    t.note("control/1/0");
    expect(t.header()).toBe("control/1/0, s1/1/0");
    t.note("control/a/ff");
    expect(t.get("control")).toBe("A/FF");
    t.clear();
    expect(t.header()).toBeUndefined();
  });

  it("compare LSNs numerically, not as text", () => {
    const t = new CommitTokens();
    t.note("control/9/FFFFFFFF");
    t.note("control/10/0");
    expect(t.get("control")).toBe("10/0");
  });
});

describe("read-your-writes headers", () => {
  it("send nothing extra before the first commit token", async () => {
    const f = scripted(json(200, {}));
    const fb = new ForgeBase("http://x.test", { fetch: f.fetch, retry: noSleep });
    await fb.request("/api/v1/x");
    expect(f.calls[0]!.headers["X-ForgeBase-Read-After"]).toBeUndefined();
    expect(f.calls[0]!.headers["X-ForgeBase-Consistency"]).toBeUndefined();
  });

  it("track X-ForgeBase-Commit and send it back as X-ForgeBase-Read-After", async () => {
    const f = scripted(
      json(201, { id: "a" }, { "x-forgebase-commit": "control/0/1A" }),
      json(409, { error: { code: "conflict", message: "x" } }, { "x-forgebase-commit": "s1/0/5" }),
      json(200, { items: [], per_page: 30 }),
    );
    const fb = new ForgeBase("http://x.test", { fetch: f.fetch, retry: noSleep });
    await fb.collection("posts").create({ title: "t" });
    await fb.collection("posts").create({ title: "t" }).catch(() => undefined);
    await fb.collection("posts").list();
    expect(f.calls[1]!.headers["X-ForgeBase-Read-After"]).toBe("control/0/1A");
    expect(f.calls[2]!.headers["X-ForgeBase-Read-After"]).toBe("control/0/1A, s1/0/5");
    expect(fb.commitTokens.get("control")).toBe("0/1A");
    // Realtime positions feed the same tokens.
    fb.commitTokens.note("control/0/2B");
    expect(fb.commitTokens.header()).toBe("control/0/2B, s1/0/5");
  });

  it("send X-ForgeBase-Consistency: strong per client or per call", async () => {
    const f = scripted(json(200, { items: [], per_page: 30 }));
    const strong = new ForgeBase("http://x.test", { fetch: f.fetch, retry: noSleep, consistency: "strong" });
    await strong.collection("posts").list();
    expect(f.calls[0]!.headers["X-ForgeBase-Consistency"]).toBe("strong");
    const fb = new ForgeBase("http://x.test", { fetch: f.fetch, retry: noSleep });
    await fb.collection("posts").list();
    await fb.collection("posts").list({ consistency: "strong" });
    await fb.collection("posts").get("a", { consistency: "strong" });
    await fb.request("/api/v1/x", { consistency: "strong" });
    expect(f.calls[1]!.headers["X-ForgeBase-Consistency"]).toBeUndefined();
    expect(f.calls.slice(2).map((c) => c.headers["X-ForgeBase-Consistency"])).toEqual(["strong", "strong", "strong"]);
  });
});
