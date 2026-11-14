import type { Transport } from "./http";
import type { Schemas } from "./types";

/*
 * Alerts and service level objectives (`/api/v1/alerts`, `/api/v1/slos`, ADR-0035): rules, alerts and their history,
 * silences, notifiers (with a test send), notification deliveries, expression queries, SLOs and their status.
 * `client.alerts` and `client.slos`.
 *
 * Platform administrators only. `[alerts] enabled = false` turns both prefixes into `404 feature_disabled`. Request and
 * response types are the server's wire models (snake_case); rules, silences and notifiers are addressed by id or name.
 */

export type Alert = Schemas["Alert"];
export type AlertDetail = Schemas["AlertDetail"];
export type AlertPage = Schemas["AlertPage"];
export type AlertState = Schemas["AlertState"];
export type AlertSeverity = Schemas["AlertSeverity"];
export type AlertRule = Schemas["AlertRule"];
export type AlertRuleInput = Schemas["AlertRuleInput"];
export type AlertRulePatch = Schemas["AlertRulePatch"];
export type AlertSilence = Schemas["AlertSilence"];
export type AlertSilenceInput = Schemas["AlertSilenceInput"];
export type AlertNotifier = Schemas["AlertNotifier"];
export type AlertNotifierInput = Schemas["AlertNotifierInput"];
export type AlertDelivery = Schemas["AlertDelivery"];
export type AlertDeliveryPage = Schemas["AlertDeliveryPage"];
export type AlertQueryResult = Schemas["AlertQueryResult"];
export type Slo = Schemas["Slo"];
export type SloInput = Schemas["SloInput"];
export type SloStatus = Schemas["SloStatus"];

/** Filters of `alerts.history`. */
export interface AlertHistoryQuery {
  /** `pending`, `firing` or `resolved`. */
  state?: AlertState | (string & {});
  /** Only the alerts of one rule. */
  ruleId?: string;
  from?: string | Date;
  to?: string | Date;
  /** Page cursor: the `next_before` of the previous page. */
  before?: string;
  limit?: number;
}

/** Filters of `alerts.notifications`. */
export interface AlertNotificationsQuery {
  /** Notifier id or name. */
  notifier?: string;
  /** Delivery status (`sent`, `failed`, ...). */
  status?: string;
  limit?: number;
}

const seg = encodeURIComponent;
const A = "/api/v1/alerts";
const S = "/api/v1/slos";
const iso = (v: string | Date | undefined): string | undefined => (v instanceof Date ? v.toISOString() : v);

/** Alert rules: `client.alerts.rules`. The built-in rules are read-only apart from `enabled`. */
export class AlertRulesApi {
  constructor(private readonly t: Transport) {}

  list(): Promise<AlertRule[]> {
    return this.t.request(`${A}/rules`);
  }

  /** Creates a custom rule: `expr` is evaluated against the metrics, `op` and `threshold` decide when it fires, `for` holds it back. */
  create(input: AlertRuleInput): Promise<AlertRule> {
    return this.t.request(`${A}/rules`, { method: "POST", body: input });
  }

  get(id: string): Promise<AlertRule> {
    return this.t.request(`${A}/rules/${seg(id)}`);
  }

  /** Changes a rule; what is left out stays. */
  update(id: string, patch: AlertRulePatch): Promise<AlertRule> {
    return this.t.request(`${A}/rules/${seg(id)}`, { method: "PATCH", body: patch });
  }

  /** Deletes a custom rule (a built-in one is a `PermissionError` / `ConflictError`: disable it instead). */
  async delete(id: string): Promise<void> {
    await this.t.request(`${A}/rules/${seg(id)}`, { method: "DELETE" });
  }
}

/** Silences (mute alerts whose labels match): `client.alerts.silences`. */
export class AlertSilencesApi {
  constructor(private readonly t: Transport) {}

  /** Active and pending silences; `all: true` includes expired ones. */
  list(o: { all?: boolean } = {}): Promise<AlertSilence[]> {
    return this.t.request(`${A}/silences`, { query: { all: o.all ? true : undefined } });
  }

  create(input: AlertSilenceInput): Promise<AlertSilence> {
    return this.t.request(`${A}/silences`, { method: "POST", body: input });
  }

  get(id: string): Promise<AlertSilence> {
    return this.t.request(`${A}/silences/${seg(id)}`);
  }

  /** Replaces a silence. */
  update(id: string, input: AlertSilenceInput): Promise<AlertSilence> {
    return this.t.request(`${A}/silences/${seg(id)}`, { method: "PUT", body: input });
  }

  /** Ends a silence now and resolves with it. */
  expire(id: string): Promise<AlertSilence> {
    return this.t.request(`${A}/silences/${seg(id)}`, { method: "DELETE" });
  }
}

/** Where alerts are sent (webhook, email, ...): `client.alerts.notifiers`. */
export class AlertNotifiersApi {
  constructor(private readonly t: Transport) {}

  list(): Promise<AlertNotifier[]> {
    return this.t.request(`${A}/notifiers`);
  }

  create(input: AlertNotifierInput): Promise<AlertNotifier> {
    return this.t.request(`${A}/notifiers`, { method: "POST", body: input });
  }

  get(id: string): Promise<AlertNotifier> {
    return this.t.request(`${A}/notifiers/${seg(id)}`);
  }

  /** Replaces a notifier. */
  update(id: string, input: AlertNotifierInput): Promise<AlertNotifier> {
    return this.t.request(`${A}/notifiers/${seg(id)}`, { method: "PUT", body: input });
  }

  async delete(id: string): Promise<void> {
    await this.t.request(`${A}/notifiers/${seg(id)}`, { method: "DELETE" });
  }

  /** Sends a test notification through the notifier and resolves with the delivery (whether it worked is in its status). */
  test(id: string): Promise<AlertDelivery> {
    return this.t.request(`${A}/notifiers/${seg(id)}/test`, { method: "POST", body: {}, retry: false });
  }
}

/** Alerts, rules, silences and notifiers: `client.alerts`. */
export class AlertsApi {
  readonly rules: AlertRulesApi;
  readonly silences: AlertSilencesApi;
  readonly notifiers: AlertNotifiersApi;

  constructor(private readonly t: Transport) {
    this.rules = new AlertRulesApi(t);
    this.silences = new AlertSilencesApi(t);
    this.notifiers = new AlertNotifiersApi(t);
  }

  /** Alerts that are firing or pending now. */
  active(o: { state?: AlertState | (string & {}); severity?: AlertSeverity | (string & {}) } = {}): Promise<Alert[]> {
    return this.t.request(A, { query: { state: o.state, severity: o.severity } });
  }

  /** Past alerts, newest first; `next_before` continues the listing. */
  history(q: AlertHistoryQuery = {}): Promise<AlertPage> {
    return this.t.request(`${A}/history`, { query: { state: q.state, rule_id: q.ruleId, from: iso(q.from), to: iso(q.to), before: q.before, limit: q.limit } });
  }

  /** One alert with its events. */
  get(id: string): Promise<AlertDetail> {
    return this.t.request(`${A}/${seg(id)}`);
  }

  /** Evaluates an expression against the metrics now (to try a rule before saving it). */
  query(expr: string): Promise<AlertQueryResult> {
    return this.t.request(`${A}/query`, { method: "POST", body: { expr } });
  }

  /** What was sent to the notifiers, newest first. */
  notifications(q: AlertNotificationsQuery = {}): Promise<AlertDeliveryPage> {
    return this.t.request(`${A}/notifications`, { query: { notifier: q.notifier, status: q.status, limit: q.limit } });
  }
}

/** Service level objectives: `client.slos`. */
export class SlosApi {
  constructor(private readonly t: Transport) {}

  list(): Promise<Slo[]> {
    return this.t.request(S);
  }

  create(input: SloInput): Promise<Slo> {
    return this.t.request(S, { method: "POST", body: input });
  }

  get(id: string): Promise<Slo> {
    return this.t.request(`${S}/${seg(id)}`);
  }

  /** Replaces an SLO. */
  update(id: string, input: SloInput): Promise<Slo> {
    return this.t.request(`${S}/${seg(id)}`, { method: "PUT", body: input });
  }

  async delete(id: string): Promise<void> {
    await this.t.request(`${S}/${seg(id)}`, { method: "DELETE" });
  }

  /** Compliance, error budget and burn rates of one SLO. */
  status(id: string): Promise<SloStatus> {
    return this.t.request(`${S}/${seg(id)}/status`);
  }

  /** The status of every SLO. */
  statuses(): Promise<SloStatus[]> {
    return this.t.request(`${S}/status`);
  }
}
