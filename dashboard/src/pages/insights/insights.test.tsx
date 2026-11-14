import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi, envelope } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { audit, fnStat, installInsights, invocation, stmt, usageRow, type InsightsState } from "../../test/insightsMock";
import { pick } from "../../test/pick";
import { auditExportUrl } from "../../api";
import { crumbsFor } from "../../components/crumbs";
import type { Regression, SlowStatement } from "../../api";
import Timeline from "./Timeline";
import { fmtMetric, fmtMs, fmtRate, fmtUs, localToIso, oneLine, rangeOf, statementHref } from "./util";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

const T = { timeout: 15_000 };

function setup(init: Partial<InsightsState> = {}) {
  const mock = signedIn(new MockApi().install());
  return { mock, st: installInsights(mock, init) };
}
/** The query string of the last request to `path`. */
const lastQuery = (mock: MockApi, path: string) => mock.queries.filter((_, i) => mock.calls[i] === `get ${path}`).at(-1)!;

describe("insights helpers", () => {
  it("formats milliseconds, microseconds, rates and units", () => {
    expect([fmtMs(0.423), fmtMs(4.56), fmtMs(250.4), fmtMs(1520), fmtMs(90_000), fmtMs(null)]).toEqual(["0.42 ms", "4.6 ms", "250 ms", "1.52 s", "1.5 min", "—"]);
    expect(fmtUs(1500)).toBe("1.5 ms");
    expect([fmtRate(0.034), fmtRate(0.0002), fmtRate(0)]).toEqual(["3.4%", "0.02%", "0%"]);
    expect(fmtMetric(5_242_880, "bytes")).toBe("5.00 MiB");
    expect(fmtMetric(12_500, "calls")).toBe("12,500 calls");
    expect(fmtMetric(7, "count")).toBe("7");
  });
  it("cuts a statement to one line and builds its link", () => {
    expect(oneLine("select\n  a\n from  t")).toBe("select a from t");
    expect(oneLine("x".repeat(200), 10)).toBe("xxxxxxxxx…");
    expect(statementHref("a/b c")).toBe("/insights/queries/a%2Fb%20c");
  });
  it("gives a preset range ending now and a local time as UTC", () => {
    const r = rangeOf("6h", Date.parse("2026-11-14T12:00:00Z"));
    expect(r).toEqual({ from: "2026-11-14T06:00:00.000Z", to: "2026-11-14T12:00:00.000Z" });
    expect(localToIso("")).toBeUndefined();
    expect(localToIso("nope")).toBeUndefined();
    expect(localToIso("2026-11-14T09:30")).toBe(new Date("2026-11-14T09:30").toISOString());
  });
  it("builds the audit export URL from the filters and the format", () => {
    expect(auditExportUrl({ actor: "user:users/42", outcome: "failure", from: "2026-11-14T00:00:00.000Z" }, "jsonl", 500)).toBe(
      "/api/v1/insights/audit/export?actor=user%3Ausers%2F42&outcome=failure&from=2026-11-14T00%3A00%3A00.000Z&format=jsonl&max_rows=500",
    );
    expect(auditExportUrl({}, "csv")).toBe("/api/v1/insights/audit/export?format=csv");
  });
});

describe("Insights studio: statements", () => {
  it("lists shapes with calls, errors, percentiles and total time, and shows the writer's status line", async () => {
    const { mock } = setup();
    renderStudio("/insights");
    const table = await screen.findByRole("table", { name: "Statements by total time" }, T);
    const rows = within(table).getAllByRole("row");
    expect(rows).toHaveLength(3);
    expect(within(rows[1]!).getByRole("link", { name: /select name from users/ })).toHaveAttribute("href", "/insights/queries/fp-users");
    expect(rows[1]).toHaveTextContent("1,200");
    expect(rows[1]).toHaveTextContent("2.1 ms");
    expect(rows[1]).toHaveTextContent("12.3 ms");
    expect(rows[1]).toHaveTextContent("88.8 ms");
    expect(rows[1]).toHaveTextContent("5.40 s");
    expect(await screen.findByText(/Slow means over 200 ms · written every 60 s · 37 statement shapes in memory · kept 14 days/, {}, T)).toBeInTheDocument();
    const q = lastQuery(mock, "/api/v1/insights/queries");
    expect(q.get("sort")).toBe("total_time");
    expect(Date.parse(q.get("to")!) - Date.parse(q.get("from")!)).toBe(3_600_000);
    expect(mock.problems).toEqual([]);
  });

  it("sends the area, sort, range and text filters", async () => {
    const { mock } = setup();
    renderStudio("/insights");
    await screen.findByRole("table", { name: "Statements by total time" }, T);
    await pick(screen.getByLabelText("Area"), "jobs");
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/queries").get("area")).toBe("jobs"), T);
    await pick(screen.getByLabelText("Sort by"), "p95");
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/queries").get("sort")).toBe("p95"), T);
    await pick(screen.getByLabelText("Time range"), "24h");
    await waitFor(() => { const q = lastQuery(mock, "/api/v1/insights/queries"); expect(Date.parse(q.get("to")!) - Date.parse(q.get("from")!)).toBe(86_400_000); }, T);
    await userEvent.type(screen.getByLabelText("Search statement text"), "update");
    await userEvent.click(within(screen.getByRole("search", { name: "Filter statements" })).getByRole("button", { name: "Search" }));
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/queries").get("q")).toBe("update"), T);
    const table = await screen.findByRole("table", { name: "Statements by total time" }, T);
    await waitFor(() => expect(within(table).getAllByRole("row")).toHaveLength(2), T);
  });

  it("pages by offset", async () => {
    const many = Array.from({ length: 30 }, (_, i) => stmt({ fingerprint: `fp-${i}`, statement: `select ${i} from t where x = ?` }));
    const { mock } = setup({ statements: many });
    renderStudio("/insights");
    const table = await screen.findByRole("table", { name: "Statements by total time" }, T);
    expect(within(table).getAllByRole("row")).toHaveLength(26);
    await userEvent.click(within(screen.getByRole("navigation", { name: "Statements pages" })).getByRole("button", { name: /next/i }));
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/queries").get("offset")).toBe("25"), T);
  });

  it("opens a statement: totals, percentiles, full text and series charts", async () => {
    setup();
    renderStudio("/insights/queries/fp-users");
    expect(await screen.findByText("select name from users where id = ? and name = ?", {}, T)).toBeInTheDocument();
    const facts = screen.getByLabelText("Statement facts");
    expect(facts).toHaveTextContent("1,200");
    expect(facts).toHaveTextContent("3 (0.3%)");
    expect(facts).toHaveTextContent("2.1 ms · 12.3 ms · 88.8 ms");
    expect(screen.getByRole("img", { name: /^Latency\. p95: latest 25\.0 ms, peak 25\.0 ms; mean: latest 9\.0 ms/ })).toBeInTheDocument();
    expect(screen.getByRole("img", { name: /^Calls and errors/ })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "← All statements" })).toHaveAttribute("href", "/insights");
  });

  it("says when a statement has no calls in the range (404)", async () => {
    setup();
    renderStudio("/insights/queries/nope");
    expect(await screen.findByText("Not found", {}, T)).toBeInTheDocument();
  });

  it("lists regressions with the change, and says nothing regressed otherwise", async () => {
    const reg: Regression = {
      kind: "latency", fingerprint: "fp-jobs", statement: "update jobs set status = ? where id = ?", area: "jobs", backend: "sqlite", ratio: 3.46, delta_ms: 80, extra_ms: 7200,
      recent: { calls: 90, errors: 0, error_rate: 0, mean_ms: 60, p50_ms: 50, p95_ms: 110, p99_ms: 130 }, baseline: { calls: 900, errors: 0, error_rate: 0, mean_ms: 20, p50_ms: 15, p95_ms: 30, p99_ms: 40 },
    };
    const err: Regression = { ...reg, kind: "errors", fingerprint: "fp-users", recent: { ...reg.recent, error_rate: 0.2 }, baseline: { ...reg.baseline, error_rate: 0.001 } };
    const { mock } = setup({ regressions: [reg, err] });
    renderStudio("/insights/regressions");
    const table = await screen.findByRole("table", { name: "Regressions" }, T);
    const rows = within(table).getAllByRole("row");
    expect(rows[1]).toHaveTextContent("slower");
    expect(rows[1]).toHaveTextContent("110 ms");
    expect(rows[1]).toHaveTextContent("30.0 ms");
    expect(rows[1]).toHaveTextContent("×3.5");
    expect(rows[1]).toHaveTextContent("7.20 s");
    expect(rows[2]).toHaveTextContent("0.1% → 20%");
    await pick(screen.getByLabelText("Recent window"), "21600");
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/queries/regressions").get("window")).toBe("21600"), T);
  });

  it("says so when nothing regressed", async () => {
    setup();
    renderStudio("/insights/regressions");
    expect(await screen.findByText("No regressions", {}, T)).toBeInTheDocument();
  });

  it("reads the slow log a page at a time with the cursor", async () => {
    const slow: SlowStatement[] = Array.from({ length: 60 }, (_, i) => ({ id: 1000 - i, at: "2026-11-14T10:00:00Z", fingerprint: "fp-users", statement: `select slow ${i} from t where a = ?`, area: "data", backend: "sqlite", duration_ms: 300 + i, rows: i, error: i === 0, write: i === 1 }));
    const { mock } = setup({ slow });
    renderStudio("/insights/slow");
    const table = await screen.findByRole("table", { name: "Slow statements" }, T);
    expect(within(table).getAllByRole("row")).toHaveLength(51);
    expect(within(table).getByText("error")).toBeInTheDocument();
    expect(within(table).getByText("write")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: /load more/i }));
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Slow statements" })).getAllByRole("row")).toHaveLength(61), T);
    expect(lastQuery(mock, "/api/v1/insights/slow-queries").get("cursor")).toBe("951");
    expect(screen.queryByRole("button", { name: /load more/i })).not.toBeInTheDocument();
  });

  it("filters the slow log by minimum time", async () => {
    const { mock } = setup();
    renderStudio("/insights/slow");
    await screen.findByText("No slow statements", {}, T);
    await userEvent.type(screen.getByLabelText("At least (ms)"), "500");
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/slow-queries").get("min_ms")).toBe("500"), T);
  });

  it("flushes the statistics on demand and reports what was written", async () => {
    const { st } = setup();
    renderStudio("/insights");
    await userEvent.click(await screen.findByRole("button", { name: "Flush statistics now" }, T));
    const note = await screen.findByText(/Wrote 37 statement shapes, 2 slow statements and 1 function profile/, {}, T);
    expect(note.closest("[role=status]")).toBeInTheDocument();
    expect(st.flushes).toBe(1);
  });

  it("warns when statement statistics are switched off", async () => {
    setup({ status: { enabled: false, slow_query_ms: 200, flush_interval_secs: 60, top_n: 200, retention_days: 14, fingerprints_in_memory: 0, unflushed_since: "2026-11-14T10:00:00Z", stored_rows: 0, stored_slow_statements: 0 } });
    renderStudio("/insights");
    expect(await screen.findByText("Statement statistics are off", {}, T)).toBeInTheDocument();
  });

  it("explains a missing permission (403) instead of showing a blank page", async () => {
    const mock = signedIn(new MockApi().install());
    mock.on("GET", "/api/v1/insights/queries", () => envelope(403, "forbidden", "missing permission engine:read_insights"));
    renderStudio("/insights");
    expect((await screen.findAllByText(/engine:read_insights/, {}, T)).length).toBeGreaterThan(0);
  });
});

describe("Insights studio: usage and cost", () => {
  it("labels cost as an estimate everywhere it appears", async () => {
    const { mock } = setup();
    renderStudio("/insights/usage");
    const note = await screen.findByText("Costs are estimates", {}, T);
    expect(note.closest("[role=note]")).toHaveTextContent(/priced with the metered price .* as if the period were one billing period/);
    const table = await screen.findByRole("table", { name: "Usage" }, T);
    expect(within(table).getByRole("columnheader", { name: /Estimated cost/ })).toHaveTextContent("estimate");
    const row = within(table).getAllByRole("row")[1]!;
    expect(row).toHaveTextContent("12.30 USD");
    expect(row).toHaveTextContent("estimate");
    expect(row).toHaveTextContent("40 credits");
    expect(row).toHaveTextContent("12,500 calls");
    expect(row).toHaveTextContent("main / acme / shop");
    expect(mock.problems).toEqual([]);
  });

  it("sends the scope, period, metric and N, and prices by cost when asked", async () => {
    const { mock } = setup();
    renderStudio("/insights/usage");
    await screen.findByRole("table", { name: "Usage" }, T);
    const q0 = lastQuery(mock, "/api/v1/insights/usage/top");
    expect(q0.get("group_by")).toBe("environment");
    expect(q0.get("period")).toBe("month");
    expect(q0.get("metric")).toBe("functions.invocations");
    expect(q0.get("n")).toBe("10");
    expect(q0.get("date")).toMatch(/^\d{4}-\d{2}$/);
    await pick(screen.getByLabelText("Scope"), "org");
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/usage/top").get("group_by")).toBe("org"), T);
    await pick(screen.getByLabelText("Metric"), "cost");
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/usage/top").get("metric")).toBe("cost"), T);
    expect(await screen.findByText("Top 10 by estimated cost", {}, T)).toBeInTheDocument();
    await pick(screen.getByLabelText("How many"), "25");
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/usage/top").get("n")).toBe("25"), T);
    await pick(screen.getByLabelText("Period"), "day");
    await waitFor(() => { const q = lastQuery(mock, "/api/v1/insights/usage/top"); expect(q.get("period")).toBe("day"); expect(q.get("date")).toMatch(/^\d{4}-\d{2}-\d{2}$/); }, T);
  });

  it("shows gauges with their peak and bytes in readable units", async () => {
    setup();
    renderStudio("/insights/usage");
    await screen.findByRole("table", { name: "Usage" }, T);
    await pick(screen.getByLabelText("Metric"), "storage.bytes");
    const table = await screen.findByRole("table", { name: "Usage" }, T);
    await waitFor(() => expect(within(table).getAllByRole("row")[1]).toHaveTextContent("5.00 MiB"), T);
    expect(within(table).getAllByRole("row")[1]).toHaveTextContent("peak 6.00 MiB");
  });

  it("says no cost can be estimated without prices, and still shows the usage", async () => {
    setup({ costAvailable: false });
    renderStudio("/insights/usage");
    const table = await screen.findByRole("table", { name: "Usage" }, T);
    expect(screen.getByText(/No cost can be estimated/)).toBeInTheDocument();
    expect(within(table).getAllByRole("row")[1]).toHaveTextContent("12,500 calls");
    expect(within(table).getAllByRole("row")[1]).not.toHaveTextContent("USD");
  });

  it("marks usage that is not attributed to an environment", async () => {
    setup({ usage: [usageRow({ id: "org:acme", name: "acme", level: "org", attributed: false, realm: null, project: null })] });
    renderStudio("/insights/usage");
    await screen.findByRole("table", { name: "Usage" }, T);
    expect(screen.getByText("not attributed")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Daily series/ })).not.toBeInTheDocument();
  });

  it("draws the daily series of an environment", async () => {
    const { mock } = setup();
    renderStudio("/insights/usage");
    await userEvent.click(await screen.findByRole("button", { name: "Daily series of production" }, T));
    const section = await screen.findByRole("region", { name: "Daily values of production" }, T);
    await within(section).findByRole("img", { name: /functions\.invocations/ }, T);
    expect(lastQuery(mock, "/api/v1/insights/usage/series").get("scope")).toBe("env:env1");
    expect(within(section).getByRole("img")).toHaveAccessibleName(/peak 140/);
    await pick(within(section).getByLabelText("Series metric"), "storage.bytes");
    expect(await within(section).findByRole("img", { name: /storage\.bytes/ }, T)).toBeInTheDocument();
    await userEvent.click(within(section).getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("region", { name: "Daily values of production" })).not.toBeInTheDocument();
  });

  it("says so when there is no usage in the period", async () => {
    setup({ usage: [] });
    renderStudio("/insights/usage");
    expect(await screen.findByText("No usage in this period", {}, T)).toBeInTheDocument();
  });
});

describe("Insights studio: functions", () => {
  it("shows runs, errors, cold starts, p95, fuel against its limit and memory", async () => {
    setup();
    renderStudio("/insights/functions");
    const table = await screen.findByRole("table", { name: "Function profiles" }, T);
    const row = within(table).getAllByRole("row")[1]!;
    expect(within(row).getByRole("link", { name: "checkout" })).toHaveAttribute("href", "/insights/functions/checkout");
    expect(row).toHaveTextContent("v3");
    expect(row).toHaveTextContent("500");
    expect(row).toHaveTextContent("12 (2.4%)");
    expect(row).toHaveTextContent("2 timeouts");
    expect(row).toHaveTextContent("40.0 ms");
    expect(row).toHaveTextContent("4.00 MiB");
    const fuel = within(row).getByRole("meter", { name: "Fuel use of checkout" });
    expect(fuel).toHaveAttribute("aria-valuenow", "80");
    expect(fuel).toHaveAttribute("aria-valuetext", "80% of the fuel limit");
    expect(row).toHaveTextContent("80% of limit");
    expect(within(row).getAllByRole("cell")[3]).toHaveTextContent("7");
  });

  it("opens a function: the host-call breakdown by version and the series", async () => {
    setup();
    renderStudio("/insights/functions/checkout");
    const v = await screen.findByRole("article", { name: "Version 3" }, T);
    expect(within(v).getByText("7 cold starts")).toBeInTheDocument();
    const calls = within(v).getByRole("table", { name: "Host calls of version 3" });
    const rows = within(calls).getAllByRole("row");
    expect(rows[1]).toHaveTextContent("db.query");
    expect(rows[1]).toHaveTextContent("1,200");
    expect(rows[1]).toHaveTextContent("70%");
    expect(rows[2]).toHaveTextContent("kv.get");
    expect(within(v).getByLabelText("Version 3 facts")).toHaveTextContent("800,000");
    expect(within(v).getByRole("meter", { name: "Fuel use of checkout" })).toHaveAttribute("aria-valuenow", "80");
    expect(screen.getByRole("img", { name: /^Runs and errors/ })).toBeInTheDocument();
  });

  it("filters profiles by function name", async () => {
    const { mock } = setup({ fnStats: [fnStat(), fnStat({ function: "emails", version: 1 })] });
    renderStudio("/insights/functions");
    await screen.findByRole("table", { name: "Function profiles" }, T);
    await userEvent.type(screen.getByLabelText("Function name"), "emails");
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/functions").get("function")).toBe("emails"), T);
  });

  it("lists recent invocations and opens the host-call timeline of one", async () => {
    setup({ invocations: [invocation()] });
    renderStudio("/insights/functions");
    const table = await screen.findByRole("table", { name: "Recent invocations" }, T);
    expect(within(table).getAllByRole("row")[1]).toHaveTextContent("cold");
    await userEvent.click(within(table).getByRole("button", { name: "Timeline of invocation 41" }));
    const timeline = await screen.findByRole("list", { name: "Host call timeline" }, T);
    const calls = within(timeline).getAllByRole("listitem");
    expect(calls.map((c) => c.getAttribute("aria-label"))).toEqual([
      "db.query at 1.0 ms for 0.50 ms, ok", "kv.get at 2.0 ms for 0.10 ms, ok", "db.query at 5.0 ms for 2.0 ms, failed",
    ]);
    const dialog = screen.getByRole("dialog");
    expect(dialog).toHaveTextContent("cold start");
    expect(dialog).toHaveTextContent("21,000 of 1,000,000");
    expect(dialog).toHaveTextContent("2.00 MiB of 64.0 MiB");
    expect(dialog).toHaveTextContent("0.90 ms"); // load time of the cold start
    expect(dialog).toHaveTextContent("order 7 placed");
  });

  it("says when a run has no profile", async () => {
    setup({ invocations: [invocation({ profile: null })] });
    renderStudio("/insights/functions");
    await userEvent.click(await screen.findByRole("button", { name: "Timeline of invocation 41" }, T));
    expect(await screen.findByText(/No profile was recorded for this run/, {}, T)).toBeInTheDocument();
  });

  it("filters invocations to failures and pages with the cursor", async () => {
    const many = Array.from({ length: 55 }, (_, i) => invocation({ id: 500 - i, status: i % 5 === 0 ? "timeout" : "ok" }));
    const { mock } = setup({ invocations: many });
    renderStudio("/insights/functions");
    const table = await screen.findByRole("table", { name: "Recent invocations" }, T);
    expect(within(table).getAllByRole("row")).toHaveLength(51);
    await userEvent.click(screen.getByRole("button", { name: /load more/i }));
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/functions/invocations").get("cursor")).toBe("451"), T);
    await userEvent.click(screen.getByRole("checkbox", { name: "Only failures" }));
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/functions/invocations").get("errors")).toBe("true"), T);
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Recent invocations" })).getAllByRole("row")).toHaveLength(12), T);
  });
});

describe("host-call timeline", () => {
  const profile = invocation().profile!;
  it("draws one row per call on a shared scale, red when it failed", () => {
    render(<Timeline profile={profile} />);
    const items = screen.getAllByRole("listitem");
    expect(items).toHaveLength(3);
    const bars = items.map((li) => li.querySelector<HTMLElement>("span[aria-hidden] > span")!);
    expect(bars[0]).toHaveClass("bg-primary");
    expect(bars[2]).toHaveClass("bg-destructive");
    // 18 ms is the scale: the second call starts at 2 ms and the third at 5 ms.
    expect(parseFloat(bars[1]!.style.left)).toBeCloseTo((2000 / 18000) * 100, 1);
    expect(parseFloat(bars[2]!.style.width)).toBeCloseTo((2000 / 18000) * 100, 1);
    expect(screen.getByText("18.0 ms")).toBeInTheDocument();
  });
  it("notes the calls beyond the 64 the timeline keeps, and a run with none", () => {
    const { rerender } = render(<Timeline profile={{ ...profile, timeline_dropped: 5 }} />);
    expect(screen.getByText(/5 more calls are counted but not drawn/)).toBeInTheDocument();
    rerender(<Timeline profile={{ ...profile, timeline: [], host_calls: 0 }} />);
    expect(screen.getByText("This run made no host calls.")).toBeInTheDocument();
  });
});

describe("Insights studio: audit", () => {
  it("searches with every filter, in UTC for the time range, and lists the results", async () => {
    const { mock } = setup({ audit: [audit(), audit({ id: 11, event: "api_key.create", outcome: "failure", actor: "admin:adm1", ip: "198.51.100.2" })] });
    renderStudio("/insights/audit");
    const table = await screen.findByRole("table", { name: "Audit entries" }, T);
    expect(within(table).getAllByRole("row")).toHaveLength(3);
    const form = screen.getByRole("search", { name: "Search audit entries" });
    await userEvent.type(within(form).getByLabelText("Actor"), "admin:adm1");
    await userEvent.type(within(form).getByLabelText("Action"), "api_key.");
    await userEvent.type(within(form).getByLabelText("Resource type"), "api_key");
    await userEvent.type(within(form).getByLabelText("Resource id"), "k1");
    await pick(within(form).getByLabelText("Outcome"), "failure");
    await userEvent.type(within(form).getByLabelText("IP address"), "198.51.100.2");
    await userEvent.type(within(form).getByLabelText("Request id"), "req-7");
    await userEvent.type(within(form).getByLabelText(/^Contains text/), "rotated");
    fireEvent.change(within(form).getByLabelText(/^From/), { target: { value: "2026-11-14T09:30" } });
    fireEvent.change(within(form).getByLabelText(/^To/), { target: { value: "2026-11-14T10:30" } });
    await pick(within(form).getByLabelText("Order"), "asc");
    await userEvent.click(within(form).getByRole("button", { name: "Search" }));
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/audit").get("actor")).toBe("admin:adm1"), T);
    const q = lastQuery(mock, "/api/v1/insights/audit");
    expect(Object.fromEntries(q.entries())).toMatchObject({
      actor: "admin:adm1", action: "api_key.", resource_type: "api_key", resource_id: "k1", outcome: "failure", ip: "198.51.100.2", request_id: "req-7", q: "rotated", order: "asc", limit: "50",
      from: new Date("2026-11-14T09:30").toISOString(), to: new Date("2026-11-14T10:30").toISOString(),
    });
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Audit entries" })).getAllByRole("row")).toHaveLength(2), T);
    expect(await screen.findByText("Oldest first.")).toBeInTheDocument();
  });

  it("gives CSV and JSON lines download links that carry the filters", async () => {
    setup();
    renderStudio("/insights/audit");
    const group = await screen.findByRole("group", { name: "Export what matches" }, T);
    const csv = within(group).getByRole("link", { name: "Export CSV" });
    const jsonl = within(group).getByRole("link", { name: "Export JSON lines" });
    expect(csv).toHaveAttribute("href", "/api/v1/insights/audit/export?format=csv");
    expect(csv).toHaveAttribute("download");
    expect(jsonl).toHaveAttribute("href", "/api/v1/insights/audit/export?format=jsonl");
    await userEvent.type(screen.getByLabelText("Actor"), "user:users/42");
    await pick(screen.getByLabelText("Outcome"), "failure");
    await userEvent.click(within(screen.getByRole("search", { name: "Search audit entries" })).getByRole("button", { name: "Search" }));
    await waitFor(() => expect(screen.getByRole("link", { name: "Export CSV" })).toHaveAttribute("href", "/api/v1/insights/audit/export?actor=user%3Ausers%2F42&outcome=failure&format=csv"), T);
    expect(screen.getByRole("link", { name: "Export JSON lines" })).toHaveAttribute("href", "/api/v1/insights/audit/export?actor=user%3Ausers%2F42&outcome=failure&format=jsonl");
    expect(screen.getByText(/prefixed with '/)).toBeInTheDocument();
  });

  it("clears the filters and the export link with them", async () => {
    setup();
    renderStudio("/insights/audit");
    await userEvent.type(await screen.findByLabelText("Actor", {}, T), "x");
    await userEvent.click(within(screen.getByRole("search", { name: "Search audit entries" })).getByRole("button", { name: "Search" }));
    await waitFor(() => expect(screen.getByRole("link", { name: "Export CSV" })).toHaveAttribute("href", expect.stringContaining("actor=x")), T);
    await userEvent.click(screen.getByRole("button", { name: "Clear" }));
    await waitFor(() => expect(screen.getByRole("link", { name: "Export CSV" })).toHaveAttribute("href", "/api/v1/insights/audit/export?format=csv"), T);
    expect(screen.getByLabelText("Actor")).toHaveValue("");
  });

  it("shows how many entries there are and how many failed", async () => {
    setup();
    renderStudio("/insights/audit");
    expect(await screen.findByText(/1 entry in this period/, {}, T)).toHaveTextContent("2 failed");
  });

  it("pages with the cursor", async () => {
    const many = Array.from({ length: 60 }, (_, i) => audit({ id: 900 - i, event: `event.${i}` }));
    const { mock } = setup({ audit: many });
    renderStudio("/insights/audit");
    const table = await screen.findByRole("table", { name: "Audit entries" }, T);
    expect(within(table).getAllByRole("row")).toHaveLength(51);
    await userEvent.click(screen.getByRole("button", { name: /load more/i }));
    await waitFor(() => expect(lastQuery(mock, "/api/v1/insights/audit").get("cursor")).toBe("851"), T);
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Audit entries" })).getAllByRole("row")).toHaveLength(61), T);
  });

  it("opens the events around an entry: the same request and the same actor", async () => {
    const e = audit({ id: 10, request_id: "req-1", impersonator: "admin:adm1" });
    const { mock } = setup({
      audit: [e],
      related: {
        same_request: [audit({ id: 9, event: "token.issue", request_id: "req-1" })],
        same_actor: [audit({ id: 8, event: "login", outcome: "failure" }), audit({ id: 12, event: "logout" })],
      },
    });
    renderStudio("/insights/audit");
    await userEvent.click(await screen.findByRole("button", { name: "Related events of entry 10" }, T));
    const req = await screen.findByRole("region", { name: "Same request" }, T);
    expect(within(req).getByText("token.issue")).toBeInTheDocument();
    const actor = screen.getByRole("region", { name: "Same actor, around the same time" });
    expect(within(actor).getByText("logout")).toBeInTheDocument();
    expect(within(actor).getByText("failure")).toBeInTheDocument();
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByLabelText("Audit entry facts")).toHaveTextContent("Impersonated by");
    expect(within(dialog).getByLabelText("Audit entry facts")).toHaveTextContent("session · users · 42");
    expect(within(dialog).getByLabelText("Entry detail")).toHaveTextContent('"method": "password"');
    expect(mock.calls).toContain("get /api/v1/insights/audit/10/related");
  });

  it("says so when nothing else is related", async () => {
    setup();
    renderStudio("/insights/audit");
    await userEvent.click(await screen.findByRole("button", { name: "Related events of entry 10" }, T));
    expect(await screen.findByText("Nothing else was logged for this request.", {}, T)).toBeInTheDocument();
    expect(screen.getByText("This actor did nothing else within five minutes.")).toBeInTheDocument();
  });

  it("says no entries match", async () => {
    setup({ audit: [] });
    renderStudio("/insights/audit");
    expect(await screen.findByText("No entries match", {}, T)).toBeInTheDocument();
  });
});

describe("Insights studio: navigation", () => {
  it("has breadcrumbs for a statement and a function that lead back to their lists", () => {
    expect(crumbsFor("/insights/queries/fp-users")).toEqual([{ label: "Insights", to: "/insights" }, { label: "Statements", to: "/insights" }, { label: "fp-users" }]);
    expect(crumbsFor("/insights/functions/checkout")).toEqual([{ label: "Insights", to: "/insights" }, { label: "Functions", to: "/insights/functions" }, { label: "Checkout" }]);
  });

  it("has one tab per area, and the studio is in the nav", async () => {
    setup();
    renderStudio("/insights");
    await screen.findByRole("table", { name: "Statements by total time" }, T);
    for (const [name, href] of [["Statements", "/insights"], ["Regressions", "/insights/regressions"], ["Slow log", "/insights/slow"], ["Usage and cost", "/insights/usage"], ["Functions", "/insights/functions"], ["Audit", "/insights/audit"]]) {
      expect(screen.getByRole("tab", { name })).toHaveAttribute("href", href);
    }
    expect(screen.getByRole("heading", { name: "Insights" })).toBeInTheDocument();
  });
});

