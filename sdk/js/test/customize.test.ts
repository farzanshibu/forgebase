import { describe, expect, it } from "vitest";
import { ConflictError, NotFoundError, PermissionError, Transport } from "../src/index";
import { CustomizeApi } from "../src/customize";
import { errBody, json, scripted } from "./helpers";

function setup(...replies: Parameters<typeof scripted>) {
  const f = scripted(...replies);
  const t = new Transport({ baseUrl: "http://x", fetch: f.fetch, retry: { maxRetries: 0 }, headers: { "X-Test": "1" } });
  return { f, c: new CustomizeApi(t) };
}
const body = (f: ReturnType<typeof scripted>, i = 0) => JSON.parse(f.calls[i]!.body!);
const line = (f: ReturnType<typeof scripted>) => f.calls.map((c) => `${c.method} ${c.url}`);

const domain = { id: "d1", realm: "default", host: "login.acme.test", status: "pending", created_at: 1, created_by: "admin:1", verification: { record_name: "_fb.login.acme.test", record_type: "TXT", record_value: "tok", expires_at: 2 } };
const branding = { realm: "default", has_logo: false, has_favicon: false, display_name: "Acme" };

describe("customize.domains", () => {
  it("adds, lists, reads, verifies, reissues and deletes", async () => {
    const { f, c } = setup(
      json(201, domain), json(200, { items: [domain] }), json(200, domain), json(200, { verified: false, reason: "no TXT record", domain }),
      json(200, domain), json(204, null),
    );
    expect((await c.domains.add("login.acme.test", { realm: "acme" })).verification?.record_type).toBe("TXT");
    expect(await c.domains.list({ realm: "acme" })).toEqual([domain]);
    await c.domains.get("d/1");
    expect(await c.domains.verify("d1")).toMatchObject({ verified: false, reason: "no TXT record" });
    await c.domains.reissue("d1");
    await c.domains.delete("d1");
    expect(line(f)).toEqual([
      "POST http://x/api/v1/customize/domains",
      "GET http://x/api/v1/customize/domains?realm=acme",
      "GET http://x/api/v1/customize/domains/d%2F1",
      "POST http://x/api/v1/customize/domains/d1/verify",
      "POST http://x/api/v1/customize/domains/d1/reissue",
      "DELETE http://x/api/v1/customize/domains/d1",
    ]);
    expect(body(f, 0)).toEqual({ host: "login.acme.test", realm: "acme" });
  });

  it("surfaces an expired token as a ConflictError", async () => {
    const { c } = setup(json(409, errBody("token_expired")));
    await expect(c.domains.verify("d1")).rejects.toBeInstanceOf(ConflictError);
  });
});

describe("customize.branding", () => {
  it("gets and replaces the branding", async () => {
    const { f, c } = setup(json(200, branding), json(200, { ...branding, primary_color: "#112233" }));
    expect(await c.branding.get()).toEqual(branding);
    expect((await c.branding.put({ primary_color: "#112233", display_name: "Acme" }, { realm: "acme" })).primary_color).toBe("#112233");
    expect(line(f)).toEqual(["GET http://x/api/v1/customize/branding", "PUT http://x/api/v1/customize/branding?realm=acme"]);
    expect(body(f, 1)).toEqual({ primary_color: "#112233", display_name: "Acme" });
  });

  it("uploads an asset as raw bytes with the given content type, and deletes it", async () => {
    let sent: unknown;
    const raw = new Uint8Array([137, 80, 78, 71]);
    const f = scripted(
      (call) => {
        void call;
        return json(200, { ...branding, has_logo: true });
      },
      json(200, { ...branding, has_logo: true }),
      json(200, branding),
    );
    const t = new Transport({
      baseUrl: "http://x",
      fetch: async (url, init) => {
        sent = init?.body;
        return f.fetch(url, init);
      },
    });
    const c = new CustomizeApi(t);
    expect((await c.branding.uploadAsset("logo", raw)).has_logo).toBe(true);
    expect(sent).toBe(raw);
    expect(f.calls[0]!.headers["Content-Type"]).toBe("image/png");
    await c.branding.uploadAsset("favicon", new Blob([raw], { type: "image/x-icon" }), { realm: "acme" });
    expect(f.calls[1]!.headers["Content-Type"]).toBe("image/x-icon");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/customize/branding/assets/favicon?realm=acme");
    expect((await c.branding.deleteAsset("logo")).has_logo).toBe(false);
    expect(f.calls[2]).toMatchObject({ method: "DELETE", url: "http://x/api/v1/customize/branding/assets/logo" });
  });
});

describe("customize.email", () => {
  const tpl = { key: "magic_link", realm: "default", subject: "Hi {{name}}", text: "Hello", customized: true };
  const delivery = { id: "dl1", realm: "default", to: "a@b.c", subject: "Hi", status: "queued", attempts: 0, created_at: 1, updated_at: 1 };

  it("manages templates", async () => {
    const { f, c } = setup(json(200, { items: [tpl] }), json(200, tpl), json(200, tpl), json(204, null));
    expect(await c.email.templates()).toEqual([tpl]);
    await c.email.template("magic link", { realm: "acme" });
    await c.email.putTemplate("magic_link", { subject: "Hi {{name}}", text: "Hello", html: "<p>Hello</p>" });
    await c.email.deleteTemplate("magic_link");
    expect(line(f)).toEqual([
      "GET http://x/api/v1/customize/email/templates",
      "GET http://x/api/v1/customize/email/templates/magic%20link?realm=acme",
      "PUT http://x/api/v1/customize/email/templates/magic_link",
      "DELETE http://x/api/v1/customize/email/templates/magic_link",
    ]);
    expect(body(f, 2)).toEqual({ subject: "Hi {{name}}", text: "Hello", html: "<p>Hello</p>" });
  });

  it("previews, sends, reads the log and retries", async () => {
    const { f, c } = setup(
      json(200, { subject: "Hi Jo", text: "Hello", missing: [] }), json(202, delivery), json(200, { items: [delivery] }), json(200, delivery), json(202, { ...delivery, status: "retrying" }),
    );
    expect((await c.email.preview({ template: "magic_link", variables: { name: "Jo" } })).subject).toBe("Hi Jo");
    expect((await c.email.send({ template: "magic_link", to: "a@b.c", variables: { name: "Jo" } })).status).toBe("queued");
    expect(await c.email.deliveries({ status: "failed", to: "a@b.c", template: "magic_link", limit: 10, before: 99, realm: "acme" })).toEqual([delivery]);
    await c.email.delivery("dl1");
    expect((await c.email.retry("dl1")).status).toBe("retrying");
    expect(line(f)).toEqual([
      "POST http://x/api/v1/customize/email/preview",
      "POST http://x/api/v1/customize/email/send",
      "GET http://x/api/v1/customize/email/deliveries?realm=acme&status=failed&to=a%40b.c&template=magic_link&limit=10&before=99",
      "GET http://x/api/v1/customize/email/deliveries/dl1",
      "POST http://x/api/v1/customize/email/deliveries/dl1/retry",
    ]);
    expect(body(f, 0)).toEqual({ template: "magic_link", variables: { name: "Jo" } });
    expect(body(f, 1)).toEqual({ template: "magic_link", to: "a@b.c", variables: { name: "Jo" } });
  });

  it("maps a missing template to NotFoundError", async () => {
    const { c } = setup(json(404, errBody("not_found")));
    await expect(c.email.template("nope")).rejects.toBeInstanceOf(NotFoundError);
  });
});

describe("customize.agreements", () => {
  const agreement = { id: "ag1", realm: "default", kind: "terms", version: 2, title: "Terms", body: "text", published_at: 1, published_by: "admin:1" };
  const acceptance = { id: "ac1", realm: "default", principal: "user:users/u1", kind: "terms", version: 2, accepted_at: 3 };

  it("lists, publishes, and reads pending", async () => {
    const { f, c } = setup(json(200, { items: [agreement] }), json(201, agreement), json(200, { pending: [{ kind: "terms", version: 2, title: "Terms" }] }));
    expect(await c.agreements.list({ kind: "terms", realm: "acme" })).toEqual([agreement]);
    expect((await c.agreements.publish({ kind: "terms", title: "Terms", body: "text", url: "https://acme.test/terms" })).version).toBe(2);
    expect(await c.agreements.pending()).toEqual([{ kind: "terms", version: 2, title: "Terms" }]);
    expect(line(f)).toEqual([
      "GET http://x/api/v1/customize/agreements?realm=acme&kind=terms",
      "POST http://x/api/v1/customize/agreements",
      "GET http://x/api/v1/customize/agreements/pending",
    ]);
    expect(body(f, 1)).toEqual({ kind: "terms", title: "Terms", body: "text", url: "https://acme.test/terms" });
  });

  it("accepts, lists own and everyone's acceptances", async () => {
    const { f, c } = setup(json(200, { items: [acceptance] }), json(200, { items: [acceptance] }), json(200, { items: [acceptance] }));
    expect(await c.agreements.accept([{ kind: "terms", version: 2 }])).toEqual([acceptance]);
    expect(await c.agreements.accepted()).toHaveLength(1);
    await c.agreements.acceptances({ principal: "user:users/u1", kind: "terms", limit: 5, realm: "acme" });
    expect(line(f)).toEqual([
      "POST http://x/api/v1/customize/agreements/accept",
      "GET http://x/api/v1/customize/agreements/accepted",
      "GET http://x/api/v1/customize/agreements/acceptances?realm=acme&principal=user%3Ausers%2Fu1&kind=terms&limit=5",
    ]);
    expect(body(f, 0)).toEqual({ items: [{ kind: "terms", version: 2 }] });
  });

  it("an outdated version is a ConflictError; a plain user cannot publish", async () => {
    const { c } = setup(json(409, errBody("agreement_outdated")), json(403, errBody("forbidden")));
    await expect(c.agreements.accept([{ kind: "terms", version: 1 }])).rejects.toBeInstanceOf(ConflictError);
    await expect(c.agreements.publish({ kind: "terms", title: "T", body: "b" })).rejects.toBeInstanceOf(PermissionError);
  });
});

describe("customize public views and docs", () => {
  it("fetches branding and agreements without credentials", async () => {
    const f = scripted(json(200, { realm: "acme", display_name: "Acme", domains: [] }), json(200, { items: [] }));
    const t = new Transport({ baseUrl: "http://x", fetch: f.fetch });
    t.credentials = { headers: async () => ({ Authorization: "Bearer secret" }), onUnauthorized: async () => false };
    const c = new CustomizeApi(t);
    expect((await c.hosted.branding({ realm: "acme" })).display_name).toBe("Acme");
    expect(await c.hosted.agreements()).toEqual([]);
    expect(f.calls.map((x) => x.url)).toEqual(["http://x/api/v1/customize/public/branding?realm=acme", "http://x/api/v1/customize/public/agreements"]);
    expect(f.calls[0]!.headers.Authorization).toBeUndefined();
  });

  it("builds asset and theme URLs, downloads an asset and the theme", async () => {
    const f = scripted(
      () => new Response(new Uint8Array([1, 2, 3]), { status: 200, headers: { "content-type": "image/png" } }),
      () => new Response(":root{--fb-primary:#123}", { status: 200, headers: { "content-type": "text/css" } }),
      json(404, errBody("not_found")),
    );
    const c = new CustomizeApi(new Transport({ baseUrl: "http://x", fetch: f.fetch, retry: { maxRetries: 0 } }));
    expect(c.hosted.assetUrl("logo", { realm: "acme" })).toBe("http://x/api/v1/customize/public/branding/logo?realm=acme");
    expect(c.hosted.themeUrl()).toBe("http://x/api/v1/customize/public/theme.css");
    const blob = await c.hosted.asset("logo");
    expect(blob.size).toBe(3);
    expect(await c.hosted.theme({ realm: "acme" })).toBe(":root{--fb-primary:#123}");
    expect(f.calls[1]!.url).toBe("http://x/api/v1/customize/public/theme.css?realm=acme");
    expect(f.calls[1]!.headers.Accept).toBe("text/css");
    await expect(c.hosted.asset("favicon")).rejects.toBeInstanceOf(NotFoundError);
  });

  it("docs returns the HTML page", async () => {
    const f = scripted(() => new Response("<!doctype html><title>Acme API</title>", { status: 200, headers: { "content-type": "text/html" } }));
    const c = new CustomizeApi(new Transport({ baseUrl: "http://x", fetch: f.fetch }));
    expect(c.docsUrl({ realm: "acme" })).toBe("http://x/api/v1/customize/docs?realm=acme");
    expect(await c.docs()).toContain("Acme API");
    expect(f.calls[0]!.headers.Accept).toBe("text/html");
  });
});

describe("customize.email provider", () => {
  const state = { realm: "default", configured: true, config: { provider: "resend", from_address: "no-reply@acme.test", api_key: "********" }, created_at: 1, updated_at: 2 };

  it("reads the provider with the secrets masked, for a realm or the instance", async () => {
    const { f, c } = setup(json(200, state), json(200, { realm: "*", configured: false, config: {} }));
    expect((await c.email.provider()).config).toMatchObject({ api_key: "********" });
    expect((await c.email.provider({ realm: "*" })).configured).toBe(false);
    expect(line(f)).toEqual(["GET http://x/api/v1/customize/email/provider", "GET http://x/api/v1/customize/email/provider?realm=*"]);
  });

  it("puts the provider: settings are flattened beside provider and from_address", async () => {
    const { f, c } = setup(json(200, state), json(400, errBody("invalid_mail_config", "api_key is required")));
    await c.email.putProvider({ provider: "resend", fromAddress: "no-reply@acme.test", settings: { api_key: "re_123" } }, { realm: "acme" });
    expect(f.calls[0]).toMatchObject({ method: "PUT", url: "http://x/api/v1/customize/email/provider?realm=acme" });
    expect(body(f)).toEqual({ api_key: "re_123", provider: "resend", from_address: "no-reply@acme.test" });
    await expect(c.email.putProvider({ provider: "resend" })).rejects.toMatchObject({ name: "ValidationError", code: "invalid_mail_config" });
    expect(body(f, 1)).toEqual({ provider: "resend" });
  });

  it("deletes the provider; a realm without one is a NotFoundError", async () => {
    const { f, c } = setup(json(204, null), json(404, errBody("not_found", "no provider")));
    expect(await c.email.deleteProvider({ realm: "acme" })).toBeUndefined();
    await expect(c.email.deleteProvider()).rejects.toBeInstanceOf(NotFoundError);
    expect(f.calls[0]).toMatchObject({ method: "DELETE", url: "http://x/api/v1/customize/email/provider?realm=acme" });
  });

  it("sends a test message; a provider failure is a ConflictError (mail_test_failed)", async () => {
    const { f, c } = setup(json(200, { provider: "resend", from: "no-reply@acme.test", message_id: "m1" }), json(409, errBody("mail_test_failed", "401 from resend")));
    expect(await c.email.testProvider("me@acme.test")).toMatchObject({ provider: "resend", message_id: "m1" });
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/customize/email/provider/test" });
    expect(body(f)).toEqual({ to: "me@acme.test" });
    const err = await c.email.testProvider("me@acme.test").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ConflictError);
    expect(err).toMatchObject({ code: "mail_test_failed" });
  });
});
