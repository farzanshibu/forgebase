import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installSystemOps, MATRIX, cap, tlsAcme, tlsFiles } from "../../test/systemMock";
import { installSetup, sampleFeatures } from "../../test/setupMock";
import type { FeatureSetting, ServerFeature } from "../../api";
import SettingField from "../setup/SettingField";
import { TLS_WARN_DAYS, expiryWords } from "./util";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

const T = { timeout: 15_000 };
const DAY = 86_400;

function setup(init: Parameters<typeof installSystemOps>[1] = {}) {
  const mock = signedIn(new MockApi().install());
  return { mock, ops: installSystemOps(mock, init) };
}

describe("capability matrix", () => {
  it("lists every feature with its engine, whether it is accelerated or portable, state, mode, reason and limits", async () => {
    setup();
    renderStudio("/system/capabilities");
    const table = await screen.findByRole("table", { name: "Capability matrix" }, T);
    expect(within(table).getAllByRole("row")).toHaveLength(8);
    const row = (feature: string) => table.querySelector<HTMLElement>(`[data-feature="${feature}"]`)!;

    const cell = (feature: string, i: number) => within(row(feature)).getAllByRole("cell")[i]!;
    const vector = row("vector");
    expect(vector).toHaveTextContent("Vector search");
    expect(vector).toHaveTextContent("pgvector 0.8.7");
    expect(within(cell("vector", 2)).getByText("accelerated")).toBeInTheDocument();
    expect(within(cell("vector", 3)).getByText("active")).toBeInTheDocument();
    expect(vector).toHaveTextContent("auto");
    expect(within(vector).getByRole("list", { name: "Limits" })).toHaveTextContent("max dimensions: 2000");
    expect(within(vector).getByRole("list", { name: "Limits" })).toHaveTextContent('index kinds: ["hnsw","ivfflat"]');

    const graph = row("graph");
    // Two badges: how it runs (portable) and its state (portable).
    expect(within(graph).getAllByText("portable")).toHaveLength(2);
    expect(graph).toHaveTextContent("Apache AGE is not installed");

    expect(within(row("opencypher")).getAllByText("portable").length).toBeGreaterThan(0);
    expect(within(row("kv")).getByText("built in")).toBeInTheDocument();
    expect(within(row("kv")).getByText("native")).toBeInTheDocument();

    const cron = row("cron");
    expect(within(cell("cron", 3)).getByText("off")).toBeInTheDocument();
    expect(within(cell("cron", 2)).queryByText(/accelerated|portable|built in/)).not.toBeInTheDocument();
    expect(cron).toHaveTextContent("switched off in [database.postgres.extensions]");

    const sharding = row("sharding");
    expect(within(cell("sharding", 3)).getByText("unavailable")).toBeInTheDocument();
    expect(sharding).toHaveTextContent("max_prepared_transactions is 0");
  });

  it("explains each state on hover", async () => {
    setup();
    renderStudio("/system/capabilities");
    const table = await screen.findByRole("table", { name: "Capability matrix" }, T);
    // Columns: feature, engine, runs on, state, mode, why, limits.
    const state = (feature: string, word: string) => within(within(table.querySelector<HTMLElement>(`[data-feature="${feature}"]`)!).getAllByRole("cell")[3]!).getByText(word);
    expect(state("vector", "active")).toHaveAttribute("title", expect.stringMatching(/PostgreSQL extension/));
    expect(state("graph", "portable")).toHaveAttribute("title", expect.stringMatching(/which works on every database/));
    expect(state("sharding", "unavailable")).toHaveAttribute("title", expect.stringMatching(/Cannot run on this server/));
    expect(state("cron", "off")).toHaveAttribute("title", "Switched off.");
    expect(state("kv", "native")).toHaveAttribute("title", expect.stringMatching(/Part of the server itself/));
  });

  it("summarises the backend and the counts, and explains each state", async () => {
    setup();
    renderStudio("/system/capabilities");
    await screen.findByRole("table", { name: "Capability matrix" }, T);
    expect(screen.getByText(/Database:/)).toHaveTextContent("Database: PostgreSQL. 2 accelerated, 2 portable, 1 off, 1 unavailable.");
    const legend = screen.getByRole("list", { name: "What each state means" });
    expect(within(legend).getAllByRole("listitem")).toHaveLength(5);
    expect(legend).toHaveTextContent("Runs on ForgeBase's own engine");
  });

  it("works on SQLite too: portable and built-in engines, nothing accelerated", async () => {
    setup({ capabilities: { backend: "sqlite", features: [cap({ feature: "fuzzy", label: "Fuzzy search", engine: "portable trigram tables" }), cap({ feature: "kv", label: "Key-value", engine: "built-in", state: "native" })] } });
    renderStudio("/system/capabilities");
    await screen.findByRole("table", { name: "Capability matrix" }, T);
    expect(screen.queryByText("accelerated", { selector: "[title]" })).not.toBeInTheDocument();
    expect(screen.getByText(/Database:/)).toHaveTextContent("Database: SQLite. 0 accelerated, 1 portable, 0 off.");
  });

  it("says so when the server lists nothing", async () => {
    setup({ capabilities: { backend: "sqlite", features: [] } });
    renderStudio("/system/capabilities");
    expect(await screen.findByText("No capabilities reported", {}, T)).toBeInTheDocument();
  });

  it("is a tab in System, next to Health", async () => {
    setup();
    renderStudio("/system");
    expect(await screen.findByRole("tab", { name: "Capabilities" }, T)).toHaveAttribute("href", "/system/capabilities");
    expect(MATRIX.features).toHaveLength(7);
  });
});

describe("TLS status card", () => {
  const card = async () => (await screen.findByRole("heading", { name: "HTTPS (TLS)" }, T)).closest("section")!;

  it("says TLS is off, and how to turn it on", async () => {
    setup({ tls: { mode: "off" } });
    renderStudio("/system");
    const c = await card();
    expect(within(c).getByText("off")).toBeInTheDocument();
    expect(c).toHaveTextContent(/plain HTTP/);
    expect(c).toHaveTextContent(/tls\.mode/);
  });

  it("is not drawn by a server that does not report TLS", async () => {
    setup({ tls: undefined });
    renderStudio("/system");
    await screen.findByText("Server", {}, T);
    expect(screen.queryByRole("heading", { name: "HTTPS (TLS)" })).not.toBeInTheDocument();
  });

  it("shows the certificate's subject, names, issuer and validity in files mode, with no warning when it is far from expiry", async () => {
    setup({ tls: tlsFiles() });
    renderStudio("/system");
    const c = await card();
    expect(within(c).getByText("Certificate files")).toBeInTheDocument();
    expect(c).toHaveTextContent("forgebase.example.com");
    expect(c).toHaveTextContent("R11 (Let's Encrypt)");
    expect(within(c).getByText("www.example.com")).toBeInTheDocument();
    expect(within(c).getByText("48 days left")).toBeInTheDocument();
    expect(c).toHaveTextContent("/etc/forge/cert.pem");
    expect(c).toHaveTextContent("31,536,000 s");
    expect(c).toHaveTextContent("1.2");
    expect(within(c).queryByRole("alert")).not.toBeInTheDocument();
    expect(within(c).queryByRole("status")).not.toBeInTheDocument();
  });

  it("warns when the certificate expires within 14 days, and tells files mode what to do", async () => {
    setup({ tls: tlsFiles({ certificate: { ...tlsFiles().certificate!, expires_in_secs: 9 * DAY + 3600 } }) });
    renderStudio("/system");
    const c = await card();
    const warn = within(c).getByRole("status");
    expect(warn).toHaveTextContent("The certificate expires within 14 days");
    expect(warn).toHaveTextContent("9 days left");
    expect(warn).toHaveTextContent(/Replace the certificate files; the server reloads them/);
    expect(within(c).getByText("9 days left", { selector: "[class*=badge], span" })).toBeInTheDocument();
  });

  it("warns on the last day, in hours", async () => {
    setup({ tls: tlsFiles({ certificate: { ...tlsFiles().certificate!, expires_in_secs: 5 * 3600 } }) });
    renderStudio("/system");
    expect(within(await card()).getByRole("status")).toHaveTextContent("5 hours left");
  });

  it("raises an alert for an expired certificate", async () => {
    setup({ tls: tlsFiles({ certificate: { ...tlsFiles().certificate!, expires_in_secs: -3 * DAY } }) });
    renderStudio("/system");
    const alert = within(await card()).getByRole("alert");
    expect(alert).toHaveTextContent("The certificate has expired");
    expect(alert).toHaveTextContent("expired 3 days ago");
  });

  it("shows ACME mode: domains, checks, authority, renewal times and no failures", async () => {
    setup({ tls: tlsAcme() });
    renderStudio("/system");
    const c = await card();
    expect(within(c).getByText("Automatic (ACME)")).toBeInTheDocument();
    expect(within(c).getByText("api.example.com")).toBeInTheDocument();
    expect(within(c).getByText("tls-alpn-01")).toBeInTheDocument();
    expect(within(c).getByText("http-01")).toBeInTheDocument();
    expect(c).toHaveTextContent("https://acme-v02.api.letsencrypt.org/directory");
    expect(c).toHaveTextContent("Last renewal");
    expect(c).toHaveTextContent("Next renewal");
    expect(within(c).getByText("Renewal failures").nextElementSibling).toHaveTextContent("0");
    expect(c).toHaveTextContent("0.0.0.0:80");
  });

  it("shows ACME failures and the last error, and says renewal should have happened", async () => {
    setup({ tls: tlsAcme({ certificate: { ...tlsAcme().certificate!, expires_in_secs: 6 * DAY }, renewal_failures: 3, last_error: "acme: rate limited", last_error_at: "2026-11-14T09:00:00Z", next_renewal: null }) });
    renderStudio("/system");
    const c = await card();
    expect(within(c).getByText("Renewal failures").nextElementSibling).toHaveTextContent("3");
    expect(within(c).getByText("Not scheduled")).toBeInTheDocument();
    const errs = within(c).getAllByRole("alert");
    expect(errs.at(-1)).toHaveTextContent("Last error");
    expect(errs.at(-1)).toHaveTextContent("acme: rate limited");
    expect(within(c).getByRole("status")).toHaveTextContent(/Renewal normally happens well before this/);
  });

  it("says no certificate is loaded yet", async () => {
    setup({ tls: tlsAcme({ certificate: null }) });
    renderStudio("/system");
    expect(await within(await card()).findByText("none loaded yet", {}, T)).toBeInTheDocument();
  });

  it("uses the same 14 day threshold as forgebase doctor", () => {
    expect(TLS_WARN_DAYS).toBe(14);
    expect(expiryWords(14 * DAY).level).toBe("ok");
    expect(expiryWords(14 * DAY - 1).level).toBe("warn");
    expect(expiryWords(0).level).toBe("warn");
    expect(expiryWords(-1).level).toBe("bad");
    expect(expiryWords(1 * DAY).text).toBe("1 day left");
    expect(expiryWords(90 * 60).text).toBe("1 hour left");
  });
});

describe("tracing status card", () => {
  const obs = (enabled: boolean): ServerFeature => ({
    id: "observability", name: "Tracing and insights", summary: "Trace ids and export.", always_on: true, can_disable: false, enabled: true, studio_path: "/system",
    settings: [
      { key: "observability.trust_incoming_trace", label: "Continue callers' traces", help: "h", type: "enum", options: ["auto", "always", "never"], value: "auto", default: "auto", source: "default", editable: true, restart_required: false, secret: false },
      { key: "observability.otlp.enabled", label: "Export traces", help: "h", type: "bool", value: enabled, default: false, source: "file", editable: true, restart_required: true, secret: false },
      { key: "observability.otlp.endpoint", label: "Collector address", help: "h", type: "url", value: "http://127.0.0.1:4318", default: null, source: "file", editable: true, restart_required: true, secret: false },
      { key: "observability.otlp.protocol", label: "Export format", help: "h", type: "enum", options: ["http/protobuf", "http/json"], value: "http/protobuf", default: "http/protobuf", source: "default", editable: true, restart_required: true, secret: false },
    ],
  });
  const card = async () => (await screen.findByRole("heading", { name: "Tracing" }, T)).closest("section")!;

  it("shows OTLP export on, its collector and format", async () => {
    setup({ features: [...sampleFeatures(), obs(true)] });
    renderStudio("/system");
    const c = await card();
    expect(within(c).getByText("enabled")).toBeInTheDocument();
    expect(c).toHaveTextContent("http://127.0.0.1:4318");
    expect(c).toHaveTextContent("http/protobuf");
    expect(c).toHaveTextContent("auto");
    expect(c).toHaveTextContent(/traces are sent to the collector/);
    expect(within(c).getByRole("link", { name: "Change in Features and settings" })).toHaveAttribute("href", "/system/features#settings-observability");
  });

  it("says nothing is exported when it is off, and hides the collector", async () => {
    setup({ features: [...sampleFeatures(), obs(false)] });
    renderStudio("/system");
    const c = await card();
    expect(within(c).getByText("off")).toBeInTheDocument();
    expect(c).not.toHaveTextContent("127.0.0.1:4318");
    expect(c).toHaveTextContent(/Nothing is exported/);
  });

  it("is not drawn when the server does not expose it", async () => {
    setup({ features: undefined });
    renderStudio("/system");
    await screen.findByRole("heading", { name: "Server" }, T);
    expect(screen.queryByRole("heading", { name: "Tracing" })).not.toBeInTheDocument();
  });
});

describe("settings of list type", () => {
  const tls = (): ServerFeature => ({
    id: "tls", name: "HTTPS", summary: "TLS and ACME.", always_on: false, can_disable: true, enabled: true, studio_path: null,
    settings: [
      { key: "tls.enabled", label: "Enabled", help: "h", type: "bool", value: true, default: false, source: "file", editable: false, restart_required: true, secret: false },
      { key: "tls.acme.domains", label: "Domain names", help: "The domain names on the certificate.", type: "list", value: ["forgebase.example.com", "api.example.com"], default: [], source: "file", editable: false, restart_required: true, secret: false },
      { key: "tls.acme.challenges", label: "Domain checks", help: "How the authority checks you control the domains.", type: "list", value: ["tls-alpn-01", "http-01"], default: ["tls-alpn-01"], source: "env", editable: false, restart_required: true, secret: false },
      { key: "tls.acme.empty", label: "No domains", help: "h", type: "list", value: [], default: [], source: "default", editable: false, restart_required: true, secret: false },
    ],
  });

  it("shows a read-only list one chip per entry, in order, in the settings view", async () => {
    const mock = new MockApi().install();
    installSetup(mock, { features: [...sampleFeatures(), tls()] });
    signedIn(mock);
    renderStudio("/system/features");
    const cfg = await screen.findByRole("region", { name: "Settings" }, T);
    const domains = within(cfg).getByRole("list", { name: "Domain names" });
    expect(within(domains).getAllByRole("listitem").map((l) => l.textContent)).toEqual(["forgebase.example.com", "api.example.com"]);
    const checks = within(cfg).getByRole("list", { name: "Domain checks" });
    expect(within(checks).getAllByRole("listitem").map((l) => l.textContent)).toEqual(["tls-alpn-01", "http-01"]);
    const row = cfg.querySelector("[data-setting='tls.acme.challenges']") as HTMLElement;
    expect(row).toHaveTextContent(/Read-only: Set by an environment variable/);
    expect(row.querySelector("[data-source]")).toHaveAttribute("data-source", "env");
    expect(within(cfg.querySelector("[data-setting='tls.acme.empty']") as HTMLElement).getByText("None set.")).toBeInTheDocument();
    // An editable list is still a textarea.
    expect(within(cfg).getByLabelText("Allowed types")).toHaveValue("image/png\nimage/jpeg");
  });

  it("renders a locked list as a plain list (and a single string as one entry)", () => {
    const s: FeatureSetting = { key: "tls.acme.domains", label: "Domain names", help: "h", type: "list", value: ["a.example.com"], default: [], source: "file", editable: false, restart_required: false, secret: false };
    const { rerender } = render(<SettingField setting={s} value={["a.example.com", "b.example.com"]} onChange={() => {}} />);
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    rerender(<SettingField setting={s} value="solo.example.com" onChange={() => {}} />);
    expect(screen.getByRole("listitem")).toHaveTextContent("solo.example.com");
    rerender(<SettingField setting={s} value={null} onChange={() => {}} />);
    expect(screen.getByText("None set.")).toBeInTheDocument();
  });
});
