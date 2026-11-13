import { describe, expect, it } from "vitest";
import { AuthManager, ConflictError, NotFoundError, PermissionError, Transport, isMfaChallenge, type Session } from "../src/index";
import { IdentityApi } from "../src/identity";
import { errBody, json, scripted } from "./helpers";

const wireSession = { token_type: "Bearer", access_token: "at1", expires_in: 900, refresh_token: "rt1", session_id: "s1", session_expires_at: "2027-01-01T00:00:00Z", subject: { kind: "user", id: "u1" } };

function setup(...replies: Parameters<typeof scripted>) {
  const f = scripted(...replies);
  const t = new Transport({ baseUrl: "http://x", fetch: f.fetch, retry: { maxRetries: 0 } });
  const mgr = new AuthManager(t, { apiKey: "fb_key" });
  t.credentials = mgr;
  return { f, mgr, id: new IdentityApi(t, mgr) };
}
const body = (f: ReturnType<typeof scripted>, i = 0) => JSON.parse(f.calls[i]!.body!);

describe("identity machine credentials", () => {
  it("lists and revokes API keys and tokens", async () => {
    const key = { id: "k1", kind: "api_key", name: "ci", owner_type: "service", owner_id: "sa1", scopes: ["records:read"], created_at: "t", revoked: false };
    const { f, id } = setup(json(200, { items: [key] }), json(204, null), json(200, { items: [{ ...key, kind: "pat" }] }), json(204, null));
    expect(await id.apiKeys.list()).toEqual([key]);
    await id.apiKeys.revoke("k/1");
    expect(await id.tokens.list()).toHaveLength(1);
    await id.tokens.revoke("t1");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/auth/api-keys",
      "DELETE http://x/api/auth/api-keys/k%2F1",
      "GET http://x/api/auth/tokens",
      "DELETE http://x/api/auth/tokens/t1",
    ]);
    expect(f.calls[0]!.headers["X-API-Key"]).toBe("fb_key");
  });

  it("manages service accounts and their keys", async () => {
    const sa = { id: "sa1", name: "ci", description: "", role: "service", created_by: "admin:1", created_at: "t", disabled: false, realm: "default" };
    const { f, id } = setup(json(200, { items: [sa] }), json(201, sa), json(200, sa), json(204, null), json(200, { items: [] }), json(201, { id: "k1", key: "fb_secret", name: "deploy", scopes: ["records:write"], tenant: "prod" }));
    expect(await id.serviceAccounts.list()).toEqual([sa]);
    await id.serviceAccounts.create({ name: "ci", description: "builds", role: "service" });
    await id.serviceAccounts.get("sa1");
    await id.serviceAccounts.delete("sa1");
    expect(await id.serviceAccounts.keys.list("sa1")).toEqual([]);
    const k = await id.serviceAccounts.keys.create("sa1", { name: "deploy", scopes: ["records:write"], expiresInDays: 30, tenant: "prod" });
    expect(k.key).toBe("fb_secret");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/auth/service-accounts",
      "POST http://x/api/auth/service-accounts",
      "GET http://x/api/auth/service-accounts/sa1",
      "DELETE http://x/api/auth/service-accounts/sa1",
      "GET http://x/api/auth/service-accounts/sa1/keys",
      "POST http://x/api/auth/service-accounts/sa1/keys",
    ]);
    expect(body(f, 1)).toEqual({ name: "ci", description: "builds", role: "service" });
    expect(body(f, 5)).toEqual({ name: "deploy", scopes: ["records:write"], expires_in_days: 30, tenant: "prod" });
  });

  it("lists and creates administrators", async () => {
    const { f, id } = setup(json(200, { items: [{ id: "a1", email: "a@b.c", created_at: "t" }] }), json(201, { id: "a2", email: "d@e.f", created_at: "t" }));
    expect(await id.admins.list()).toHaveLength(1);
    expect((await id.admins.create({ email: "d@e.f", password: "pw" })).id).toBe("a2");
    expect(body(f, 1)).toEqual({ email: "d@e.f", password: "pw" });
  });
});

describe("identity OAuth / OIDC", () => {
  const client = { client_id: "c1", realm: "default", name: "App", type: "confidential", redirect_uris: ["https://app/cb"], post_logout_redirect_uris: [], grant_types: ["authorization_code"], scopes: ["openid"], skip_consent: false, disabled: false, created_at: "t", updated_at: "t" };

  it("covers the client lifecycle with camelCase mapped to the wire", async () => {
    const { f, id } = setup(
      json(200, { items: [client] }),
      json(201, { client, client_secret: "s3cret" }),
      json(200, client),
      json(200, { ...client, disabled: true }),
      json(200, { client, client_secret: "n3w" }),
      json(204, null),
    );
    expect(await id.clients.list({ realm: "acme" })).toEqual([client]);
    const created = await id.clients.create({ name: "App", realm: "acme", type: "public", redirectUris: ["https://app/cb"], skipConsent: true, accessTokenTtl: 600 });
    expect(created.client_secret).toBe("s3cret");
    await id.clients.get("c1");
    expect((await id.clients.update("c1", { disabled: true, redirectUris: ["https://b/cb"] })).disabled).toBe(true);
    expect((await id.clients.rotateSecret("c1", { graceSeconds: 3600 })).client_secret).toBe("n3w");
    await id.clients.delete("c1");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/auth/clients?realm=acme",
      "POST http://x/api/auth/clients",
      "GET http://x/api/auth/clients/c1",
      "PATCH http://x/api/auth/clients/c1",
      "POST http://x/api/auth/clients/c1/secret",
      "DELETE http://x/api/auth/clients/c1",
    ]);
    expect(body(f, 1)).toEqual({ realm: "acme", name: "App", type: "public", redirect_uris: ["https://app/cb"], skip_consent: true, access_token_ttl: 600 });
    expect(body(f, 3)).toEqual({ disabled: true, redirect_uris: ["https://b/cb"] });
    expect(body(f, 4)).toEqual({ grace_seconds: 3600 });
  });

  it("lists and revokes own consents; discovery helpers", async () => {
    const { f, id } = setup(json(200, { items: [{ id: "cn1", client_id: "c1", client_name: "App", realm: "default", scopes: ["openid"], created_at: "t", updated_at: "t" }] }), json(204, null), json(200, { issuer: "http://x/api/auth/oidc/acme" }), json(200, { keys: [] }));
    expect(await id.consents.list()).toHaveLength(1);
    await id.consents.revoke("cn1");
    expect(id.oidc.issuer("acme")).toBe("http://x/api/auth/oidc/acme");
    expect(id.oidc.discoveryUrl("acme")).toBe("http://x/api/auth/oidc/acme/.well-known/openid-configuration");
    expect((await id.oidc.discovery("acme")).issuer).toBe("http://x/api/auth/oidc/acme");
    expect(await id.oidc.jwks("acme")).toEqual({ keys: [] });
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/auth/consents",
      "DELETE http://x/api/auth/consents/cn1",
      "GET http://x/api/auth/oidc/acme/.well-known/openid-configuration",
      "GET http://x/api/auth/oidc/acme/jwks",
    ]);
    expect(f.calls[2]!.headers["X-API-Key"]).toBeUndefined();
  });
});

describe("identity realms", () => {
  const realm = { realm: "acme", collection: "acme_users", config: { signup: true, methods: { password: true } }, created_at: "t", updated_at: "t" };

  it("reads, creates and updates realms", async () => {
    const { f, id } = setup(json(200, { realm: "acme", methods: {}, oauth_providers: [] }), json(200, { items: [realm] }), json(200, realm), json(201, realm), json(200, realm));
    expect((await id.realms.info()).realm).toBe("acme");
    expect(await id.realms.list()).toEqual([realm]);
    await id.realms.get("acme");
    await id.realms.create({ id: "acme", name: "Acme", config: { signup: false } });
    await id.realms.update("acme", { config: { signup: true } });
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/auth/realm",
      "GET http://x/api/auth/realms",
      "GET http://x/api/auth/realms/acme",
      "POST http://x/api/auth/realms",
      "PUT http://x/api/auth/realms/acme",
    ]);
    expect(f.calls[0]!.headers["X-API-Key"]).toBeUndefined();
    expect(body(f, 3)).toEqual({ id: "acme", name: "Acme", config: { signup: false } });
    expect(body(f, 4)).toEqual({ config: { signup: true } });
  });

  it("keys, consents, providers and sms", async () => {
    const { f, id } = setup(
      json(200, { realm: "acme", current: "k1", items: [] }),
      json(200, { items: [], limit: 20, offset: 40 }),
      json(204, null),
      json(200, { items: [{ id: "google", realm: "acme", kind: "google" }] }),
      json(200, { id: "google", realm: "acme", kind: "google" }),
      json(200, { id: "google", realm: "acme", kind: "google" }),
      json(204, null),
      json(200, { configured: false }),
      json(200, { configured: true, provider: "dev" }),
      json(204, null),
    );
    expect((await id.realms.keys("acme")).current).toBe("k1");
    expect(await id.realms.consents("acme", { clientId: "c 1", limit: 20, offset: 40 })).toMatchObject({ limit: 20, offset: 40 });
    await id.realms.revokeConsent("acme", "cn1");
    expect(await id.realms.providers.list("acme")).toHaveLength(1);
    await id.realms.providers.get("acme", "google");
    await id.realms.providers.put("acme", "google", { kind: "google", client_id: "x", client_secret: "y" });
    await id.realms.providers.delete("acme", "google");
    expect((await id.realms.sms.get("acme")).configured).toBe(false);
    await id.realms.sms.put("acme", { provider: "dev" });
    await id.realms.sms.delete("acme");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/auth/realms/acme/keys",
      "GET http://x/api/auth/realms/acme/consents?client_id=c+1&limit=20&offset=40",
      "DELETE http://x/api/auth/realms/acme/consents/cn1",
      "GET http://x/api/auth/realms/acme/oauth-providers",
      "GET http://x/api/auth/realms/acme/oauth-providers/google",
      "PUT http://x/api/auth/realms/acme/oauth-providers/google",
      "DELETE http://x/api/auth/realms/acme/oauth-providers/google",
      "GET http://x/api/auth/realms/acme/sms",
      "PUT http://x/api/auth/realms/acme/sms",
      "DELETE http://x/api/auth/realms/acme/sms",
    ]);
    expect(body(f, 5)).toEqual({ kind: "google", client_id: "x", client_secret: "y" });
    expect(body(f, 8)).toEqual({ provider: "dev" });
  });
});

describe("identity users, impersonation, audit", () => {
  const status = { status: "suspended", reason: "abuse", until: "2027-01-01T00:00:00Z" };

  it("drives the user lifecycle", async () => {
    const { f, id } = setup(
      json(200, { id: "u1" }), json(200, status), json(200, status), json(200, { status: "active" }), json(200, { status: "deactivated" }), json(200, { status: "active" }),
      json(200, { status: "anonymised" }), json(204, null), json(200, { user: {} }), json(200, { items: [{ id: "s1" }] }), json(200, { revoked: 3 }), json(200, { items: [{ ip: "1.2.3.4" }] }),
    );
    await id.users.get("users", "u 1");
    expect((await id.users.status("users", "u1")).status).toBe("suspended");
    await id.users.suspend("users", "u1", { reason: "abuse", until: new Date(1_800_000_000_000) });
    await id.users.unsuspend("users", "u1");
    await id.users.deactivate("users", "u1", { reason: "request" });
    await id.users.reactivate("users", "u1");
    await id.users.anonymise("users", "u1");
    await id.users.delete("users", "u1");
    await id.users.export("users", "u1");
    expect(await id.users.sessions("users", "u1")).toHaveLength(1);
    expect(await id.users.revokeSessions("users", "u1")).toBe(3);
    expect(await id.users.loginHistory("users", "u1", { limit: 10 })).toHaveLength(1);
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/auth/users/users/u%201",
      "GET http://x/api/auth/users/users/u1/status",
      "POST http://x/api/auth/users/users/u1/suspend",
      "POST http://x/api/auth/users/users/u1/unsuspend",
      "POST http://x/api/auth/users/users/u1/deactivate",
      "POST http://x/api/auth/users/users/u1/reactivate",
      "POST http://x/api/auth/users/users/u1/anonymise",
      "POST http://x/api/auth/users/users/u1/delete",
      "GET http://x/api/auth/users/users/u1/export",
      "GET http://x/api/auth/users/users/u1/sessions",
      "POST http://x/api/auth/users/users/u1/sessions/revoke",
      "GET http://x/api/auth/users/users/u1/login-history?limit=10",
    ]);
    expect(body(f, 2)).toEqual({ reason: "abuse", until: 1_800_000_000 });
    expect(body(f, 4)).toEqual({ reason: "request" });
  });

  it("starts, lists and ends impersonation", async () => {
    const grant = { token_type: "Bearer", access_token: "imp", expires_in: 900, expires_at: "t", session_id: "s9", impersonator: "admin:1", subject: {}, reason: "ticket 4" };
    const { f, id } = setup(json(200, grant), json(200, { items: [{ session_id: "s9", active: true }] }), json(204, null));
    expect((await id.impersonation.start({ principal: "user:users/u1", reason: "ticket 4", ttlMinutes: 5 })).access_token).toBe("imp");
    expect(await id.impersonation.list()).toHaveLength(1);
    await id.impersonation.end("s9");
    expect(body(f, 0)).toEqual({ principal: "user:users/u1", reason: "ticket 4", ttl_minutes: 5 });
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "POST http://x/api/auth/impersonate",
      "GET http://x/api/auth/impersonations",
      "POST http://x/api/auth/impersonations/s9/end",
    ]);
  });

  it("audit, login history and key rotation", async () => {
    const { f, id } = setup(json(200, { items: [{ id: 7, event: "login.failed" }] }), json(200, { items: [] }), json(200, { active_kid: "k2" }), json(200, { realm: "acme", kid: "o2" }), json(200, { realm: "default", kid: "o3" }));
    expect(await id.audit({ event: "login.failed", subjectId: "u1", before: 100, limit: 5 })).toHaveLength(1);
    expect(await id.loginHistory()).toEqual([]);
    expect(await id.rotateSigningKey()).toBe("k2");
    expect(await id.rotateOidcKey({ realm: "acme" })).toEqual({ realm: "acme", kid: "o2" });
    await id.rotateOidcKey();
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "GET http://x/api/auth/audit?event=login.failed&subject_id=u1&before=100&limit=5",
      "GET http://x/api/auth/login-history",
      "POST http://x/api/auth/keys/rotate",
      "POST http://x/api/auth/keys/oidc/rotate",
      "POST http://x/api/auth/keys/oidc/rotate",
    ]);
    expect(body(f, 3)).toEqual({ realm: "acme" });
    expect(body(f, 4)).toEqual({});
  });
});

describe("identity account security", () => {
  it("TOTP and recovery codes", async () => {
    const { f, id } = setup(
      json(200, { secret: "BASE32", otpauth_url: "otpauth://totp/x" }),
      json(200, { enabled: true, recovery_codes: ["a", "b"] }),
      json(200, { enabled: false }),
      json(200, { recovery_codes: ["c"] }),
    );
    expect((await id.mfa.setupTotp()).secret).toBe("BASE32");
    expect((await id.mfa.confirmTotp("123456")).recovery_codes).toEqual(["a", "b"]);
    expect((await id.mfa.disableTotp("123456")).enabled).toBe(false);
    expect(await id.mfa.recoveryCodes("654321")).toEqual(["c"]);
    expect(f.calls.map((c) => c.url)).toEqual([
      "http://x/api/auth/mfa/totp/setup",
      "http://x/api/auth/mfa/totp/confirm",
      "http://x/api/auth/mfa/totp/disable",
      "http://x/api/auth/mfa/recovery-codes",
    ]);
    expect(body(f, 1)).toEqual({ code: "123456" });
  });

  it("phone: code, sign-in (stores the session), link, unlink", async () => {
    const { f, mgr, id } = setup(
      json(202, { status: "accepted" }), json(200, wireSession), json(200, { phone: "+4915112345678", verified_at: "t" }), json(202, { status: "code_sent" }), json(200, { phone: "+4915112345678" }), json(204, null),
    );
    await id.phone.requestCode("+4915112345678", { collection: "users" });
    const s = (await id.phone.signIn({ phone: "+4915112345678", code: "111222", deviceName: "cli" })) as Session;
    expect(s.accessToken).toBe("at1");
    expect((await mgr.getSession())?.sessionId).toBe("s1");
    expect((await id.phone.get()).phone).toBe("+4915112345678");
    await id.phone.add("+4915112345678");
    await id.phone.confirm("+4915112345678", "999888");
    await id.phone.remove();
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "POST http://x/api/auth/phone/request",
      "POST http://x/api/auth/phone/verify",
      "GET http://x/api/auth/phone",
      "POST http://x/api/auth/phone/add",
      "POST http://x/api/auth/phone/confirm",
      "DELETE http://x/api/auth/phone",
    ]);
    expect(body(f, 0)).toEqual({ phone: "+4915112345678", collection: "users" });
    expect(body(f, 1)).toEqual({ phone: "+4915112345678", code: "111222", device_name: "cli" });
    expect(f.calls[0]!.headers["X-API-Key"]).toBeUndefined();
    expect(f.calls[2]!.headers["X-API-Key"]).toBe("fb_key");
  });

  it("passkeys: register, sign in, second factor, list, delete", async () => {
    const opts = { ceremony_id: "cer1", publicKey: { challenge: "abc" } };
    const cred = { id: "cred1", name: "mac", user_verified: true, use_as_mfa: false, backup_eligible: true, sign_count: 0, created_at: "t" };
    const { f, id } = setup(
      json(200, opts), json(201, { credential: cred }), json(200, opts), json(200, wireSession), json(200, opts),
      json(200, { mfa_required: true, mfa_token: "m2", expires_in: 300 }), json(200, { credentials: [cred] }), json(204, null),
    );
    expect((await id.webauthn.registerOptions({ useAsMfa: true })).ceremony_id).toBe("cer1");
    expect((await id.webauthn.registerVerify({ ceremonyId: "cer1", credential: { id: "x" }, name: "mac" })).id).toBe("cred1");
    await id.webauthn.signInOptions({ email: "a@b.c" });
    const s = await id.webauthn.signIn({ ceremonyId: "cer1", credential: { id: "x" }, cookie: false });
    expect(isMfaChallenge(s)).toBe(false);
    await id.webauthn.mfaOptions("m1");
    const m = await id.webauthn.verifyMfa({ mfaToken: "m1", ceremonyId: "cer2", credential: { id: "y" } });
    expect(isMfaChallenge(m) && m.mfaToken).toBe("m2");
    expect(await id.webauthn.credentials()).toEqual([cred]);
    await id.webauthn.deleteCredential("cred1");
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "POST http://x/api/auth/webauthn/register/options",
      "POST http://x/api/auth/webauthn/register/verify",
      "POST http://x/api/auth/webauthn/login/options",
      "POST http://x/api/auth/webauthn/login/verify",
      "POST http://x/api/auth/webauthn/mfa/options",
      "POST http://x/api/auth/webauthn/mfa/verify",
      "GET http://x/api/auth/webauthn/credentials",
      "DELETE http://x/api/auth/webauthn/credentials/cred1",
    ]);
    expect(body(f, 0)).toEqual({ use_as_mfa: true });
    expect(body(f, 1)).toEqual({ ceremony_id: "cer1", credential: { id: "x" }, name: "mac" });
    expect(body(f, 5)).toEqual({ mfa_token: "m1", ceremony_id: "cer2", credential: { id: "y" } });
  });

  it("ldap sign-in and the account lifecycle", async () => {
    const { f, id } = setup(json(200, wireSession), json(200, { status: "deactivated" }), json(200, { status: "deleted" }), json(200, { user: { id: "u1" } }));
    expect(((await id.ldap.signIn({ username: "jo", password: "pw" })) as Session).sessionId).toBe("s1");
    await id.account.deactivate({ currentPassword: "pw" });
    await id.account.delete({ currentPassword: "pw", anonymise: true });
    await id.account.export();
    expect(f.calls.map((c) => `${c.method} ${c.url}`)).toEqual([
      "POST http://x/api/auth/ldap/login",
      "POST http://x/api/auth/account/deactivate",
      "POST http://x/api/auth/account/delete",
      "GET http://x/api/auth/account/export",
    ]);
    expect(body(f, 0)).toEqual({ username: "jo", password: "pw" });
    expect(body(f, 1)).toEqual({ current_password: "pw" });
    expect(body(f, 2)).toEqual({ current_password: "pw", anonymise: true });
  });
});

describe("identity errors", () => {
  it("maps status codes to typed errors", async () => {
    const { id } = setup(json(403, errBody("forbidden", "admin only")), json(404, errBody("not_found")), json(409, errBody("conflict", "taken")));
    await expect(id.admins.list()).rejects.toBeInstanceOf(PermissionError);
    await expect(id.serviceAccounts.get("nope")).rejects.toBeInstanceOf(NotFoundError);
    await expect(id.realms.create({ id: "acme" })).rejects.toBeInstanceOf(ConflictError);
  });
});
