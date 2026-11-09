/** In-memory fakes of the identity provider, realm identity and customize REST for Identity Studio tests (plain handlers: these routes are not in the OpenAPI fixture). */
import type { Agreement, Acceptance, Branding, ClientApp, ConsentGrant, Delivery, Domain, EmailTemplate, RealmIdentity } from "../api";
import { envelope, type MockApi } from "./mockApi";

const ISO = "2026-11-09T10:00:00Z";
const MS = Date.parse(ISO);

export interface IdState {
  realms: RealmIdentity[];
  clients: ClientApp[];
  consents: ConsentGrant[];
  domains: Domain[];
  branding: Branding;
  templates: EmailTemplate[];
  deliveries: Delivery[];
  agreements: Agreement[];
  acceptances: Acceptance[];
  secrets: string[];
  keyRotations: string[];
  verifyResult: { verified: boolean; reason: string | null };
}

const cfg = (over: Partial<RealmIdentity["config"]> = {}): RealmIdentity["config"] => ({
  signup: true, methods: { password: true, magic_link: true, email_otp: true, oauth: true, passkey: true, phone: false, ldap: false },
  require_verified_email: null, phone: { signup: true }, ldap: null, turnstile: null, ...over,
});

export function installIdentity(mock: MockApi): IdState {
  const st: IdState = {
    realms: [
      { realm: "default", collection: "users", config: cfg(), created_at: ISO, updated_at: ISO },
      { realm: "acme", collection: "acme_users", config: cfg({ turnstile: { enabled: true, site_key: "0xKEY", secret: "********", actions: ["signup"], hostnames: [], verify_url: null } }), created_at: ISO, updated_at: ISO },
    ],
    clients: [{
      client_id: "app_web", realm: "default", name: "Web app", description: null, type: "confidential", redirect_uris: ["https://app.example.com/cb"], post_logout_redirect_uris: [],
      grant_types: ["authorization_code", "refresh_token"], scopes: ["openid", "profile"], skip_consent: false, access_token_ttl: null, refresh_token_ttl: null, disabled: false,
      secret_created_at: ISO, previous_secret_expires_at: null, created_at: ISO, updated_at: ISO,
    }],
    consents: [
      { id: "con1", client_id: "app_web", client_name: "Web app", realm: "default", scopes: ["openid", "email"], created_at: ISO, updated_at: ISO },
      { id: "con2", client_id: "other", client_name: "Other realm app", realm: "acme", scopes: ["openid"], created_at: ISO, updated_at: ISO },
    ],
    domains: [{
      id: "dom1", realm: "default", host: "login.example.com", status: "pending", created_at: MS, verified_at: null, last_checked: null, last_error: null, created_by: "admin",
      verification: { record_name: "_forgebase-verify", record_type: "TXT", record_value: "forgebase-verify=tok123", expires_at: MS + 86_400_000 },
    }],
    branding: { realm: "default", display_name: "Acme", primary_color: "#ff8800", background_color: null, text_color: null, accent_color: null, signin_title: null, signin_subtitle: null, signin_footer: null, support_email: null, has_logo: false, has_favicon: false, updated_at: null },
    templates: [
      { realm: "default", key: "password_reset", subject: "Reset your password", text: "Hi {{ user.name }}, use {{ reset_url }}", html: null, customized: false, variables: ["user.name", "reset_url"], updated_at: null },
      { realm: "default", key: "welcome", subject: "Welcome", text: "Welcome {{ name }}", html: null, customized: true, variables: ["name"], updated_at: MS },
    ],
    deliveries: [
      { id: "del1", realm: "default", template: "welcome", to: "a@example.com", subject: "Welcome", status: "failed", attempts: 3, last_error: "smtp 550 mailbox full", provider: "smtp", job_id: null, created_at: MS, updated_at: MS, sent_at: null },
      { id: "del2", realm: "default", template: "welcome", to: "b@example.com", subject: "Welcome", status: "sent", attempts: 1, last_error: null, provider: "smtp", job_id: null, created_at: MS, updated_at: MS, sent_at: MS },
    ],
    agreements: [{ id: "ag1", realm: "default", kind: "terms", version: 1, title: "Terms of service", body: "Be kind.", url: null, published_at: MS, published_by: "admin" }],
    acceptances: [{ id: "ac1", realm: "default", principal: "user:users/u1", kind: "terms", version: 1, accepted_at: MS, ip: "10.0.0.1", user_agent: null }],
    secrets: [], keyRotations: [], verifyResult: { verified: false, reason: "TXT record not found" },
  };
  const ok = (body: unknown, status = 200) => ({ status, body });
  const realmOf = (q: URLSearchParams) => q.get("realm") ?? "default";
  const A = "/api/auth";

  mock.on("GET", `${A}/realms`, () => ok({ items: st.realms }));
  mock.on("PUT", `${A}/realms/{realm}`, ({ params, body }) => {
    const b = body as { config: RealmIdentity["config"] };
    if (b.config.methods.ldap && b.config.ldap && !b.config.ldap.url) return envelope(400, "invalid_request", "ldap.url must be ldap:// or ldaps://");
    let r = st.realms.find((x) => x.realm === params.realm);
    // Write-only secrets: the server masks stored ones and keeps them when the mask, null or nothing is sent.
    const mask = (s: string | null | undefined, old: string | null | undefined) => (s === "" ? null : s === null || s === undefined || s === "********" ? (old ?? null) : "********");
    const next = structuredClone(b.config);
    if (next.ldap) next.ldap.bind_password = mask(next.ldap.bind_password, r?.config.ldap?.bind_password);
    if (next.turnstile) next.turnstile.secret = mask(next.turnstile.secret, r?.config.turnstile?.secret);
    if (!r) { r = { realm: params.realm!, collection: `${params.realm}_users`, config: next, created_at: ISO, updated_at: ISO }; st.realms.push(r); }
    r.config = next; r.updated_at = "2026-11-09T11:00:00Z";
    return ok(r);
  });
  mock.on("GET", `${A}/realm`, ({ headers }) => ok({ realm: headers["X-ForgeBase-Realm"] ?? "default", oauth_providers: [{ id: "github", kind: "github", display_name: "GitHub" }] }));
  mock.on("POST", "/api/v1/realms", ({ body }) => {
    const b = body as { slug: string; name: string };
    return ok({ id: b.slug, name: b.name }, 201);
  });

  mock.on("GET", `${A}/clients`, ({ query }) => ok({ items: st.clients.filter((c) => c.realm === realmOf(query)) }));
  mock.on("POST", `${A}/clients`, ({ body }) => {
    const b = body as Partial<ClientApp> & { name: string };
    if (!b.redirect_uris?.length && b.type !== "public" && false) return envelope(400, "invalid_request", "redirect_uris");
    const client: ClientApp = {
      client_id: `app_${st.clients.length + 1}`, realm: b.realm ?? "default", name: b.name, description: b.description ?? null, type: b.type ?? "confidential", redirect_uris: b.redirect_uris ?? [],
      post_logout_redirect_uris: b.post_logout_redirect_uris ?? [], grant_types: b.grant_types ?? [], scopes: b.scopes ?? [], skip_consent: !!b.skip_consent, access_token_ttl: b.access_token_ttl ?? null,
      refresh_token_ttl: b.refresh_token_ttl ?? null, disabled: false, secret_created_at: b.type === "public" ? null : ISO, previous_secret_expires_at: null, created_at: ISO, updated_at: ISO,
    };
    st.clients.push(client);
    return ok(b.type === "public" ? { client } : { client, client_secret: "fbs_first_secret" }, 201);
  });
  mock.on("PATCH", `${A}/clients/{id}`, ({ params, body }) => {
    const c = st.clients.find((x) => x.client_id === params.id);
    if (!c) return envelope(404, "not_found", "no such client");
    Object.assign(c, body);
    return ok(c);
  });
  mock.on("DELETE", `${A}/clients/{id}`, ({ params }) => { st.clients = st.clients.filter((c) => c.client_id !== params.id); return { status: 204 }; });
  mock.on("POST", `${A}/clients/{id}/secret`, ({ params, body }) => {
    const c = st.clients.find((x) => x.client_id === params.id);
    if (!c) return envelope(404, "not_found", "no such client");
    const grace = (body as { grace_seconds?: number }).grace_seconds ?? 0;
    c.previous_secret_expires_at = grace > 0 ? "2026-11-09T11:00:00Z" : null;
    st.secrets.push("fbs_rotated_secret");
    return ok({ client: c, client_secret: "fbs_rotated_secret" });
  });
  mock.on("GET", `${A}/consents`, () => ok({ items: st.consents }));
  mock.on("DELETE", `${A}/consents/{id}`, ({ params }) => { st.consents = st.consents.filter((c) => c.id !== params.id); return { status: 204 }; });
  mock.on("POST", `${A}/keys/oidc/rotate`, ({ body }) => { const realm = (body as { realm: string }).realm; st.keyRotations.push(realm); return ok({ realm, kid: "kid-2" }); });

  const C = "/api/v1/customize";
  mock.on("GET", `${C}/domains`, () => ok({ items: st.domains }));
  mock.on("POST", `${C}/domains`, ({ body }) => {
    const host = (body as { host: string }).host;
    if (st.domains.some((d) => d.host === host)) return envelope(409, "domain_taken", "domain already claimed");
    const d: Domain = { id: `dom${st.domains.length + 1}`, realm: "default", host, status: "pending", created_at: MS, verified_at: null, last_checked: null, last_error: null, created_by: "admin", verification: { record_name: "_forgebase-verify", record_type: "TXT", record_value: "forgebase-verify=newtok", expires_at: MS + 86_400_000 } };
    st.domains.push(d);
    return ok(d, 201);
  });
  mock.on("DELETE", `${C}/domains/{id}`, ({ params }) => { st.domains = st.domains.filter((d) => d.id !== params.id); return { status: 204 }; });
  mock.on("POST", `${C}/domains/{id}/verify`, ({ params }) => {
    const d = st.domains.find((x) => x.id === params.id)!;
    if (st.verifyResult.verified) { d.status = "verified"; d.verification = null; d.verified_at = MS; }
    return ok({ ...st.verifyResult, domain: d });
  });
  mock.on("POST", `${C}/domains/{id}/reissue`, ({ params }) => {
    const d = st.domains.find((x) => x.id === params.id)!;
    d.verification = { ...d.verification!, record_value: "forgebase-verify=reissued" };
    return ok(d);
  });

  mock.on("GET", `${C}/branding`, () => ok(st.branding));
  mock.on("PUT", `${C}/branding`, ({ body }) => { Object.assign(st.branding, body, { updated_at: MS }); return ok(st.branding); });
  mock.on("PUT", `${C}/branding/assets/{kind}`, ({ params }) => { if (params.kind === "logo") st.branding.has_logo = true; else st.branding.has_favicon = true; return ok(st.branding); });
  mock.on("DELETE", `${C}/branding/assets/{kind}`, ({ params }) => { if (params.kind === "logo") st.branding.has_logo = false; else st.branding.has_favicon = false; return ok(st.branding); });

  mock.on("GET", `${C}/email/templates`, () => ok({ items: st.templates }));
  mock.on("PUT", `${C}/email/templates/{key}`, ({ params, body }) => {
    const t = st.templates.find((x) => x.key === params.key)!;
    Object.assign(t, body, { customized: true, updated_at: MS });
    return ok(t);
  });
  mock.on("DELETE", `${C}/email/templates/{key}`, ({ params }) => {
    const t = st.templates.find((x) => x.key === params.key)!;
    Object.assign(t, { customized: false, subject: "Default subject", updated_at: null });
    return { status: 204 };
  });
  mock.on("POST", `${C}/email/preview`, ({ body }) => {
    const b = body as { draft: { subject: string; text: string }; variables: Record<string, unknown> };
    const user = (b.variables.user ?? {}) as Record<string, string>;
    const missing = /{{\s*reset_url\s*}}/.test(b.draft.text) && !b.variables.reset_url ? ["reset_url"] : [];
    return ok({ subject: b.draft.subject, text: b.draft.text.replace(/{{\s*user\.name\s*}}/, user.name ?? "").replace(/{{\s*reset_url\s*}}/, String(b.variables.reset_url ?? "")), html: null, missing });
  });
  mock.on("POST", `${C}/email/send`, ({ body }) => {
    const b = body as { template: string; to: string };
    const d: Delivery = { id: `del${st.deliveries.length + 1}`, realm: "default", template: b.template, to: b.to, subject: "Test", status: "queued", attempts: 0, last_error: null, provider: null, job_id: null, created_at: MS, updated_at: MS, sent_at: null };
    st.deliveries.unshift(d);
    return ok(d, 202);
  });
  mock.on("GET", `${C}/email/deliveries`, ({ query }) => ok({ items: st.deliveries.filter((d) => !query.get("status") || d.status === query.get("status")) }));
  mock.on("POST", `${C}/email/deliveries/{id}/retry`, ({ params }) => {
    const d = st.deliveries.find((x) => x.id === params.id)!;
    d.status = "queued"; d.last_error = null;
    return ok(d);
  });

  mock.on("GET", `${C}/agreements`, () => ok({ items: [...st.agreements].sort((a, b) => b.version - a.version) }));
  mock.on("POST", `${C}/agreements`, ({ body }) => {
    const b = body as { kind: string; title: string; body: string; url?: string };
    const version = Math.max(0, ...st.agreements.filter((a) => a.kind === b.kind).map((a) => a.version)) + 1;
    const a: Agreement = { id: `ag${st.agreements.length + 1}`, realm: "default", kind: b.kind, version, title: b.title, body: b.body, url: b.url ?? null, published_at: MS, published_by: "admin" };
    st.agreements.push(a);
    return ok(a, 201);
  });
  mock.on("GET", `${C}/agreements/acceptances`, ({ query }) => ok({ items: st.acceptances.filter((a) => !query.get("kind") || a.kind === query.get("kind")) }));
  return st;
}
