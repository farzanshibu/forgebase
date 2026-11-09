import type { Transport } from "./http";
import type { Schemas } from "./types";

export type Decision = Schemas["Decision"];
export type Entitlements = Schemas["Entitlements"];
export type UsageSummary = Schemas["UsageSummary"];
export type Reservation = Schemas["Reservation"];
export type Lease = Schemas["Lease"];
export type Consumption = Schemas["Consumption"];

export type HasQuery =
  | { feature: string; subject?: string }
  | { permission: string }
  | { plan: string; subject?: string };

export interface QuotaRequest {
  feature: string;
  /** Units (default 1). */
  units?: number;
  /** Counter for per-user / team / resource quotas. */
  scopeKey?: string;
  /** Billed subject (default: the caller's). */
  subject?: string;
  metadata?: Record<string, unknown>;
}

export interface MeteredRequest extends QuotaRequest {
  /** Required for consume / reserve / lease: a replay returns the first result. */
  idempotencyKey: string;
}

const wire = (r: QuotaRequest & { idempotencyKey?: string; ttlSecs?: number }) => ({
  feature: r.feature,
  units: r.units,
  scope_key: r.scopeKey,
  subject: r.subject,
  metadata: r.metadata,
  idempotency_key: r.idempotencyKey,
  ttl_secs: r.ttlSecs,
});

/** Entitlement and quota helpers (FB-24 routes): `client.entitlements`. */
export class EntitlementsApi {
  constructor(private readonly t: Transport) {}

  /** `has({feature})`, `has({permission})` or `has({plan})`: the boolean answer. */
  async has(q: HasQuery): Promise<boolean> {
    return (await this.hasDetailed(q)).has;
  }

  /** Same as `has` with the reason when the answer is no. */
  hasDetailed(q: HasQuery): Promise<{ has: boolean; reason?: string | null }> {
    const query = { feature: undefined, permission: undefined, plan: undefined, subject: undefined, ...q } as Record<string, string | undefined>;
    return this.t.request("/api/v1/entitlements/has", { query });
  }

  /** Effective plan and grants of the caller (or of `subject`, e.g. `org:<id>`). */
  get(subject?: string): Promise<Entitlements> {
    return this.t.request("/api/v1/entitlements", { query: { subject } });
  }

  /** Usage of every metered feature. */
  async usage(o: { subject?: string; scopeKey?: string } = {}): Promise<UsageSummary[]> {
    return (await this.t.request<{ items: UsageSummary[] }>("/api/v1/usage", { query: { subject: o.subject, scope_key: o.scopeKey } })).items;
  }

  /** Would these units be granted right now? No side effects. */
  checkQuota(r: QuotaRequest): Promise<Decision> {
    return this.t.request("/api/v1/quotas/check", { method: "POST", body: wire(r) });
  }

  /** Records usage at once. A replayed key returns the first result with `replayed: true`. Throws `QuotaExceededError` past a hard limit. */
  consume(r: MeteredRequest): Promise<Consumption> {
    return this.t.request("/api/v1/usage/consume", { method: "POST", body: wire(r) });
  }

  /** Holds units until `commit` or `release` (or `ttlSecs`). */
  reserve(r: MeteredRequest & { ttlSecs?: number }): Promise<Reservation> {
    return this.t.request("/api/v1/usage/reservations", { method: "POST", body: wire(r) });
  }

  commitReservation(id: string, units?: number): Promise<Reservation> {
    return this.t.request(`/api/v1/usage/reservations/${encodeURIComponent(id)}/commit`, { method: "POST", body: { units } });
  }

  releaseReservation(id: string): Promise<Reservation> {
    return this.t.request(`/api/v1/usage/reservations/${encodeURIComponent(id)}/release`, { method: "POST", body: {} });
  }

  /** Reserve, run `fn`, then commit; release the hold when `fn` throws. */
  async withReservation<R>(r: MeteredRequest & { ttlSecs?: number }, fn: (res: Reservation) => Promise<R>): Promise<R> {
    const res = await this.reserve(r);
    try {
      const out = await fn(res);
      await this.commitReservation(res.id);
      return out;
    } catch (err) {
      await this.releaseReservation(res.id).catch(() => undefined);
      throw err;
    }
  }

  /** Concurrency lease (e.g. "at most 5 running jobs"). */
  acquireLease(r: MeteredRequest & { ttlSecs?: number }): Promise<Lease> {
    return this.t.request("/api/v1/usage/leases", { method: "POST", body: wire(r) });
  }

  renewLease(id: string, ttlSecs?: number): Promise<Lease> {
    return this.t.request(`/api/v1/usage/leases/${encodeURIComponent(id)}/renew`, { method: "POST", body: { ttl_secs: ttlSecs } });
  }

  releaseLease(id: string): Promise<void> {
    return this.t.request(`/api/v1/usage/leases/${encodeURIComponent(id)}`, { method: "DELETE" });
  }
}
