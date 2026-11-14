import type { Transport } from "./http";
import type { Schemas } from "./types";

/*
 * Instance administration (`/api/system`, `/api/v1/system`): `client.system`.
 * Every call needs an administrator. Destructive database operations ask for a typed confirmation and, for an
 * administrator with MFA, a second factor from the last ten minutes (`PermissionError`, code `mfa_step_up_required`).
 */

export type SystemFeatures = Schemas["SystemFeatures"];
export type SystemFeature = Schemas["SystemFeature"];
export type SystemSetting = Schemas["SystemSetting"];
export type ConfigPatched = Schemas["SystemConfigPatched"];
export type Topology = Schemas["TopologyView"];
export type TopologySet = Schemas["SetInfo"];
export type TopologyNode = Schemas["NodeInfo"];
export type NodeDetail = Schemas["NodeDetail"];
export type NodeAdded = Schemas["Added"];
export type PromoteCheck = Schemas["PromoteCheck"];
export type JoinToken = Schemas["JoinToken"];
export type Operation = Schemas["Operation"];
export type Placement = Schemas["PlacementInfo"];
export type PlacementPage = Schemas["PlacementPage"];
export type RebalancePlan = Schemas["RebalancePlanView"];
export type RetiredData = Schemas["RetiredInfo"];
export type Snapshot = Schemas["SnapshotView"];
export type SnapshotManifest = Schemas["Manifest"];
export type VerifyReport = Schemas["VerifyReport"];
export type PruneResult = Schemas["PruneResult"];
export type DbLogin = Schemas["DbLogin"];
export type DbLoginSecret = Schemas["LoginSecret"];
export type ExtensionInfo = Schemas["ExtensionInfo"];
export type Capabilities = Schemas["Capabilities"];
export type Capability = Schemas["Capability"];

export interface AddNodeInput {
  /** `local` clones a replica on this host (an operation); `external` registers one that already runs (`url`). */
  kind: "local" | "external";
  name: string;
  /** Replica set (default `control`). */
  set?: string;
  /** Connection URL of an external replica. */
  url?: string;
  /** Share of reads, 0 to 1000 (0 keeps it out of rotation). */
  weight?: number;
}

export interface AddShardInput {
  name: string;
  /** Its primary. */
  node: { kind: string; name: string; url?: string };
  /** Relative capacity (default 1). */
  weight?: number;
}

export interface UpdateShardInput {
  /** `active` or `draining` (takes no new tenants). */
  state?: "active" | "draining";
  /** Relative capacity, 1 to 1000. */
  weight?: number;
  /** Takes new tenants. */
  hostsTenants?: boolean;
}

export interface PlacementsOptions {
  set?: string;
  project?: string;
  /** Tenant id contains. */
  search?: string;
  cursor?: string;
  /** 1 to 200 (default 50). */
  limit?: number;
}

export interface CreateSnapshotOptions {
  label?: string;
  /** Include derived index files (default true). */
  includeIndexes?: boolean;
  /** Retention applied after the snapshot is taken. */
  retention?: Retention;
}

export interface Retention {
  keepLast?: number;
  maxAgeDays?: number;
}

export interface DbLoginInput {
  name: string;
  admin?: boolean;
  readonly?: boolean;
  connectionLimit?: number;
  principal?: string;
  statementTimeoutMs?: number;
  tenant?: string;
  /** RFC 3339 expiry. */
  validUntil?: string;
}

const seg = encodeURIComponent;
const FINAL = new Set(["succeeded", "failed", "cancelled", "dead"]);
const bearer = (agentKey: string) => ({ Authorization: `Bearer ${agentKey}` });
const retention = (r: Retention | undefined) => (r ? { keep_last: r.keepLast, max_age_days: r.maxAgeDays } : undefined);

/** Calls of a node agent (`forgebase node join`), authenticated with its agent key instead of a session. */
export class DatabaseAgentApi {
  constructor(private readonly t: Transport) {}

  /** Redeems a join token and returns the agent's grant (agent key, replication login). */
  join(body: Record<string, unknown>): Promise<Record<string, unknown>> {
    return this.t.request("/api/system/database/agent/join", { method: "POST", body, anonymous: true });
  }

  /** The agent's long-poll for commands. */
  commands(nodeId: string, agentKey: string, o: { waitMs?: number; resend?: boolean; signal?: AbortSignal } = {}): Promise<Record<string, unknown>> {
    return this.t.request(`/api/system/database/agent/nodes/${seg(nodeId)}/commands`, {
      anonymous: true,
      headers: bearer(agentKey),
      query: { wait_ms: o.waitMs, resend: o.resend === undefined ? undefined : o.resend ? 1 : 0 },
      signal: o.signal,
    });
  }

  /** The agent's heartbeat: state, replay position, free disk and command answers. */
  heartbeat(nodeId: string, agentKey: string, body: Record<string, unknown>): Promise<Record<string, unknown>> {
    return this.t.request(`/api/system/database/agent/nodes/${seg(nodeId)}/heartbeat`, { method: "POST", body, anonymous: true, headers: bearer(agentKey) });
  }
}

/** Replicas, shards, tenant placement and rebalancing: `client.system.database`. */
export class DatabaseTopologyApi {
  readonly agent: DatabaseAgentApi;

  constructor(private readonly t: Transport) {
    this.agent = new DatabaseAgentApi(t);
  }

  /** Sets, nodes and running operations. */
  topology(): Promise<Topology> {
    return this.t.request("/api/system/database/topology");
  }

  /** One node with its replication lag history. */
  node(id: string): Promise<NodeDetail> {
    return this.t.request(`/api/system/database/nodes/${seg(id)}`);
  }

  /** Adds a replica. A local one is cloned by an operation (`result.operation`); an external one is registered at once (`result.node`). */
  addNode(input: AddNodeInput): Promise<NodeAdded> {
    return this.t.request("/api/system/database/nodes", { method: "POST", body: { kind: input.kind, name: input.name, set: input.set, url: input.url, weight: input.weight } });
  }

  /** Changes a replica's share of reads (0 takes it out of rotation). */
  updateNode(id: string, weight: number): Promise<TopologyNode> {
    return this.t.request(`/api/system/database/nodes/${seg(id)}`, { method: "PATCH", body: { weight } });
  }

  /** Removes a replica; `confirm` is its name. */
  removeNode(id: string, confirm: string): Promise<{ removed: string }> {
    return this.t.request(`/api/system/database/nodes/${seg(id)}`, { method: "DELETE", query: { confirm } });
  }

  /** Can this replica be promoted now, and how far behind is it? */
  promoteCheck(id: string): Promise<PromoteCheck> {
    return this.t.request(`/api/system/database/nodes/${seg(id)}/promote-check`);
  }

  /** Promotes a replica to primary (an operation); `confirm` is the node's name. */
  promote(id: string, o: { confirm: string; acceptDataLoss?: boolean }): Promise<Operation> {
    return this.accepted(`/api/system/database/nodes/${seg(id)}/promote`, { confirm: o.confirm, accept_data_loss: o.acceptDataLoss });
  }

  /** Re-clones a replica from the primary (an operation). */
  rebuildNode(id: string): Promise<Operation> {
    return this.accepted(`/api/system/database/nodes/${seg(id)}/rebuild`, undefined);
  }

  /** A token (shown once) and the command that joins a replica on another host. */
  createJoinToken(o: { set?: string; role?: string } = {}): Promise<JoinToken> {
    return this.t.request("/api/system/database/join-tokens", { method: "POST", body: { set: o.set, role: o.role } });
  }

  /** Adds a shard (an operation). */
  addShard(input: AddShardInput): Promise<Operation> {
    return this.accepted("/api/system/database/shards", { name: input.name, node: input.node, weight: input.weight });
  }

  updateShard(id: string, patch: UpdateShardInput): Promise<TopologySet> {
    return this.t.request(`/api/system/database/shards/${seg(id)}`, {
      method: "PATCH",
      body: { state: patch.state, weight: patch.weight, hosts_tenants: patch.hostsTenants },
    });
  }

  /** Removes an empty shard (`ConflictError`, code `shard_not_empty`, otherwise); `confirm` is its name. */
  removeShard(id: string, confirm: string): Promise<{ removed: string }> {
    return this.t.request(`/api/system/database/shards/${seg(id)}`, { method: "DELETE", query: { confirm } });
  }

  /** Where each tenant lives, one page (`next_cursor` continues). */
  placements(o: PlacementsOptions = {}): Promise<PlacementPage> {
    return this.t.request("/api/system/database/placements", { query: { set: o.set, project: o.project, q: o.search, cursor: o.cursor, limit: o.limit } });
  }

  /** Moves a tenant to another set online (an operation); `confirm` is the tenant id. */
  moveTenant(tenant: string, o: { to: string; confirm: string }): Promise<Operation> {
    return this.accepted(`/api/system/database/placements/${seg(tenant)}/move`, { to: o.to, confirm: o.confirm });
  }

  /** Plans a rebalance without running it. */
  rebalancePlan(o: { sets?: string[]; maxMoves?: number } = {}): Promise<RebalancePlan> {
    return this.t.request("/api/system/database/rebalance/plan", { method: "POST", body: { sets: o.sets, max_moves: o.maxMoves } });
  }

  /** Runs a stored plan (an operation); `confirm` is the plan id. */
  rebalance(planId: string, confirm: string): Promise<Operation> {
    return this.accepted("/api/system/database/rebalance", { plan_id: planId, confirm });
  }

  /** Data of removed shards still on disk. */
  async retired(): Promise<RetiredData[]> {
    return (await this.t.request<{ items: RetiredData[] }>("/api/system/database/retired")).items;
  }

  /** Deletes the data of a removed shard; `confirm` is its node's name. */
  deleteRetired(id: string, confirm: string): Promise<{ removed: string }> {
    return this.t.request(`/api/system/database/retired/${seg(id)}`, { method: "DELETE", query: { confirm } });
  }

  /** Topology operations, newest first (`active`: pending and running only). */
  async operations(o: { active?: boolean; limit?: number } = {}): Promise<Operation[]> {
    return (await this.t.request<{ items: Operation[] }>("/api/system/database/operations", { query: { active: o.active, limit: o.limit } })).items;
  }

  operation(id: string): Promise<Operation> {
    return this.t.request(`/api/system/database/operations/${seg(id)}`);
  }

  /** Cancels an operation at its next safe point (`ConflictError`, code `operation_not_cancellable`, past it). */
  cancelOperation(id: string): Promise<Operation> {
    return this.t.request(`/api/system/database/operations/${seg(id)}/cancel`, { method: "POST" });
  }

  /** Polls an operation until it has `succeeded`, `failed`, been `cancelled` or died. Resolves with its final state. */
  async waitForOperation(id: string, o: { intervalMs?: number; timeoutMs?: number; signal?: AbortSignal } = {}): Promise<Operation> {
    const deadline = o.timeoutMs === undefined ? Infinity : Date.now() + o.timeoutMs;
    for (;;) {
      const op = await this.operation(id);
      if (FINAL.has(op.status)) return op;
      if (Date.now() >= deadline) throw new Error(`operation ${id} is still ${op.status} after ${o.timeoutMs} ms`);
      await new Promise<void>((resolve, reject) => {
        if (o.signal?.aborted) return reject(o.signal.reason);
        const timer = setTimeout(resolve, o.intervalMs ?? 1000);
        o.signal?.addEventListener("abort", () => (clearTimeout(timer), reject(o.signal?.reason)), { once: true });
      });
    }
  }

  private async accepted(path: string, body: unknown): Promise<Operation> {
    return (await this.t.request<{ operation: Operation }>(path, { method: "POST", body })).operation;
  }
}

/** Snapshots of the data directory: `client.system.backups`. */
export class BackupsApi {
  constructor(private readonly t: Transport) {}

  async list(): Promise<Snapshot[]> {
    return (await this.t.request<{ items: Snapshot[] }>("/api/v1/system/backups")).items;
  }

  /** Takes a snapshot now. */
  create(o: CreateSnapshotOptions = {}): Promise<SnapshotManifest> {
    return this.t.request("/api/v1/system/backups", {
      method: "POST",
      body: { label: o.label, include_indexes: o.includeIndexes, retention: retention(o.retention) },
    });
  }

  get(id: string): Promise<Snapshot> {
    return this.t.request(`/api/v1/system/backups/${seg(id)}`);
  }

  delete(id: string): Promise<void> {
    return this.t.request(`/api/v1/system/backups/${seg(id)}`, { method: "DELETE" });
  }

  /** Re-reads every file of a snapshot and checks it against the manifest. */
  verify(id: string): Promise<VerifyReport> {
    return this.t.request(`/api/v1/system/backups/${seg(id)}/verify`, { method: "POST" });
  }

  /** Deletes snapshots outside the retention; returns the ids removed. */
  async prune(r: Retention = {}): Promise<string[]> {
    return (await this.t.request<PruneResult>("/api/v1/system/backups/prune", { method: "POST", body: retention(r) })).deleted;
  }
}

/** Direct database logins (Postgres engine): `client.system.dbLogins`. */
export class DbLoginsApi {
  constructor(private readonly t: Transport) {}

  async list(): Promise<DbLogin[]> {
    return (await this.t.request<{ items: DbLogin[] }>("/api/v1/system/db-logins")).items;
  }

  /** Creates a login; the password is in the result and shown once. */
  create(input: DbLoginInput): Promise<DbLoginSecret> {
    return this.t.request("/api/v1/system/db-logins", {
      method: "POST",
      body: {
        name: input.name,
        admin: input.admin,
        readonly: input.readonly,
        connection_limit: input.connectionLimit,
        principal: input.principal,
        statement_timeout_ms: input.statementTimeoutMs,
        tenant: input.tenant,
        valid_until: input.validUntil,
      },
    });
  }

  get(name: string): Promise<DbLogin> {
    return this.t.request(`/api/v1/system/db-logins/${seg(name)}`);
  }

  /** A new password (shown once), optionally with a new expiry. */
  rotate(name: string, o: { validUntil?: string } = {}): Promise<DbLoginSecret> {
    return this.t.request(`/api/v1/system/db-logins/${seg(name)}/rotate`, { method: "POST", body: { valid_until: o.validUntil } });
  }

  delete(name: string): Promise<void> {
    return this.t.request(`/api/v1/system/db-logins/${seg(name)}`, { method: "DELETE" });
  }
}

/** Postgres extensions: `client.system.extensions`. */
export class ExtensionsApi {
  constructor(private readonly t: Transport) {}

  async list(): Promise<ExtensionInfo[]> {
    return (await this.t.request<{ items: ExtensionInfo[] }>("/api/v1/system/extensions")).items;
  }

  install(name: string): Promise<ExtensionInfo> {
    return this.t.request("/api/v1/system/extensions", { method: "POST", body: { name } });
  }

  drop(name: string): Promise<void> {
    return this.t.request(`/api/v1/system/extensions/${seg(name)}`, { method: "DELETE" });
  }
}

/** Instance administration: `client.system`. */
export class SystemApi {
  readonly database: DatabaseTopologyApi;
  readonly backups: BackupsApi;
  readonly dbLogins: DbLoginsApi;
  readonly extensions: ExtensionsApi;

  constructor(private readonly t: Transport) {
    this.database = new DatabaseTopologyApi(t);
    this.backups = new BackupsApi(t);
    this.dbLogins = new DbLoginsApi(t);
    this.extensions = new ExtensionsApi(t);
  }

  /** Name, version, data directory, uptime and database summary. */
  info(): Promise<Record<string, unknown>> {
    return this.t.request("/api/system/info");
  }

  /**
   * Every feature of the server with the engine it runs on (a PostgreSQL extension or the portable implementation), whether it
   * is `active`, `portable`, `off`, `native` or `unavailable`, its configured mode, the reason and its limits (ADR-0034).
   */
  capabilities(): Promise<Capabilities> {
    return this.t.request("/api/system/capabilities");
  }

  /** Every module, whether it is on, and its typed settings (secrets have `value: null`). */
  features(): Promise<SystemFeatures> {
    return this.t.request("/api/system/features");
  }

  /**
   * Writes settings to the config file (all keys validated first, one atomic write). `set` maps a setting key to its value;
   * changes apply after a restart (`restart_required`). A bad key is a `ValidationError` with `details` per key.
   */
  updateConfig(set: Record<string, unknown>): Promise<ConfigPatched> {
    return this.t.request("/api/system/config", { method: "PATCH", body: { set } });
  }
}
