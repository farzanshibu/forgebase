/**
 * In-memory fake of `/api/v1/alerts` and `/api/v1/slos` (ADR-0035), built from crates/forgebase-alerts (model.rs, http.rs).
 * The SDK OpenAPI copies are stale, so these routes are custom (`mock.on`), not contract-checked.
 */
import type { Alert, AlertRule, Notifier, NotifierDelivery, Silence, SloStatus } from "../api";
import { envelope, type MockApi, type MockRequest } from "./mockApi";

const T0 = "2026-11-14T10:00:00.000Z";

export const alert = (o: Partial<Alert> = {}): Alert => ({
  id: "alr-1", fingerprint: "fp1", rule_id: "queue_push_paused", name: "QueuePushPaused", severity: "critical", state: "firing",
  labels: { alertname: "QueuePushPaused", severity: "critical", subscription: "orders", topic: "events" },
  annotations: { summary: "Push delivery of orders is paused", runbook_url: "https://wiki.example.com/push" },
  value: 1, active_at: T0, fired_at: T0, resolved_at: null, last_evaluated_at: T0, silenced_by: [], ...o,
});

export const rule = (o: Partial<AlertRule> = {}): AlertRule => ({
  id: "http_5xx_ratio_high", name: "HttpServerErrorsHigh", source: "builtin", expr: "sum(rate(forgebase_http_requests_total{status=~\"5..\"}[5m]))", op: ">", threshold: 0.05,
  for: "5m", for_ms: 300_000, severity: "warning", labels: {}, annotations: {}, enabled: true, disabled_by_config: false, ...o,
});

export const notifier = (o: Partial<Notifier> = {}): Notifier => ({
  id: "ntf-1", name: "ops-hook", kind: "webhook", url: "https://hooks.example.com/forge", to: [], topic: null, matchers: [], min_severity: "info", group_by: ["alertname"],
  repeat_interval: "4h", send_resolved: true, enabled: true, last_success_at: T0, last_failure_at: null, last_error: null, created_at: T0, updated_at: T0, ...o,
});

export const slo = (o: Partial<SloStatus["slo"]> = {}): SloStatus["slo"] => ({
  id: "slo-1", name: "records-api", description: "Records API availability", kind: "availability", objective: 0.999, window: "30d", window_ms: 2_592_000_000,
  route_prefix: "/api/v1/collections", route_regex: null, methods: [], latency_threshold_ms: null, alerting: true, labels: {}, created_at: T0, updated_at: T0, ...o,
});

export const sloStatus = (o: Partial<SloStatus> = {}): SloStatus => ({
  slo: slo(), total: 120_000, good: 119_950, bad: 50, sli: 0.99958, error_budget: { allowed: 120, consumed: 50, remaining_ratio: 0.583 },
  burn_rates: { "5m": 0, "30m": 1.2, "1h": 0.8, "6h": 0.4, "3d": 0.41 },
  alerts: [
    { rule_id: "slo:records-api:page_fast", tier: "page_fast", long_window: "1h", short_window: "5m", burn_rate_threshold: 14.4, severity: "critical", firing: false },
    { rule_id: "slo:records-api:page_slow", tier: "page_slow", long_window: "6h", short_window: "30m", burn_rate_threshold: 6, severity: "critical", firing: true },
  ], ...o,
});

export interface AlertsState {
  active: Alert[];
  history: Alert[];
  rules: AlertRule[];
  silences: Silence[];
  notifiers: Notifier[];
  deliveries: NotifierDelivery[];
  slos: SloStatus[];
  /** Expressions evaluated through POST /alerts/query. */
  queried: string[];
  /** The answer of POST /alerts/query; an error envelope for an expression that is not valid. */
  samples: { labels: Record<string, string>; value: number | null }[];
  /** What POST /alerts/notifiers/{id}/test answers. */
  testReply: () => ReturnType<typeof envelope> | { status: number; body: unknown };
  seq: number;
}

/** Install the routes on `mock` and return the state a test can change. Call after `signedIn`. */
export function installAlerts(mock: MockApi, init: Partial<AlertsState> = {}): AlertsState {
  const st: AlertsState = {
    active: [alert()], history: [], rules: [rule(), rule({ id: "disk_free_low", name: "DiskSpaceLow", expr: "forgebase:data_dir_free_ratio", op: "<", threshold: 0.1 })],
    silences: [], notifiers: [notifier()], deliveries: [], slos: [sloStatus()], queried: [], samples: [{ labels: { route: "/api/v1/x" }, value: 0.2 }],
    testReply: () => ({ status: 200, body: { id: "dlv-t", notifier_id: "ntf-1", group_key: "test", event: "test", status: "sent", attempts: 1, next_attempt_at: null, last_status: 200, last_error: null, sent_at: T0, created_at: T0, payload: {} } }),
    seq: 100, ...init,
  };
  const A = "/api/v1/alerts";
  const S = "/api/v1/slos";
  const id = (p: string) => `${p}-${++st.seq}`;
  const notFound = (what: string) => envelope(404, `${what}_not_found`, `${what} not found`);
  const on = (method: string, path: string, h: (r: MockRequest) => { status: number; body?: unknown }) => mock.on(method, path, h);

  on("GET", A, ({ query }) => ({ status: 200, body: st.active.filter((a) => (!query.get("state") || a.state === query.get("state")) && (!query.get("severity") || a.severity === query.get("severity"))) }));
  on("GET", `${A}/history`, ({ query }) => {
    const limit = Number(query.get("limit") ?? 100);
    const before = query.get("before");
    const all = st.history.filter((a) => !query.get("state") || a.state === query.get("state"));
    const start = before ? all.findIndex((a) => a.id === before) + 1 : 0;
    const page = all.slice(start, start + limit);
    return { status: 200, body: { items: page, next_before: start + limit < all.length ? page.at(-1)?.id ?? null : null } };
  });
  on("POST", `${A}/query`, ({ body }) => {
    const expr = (body as { expr: string }).expr;
    st.queried.push(expr);
    if (expr.includes("(((")) return envelope(400, "invalid_expression", "unexpected token at position 3");
    return { status: 200, body: { at: T0, samples: st.samples } };
  });
  on("GET", `${A}/rules`, () => ({ status: 200, body: st.rules }));
  on("POST", `${A}/rules`, ({ body }) => {
    const b = body as Partial<AlertRule> & { name: string };
    if (st.rules.some((r) => r.name === b.name)) return envelope(409, "alert_rule_exists", "a rule with that name exists");
    const r = rule({ ...b, id: id("rule"), source: "api", for_ms: 0, labels: b.labels ?? {}, annotations: b.annotations ?? {} });
    st.rules.push(r);
    return { status: 201, body: r };
  });
  on("PATCH", `${A}/rules/{id}`, ({ params, body }) => {
    const r = st.rules.find((x) => x.id === params.id);
    if (!r) return notFound("alert_rule");
    const b = body as Partial<AlertRule>;
    if (r.source !== "api" && Object.keys(b).some((k) => k !== "enabled")) return envelope(409, "rule_read_only", "built-in rules take enabled only");
    if (b.enabled && r.disabled_by_config) return envelope(409, "rule_disabled_by_config", "disabled in the config file");
    Object.assign(r, b);
    return { status: 200, body: r };
  });
  on("DELETE", `${A}/rules/{id}`, ({ params }) => {
    const r = st.rules.find((x) => x.id === params.id);
    if (!r) return notFound("alert_rule");
    if (r.source !== "api") return envelope(409, "rule_read_only", "only custom rules can be deleted");
    st.rules = st.rules.filter((x) => x !== r);
    return { status: 204 };
  });
  on("GET", `${A}/silences`, ({ query }) => ({ status: 200, body: st.silences.filter((s) => query.get("all") === "true" || s.status !== "expired") }));
  on("POST", `${A}/silences`, ({ body }) => {
    const b = body as { matchers: Silence["matchers"]; duration?: string; comment?: string };
    const s: Silence = { id: id("sil"), matchers: b.matchers, starts_at: T0, ends_at: "2026-11-14T12:00:00.000Z", comment: b.comment ?? "", created_by: "admin:adm1", created_at: T0, status: "active" };
    st.silences.push(s);
    return { status: 201, body: s };
  });
  on("DELETE", `${A}/silences/{id}`, ({ params }) => {
    const s = st.silences.find((x) => x.id === params.id);
    if (!s) return notFound("silence");
    s.status = "expired";
    return { status: 200, body: s };
  });
  on("GET", `${A}/notifiers`, () => ({ status: 200, body: st.notifiers }));
  on("POST", `${A}/notifiers`, ({ body }) => {
    const b = body as Record<string, unknown> & { name: string; kind: Notifier["kind"]; url?: string };
    if (st.notifiers.some((n) => n.name === b.name)) return envelope(409, "notifier_exists", "a notifier with that name exists");
    // Secrets are sealed: a Slack URL is listed as its host, a routing key not at all; a webhook's signing secret is in this answer only.
    const url = b.kind === "slack" && b.url ? new URL(b.url).host : (b.url ?? null);
    const n = notifier({ ...(b as Partial<Notifier>), id: id("ntf"), url, to: (b.to as string[]) ?? [], topic: (b.topic as string) ?? null });
    delete (n as unknown as Record<string, unknown>).routing_key;
    st.notifiers.push(n);
    return { status: 201, body: b.kind === "webhook" ? { ...n, signing_secret: "whsec_abc123secret" } : n };
  });
  on("PUT", `${A}/notifiers/{id}`, ({ params, body }) => {
    const n = st.notifiers.find((x) => x.id === params.id);
    if (!n) return notFound("notifier");
    const b = body as Record<string, unknown>;
    const { routing_key: _k, ...rest } = b;
    void _k;
    Object.assign(n, rest, n.kind === "slack" && b.url ? { url: new URL(String(b.url)).host } : n.kind === "slack" ? { url: n.url } : {});
    return { status: 200, body: n };
  });
  on("DELETE", `${A}/notifiers/{id}`, ({ params }) => {
    if (!st.notifiers.some((x) => x.id === params.id)) return notFound("notifier");
    st.notifiers = st.notifiers.filter((x) => x.id !== params.id);
    return { status: 204 };
  });
  on("POST", `${A}/notifiers/{id}/test`, ({ params }) => {
    if (!st.notifiers.some((x) => x.id === params.id)) return notFound("notifier");
    return st.testReply();
  });
  on("GET", `${A}/notifications`, ({ query }) => ({ status: 200, body: { items: st.deliveries.filter((d) => (!query.get("status") || d.status === query.get("status")) && (!query.get("notifier") || d.notifier_id === query.get("notifier"))) } }));
  on("GET", `${A}/{id}`, ({ params }) => {
    const a = [...st.active, ...st.history].find((x) => x.id === params.id);
    return a ? { status: 200, body: { ...a, events: [{ at: T0, state: "pending", value: 1 }, { at: T0, state: "firing", value: 1 }] } } : notFound("alert");
  });

  on("GET", S, () => ({ status: 200, body: st.slos.map((s) => s.slo) }));
  on("GET", `${S}/status`, () => ({ status: 200, body: st.slos }));
  on("POST", S, ({ body }) => {
    const b = body as Partial<SloStatus["slo"]> & { name: string };
    if (st.slos.some((s) => s.slo.name === b.name)) return envelope(409, "slo_exists", "an SLO with that name exists");
    const s = slo({ ...b, id: id("slo") });
    st.slos.push(sloStatus({ slo: s, total: 0, good: 0, bad: 0, sli: null, error_budget: { allowed: 0, consumed: 0, remaining_ratio: 1 }, burn_rates: { "5m": null, "30m": null, "1h": null, "6h": null, "3d": null } }));
    return { status: 201, body: s };
  });
  on("PUT", `${S}/{id}`, ({ params, body }) => {
    const s = st.slos.find((x) => x.slo.id === params.id);
    if (!s) return notFound("slo");
    Object.assign(s.slo, body);
    return { status: 200, body: s.slo };
  });
  on("DELETE", `${S}/{id}`, ({ params }) => {
    if (!st.slos.some((x) => x.slo.id === params.id)) return notFound("slo");
    st.slos = st.slos.filter((x) => x.slo.id !== params.id);
    return { status: 204 };
  });
  return st;
}
