import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installIdentity } from "../../test/idMock";
import { nest } from "./util";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

function setup() {
  const mock = signedIn(new MockApi().install());
  return { mock, st: installIdentity(mock) };
}
// Last JSON body sent to `method template`.
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const body = (mock: MockApi, key: string) => mock.bodies[key]![mock.bodies[key]!.length - 1] as Record<string, any>;

describe("Identity Studio shell", () => {
  it("is reachable from the sidebar and switches realm", async () => {
    const { mock } = setup();
    renderStudio("/");
    await userEvent.click(await screen.findByRole("link", { name: /Identity & Tenant Studio/ }));
    expect(await screen.findByRole("heading", { name: "Identity and Tenant Studio" })).toBeInTheDocument();
    expect(await screen.findByRole("cell", { name: "acme" })).toBeInTheDocument();
    await userEvent.selectOptions(screen.getByLabelText("Realm"), "acme");
    await userEvent.click(screen.getByRole("link", { name: "Client apps" }));
    expect(await screen.findByText("No client apps yet.")).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });
});

describe("Realms and tenant settings", () => {
  it("creates a realm with its identity config and saves tenant settings", async () => {
    const { mock, st } = setup();
    renderStudio("/identity");
    await screen.findByRole("cell", { name: "acme" });
    await userEvent.type(screen.getByLabelText("Realm id"), "globex");
    await userEvent.type(screen.getByLabelText("Realm name"), "Globex");
    await userEvent.click(screen.getByRole("button", { name: "Create realm" }));
    await waitFor(() => expect(st.realms.map((r) => r.realm)).toContain("globex"));
    expect(body(mock, "post /api/v1/realms")).toEqual({ slug: "globex", name: "Globex" });

    await userEvent.click(await screen.findByRole("button", { name: "Settings acme" }));
    const form = screen.getByRole("form", { name: "Tenant settings" });
    await userEvent.click(within(form).getByLabelText(/^Allow sign-up/));
    await userEvent.click(within(form).getByLabelText("Passkey"));
    await userEvent.selectOptions(within(form).getByLabelText("Require verified email"), "true");
    await userEvent.click(within(form).getByRole("button", { name: "Save settings" }));
    expect(await within(form).findByRole("status")).toHaveTextContent("Saved");
    const sent = body(mock, "put /api/auth/realms/{realm}").config;
    expect(sent).toMatchObject({ signup: false, require_verified_email: true, methods: { passkey: false } });
    expect(st.realms.find((r) => r.realm === "acme")!.config.signup).toBe(false);
  });
});

describe("Client apps", () => {
  it("creates a confidential app, shows the secret once, rotates with a grace period and deletes", async () => {
    const { mock, st } = setup();
    renderStudio("/identity/clients");
    await screen.findByText("app_web");
    const form = screen.getByRole("form", { name: "Create client app" });
    await userEvent.type(within(form).getByLabelText("Name"), "Billing");
    await userEvent.type(within(form).getByLabelText("Redirect URIs"), "https://bill.example.com/cb{enter}https://bill.example.com/cb2");
    await userEvent.click(within(form).getByLabelText("client_credentials"));
    await userEvent.click(within(form).getByLabelText(/^Skip consent screen/));
    await userEvent.type(within(form).getByLabelText("Access token lifetime"), "600");
    await userEvent.click(within(form).getByRole("button", { name: "Create client app" }));
    expect(await screen.findByTestId("secret-once")).toHaveTextContent("fbs_first_secret");
    expect(body(mock, "post /api/auth/clients")).toMatchObject({
      name: "Billing", realm: "default", type: "confidential", redirect_uris: ["https://bill.example.com/cb", "https://bill.example.com/cb2"],
      skip_consent: true, access_token_ttl: 600,
    });
    expect(body(mock, "post /api/auth/clients").grant_types).toContain("client_credentials");

    await userEvent.click(screen.getByRole("button", { name: "I have copied it" }));
    expect(screen.queryByTestId("secret-once")).not.toBeInTheDocument();
    expect(document.body.textContent).not.toContain("fbs_first_secret");

    await userEvent.selectOptions(screen.getByLabelText("Grace for Web app"), "86400");
    await userEvent.click(screen.getByRole("button", { name: "Rotate secret Web app" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm rotate secret web app" }));
    expect(await screen.findByTestId("secret-once")).toHaveTextContent("fbs_rotated_secret");
    expect(body(mock, "post /api/auth/clients/{id}/secret")).toEqual({ grace_seconds: 86400 });
    expect(await screen.findByText(/old secret valid until/)).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Delete Billing" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm delete billing" }));
    await waitFor(() => expect(st.clients.map((c) => c.name)).toEqual(["Web app"]));
    expect(mock.problems).toEqual([]);
  });

  it("creates a public app without a secret and edits an app", async () => {
    const { mock, st } = setup();
    renderStudio("/identity/clients");
    await screen.findByText("app_web");
    const form = screen.getByRole("form", { name: "Create client app" });
    await userEvent.type(within(form).getByLabelText("Name"), "SPA");
    await userEvent.selectOptions(within(form).getByLabelText("Type"), "public");
    await userEvent.click(within(form).getByRole("button", { name: "Create client app" }));
    expect(await screen.findByText("none (PKCE)")).toBeInTheDocument();
    expect(screen.queryByTestId("secret-once")).not.toBeInTheDocument();
    expect(body(mock, "post /api/auth/clients").type).toBe("public");

    await userEvent.click(screen.getByRole("button", { name: "Edit Web app" }));
    const edit = screen.getByRole("form", { name: "Edit client app" });
    await userEvent.clear(within(edit).getByLabelText("Name"));
    await userEvent.type(within(edit).getByLabelText("Name"), "Web app v2");
    await userEvent.click(within(edit).getByLabelText("Disabled"));
    await userEvent.click(within(edit).getByRole("button", { name: "Save client app" }));
    expect(await screen.findByText("disabled")).toBeInTheDocument();
    expect(st.clients[0]).toMatchObject({ name: "Web app v2", disabled: true });
    expect(body(mock, "patch /api/auth/clients/{id}")).toMatchObject({ name: "Web app v2", disabled: true });
  });
});

describe("Identity providers", () => {
  it("lists social providers read-only and keeps LDAP and Turnstile secrets write-only", async () => {
    const { mock, st } = setup();
    renderStudio("/identity/providers");
    expect(await screen.findByText(/GitHub/)).toBeInTheDocument();
    expect(mock.headers.some((h) => h["X-ForgeBase-Realm"] === "default")).toBe(true);

    const ldap = screen.getByRole("form", { name: "LDAP" });
    await userEvent.click(within(ldap).getByLabelText("LDAP sign-in enabled"));
    await userEvent.type(within(ldap).getByLabelText("LDAP URL"), "ldaps://ldap.example.com");
    await userEvent.type(within(ldap).getByLabelText("Base DN"), "dc=example,dc=com");
    await userEvent.type(within(ldap).getByLabelText("Bind password"), "s3cret");
    await userEvent.click(within(ldap).getByRole("button", { name: "Save LDAP" }));
    expect(await within(ldap).findByRole("status")).toHaveTextContent("Saved");
    expect(body(mock, "put /api/auth/realms/{realm}").config.ldap).toMatchObject({ url: "ldaps://ldap.example.com", bind_password: "s3cret" });

    // Back from the server the secret is only a mask: the field never shows a value, only that one is set.
    await waitFor(() => expect(screen.getByLabelText("Bind password")).toHaveAttribute("placeholder", "set (leave empty to keep)"));
    expect(screen.getByLabelText("Bind password")).toHaveValue("");
    expect(st.realms[0]!.config.ldap!.bind_password).toBe("********");
    expect(document.body.textContent).not.toContain("s3cret");

    // Saving without touching the secret sends the mask (keep); Clear sends "".
    await userEvent.click(within(screen.getByRole("form", { name: "LDAP" })).getByRole("button", { name: "Save LDAP" }));
    await waitFor(() => expect(mock.bodies["put /api/auth/realms/{realm}"]!.length).toBe(2));
    expect(body(mock, "put /api/auth/realms/{realm}").config.ldap.bind_password).toBe("********");
    await userEvent.click(within(screen.getByRole("form", { name: "LDAP" })).getByRole("button", { name: "Clear Bind password" }));
    await userEvent.click(within(screen.getByRole("form", { name: "LDAP" })).getByRole("button", { name: "Save LDAP" }));
    await waitFor(() => expect(mock.bodies["put /api/auth/realms/{realm}"]!.length).toBe(3));
    expect(body(mock, "put /api/auth/realms/{realm}").config.ldap.bind_password).toBe("");
    await waitFor(() => expect(screen.getByLabelText("Bind password")).toHaveAttribute("placeholder", "not set"));
  });

  it("configures Turnstile and the phone method, and shows the server's validation error", async () => {
    const { mock } = setup();
    renderStudio("/identity/providers");
    await screen.findByRole("form", { name: "LDAP" });
    const ts = screen.getByRole("form", { name: "Turnstile (bot protection)" });
    await userEvent.click(within(ts).getByLabelText("Turnstile enabled"));
    await userEvent.type(within(ts).getByLabelText("Site key"), "0xABC");
    await userEvent.type(within(ts).getByLabelText("Turnstile secret"), "ts-secret");
    await userEvent.click(within(ts).getByLabelText("otp"));
    await userEvent.click(within(ts).getByRole("button", { name: "Save Turnstile (bot protection)" }));
    await waitFor(() => expect(body(mock, "put /api/auth/realms/{realm}").config.turnstile).toMatchObject({ enabled: true, site_key: "0xABC", secret: "ts-secret", actions: ["signup", "login", "otp"] }));

    const sms = screen.getByRole("form", { name: "Phone (SMS)" });
    await userEvent.click(within(sms).getByLabelText("Phone sign-in enabled"));
    await userEvent.click(within(sms).getByRole("button", { name: "Save Phone (SMS)" }));
    await waitFor(() => expect(body(mock, "put /api/auth/realms/{realm}").config.methods.phone).toBe(true));

    // LDAP enabled without a URL: the server refuses and the message is shown.
    const ldap = screen.getByRole("form", { name: "LDAP" });
    await userEvent.click(within(ldap).getByLabelText("LDAP sign-in enabled"));
    await userEvent.click(within(ldap).getByRole("button", { name: "Save LDAP" }));
    expect(await within(ldap).findByRole("alert")).toHaveTextContent("ldap.url must be ldap:// or ldaps://");
  });
});

describe("Consents and OIDC keys", () => {
  it("lists the realm's consents, revokes one and rotates the signing keys", async () => {
    const { mock, st } = setup();
    renderStudio("/identity/consents");
    expect(await screen.findByText("Web app")).toBeInTheDocument();
    expect(screen.queryByText("Other realm app")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Revoke Web app" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm revoke web app" }));
    expect(await screen.findByText("No consents in this realm.")).toBeInTheDocument();
    expect(st.consents.map((c) => c.id)).toEqual(["con2"]);

    await userEvent.click(screen.getByRole("button", { name: "Rotate keys for default" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm rotate keys for default" }));
    expect(await screen.findByText("kid-2")).toBeInTheDocument();
    expect(body(mock, "post /api/auth/keys/oidc/rotate")).toEqual({ realm: "default" });
  });
});

describe("Consents without a user session", () => {
  it("explains that the server lists a signed-in user's own consents only", async () => {
    const mock = signedIn(new MockApi().install());
    mock.on("GET", "/api/auth/realms", () => ({ status: 200, body: { items: [] } }));
    mock.on("GET", "/api/auth/consents", () => ({ status: 400, body: { error: { code: "invalid_request", message: "only users have consents", request_id: "r" } } }));
    renderStudio("/identity/consents");
    expect(await screen.findByText(/only users have consents/)).toBeInTheDocument();
    expect(screen.getByText(/no administrator-wide consent list yet/)).toBeInTheDocument();
  });
});

describe("Branding", () => {
  it("edits colours and texts with a live preview, validates colours and uploads a logo", async () => {
    const { mock, st } = setup();
    renderStudio("/identity/branding");
    const form = await screen.findByRole("form", { name: "Branding" });
    const preview = screen.getByLabelText("Sign-in preview");
    expect(within(form).getByLabelText("Primary colour")).toHaveValue("#ff8800");
    await userEvent.type(within(form).getByLabelText("Sign-in title"), "Welcome back");
    expect(preview).toHaveTextContent("Welcome back");
    await userEvent.type(within(form).getByLabelText("Background colour"), "#102030");
    expect(preview).toHaveStyle({ background: "#102030" });

    await userEvent.type(within(form).getByLabelText("Accent colour"), "red");
    expect(screen.getByRole("alert")).toHaveTextContent("Colours must be hex");
    expect(within(form).getByRole("button", { name: "Save branding" })).toBeDisabled();
    await userEvent.clear(within(form).getByLabelText("Accent colour"));
    await userEvent.click(within(form).getByRole("button", { name: "Save branding" }));
    expect(await within(form).findByRole("status")).toHaveTextContent("Saved");
    expect(body(mock, "put /api/v1/customize/branding")).toMatchObject({ display_name: "Acme", primary_color: "#ff8800", background_color: "#102030", signin_title: "Welcome back", accent_color: null });

    await userEvent.upload(screen.getByLabelText("Logo file"), new File([new Uint8Array([1, 2, 3])], "logo.png", { type: "image/png" }));
    expect(await screen.findByAltText("Current logo")).toBeInTheDocument();
    expect(st.branding.has_logo).toBe(true);
    expect(await screen.findByAltText("Preview logo")).toBeInTheDocument();
    expect(mock.headers.find((h) => h["Content-Type"] === "image/png")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Remove logo" }));
    await waitFor(() => expect(st.branding.has_logo).toBe(false));

    const big = new File([new Uint8Array(300 * 1024)], "big.png", { type: "image/png" });
    await userEvent.upload(screen.getByLabelText("Favicon file"), big);
    expect(await screen.findByText(/at most 256 KiB \(this one/)).toBeInTheDocument();
    expect(st.branding.has_favicon).toBe(false);
  });
});

describe("Custom domains", () => {
  it("claims a host, shows the TXT record and verifies it", async () => {
    const { mock, st } = setup();
    renderStudio("/identity/domains");
    const list = await screen.findByLabelText("TXT record for login.example.com");
    expect(list).toHaveTextContent("_forgebase-verify.login.example.com");
    expect(list).toHaveTextContent("forgebase-verify=tok123");

    await userEvent.click(screen.getByRole("button", { name: "Verify login.example.com" }));
    expect(await screen.findByRole("status")).toHaveTextContent("TXT record not found");
    await userEvent.click(screen.getByRole("button", { name: "New token login.example.com" }));
    await waitFor(() => expect(screen.getByLabelText("TXT record for login.example.com")).toHaveTextContent("reissued"));

    st.verifyResult = { verified: true, reason: null };
    await userEvent.click(screen.getByRole("button", { name: "Verify login.example.com" }));
    expect(await screen.findByText(/^Verified 2026-11-09/)).toBeInTheDocument();
    expect(screen.queryByLabelText("TXT record for login.example.com")).not.toBeInTheDocument();

    await userEvent.type(screen.getByLabelText("Host"), "Auth.Example.com");
    await userEvent.click(screen.getByRole("button", { name: "Claim domain" }));
    expect(await screen.findByLabelText("TXT record for auth.example.com")).toBeInTheDocument();
    expect(body(mock, "post /api/v1/customize/domains")).toEqual({ host: "auth.example.com", realm: "default" });

    await userEvent.type(screen.getByLabelText("Host"), "auth.example.com");
    await userEvent.click(screen.getByRole("button", { name: "Claim domain" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("domain_taken");
    await userEvent.click(screen.getByRole("button", { name: "Remove auth.example.com" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm remove auth.example.com" }));
    await waitFor(() => expect(st.domains.map((d) => d.host)).toEqual(["login.example.com"]));
  });
});

describe("Email templates", () => {
  it("edits a template, previews with sample variables, sends a test and resets", async () => {
    const { mock, st } = setup();
    renderStudio("/identity/email");
    await userEvent.click(await screen.findByRole("button", { name: "Edit template password_reset" }));
    const form = screen.getByRole("form", { name: "Edit template" });
    expect(screen.getByLabelText("Template variables")).toHaveTextContent("{{reset_url}}");
    await userEvent.clear(within(form).getByLabelText("Subject"));
    await userEvent.type(within(form).getByLabelText("Subject"), "Choose a new password");
    await userEvent.click(within(form).getByRole("button", { name: "Save template" }));
    await waitFor(() => expect(st.templates[0]).toMatchObject({ subject: "Choose a new password", customized: true }));
    expect(body(mock, "put /api/v1/customize/email/templates/{key}")).toMatchObject({ subject: "Choose a new password", html: null });

    // Preview: dotted variable names become nested objects; a missing variable is reported.
    const samples = await screen.findByLabelText("Sample values");
    await userEvent.clear(samples);
    await userEvent.type(samples, "user.name=Ada");
    await userEvent.click(screen.getByRole("button", { name: "Preview" }));
    const prev = await screen.findByLabelText("Email preview");
    expect(prev).toHaveTextContent("Hi Ada");
    expect(prev).toHaveTextContent("Missing variables: reset_url");
    expect(body(mock, "post /api/v1/customize/email/preview").variables).toEqual({ user: { name: "Ada" } });

    await userEvent.type(screen.getByLabelText("Send test to"), "me@example.com");
    await userEvent.click(screen.getByRole("button", { name: "Send test" }));
    expect(await screen.findByText("Test email queued for me@example.com")).toBeInTheDocument();
    expect(body(mock, "post /api/v1/customize/email/send")).toMatchObject({ template: "password_reset", to: "me@example.com" });

    await userEvent.click(screen.getByRole("button", { name: "Reset to default" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm reset to default" }));
    await waitFor(() => expect(st.templates[0]!.customized).toBe(false));
  });

  it("shows the delivery log, filters by status and retries a failed delivery", async () => {
    const { mock, st } = setup();
    renderStudio("/identity/email");
    expect(await screen.findByText("a@example.com")).toBeInTheDocument();
    expect(screen.getByText("smtp 550 mailbox full")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Retry b@example.com" })).not.toBeInTheDocument();
    await userEvent.selectOptions(screen.getByLabelText("Delivery status"), "sent");
    await waitFor(() => expect(screen.queryByText("a@example.com")).not.toBeInTheDocument());
    expect(mock.calls.filter((c) => c === "get /api/v1/customize/email/deliveries").length).toBeGreaterThan(1);
    await userEvent.selectOptions(screen.getByLabelText("Delivery status"), "");
    await userEvent.click(await screen.findByRole("button", { name: "Retry a@example.com" }));
    await waitFor(() => expect(st.deliveries[0]!.status).toBe("queued"));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Retry a@example.com" })).not.toBeInTheDocument());
  });

  it("nests dotted variable names", () => {
    expect(nest("a=1\nuser.name=Ada\nuser.email=a@b.c\nbad\n=x")).toEqual({ a: "1", user: { name: "Ada", email: "a@b.c" } });
  });
});

describe("Legal agreements", () => {
  it("publishes a new version, shows the text and lists acceptances", async () => {
    const { mock, st } = setup();
    renderStudio("/identity/legal");
    expect(await screen.findByText("Terms of service")).toBeInTheDocument();
    expect(screen.getByText("current")).toBeInTheDocument();
    expect(await screen.findByText("user:users/u1")).toBeInTheDocument();

    const form = screen.getByRole("form", { name: "Publish agreement" });
    await userEvent.type(within(form).getByLabelText("Kind"), "terms");
    await userEvent.type(within(form).getByLabelText("Title"), "Terms of service 2026");
    expect(within(form).getByText(/creates version 2 of “terms”/)).toBeInTheDocument();
    await userEvent.type(within(form).getByLabelText("Body"), "Be kinder.");
    await userEvent.click(within(form).getByRole("button", { name: "Publish version" }));
    expect(await screen.findByText("Terms of service 2026")).toBeInTheDocument();
    expect(st.agreements.map((a) => a.version)).toEqual([1, 2]);
    expect(body(mock, "post /api/v1/customize/agreements")).toMatchObject({ kind: "terms", title: "Terms of service 2026", body: "Be kinder." });
    const row = screen.getByRole("row", { name: /service 2026 20/ });
    expect(within(row).getByText("current")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "View terms v1" }));
    expect(screen.getByLabelText("Agreement text")).toHaveTextContent("Be kind.");
    await userEvent.selectOptions(screen.getByLabelText("Acceptance kind"), "terms");
    expect(await screen.findByText("terms v1")).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });
});
