/**
 * Typed client for `/api/v1/alerts` and `/api/v1/slos` (ADR-0035, docs/api.md §11.8). Platform administrators only.
 * Lists of rules, silences, notifiers and SLOs are bare arrays; history and deliveries are `{items}` pages.
 */
import { request, send } from "./client";

const A = "/api/v1/alerts";
const S = "/api/v1/slos";
const enc = encodeURIComponent;

export type AlertSeverity = "info" | "warning" | "critical";
export type AlertState = "pending" | "firing" | "resolved";
export type RuleSource = "builtin" | "config" | "api" | "slo";
export type Comparison = ">" | ">=" | "<" | "<=" | "==" | "!=";
export const COMPARISONS: Comparison[] = [">", ">=", "<", "<=", "==", "!="];
export const SEVERITIES: AlertSeverity[] = ["info", "warning", "critical"];

export interface Alert {
  id: string;
  fingerprint: string;
  rule_id: string;
  name: string;
  severity: AlertSeverity;
  state: AlertState;
  labels: Record<string, string>;
  annotations: Record<string, string>;
  value: number | null;
  active_at: string;
  fired_at: string | null;
  resolved_at: string | null;
  last_evaluated_at: string;
  /** Ids of the active silences that match it. */
  silenced_by: string[];
}
export interface AlertEvent { at: string; state: AlertState; value: number | null }
export interface AlertDetail extends Alert { events: AlertEvent[] }
export interface AlertPage { items: Alert[]; next_before: string | null }

export interface AlertRule {
  id: string;
  name: string;
  source: RuleSource;
  expr: string;
  op: Comparison;
  threshold: number;
  for: string;
  for_ms: number;
  severity: AlertSeverity;
  labels: Record<string, string>;
  annotations: Record<string, string>;
  enabled: boolean;
  /** Switched off by `[alerts] disabled_rules`: the API cannot switch it on. */
  disabled_by_config: boolean;
  created_at?: string;
  updated_at?: string;
}
export interface AlertRuleInput {
  name: string;
  expr: string;
  op?: Comparison;
  threshold: number;
  for?: string;
  severity?: AlertSeverity;
  labels?: Record<string, string>;
  annotations?: Record<string, string>;
  enabled?: boolean;
}
/** Built-in, file and SLO rules take `enabled` only; a custom rule takes any field. */
export type AlertRulePatch = Partial<Omit<AlertRuleInput, "name">>;

export interface QuerySample { labels: Record<string, string>; value: number | null }
export interface QueryResult { at: string; samples: QuerySample[] }

export type MatcherOp = "=" | "!=" | "=~" | "!~";
export const MATCHER_OPS: MatcherOp[] = ["=", "!=", "=~", "!~"];
export interface LabelMatcher { label: string; op: MatcherOp; value: string }
export type SilenceStatus = "active" | "pending" | "expired";
export interface Silence {
  id: string;
  matchers: LabelMatcher[];
  starts_at: string;
  ends_at: string;
  comment: string;
  created_by: string;
  created_at: string;
  status: SilenceStatus;
}
export interface SilenceInput { matchers: LabelMatcher[]; starts_at?: string; ends_at?: string; duration?: string; comment?: string }

export type NotifierKind = "webhook" | "slack" | "email" | "pagerduty" | "queue";
export const NOTIFIER_KINDS: NotifierKind[] = ["webhook", "slack", "email", "pagerduty", "queue"];
export interface Notifier {
  id: string;
  name: string;
  kind: NotifierKind;
  /** Webhook URL, PagerDuty events URL; Slack: its host only. */
  url: string | null;
  to: string[];
  topic: string | null;
  matchers: LabelMatcher[];
  min_severity: AlertSeverity;
  group_by: string[];
  repeat_interval: string;
  send_resolved: boolean;
  enabled: boolean;
  last_success_at: string | null;
  last_failure_at: string | null;
  last_error: string | null;
  created_at: string;
  updated_at: string;
  /** Webhook signing secret (`whsec_…`): only in the answer that created it. */
  signing_secret?: string;
}
/** Secrets (`url` of Slack, `routing_key`) are write-only: leave them out to keep the stored value. */
export interface NotifierInput {
  name: string;
  kind: NotifierKind;
  url?: string;
  routing_key?: string;
  to?: string[];
  topic?: string;
  matchers?: LabelMatcher[];
  min_severity?: AlertSeverity;
  group_by?: string[];
  repeat_interval?: string;
  send_resolved?: boolean;
  enabled?: boolean;
}
export type NotifierDeliveryStatus = "pending" | "sent" | "failed";
export interface NotifierDelivery {
  id: string;
  notifier_id: string;
  group_key: string;
  /** `firing`, `resolved` or `test`. */
  event: string;
  status: NotifierDeliveryStatus;
  attempts: number;
  next_attempt_at: string | null;
  last_status: number | null;
  last_error: string | null;
  sent_at: string | null;
  created_at: string;
  payload: unknown;
}

export type SloKind = "availability" | "latency";
export interface Slo {
  id: string;
  name: string;
  description: string;
  kind: SloKind;
  objective: number;
  window: string;
  window_ms: number;
  route_prefix: string | null;
  route_regex: string | null;
  methods: string[];
  latency_threshold_ms: number | null;
  alerting: boolean;
  labels: Record<string, string>;
  created_at: string;
  updated_at: string;
}
export interface SloInput {
  name: string;
  description?: string;
  kind: SloKind;
  objective: number;
  window?: string;
  route_prefix?: string;
  route_regex?: string;
  methods?: string[];
  latency_threshold_ms?: number;
  alerting?: boolean;
  labels?: Record<string, string>;
}
export interface BurnTier {
  rule_id: string;
  /** `page_fast`, `page_slow` or `ticket`. */
  tier: string;
  long_window: string;
  short_window: string;
  burn_rate_threshold: number;
  severity: AlertSeverity;
  firing: boolean;
}
export interface SloStatus {
  slo: Slo;
  total: number;
  good: number;
  bad: number;
  /** `good / total`; null without traffic. */
  sli: number | null;
  error_budget: { allowed: number; consumed: number; remaining_ratio: number };
  /** Burn rate per window (`5m`, `30m`, `1h`, `6h`, `3d`); null without traffic. */
  burn_rates: Record<string, number | null>;
  alerts: BurnTier[];
}

export const ALERT_OPERATIONS = [
  "get /api/v1/alerts", "get /api/v1/alerts/history", "get /api/v1/alerts/{id}", "post /api/v1/alerts/query",
  "get /api/v1/alerts/rules", "post /api/v1/alerts/rules", "get /api/v1/alerts/rules/{id}", "patch /api/v1/alerts/rules/{id}", "delete /api/v1/alerts/rules/{id}",
  "get /api/v1/alerts/silences", "post /api/v1/alerts/silences", "get /api/v1/alerts/silences/{id}", "put /api/v1/alerts/silences/{id}", "delete /api/v1/alerts/silences/{id}",
  "get /api/v1/alerts/notifiers", "post /api/v1/alerts/notifiers", "get /api/v1/alerts/notifiers/{id}", "put /api/v1/alerts/notifiers/{id}", "delete /api/v1/alerts/notifiers/{id}",
  "post /api/v1/alerts/notifiers/{id}/test", "get /api/v1/alerts/notifications",
  "get /api/v1/slos", "post /api/v1/slos", "get /api/v1/slos/status", "get /api/v1/slos/{id}", "put /api/v1/slos/{id}", "delete /api/v1/slos/{id}", "get /api/v1/slos/{id}/status",
] as const;

function qs(params: object): string {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v !== undefined && v !== null && v !== "") q.set(k, String(v));
  const s = q.toString();
  return s ? `?${s}` : "";
}

// Alerts
export const listAlerts = (f: { state?: "pending" | "firing"; severity?: AlertSeverity } = {}, signal?: AbortSignal) => request<Alert[]>(`${A}${qs(f)}`, { signal });
export const alertHistory = (f: { state?: AlertState; rule_id?: string; before?: string; limit?: number } = {}, signal?: AbortSignal) => request<AlertPage>(`${A}/history${qs(f)}`, { signal });
export const getAlert = (id: string, signal?: AbortSignal) => request<AlertDetail>(`${A}/${enc(id)}`, { signal });
/** Evaluates an expression now (nothing changes). A bad expression answers 400 `invalid_expression`. */
export const queryExpression = (expr: string) => send<QueryResult>("POST", `${A}/query`, { expr });

// Rules
export const listRules = (signal?: AbortSignal) => request<AlertRule[]>(`${A}/rules`, { signal });
export const createRule = (r: AlertRuleInput) => send<AlertRule>("POST", `${A}/rules`, r);
export const patchRule = (id: string, p: AlertRulePatch) => send<AlertRule>("PATCH", `${A}/rules/${enc(id)}`, p);
export const deleteRule = (id: string) => send<void>("DELETE", `${A}/rules/${enc(id)}`);

// Silences
export const listSilences = (all = false, signal?: AbortSignal) => request<Silence[]>(`${A}/silences${qs({ all: all || undefined })}`, { signal });
export const createSilence = (s: SilenceInput) => send<Silence>("POST", `${A}/silences`, s);
export const expireSilence = (id: string) => send<Silence>("DELETE", `${A}/silences/${enc(id)}`);

// Notifiers
export const listNotifiers = (signal?: AbortSignal) => request<Notifier[]>(`${A}/notifiers`, { signal });
export const createNotifier = (n: NotifierInput) => send<Notifier>("POST", `${A}/notifiers`, n);
export const updateNotifier = (id: string, n: NotifierInput) => send<Notifier>("PUT", `${A}/notifiers/${enc(id)}`, n);
export const deleteNotifier = (id: string) => send<void>("DELETE", `${A}/notifiers/${enc(id)}`);
/** Sends a test notification now (10 per notifier and minute, then 429) and returns its delivery. */
export const testNotifier = (id: string) => send<NotifierDelivery>("POST", `${A}/notifiers/${enc(id)}/test`);
export const listNotifierDeliveries = async (f: { notifier?: string; status?: NotifierDeliveryStatus; limit?: number } = {}, signal?: AbortSignal) =>
  (await request<{ items: NotifierDelivery[] }>(`${A}/notifications${qs(f)}`, { signal })).items;

// SLOs
export const listSlos = (signal?: AbortSignal) => request<Slo[]>(S, { signal });
export const createSlo = (s: SloInput) => send<Slo>("POST", S, s);
export const updateSlo = (id: string, s: SloInput) => send<Slo>("PUT", `${S}/${enc(id)}`, s);
export const deleteSlo = (id: string) => send<void>("DELETE", `${S}/${enc(id)}`);
export const listSloStatus = (signal?: AbortSignal) => request<SloStatus[]>(`${S}/status`, { signal });
