import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { pick } from "../../test/pick";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installIdentity } from "../../test/idMock";
import { sheet } from "../../test/dataStudioMock";
import { checkProviderForm, PROVIDER_KINDS, PROVIDER_KIND_INFO } from "../../api";
import { MAIL_FIELDS, SMS_FIELDS, checkGateway, checkMail, isHttpUrl, mailBody } from "./gatewayFields";

const adminScope = vi.hoisted(() => ({ value: undefined as undefined | { realm: string } }));
vi.mock("../../auth/session", async (orig) => {
  const m = await orig<typeof import("../../auth/session")>();
  return { ...m, useOptionalSession: () => (adminScope.value ? ({ adminScope: adminScope.value } as ReturnType<typeof m.useOptionalSession>) : m.useOptionalSession()) };
});

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
  adminScope.value = undefined;
});

function setup() {
  const mock = signedIn(new MockApi().install());
  return { mock, st: installIdentity(mock) };
}
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const body = (mock: MockApi, key: string) => mock.bodies[key]![mock.bodies[key]!.length - 1] as Record<string, any>;
const PUT_PROVIDER = "put /api/auth/realms/{realm}/oauth-providers/{id}";
const GUID = "11111111-2222-3333-4444-555555555555";

describe("provider form checks", () => {
  it("knows every login provider kind and what each needs", () => {
    expect(PROVIDER_KINDS).toEqual(["google", "github", "apple", "facebook", "wechat", "discord", "oidc", "microsoft", "gitlab", "slack", "linkedin", "twitch", "bitbucket", "spotify", "keycloak"]);
    expect(PROVIDER_KIND_INFO.microsoft!.fields).toEqual(["tenant", "allowed_tenants", "base_url"]);
    expect(PROVIDER_KIND_INFO.gitlab!.fields).toEqual(["base_url"]);
    expect(PROVIDER_KIND_INFO.keycloak).toMatchObject({ fields: ["base_url", "tenant"], tenantRequired: true, baseUrlRequired: true });
  });

  it("checks Microsoft tenants, Keycloak's server and realm, base URLs and reserved authorize parameters", () => {
    expect(checkProviderForm({ kind: "microsoft" })).toEqual({});
    expect(checkProviderForm({ kind: "microsoft", tenant: "organizations", allowed_tenants: [GUID] })).toEqual({});
    expect(checkProviderForm({ kind: "microsoft", tenant: GUID })).toEqual({});
    expect(checkProviderForm({ kind: "microsoft", tenant: "contoso.com" }).tenant).toMatch(/common, organizations, consumers or a tenant GUID/);
    expect(checkProviderForm({ kind: "microsoft", allowed_tenants: ["nope"] }).allowed_tenants).toMatch(/GUIDs/);
    expect(checkProviderForm({ kind: "keycloak" })).toEqual({ base_url: "Keycloak needs the server URL.", tenant: "Keycloak needs the realm name." });
    expect(checkProviderForm({ kind: "keycloak", base_url: "https://sso.example.com", tenant: "my realm" })).toEqual({});
    expect(checkProviderForm({ kind: "keycloak", base_url: "https://sso.example.com", tenant: "bad/realm" }).tenant).toMatch(/realm name has/);
    expect(checkProviderForm({ kind: "gitlab", base_url: "ftp://x" }).base_url).toMatch(/http\(s\) URL/);
    expect(checkProviderForm({ kind: "gitlab", base_url: "https://u:p@gitlab.example.com" }).base_url).toBeTruthy();
    expect(checkProviderForm({ kind: "gitlab", base_url: "https://gitlab.example.com?x=1" }).base_url).toBeTruthy();
    expect(checkProviderForm({ kind: "slack", authorize_params: { team: "T0123" } })).toEqual({});
    expect(checkProviderForm({ kind: "slack", authorize_params: { state: "x" } }).authorize_params).toMatch(/state cannot be set/);
  });

  it("checks gateway fields: required, URLs, emails, ports and secrets", () => {
    expect(isHttpUrl("https://a.example")).toBe(true);
    expect(isHttpUrl("https://u@a.example")).toBe(false);
    expect(isHttpUrl("a.example")).toBe(false);
    expect(checkGateway(SMS_FIELDS.vonage, {})).toEqual({ api_key: "API key is required.", api_secret: "API secret is required.", from: "From is required." });
    expect(checkGateway(SMS_FIELDS.vonage, { api_key: "********", api_secret: "s", from: "+14155550100", base_url: "nope" })).toEqual({ base_url: "Use an http(s) URL without credentials." });
    expect(checkGateway(SMS_FIELDS.sns, { access_key_id: "A", secret_access_key: "s", region: "eu-west-1" })).toEqual({});
    expect(checkMail("smtp", { host: "smtp.x", envelope_from: "bad", port: "70000" }, "")).toMatchObject({ envelope_from: expect.stringMatching(/email address/), port: "Use a port from 1 to 65535." });
    expect(checkMail("smtp", { host: "smtp.x", envelope_from: "b@x.co", username: "u" }, "")).toEqual({ password: "A username needs a password. Set both, or neither." });
    expect(checkMail("smtp", { host: "smtp.x", envelope_from: "b@x.co", password: "p" }, "")).toEqual({ username: "A password needs a username. Set both, or neither." });
    expect(checkMail("mailgun", { api_key: "k", domain: "localhost" }, "")).toEqual({ domain: "Use a sending domain such as mg.example.com." });
    expect(checkMail("postmark", { server_token: "t", message_stream: "bad stream!" }, "")).toEqual({ message_stream: "Use letters, digits, - and _." });
    expect(checkMail("resend", { api_key: "k" }, "not-an-email")).toEqual({ from_address: expect.stringMatching(/email address/) });
    expect(checkMail("log", {}, "")).toEqual({});
  });

  it("builds a mail body of this provider's fields only, keeping a stored secret by its mask", () => {
    expect(mailBody("resend", { api_key: "********", base_url: " ", host: "stale" }, " no-reply@x.co ")).toEqual({ provider: "resend", api_key: "********", from_address: "no-reply@x.co" });
    expect(mailBody("smtp", { host: "smtp.x", port: "2525", envelope_from: "b@x.co", username: "u", password: "p" }, "")).toEqual({ provider: "smtp", host: "smtp.x", port: 2525, envelope_from: "b@x.co", username: "u", password: "p" });
    expect(MAIL_FIELDS.ses.map((f) => f.key)).toEqual(["access_key_id", "secret_access_key", "session_token", "region", "configuration_set", "endpoint"]);
  });
});

describe("Login providers: the new kinds", () => {
  async function openNew() {
    const { mock, st } = setup();
    renderStudio("/identity/providers");
    await screen.findByText(/GitHub/);
    await userEvent.click(screen.getByRole("button", { name: "Add provider" }));
    const form = await sheet("New social provider");
    return { mock, st, form };
  }

  it("offers Microsoft, GitLab, Slack, LinkedIn, Twitch, Bitbucket, Spotify and Keycloak", async () => {
    await openNew();
    await userEvent.click(screen.getByLabelText("Provider kind"));
    const options = within(await screen.findByRole("listbox")).getAllByRole("option").map((o) => o.getAttribute("data-value"));
    expect(options).toEqual(PROVIDER_KINDS);
    expect(within(screen.getByRole("listbox")).getByRole("option", { name: "Microsoft (Entra ID)" })).toBeInTheDocument();
  });

  it("saves a Microsoft provider with a tenant, allowed tenants, a login host and authorize parameters", async () => {
    const { mock, st, form } = await openNew();
    await userEvent.type(within(form).getByLabelText("Provider id"), "work");
    await pick(within(form).getByLabelText("Provider kind"), "microsoft");
    await userEvent.type(within(form).getByLabelText("Client id"), "ms-client");
    await userEvent.type(within(form).getByLabelText("Client secret"), "ms-secret");
    await userEvent.type(within(form).getByLabelText("Tenant"), "organizations");
    await userEvent.type(within(form).getByLabelText("Allowed tenants"), `${GUID}, 99999999-2222-3333-4444-555555555555`);
    await userEvent.type(within(form).getByLabelText("Login host"), "https://login.microsoftonline.us");
    await userEvent.click(within(form).getByLabelText("Extra authorize parameters"));
    await userEvent.paste("prompt=select_account\ndomain_hint=contoso.com");
    expect(within(form).getByText(/xms_edov/)).toBeInTheDocument();
    await userEvent.click(within(form).getByRole("button", { name: "Save provider" }));
    await waitFor(() => expect(st.providers.map((p) => p.id)).toContain("work"));
    expect(body(mock, PUT_PROVIDER)).toMatchObject({
      kind: "microsoft", client_id: "ms-client", client_secret: "ms-secret", tenant: "organizations", base_url: "https://login.microsoftonline.us",
      allowed_tenants: [GUID, "99999999-2222-3333-4444-555555555555"], authorize_params: { prompt: "select_account", domain_hint: "contoso.com" },
    });
    expect(document.body.textContent).not.toContain("ms-secret");
  });

  it("refuses a Microsoft tenant that is a domain name, with the reason next to the field", async () => {
    const { form } = await openNew();
    await userEvent.type(within(form).getByLabelText("Provider id"), "work");
    await pick(within(form).getByLabelText("Provider kind"), "microsoft");
    await userEvent.type(within(form).getByLabelText("Client id"), "ms-client");
    await userEvent.type(within(form).getByLabelText("Tenant"), "contoso.com");
    expect(await within(form).findByText("Use common, organizations, consumers or a tenant GUID.")).toBeInTheDocument();
    expect(within(form).getByRole("button", { name: "Save provider" })).toBeDisabled();
  });

  it("requires a server URL and a realm for Keycloak, then saves them", async () => {
    const { mock, form } = await openNew();
    await userEvent.type(within(form).getByLabelText("Provider id"), "sso");
    await pick(within(form).getByLabelText("Provider kind"), "keycloak");
    await userEvent.type(within(form).getByLabelText("Client id"), "kc");
    expect(within(form).getByRole("button", { name: "Save provider" })).toBeDisabled();
    expect(await within(form).findByText("Keycloak needs the server URL.")).toBeInTheDocument();
    expect(within(form).getByText("Keycloak needs the realm name.")).toBeInTheDocument();
    await userEvent.type(within(form).getByLabelText(/^Server URL/), "https://sso.example.com");
    await userEvent.type(within(form).getByLabelText(/^Realm/), "acme");
    await userEvent.type(within(form).getByLabelText("Client secret"), "kc-secret");
    await userEvent.click(within(form).getByRole("button", { name: "Save provider" }));
    await waitFor(() => expect(body(mock, PUT_PROVIDER)).toMatchObject({ kind: "keycloak", base_url: "https://sso.example.com", tenant: "acme" }));
  });

  it("shows only the GitLab URL for GitLab, and no extra fields for Slack, with a reserved parameter refused", async () => {
    const { mock, form } = await openNew();
    await pick(within(form).getByLabelText("Provider kind"), "gitlab");
    expect(within(form).getByLabelText("GitLab URL")).toBeInTheDocument();
    expect(within(form).queryByLabelText("Tenant")).not.toBeInTheDocument();
    await userEvent.type(within(form).getByLabelText("GitLab URL"), "https://gitlab.example.com");
    await pick(within(form).getByLabelText("Provider kind"), "slack");
    expect(within(form).queryByLabelText("GitLab URL")).not.toBeInTheDocument();
    await userEvent.type(within(form).getByLabelText("Provider id"), "team-chat");
    await userEvent.type(within(form).getByLabelText("Client id"), "slack-client");
    await userEvent.type(within(form).getByLabelText("Client secret"), "slack-secret");
    await userEvent.click(within(form).getByLabelText("Extra authorize parameters"));
    await userEvent.paste("scope=admin");
    expect(await within(form).findByText(/scope cannot be set: the sign-in flow sets it/)).toBeInTheDocument();
    expect(within(form).getByRole("button", { name: "Save provider" })).toBeDisabled();
    await userEvent.clear(within(form).getByLabelText("Extra authorize parameters"));
    await userEvent.paste("team=T0123");
    await userEvent.click(within(form).getByRole("button", { name: "Save provider" }));
    await waitFor(() => expect(body(mock, PUT_PROVIDER)).toMatchObject({ kind: "slack", authorize_params: { team: "T0123" } }));
    expect(body(mock, PUT_PROVIDER)).not.toHaveProperty("base_url");
  });

  it("edits a saved Keycloak provider with its fields filled in", async () => {
    const { st } = setup();
    st.providers.push({ id: "sso", realm: "default", kind: "keycloak", display_name: "SSO", client_id: "kc", client_secret: "********", source: "admin", read_only: false, tenant: "acme", base_url: "https://sso.example.com", authorize_params: { kc_idp_hint: "azure" } });
    st.providerSecrets["default/sso"] = "x";
    renderStudio("/identity/providers");
    await userEvent.click(await screen.findByRole("button", { name: "Edit provider sso" }));
    const edit = await sheet("Edit provider sso");
    expect(within(edit).getByLabelText(/^Realm/)).toHaveValue("acme");
    expect(within(edit).getByLabelText(/^Server URL/)).toHaveValue("https://sso.example.com");
    expect(within(edit).getByLabelText("Extra authorize parameters")).toHaveValue("kc_idp_hint=azure");
  });
});

describe("SMS gateways", () => {
  async function form() {
    const ctx = setup();
    renderStudio("/identity/providers");
    return { ...ctx, form: await screen.findByRole("form", { name: "SMS gateway" }) };
  }

  it("lists every gateway type", async () => {
    const { form: f } = await form();
    await userEvent.click(within(f).getByLabelText("Gateway type"));
    const labels = within(await screen.findByRole("listbox")).getAllByRole("option").map((o) => o.getAttribute("data-value"));
    expect(labels).toEqual(["dev", "twilio", "webhook", "vonage", "messagebird", "bird", "sns", "plivo"]);
  });

  it.each([
    ["vonage", { "API key": "sec-vk", "API secret": "sec-vs", From: "+14155550100" }, { provider: "vonage", api_key: "sec-vk", api_secret: "sec-vs", from: "+14155550100" }],
    ["messagebird", { "Access key": "sec-mk", From: "ForgeBase" }, { provider: "messagebird", api_key: "sec-mk", from: "ForgeBase" }],
    ["bird", { "Access key": "sec-bk", "Workspace id": "ws1", "Channel id": "ch1" }, { provider: "bird", api_key: "sec-bk", workspace_id: "ws1", channel_id: "ch1" }],
    ["sns", { "Access key id": "AKIA1", "Secret access key": "sec-sk", Region: "eu-west-1", "Sender id": "ACME" }, { provider: "sns", access_key_id: "AKIA1", secret_access_key: "sec-sk", region: "eu-west-1", sender_id: "ACME" }],
    ["plivo", { "Auth id": "pid", "Auth token": "sec-pt", From: "+14155550100" }, { provider: "plivo", auth_id: "pid", auth_token: "sec-pt", from: "+14155550100" }],
  ])("sets up %s with its own fields and keeps the secrets write-only", async (type, values, expected) => {
    const { mock, st, form: f } = await form();
    await pick(within(f).getByLabelText("Gateway type"), type);
    for (const [label, v] of Object.entries(values)) await userEvent.type(within(f).getByLabelText(label), v);
    await userEvent.click(within(f).getByRole("button", { name: "Save SMS gateway" }));
    await waitFor(() => expect(body(mock, "put /api/auth/realms/{realm}/sms")).toEqual(expected));
    await waitFor(() => expect(st.sms.default).toBeDefined());
    // Back from the server a secret is a mask: the page never holds its value.
    const secretLabel = Object.keys(values).find((l) => /secret|token|key$/i.test(l) && !/id$/i.test(l) && l !== "Access key id")!;
    await waitFor(() => expect(screen.getByLabelText(secretLabel)).toHaveAttribute("placeholder", "set (leave empty to keep)"));
    for (const v of Object.values(values).filter((x) => x.startsWith("sec-"))) expect(document.body.textContent).not.toContain(v);
  });

  it("checks the required fields and the base URL before saving", async () => {
    const { mock, form: f } = await form();
    await pick(within(f).getByLabelText("Gateway type"), "vonage");
    await userEvent.click(within(f).getByRole("button", { name: "Save SMS gateway" }));
    expect(await within(f).findByText("API key is required.")).toBeInTheDocument();
    expect(within(f).getByText("API secret is required.")).toBeInTheDocument();
    expect(within(f).getByText("From is required.")).toBeInTheDocument();
    expect(mock.bodies["put /api/auth/realms/{realm}/sms"]).toBeUndefined();
    await userEvent.type(within(f).getByLabelText("API key"), "k");
    await userEvent.type(within(f).getByLabelText("API secret"), "s");
    await userEvent.type(within(f).getByLabelText("From"), "+1");
    await userEvent.type(within(f).getByLabelText("Base URL"), "gateway.example.com");
    expect(await within(f).findByText("Use an http(s) URL without credentials.")).toBeInTheDocument();
    await userEvent.click(within(f).getByRole("button", { name: "Save SMS gateway" }));
    expect(mock.bodies["put /api/auth/realms/{realm}/sms"]).toBeUndefined();
  });

  it("drops the old gateway's fields when the type changes", async () => {
    const { mock, form: f } = await form();
    await pick(within(f).getByLabelText("Gateway type"), "twilio");
    await userEvent.type(within(f).getByLabelText("Account SID"), "AC1");
    await pick(within(f).getByLabelText("Gateway type"), "plivo");
    expect(within(f).queryByLabelText("Account SID")).not.toBeInTheDocument();
    await userEvent.type(within(f).getByLabelText("Auth id"), "pid");
    await userEvent.type(within(f).getByLabelText("Auth token"), "sec-pt");
    await userEvent.type(within(f).getByLabelText("From"), "+14155550100");
    await userEvent.click(within(f).getByRole("button", { name: "Save SMS gateway" }));
    await waitFor(() => expect(body(mock, "put /api/auth/realms/{realm}/sms")).toEqual({ provider: "plivo", auth_id: "pid", auth_token: "sec-pt", from: "+14155550100" }));
  });
});

describe("Email provider page", () => {
  it("is a tab of Identity, for platform administrators only", async () => {
    setup();
    renderStudio("/identity");
    const tabs = await screen.findByRole("tablist", { name: "Identity sections" });
    expect(within(tabs).getByRole("tab", { name: "Email provider" })).toBeInTheDocument();
  });

  it("hides the tab from a realm administrator", async () => {
    adminScope.value = { realm: "acme" };
    setup();
    renderStudio("/identity");
    await screen.findByRole("tablist", { name: "Identity sections" });
    expect(screen.queryByRole("tab", { name: "Email provider" })).not.toBeInTheDocument();
  });

  it("says a realm without a provider uses the instance's, which falls back to the log provider", async () => {
    setup();
    renderStudio("/identity/email-provider");
    expect(await screen.findByText(/uses the instance's provider, which falls back to the log provider/)).toBeInTheDocument();
    expect(screen.getByText("not configured")).toBeInTheDocument();
  });

  it("lists the six transports and the log provider", async () => {
    setup();
    renderStudio("/identity/email-provider");
    const form = await screen.findByRole("form", { name: "Mail provider" });
    await userEvent.click(within(form).getByLabelText("Provider"));
    expect(within(await screen.findByRole("listbox")).getAllByRole("option").map((o) => o.getAttribute("data-value"))).toEqual(["smtp", "resend", "postmark", "sendgrid", "mailgun", "ses", "log"]);
  });

  it.each([
    ["resend", { "API key": "re_key" }, { provider: "resend", api_key: "re_key" }],
    ["postmark", { "Server token": "pm_tok", "Message stream": "broadcast" }, { provider: "postmark", server_token: "pm_tok", message_stream: "broadcast" }],
    ["sendgrid", { "API key": "SG.key" }, { provider: "sendgrid", api_key: "SG.key" }],
    ["mailgun", { "API key": "mg_key", "Sending domain": "mg.example.com" }, { provider: "mailgun", api_key: "mg_key", domain: "mg.example.com" }],
    ["ses", { "Access key id": "AKIA1", "Secret access key": "ses_secret", Region: "eu-west-1" }, { provider: "ses", access_key_id: "AKIA1", secret_access_key: "ses_secret", region: "eu-west-1" }],
    ["smtp", { Host: "smtp.example.com", "Envelope sender": "bounces@example.com", Username: "u", Password: "smtp_pw" }, { provider: "smtp", host: "smtp.example.com", envelope_from: "bounces@example.com", username: "u", password: "smtp_pw" }],
  ])("saves %s with its own fields, and the secret comes back masked and write-only", async (type, values, expected) => {
    const { mock, st } = setup();
    renderStudio("/identity/email-provider");
    const form = await screen.findByRole("form", { name: "Mail provider" });
    await pick(within(form).getByLabelText("Provider"), type);
    for (const [label, v] of Object.entries(values)) await userEvent.type(within(form).getByLabelText(label), v);
    await userEvent.type(within(form).getByLabelText("From address"), "no-reply@example.com");
    await userEvent.click(within(form).getByRole("button", { name: "Save mail provider" }));
    await waitFor(() => expect(st.mail.default).toBeDefined());
    expect(body(mock, "put /api/v1/customize/email/provider")).toEqual({ ...expected, from_address: "no-reply@example.com" });
    const secret = Object.keys(values).find((l) => /^(API key|Server token|Secret access key|Password)$/.test(l))!;
    const field = await screen.findByLabelText(secret);
    await waitFor(() => expect(field).toHaveAttribute("placeholder", "set (leave empty to keep)"));
    expect(screen.getAllByText("********").length).toBeGreaterThan(0);
    for (const v of Object.values(values).filter((x) => /^(re_key|pm_tok|SG\.key|mg_key|ses_secret|smtp_pw)$/.test(x))) expect(document.body.textContent).not.toContain(v);
  });

  it("keeps a stored secret when other fields change, sending the mask, and replaces it when typed", async () => {
    const { mock, st } = setup();
    st.mail.default = { provider: "resend", api_key: "********", from_address: "no-reply@example.com" };
    st.mailSecrets["default/api_key"] = "re_first";
    renderStudio("/identity/email-provider");
    const form = await screen.findByRole("form", { name: "Mail provider" });
    expect(within(form).getByLabelText("From address")).toHaveValue("no-reply@example.com");
    expect(within(form).getByText("Provider")).toBeInTheDocument();
    await userEvent.type(within(form).getByLabelText("Base URL"), "https://api.resend.test");
    await userEvent.click(within(form).getByRole("button", { name: "Save mail provider" }));
    await waitFor(() => expect(body(mock, "put /api/v1/customize/email/provider")).toEqual({ provider: "resend", api_key: "********", base_url: "https://api.resend.test", from_address: "no-reply@example.com" }));
    expect(st.mailSecrets["default/api_key"]).toBe("re_first");
    await userEvent.type(await screen.findByLabelText("API key"), "re_second");
    await userEvent.click(screen.getByRole("button", { name: "Save mail provider" }));
    await waitFor(() => expect(st.mailSecrets["default/api_key"]).toBe("re_second"));
  });

  it("validates before saving: required fields, the SMTP sender and password pair, the Mailgun domain", async () => {
    const { mock } = setup();
    renderStudio("/identity/email-provider");
    const form = await screen.findByRole("form", { name: "Mail provider" });
    await userEvent.click(within(form).getByRole("button", { name: "Save mail provider" }));
    expect(await within(form).findByText("Host is required.")).toBeInTheDocument();
    expect(within(form).getByText("Envelope sender is required.")).toBeInTheDocument();
    await userEvent.type(within(form).getByLabelText("Host"), "smtp.example.com");
    await userEvent.type(within(form).getByLabelText("Envelope sender"), "not-an-address");
    await userEvent.type(within(form).getByLabelText("Username"), "mailer");
    await userEvent.type(within(form).getByLabelText("Port"), "70000");
    expect(await within(form).findByText(/Use an email address such as bounces@example.com/)).toBeInTheDocument();
    expect(within(form).getByText("A username needs a password. Set both, or neither.")).toBeInTheDocument();
    expect(within(form).getByText("Use a port from 1 to 65535.")).toBeInTheDocument();
    await userEvent.click(within(form).getByRole("button", { name: "Save mail provider" }));
    expect(mock.bodies["put /api/v1/customize/email/provider"]).toBeUndefined();
    await pick(within(form).getByLabelText("Provider"), "mailgun");
    await userEvent.type(within(form).getByLabelText("API key"), "k");
    await userEvent.type(within(form).getByLabelText("Sending domain"), "localhost");
    expect(await within(form).findByText("Use a sending domain such as mg.example.com.")).toBeInTheDocument();
  });

  it("configures the instance (*) separately from a realm", async () => {
    const { mock, st } = setup();
    renderStudio("/identity/email-provider");
    const form = await screen.findByRole("form", { name: "Mail provider" });
    await pick(screen.getByLabelText("Configure"), "instance");
    await waitFor(() => expect(mock.queries.some((q, i) => mock.calls[i]!.endsWith("/email/provider") && q.get("realm") === "*")).toBe(true));
    const f2 = await screen.findByText(/No instance provider is set/);
    expect(f2).toBeInTheDocument();
    await pick(within(await screen.findByRole("form", { name: "Mail provider" })).getByLabelText("Provider"), "resend");
    await userEvent.type(within(screen.getByRole("form", { name: "Mail provider" })).getByLabelText("API key"), "re_inst");
    await userEvent.click(within(screen.getByRole("form", { name: "Mail provider" })).getByRole("button", { name: "Save mail provider" }));
    await waitFor(() => expect(st.mail["*"]).toMatchObject({ provider: "resend" }));
    expect(st.mail.default).toBeUndefined();
    expect(form).toBeDefined();
  });

  it("removes a provider after a confirmation", async () => {
    const { st } = setup();
    st.mail.default = { provider: "log" };
    renderStudio("/identity/email-provider");
    await userEvent.click(await screen.findByRole("button", { name: "Remove mail provider" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm remove mail provider" }));
    await waitFor(() => expect(st.mail.default).toBeUndefined());
  });

  it("sends a test email and shows the provider, the From and the provider's message id", async () => {
    const { st } = setup();
    st.mail.default = { provider: "resend", api_key: "********" };
    st.mailSecrets["default/api_key"] = "re_k";
    renderStudio("/identity/email-provider");
    const box = await screen.findByRole("region", { name: "Send test email" });
    expect(within(box).getByRole("button", { name: "Send test email" })).toBeDisabled();
    await userEvent.type(within(box).getByLabelText("Send to"), "me@example.com");
    await userEvent.click(within(box).getByRole("button", { name: "Send test email" }));
    expect(await within(box).findByText("Test email accepted")).toBeInTheDocument();
    expect(box).toHaveTextContent("re_abc123");
    expect(box).toHaveTextContent("no-reply@example.com");
    expect(st.mailTests).toEqual([{ realm: "default", to: "me@example.com" }]);
  });

  it("shows the provider's reason when the test is refused, and checks the address first", async () => {
    const { st } = setup();
    st.mail.default = { provider: "resend", api_key: "********" };
    st.mailTest = "fail";
    renderStudio("/identity/email-provider");
    const box = await screen.findByRole("region", { name: "Send test email" });
    await userEvent.type(within(box).getByLabelText("Send to"), "not-an-email");
    expect(await within(box).findByText("Use an email address such as you@example.com.")).toBeInTheDocument();
    expect(within(box).getByRole("button", { name: "Send test email" })).toBeDisabled();
    await userEvent.clear(within(box).getByLabelText("Send to"));
    await userEvent.type(within(box).getByLabelText("Send to"), "me@example.com");
    await userEvent.click(within(box).getByRole("button", { name: "Send test email" }));
    const alert = await within(box).findByRole("alert");
    expect(alert).toHaveTextContent("The provider did not accept the test message");
    expect(alert).toHaveTextContent("resend refused the credentials");
  });

  it("shows the provider message id in the delivery log", async () => {
    setup();
    renderStudio("/identity/email");
    const table = await screen.findByRole("table", { name: "Deliveries" });
    expect(within(table).getByRole("columnheader", { name: "Provider message id" })).toBeInTheDocument();
    expect(within(table).getByText("re_9f3a2c")).toBeInTheDocument();
  });
});
