import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi, envelope } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { alert, installAlerts, notifier, rule, slo, sloStatus, type AlertsState } from "../../test/alertsMock";
import { confirmDialog, sheet, toast } from "../../test/dataStudioMock";
import { pick } from "../../test/pick";
import { burnTone, fmtValue, isDuration, kvText, matchersFromAlert, parseKv, parseMatcherLines, passes, pct, runbookUrl } from "./util";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

const T = { timeout: 15_000 };

function setup(init: Partial<AlertsState> = {}) {
  const mock = signedIn(new MockApi().install());
  return { mock, st: installAlerts(mock, init) };
}
const lastBody = (mock: MockApi, key: string) => mock.bodies[key]?.at(-1) as Record<string, unknown>;

describe("alerts helpers", () => {
  it("parses key=value lines and says which line is wrong", () => {
    expect(parseKv("team=payments\n\n  env = prod ")).toEqual({ value: { team: "payments", env: "prod" } });
    expect(parseKv("a=b\noops").error).toMatch(/“oops” needs the form name=value/);
    expect(kvText({ a: "1", b: "2" })).toBe("a=1\nb=2");
  });
  it("parses matcher lines with every operator", () => {
    expect(parseMatcherLines("severity=critical\ntopic=~orders.*\nenv!=dev\nx!~y").value).toEqual([
      { label: "severity", op: "=", value: "critical" }, { label: "topic", op: "=~", value: "orders.*" }, { label: "env", op: "!=", value: "dev" }, { label: "x", op: "!~", value: "y" },
    ]);
    expect(parseMatcherLines("nonsense").error).toBeDefined();
  });
  it("prefills a silence from every label but severity", () => {
    expect(matchersFromAlert(alert())).toEqual([
      { label: "alertname", op: "=", value: "QueuePushPaused" }, { label: "subscription", op: "=", value: "orders" }, { label: "topic", op: "=", value: "events" },
    ]);
  });
  it("links a runbook only when it is a web address", () => {
    expect(runbookUrl({ runbook_url: "https://wiki.example.com/x" })).toBe("https://wiki.example.com/x");
    expect(runbookUrl({ runbook_url: "javascript:alert(1)" })).toBeUndefined();
    expect(runbookUrl({})).toBeUndefined();
  });
  it("compares like the engine and never fires on NaN", () => {
    expect(passes(0.2, ">", 0.1)).toBe(true);
    expect(passes(0.1, ">", 0.1)).toBe(false);
    expect(passes(0.1, ">=", 0.1)).toBe(true);
    expect(passes(null, "!=", 0)).toBe(false);
  });
  it("formats numbers, percentages and durations", () => {
    expect(fmtValue(null)).toBe("—");
    expect(fmtValue(0.123456)).toBe("0.123");
    expect(pct(0.99958, 3)).toBe("99.958%");
    expect(pct(0.58, 0)).toBe("58%");
    expect(pct(null)).toBe("—");
    expect(["5m", "1h30m", "30d"].every(isDuration)).toBe(true);
    expect(["", "5", "m5", "5 m"].some(isDuration)).toBe(false);
  });
  it("tones a burn rate: 1 spends the budget exactly, 6 and above pages", () => {
    expect([burnTone(null), burnTone(0.4), burnTone(1), burnTone(5.9), burnTone(6)]).toEqual(["neutral", "ok", "warn", "warn", "bad"]);
  });
});

describe("Alerts studio: open alerts", () => {
  it("lists a firing alert with severity, state, labels, summary and a runbook link", async () => {
    const { mock } = setup();
    renderStudio("/alerts");
    const card = await screen.findByRole("article", { name: "QueuePushPaused firing" }, T);
    expect(within(card).getByText("critical")).toBeInTheDocument();
    expect(within(card).getByText("firing")).toBeInTheDocument();
    expect(within(card).getByText("Push delivery of orders is paused")).toBeInTheDocument();
    const labels = within(card).getByRole("list", { name: "Labels" });
    expect(within(labels).getByText("subscription=orders")).toBeInTheDocument();
    expect(within(card).getByRole("link", { name: "Runbook" })).toHaveAttribute("href", "https://wiki.example.com/push");
    expect(screen.getByRole("heading", { name: "Alerts and SLOs" })).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("does not link a runbook that is not a web address", async () => {
    setup({ active: [alert({ annotations: { summary: "x", runbook_url: "javascript:alert(1)" } })] });
    renderStudio("/alerts");
    await screen.findByRole("article", { name: "QueuePushPaused firing" }, T);
    expect(screen.queryByRole("link", { name: "Runbook" })).not.toBeInTheDocument();
  });

  it("filters by state and severity through the query string", async () => {
    const { mock } = setup({ active: [alert(), alert({ id: "alr-2", name: "DiskSpaceLow", severity: "warning", state: "pending", labels: { alertname: "DiskSpaceLow", severity: "warning" } })] });
    renderStudio("/alerts");
    await screen.findByRole("article", { name: "DiskSpaceLow pending" }, T);
    await pick(screen.getByLabelText("State"), "firing");
    await waitFor(() => expect(screen.queryByRole("article", { name: "DiskSpaceLow pending" })).not.toBeInTheDocument(), T);
    expect(mock.queries.some((q, i) => mock.calls[i] === "get /api/v1/alerts" && q.get("state") === "firing")).toBe(true);
  });

  it("says so when nothing is firing", async () => {
    setup({ active: [] });
    renderStudio("/alerts");
    expect(await screen.findByText("Nothing is firing", {}, T)).toBeInTheDocument();
  });

  it("marks a silenced alert and opens its state changes", async () => {
    setup({ active: [alert({ silenced_by: ["sil-9"] })] });
    renderStudio("/alerts");
    const card = await screen.findByRole("article", { name: "QueuePushPaused firing" }, T);
    expect(within(card).getByText("silenced")).toBeInTheDocument();
    await userEvent.click(within(card).getByRole("button", { name: "Details of QueuePushPaused" }));
    const events = await screen.findByRole("region", { name: "State changes" }, T);
    expect(await within(events).findAllByText("pending", {}, T)).not.toHaveLength(0);
    expect(within(events).getAllByText("firing").length).toBeGreaterThan(0);
  });

  it("silences an alert with its labels prefilled as matchers (severity left out), then creates it", async () => {
    const { mock, st } = setup();
    renderStudio("/alerts");
    await userEvent.click(await screen.findByRole("button", { name: "Silence QueuePushPaused" }, T));
    const form = await sheet("New silence");
    expect(within(form).getByLabelText("Matcher 1 label")).toHaveValue("alertname");
    expect(within(form).getByLabelText("Matcher 1 value")).toHaveValue("QueuePushPaused");
    expect(within(form).getByLabelText("Matcher 2 label")).toHaveValue("subscription");
    expect(within(form).getByLabelText("Matcher 2 value")).toHaveValue("orders");
    expect(within(form).getByLabelText("Matcher 3 value")).toHaveValue("events");
    expect(within(form).queryByDisplayValue("severity")).not.toBeInTheDocument();
    expect(within(form).getByLabelText(/^Duration/)).toHaveValue("2h");
    expect(within(form).getByLabelText(/^Comment/)).toHaveValue("Silencing QueuePushPaused");
    fireEvent.change(within(form).getByLabelText(/^Duration/), { target: { value: "4h" } });
    await userEvent.click(within(form).getByRole("button", { name: "Create silence" }));
    await toast("Silence created");
    expect(lastBody(mock, "post /api/v1/alerts/silences")).toEqual({
      matchers: [{ label: "alertname", op: "=", value: "QueuePushPaused" }, { label: "subscription", op: "=", value: "orders" }, { label: "topic", op: "=", value: "events" }],
      duration: "4h", comment: "Silencing QueuePushPaused",
    });
    expect(st.silences).toHaveLength(1);
  });

  it("keeps Create silence off until a matcher is complete and the duration is valid", async () => {
    setup();
    renderStudio("/alerts/silences?new=1");
    const form = await sheet("New silence");
    expect(within(form).getByRole("button", { name: "Create silence" })).toBeDisabled();
    fireEvent.change(within(form).getByLabelText("Matcher 1 label"), { target: { value: "topic" } });
    fireEvent.change(within(form).getByLabelText("Matcher 1 value"), { target: { value: "events" } });
    expect(within(form).getByRole("button", { name: "Create silence" })).toBeEnabled();
    fireEvent.change(within(form).getByLabelText(/^Duration/), { target: { value: "soon" } });
    expect(within(form).getByRole("button", { name: "Create silence" })).toBeDisabled();
    expect(within(form).getByText(/Use a length such as 30m, 2h or 1d/)).toBeInTheDocument();
  });
});

describe("Alerts studio: history", () => {
  const h = (n: number) => alert({ id: `alr-h${n}`, name: `Old${n}`, state: "resolved", resolved_at: "2026-11-14T11:00:00.000Z", labels: { alertname: `Old${n}` } });
  it("lists every alert and loads more with the next_before cursor", async () => {
    const { mock } = setup({ history: Array.from({ length: 60 }, (_, i) => h(i + 1)) });
    renderStudio("/alerts/history");
    const table = await screen.findByRole("table", { name: "Alert history" }, T);
    expect(within(table).getAllByRole("row")).toHaveLength(51);
    await userEvent.click(screen.getByRole("button", { name: /load more/i }));
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Alert history" })).getAllByRole("row")).toHaveLength(61), T);
    const more = mock.queries.filter((_, i) => mock.calls[i] === "get /api/v1/alerts/history").at(-1)!;
    expect(more.get("before")).toBe("alr-h50");
    expect(screen.queryByRole("button", { name: /load more/i })).not.toBeInTheDocument();
  });
  it("is empty with words, not a blank table", async () => {
    setup();
    renderStudio("/alerts/history");
    expect(await screen.findByText("No alerts yet", {}, T)).toBeInTheDocument();
  });
});

describe("Alerts studio: rules", () => {
  it("lists built-in rules with their source and an enable switch that sends enabled only", async () => {
    const { mock, st } = setup();
    renderStudio("/alerts/rules");
    const table = await screen.findByRole("table", { name: "Alert rules" }, T);
    expect(within(table).getAllByText("built-in")).toHaveLength(2);
    const sw = within(table).getByRole("switch", { name: "Enable HttpServerErrorsHigh" });
    expect(sw).toBeChecked();
    await userEvent.click(sw);
    await toast("Rule HttpServerErrorsHigh disabled");
    expect(lastBody(mock, "patch /api/v1/alerts/rules/{id}")).toEqual({ enabled: false });
    expect(st.rules[0]!.enabled).toBe(false);
  });

  it("locks the switch of a rule the config file disabled, and says so", async () => {
    setup({ rules: [rule({ enabled: false, disabled_by_config: true })] });
    renderStudio("/alerts/rules");
    const table = await screen.findByRole("table", { name: "Alert rules" }, T);
    expect(within(table).getByRole("switch", { name: "Enable HttpServerErrorsHigh" })).toHaveAttribute("aria-disabled", "true");
    expect(within(table).getByText("off in config")).toBeInTheDocument();
  });

  it("offers Edit and Delete only on custom rules", async () => {
    setup({ rules: [rule(), rule({ id: "rule-7", name: "CheckoutSlow", source: "api" })] });
    renderStudio("/alerts/rules");
    await screen.findByRole("table", { name: "Alert rules" }, T);
    expect(screen.getByRole("button", { name: "Edit CheckoutSlow" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit HttpServerErrorsHigh" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Delete HttpServerErrorsHigh" })).not.toBeInTheDocument();
  });

  it("tests an expression through POST /alerts/query and marks the series that would fire", async () => {
    const { mock, st } = setup({ samples: [{ labels: { route: "/api/v1/a" }, value: 0.2 }, { labels: { route: "/api/v1/b" }, value: 0.01 }, { labels: {}, value: null }] });
    renderStudio("/alerts/rules");
    await userEvent.click(await screen.findByRole("button", { name: "New rule" }, T));
    const form = await sheet("New rule");
    const test = within(form).getByRole("button", { name: "Test expression" });
    expect(test).toBeDisabled();
    fireEvent.change(within(form).getByLabelText(/^Expression/), { target: { value: "sum(rate(forgebase_http_requests_total[5m]))" } });
    fireEvent.change(within(form).getByLabelText(/^Threshold/), { target: { value: "0.1" } });
    await userEvent.click(test);
    const result = await within(form).findByRole("region", { name: "Expression result" }, T);
    expect(st.queried).toEqual(["sum(rate(forgebase_http_requests_total[5m]))"]);
    expect(lastBody(mock, "post /api/v1/alerts/query")).toEqual({ expr: "sum(rate(forgebase_http_requests_total[5m]))" });
    expect(within(result).getByText("3 samples at", { exact: false })).toBeInTheDocument();
    expect(within(result).getAllByText("would fire")).toHaveLength(1);
    expect(within(result).getAllByText("quiet")).toHaveLength(2);
    expect(within(result).getByText("route=/api/v1/a")).toBeInTheDocument();
    expect(within(result).getByText("—")).toBeInTheDocument(); // NaN
    expect(st.rules).toHaveLength(2); // testing changes nothing
  });

  it("shows the server's words for an expression that does not parse", async () => {
    setup();
    renderStudio("/alerts/rules?new=1");
    const form = await sheet("New rule");
    fireEvent.change(within(form).getByLabelText(/^Expression/), { target: { value: "(((" } });
    await userEvent.click(within(form).getByRole("button", { name: "Test expression" }));
    const alertBox = await within(form).findByRole("alert", {}, T);
    expect(alertBox).toHaveTextContent("unexpected token at position 3");
    expect(alertBox).toHaveTextContent("invalid_expression");
  });

  it("creates a custom rule with labels and annotations", async () => {
    const { mock, st } = setup();
    renderStudio("/alerts/rules?new=1");
    const form = await sheet("New rule");
    expect(within(form).getByRole("button", { name: "Create rule" })).toBeDisabled();
    await userEvent.type(within(form).getByLabelText(/^Name/), "CheckoutSlow");
    fireEvent.change(within(form).getByLabelText(/^Expression/), { target: { value: "forgebase:queue_backlog" } });
    fireEvent.change(within(form).getByLabelText(/^Threshold/), { target: { value: "500" } });
    await pick(within(form).getByLabelText(/^Severity/), "critical");
    fireEvent.change(within(form).getByLabelText(/^Labels/), { target: { value: "team=payments" } });
    fireEvent.change(within(form).getByLabelText(/^Annotations/), { target: { value: "runbook_url=https://wiki.example.com/co" } });
    await userEvent.click(within(form).getByRole("button", { name: "Create rule" }));
    await toast("Rule CheckoutSlow created");
    expect(lastBody(mock, "post /api/v1/alerts/rules")).toEqual({
      name: "CheckoutSlow", expr: "forgebase:queue_backlog", op: ">", threshold: 500, for: "5m", severity: "critical", labels: { team: "payments" }, annotations: { runbook_url: "https://wiki.example.com/co" },
    });
    expect(st.rules.at(-1)?.source).toBe("api");
  });

  it("blocks a bad threshold, name and label line before asking the server", async () => {
    setup();
    renderStudio("/alerts/rules?new=1");
    const form = await sheet("New rule");
    await userEvent.type(within(form).getByLabelText(/^Name/), "bad name!");
    fireEvent.change(within(form).getByLabelText(/^Expression/), { target: { value: "x" } });
    fireEvent.change(within(form).getByLabelText(/^Threshold/), { target: { value: "lots" } });
    fireEvent.change(within(form).getByLabelText(/^Labels/), { target: { value: "oops" } });
    expect(within(form).getByText(/Use letters, digits/)).toBeInTheDocument();
    expect(within(form).getByText(/Use a number, such as 0.05/)).toBeInTheDocument();
    expect(within(form).getByText(/needs the form name=value/)).toBeInTheDocument();
    expect(within(form).getByRole("button", { name: "Create rule" })).toBeDisabled();
  });

  it("edits a custom rule with PATCH and deletes it after a confirmation", async () => {
    const { mock, st } = setup({ rules: [rule({ id: "rule-7", name: "CheckoutSlow", source: "api", expr: "forgebase:queue_backlog", threshold: 500 })] });
    renderStudio("/alerts/rules");
    await userEvent.click(await screen.findByRole("button", { name: "Edit CheckoutSlow" }, T));
    const form = await sheet("Edit rule CheckoutSlow");
    expect(within(form).getByLabelText(/^Name/)).toBeDisabled();
    fireEvent.change(within(form).getByLabelText(/^Threshold/), { target: { value: "900" } });
    await userEvent.click(within(form).getByRole("button", { name: "Save rule" }));
    await waitFor(() => expect(mock.bodies["patch /api/v1/alerts/rules/{id}"]).toBeDefined(), T);
    expect(lastBody(mock, "patch /api/v1/alerts/rules/{id}")).toMatchObject({ expr: "forgebase:queue_backlog", threshold: 900 });
    expect(st.rules[0]!.threshold).toBe(900);
    await userEvent.click(await screen.findByRole("button", { name: "Delete CheckoutSlow" }, T));
    await confirmDialog("Delete rule CheckoutSlow?", "Delete rule");
    await toast("Rule CheckoutSlow deleted");
    expect(st.rules).toHaveLength(0);
  });

  it("explains a refused change when the server's answer differs from what was loaded", async () => {
    const { st } = setup({ rules: [rule({ enabled: false })] });
    renderStudio("/alerts/rules");
    const sw = await screen.findByRole("switch", { name: "Enable HttpServerErrorsHigh" }, T);
    st.rules[0]!.disabled_by_config = true; // changed in the config file since the page loaded
    await userEvent.click(sw);
    const box = await screen.findByRole("alert", {}, T);
    expect(box).toHaveTextContent("disabled in the config file");
    expect(box).toHaveTextContent("rule_disabled_by_config");
  });
});

describe("Alerts studio: silences", () => {
  const sil = (o = {}) => ({ id: "sil-1", matchers: [{ label: "topic", op: "=" as const, value: "events" }], starts_at: "2026-11-14T10:00:00.000Z", ends_at: "2026-11-14T12:00:00.000Z", comment: "maintenance", created_by: "admin:adm1", created_at: "2026-11-14T10:00:00.000Z", status: "active" as const, ...o });
  it("lists active silences with their matchers and ends one after a confirmation", async () => {
    const { st } = setup({ silences: [sil()] });
    renderStudio("/alerts/silences");
    const table = await screen.findByRole("table", { name: "Silences" }, T);
    expect(within(table).getByText("topic=events")).toBeInTheDocument();
    expect(within(table).getByText("maintenance")).toBeInTheDocument();
    await userEvent.click(within(table).getByRole("button", { name: "End silence sil-1" }));
    await confirmDialog("End this silence?", "End silence");
    await toast("Silence ended");
    expect(st.silences[0]!.status).toBe("expired");
  });
  it("shows ended silences only when asked (?all=true)", async () => {
    const { mock } = setup({ silences: [sil(), sil({ id: "sil-2", status: "expired", comment: "old" })] });
    renderStudio("/alerts/silences");
    const table = await screen.findByRole("table", { name: "Silences" }, T);
    expect(within(table).queryByText("old")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("checkbox", { name: "Show ended" }));
    expect(await screen.findByText("old", {}, T)).toBeInTheDocument();
    expect(mock.queries.some((q, i) => mock.calls[i] === "get /api/v1/alerts/silences" && q.get("all") === "true")).toBe(true);
    expect(screen.queryByRole("button", { name: "End silence sil-2" })).not.toBeInTheDocument();
  });
});

describe("Alerts studio: notifiers", () => {
  it("lists notifiers without a secret: a Slack URL is only its host", async () => {
    setup({ notifiers: [notifier({ id: "ntf-2", name: "slack-ops", kind: "slack", url: "hooks.slack.com" }), notifier({ id: "ntf-3", name: "mail", kind: "email", url: null, to: ["a@example.com", "b@example.com"] })] });
    renderStudio("/alerts/notifiers");
    const table = await screen.findByRole("table", { name: "Notifiers" }, T);
    expect(within(table).getByText("hooks.slack.com")).toBeInTheDocument();
    expect(within(table).getByText("a@example.com, b@example.com")).toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/whsec_|routing/i);
  });

  it("shows a webhook's signing secret once, when it is created, and never in the list", async () => {
    const { mock } = setup({ notifiers: [] });
    renderStudio("/alerts/notifiers?new=1");
    const form = await sheet("New notifier");
    await userEvent.type(within(form).getByLabelText(/^Name/), "ops-hook");
    await userEvent.type(within(form).getByLabelText(/^URL/), "https://hooks.example.com/forge");
    await userEvent.click(within(form).getByRole("button", { name: "Create notifier" }));
    const secret = await screen.findByLabelText("Signing secret", {}, T);
    expect(secret).toHaveTextContent("whsec_abc123secret");
    expect(screen.getByText(/shown once and cannot be read again/)).toBeInTheDocument();
    expect(lastBody(mock, "post /api/v1/alerts/notifiers")).toMatchObject({ name: "ops-hook", kind: "webhook", url: "https://hooks.example.com/forge", min_severity: "info", group_by: ["alertname"], repeat_interval: "4h", send_resolved: true, enabled: true });
    await screen.findByRole("table", { name: "Notifiers" }, T);
    await userEvent.click(screen.getByRole("button", { name: "I have copied it" }));
    await waitFor(() => expect(screen.queryByLabelText("Signing secret")).not.toBeInTheDocument());
    expect(document.body.textContent).not.toMatch(/whsec_/);
  });

  it("takes a Slack URL as a write-only secret and lists only its host", async () => {
    const { mock } = setup({ notifiers: [] });
    renderStudio("/alerts/notifiers?new=1");
    const form = await sheet("New notifier");
    await userEvent.type(within(form).getByLabelText(/^Name/), "slack-ops");
    await pick(within(form).getByLabelText(/^Kind/), "slack");
    const url = within(form).getByLabelText(/^Incoming webhook URL/);
    expect(url).toHaveAttribute("type", "password");
    expect(within(form).getByRole("button", { name: "Create notifier" })).toBeDisabled();
    await userEvent.type(url, "https://hooks.slack.com/services/T000/B000/XXXX");
    await userEvent.click(within(form).getByRole("button", { name: "Create notifier" }));
    expect(await screen.findByText("hooks.slack.com", {}, T)).toBeInTheDocument();
    expect(lastBody(mock, "post /api/v1/alerts/notifiers")).toMatchObject({ kind: "slack", url: "https://hooks.slack.com/services/T000/B000/XXXX" });
    expect(screen.queryByText(/services\/T000/)).not.toBeInTheDocument();
    expect(screen.queryByLabelText("Signing secret")).not.toBeInTheDocument();
  });

  it("never shows a stored secret again: an edit starts blank and a blank field keeps it", async () => {
    const { mock } = setup({ notifiers: [notifier({ id: "ntf-2", name: "slack-ops", kind: "slack", url: "hooks.slack.com" }), notifier({ id: "ntf-4", name: "pager", kind: "pagerduty", url: "https://events.pagerduty.com/v2/enqueue" })] });
    renderStudio("/alerts/notifiers");
    await userEvent.click(await screen.findByRole("button", { name: "Edit slack-ops" }, T));
    let form = await sheet("Edit notifier slack-ops");
    const url = within(form).getByLabelText(/^Incoming webhook URL/);
    expect(url).toHaveValue("");
    expect(url).toHaveAttribute("placeholder", "Unchanged");
    expect(within(form).getByText(/Stored sealed \(hooks.slack.com\)\. Leave empty to keep it/)).toBeInTheDocument();
    await userEvent.click(within(form).getByRole("button", { name: "Save notifier" }));
    await waitFor(() => expect(mock.bodies["put /api/v1/alerts/notifiers/{id}"]).toHaveLength(1), T);
    expect(lastBody(mock, "put /api/v1/alerts/notifiers/{id}")).not.toHaveProperty("url");
    await waitFor(() => expect(screen.queryByRole("form", { name: /Edit notifier/ })).not.toBeInTheDocument(), T);

    await userEvent.click(await screen.findByRole("button", { name: "Edit pager" }, T));
    form = await sheet("Edit notifier pager");
    const key = within(form).getByLabelText(/^Routing key/);
    expect(key).toHaveValue("");
    expect(key).toHaveAttribute("type", "password");
    await userEvent.type(key, "R0UTINGKEY");
    await userEvent.click(within(form).getByRole("button", { name: "Save notifier" }));
    await waitFor(() => expect(mock.bodies["put /api/v1/alerts/notifiers/{id}"]).toHaveLength(2), T);
    expect(lastBody(mock, "put /api/v1/alerts/notifiers/{id}")).toMatchObject({ kind: "pagerduty", routing_key: "R0UTINGKEY" });
    expect(document.body.textContent).not.toContain("R0UTINGKEY");
  });

  it("has the fields of each kind", async () => {
    setup({ notifiers: [] });
    renderStudio("/alerts/notifiers?new=1");
    const form = await sheet("New notifier");
    await pick(within(form).getByLabelText(/^Kind/), "email");
    expect(within(form).getByLabelText(/^Recipients/)).toBeInTheDocument();
    await pick(within(form).getByLabelText(/^Kind/), "queue");
    expect(within(form).getByLabelText(/^Topic/)).toBeInTheDocument();
    await pick(within(form).getByLabelText(/^Kind/), "pagerduty");
    expect(within(form).getByLabelText(/^Routing key/)).toBeInTheDocument();
    expect(within(form).getByLabelText(/^Events URL/)).toBeInTheDocument();
  });

  it("sends a test and shows the delivery, then lists it under Deliveries", async () => {
    const { st } = setup();
    renderStudio("/alerts/notifiers");
    await userEvent.click(await screen.findByRole("button", { name: "Send a test to ops-hook" }, T));
    const note = await screen.findByText(/Test sent to ops-hook/, {}, T);
    expect(note.closest("[role=status]")).toHaveTextContent("Delivered (HTTP 200)");
    expect(st.queried).toEqual([]);
  });

  it("shows a failed test with the receiver's error", async () => {
    setup({ testReply: () => ({ status: 200, body: { id: "d", notifier_id: "ntf-1", group_key: "t", event: "test", status: "failed", attempts: 1, next_attempt_at: null, last_status: 500, last_error: "receiver answered 500", sent_at: null, created_at: "2026-11-14T10:00:00.000Z", payload: {} } }) });
    renderStudio("/alerts/notifiers");
    await userEvent.click(await screen.findByRole("button", { name: "Send a test to ops-hook" }, T));
    const note = await screen.findByText(/Test to ops-hook failed/, {}, T);
    expect(note.closest("[role=status]")).toHaveTextContent("receiver answered 500");
  });

  it("says when test notifications are rate limited", async () => {
    setup({ testReply: () => envelope(429, "rate_limited", "10 tests per notifier and minute") });
    renderStudio("/alerts/notifiers");
    await userEvent.click(await screen.findByRole("button", { name: "Send a test to ops-hook" }, T));
    expect(await screen.findByText("Too many requests", {}, T)).toBeInTheDocument();
  });

  it("lists deliveries newest first with their status, and filters by it", async () => {
    const d = (o = {}) => ({ id: "d1", notifier_id: "ntf-1", group_key: "g", event: "firing", status: "sent" as const, attempts: 1, next_attempt_at: null, last_status: 200, last_error: null, sent_at: "2026-11-14T10:00:00.000Z", created_at: "2026-11-14T10:00:00.000Z", payload: {}, ...o });
    const { mock } = setup({ deliveries: [d(), d({ id: "d2", status: "failed", attempts: 8, last_status: 502, last_error: "bad gateway" })] });
    renderStudio("/alerts/notifiers");
    const table = await screen.findByRole("table", { name: "Deliveries" }, T);
    expect(within(table).getAllByRole("row")).toHaveLength(3);
    expect(within(table).getByText(/bad gateway/)).toBeInTheDocument();
    await pick(screen.getByLabelText("Delivery status"), "failed");
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Deliveries" })).getAllByRole("row")).toHaveLength(2), T);
    expect(mock.queries.some((q, i) => mock.calls[i] === "get /api/v1/alerts/notifications" && q.get("status") === "failed")).toBe(true);
  });

  it("deletes a notifier after a confirmation that names what is lost", async () => {
    const { st } = setup();
    renderStudio("/alerts/notifiers");
    await userEvent.click(await screen.findByRole("button", { name: "Delete ops-hook" }, T));
    expect(await screen.findByRole("alertdialog", { name: "Delete notifier ops-hook?" }, T)).toHaveTextContent(/delivery history is deleted too/);
    await confirmDialog("Delete notifier ops-hook?", "Delete notifier");
    await toast("Notifier ops-hook deleted");
    expect(st.notifiers).toHaveLength(0);
  });
});

describe("Alerts studio: SLOs", () => {
  it("renders SLI against the objective, the error budget left, burn rates and tiers", async () => {
    setup();
    renderStudio("/alerts/slos");
    const card = await screen.findByRole("article", { name: "SLO records-api" }, T);
    expect(within(card).getByText("99.9% over 30d", { exact: false })).toBeInTheDocument();
    expect(within(card).getByLabelText("SLI of records-api")).toHaveTextContent("99.958%");
    expect(within(card).getByText("meeting objective")).toBeInTheDocument();
    expect(within(card).getByText(/120,000 requests: 119,950 good, 50 bad/)).toBeInTheDocument();
    const bar = within(card).getByRole("progressbar", { name: "Error budget remaining of records-api" });
    expect(bar).toHaveAttribute("aria-valuenow", "58");
    expect(bar).toHaveAttribute("aria-valuetext", "58% left");
    expect(within(card).getByText("58% of the error budget left")).toBeInTheDocument();
    expect(within(card).getByText(/50 of 120 allowed bad requests used/)).toBeInTheDocument();
    const burn = within(card).getByRole("region", { name: "Burn rates of records-api" });
    expect(within(burn).getAllByRole("row").map((r) => r.textContent)).toEqual(["5m0×", "30m1.2×", "1h0.8×", "6h0.4×", "3d0.41×"]);
    const tiers = within(card).getByRole("region", { name: "Alert tiers of records-api" });
    expect(within(tiers).getByText("Fast page")).toBeInTheDocument();
    expect(within(tiers).getAllByText("firing")).toHaveLength(1);
    expect(within(tiers).getByText(/6h and 30m above 6×/)).toBeInTheDocument();
  });

  it("says overspent when the budget is gone and the SLI is under the objective", async () => {
    setup({ slos: [sloStatus({ sli: 0.99, bad: 1200, total: 120000, good: 118800, error_budget: { allowed: 120, consumed: 1200, remaining_ratio: -9 }, burn_rates: { "5m": 20, "1h": 14.4 } })] });
    renderStudio("/alerts/slos");
    const card = await screen.findByRole("article", { name: "SLO records-api" }, T);
    expect(within(card).getByText("below objective")).toBeInTheDocument();
    expect(within(card).getByText("Overspent by 900%")).toBeInTheDocument();
    expect(within(card).getByRole("progressbar")).toHaveAttribute("aria-valuenow", "0");
    expect(within(card).getByText("20×")).toBeInTheDocument();
  });

  it("explains an SLO with no traffic and no burn data", async () => {
    setup({ slos: [sloStatus({ total: 0, good: 0, bad: 0, sli: null, error_budget: { allowed: 0, consumed: 0, remaining_ratio: 1 }, burn_rates: { "5m": null, "30m": null }, alerts: [] })] });
    renderStudio("/alerts/slos");
    const card = await screen.findByRole("article", { name: "SLO records-api" }, T);
    expect(within(card).getByText(/No matching requests in the window yet/)).toBeInTheDocument();
    expect(within(card).getAllByText("no data")).toHaveLength(2);
    expect(within(card).queryByLabelText("SLI of records-api")).not.toBeInTheDocument();
  });

  it("describes a latency SLO with its threshold and routes", async () => {
    setup({ slos: [sloStatus({ slo: slo({ name: "checkout-latency", kind: "latency", objective: 0.99, window: "28d", route_prefix: null, route_regex: "/api/v1/functions/checkout/.*", methods: ["POST"], latency_threshold_ms: 300 }) })] });
    renderStudio("/alerts/slos");
    const card = await screen.findByRole("article", { name: "SLO checkout-latency" }, T);
    expect(card).toHaveTextContent("99% over 28d, answered within 300 ms · routes matching /api/v1/functions/checkout/.* · POST");
  });

  it("creates an SLO: the objective is typed as a percentage and sent as a ratio", async () => {
    const { mock, st } = setup({ slos: [] });
    renderStudio("/alerts/slos");
    expect(await screen.findByText("No SLOs yet", {}, T)).toBeInTheDocument();
    await userEvent.click(screen.getAllByRole("button", { name: "New SLO" })[0]!);
    const form = await sheet("New SLO");
    await userEvent.type(within(form).getByLabelText(/^Name/), "records-api");
    fireEvent.change(within(form).getByLabelText(/^Objective/), { target: { value: "99.95" } });
    await pick(within(form).getByLabelText(/^Routes/), "prefix");
    await userEvent.type(within(form).getByLabelText(/^Route prefix/), "/api/v1/collections");
    await userEvent.click(within(form).getByRole("button", { name: "Create SLO" }));
    await toast("SLO records-api created");
    expect(lastBody(mock, "post /api/v1/slos")).toMatchObject({ name: "records-api", kind: "availability", objective: 0.9995, window: "30d", route_prefix: "/api/v1/collections", alerting: true });
    expect(lastBody(mock, "post /api/v1/slos")).not.toHaveProperty("latency_threshold_ms");
    expect(st.slos).toHaveLength(1);
    expect(await screen.findByRole("article", { name: "SLO records-api" }, T)).toBeInTheDocument();
  });

  it("asks for a latency threshold on a latency SLO and checks the objective range", async () => {
    const { mock } = setup({ slos: [] });
    renderStudio("/alerts/slos?new=1");
    const form = await sheet("New SLO");
    await userEvent.type(within(form).getByLabelText(/^Name/), "lat");
    await pick(within(form).getByLabelText(/^Kind/), "latency");
    expect(within(form).getByLabelText(/^Latency threshold/)).toHaveValue("300");
    fireEvent.change(within(form).getByLabelText(/^Objective/), { target: { value: "50" } });
    expect(within(form).getByText(/Use a percentage from 90 to 99.999/)).toBeInTheDocument();
    expect(within(form).getByRole("button", { name: "Create SLO" })).toBeDisabled();
    fireEvent.change(within(form).getByLabelText(/^Objective/), { target: { value: "99" } });
    await userEvent.click(within(form).getByRole("button", { name: "Create SLO" }));
    await waitFor(() => expect(mock.bodies["post /api/v1/slos"]).toBeDefined(), T);
    expect(lastBody(mock, "post /api/v1/slos")).toMatchObject({ kind: "latency", objective: 0.99, latency_threshold_ms: 300 });
  });

  it("edits an SLO with PUT and deletes it after a confirmation", async () => {
    const { mock, st } = setup();
    renderStudio("/alerts/slos");
    await userEvent.click(await screen.findByRole("button", { name: "Edit records-api" }, T));
    const form = await sheet("Edit SLO records-api");
    expect(within(form).getByLabelText(/^Objective/)).toHaveValue("99.9");
    fireEvent.change(within(form).getByLabelText(/^Objective/), { target: { value: "99.5" } });
    await userEvent.click(within(form).getByRole("button", { name: "Save SLO" }));
    await waitFor(() => expect(mock.bodies["put /api/v1/slos/{id}"]).toBeDefined(), T);
    expect(lastBody(mock, "put /api/v1/slos/{id}")).toMatchObject({ objective: 0.995 });
    await userEvent.click(await screen.findByRole("button", { name: "Delete records-api" }, T));
    await confirmDialog("Delete SLO records-api?", "Delete SLO");
    await toast("SLO records-api deleted");
    expect(st.slos).toHaveLength(0);
  });
});

describe("Alerts studio: access and switches", () => {
  it("explains a switched-off alerting engine (404 feature_disabled)", async () => {
    const mock = signedIn(new MockApi().install());
    mock.on("GET", "/api/v1/alerts", () => envelope(404, "feature_disabled", "alerting is switched off ([alerts] enabled = false)"));
    renderStudio("/alerts");
    expect(await screen.findByText(/This feature is switched off|alerting is switched off/, {}, T)).toBeInTheDocument();
  });

  it("is a studio in the Operate group and each tab is a link", async () => {
    setup();
    renderStudio("/alerts");
    await screen.findByRole("article", { name: "QueuePushPaused firing" }, T);
    const nav = screen.getByRole("navigation", { name: /studios|main/i });
    expect(within(nav).getByRole("link", { name: "Alerts" })).toHaveAttribute("href", "/alerts");
    for (const [name, href] of [["History", "/alerts/history"], ["Rules", "/alerts/rules"], ["Silences", "/alerts/silences"], ["Notifiers", "/alerts/notifiers"], ["SLOs", "/alerts/slos"]]) {
      expect(screen.getByRole("tab", { name })).toHaveAttribute("href", href);
    }
  });
});
