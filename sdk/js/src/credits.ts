import type { Transport } from "./http";
import type { Schemas } from "./types";

/*
 * Prepaid credits (`/api/v1/credits`): `client.credits`.
 * A balance per user or organization (`subject`: `org:<id>` or a principal key; default the caller's) backed by an
 * append-only ledger. A debit the balance does not cover is `402 insufficient_credits` (an `ApiError` with
 * `details.balance` and `details.requested`). Amounts are whole credits.
 */

export type CreditBalance = Schemas["Balance"];
export type CreditLot = Schemas["Lot"];
export type CreditEntryKind = Schemas["EntryKind"];
export type CreditLedgerEntry = Schemas["LedgerEntry"];
export type CreditLedgerPage = Schemas["LedgerPage"];
/** A posted entry. `replayed` is true when the idempotency key was seen before: nothing new was written. */
export type CreditPosted = Schemas["Posted"];
export type CreditReversal = Schemas["Reversal"];
export type CreditRate = Schemas["Rate"];
export type CreditRateBasis = Schemas["RateBasis"];
export type CreditUsageCharge = Schemas["UsageCharge"];
export type CreditUsagePage = Schemas["UsageChargePage"];
export type CreditAudit = Schemas["AuditReport"];
export type CreditInterval = Schemas["Interval"];
export type CreditBalanceHistory = Schemas["BalanceHistory"];
export type CreditConsumption = Schemas["ConsumptionStats"];
export type CreditSummary = Schemas["CreditSummary"];
export type CreditsMaintenanceReport = Schemas["CreditsMaintenanceReport"];

/** A time as `Date` or RFC 3339 string. */
export type CreditTime = Date | string;

export interface CreditSubjectOptions {
  /** `org:<id>` or a principal key (default: the caller's subject). */
  subject?: string;
}

export interface CreditLotsOptions extends CreditSubjectOptions {
  /** Include empty and lapsed lots. */
  all?: boolean;
}

export interface CreditLedgerOptions extends CreditSubjectOptions {
  /** Platform administrators: every subject. */
  all?: boolean;
  kind?: CreditEntryKind;
  /** Entries after this `seq` (the `next` of the previous page). */
  after?: number;
  /** At or after. */
  from?: CreditTime;
  /** Before. */
  to?: CreditTime;
  /** Page size (default 100, at most 1000). */
  limit?: number;
}

export interface CreditUsageOptions extends CreditSubjectOptions {
  /** Platform administrators: every subject. */
  all?: boolean;
  /** Only charges whose balance did not cover the cost. */
  shortfall?: boolean;
  /** Charges after this usage event seq (the `next` of the previous page). */
  after?: number;
  /** Page size (default 100, at most 1000). */
  limit?: number;
}

/** Statistics window; default: the last 30 days, bucketed by day. */
export interface CreditWindow extends CreditSubjectOptions {
  from?: CreditTime;
  to?: CreditTime;
  interval?: CreditInterval;
}

export interface CreditConsumptionOptions extends CreditWindow {
  /** Platform administrators: the whole instance. */
  all?: boolean;
}

/** Adds credits (`topUp`: purchased; `grant`: free, optionally lapsing). Operators only. */
export interface CreditInput {
  /** Subject to credit. */
  subject: string;
  /** Credits, positive. */
  amount: number;
  /** A replay returns the first result with `replayed: true`. */
  idempotencyKey: string;
  /** Grants: when the credits lapse. */
  expiresAt?: CreditTime;
  /** Id in the payment system or whatever caused the credit. */
  externalRef?: string;
  reason?: string;
  metadata?: Record<string, unknown>;
}

/** Debits credits at once, all or nothing. */
export interface CreditSpendInput {
  amount: number;
  idempotencyKey: string;
  /** Subject to debit (default: the caller's). */
  subject?: string;
  /** Feature the spend is for. */
  feature?: string;
  /** Units of `feature` consumed. */
  units?: number;
  externalRef?: string;
  reason?: string;
  metadata?: Record<string, unknown>;
}

/** Refund or reversal of an earlier entry. */
export interface CreditAmendInput {
  /** The consumption / spend (refund) or top-up / grant (reversal) to amend. */
  entryId: string;
  idempotencyKey: string;
  /** Credits; default: all that is left to give back or take back. */
  amount?: number;
  externalRef?: string;
  reason?: string;
  metadata?: Record<string, unknown>;
}

export interface CreditRateInput {
  /** Credits charged per `perUnits` units. */
  creditsPerUnit: number;
  /** Units the price covers (default 1). */
  perUnits?: number;
  /** `units` (default): every unit used is charged; `overage`: only units past the feature's soft limit. */
  basis?: CreditRateBasis;
}

const seg = encodeURIComponent;

const time = (v: CreditTime | undefined): string | undefined => (v instanceof Date ? v.toISOString() : v);

/** Drops unset fields (query strings and bodies carry only what was given). */
const wire = <T extends object>(o: T): T => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as T;

const flag = (v: boolean | undefined): true | undefined => (v ? true : undefined);

const windowQuery = (o: CreditWindow & { all?: boolean }) => ({
  subject: o.subject,
  all: flag(o.all),
  from: time(o.from),
  to: time(o.to),
  interval: o.interval,
});

const creditBody = (i: CreditInput) =>
  wire({
    subject: i.subject,
    amount: i.amount,
    idempotency_key: i.idempotencyKey,
    expires_at: time(i.expiresAt),
    external_ref: i.externalRef,
    reason: i.reason,
    metadata: i.metadata,
  });

/** Prices of metered features in credits: `client.credits.rates`. */
export class CreditRatesApi {
  constructor(private readonly t: Transport) {}

  /** Every price. */
  async list(): Promise<CreditRate[]> {
    return (await this.t.request<{ items: CreditRate[] }>("/api/v1/credits/rates")).items;
  }

  /** Sets the price of a feature (platform administrators). */
  set(feature: string, rate: CreditRateInput): Promise<CreditRate> {
    return this.t.request(`/api/v1/credits/rates/${seg(feature)}`, {
      method: "PUT",
      body: wire({ credits_per_unit: rate.creditsPerUnit, per_units: rate.perUnits, basis: rate.basis }),
    });
  }

  /** Removes the price of a feature (platform administrators). */
  delete(feature: string): Promise<void> {
    return this.t.request(`/api/v1/credits/rates/${seg(feature)}`, { method: "DELETE" });
  }
}

/** Position of the usage-event consumer, which charges usage events in credits: `client.credits.usageCursor`. */
export class CreditUsageCursorApi {
  constructor(private readonly t: Transport) {}

  /** The usage event seq charged so far (platform administrators). */
  async get(): Promise<number> {
    return (await this.t.request<Schemas["Cursor"]>("/api/v1/credits/usage/cursor")).seq;
  }

  /** Moves the consumer (platform administrators). Events already charged are never charged again. */
  async set(seq: number): Promise<number> {
    return (await this.t.request<Schemas["Cursor"]>("/api/v1/credits/usage/cursor", { method: "PUT", body: { seq } })).seq;
  }
}

/** Statistics: `client.credits.stats`. Windows default to the last 30 days, bucketed by day. */
export class CreditStatsApi {
  constructor(private readonly t: Transport) {}

  /** Opening balance, then per bucket the credits added, removed and the closing balance. */
  balance(o: CreditWindow = {}): Promise<CreditBalanceHistory> {
    return this.t.request("/api/v1/credits/stats/balance", { query: windowQuery(o) });
  }

  /** Credits consumed by feature and period, net of refunds. */
  consumption(o: CreditConsumptionOptions = {}): Promise<CreditConsumption> {
    return this.t.request("/api/v1/credits/stats/consumption", { query: windowQuery(o) });
  }

  /** Totals by entry kind, the balance and the usage shortfall. */
  summary(o: Omit<CreditConsumptionOptions, "interval"> = {}): Promise<CreditSummary> {
    return this.t.request("/api/v1/credits/stats/summary", { query: { ...windowQuery(o), interval: undefined } });
  }
}

/** Credits helpers: `client.credits`. */
export class CreditsApi {
  readonly rates: CreditRatesApi;
  readonly usageCursor: CreditUsageCursorApi;
  readonly stats: CreditStatsApi;

  constructor(private readonly t: Transport) {
    this.rates = new CreditRatesApi(t);
    this.usageCursor = new CreditUsageCursorApi(t);
    this.stats = new CreditStatsApi(t);
  }

  /** Balance and next expiries of a subject (the caller by default). */
  balance(o: CreditSubjectOptions = {}): Promise<CreditBalance> {
    return this.t.request("/api/v1/credits/balance", { query: { subject: o.subject } });
  }

  /** What is left of each top-up and grant, oldest first. */
  async lots(o: CreditLotsOptions = {}): Promise<CreditLot[]> {
    return (await this.t.request<{ items: CreditLot[] }>("/api/v1/credits/lots", { query: { subject: o.subject, all: flag(o.all) } })).items;
  }

  /** One page of ledger entries in `seq` order; pass `next` back as `after` for the following page. */
  ledger(o: CreditLedgerOptions = {}): Promise<CreditLedgerPage> {
    return this.t.request("/api/v1/credits/ledger", {
      query: { subject: o.subject, all: flag(o.all), kind: o.kind, after: o.after, from: time(o.from), to: time(o.to), limit: o.limit },
    });
  }

  /** Every ledger entry matching `o`, page after page. */
  async *entries(o: Omit<CreditLedgerOptions, "after"> & { after?: number } = {}): AsyncGenerator<CreditLedgerEntry, void, undefined> {
    let after = o.after;
    for (;;) {
      const page = await this.ledger({ ...o, after });
      yield* page.items;
      if (page.next === null || page.next === undefined || page.items.length === 0) return;
      after = page.next;
    }
  }

  /** One ledger entry. */
  entry(id: string): Promise<CreditLedgerEntry> {
    return this.t.request(`/api/v1/credits/ledger/${seg(id)}`);
  }

  /** Checks that the balance, ledger, lots and their allocations agree. */
  audit(o: CreditSubjectOptions = {}): Promise<CreditAudit> {
    return this.t.request("/api/v1/credits/audit", { query: { subject: o.subject } });
  }

  /** Adds purchased credits (operators). Idempotent by key. */
  topUp(i: CreditInput): Promise<CreditPosted> {
    return this.t.request("/api/v1/credits/top-ups", { method: "POST", body: creditBody(i) });
  }

  /** Adds free credits, optionally lapsing at `expiresAt` (operators). Idempotent by key. */
  grant(i: CreditInput): Promise<CreditPosted> {
    return this.t.request("/api/v1/credits/grants", { method: "POST", body: creditBody(i) });
  }

  /** Debits credits, all or nothing. A balance that does not cover it is an `ApiError` with status 402 (`insufficient_credits`). */
  spend(i: CreditSpendInput): Promise<CreditPosted> {
    return this.t.request("/api/v1/credits/spend", {
      method: "POST",
      body: wire({
        amount: i.amount,
        idempotency_key: i.idempotencyKey,
        subject: i.subject,
        feature: i.feature,
        units: i.units,
        external_ref: i.externalRef,
        reason: i.reason,
        metadata: i.metadata,
      }),
    });
  }

  /** Gives back all or part of a consumption or spend. */
  refund(i: CreditAmendInput): Promise<CreditPosted> {
    return this.t.request("/api/v1/credits/refunds", { method: "POST", body: amendBody(i) });
  }

  /** Takes back what is left of a top-up or grant (operators). Nothing left is not an error: `reversed` is 0. */
  reverse(i: CreditAmendInput): Promise<CreditReversal> {
    return this.t.request("/api/v1/credits/reversals", { method: "POST", body: amendBody(i) });
  }

  /** One page of usage events charged in credits, with the shortfall the balance did not cover. */
  usage(o: CreditUsageOptions = {}): Promise<CreditUsagePage> {
    return this.t.request("/api/v1/credits/usage", {
      query: { subject: o.subject, all: flag(o.all), shortfall: flag(o.shortfall), after: o.after, limit: o.limit },
    });
  }

  /** Runs expiry and usage consumption now (platform administrators); the server also does it every minute. */
  maintenance(): Promise<CreditsMaintenanceReport> {
    return this.t.request("/api/v1/credits/maintenance", { method: "POST" });
  }
}

const amendBody = (i: CreditAmendInput) =>
  wire({
    entry_id: i.entryId,
    idempotency_key: i.idempotencyKey,
    amount: i.amount,
    external_ref: i.externalRef,
    reason: i.reason,
    metadata: i.metadata,
  });
