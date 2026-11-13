import type { Transport } from "./http";
import type { Schemas } from "./types";

/*
 * The administrative side of entitlements and quotas (FB-24): the catalog (plans, features, add-ons, usage packs,
 * quota policies), per-subject overrides and enterprise limits. Reached through `client.entitlements`; writes need
 * platform administrator rights.
 *
 * Inputs are camelCase and map to the server's snake_case JSON one level deep: nested objects (a plan's `features`)
 * keep the wire shape. Replies are the server's JSON as is.
 */

type CamelCase<S extends string> = S extends `${infer H}_${infer T}` ? `${H}${Capitalize<CamelCase<T>>}` : S;
/** Top-level keys of a wire type in camelCase. */
type Camelize<T> = { [K in keyof T as CamelCase<K & string>]: T[K] };

const seg = encodeURIComponent;

/** camelCase keys to snake_case, one level deep; `undefined` values are left out. */
export const snake = (o: object): Record<string, unknown> =>
  Object.fromEntries(
    Object.entries(o)
      .filter(([, v]) => v !== undefined)
      .map(([k, v]) => [k.replace(/[A-Z]/g, (c) => `_${c.toLowerCase()}`), v]),
  );

export type EntitlementFeature = Schemas["Feature"];
export type EntitlementFeatureInput = Camelize<Schemas["FeatureInput"]>;
export type EntitlementPlan = Schemas["Plan"];
export type EntitlementPlanInput = Camelize<Schemas["PlanInput"]>;
export type PlanFeature = Schemas["PlanFeature"];
export type EntitlementAddon = Schemas["Addon"];
export type EntitlementAddonInput = Camelize<Schemas["AddonInput"]>;
export type UsagePack = Schemas["UsagePack"];
export type UsagePackInput = Camelize<Schemas["UsagePackInput"]>;
export type PackGrant = Schemas["PackGrant"];
export type PackGrantInput = Camelize<Schemas["PackGrantInput"]>;
export type QuotaPolicy = Schemas["QuotaPolicy"];
export type QuotaPolicyInput = Camelize<Schemas["QuotaPolicy"]>;
export type EntitlementOverride = Schemas["Override"];
export type EntitlementOverrideInput = Camelize<Schemas["OverrideInput"]>;
export type CustomLimit = Schemas["CustomLimit"];
export type CustomLimitInput = Camelize<Schemas["CustomLimit"]>;
export type EntitlementSubscription = Schemas["Subscription"];
export type SubscriptionSyncInput = Camelize<Schemas["SubscriptionSync"]>;
export type SyncOutcome = Schemas["SyncOutcome"];
export type StartTrialInput = Camelize<Schemas["TrialInput"]>;
export type SubjectAddonInput = { subject: string; addon: string; quantity: number };
export type GaugeInput = Camelize<Schemas["GaugeRequest"]>;
export type UsageEvent = Schemas["UsageEvent"];
export type UsageEventPage = Schemas["UsageEventPage"];
export type UsageAlert = Schemas["UsageAlert"];
export type EntitlementsMaintenanceReport = Schemas["EntitlementsMaintenanceReport"];

export interface UsageEventsOptions {
  /** Subject (administrators may omit it: every subject; others get their own). */
  subject?: string;
  /** Only this feature. */
  feature?: string;
  /** Cursor: events with a larger `seq` (the previous page's `next`). */
  after?: number;
  /** Page size, default 100, at most 1000. */
  limit?: number;
}

/** Plans: `client.entitlements.plans`. Reads need `billing:read`; writes need a platform administrator. */
export class EntitlementPlansApi {
  constructor(private readonly t: Transport) {}

  async list(): Promise<EntitlementPlan[]> {
    return (await this.t.request<{ items: EntitlementPlan[] }>("/api/v1/plans")).items;
  }

  get(key: string): Promise<EntitlementPlan> {
    return this.t.request(`/api/v1/plans/${seg(key)}`);
  }

  /** Creates or replaces a plan and its features. */
  put(key: string, input: EntitlementPlanInput): Promise<EntitlementPlan> {
    return this.t.request(`/api/v1/plans/${seg(key)}`, { method: "PUT", body: snake(input) });
  }

  /** Deletes a plan no subscription uses. */
  delete(key: string): Promise<void> {
    return this.t.request(`/api/v1/plans/${seg(key)}`, { method: "DELETE" });
  }
}

/** Features of the catalog: `client.entitlements.features`. */
export class EntitlementFeaturesApi {
  constructor(private readonly t: Transport) {}

  async list(): Promise<EntitlementFeature[]> {
    return (await this.t.request<{ items: EntitlementFeature[] }>("/api/v1/features")).items;
  }

  get(key: string): Promise<EntitlementFeature> {
    return this.t.request(`/api/v1/features/${seg(key)}`);
  }

  /** Creates or replaces a feature (`kind`: `boolean`, `metered`, `gauge`, `concurrency` or `rate`). */
  put(key: string, input: EntitlementFeatureInput): Promise<EntitlementFeature> {
    return this.t.request(`/api/v1/features/${seg(key)}`, { method: "PUT", body: snake(input) });
  }

  delete(key: string): Promise<void> {
    return this.t.request(`/api/v1/features/${seg(key)}`, { method: "DELETE" });
  }
}

/** Add-ons of the catalog: `client.entitlements.addons`. */
export class EntitlementAddonsApi {
  constructor(private readonly t: Transport) {}

  async list(): Promise<EntitlementAddon[]> {
    return (await this.t.request<{ items: EntitlementAddon[] }>("/api/v1/addons")).items;
  }

  /** Creates or replaces an add-on. */
  put(key: string, input: EntitlementAddonInput): Promise<EntitlementAddon> {
    return this.t.request(`/api/v1/addons/${seg(key)}`, { method: "PUT", body: snake(input) });
  }

  delete(key: string): Promise<void> {
    return this.t.request(`/api/v1/addons/${seg(key)}`, { method: "DELETE" });
  }
}

/** Usage packs: the catalog and the packs a subject holds. `client.entitlements.usagePacks`. */
export class UsagePacksApi {
  constructor(private readonly t: Transport) {}

  /** Usage packs of the catalog. */
  async list(): Promise<UsagePack[]> {
    return (await this.t.request<{ items: UsagePack[] }>("/api/v1/usage-packs")).items;
  }

  /** Creates or replaces a usage pack of the catalog. */
  put(key: string, input: UsagePackInput): Promise<UsagePack> {
    return this.t.request(`/api/v1/usage-packs/${seg(key)}`, { method: "PUT", body: snake(input) });
  }

  delete(key: string): Promise<void> {
    return this.t.request(`/api/v1/usage-packs/${seg(key)}`, { method: "DELETE" });
  }

  /** Packs `subject` holds (default: the caller's; `billing:read`). */
  async grants(subject?: string): Promise<PackGrant[]> {
    return (await this.t.request<{ items: PackGrant[] }>("/api/v1/entitlements/usage-packs", { query: { subject } })).items;
  }

  /** Grants a pack to a subject (after a purchase). A replayed `idempotencyKey` returns the first grant. */
  grant(input: PackGrantInput): Promise<PackGrant> {
    return this.t.request("/api/v1/entitlements/usage-packs", { method: "POST", body: snake(input) });
  }
}

/** Quota policies per feature: `client.entitlements.quotaPolicies`. */
export class QuotaPoliciesApi {
  constructor(private readonly t: Transport) {}

  async list(): Promise<QuotaPolicy[]> {
    return (await this.t.request<{ items: QuotaPolicy[] }>("/api/v1/quota-policies")).items;
  }

  /** Creates or replaces the policy of a feature (enforcement, scope, cost, overage, rollover, lease length). */
  put(feature: string, policy: QuotaPolicyInput): Promise<QuotaPolicy> {
    return this.t.request(`/api/v1/quota-policies/${seg(feature)}`, { method: "PUT", body: snake(policy) });
  }

  delete(feature: string): Promise<void> {
    return this.t.request(`/api/v1/quota-policies/${seg(feature)}`, { method: "DELETE" });
  }
}

/** Per-subject allow / deny overrides of a feature: `client.entitlements.overrides`. Deny always wins. */
export class EntitlementOverridesApi {
  constructor(private readonly t: Transport) {}

  /** Overrides, of one subject when `subject` is given. */
  async list(subject?: string): Promise<EntitlementOverride[]> {
    return (await this.t.request<{ items: EntitlementOverride[] }>("/api/v1/entitlements/overrides", { query: { subject } })).items;
  }

  add(input: EntitlementOverrideInput): Promise<EntitlementOverride> {
    return this.t.request("/api/v1/entitlements/overrides", { method: "POST", body: snake(input) });
  }

  delete(id: string): Promise<void> {
    return this.t.request(`/api/v1/entitlements/overrides/${seg(id)}`, { method: "DELETE" });
  }
}

/** Enterprise limits of a subject (they replace the plan's grant): `client.entitlements.limits`. */
export class EntitlementLimitsApi {
  constructor(private readonly t: Transport) {}

  /** Limits, of one subject when `subject` is given. */
  async list(subject?: string): Promise<CustomLimit[]> {
    return (await this.t.request<{ items: CustomLimit[] }>("/api/v1/entitlements/limits", { query: { subject } })).items;
  }

  put(limit: CustomLimitInput): Promise<CustomLimit> {
    return this.t.request("/api/v1/entitlements/limits", { method: "PUT", body: snake(limit) });
  }

  delete(subject: string, feature: string): Promise<void> {
    return this.t.request("/api/v1/entitlements/limits", { method: "DELETE", query: { subject, feature } });
  }
}
