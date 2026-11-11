/**
 * Database topology (FB-175 routes, FB-176 screen): the primary server, its read replicas and the shards, managed
 * from the dashboard. Every type serialises as the server stores it (snake case). Administrators only; PostgreSQL
 * only (400 `postgres_required` on SQLite, where `GET topology` still answers with one server).
 *
 * Tenant moves and rebalancing answer 501 `not_implemented` until FB-174 merges; `isNotImplemented` tells the
 * screen to say "coming in this release" instead of showing an error.
 */
import { ApiError, request, send } from "./client";

const BASE = "/api/system/database";
const enc = encodeURIComponent;

/** Every `method path` this file calls (the OpenAPI fixture lists them after the next regeneration). */
export const TOPOLOGY_OPERATIONS = [
  "get /api/system/database/topology", "post /api/system/database/nodes", "get /api/system/database/nodes/{id}",
  "patch /api/system/database/nodes/{id}", "delete /api/system/database/nodes/{id}", "get /api/system/database/nodes/{id}/promote-check",
  "post /api/system/database/nodes/{id}/promote", "post /api/system/database/nodes/{id}/rebuild", "post /api/system/database/join-tokens",
  "post /api/system/database/shards", "patch /api/system/database/shards/{id}", "delete /api/system/database/shards/{id}",
  "get /api/system/database/placements", "post /api/system/database/placements/{tenant}/move", "post /api/system/database/rebalance/plan",
  "post /api/system/database/rebalance", "get /api/system/database/operations", "get /api/system/database/operations/{id}",
  "post /api/system/database/operations/{id}/cancel",
] as const;

export type SetKind = "control" | "shard";
export type SetState = "provisioning" | "active" | "draining" | "migrating" | "retired";
export type NodeKind = "local" | "remote" | "external";
export type NodeRole = "primary" | "replica";
export type NodeState = "provisioning" | "ready" | "catching_up" | "streaming" | "lagging" | "unreachable" | "needs_rebuild" | "promoting";

export interface TopologySet {
  /** `control` for control. */
  id: string;
  name: string;
  kind: SetKind;
  number: number;
  state: SetState;
  weight: number;
  hosts_tenants: boolean;
  tenants: number;
  est_bytes: number;
}

/** The `forgebase node` agent of a remote node. */
export interface NodeAgent {
  state?: string;
  address?: string;
  replay_lsn?: string | null;
  disk_free?: number | null;
  last_seen_at?: string | null;
}

export interface TopologyNode {
  id: string;
  set: string;
  name: string;
  kind: NodeKind;
  role: NodeRole;
  state: NodeState;
  weight: number;
  engine_version?: string | null;
  system_identifier?: string | null;
  timeline?: number | null;
  /** `api`, or `config` (declared in forgebase.toml: read-only here). */
  origin: "api" | "config" | string;
  in_rotation?: boolean | null;
  lag_ms?: number | null;
  lag_bytes?: number | null;
  replay_lsn?: string | null;
  last_error?: string | null;
  agent?: NodeAgent | null;
  created_at: string;
  updated_at: string;
}

export interface LagSample {
  at: string;
  lag_ms: number;
  lag_bytes: number;
}

export interface NodeDetail {
  node: TopologyNode;
  /** The last hour; empty after a server restart. */
  lag_history: LagSample[];
}

export interface OperationProgress {
  percent?: number | null;
  stage?: string | null;
  message?: string | null;
}

export type OperationStatus = "queued" | "pending" | "running" | "succeeded" | "failed" | "cancelled" | "aborted" | string;

export interface TopologyOperation {
  /** The job id: progress streams on realtime channel `jobs:<id>`. */
  id: string;
  /** `database.node.add`, `database.node.rebuild`, `database.promote`, `database.shard.add`, … */
  kind: string;
  status: OperationStatus;
  set?: string | null;
  node?: string | null;
  progress?: OperationProgress | null;
  cancel_requested: boolean;
  cancellable: boolean;
  result?: unknown;
  last_error?: string | null;
  created_by: string;
  created_at: string;
  started_at?: string | null;
  finished_at?: string | null;
}

export interface TopologyView {
  /** `postgres` or `sqlite`. */
  backend: "postgres" | "sqlite" | string;
  engine_version?: string | null;
  epoch: number;
  sets: TopologySet[];
  nodes: TopologyNode[];
  operations: TopologyOperation[];
  /** Set after promoting control's primary: restart the server to move its connections. */
  restart_required?: string | null;
}

export interface AddNodeBody {
  set?: string;
  name: string;
  /** `local` (a replica on this server, cloned now) or `external` (a replica you run, at `url`). */
  kind: "local" | "external";
  url?: string;
  weight?: number;
}

/** `local` answers with an operation, `external` with the node. */
export interface Added {
  operation?: TopologyOperation | null;
  node?: TopologyNode | null;
}

export interface PromoteCheck {
  replayed?: string | null;
  primary_lsn?: string | null;
  behind_bytes?: number | null;
  /** False: the old primary is down; promoting needs `accept_data_loss`. */
  primary_reachable: boolean;
}

export interface JoinToken {
  id: string;
  /** `fbj_…`, shown once. */
  token: string;
  set: string;
  role: string;
  expires_at: string;
  /** The command to run on the other host. */
  command: string;
}

export interface AddShardBody {
  name: string;
  node: { name: string; kind: "external"; url: string };
  weight?: number;
}

export interface UpdateShardBody {
  state?: string;
  weight?: number;
  hosts_tenants?: boolean;
}

export interface Placement {
  tenant: string;
  project?: string | null;
  organization?: string | null;
  set: string;
  state: string;
  est_rows: number;
  est_bytes: number;
  moved_at?: string | null;
}

export interface PlacementPage {
  items: Placement[];
  next_cursor?: string | null;
}

export interface PlacementQuery {
  set?: string;
  project?: string;
  q?: string;
  cursor?: string;
  limit?: number;
}

/** One planned move of a rebalance (ADR-0029 §4.3). */
export interface PlannedMove {
  tenant: string;
  project?: string | null;
  from: string;
  to: string;
  est_bytes: number;
  freeze_estimate_ms?: number | null;
}

export interface RebalancePlan {
  plan_id: string;
  moves: PlannedMove[];
  est_bytes?: number;
  freeze_estimate_ms?: number | null;
}

export interface MoveEstimate {
  est_bytes?: number | null;
  freeze_estimate_ms?: number | null;
}

export const getTopology = (signal?: AbortSignal) => request<TopologyView>(`${BASE}/topology`, { signal });
export const getTopologyNode = (id: string, signal?: AbortSignal) => request<NodeDetail>(`${BASE}/nodes/${enc(id)}`, { signal });
export const addNode = (body: AddNodeBody) => send<Added>("POST", `${BASE}/nodes`, body);
export const setNodeWeight = (id: string, weight: number) => send<TopologyNode>("PATCH", `${BASE}/nodes/${enc(id)}`, { weight });
/** `confirm` is the node's name. */
export const removeNode = (id: string, confirm: string) => send<{ removed: string }>("DELETE", `${BASE}/nodes/${enc(id)}?confirm=${enc(confirm)}`);
export const promoteCheck = (id: string, signal?: AbortSignal) => request<PromoteCheck>(`${BASE}/nodes/${enc(id)}/promote-check`, { signal });
export const promoteNode = (id: string, confirm: string, acceptDataLoss: boolean) =>
  send<{ operation: TopologyOperation }>("POST", `${BASE}/nodes/${enc(id)}/promote`, { confirm, accept_data_loss: acceptDataLoss });
export const rebuildNode = (id: string) => send<{ operation: TopologyOperation }>("POST", `${BASE}/nodes/${enc(id)}/rebuild`, {});
export const issueJoinToken = (set: string, role: "replica" = "replica") => send<JoinToken>("POST", `${BASE}/join-tokens`, { set, role });
export const addShard = (body: AddShardBody) => send<{ operation: TopologyOperation }>("POST", `${BASE}/shards`, body);
export const updateShard = (id: string, body: UpdateShardBody) => send<TopologySet>("PATCH", `${BASE}/shards/${enc(id)}`, body);
export const removeShard = (id: string, confirm: string) => send<{ removed: string }>("DELETE", `${BASE}/shards/${enc(id)}?confirm=${enc(confirm)}`);

export async function listPlacements(q: PlacementQuery = {}, signal?: AbortSignal): Promise<PlacementPage> {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(q)) if (v !== undefined && v !== "") p.set(k, String(v));
  const s = p.toString();
  return request<PlacementPage>(`${BASE}/placements${s ? `?${s}` : ""}`, { signal });
}
export const moveTenant = (tenant: string, to: string, confirm: string) =>
  send<{ operation: TopologyOperation }>("POST", `${BASE}/placements/${enc(tenant)}/move`, { to, confirm });
export const planRebalance = (body: { sets?: string[]; max_moves?: number } = {}) => send<RebalancePlan>("POST", `${BASE}/rebalance/plan`, body);
export const runRebalance = (planId: string, confirm: string) => send<{ operation: TopologyOperation }>("POST", `${BASE}/rebalance`, { plan_id: planId, confirm });

export async function listOperations(active = false, signal?: AbortSignal): Promise<TopologyOperation[]> {
  return (await request<{ items: TopologyOperation[] }>(`${BASE}/operations${active ? "?active=true" : ""}`, { signal })).items;
}
export const getOperation = (id: string, signal?: AbortSignal) => request<TopologyOperation>(`${BASE}/operations/${enc(id)}`, { signal });
export const cancelOperation = (id: string) => send<TopologyOperation>("POST", `${BASE}/operations/${enc(id)}/cancel`, {});

/** 501 `not_implemented`: tenant moves and rebalancing arrive with FB-174. */
export const isNotImplemented = (e: unknown): boolean => e instanceof ApiError && (e.status === 501 || e.code === "not_implemented");
/** 403 `mfa_step_up_required`: sign in again with the second factor, then retry. */
export const isStepUpRequired = (e: unknown): boolean => e instanceof ApiError && e.code === "mfa_step_up_required";
/** An operation that has stopped. */
export const isFinished = (o: Pick<TopologyOperation, "status">): boolean => ["succeeded", "failed", "cancelled", "aborted", "dead"].includes(o.status);
